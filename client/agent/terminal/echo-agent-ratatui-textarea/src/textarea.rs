use crate::editor::{
    ApplyEditPlanError, EditBuffer, EditCommand, EditCommandCategory, EditOutcome, EditPlan,
    HorizontalEdge, Movement, WordStyle, classify_key_event, resolve_movement,
};
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::WidgetRef;
use ratatui_core::buffer::Buffer as CoreBuffer;
use ratatui_core::layout::Rect as CoreRect;
use ratatui_core::widgets::Widget as _;
use std::cell::Ref;
use std::cell::RefCell;
use std::ops::Range;
use std::time::Instant;
use textwrap::Options;
use tui_scrollbar::{ScrollBar, ScrollLengths};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Stable, unique identifier for a text element. Monotonically increasing, never reused.
///
/// The host app can use this as a key into its own metadata store
/// (e.g. `HashMap<ElementId, PasteMetadata>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementId(u64);

impl ElementId {
    /// Construct an `ElementId` from a raw `u64` value.
    ///
    /// Primarily useful for tests and serialization; normal code should use
    /// the IDs returned by [`TextArea::insert_element`].
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// Opaque element kind tag. The textarea does not interpret this value;
/// the host app defines constants like `ElementKind(1)` for pastes,
/// `ElementKind(2)` for file references, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementKind(pub u16);

// ── Clipboard ──

/// Trait for clipboard access. The textarea calls this on copy/cut/paste.
///
/// The default implementation ([`InternalClipboard`]) stores text in memory.
/// Host apps can provide a system clipboard backend (e.g. `arboard`) via
/// [`TextArea::set_clipboard_provider`].
pub trait ClipboardProvider: std::fmt::Debug {
    /// Read the current clipboard contents (for paste).
    fn get(&mut self) -> Option<String>;
    /// Write text to the clipboard (on copy/cut).
    fn set(&mut self, text: &str);
}

/// In-memory clipboard — the default provider.
#[derive(Debug, Default)]
pub struct InternalClipboard {
    contents: Option<String>,
}

impl ClipboardProvider for InternalClipboard {
    fn get(&mut self) -> Option<String> {
        self.contents.clone()
    }

    fn set(&mut self, text: &str) {
        self.contents = Some(text.to_string());
    }
}

// ── Text element events ──

/// An interaction with a [`TextElement`], returned by [`TextArea::poll_element_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextElementEvent {
    /// The element that was interacted with.
    pub id: ElementId,
    /// What kind of interaction occurred.
    pub kind: TextElementEventKind,
}

/// The kind of element interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextElementEventKind {
    /// The element was clicked (single click).
    Click,
    /// The mouse entered the element (was outside or on a different element).
    HoverEnter,
    /// The mouse left the element (moved to plain text or a different element).
    HoverLeave,
}

/// An atomic text element embedded in the buffer.
///
/// Elements are indivisible units for navigation and editing. The cursor
/// cannot be placed inside an element; it jumps from the start boundary
/// to the end boundary atomically.
#[derive(Debug, Clone)]
pub struct TextElement {
    /// Stable identifier, unique across the lifetime of the `TextArea`.
    pub id: ElementId,
    /// Byte range in the underlying text buffer.
    pub range: Range<usize>,
    /// Host-defined kind tag.
    pub kind: ElementKind,
    /// Custom display text and styling. When `Some`, this `Line` is rendered
    /// instead of the raw buffer text. When `None`, the buffer text is rendered
    /// with a default element style (cyan).
    pub display: Option<Line<'static>>,
}

// ── Selection ──

/// A byte-range selection in the buffer, created by mouse drag.
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    /// Buffer position where the selection started (fixed anchor).
    pub anchor: usize,
    /// Buffer position where the selection currently extends to (moves with drag).
    pub head: usize,
}

// ── Mouse ──

/// Result of processing a mouse event in the textarea.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseAction {
    /// Nothing interesting happened.
    Nothing,
    /// Cursor was placed at a position (single click on plain text).
    CursorPlaced,
    /// Selection was updated (drag in progress, or double/triple click).
    SelectionUpdated,
    /// Selection was finalized — text copied to clipboard.
    /// Host should call `take_clipboard()` to retrieve it.
    SelectionFinished,
    /// Content was scrolled (mouse wheel).
    Scrolled,
}

/// Tracks consecutive clicks at the same screen position to detect
/// double-click (word select) and triple-click (line select).
#[derive(Debug)]
struct ClickTracker {
    last_time: Instant,
    last_pos: (u16, u16),
    count: u8,
}

impl Default for ClickTracker {
    fn default() -> Self {
        Self {
            last_time: Instant::now(),
            last_pos: (u16::MAX, u16::MAX),
            count: 0,
        }
    }
}

impl ClickTracker {
    /// Maximum time between clicks to count as multi-click (ms).
    const MULTI_CLICK_MS: u128 = 500;

    /// Register a click at `(col, row)`. Returns the click count (1, 2, or 3).
    fn register(&mut self, col: u16, row: u16) -> u8 {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_time).as_millis();
        if elapsed < Self::MULTI_CLICK_MS && self.last_pos == (col, row) && self.count < 3 {
            self.count += 1;
        } else {
            self.count = 1;
        }
        self.last_time = now;
        self.last_pos = (col, row);
        self.count
    }
}

#[derive(Debug)]
pub struct TextArea {
    text: EditBuffer,
    wrap_cache: RefCell<Option<WrapCache>>,
    preferred_col: Option<usize>,
    elements: Vec<TextElement>,
    next_element_id: u64,
    kill_buffer: String,
    undo: UndoState,
    /// Active selection (mouse drag). `None` when no selection.
    selection: Option<Selection>,
    /// Clipboard provider — defaults to [`InternalClipboard`].
    /// Swap with [`set_clipboard_provider`](Self::set_clipboard_provider)
    /// for system clipboard support.
    clipboard_provider: Box<dyn ClipboardProvider>,
    /// Last copied text — set on copy/cut, cleared by `take_clipboard()`.
    /// This is the "notification" channel: the host calls `take_clipboard()`
    /// to detect that something was just copied.
    clipboard: Option<String>,
    /// Whether to keep the selection visible after mouse-up.
    /// When `false`, selection clears immediately on mouse-up (fully transient).
    pub keep_selection_after_mouseup: bool,
    /// Style applied to selected text.  Defaults to a tokyonight-inspired
    /// blue background (`rgb(49, 62, 115)`) with an explicit light foreground
    /// (`rgb(192, 202, 245)`) so the selection is legible regardless of the
    /// host terminal's colour scheme.
    ///
    /// Override to match your own theme, e.g.:
    /// ```ignore
    /// textarea.selection_style = Style::default().bg(Color::Rgb(60, 60, 60));
    /// ```
    pub selection_style: Style,
    /// Screen position of the last mouse-down (for distinguishing click vs drag).
    mouse_down_pos: Option<(u16, u16)>,
    /// Buffer byte position of the mouse-down anchor (for drag selection).
    drag_anchor: Option<usize>,
    /// Whether a drag is currently in progress.
    drag_active: bool,
    /// Last time drag-scroll was applied (throttle).
    last_drag_scroll: Option<Instant>,
    /// Number of drag-scroll steps taken so far (for acceleration).
    drag_scroll_steps: u32,
    /// Stored drag event for continuous drag-scroll (re-triggered on timer).
    /// Set when a drag moves outside the textarea area; cleared on mouse-up.
    pending_drag_scroll: Option<MouseEvent>,
    /// Tracks multi-click (double/triple) at the same position.
    click_tracker: ClickTracker,
    /// Internal scroll offset set by mousewheel events.  When `Some`, this
    /// overrides the external `TextAreaState.scroll` so the viewport scrolls
    /// independently of the cursor.  Cleared whenever the cursor moves
    /// (typing, navigation, click) so the viewport snaps back to follow it.
    scroll_override: Option<u16>,
    /// Whether to show a scrollbar on the right edge when content overflows.
    /// When enabled, the rightmost column is reserved for the scrollbar track
    /// and the text area wraps at `width - 1`. Defaults to `true`.
    pub show_scrollbar: bool,
    /// Style for the scrollbar track (empty space).  Defaults to a dark
    /// tokyonight-inspired background.  Override to match your theme's
    /// background when embedding the textarea in a non-default-bg context.
    pub scrollbar_track_style: Style,
    /// Style for the scrollbar thumb (draggable indicator).  Defaults to a
    /// slightly lighter tokyonight shade.  Override to match your theme.
    pub scrollbar_thumb_style: Style,
    /// Padding (in columns) between the text content and the scrollbar track.
    /// Only applies when the scrollbar is visible.  Defaults to `0`.
    pub scrollbar_padding: u16,
    /// Whether the user is currently dragging the scrollbar thumb.
    scrollbar_dragging: bool,
    /// Currently hovered element (for enter/leave detection).
    hovered_element: Option<ElementId>,
    /// Pending element event — consumed by [`poll_element_event`](Self::poll_element_event).
    pending_element_event: Option<TextElementEvent>,
    /// Columns per tab character for display width and tab→space expansion on
    /// insert. `0` leaves tabs as-is (unicode-width treats them as 0-width).
    /// Defaults to `4`, matching scrollback `appearance::tab_width`.
    tab_width: u8,
}

#[derive(Debug, Clone)]
struct WrapCache {
    width: u16,
    lines: Vec<Range<usize>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TextAreaState {
    /// Index into wrapped lines of the first visible line.
    pub scroll: u16,
}

// ── Undo/Redo ──

/// A snapshot of the textarea state for undo/redo.
#[derive(Debug, Clone)]
struct UndoEntry {
    text: String,
    cursor: usize,
    elements: Vec<TextElement>,
}

/// What kind of mutation is being performed. Used for batching consecutive
/// same-kind operations into a single undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutationKind {
    /// Character-by-character typing, `insert_str`, `yank`.
    Insert,
    /// Backspace, delete forward.
    Delete,
    /// Ctrl+K, Ctrl+U, word-delete — always a discrete undo step.
    Kill,
    /// `insert_element`, `replace_range_with_element` — always discrete.
    Element,
    /// `set_text`, `replace_range` (host-driven) — always discrete.
    Replace,
}

/// Manages the undo/redo stacks.
#[derive(Debug)]
struct UndoState {
    stack: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    max_depth: usize,
    /// The kind of the last mutation that was checkpointed.
    last_kind: Option<MutationKind>,
    /// Cursor position *after* the last mutation completed.
    /// Used to detect cursor jumps (arrows between inserts → new undo group).
    last_cursor: usize,
    /// Whether the last inserted character was whitespace.
    /// Used to break insert batches at word boundaries (ws↔non-ws transitions).
    last_insert_ws: bool,
    /// Nesting depth for undo groups. When > 0, `pre_mutate` is suppressed.
    group_depth: usize,
    /// Snapshot taken when the outermost `begin_undo_group()` was called.
    /// Used by `end_undo_group` to push the checkpoint, or by
    /// `cancel_undo_group` to restore the pre-group state.
    group_checkpoint: Option<UndoEntry>,
}

impl Default for UndoState {
    fn default() -> Self {
        Self {
            stack: Vec::new(),
            redo: Vec::new(),
            max_depth: 100,
            last_kind: None,
            last_cursor: 0,
            last_insert_ws: false,
            group_depth: 0,
            group_checkpoint: None,
        }
    }
}

/// Whether `key` is the undo chord [`TextArea::input`] binds: lowercase
/// 'z' with Ctrl or Cmd. Uppercase 'Z' (redo) is intentionally excluded,
/// which keeps this guard disjoint from the redo arm regardless of order.
///
/// Single source for the binding: `input()`'s undo arm consumes this
/// predicate, and hosts that react to undo (e.g. retiring an undo hint)
/// call it too, so the chord and its observers cannot drift.
pub fn is_undo_input(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('z'))
        && (key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::SUPER))
}

impl TextArea {
    /// Compute the number of lines to scroll per mouse wheel tick based on
    /// the viewport height.  Small viewports scroll slowly (1 line), large
    /// viewports scroll faster (up to 3 lines).
    fn scroll_lines_for_height(height: u16) -> u16 {
        match height {
            0..=5 => 1,
            6..=15 => 2,
            _ => 3,
        }
    }

    /// Drag-scroll throttle intervals (ms): ramps up from slow to fast.
    /// After the last entry, the final value repeats.
    const DRAG_SCROLL_RAMP_MS: &[u128] = &[80, 60, 40];

    /// Compute the drag-scroll interval for the given step count.
    fn drag_scroll_interval(step: u32) -> u128 {
        let ramp = Self::DRAG_SCROLL_RAMP_MS;
        ramp[ramp.len().min(step as usize + 1) - 1]
    }

    /// How many extra lines to scroll based on distance from area edge.
    /// Returns 1 for 1-2 rows outside, 2 for 3-4 rows, 3 for 5-8, etc.
    fn drag_scroll_lines_for_distance(distance: u16) -> usize {
        match distance {
            0..=2 => 1,
            3..=5 => 2,
            6..=10 => 3,
            _ => 5,
        }
    }

    /// Clamp a buffer position so it stays within a wrapped line's range
    /// `[line_start, line_end)`.  Without this, `display_col_to_buffer_pos`
    /// can return `line_end` when the column exceeds the line's display
    /// width — and `line_end` equals the *next* wrapped line's start,
    /// which confuses `effective_scroll` into thinking the cursor hasn't
    /// actually moved to the target line.
    ///
    /// Uses `self.text` to find the last valid char boundary inside the line
    /// so we never land in the middle of a multi-byte character.
    fn clamp_to_line(&self, pos: usize, line_start: usize, line_end: usize) -> usize {
        if line_end > line_start {
            // Find the start of the last character in the line.
            let last_char_start = self.text[line_start..line_end]
                .char_indices()
                .next_back()
                .map(|(i, _)| line_start + i)
                .unwrap_or(line_start);
            pos.min(last_char_start)
        } else {
            line_start
        }
    }

    pub fn new() -> Self {
        Self {
            text: EditBuffer::new(),
            wrap_cache: RefCell::new(None),
            preferred_col: None,
            elements: Vec::new(),
            next_element_id: 0,
            kill_buffer: String::new(),
            undo: UndoState::default(),
            selection: None,
            clipboard_provider: Box::new(InternalClipboard::default()),
            clipboard: None,
            keep_selection_after_mouseup: true,
            selection_style: Style::default()
                .bg(Color::Rgb(49, 62, 115))
                .fg(Color::Rgb(192, 202, 245)),
            mouse_down_pos: None,
            drag_anchor: None,
            drag_active: false,
            last_drag_scroll: None,
            drag_scroll_steps: 0,
            pending_drag_scroll: None,
            click_tracker: ClickTracker::default(),
            scroll_override: None,
            show_scrollbar: true,
            scrollbar_track_style: Style::default().bg(Color::Rgb(32, 35, 53)),
            scrollbar_thumb_style: Style::default()
                .fg(Color::Rgb(42, 46, 65))
                .bg(Color::Rgb(32, 35, 53)),
            scrollbar_padding: 0,
            scrollbar_dragging: false,
            hovered_element: None,
            pending_element_event: None,
            tab_width: 4,
        }
    }

    /// Columns per tab for display width and tab→space expansion (`0` = passthrough).
    pub fn tab_width(&self) -> u8 {
        self.tab_width
    }

    /// Set columns per tab. Also controls expansion on insert/`set_text`/`replace_range`.
    pub fn set_tab_width(&mut self, tab_width: u8) {
        if self.tab_width != tab_width {
            self.tab_width = tab_width;
            self.wrap_cache.replace(None);
        }
    }

    /// Expand `\t` to `tab_width` spaces (scrollback-compatible fixed width).
    /// `tab_width == 0` or no tabs → borrowed input.
    ///
    /// Public because it is the exact transform every insert path applies
    /// (see [`insert_str`](Self::insert_str) /
    /// [`insert_element`](Self::insert_element)), letting hosts canonicalize
    /// external text before comparing it against buffer content.
    pub fn expand_tabs<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        expand_tabs_with_width(text, self.tab_width)
    }

    /// Display width of plain buffer text, treating tabs as `tab_width` columns.
    fn plain_display_width(&self, text: &str) -> usize {
        plain_display_width_with_tab(text, self.tab_width)
    }

    /// Display width of a single grapheme cluster (tab uses `tab_width`).
    fn grapheme_display_width(&self, grapheme: &str) -> usize {
        grapheme_display_width_with_tab(grapheme, self.tab_width)
    }

    fn element_ranges(&self) -> Vec<Range<usize>> {
        self.elements
            .iter()
            .map(|element| element.range.clone())
            .collect()
    }

    fn adjust_position_after_edit(
        position: usize,
        replaced: &Range<usize>,
        inserted_len: usize,
    ) -> usize {
        if position < replaced.start {
            position
        } else if position <= replaced.end {
            replaced.start + inserted_len
        } else {
            position - replaced.len() + inserted_len
        }
    }

    fn is_semantic_edit(plan: &EditPlan) -> bool {
        plan.removed_text() != plan.replacement() || !plan.replaced_byte_range().is_empty()
    }

    fn assert_valid_edit_plan(&self, plan: &EditPlan) {
        if let Err(error) = self.text.validate_plan(plan) {
            panic!("textarea edit invariant failed: {error:?}");
        }
    }

    fn apply_validated_edit_plan(
        &mut self,
        plan: EditPlan,
        mutation_kind: Option<MutationKind>,
    ) -> EditOutcome {
        let semantic_edit = Self::is_semantic_edit(&plan);
        let replaced = plan.replaced_byte_range();
        let inserted_len = plan.replacement().len();
        let outcome = self.text.apply_validated_plan(&plan);
        if semantic_edit {
            self.update_elements_after_replace(replaced.start, replaced.end, inserted_len);
            if let Some(selection) = &mut self.selection {
                selection.anchor =
                    Self::adjust_position_after_edit(selection.anchor, &replaced, inserted_len);
                selection.head =
                    Self::adjust_position_after_edit(selection.head, &replaced, inserted_len);
            }
            if self
                .selection
                .is_some_and(|selection| selection.anchor == selection.head)
            {
                self.selection = None;
            }
            self.wrap_cache.replace(None);
            if mutation_kind == Some(MutationKind::Kill) {
                self.kill_buffer = plan.into_removed_text();
            }
        }
        if semantic_edit || !matches!(outcome, EditOutcome::Unchanged) {
            self.preferred_col = None;
            self.scroll_override = None;
        }
        outcome
    }

    fn try_apply_edit_plan(
        &mut self,
        plan: EditPlan,
        mutation_kind: Option<MutationKind>,
    ) -> Result<EditOutcome, ApplyEditPlanError> {
        self.text.validate_plan(&plan)?;
        let semantic_edit = Self::is_semantic_edit(&plan);
        if semantic_edit && let Some(kind) = mutation_kind {
            self.pre_mutate(kind);
        }
        let outcome = self.apply_validated_edit_plan(plan, mutation_kind);
        if semantic_edit && mutation_kind.is_some() {
            self.post_mutate();
        }
        Ok(outcome)
    }

    fn apply_edit_plan(
        &mut self,
        plan: EditPlan,
        mutation_kind: Option<MutationKind>,
    ) -> EditOutcome {
        match self.try_apply_edit_plan(plan, mutation_kind) {
            Ok(outcome) => outcome,
            Err(error) => panic!("textarea edit invariant failed: {error:?}"),
        }
    }

    fn apply_edit_command(
        &mut self,
        command: EditCommand,
        mutation_kind: Option<MutationKind>,
    ) -> EditOutcome {
        let category = command.category();
        let ranges = self.element_ranges();
        let plan = self.text.plan_command(command, &ranges);
        let outcome = self.apply_edit_plan(plan, mutation_kind);
        if category == EditCommandCategory::Navigation {
            self.preferred_col = None;
            self.scroll_override = None;
        }
        outcome
    }

    fn plan_edit_replacement(&self, range: Range<usize>, replacement: &str) -> EditPlan {
        let replacement = self.expand_tabs(replacement).into_owned();
        let ranges = self.element_ranges();
        self.text
            .plan_replace_byte_range(range, &replacement, &ranges)
    }

    fn apply_edit_replacement(
        &mut self,
        range: Range<usize>,
        replacement: &str,
        mutation_kind: Option<MutationKind>,
    ) {
        let plan = self.plan_edit_replacement(range, replacement);
        self.apply_edit_plan(plan, mutation_kind);
    }

    pub fn set_text(&mut self, text: &str) {
        let cursor = self.cursor();
        let plan = self.plan_edit_replacement(0..self.text.len(), text);
        self.assert_valid_edit_plan(&plan);
        self.pre_mutate(MutationKind::Replace);
        let _ = self.text.apply_validated_plan(&plan);
        self.elements.clear();
        let len = self.text.len();
        self.set_cursor_inner(cursor.min(len));
        self.wrap_cache.replace(None);
        self.preferred_col = None;
        // Kill buffer intentionally survives: yank is independent of buffer
        // content, so a cut can be pasted into a fresh prompt after send.
        self.selection = None;
        self.mouse_down_pos = None;
        self.drag_anchor = None;
        self.drag_active = false;
        self.last_drag_scroll = None;
        self.drag_scroll_steps = 0;
        self.pending_drag_scroll = None;
        self.click_tracker = ClickTracker::default();
        self.scroll_override = None;
        self.scrollbar_dragging = false;
        self.hovered_element = None;
        self.pending_element_event = None;
        self.post_mutate();
    }

    pub fn text(&self) -> &str {
        self.text.text()
    }

    pub fn insert_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.scroll_override = None;
        // Word boundary: break the insert batch when char class changes (ws↔non-ws).
        if let Some(first) = text.chars().next() {
            let first_ws = first.is_whitespace();
            if self.undo.last_kind == Some(MutationKind::Insert)
                && self.undo.last_insert_ws != first_ws
            {
                // Force pre_mutate to see a "kind change" so it pushes a checkpoint.
                self.undo.last_kind = None;
            }
        }
        self.apply_edit_replacement(
            self.cursor()..self.cursor(),
            text,
            Some(MutationKind::Insert),
        );
        if let Some(last) = text.chars().last() {
            self.undo.last_insert_ws = last.is_whitespace();
        }
    }

    pub fn insert_str_at(&mut self, pos: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        self.apply_edit_replacement(pos..pos, text, Some(MutationKind::Insert));
        if let Some(last) = text.chars().last() {
            self.undo.last_insert_ws = last.is_whitespace();
        }
    }

    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        self.apply_edit_replacement(range, text, Some(MutationKind::Replace));
    }

    pub fn cursor(&self) -> usize {
        self.text.cursor_byte()
    }

    pub fn set_cursor(&mut self, pos: usize) {
        let pos = pos.clamp(0, self.text.len());
        let pos = self.clamp_pos_to_nearest_boundary(pos);
        self.set_cursor_inner(pos);
        self.preferred_col = None;
        self.scroll_override = None;
    }

    fn set_cursor_inner(&mut self, pos: usize) {
        let _ = self.text.set_cursor_byte(pos);
    }

    /// Override the scroll position, bypassing cursor-follow logic.
    ///
    /// When set to `Some(offset)`, `effective_scroll` will use this offset
    /// instead of ensuring the cursor is visible. Useful for forcing a
    /// specific viewport (e.g., scroll-to-top when the textarea is collapsed
    /// and unfocused). Set to `None` to restore normal cursor-following.
    ///
    /// Note: unlike the internal scroll_override set by mousewheel events,
    /// this is NOT cleared by cursor movement — it persists until explicitly
    /// cleared by the caller.
    pub fn set_scroll_override(&mut self, scroll: Option<u16>) {
        self.scroll_override = scroll;
    }

    /// Current scroll override value (if any).
    pub fn scroll_override(&self) -> Option<u16> {
        self.scroll_override
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        self.wrapped_lines(width).len() as u16
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_pos_with_state(area, TextAreaState::default())
    }

    /// Compute the on-screen cursor position taking scrolling into account.
    ///
    /// Returns `None` if the cursor is not visible in the current viewport
    /// (e.g. the user scrolled the viewport away from the cursor via mousewheel).
    ///
    /// Unlike [`screen_position_of`], this applies a wrap-boundary adjustment:
    /// when the cursor sits at the exact wrap boundary (col == content width),
    /// it is shown at the start of the next visual line instead of on the
    /// invisible right border.
    pub fn cursor_pos_with_state(&self, area: Rect, state: TextAreaState) -> Option<(u16, u16)> {
        let tw = self.text_width(area);
        let lines = self.wrapped_lines(tw);
        let effective_scroll = self.effective_scroll(area.height, &lines, state.scroll);
        let mut i = Self::wrapped_line_index_by_start(&lines, self.cursor())?;
        let ls = &lines[i];
        let mut col = self.display_width_of_range(ls.start, self.cursor()) as u16;

        // If the cursor sits at the exact wrap boundary (col == content width),
        // show it at the start of the next visual line instead of on the
        // invisible right border.  When the cursor is at text.len() and the
        // last line is exactly full, there is no next wrapped line — but we
        // still want the cursor on a new row at column 0.
        if col >= tw {
            i += 1;
            col = 0;
        }

        // If the cursor's visual line is outside the visible viewport, hide it.
        let scroll = effective_scroll as usize;
        if i < scroll || i >= scroll + area.height as usize {
            return None;
        }

        let screen_row = (i - scroll) as u16;
        Some((area.x + col, area.y + screen_row))
    }

    /// Compute the on-screen position of an arbitrary buffer byte offset.
    ///
    /// Returns `None` if the position is outside the visible viewport.
    /// Does not apply cursor-specific wrap-boundary adjustments — see
    /// [`cursor_pos_with_state`] for cursor positioning.
    pub fn screen_position_of(
        &self,
        pos: usize,
        area: Rect,
        state: TextAreaState,
    ) -> Option<(u16, u16)> {
        let tw = self.text_width(area);
        let lines = self.wrapped_lines(tw);
        let effective_scroll = self.effective_scroll(area.height, &lines, state.scroll);
        let i = Self::wrapped_line_index_by_start(&lines, pos)?;
        let ls = &lines[i];
        let col = self.display_width_of_range(ls.start, pos) as u16;

        let scroll = effective_scroll as usize;
        if i < scroll || i >= scroll + area.height as usize {
            return None;
        }

        let screen_row = (i - scroll) as u16;
        Some((area.x + col, area.y + screen_row))
    }

    /// Compute the on-screen cells covered by a buffer byte range.
    ///
    /// A soft-wrapped range can cross visual rows, so unlike
    /// [`screen_position_of`] this returns one height-1 [`Rect`] per visual
    /// row the range intersects, top to bottom, clamped to the content
    /// region (`text_width` columns — excludes any scrollbar column). Rows
    /// scrolled outside the viewport are skipped, so a partially visible
    /// range yields only its visible rows. Bytes belonging to no row (a
    /// `\n`, or whitespace dropped at a wrap boundary) are not covered;
    /// trailing spaces kept on a row are. Ranges that are empty, extend
    /// past the text, or have non-char-boundary endpoints yield no spans.
    pub fn screen_spans_of_range(
        &self,
        range: Range<usize>,
        area: Rect,
        state: TextAreaState,
    ) -> Vec<Rect> {
        let mut spans = Vec::new();
        if range.start >= range.end
            || range.end > self.text.len()
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
        {
            return spans;
        }
        let tw = self.text_width(area);
        let lines = self.wrapped_lines(tw);
        let scroll = self.effective_scroll(area.height, &lines, state.scroll) as usize;
        // Rows before the one containing `range.start` cannot intersect;
        // `None` (start ahead of the first row) falls back to scanning all.
        let first = Self::wrapped_line_index_by_start(&lines, range.start).unwrap_or(0);
        // Rendered content stops at `tw` columns; a row's trailing wrap
        // spaces can measure wider, so clamp to the content edge, not the
        // full area (whose last column may hold the scrollbar).
        let right_edge = area.x.saturating_add(tw);
        for (i, ls) in lines.iter().enumerate().skip(first) {
            if ls.start >= range.end {
                break;
            }
            if i < scroll {
                continue;
            }
            if i >= scroll + area.height as usize {
                break;
            }
            let seg_start = range.start.max(ls.start);
            let seg_end = range.end.min(ls.end);
            if seg_start >= seg_end {
                continue;
            }
            let start_x = area
                .x
                .saturating_add(self.display_width_of_range(ls.start, seg_start) as u16)
                .min(right_edge);
            let end_x = area
                .x
                .saturating_add(self.display_width_of_range(ls.start, seg_end) as u16)
                .min(right_edge);
            if start_x < end_x {
                spans.push(Rect {
                    x: start_x,
                    y: area.y + (i - scroll) as u16,
                    width: end_x - start_x,
                    height: 1,
                });
            }
        }
        spans
    }

    /// Map screen coordinates `(col, row)` to a buffer byte position.
    ///
    /// Returns `None` if `(col, row)` is outside the textarea `area`.
    ///
    /// Edge cases:
    /// - Click past end of a wrapped line → snaps to line end.
    /// - Click below all text → snaps to `text.len()`.
    /// - Click on an element → snaps to nearest element boundary (start or end).
    pub fn buffer_pos_at_screen(
        &self,
        col: u16,
        row: u16,
        area: Rect,
        state: TextAreaState,
    ) -> Option<usize> {
        // Outside the textarea area → None.
        if col < area.x || col >= area.x + area.width || row < area.y || row >= area.y + area.height
        {
            return None;
        }

        let tw = self.text_width(area);
        let lines = self.wrapped_lines(tw);
        let scroll = self.effective_scroll(area.height, &lines, state.scroll);

        let visual_row = (row - area.y) as usize + scroll as usize;

        // Below all text → end of text.
        if visual_row >= lines.len() {
            return Some(self.text.len());
        }

        let line = &lines[visual_row];
        let target_col = (col - area.x) as usize;
        // Clamp line.end to text length (safety measure for edge cases).
        let line_end = line.end.min(self.text.len());
        Some(
            self.display_col_to_buffer_pos(line.start, line_end, target_col)
                .0,
        )
    }

    /// Like `buffer_pos_at_screen` but also indicates whether the column
    /// fell on an element's display region.
    fn buffer_pos_at_screen_ex(
        &self,
        col: u16,
        row: u16,
        area: Rect,
        state: TextAreaState,
    ) -> Option<(usize, bool)> {
        if col < area.x || row < area.y {
            return None;
        }

        let tw = self.text_width(area);
        let lines = self.wrapped_lines(tw);
        let scroll = self.effective_scroll(area.height, &lines, state.scroll);

        let visual_row = (row - area.y) as usize + scroll as usize;

        if visual_row >= lines.len() {
            return Some((self.text.len(), false));
        }

        let line = &lines[visual_row];
        let target_col = (col - area.x) as usize;
        let line_end = line.end.min(self.text.len());
        Some(self.display_col_to_buffer_pos(line.start, line_end, target_col))
    }

    /// Return the element at screen coordinates, if any.
    ///
    /// Uses `buffer_pos_at_screen` to find the buffer position, then checks
    /// whether that position falls inside an element.
    pub fn element_at_screen(
        &self,
        col: u16,
        row: u16,
        area: Rect,
        state: TextAreaState,
    ) -> Option<&TextElement> {
        let (pos, hit_element) = self.buffer_pos_at_screen_ex(col, row, area, state)?;
        if hit_element {
            // hit_element means the column fell on an element's display.
            // pos may be elem start or elem end — match either.
            self.elements
                .iter()
                .find(|e| pos >= e.range.start && pos <= e.range.end && !e.range.is_empty())
        } else {
            self.elements
                .iter()
                .find(|e| pos >= e.range.start && pos < e.range.end)
        }
    }

    // ── Selection API ──

    /// Normalized selection range, expanded to element boundaries.
    ///
    /// Returns `None` if no selection is active or anchor == head (empty).
    pub fn selection_range(&self) -> Option<Range<usize>> {
        let sel = self.selection?;
        if sel.anchor == sel.head {
            return None;
        }
        let start = sel.anchor.min(sel.head);
        let end = sel.anchor.max(sel.head);
        let expanded = self.expand_range_to_element_boundaries(start..end);
        let mut clamped_start = expanded.start.min(self.text.len());
        let mut clamped_end = expanded.end.min(self.text.len());
        // Snap to char boundaries so a stale endpoint can never split a
        // multi-byte char (slicing in selected_text would panic).
        while clamped_start > 0 && !self.text.is_char_boundary(clamped_start) {
            clamped_start -= 1;
        }
        while clamped_end < self.text.len() && !self.text.is_char_boundary(clamped_end) {
            clamped_end += 1;
        }
        if clamped_start >= clamped_end {
            None
        } else {
            Some(clamped_start..clamped_end)
        }
    }

    /// Text within the current selection (buffer text, not display text).
    pub fn selected_text(&self) -> Option<String> {
        let range = self.selection_range()?;
        Some(self.text[range].to_string())
    }

    /// Clear the selection without affecting the clipboard.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Delete the selected range (if any). Returns `true` if text was deleted.
    ///
    /// This is a single undo step. After deletion, the cursor is placed at
    /// the start of the deleted range and the selection is cleared.
    pub fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection_range() else {
            return false;
        };
        let start = range.start;
        self.apply_edit_replacement(range, "", Some(MutationKind::Replace));
        self.set_cursor_inner(start.min(self.text.len()));
        self.post_mutate();
        self.selection = None;
        true
    }

    /// Insert `text`, replacing the active selection (if any) as a single undo step.
    pub fn insert_str_replacing_selection(&mut self, text: &str) {
        if self.selection_range().is_none() {
            self.clear_selection();
            self.insert_str(text);
            return;
        }
        self.begin_undo_group();
        self.delete_selection();
        self.insert_str(text);
        self.end_undo_group();
    }

    /// Set the selection programmatically.
    pub fn set_selection(&mut self, anchor: usize, head: usize) {
        self.selection = Some(Selection { anchor, head });
    }

    /// Collapse the active selection: cursor to `pos`, selection cleared.
    fn collapse_selection_to(&mut self, pos: usize) {
        self.set_cursor(pos);
        self.clear_selection();
    }

    /// Extend (or start) the selection; the anchor is sticky like browser text fields.
    fn extend_selection(&mut self, movement: Movement) {
        let anchor = match self.selection {
            Some(sel) => {
                // Move from the head — mouse selections park the cursor inside the highlight.
                if self.cursor() != sel.head {
                    self.set_cursor(sel.head);
                }
                sel.anchor
            }
            None => self.cursor(),
        };
        self.apply_movement(movement);
        // A movement that lands on the anchor selects nothing — no phantom
        // zero-width selection (e.g. Shift+Left at position 0).
        if self.cursor() == anchor {
            self.clear_selection();
        } else {
            self.set_selection(anchor, self.cursor());
        }
    }

    /// Take the clipboard contents (returns `None` if empty).
    ///
    /// This is the primary way for the host app to retrieve text
    /// that was selected by mouse drag / double-click / triple-click.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    /// Peek at the current clipboard content without consuming it.
    pub fn clipboard(&self) -> Option<&str> {
        self.clipboard.as_deref()
    }

    /// Replace the clipboard provider. The default is [`InternalClipboard`]
    /// (in-memory only). Pass an `arboard`-backed implementation to sync
    /// copy/cut/paste with the system clipboard.
    pub fn set_clipboard_provider(&mut self, provider: Box<dyn ClipboardProvider>) {
        self.clipboard_provider = provider;
    }

    // ── Element events ──

    /// Take the pending [`TextElementEvent`], if any.
    ///
    /// Call this after [`handle_mouse`](Self::handle_mouse) to check whether
    /// an element was clicked or hover-entered/left.
    pub fn poll_element_event(&mut self) -> Option<TextElementEvent> {
        self.pending_element_event.take()
    }

    /// Internal: set clipboard text via the provider AND the notification field.
    fn set_clipboard_text(&mut self, text: String) {
        if !text.is_empty() {
            self.clipboard_provider.set(&text);
            self.clipboard = Some(text);
        }
    }

    // ── Timers / tick ──

    /// Recommended poll timeout for the host event loop.
    ///
    /// When the textarea has pending timer-driven work (e.g. continuous
    /// drag-scrolling while the mouse is held outside the area), this
    /// returns `Some(ms)`.  The host should use this as the
    /// `event::poll` timeout.  When the poll times out without an event,
    /// call [`tick`](Self::tick).
    ///
    /// Returns `None` when no timer work is pending — the host can use
    /// its own default timeout.
    pub fn poll_timeout_ms(&self) -> Option<u64> {
        // Drag-scroll is the only timer-driven feature for now.
        self.pending_drag_scroll.as_ref()?;
        let interval = Self::drag_scroll_interval(self.drag_scroll_steps);
        Some(interval as u64)
    }

    /// Advance timer-driven work (called by the host when `poll` times
    /// out).  Returns a `MouseAction` describing what changed (typically
    /// `SelectionUpdated` for drag-scroll, or `Nothing`).
    pub fn tick(&mut self, area: Rect, state: TextAreaState) -> MouseAction {
        // Drag-scroll continuation.
        if let Some(event) = self.pending_drag_scroll {
            return self.handle_mouse(event, area, state);
        }
        MouseAction::Nothing
    }

    // ── Mouse ──

    /// Shared single/double-click treatment of a click that landed on an
    /// element display (`hit_element`): snap the cursor to the element
    /// start, anchor drags there, and emit [`TextElementEventKind::Click`].
    ///
    /// Returns `None` when the click was not on an element.
    fn element_click_snap(&mut self, pos: usize, hit_element: bool) -> Option<MouseAction> {
        if !hit_element {
            return None;
        }
        let elem = self
            .elements
            .iter()
            .find(|e| pos >= e.range.start && pos <= e.range.end && !e.range.is_empty())?;
        let id = elem.id;
        let start = elem.range.start;
        self.set_cursor_inner(start);
        self.preferred_col = None;
        self.drag_anchor = Some(start);
        self.pending_element_event = Some(TextElementEvent {
            id,
            kind: TextElementEventKind::Click,
        });
        Some(MouseAction::CursorPlaced)
    }

    /// Process a crossterm `MouseEvent` and return what happened.
    ///
    /// The host app is expected to call this from its event loop for
    /// every `Event::Mouse(mouse)` and pass the textarea's render `area`
    /// plus the current `TextAreaState` (for scroll info).
    pub fn handle_mouse(
        &mut self,
        event: MouseEvent,
        area: Rect,
        state: TextAreaState,
    ) -> MouseAction {
        // ── Scrollbar interaction ──
        // When scrollbar is shown, clicks/drags on the rightmost column
        // control the scroll position instead of placing the cursor.
        let tw = self.text_width(area);
        let has_scrollbar = self.show_scrollbar && tw < area.width;
        let on_scrollbar = has_scrollbar && event.column == area.x + area.width - 1;

        // Handle scrollbar drag continuation (even if pointer moved off the column).
        if self.scrollbar_dragging {
            match event.kind {
                MouseEventKind::Drag(MouseButton::Left)
                | MouseEventKind::Down(MouseButton::Left) => {
                    return self.handle_scrollbar_click(event.row, area, tw);
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.scrollbar_dragging = false;
                    return MouseAction::Scrolled;
                }
                _ => {}
            }
        }

        if on_scrollbar && let MouseEventKind::Down(MouseButton::Left) = event.kind {
            self.scrollbar_dragging = true;
            // If the click is on the thumb, don't jump — just start the drag
            // from the current position.  Only jump when clicking the track.
            if self.is_scrollbar_thumb_at(event.row, area, tw) {
                return MouseAction::Scrolled;
            }
            return self.handle_scrollbar_click(event.row, area, tw);
        }

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Some terminals re-emit Down(Left) after a scroll event
                // even though the button was held the whole time.  When a
                // drag is already active, treat this as a drag continuation
                // so the selection anchor is preserved.
                if self.drag_active {
                    return self.handle_mouse(
                        MouseEvent {
                            kind: MouseEventKind::Drag(MouseButton::Left),
                            ..event
                        },
                        area,
                        state,
                    );
                }

                let col = event.column;
                let row = event.row;

                // Track multi-click (double/triple).
                let click_count = self.click_tracker.register(col, row);

                // Record the mouse-down position (for drag detection).
                self.mouse_down_pos = Some((col, row));
                self.drag_active = false;
                self.last_drag_scroll = None;
                self.drag_scroll_steps = 0;
                self.pending_drag_scroll = None;

                // Clear any existing selection.
                self.clear_selection();

                // Map screen coordinates to buffer position.
                // IMPORTANT: this must happen BEFORE clearing scroll_override
                // so that effective_scroll uses the current viewport, not the
                // cursor-following fallback.
                let Some((pos, hit_element)) = self.buffer_pos_at_screen_ex(col, row, area, state)
                else {
                    self.scroll_override = None;
                    self.drag_anchor = None;
                    return MouseAction::Nothing;
                };

                // Now that we have the correct buffer position, clear the
                // scroll override so the viewport follows the cursor again.
                self.scroll_override = None;

                match click_count {
                    2 => {
                        // Double-click on an element display: snap like a
                        // single click (cursor to element start + Click
                        // event). Word-selecting would select and copy the
                        // element's hidden buffer text to the clipboard;
                        // the host decides what a chip double-click means.
                        // Triple-click line-select below intentionally keeps
                        // buffer-text semantics, element content included —
                        // a copy gesture, like drag-select across a chip.
                        if let Some(action) = self.element_click_snap(pos, hit_element) {
                            return action;
                        }
                        // Double-click: select word under cursor.
                        // Whitespace clicks just place the cursor (no selection).
                        let is_ws = pos < self.text.len()
                            && self.text[pos..]
                                .chars()
                                .next()
                                .is_none_or(|ch| ch.is_whitespace());
                        let start = self.word_start_at(pos);
                        let end = self.word_end_at(pos);
                        if !is_ws && start < end {
                            self.selection = Some(Selection {
                                anchor: start,
                                head: end,
                            });
                            // Place cursor on the last character of the
                            // selection (neovim style), not one past the end.
                            let cursor = self.text[start..end]
                                .char_indices()
                                .next_back()
                                .map(|(i, _)| start + i)
                                .unwrap_or(start);
                            self.set_cursor_inner(cursor);
                            self.preferred_col = None;
                            if let Some(text) = self.selected_text() {
                                self.set_clipboard_text(text);
                            }
                            return MouseAction::SelectionFinished;
                        }
                        // Clicked on whitespace — just place cursor.
                        self.set_cursor_inner(pos);
                        self.preferred_col = None;
                        MouseAction::CursorPlaced
                    }
                    3 => {
                        // Triple-click: select entire source line (\n-delimited).
                        let line_start = self.beginning_of_line(pos);
                        // Include the trailing \n if present.
                        let line_end_excl = self.end_of_line(pos);
                        let line_end = if line_end_excl < self.text.len() {
                            line_end_excl + 1 // include \n
                        } else {
                            line_end_excl
                        };
                        self.selection = Some(Selection {
                            anchor: line_start,
                            head: line_end,
                        });
                        // Keep cursor at the click position (like neovim),
                        // not at the end of the selection.
                        self.set_cursor_inner(pos);
                        self.preferred_col = None;
                        if let Some(text) = self.selected_text() {
                            self.set_clipboard_text(text);
                        }
                        MouseAction::SelectionFinished
                    }
                    _ => {
                        // Single click: place cursor.
                        //
                        // If click landed on an element display, snap cursor
                        // to elem start. `hit_element` is reliable because
                        // display_col_to_buffer_pos sets it when the column
                        // falls within an element's visual width.
                        if let Some(action) = self.element_click_snap(pos, hit_element) {
                            return action;
                        }

                        self.drag_anchor = Some(pos);
                        self.set_cursor_inner(pos);
                        self.preferred_col = None;

                        MouseAction::CursorPlaced
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(anchor) = self.drag_anchor else {
                    return MouseAction::Nothing;
                };

                // Compute the buffer position for the drag endpoint.
                // We need to scope the `lines` borrow so it's dropped before
                // we mutate self.

                // Throttle drag-scroll (above/below area) to avoid
                // lightning-fast scrolling at mouse-report rate.
                // Acceleration: first step waits 80ms, then 60ms, then 40ms.
                let outside_area = event.row < area.y || event.row >= area.y + area.height;
                if outside_area {
                    // Store event for continuous drag-scroll re-triggering.
                    self.pending_drag_scroll = Some(event);

                    let now = Instant::now();
                    let interval = Self::drag_scroll_interval(self.drag_scroll_steps);
                    if let Some(last) = self.last_drag_scroll
                        && now.duration_since(last).as_millis() < interval
                    {
                        return MouseAction::Nothing;
                    }
                    self.last_drag_scroll = Some(now);
                    self.drag_scroll_steps = self.drag_scroll_steps.saturating_add(1);
                } else {
                    // Back inside area — cancel continuous drag-scroll.
                    self.pending_drag_scroll = None;
                }

                let (head, new_scroll) = {
                    let tw = self.text_width(area);
                    let lines = self.wrapped_lines(tw);
                    let scroll = self.effective_scroll(area.height, &lines, state.scroll) as usize;
                    let visible_end = scroll + area.height as usize;

                    if event.row < area.y {
                        // ── Dragging above the area → scroll up ──
                        let dist = area.y - event.row;
                        let n = Self::drag_scroll_lines_for_distance(dist);
                        let target_line = scroll.saturating_sub(n);
                        let pos = if target_line < lines.len() {
                            let col = event.column.saturating_sub(area.x) as usize;
                            let line = &lines[target_line];
                            let line_end = line.end.min(self.text.len());
                            let p = self.display_col_to_buffer_pos(line.start, line_end, col).0;
                            self.clamp_to_line(p, line.start, line_end)
                        } else {
                            0
                        };
                        (pos, Some(target_line as u16))
                    } else if event.row >= area.y + area.height {
                        // ── Dragging below the area → scroll down ──
                        let dist = event.row - (area.y + area.height) + 1;
                        let n = Self::drag_scroll_lines_for_distance(dist);
                        let target_line = (visible_end + n - 1).min(lines.len().saturating_sub(1));
                        let max_scroll = lines.len().saturating_sub(area.height as usize);
                        let new_scroll = (target_line + 1)
                            .saturating_sub(area.height as usize)
                            .min(max_scroll);
                        let pos = if target_line < lines.len() {
                            let col = event.column.saturating_sub(area.x) as usize;
                            let line = &lines[target_line];
                            let line_end = line.end.min(self.text.len());
                            let pos = self.display_col_to_buffer_pos(line.start, line_end, col).0;
                            self.clamp_to_line(pos, line.start, line_end)
                        } else {
                            self.text.len()
                        };
                        (pos, Some(new_scroll as u16))
                    } else {
                        // ── Within the area → normal drag ──
                        let col = event.column.clamp(area.x, area.x + tw.saturating_sub(1));
                        let row = event.row;
                        drop(lines); // release borrow for buffer_pos_at_screen
                        match self.buffer_pos_at_screen(col, row, area, state) {
                            Some(pos) => (pos, None),
                            None => return MouseAction::Nothing,
                        }
                    }
                };

                if let Some(s) = new_scroll {
                    self.scroll_override = Some(s);
                }
                if head == anchor {
                    self.drag_active = false;
                    self.selection = None;
                } else {
                    self.drag_active = true;
                    self.selection = Some(Selection { anchor, head });
                }
                self.set_cursor_inner(head);
                self.preferred_col = None;

                if self.selection.is_some() {
                    MouseAction::SelectionUpdated
                } else {
                    MouseAction::CursorPlaced
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.mouse_down_pos = None;
                let was_drag = self.drag_active;
                self.drag_active = false;
                self.scrollbar_dragging = false;
                self.pending_drag_scroll = None;
                self.drag_anchor = None;

                if was_drag {
                    // Discard zero-width selections (anchor == head) that arise
                    // from mouse jitter — they look like an active selection to
                    // the keyboard handler and silently swallow Backspace/Delete.
                    if self.selection_range().is_none() {
                        self.selection = None;
                        MouseAction::CursorPlaced
                    } else {
                        // Finalize selection: copy to clipboard.
                        if let Some(text) = self.selected_text()
                            && !text.is_empty()
                        {
                            self.set_clipboard_text(text);
                        }

                        if !self.keep_selection_after_mouseup {
                            self.selection = None;
                        }

                        MouseAction::SelectionFinished
                    }
                } else {
                    MouseAction::Nothing
                }
            }
            MouseEventKind::ScrollDown => {
                let tw = self.text_width(area);
                let lines = self.wrapped_lines(tw);
                let total = lines.len();
                if total <= area.height as usize {
                    return MouseAction::Nothing;
                }
                let max_scroll = total.saturating_sub(area.height as usize) as u16;
                let current = self
                    .scroll_override
                    .unwrap_or_else(|| self.effective_scroll(area.height, &lines, state.scroll));
                let scroll_lines = Self::scroll_lines_for_height(area.height);
                let new_scroll = (current + scroll_lines).min(max_scroll);
                if new_scroll == current {
                    return MouseAction::Nothing;
                }
                // If dragging, extend the selection head to follow the scroll.
                let drag_new_pos = if self.drag_active {
                    let target_line =
                        (new_scroll as usize + area.height as usize - 1).min(lines.len() - 1);
                    Some(lines[target_line].start)
                } else {
                    None
                };
                drop(lines);
                self.scroll_override = Some(new_scroll);
                if let Some(new_pos) = drag_new_pos {
                    if let Some(sel) = &mut self.selection {
                        sel.head = new_pos;
                    }
                    self.set_cursor_inner(new_pos);
                }
                MouseAction::Scrolled
            }
            MouseEventKind::ScrollUp => {
                let tw = self.text_width(area);
                let lines = self.wrapped_lines(tw);
                let total = lines.len();
                if total <= area.height as usize {
                    return MouseAction::Nothing;
                }
                let current = self
                    .scroll_override
                    .unwrap_or_else(|| self.effective_scroll(area.height, &lines, state.scroll));
                let scroll_lines = Self::scroll_lines_for_height(area.height);
                let new_scroll = current.saturating_sub(scroll_lines);
                if new_scroll == current {
                    return MouseAction::Nothing;
                }
                // If dragging, extend the selection head to follow the scroll.
                let drag_new_pos = if self.drag_active {
                    let target_line = new_scroll as usize;
                    Some(if target_line < lines.len() {
                        lines[target_line].start
                    } else {
                        0
                    })
                } else {
                    None
                };
                drop(lines);
                self.scroll_override = Some(new_scroll);
                if let Some(new_pos) = drag_new_pos {
                    if let Some(sel) = &mut self.selection {
                        sel.head = new_pos;
                    }
                    self.set_cursor_inner(new_pos);
                }
                MouseAction::Scrolled
            }
            MouseEventKind::Moved => {
                // Hover detection: hit-test elements under the cursor.
                let hovered_id = self
                    .element_at_screen(event.column, event.row, area, state)
                    .map(|e| e.id);

                let prev = self.hovered_element;
                if hovered_id != prev {
                    // Emit leave for the old element first, then enter for the new one.
                    // We only store the last event; if both happen, prefer enter
                    // (the caller already knows about the old element from a prior enter).
                    if let Some(old_id) = prev {
                        self.pending_element_event = Some(TextElementEvent {
                            id: old_id,
                            kind: TextElementEventKind::HoverLeave,
                        });
                    }
                    if let Some(new_id) = hovered_id {
                        self.pending_element_event = Some(TextElementEvent {
                            id: new_id,
                            kind: TextElementEventKind::HoverEnter,
                        });
                    }
                    self.hovered_element = hovered_id;
                }
                MouseAction::Nothing
            }
            _ => MouseAction::Nothing,
        }
    }

    /// Handle a click or drag on the scrollbar track.
    ///
    /// Maps the row position proportionally to a scroll offset:
    /// clicking at the top of the track scrolls to the start, at the
    /// bottom scrolls to the end.
    fn handle_scrollbar_click(&mut self, row: u16, area: Rect, tw: u16) -> MouseAction {
        if area.height == 0 {
            return MouseAction::Nothing;
        }
        let total = {
            let lines = self.wrapped_lines(tw);
            lines.len()
        };
        if total <= area.height as usize {
            return MouseAction::Nothing;
        }
        let max_scroll = total.saturating_sub(area.height as usize) as u16;
        let rel_row = row.saturating_sub(area.y);
        // Map relative row to a scroll offset proportionally.
        let scroll = if area.height <= 1 {
            0
        } else {
            ((rel_row as u32 * max_scroll as u32) / (area.height.saturating_sub(1)) as u32) as u16
        };
        self.scroll_override = Some(scroll.min(max_scroll));
        MouseAction::Scrolled
    }

    /// Check whether the given screen row falls on the scrollbar thumb.
    ///
    /// Renders the scrollbar into a scratch buffer and checks whether the
    /// cell at `row` is a non-space character (thumb glyph) or a space (track).
    fn is_scrollbar_thumb_at(&self, row: u16, area: Rect, tw: u16) -> bool {
        if area.height == 0 {
            return false;
        }
        let total = {
            let lines = self.wrapped_lines(tw);
            lines.len()
        };
        if total <= area.height as usize {
            return false;
        }
        let current_scroll = self.scroll_override.unwrap_or(0);

        let lengths = ScrollLengths {
            content_len: total,
            viewport_len: area.height as usize,
        };
        let scrollbar = ScrollBar::vertical(lengths).offset(current_scroll as usize);
        let sb_x = area.right().saturating_sub(1);
        let core_area = CoreRect {
            x: sb_x,
            y: area.y,
            width: 1,
            height: area.height,
        };
        let mut scratch = CoreBuffer::empty(core_area);
        (&scrollbar).render(core_area, &mut scratch);

        if row < area.y || row >= area.y + area.height {
            return false;
        }
        scratch[(sb_x, row)].symbol() != " "
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn is_word_char(ch: char) -> bool {
        ch.is_alphanumeric() || ch == '_'
    }

    /// Classify a character into a word-class for double-click selection.
    ///
    /// Three classes (matching vim/neovim `w` word definition):
    /// - `0`: whitespace
    /// - `1`: word chars (alphanumeric + underscore)
    /// - `2`: punctuation / everything else
    fn char_class(ch: char) -> u8 {
        if ch.is_whitespace() {
            0
        } else if Self::is_word_char(ch) {
            1
        } else {
            2
        }
    }

    /// Find the start of the word containing `pos` (for double-click selection).
    ///
    /// Uses vim-style word classes: word chars (alphanumeric + `_`), punctuation,
    /// and whitespace are three distinct groups.  Scans backward until the class
    /// changes.
    ///
    /// If `pos` is inside an element, returns the element start.
    fn word_start_at(&self, pos: usize) -> usize {
        // If inside an element, return element start.
        if let Some(elem) = self
            .elements
            .iter()
            .find(|e| pos >= e.range.start && pos < e.range.end)
        {
            return elem.range.start;
        }

        // Determine the class of the character at `pos` (or just before if at end).
        let target_class = if pos < self.text.len() {
            Self::char_class(self.text[pos..].chars().next().unwrap())
        } else if pos > 0 {
            let ch = self.text[..pos].chars().next_back().unwrap();
            Self::char_class(ch)
        } else {
            return 0;
        };

        let before = &self.text[..pos];
        let word_start = before
            .char_indices()
            .rev()
            .find(|&(_, ch)| Self::char_class(ch) != target_class)
            .map(|(idx, ch)| idx + ch.len_utf8())
            .unwrap_or(0);
        self.adjust_pos_out_of_elements(word_start, true)
    }

    /// Find the end of the word containing `pos` (for double-click selection).
    ///
    /// Uses vim-style word classes (see [`Self::char_class`]).
    ///
    /// If `pos` is inside an element, returns the element end.
    fn word_end_at(&self, pos: usize) -> usize {
        // If inside an element, return element end.
        if let Some(elem) = self
            .elements
            .iter()
            .find(|e| pos >= e.range.start && pos < e.range.end)
        {
            return elem.range.end;
        }

        // Determine the class of the character at `pos`.
        let target_class = if pos < self.text.len() {
            Self::char_class(self.text[pos..].chars().next().unwrap())
        } else {
            return self.text.len();
        };

        let after = &self.text[pos..];
        let word_end = after
            .char_indices()
            .find(|&(_, ch)| Self::char_class(ch) != target_class)
            .map(|(rel_idx, _)| pos + rel_idx)
            .unwrap_or(self.text.len());
        self.adjust_pos_out_of_elements(word_end, false)
    }

    fn current_display_col(&self) -> usize {
        let bol = self.beginning_of_current_line();
        self.display_width_of_range(bol, self.cursor())
    }

    /// Compute the display width of the buffer range `[from..to)`.
    ///
    /// Plain runs use tab-aware width (`tab_width` columns per `\t`, or
    /// unicode-width when `tab_width == 0`). Element ranges with a custom
    /// `display` use the element's display width instead of the buffer text
    /// width. This is the core of the display projection system.
    fn display_width_of_range(&self, from: usize, to: usize) -> usize {
        if from >= to {
            return 0;
        }
        let mut width = 0usize;
        let mut pos = from;

        for elem in &self.elements {
            if elem.range.start >= to {
                break; // elements are sorted, no more overlap possible
            }
            if elem.range.end <= pos {
                continue; // element is entirely before our current position
            }

            // Plain text before this element
            if pos < elem.range.start {
                let plain_end = elem.range.start.min(to);
                width += self.plain_display_width(&self.text[pos..plain_end]);
                pos = plain_end;
            }
            if pos >= to {
                break;
            }

            // Element region
            let elem_start_in_range = elem.range.start.max(pos);
            let elem_end_in_range = elem.range.end.min(to);
            if elem_start_in_range < elem_end_in_range {
                if let Some(display) = &elem.display {
                    // If the range covers the entire element (or starts at element start),
                    // use the full display width. If it covers only a partial overlap
                    // (cursor inside element — shouldn't happen normally), fall back to
                    // buffer text width.
                    if elem_start_in_range == elem.range.start {
                        let display_w: usize = display
                            .spans
                            .iter()
                            .map(|s| s.content.as_ref().width())
                            .sum();
                        width += display_w;
                    } else {
                        width += self.plain_display_width(
                            &self.text[elem_start_in_range..elem_end_in_range],
                        );
                    }
                } else {
                    width += self
                        .plain_display_width(&self.text[elem_start_in_range..elem_end_in_range]);
                }
                pos = elem_end_in_range;
            }
        }

        // Remaining plain text after all elements
        if pos < to {
            width += self.plain_display_width(&self.text[pos..to]);
        }

        width
    }

    fn wrapped_line_index_by_start(lines: &[Range<usize>], pos: usize) -> Option<usize> {
        // partition_point returns the index of the first element for which
        // the predicate is false, i.e. the count of elements with start <= pos.
        let idx = lines.partition_point(|r| r.start <= pos);
        if idx == 0 { None } else { Some(idx - 1) }
    }

    /// Map a display column to a buffer byte position on a given wrapped line.
    ///
    /// Pure query — does not mutate any state. Handles elements (snapping to
    /// nearest element boundary) and wide unicode graphemes.
    /// If `target_col` is past the line's display width, returns `line_end`
    /// (clamped to the nearest element boundary).
    ///
    /// Returns `(byte_pos, hit_element)` where `hit_element` is `true` when
    /// the column fell on an element's display region.
    fn display_col_to_buffer_pos(
        &self,
        line_start: usize,
        line_end: usize,
        target_col: usize,
    ) -> (usize, bool) {
        let mut width_so_far = 0usize;
        let mut pos = line_start;

        while pos < line_end {
            // Check if pos is at or inside an element
            if let Some(elem_idx) = self
                .elements
                .iter()
                .position(|e| pos >= e.range.start && pos < e.range.end)
            {
                let elem = &self.elements[elem_idx];
                let elem_start = elem.range.start;
                let elem_buf_end = elem.range.end;
                // The visible portion of the element on this line
                let elem_line_end = elem_buf_end.min(line_end);

                if pos == elem_start {
                    // We're at the start of an element — treat it as a whole unit.
                    let elem_display_w = if let Some(display) = &elem.display {
                        display
                            .spans
                            .iter()
                            .map(|s| s.content.as_ref().width())
                            .sum()
                    } else {
                        self.plain_display_width(&self.text[elem_start..elem_line_end])
                    };

                    if width_so_far + elem_display_w > target_col {
                        // Click landed on this element display — snap to the
                        // nearer boundary (start vs end of the underlying
                        // buffer text) so that drag-selection works naturally.
                        let dist_start = target_col.saturating_sub(width_so_far);
                        let dist_end = elem_display_w.saturating_sub(dist_start);
                        if dist_start <= dist_end {
                            return (elem_start, true);
                        } else {
                            return (elem_buf_end, true);
                        }
                    }
                    width_so_far += elem_display_w;
                    pos = elem_buf_end.min(line_end); // move past element (or to line end)
                } else {
                    // We're in the middle of an element (e.g. a wrapped line starts
                    // mid-element). Skip past the rest of the element on this line.
                    let partial_w = self.plain_display_width(&self.text[pos..elem_line_end]);
                    if width_so_far + partial_w > target_col {
                        // Snap to element's actual end boundary
                        return (elem_buf_end, true);
                    }
                    width_so_far += partial_w;
                    pos = elem_buf_end.min(line_end); // move past element (or to line end)
                }
                continue;
            }

            // Plain text grapheme
            let slice = &self.text[pos..line_end];
            if let Some(grapheme) = slice.graphemes(true).next() {
                let grapheme_width = self.grapheme_display_width(grapheme);
                width_so_far += grapheme_width;
                if width_so_far > target_col {
                    return (self.clamp_pos_to_nearest_boundary(pos), false);
                }
                pos += grapheme.len();
            } else {
                break;
            }
        }

        (self.clamp_pos_to_nearest_boundary(line_end), false)
    }

    fn move_to_display_col_on_line(
        &mut self,
        line_start: usize,
        line_end: usize,
        target_col: usize,
    ) {
        let cursor = self
            .display_col_to_buffer_pos(line_start, line_end, target_col)
            .0;
        self.set_cursor_inner(cursor);
    }

    fn beginning_of_line(&self, pos: usize) -> usize {
        // Scan backward for '\n' that is NOT inside an element.
        // Newlines inside elements (e.g. multi-line paste) are not line boundaries.
        for i in (0..pos).rev() {
            if self.text.as_bytes()[i] == b'\n' && !self.is_inside_element(i) {
                return i + 1;
            }
        }
        0
    }
    fn beginning_of_current_line(&self) -> usize {
        self.beginning_of_line(self.cursor())
    }

    fn end_of_line(&self, pos: usize) -> usize {
        // Scan forward for '\n' that is NOT inside an element.
        for i in pos..self.text.len() {
            if self.text.as_bytes()[i] == b'\n' && !self.is_inside_element(i) {
                return i;
            }
        }
        self.text.len()
    }
    fn end_of_current_line(&self) -> usize {
        self.end_of_line(self.cursor())
    }

    /// Check if a byte position is inside (strictly within) an element.
    fn is_inside_element(&self, pos: usize) -> bool {
        self.elements
            .iter()
            .any(|e| pos >= e.range.start && pos < e.range.end)
    }

    fn apply_classified_command(&mut self, command: EditCommand) {
        if let EditCommand::Insert(character) = command {
            self.insert_str(&character.to_string());
            return;
        }
        let mutation_kind = match command.category() {
            EditCommandCategory::Insert => unreachable!("insert commands return above"),
            EditCommandCategory::Navigation => None,
            EditCommandCategory::Delete => Some(MutationKind::Delete),
            EditCommandCategory::Kill => Some(MutationKind::Kill),
        };
        self.apply_edit_command(command, mutation_kind);
    }

    /// Execute a resolved cursor [`Movement`] (see [`resolve_movement`]).
    fn apply_movement(&mut self, movement: Movement) {
        match movement {
            Movement::Command(command, _) => {
                self.apply_edit_command(command, None);
            }
            Movement::VisualRowUp => self.move_cursor_up(),
            Movement::VisualRowDown => self.move_cursor_down(),
            Movement::VisualRowStart => self.move_cursor_to_beginning_of_line(false),
            Movement::VisualRowEnd => self.move_cursor_to_end_of_line(false),
            Movement::LogicalLineStart => self.set_cursor(self.beginning_of_current_line()),
            Movement::LogicalLineEnd => self.set_cursor(self.end_of_current_line()),
        }
    }

    pub fn input(&mut self, event: KeyEvent) {
        // ── Shift+movement extends the selection (browser-style) ──
        // Super+Up/Down never extend (terminals claim Cmd+Up/Down); they fall
        // through to the same plain movement as before this feature.
        if event.modifiers.contains(KeyModifiers::SHIFT)
            && !(matches!(event.code, KeyCode::Up | KeyCode::Down)
                && event.modifiers.contains(KeyModifiers::SUPER))
        {
            // Windows Win32 input reports shifted letters uppercase — fold the
            // case so Ctrl+Shift+A/E/P/N classify like their lowercase forms.
            let code = match event.code {
                KeyCode::Char(c) if c.is_ascii_uppercase() => KeyCode::Char(c.to_ascii_lowercase()),
                code => code,
            };
            let unshifted = KeyEvent::new(code, event.modifiers.difference(KeyModifiers::SHIFT));
            if let Some(movement) = resolve_movement(&unshifted) {
                self.extend_selection(movement);
                return;
            }
        }

        // ── Selection-aware interception ──
        if self.selection.is_some() {
            let classified = classify_key_event(&event);
            if let Some(EditCommand::Insert(character)) = classified {
                self.insert_str_replacing_selection(&character.to_string());
                return;
            }
            let category = classified.map(EditCommand::category);
            // Movement → collapse to the directional edge, then move FROM that edge.
            // (Zero-width selections fail the range check and take the catch-all below.)
            if let Some(range) = self.selection_range()
                && let Some(movement) = resolve_movement(&event)
            {
                let edge = match movement.collapse_edge() {
                    HorizontalEdge::Start => range.start,
                    HorizontalEdge::End => range.end,
                };
                // Vertical continuation keeps the sticky column across the
                // collapse (browser goal-column behavior); horizontal moves reset it.
                let sticky_col = self.preferred_col;
                self.collapse_selection_to(edge);
                if matches!(movement, Movement::VisualRowUp | Movement::VisualRowDown) {
                    self.preferred_col = sticky_col;
                }
                if !movement.stops_at_collapse_edge() {
                    self.apply_movement(movement);
                }
                return;
            }
            match event {
                // Enter / Ctrl-J/M → replace selection with newline.
                KeyEvent {
                    code: KeyCode::Char('j' | 'm'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => {
                    self.insert_str_replacing_selection("\n");
                    return;
                }
                // Delete-family chords delete just the selection; kills stash it for yank.
                _ if matches!(
                    category,
                    Some(EditCommandCategory::Delete | EditCommandCategory::Kill)
                ) =>
                {
                    if category == Some(EditCommandCategory::Kill)
                        && let Some(text) = self.selected_text()
                    {
                        self.kill_buffer = text;
                    }
                    if self.delete_selection() {
                        return;
                    }
                    // Zero-width selection — clear and fall through.
                    self.clear_selection();
                }
                // Ctrl-X (exact, matching the pre-selection binding) / Cmd+X → cut selection.
                KeyEvent {
                    code: KeyCode::Char('x'),
                    modifiers,
                    ..
                } if modifiers == KeyModifiers::CONTROL
                    || modifiers.contains(KeyModifiers::SUPER) =>
                {
                    if let Some(text) = self.selected_text() {
                        self.set_clipboard_text(text);
                    }
                    if self.delete_selection() {
                        return;
                    }
                    // Zero-width selection — clear and fall through.
                    self.clear_selection();
                }
                // Cmd+C → copy selection, keeping the highlight (browser semantics).
                KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::SUPER) => {
                    if let Some(text) = self.selected_text() {
                        self.set_clipboard_text(text);
                    } else {
                        // Zero-width — nothing to copy; drop the stale selection.
                        self.clear_selection();
                    }
                    return;
                }
                // Ctrl+Y/V fall through, selection intact — the paste arms below replace it.
                KeyEvent {
                    code: KeyCode::Char('y' | 'v'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } => {}
                // All other keys → clear selection, fall through to normal handling.
                _ => {
                    self.clear_selection();
                }
            }
        }

        if let Some(movement) = resolve_movement(&event) {
            self.apply_movement(movement);
            return;
        }

        if let Some(command) = classify_key_event(&event) {
            self.apply_classified_command(command);
            return;
        }

        match event {
            KeyEvent {
                code: KeyCode::Char('j' | 'm'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                ..
            } => self.insert_str("\n"),
            KeyEvent {
                code: KeyCode::Char('y'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.yank();
            }

            // Undo / Redo (Ctrl or Cmd)
            KeyEvent {
                code: KeyCode::Char('Z'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL)
                || modifiers.contains(KeyModifiers::SUPER) =>
            {
                // Ctrl/Cmd-Shift-Z → redo (terminals that report uppercase Z + Shift)
                self.redo();
            }
            k if is_undo_input(&k) => {
                self.undo();
            }
            KeyEvent {
                code: KeyCode::Char('r'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.redo();
            }

            // Ctrl-V → paste from clipboard provider, replacing any selection.
            KeyEvent {
                code: KeyCode::Char('v'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                if let Some(text) = self.clipboard_provider.get() {
                    self.insert_str_replacing_selection(&text);
                }
            }

            _o => {
                #[cfg(feature = "debug-logs")]
                tracing::debug!("Unhandled key event in TextArea: {:?}", _o);
            }
        }
    }

    // ── Undo/Redo ──

    /// Create a snapshot of the current textarea state.
    fn snapshot(&self) -> UndoEntry {
        UndoEntry {
            text: self.text().to_owned(),
            cursor: self.cursor(),
            elements: self.elements.clone(),
        }
    }

    /// Restore the textarea state from a snapshot.
    fn restore(&mut self, entry: UndoEntry) {
        self.text = EditBuffer::from_parts(entry.text, entry.cursor);
        self.elements = entry.elements;
        self.wrap_cache.replace(None);
        self.preferred_col = None;
        // Note: next_element_id is intentionally NOT restored — it only increases.
        // Note: kill_buffer is intentionally NOT restored — yank is separate from undo.
    }

    /// Called before a mutation to decide whether to push a new undo checkpoint.
    ///
    /// Batching rules:
    /// - Inside an undo group (`group_depth > 0`) → skip entirely.
    /// - First mutation ever → always checkpoint.
    /// - Kind changed from last → checkpoint.
    /// - Cursor moved since last mutation (arrows, clicks) → checkpoint.
    /// - Kill / Element / Replace → always checkpoint (discrete actions).
    /// - Same Insert or Delete with consecutive cursor → extend batch (no checkpoint).
    /// - Word boundary (ws↔non-ws transition) → checkpoint (handled by callers
    ///   resetting `last_kind` before calling this method).
    fn pre_mutate(&mut self, kind: MutationKind) {
        // Inside an undo group — the group handles its own checkpoint.
        if self.undo.group_depth > 0 {
            return;
        }

        let should_push = match self.undo.last_kind {
            None => true,
            Some(prev) => {
                prev != kind
                    || self.cursor() != self.undo.last_cursor
                    || matches!(
                        kind,
                        MutationKind::Kill | MutationKind::Element | MutationKind::Replace
                    )
            }
        };

        if should_push {
            let entry = self.snapshot();
            self.undo.stack.push(entry);
            if self.undo.stack.len() > self.undo.max_depth {
                self.undo.stack.remove(0);
            }
        }
        self.undo.redo.clear();
        self.undo.last_kind = Some(kind);
    }

    /// Update `last_cursor` after a mutation completes so the next `pre_mutate`
    /// can detect cursor jumps.
    fn post_mutate(&mut self) {
        self.undo.last_cursor = self.cursor();
    }

    /// Clear the undo/redo history, leaving the current text and cursor
    /// untouched.
    ///
    /// Use this when a buffer is reset to represent a *new logical
    /// context* — e.g. a shared input widget that is reused for a
    /// different target — so that a later `undo` can't resurrect text
    /// that belonged to the previous context. `set_text` deliberately
    /// records a checkpoint (so an accidental replace is undoable), so
    /// callers that want a hard reset must follow it with this.
    pub fn clear_history(&mut self) {
        self.undo.stack.clear();
        self.undo.redo.clear();
        self.undo.last_kind = None;
        self.undo.last_cursor = self.cursor();
    }

    /// Undo the last mutation. Returns `true` if there was something to undo.
    pub fn undo(&mut self) -> bool {
        if let Some(entry) = self.undo.stack.pop() {
            self.scroll_override = None;
            let current = self.snapshot();
            self.undo.redo.push(current);
            self.restore(entry);
            // Reset batching — next mutation starts a fresh group.
            self.undo.last_kind = None;
            self.undo.last_cursor = self.cursor();
            true
        } else {
            false
        }
    }

    /// Redo the last undone mutation. Returns `true` if there was something to redo.
    pub fn redo(&mut self) -> bool {
        if let Some(entry) = self.undo.redo.pop() {
            self.scroll_override = None;
            let current = self.snapshot();
            self.undo.stack.push(current);
            self.restore(entry);
            // Reset batching — next mutation starts a fresh group.
            self.undo.last_kind = None;
            self.undo.last_cursor = self.cursor();
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undo.redo.is_empty()
    }

    /// Begin an undo group. All mutations between `begin_undo_group()` and
    /// `end_undo_group()` are collapsed into a single undo step.
    ///
    /// Groups can be nested: only the outermost `end_undo_group()` pushes
    /// the checkpoint. Inner begin/end pairs are reference-counted.
    ///
    /// Use cases:
    /// - Autocomplete: `replace_range_with_element` + `insert_str(" ")` = 1 undo step
    /// - Line-select: enter → N live-updates → confirm = 1 undo step
    pub fn begin_undo_group(&mut self) {
        if self.undo.group_depth == 0 {
            // Outermost group — take the snapshot.
            self.undo.group_checkpoint = Some(self.snapshot());
        }
        self.undo.group_depth += 1;
    }

    /// End an undo group. If this closes the outermost group and the state
    /// actually changed, a single undo entry is pushed.
    pub fn end_undo_group(&mut self) {
        if self.undo.group_depth == 0 {
            return; // Unbalanced call — ignore.
        }
        self.undo.group_depth -= 1;
        if self.undo.group_depth == 0 {
            if let Some(checkpoint) = self.undo.group_checkpoint.take() {
                // Only push if state actually changed.
                let changed = checkpoint.text.as_str() != self.text()
                    || checkpoint.cursor != self.cursor()
                    || checkpoint.elements.len() != self.elements.len();
                if changed {
                    self.undo.stack.push(checkpoint);
                    if self.undo.stack.len() > self.undo.max_depth {
                        self.undo.stack.remove(0);
                    }
                    self.undo.redo.clear();
                }
            }
            // Reset batching state so the next mutation starts fresh.
            self.undo.last_kind = None;
            self.undo.last_cursor = self.cursor();
        }
    }

    /// Cancel an undo group. Restores the textarea to the state it was in
    /// when `begin_undo_group()` was called — no undo entry is created.
    ///
    /// Use case: line-select cancel → revert all live-updates, leave no trace.
    pub fn cancel_undo_group(&mut self) {
        if self.undo.group_depth == 0 {
            return; // Unbalanced call — ignore.
        }
        // Always restore to the outermost checkpoint, regardless of nesting.
        self.undo.group_depth = 0;
        if let Some(checkpoint) = self.undo.group_checkpoint.take() {
            self.restore(checkpoint);
        }
        // Reset batching state.
        self.undo.last_kind = None;
        self.undo.last_cursor = self.cursor();
    }

    // ####### Input Functions #######
    pub fn delete_backward(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        if n == 1 {
            self.apply_edit_command(
                EditCommand::DeleteGraphemeBackward,
                Some(MutationKind::Delete),
            );
            return;
        }
        self.begin_undo_group();
        for _ in 0..n {
            if matches!(
                self.apply_edit_command(
                    EditCommand::DeleteGraphemeBackward,
                    Some(MutationKind::Delete),
                ),
                EditOutcome::Unchanged
            ) {
                break;
            }
        }
        self.end_undo_group();
    }

    pub fn delete_forward(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        if n == 1 {
            self.apply_edit_command(
                EditCommand::DeleteGraphemeForward,
                Some(MutationKind::Delete),
            );
            return;
        }
        self.begin_undo_group();
        for _ in 0..n {
            if matches!(
                self.apply_edit_command(
                    EditCommand::DeleteGraphemeForward,
                    Some(MutationKind::Delete),
                ),
                EditOutcome::Unchanged
            ) {
                break;
            }
        }
        self.end_undo_group();
    }

    pub fn delete_backward_word(&mut self) {
        self.apply_edit_command(
            EditCommand::DeleteWordBackward(WordStyle::Small),
            Some(MutationKind::Kill),
        );
    }

    /// readline `unix-word-rubout` (whitespace-delimited), vs
    /// [`Self::delete_backward_word`]'s punctuation-chunked M-DEL semantics.
    pub fn delete_backward_unix_word(&mut self) {
        self.apply_edit_command(
            EditCommand::DeleteWordBackward(WordStyle::WhitespaceDelimited),
            Some(MutationKind::Kill),
        );
    }

    /// Delete text to the right of the cursor using readline-style word semantics.
    ///
    /// Deletes from the current cursor position through the end of the next word as determined
    /// by `end_of_next_word()`. Any delimiters between the cursor and that word
    /// (whitespace, punctuation, newlines) are included in the deletion.
    pub fn delete_forward_word(&mut self) {
        self.apply_edit_command(
            EditCommand::DeleteWordForward(WordStyle::Small),
            Some(MutationKind::Kill),
        );
    }

    pub fn kill_to_end_of_line(&mut self) {
        self.apply_edit_command(EditCommand::DeleteToLineEnd, Some(MutationKind::Kill));
    }

    pub fn kill_to_beginning_of_line(&mut self) {
        self.apply_edit_command(EditCommand::DeleteToLineStart, Some(MutationKind::Kill));
    }

    /// Kill the entire current line (BOL to EOL), regardless of cursor position.
    /// If the line is already empty, consumes the preceding newline to join lines.
    pub fn kill_current_line(&mut self) {
        let bol = self.beginning_of_current_line();
        let eol = self.end_of_current_line();

        let range = if bol == eol {
            if bol > 0 { Some(bol - 1..bol) } else { None }
        } else {
            Some(bol..eol)
        };

        if let Some(range) = range {
            self.apply_edit_replacement(range, "", Some(MutationKind::Kill));
        }
    }

    pub fn yank(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        // Yank over a highlight replaces it as a single undo step.
        let replacing_selection = self.selection_range().is_some();
        if replacing_selection {
            self.begin_undo_group();
            self.delete_selection();
        }
        let text = self.kill_buffer.clone();
        self.apply_edit_replacement(
            self.cursor()..self.cursor(),
            &text,
            Some(MutationKind::Insert),
        );
        if replacing_selection {
            self.end_undo_group();
        }
        if let Some(last) = text.chars().last() {
            self.undo.last_insert_ws = last.is_whitespace();
        }
    }

    /// Move the cursor left by a single grapheme cluster.
    pub fn move_cursor_left(&mut self) {
        self.apply_edit_command(EditCommand::MoveGraphemeLeft, None);
    }

    /// Move the cursor right by a single grapheme cluster.
    pub fn move_cursor_right(&mut self) {
        self.apply_edit_command(EditCommand::MoveGraphemeRight, None);
    }

    pub fn move_cursor_up(&mut self) {
        self.scroll_override = None;
        // If we have a wrapping cache, prefer navigating across wrapped (visual) lines.
        if let Some((target_col, maybe_line)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some(idx) = Self::wrapped_line_index_by_start(lines, self.cursor()) {
                    let cur_range = &lines[idx];
                    let target_col = self.preferred_col.unwrap_or_else(|| {
                        self.display_width_of_range(cur_range.start, self.cursor())
                    });
                    if idx > 0 {
                        let prev = &lines[idx - 1];
                        let line_start = prev.start;
                        let line_end = prev.end;
                        Some((target_col, Some((line_start, line_end))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            // We had wrapping info. Apply movement accordingly.
            match maybe_line {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col_on_line(line_start, line_end, target_col);
                    return;
                }
                None => {
                    // Already at first visual line -> move to start
                    self.set_cursor_inner(0);
                    self.preferred_col = None;
                    return;
                }
            }
        }

        // Fallback to logical line navigation if we don't have wrapping info yet.
        if let Some(prev_nl) = self.text[..self.cursor()].rfind('\n') {
            let target_col = match self.preferred_col {
                Some(c) => c,
                None => {
                    let c = self.current_display_col();
                    self.preferred_col = Some(c);
                    c
                }
            };
            let prev_line_start = self.text[..prev_nl].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let prev_line_end = prev_nl;
            self.move_to_display_col_on_line(prev_line_start, prev_line_end, target_col);
        } else {
            self.set_cursor_inner(0);
            self.preferred_col = None;
        }
    }

    pub fn move_cursor_down(&mut self) {
        self.scroll_override = None;
        // If we have a wrapping cache, prefer navigating across wrapped (visual) lines.
        if let Some((target_col, move_to_last)) = {
            let cache_ref = self.wrap_cache.borrow();
            if let Some(cache) = cache_ref.as_ref() {
                let lines = &cache.lines;
                if let Some(idx) = Self::wrapped_line_index_by_start(lines, self.cursor()) {
                    let cur_range = &lines[idx];
                    let target_col = self.preferred_col.unwrap_or_else(|| {
                        self.display_width_of_range(cur_range.start, self.cursor())
                    });
                    if idx + 1 < lines.len() {
                        let next = &lines[idx + 1];
                        let line_start = next.start;
                        let line_end = next.end;
                        Some((target_col, Some((line_start, line_end))))
                    } else {
                        Some((target_col, None))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } {
            match move_to_last {
                Some((line_start, line_end)) => {
                    if self.preferred_col.is_none() {
                        self.preferred_col = Some(target_col);
                    }
                    self.move_to_display_col_on_line(line_start, line_end, target_col);
                    return;
                }
                None => {
                    // Already on last visual line -> move to end
                    self.set_cursor_inner(self.text.len());
                    self.preferred_col = None;
                    return;
                }
            }
        }

        // Fallback to logical line navigation if we don't have wrapping info yet.
        let target_col = match self.preferred_col {
            Some(c) => c,
            None => {
                let c = self.current_display_col();
                self.preferred_col = Some(c);
                c
            }
        };
        if let Some(next_nl) = self.text[self.cursor()..]
            .find('\n')
            .map(|i| i + self.cursor())
        {
            let next_line_start = next_nl + 1;
            let next_line_end = self.text[next_line_start..]
                .find('\n')
                .map(|i| i + next_line_start)
                .unwrap_or(self.text.len());
            self.move_to_display_col_on_line(next_line_start, next_line_end, target_col);
        } else {
            self.set_cursor_inner(self.text.len());
            self.preferred_col = None;
        }
    }

    /// Home / Super+Left when `move_up_at_bol` is false (visual row if wrapped);
    /// Ctrl+A when true (logical line; already-at-BOL chains to previous line).
    pub fn move_cursor_to_beginning_of_line(&mut self, move_up_at_bol: bool) {
        if move_up_at_bol {
            self.apply_edit_command(EditCommand::MoveLogicalLineStart, None);
            return;
        }
        if let Some(bol) = self.beginning_of_current_visual_line() {
            self.set_cursor(bol);
            return;
        }

        let bol = self.beginning_of_current_line();
        self.set_cursor(bol);
    }

    /// End / Super+Right when `move_down_at_eol` is false (visual row if wrapped);
    /// Ctrl+E when true (logical line; already-at-EOL chains to next line).
    pub fn move_cursor_to_end_of_line(&mut self, move_down_at_eol: bool) {
        if move_down_at_eol {
            self.apply_edit_command(EditCommand::MoveLogicalLineEnd, None);
            return;
        }
        if let Some(eol) = self.end_of_current_visual_line() {
            self.set_cursor(eol);
            return;
        }

        let eol = self.end_of_current_line();
        self.set_cursor(eol);
    }

    fn beginning_of_current_visual_line(&self) -> Option<usize> {
        let cache = self.wrap_cache.borrow();
        let cache = cache.as_ref()?;
        let idx = Self::wrapped_line_index_by_start(&cache.lines, self.cursor())?;
        Some(cache.lines[idx].start)
    }

    /// Soft-continued visual rows land on the last char (exclusive end is the
    /// next row's start). Final segment of a logical line uses exclusive end.
    fn end_of_current_visual_line(&self) -> Option<usize> {
        let cache = self.wrap_cache.borrow();
        let cache = cache.as_ref()?;
        let idx = Self::wrapped_line_index_by_start(&cache.lines, self.cursor())?;
        let line = &cache.lines[idx];
        let end = line.end.min(self.text.len());
        let soft_continued = cache
            .lines
            .get(idx + 1)
            .is_some_and(|next| next.start == end);
        if soft_continued && end > line.start {
            Some(self.clamp_to_line(end, line.start, end))
        } else {
            Some(end)
        }
    }

    // ===== Text elements support =====

    /// Insert an atomic text element at the current cursor position.
    ///
    /// The `text` is inserted into the buffer and registered as an element.
    /// The `kind` tag is opaque to the textarea (host-defined).
    /// The `display` optionally overrides how the element is rendered.
    ///
    /// Returns the assigned [`ElementId`] so the host can store associated metadata.
    pub fn insert_element(
        &mut self,
        text: &str,
        kind: ElementKind,
        display: Option<Line<'static>>,
    ) -> ElementId {
        let plan = self.plan_edit_replacement(self.cursor()..self.cursor(), text);
        self.apply_element_transaction(plan, kind, display)
    }

    /// Replace a range of buffer text with an atomic element.
    ///
    /// This is the "confirm autocomplete" operation: the trigger text (e.g. `@foo`)
    /// is deleted and replaced with element text (e.g. `@src/foo.rs`) in a single
    /// atomic operation. The cursor is placed at the end of the new element.
    ///
    /// Returns the assigned [`ElementId`].
    pub fn replace_range_with_element(
        &mut self,
        range: Range<usize>,
        text: &str,
        kind: ElementKind,
        display: Option<Line<'static>>,
    ) -> ElementId {
        let plan = self.plan_edit_replacement(range, text);
        self.apply_element_transaction(plan, kind, display)
    }

    fn apply_element_transaction(
        &mut self,
        plan: EditPlan,
        kind: ElementKind,
        display: Option<Line<'static>>,
    ) -> ElementId {
        let start = plan.replaced_byte_range().start;
        let inserted_len = plan.replacement().len();
        self.assert_valid_edit_plan(&plan);
        self.pre_mutate(MutationKind::Element);
        self.apply_validated_edit_plan(plan, Some(MutationKind::Element));
        let end = start + inserted_len;
        let id = self.add_element(start..end, kind, display);
        self.set_cursor(end);
        self.post_mutate();
        id
    }

    fn add_element(
        &mut self,
        range: Range<usize>,
        kind: ElementKind,
        display: Option<Line<'static>>,
    ) -> ElementId {
        let id = ElementId(self.next_element_id);
        self.next_element_id += 1;
        let elem = TextElement {
            id,
            range,
            kind,
            display,
        };
        self.elements.push(elem);
        self.elements.sort_by_key(|e| e.range.start);
        self.wrap_cache.replace(None);
        id
    }

    /// Returns the element at the current cursor position, if any.
    ///
    /// If the cursor is at an element's start boundary, that element is returned.
    /// If the cursor is strictly inside an element (shouldn't happen in normal
    /// operation), the containing element is returned.
    pub fn element_at_cursor(&self) -> Option<&TextElement> {
        self.elements
            .iter()
            .find(|e| self.cursor() >= e.range.start && self.cursor() < e.range.end)
    }

    /// Returns the underlying buffer text for the element with the given id.
    pub fn element_text(&self, id: ElementId) -> Option<&str> {
        self.elements
            .iter()
            .find(|e| e.id == id)
            .map(|e| &self.text[e.range.clone()])
    }

    /// Update the display for an existing element. Invalidates the wrap cache.
    pub fn set_element_display(&mut self, id: ElementId, display: Option<Line<'static>>) {
        if let Some(e) = self.elements.iter_mut().find(|e| e.id == id) {
            e.display = display;
            self.wrap_cache.replace(None);
        }
    }

    /// Returns a slice of all elements, sorted by buffer position.
    pub fn elements(&self) -> &[TextElement] {
        &self.elements
    }

    /// Re-register elements after a [`set_text`] call that placed their
    /// buffer text back verbatim. Each `(range, kind, display)` tuple
    /// describes one element whose text already occupies `range` in the
    /// buffer. No text is inserted — this only recreates the element
    /// metadata so the textarea renders chips instead of raw text.
    pub fn restore_elements(
        &mut self,
        elems: impl IntoIterator<Item = (Range<usize>, ElementKind, Option<Line<'static>>)>,
    ) {
        for (range, kind, display) in elems {
            self.add_element(range, kind, display);
        }
        self.wrap_cache.replace(None);
    }

    /// Inline an element: remove it from the element list so its buffer text
    /// becomes plain editable characters. The text content is unchanged.
    ///
    /// The cursor is placed at the end of the inlined region.
    /// This operation is a single undoable step.
    ///
    /// Returns `true` if the element was found and inlined, `false` otherwise.
    pub fn inline_element(&mut self, id: ElementId) -> bool {
        let Some(idx) = self.elements.iter().position(|e| e.id == id) else {
            return false;
        };
        let end = self.elements[idx].range.end;

        // Snapshot for undo before removing the element.
        self.pre_mutate(MutationKind::Element);

        self.elements.remove(idx);
        self.set_cursor_inner(end);
        self.preferred_col = None;
        self.wrap_cache.replace(None);
        self.undo.last_kind = None; // always discrete

        true
    }

    /// Get the contiguous non-whitespace "word" that the cursor is inside or at the start of.
    ///
    /// Returns `(byte_range, text)` where `byte_range` is the range in the buffer.
    /// Returns `None` if the cursor is on whitespace or the buffer is empty.
    ///
    /// This is useful for trigger-character detection (e.g. finding `@foo` under the cursor
    /// for autocomplete). The host can then check `text.starts_with('@')` etc.
    pub fn word_at_cursor(&self) -> Option<(Range<usize>, &str)> {
        if self.text.is_empty() {
            return None;
        }
        let pos = self.cursor().min(self.text.len());

        // Find word start: scan backward from cursor to find whitespace boundary
        let start = self.text[..pos]
            .rfind(|c: char| c.is_whitespace())
            .map(|i| {
                i + self.text[i..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1)
            })
            .unwrap_or(0);

        // Find word end: scan forward from cursor to find whitespace boundary
        let end = self.text[pos..]
            .find(|c: char| c.is_whitespace())
            .map(|i| i + pos)
            .unwrap_or(self.text.len());

        // Also extend backward from start in case cursor is at word boundary
        // Actually, we also need to handle cursor being between words.
        // If cursor is at whitespace, return None.
        if start >= end {
            return None;
        }

        // If cursor is beyond the word end (cursor at whitespace after word), return None
        // Unless cursor is exactly at start position of the word
        let word = &self.text[start..end];
        if word.chars().all(|c| c.is_whitespace()) {
            return None;
        }

        Some((start..end, word))
    }

    fn find_element_containing(&self, pos: usize) -> Option<usize> {
        self.elements
            .iter()
            .position(|e| pos > e.range.start && pos < e.range.end)
    }

    fn clamp_pos_to_nearest_boundary(&self, mut pos: usize) -> usize {
        if pos > self.text.len() {
            pos = self.text.len();
        }
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            let dist_start = pos.saturating_sub(e.range.start);
            let dist_end = e.range.end.saturating_sub(pos);
            if dist_start <= dist_end {
                e.range.start
            } else {
                e.range.end
            }
        } else {
            pos
        }
    }

    fn expand_range_to_element_boundaries(&self, mut range: Range<usize>) -> Range<usize> {
        // Expand to include any intersecting elements fully
        loop {
            let mut changed = false;
            for e in &self.elements {
                if e.range.start < range.end && e.range.end > range.start {
                    let new_start = range.start.min(e.range.start);
                    let new_end = range.end.max(e.range.end);
                    if new_start != range.start || new_end != range.end {
                        range.start = new_start;
                        range.end = new_end;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        range
    }

    fn shift_elements(&mut self, at: usize, removed: usize, inserted: usize) {
        // Generic shift: for pure insert, removed = 0; for delete, inserted = 0.
        let end = at + removed;
        let diff = inserted as isize - removed as isize;
        // Remove elements fully deleted by the operation and shift the rest
        self.elements
            .retain(|e| !(e.range.start >= at && e.range.end <= end));
        for e in &mut self.elements {
            if e.range.end <= at {
                // before edit
            } else if e.range.start >= end {
                // after edit
                e.range.start = ((e.range.start as isize) + diff) as usize;
                e.range.end = ((e.range.end as isize) + diff) as usize;
            } else {
                // Overlap with element but not fully contained (shouldn't happen when using
                // element-aware replace, but degrade gracefully by snapping element to new bounds)
                let new_start = at.min(e.range.start);
                let new_end = at + inserted.max(e.range.end.saturating_sub(end));
                e.range.start = new_start;
                e.range.end = new_end;
            }
        }
    }

    fn update_elements_after_replace(&mut self, start: usize, end: usize, inserted_len: usize) {
        self.shift_elements(start, end.saturating_sub(start), inserted_len);
    }

    /// Move to the beginning of the previous navigable chunk.
    ///
    /// Word characters are alphanumeric plus `_`. Punctuation runs (such as
    /// `-`) are their own chunk, so moving left across `aa-bb` stops at the
    /// right side of `-`, then the left side of `-`, then the start of `aa`.
    /// Whitespace is skipped over. Elements remain atomic units.
    pub fn beginning_of_previous_word(&self) -> usize {
        let ranges = self.element_ranges();
        self.text
            .plan_command(EditCommand::MoveWordLeft(WordStyle::Small), &ranges)
            .cursor_byte()
    }

    /// Start of the previous whitespace-delimited WORD; elements count as
    /// non-whitespace.
    pub fn beginning_of_previous_unix_word(&self) -> usize {
        let ranges = self.element_ranges();
        self.text
            .plan_command(
                EditCommand::MoveWordLeft(WordStyle::WhitespaceDelimited),
                &ranges,
            )
            .cursor_byte()
    }

    /// Move to the end of the next navigable chunk.
    ///
    /// Word characters are alphanumeric plus `_`. Punctuation runs (such as
    /// `-`) are their own chunk, so moving right across `aa-bb` stops at the
    /// left side of `-`, then the right side of `-`, then the end of `bb`.
    /// Whitespace is skipped over. Elements remain atomic units.
    pub fn end_of_next_word(&self) -> usize {
        let ranges = self.element_ranges();
        self.text
            .plan_command(EditCommand::MoveWordRight(WordStyle::Small), &ranges)
            .cursor_byte()
    }

    fn adjust_pos_out_of_elements(&self, pos: usize, prefer_start: bool) -> usize {
        if let Some(idx) = self.find_element_containing(pos) {
            let e = &self.elements[idx];
            if prefer_start {
                e.range.start
            } else {
                e.range.end
            }
        } else {
            pos
        }
    }

    #[expect(clippy::unwrap_used)]
    fn wrapped_lines(&self, width: u16) -> Ref<'_, Vec<Range<usize>>> {
        // A zero-width terminal must not reach textwrap — it can produce
        // borrowed empty slices that don't point into the input buffer,
        // causing out-of-bounds panics in wrap_ranges pointer arithmetic.
        let width = width.max(1);
        // Ensure cache is ready (potentially mutably borrow, then drop)
        {
            let mut cache = self.wrap_cache.borrow_mut();
            let needs_recalc = match cache.as_ref() {
                Some(c) => c.width != width,
                None => true,
            };
            if needs_recalc {
                let lines = if self.elements.iter().any(|e| e.display.is_some()) {
                    self.element_aware_wrap_ranges(width as usize)
                } else {
                    crate::wrapping::wrap_ranges(
                        &self.text,
                        Options::new(width as usize)
                            .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
                    )
                };
                *cache = Some(WrapCache { width, lines });
            }
        }

        let cache = self.wrap_cache.borrow();
        Ref::map(cache, |c| &c.as_ref().unwrap().lines)
    }

    /// Element-display-aware greedy wrapping.
    ///
    /// Produces wrap ranges where each range is `start..end` with `end` being
    /// the exclusive byte position of the content (including any trailing
    /// spaces that belong to this visual line).
    ///
    /// Elements are treated as atomic units for wrapping: if an element's
    /// display width doesn't fit on the current line, the element is moved
    /// to a new line (like word-wrap). If it doesn't fit on *any* line
    /// (wider than terminal), it gets its own line and rendering truncates it.
    fn element_aware_wrap_ranges(&self, width: usize) -> Vec<Range<usize>> {
        let width = width.max(1);
        let mut result = Vec::new();

        // Process each logical line (split by \n), but skip \n inside elements.
        // Newlines inside elements are internal to the element and must not create
        // visual line breaks — the element's display is a single-line chip.
        let mut seg_start = 0;
        loop {
            let seg_end = self.next_logical_newline(seg_start);

            self.greedy_wrap_segment(seg_start, seg_end, width, &mut result);

            if seg_end >= self.text.len() {
                break;
            }
            seg_start = seg_end + 1; // skip \n
        }

        if result.is_empty() {
            result.push(0..0);
        }

        result
    }

    /// Find the next `\n` at or after `from` that is NOT inside an element.
    ///
    /// Returns `self.text.len()` if no such newline exists.
    fn next_logical_newline(&self, from: usize) -> usize {
        let mut pos = from;
        while pos < self.text.len() {
            // If pos is inside an element, skip past the entire element.
            if let Some(elem) = self
                .elements
                .iter()
                .find(|e| pos >= e.range.start && pos < e.range.end)
            {
                pos = elem.range.end;
                continue;
            }
            if self.text.as_bytes()[pos] == b'\n' {
                return pos;
            }
            pos += 1;
        }
        self.text.len()
    }

    /// Greedy-wrap a single logical line (no \n inside `start..end`).
    fn greedy_wrap_segment(
        &self,
        start: usize,
        end: usize,
        width: usize,
        result: &mut Vec<Range<usize>>,
    ) {
        if start >= end {
            // Empty logical line
            result.push(start..end);
            return;
        }

        let mut line_start = start;
        let mut pos = start;
        let mut display_w: usize = 0;
        // Position right after the last break opportunity (start of the next word/element).
        let mut last_break_pos: Option<usize> = None;

        while pos < end {
            // Check if pos is at the start of an element
            if let Some(elem) = self
                .elements
                .iter()
                .find(|e| pos == e.range.start && e.range.start < e.range.end)
            {
                let elem_end = elem.range.end.min(end);
                let elem_dw: usize = if let Some(display) = &elem.display {
                    display
                        .spans
                        .iter()
                        .map(|s| s.content.as_ref().width())
                        .sum()
                } else {
                    self.plain_display_width(&self.text[elem.range.start..elem_end])
                };

                if display_w > 0 && display_w + elem_dw > width {
                    // Element doesn't fit on current line — break before it.
                    let break_at = last_break_pos.unwrap_or(pos);
                    result.push(line_start..break_at);
                    line_start = break_at;
                    // Skip leading spaces/tabs on the new line
                    while line_start < end
                        && line_start < pos
                        && matches!(self.text.as_bytes().get(line_start), Some(b' ' | b'\t'))
                    {
                        line_start += 1;
                    }
                    pos = line_start;
                    display_w = 0;
                    last_break_pos = None;
                    continue;
                }

                display_w += elem_dw;
                pos = elem_end;
                // After element is a break opportunity
                last_break_pos = Some(pos);
                continue;
            }

            // Plain text grapheme cluster
            let slice = &self.text[pos..end];
            let Some(grapheme) = slice.graphemes(true).next() else {
                break;
            };
            let grapheme_width = self.grapheme_display_width(grapheme);

            if display_w + grapheme_width > width && display_w > 0 {
                // Need to wrap
                let break_at = last_break_pos.unwrap_or(pos);
                result.push(line_start..break_at);
                line_start = break_at;
                // Skip leading spaces/tabs on the new line
                while line_start < end
                    && line_start < pos
                    && matches!(self.text.as_bytes().get(line_start), Some(b' ' | b'\t'))
                {
                    line_start += 1;
                }
                display_w = self.display_width_of_range(line_start, pos);
                last_break_pos = None;
                if line_start == pos {
                    // No break opportunity found; break at current position (break_words).
                    display_w = grapheme_width;
                    pos += grapheme.len();
                }
                continue;
            }

            if grapheme == " " || grapheme == "\t" {
                // Space/tab is a break opportunity; break point is after it.
                last_break_pos = Some(pos + grapheme.len());
            }

            display_w += grapheme_width;
            pos += grapheme.len();
        }

        // Final visual line of this logical line
        result.push(line_start..end);
    }

    /// Calculate the scroll offset that should be used to satisfy the
    /// invariants given the current area size and wrapped lines.
    ///
    /// - Cursor is always on screen.
    /// - No scrolling if content fits in the area.
    fn effective_scroll(
        &self,
        area_height: u16,
        lines: &[Range<usize>],
        current_scroll: u16,
    ) -> u16 {
        let total_lines = lines.len() as u16;
        if area_height >= total_lines {
            return 0;
        }

        let max_scroll = total_lines.saturating_sub(area_height);

        // If we have an internal scroll override (from mousewheel), use it
        // — but still clamp to valid range.
        if let Some(ovr) = self.scroll_override {
            return ovr.min(max_scroll);
        }

        // Where is the cursor within wrapped lines? Prefer assigning boundary positions
        // (where pos equals the start of a wrapped line) to that later line.
        let cursor_line_idx =
            Self::wrapped_line_index_by_start(lines, self.cursor()).unwrap_or(0) as u16;

        let mut scroll = current_scroll.min(max_scroll);

        // Ensure cursor is visible within [scroll, scroll + area_height)
        if cursor_line_idx < scroll {
            scroll = cursor_line_idx;
        } else if cursor_line_idx >= scroll + area_height {
            scroll = cursor_line_idx + 1 - area_height;
        }
        scroll
    }

    /// Compute the effective content width for text wrapping, accounting for
    /// the scrollbar column.  Uses a 2-shot approach:
    ///
    /// 1. Wrap at full `area_width` to get line count.
    /// 2. If scrollbar needed (lines > height) and `show_scrollbar`, reduce
    ///    width by 1 for the scrollbar track.
    ///
    /// Returns `(content_width, needs_scrollbar)`.
    fn content_width(&self, area_width: u16, area_height: u16) -> (u16, bool) {
        if !self.show_scrollbar || area_width <= 1 {
            return (area_width, false);
        }
        // First shot — wrap at full width to check if content overflows.
        let lines = self.wrapped_lines(area_width);
        let needs = lines.len() as u16 > area_height;
        if needs {
            // 1 for scrollbar track + padding gap
            let reserved = 1 + self.scrollbar_padding;
            (area_width.saturating_sub(reserved), true)
        } else {
            (area_width, false)
        }
    }

    /// Convenience: content width for wrapping (area width minus scrollbar if needed).
    fn text_width(&self, area: Rect) -> u16 {
        self.content_width(area.width, area.height).0
    }
}

impl WidgetRef for &TextArea {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let (cw, needs_sb) = self.content_width(area.width, area.height);
        let content_area = Rect { width: cw, ..area };
        let lines = self.wrapped_lines(cw);
        self.render_lines(content_area, buf, &lines, 0..lines.len());
        if needs_sb {
            self.render_scrollbar(area, buf, lines.len() as u16, area.height, 0);
        }
    }
}

impl StatefulWidgetRef for &TextArea {
    type State = TextAreaState;

    fn render_ref(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let (cw, needs_sb) = self.content_width(area.width, area.height);
        let content_area = Rect { width: cw, ..area };
        let lines = self.wrapped_lines(cw);
        let scroll = self.effective_scroll(area.height, &lines, state.scroll);
        state.scroll = scroll;

        let start = scroll as usize;
        let end = (scroll + area.height).min(lines.len() as u16) as usize;
        self.render_lines(content_area, buf, &lines, start..end);
        if needs_sb {
            self.render_scrollbar(area, buf, lines.len() as u16, area.height, scroll);
        }
    }
}

impl TextArea {
    /// Render a scrollbar in the rightmost column of `area`.
    ///
    /// Uses `tui_scrollbar::ScrollBar` rendered into a scratch ratatui-core
    /// buffer, then copies cells into the main buffer with muted styling.
    fn render_scrollbar(
        &self,
        area: Rect,
        buf: &mut Buffer,
        total_lines: u16,
        viewport_lines: u16,
        offset: u16,
    ) {
        if total_lines <= viewport_lines || area.width == 0 || area.height == 0 {
            return;
        }

        let sb_area = Rect {
            x: area.right().saturating_sub(1),
            y: area.y,
            width: 1,
            height: area.height,
        };

        let lengths = ScrollLengths {
            content_len: total_lines as usize,
            viewport_len: viewport_lines as usize,
        };
        let scrollbar = ScrollBar::vertical(lengths).offset(offset as usize);

        // Render into ratatui-core scratch buffer then copy with styling.
        let core_area = CoreRect {
            x: sb_area.x,
            y: sb_area.y,
            width: sb_area.width,
            height: sb_area.height,
        };
        let mut scratch = CoreBuffer::empty(core_area);
        (&scrollbar).render(core_area, &mut scratch);

        let track_style = self.scrollbar_track_style;
        let thumb_style = self.scrollbar_thumb_style;

        for row in 0..sb_area.height {
            let x = sb_area.x;
            let y = sb_area.y + row;
            let src = &scratch[(x, y)];
            let dst = &mut buf[(x, y)];
            let symbol = src.symbol();
            dst.set_symbol(symbol);
            if symbol == " " {
                dst.set_style(track_style);
            } else {
                dst.set_style(thumb_style);
            }
        }
    }

    fn render_lines(
        &self,
        area: Rect,
        buf: &mut Buffer,
        lines: &[Range<usize>],
        range: std::ops::Range<usize>,
    ) {
        let area_right = area.x + area.width; // exclusive right boundary
        let sel_range = self.selection_range();

        for (row, idx) in range.enumerate() {
            let r = &lines[idx];
            let y = area.y + row as u16;
            let line_range = r.start..r.end;

            // Render the line segment-by-segment (plain text → element → plain text → …)
            // using display-aware x positioning. This ensures that when an element's
            // display text is wider (or narrower) than its buffer text, all subsequent
            // content is positioned correctly.
            let mut display_x: u16 = 0; // current display column
            let mut buf_pos = line_range.start; // current position in the buffer

            // Collect elements that overlap this visual line, in order.
            let overlapping: Vec<&TextElement> = self
                .elements
                .iter()
                .filter(|e| {
                    let os = e.range.start.max(line_range.start);
                    let oe = e.range.end.min(line_range.end);
                    os < oe
                })
                .collect();

            for elem in &overlapping {
                let overlap_start = elem.range.start.max(line_range.start);
                let overlap_end = elem.range.end.min(line_range.end);

                // 1. Render plain text before this element (buf_pos..overlap_start)
                if buf_pos < overlap_start && display_x < area.width {
                    let plain = &self.text[buf_pos..overlap_start];
                    let avail = (area.width - display_x) as usize;
                    let (paint, paint_w) = paint_plain_for_display(plain, avail, self.tab_width);
                    buf.set_string(area.x + display_x, y, paint.as_ref(), Style::default());
                    display_x += paint_w as u16;
                }

                // 2. Render the element
                if display_x >= area.width {
                    buf_pos = overlap_end;
                    continue;
                }

                let avail = (area.width - display_x) as usize;

                if let Some(display) = &elem.display {
                    if overlap_start == elem.range.start {
                        // First visual line of the element — render display text.
                        let display = truncate_line_display(display, avail);
                        for span in &display.spans {
                            let content = span.content.as_ref();
                            let w = content.width() as u16;
                            if display_x >= area.width {
                                break;
                            }
                            buf.set_string(area.x + display_x, y, content, span.style);
                            display_x += w;
                        }
                    }
                    // If element spans multiple visual lines but has a display,
                    // subsequent lines show nothing for this element region (blank).
                    // display_x doesn't advance (already blank in the buffer).
                } else {
                    // No custom display: render buffer text with default element style.
                    let styled = &self.text[overlap_start..overlap_end];
                    let style = Style::default().fg(Color::Cyan);
                    let (paint, paint_w) = paint_plain_for_display(styled, avail, self.tab_width);
                    buf.set_string(area.x + display_x, y, paint.as_ref(), style);
                    display_x += paint_w as u16;
                }

                buf_pos = overlap_end;
            }

            // 3. Render any remaining plain text after the last element
            if buf_pos < line_range.end && display_x < area.width {
                let plain = &self.text[buf_pos..line_range.end];
                let avail = (area.width - display_x) as usize;
                let (paint, paint_w) = paint_plain_for_display(plain, avail, self.tab_width);
                buf.set_string(area.x + display_x, y, paint.as_ref(), Style::default());
                // Keep display_x consistent with earlier segments (selection uses
                // display_width_of_range on a second pass).
                let _painted_end = display_x.saturating_add(paint_w as u16);
                let _ = _painted_end;
            }

            // 4. Apply selection highlight (second pass over cells)
            if let Some(sel_range) = &sel_range {
                // Intersect the selection with this visual line's buffer range.
                let line_sel_start = sel_range.start.max(line_range.start);
                let line_sel_end = sel_range.end.min(line_range.end);
                if line_sel_start < line_sel_end {
                    // Compute display column range for the selected portion.
                    let col_start =
                        self.display_width_of_range(line_range.start, line_sel_start) as u16;
                    let col_end =
                        self.display_width_of_range(line_range.start, line_sel_end) as u16;
                    let col_start = col_start.min(area.width);
                    let col_end = col_end.min(area.width);
                    for cx in col_start..col_end {
                        let cell = &mut buf[(area.x + cx, y)];
                        cell.set_style(self.selection_style);
                    }
                }
            }

            let _ = area_right; // suppress unused warning (used for documentation)
        }
    }
}

/// Expand `\t` to a fixed number of spaces (`tab_width`), matching scrollback.
fn expand_tabs_with_width(text: &str, tab_width: u8) -> std::borrow::Cow<'_, str> {
    if tab_width == 0 || !text.contains('\t') {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(text.replace('\t', &" ".repeat(tab_width as usize)))
}

fn grapheme_display_width_with_tab(grapheme: &str, tab_width: u8) -> usize {
    if grapheme == "\t" {
        if tab_width == 0 {
            0
        } else {
            tab_width as usize
        }
    } else {
        grapheme.width()
    }
}

fn plain_display_width_with_tab(text: &str, tab_width: u8) -> usize {
    if tab_width == 0 || !text.contains('\t') {
        return text.width();
    }
    text.graphemes(true)
        .map(|g| grapheme_display_width_with_tab(g, tab_width))
        .sum()
}

/// Clip a string to fit within `max_width` display columns (tabs = 0 width).
/// Returns a substring that is at most `max_width` columns wide.
fn clip_str_to_display_width(s: &str, max_width: usize) -> &str {
    clip_str_to_display_width_with_tab(s, max_width, 0)
}

/// Clip considering tabs as `tab_width` columns (byte index into original `s`).
fn clip_str_to_display_width_with_tab(s: &str, max_width: usize, tab_width: u8) -> &str {
    let mut width = 0;
    for (i, grapheme) in s.grapheme_indices(true) {
        let grapheme_width = grapheme_display_width_with_tab(grapheme, tab_width);
        if width + grapheme_width > max_width {
            return &s[..i];
        }
        width += grapheme_width;
    }
    s
}

/// Clip and expand tabs so paint width matches cursor/display-width math.
/// Returns (paint string, display columns used). Borrows when no expansion needed.
fn paint_plain_for_display(
    s: &str,
    max_width: usize,
    tab_width: u8,
) -> (std::borrow::Cow<'_, str>, usize) {
    let clipped = clip_str_to_display_width_with_tab(s, max_width, tab_width);
    let paint = expand_tabs_with_width(clipped, tab_width);
    let w = plain_display_width_with_tab(clipped, tab_width);
    (paint, w)
}

/// Truncate a display `Line` to fit within `max_width` columns.
///
/// If the line fits, it is returned as-is (cloned). If it overflows:
/// - Reserve 1 column for `…`.
/// - **Bracket-preservation heuristic:** if the display text ends with a closing
///   bracket (`]`, `)`, `}`, `>`), preserve it so e.g. `[Pasted ~10 lines]`
///   becomes `[Pasted ~1…]` rather than `[Pasted ~10…`.
/// - Otherwise, truncate and append `…`.
fn truncate_line_display(line: &Line<'static>, max_width: usize) -> Line<'static> {
    use ratatui::text::Span;

    let total_width: usize = line.spans.iter().map(|s| s.content.as_ref().width()).sum();
    if total_width <= max_width {
        return line.clone();
    }
    if max_width == 0 {
        return Line::default();
    }

    // Determine if we should preserve a closing bracket.
    let last_char = line
        .spans
        .iter()
        .rev()
        .find_map(|s| s.content.as_ref().chars().last());
    let (preserve_bracket, bracket_char, bracket_style) = match last_char {
        Some(ch @ (']' | ')' | '}' | '>')) => {
            // Find the style of the last span containing this char.
            let style = line.spans.last().map(|s| s.style).unwrap_or_default();
            (true, Some(ch), style)
        }
        _ => (false, None, Style::default()),
    };

    // Budget: max_width minus 1 for '…', minus 1 for bracket if preserving.
    // If max_width is too small for both ellipsis and bracket, skip bracket.
    let preserve_bracket = preserve_bracket && max_width >= 3;
    let content_budget = if preserve_bracket {
        max_width.saturating_sub(2) // 1 for …, 1 for bracket
    } else {
        max_width.saturating_sub(1) // 1 for …
    };

    let mut new_spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;

    for span in &line.spans {
        let content = span.content.as_ref();
        let sw = content.width();
        if used + sw <= content_budget {
            new_spans.push(span.clone());
            used += sw;
        } else {
            // Partially include this span without splitting a grapheme cluster.
            let remaining = content_budget - used;
            if remaining > 0 {
                let partial = clip_str_to_display_width(content, remaining);
                if !partial.is_empty() {
                    new_spans.push(Span::styled(partial.to_string(), span.style));
                }
            }
            break;
        }
    }

    // Append ellipsis (inherits style of last content span, or default).
    let ellipsis_style = new_spans.last().map(|s| s.style).unwrap_or_default();
    new_spans.push(Span::styled("…", ellipsis_style));

    // Append preserved bracket if applicable.
    if preserve_bracket && let Some(ch) = bracket_char {
        new_spans.push(Span::styled(ch.to_string(), bracket_style));
    }

    Line::from(new_spans)
}

#[cfg(test)]
#[path = "textarea_tests.rs"]
mod tests;
