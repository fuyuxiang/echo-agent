//! `ListPaneState` — non-generic view state for a scrollable list pane.
//!
//! Owns scroll position, selection (via stable IDs), layout cache, follow mode,
//! wrap mode and filter.  Does **not** own any items — those are borrowed from
//! an external model during [`prepare_layout`] and rendering.

use std::ops::Range;

use echo_agent_ratatui_textarea::{ClipboardProvider, InternalClipboard, TextArea, TextAreaState};
use ratatui::layout::Rect;

use super::ListItem;
use super::layout::{ListLayoutCache, WrapMode};
#[cfg(doc)]
use super::render;
use crate::key;
use crate::render::scrollbar::SCROLLBAR_TOTAL_COLS;
use crate::search::{QueryKind, TextMatcher};

// ---------------------------------------------------------------------------
// ListMatcher — unified filter / search
// ---------------------------------------------------------------------------

/// Whether the matcher hides non-matching items or just highlights them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    /// Filter mode: non-matching items are hidden from the list.
    /// `vis_map` is derived from `match_indices`.
    Filter,
    /// Search mode: all items remain visible.  Matching items are
    /// highlighted and navigable with n/N.
    Search,
}

/// Unified filter / search state for a list pane.
///
/// Wraps a [`TextMatcher`] with the list-specific bits: whether matches are
/// hidden or highlighted ([`MatchMode`]), the eagerly-built physical match
/// indices, and the `current_match` cursor used by n/N navigation.
#[derive(Debug, Clone)]
pub struct ListMatcher {
    text: TextMatcher,
    /// Whether this matcher hides or highlights.
    pub mode: MatchMode,
    /// Physical indices of items whose `search_text()` matches, sorted ascending.
    pub match_indices: Vec<usize>,
    /// Index into `match_indices` identifying the "current" match for n/N
    /// navigation.  `None` when there are no matches.
    pub current_match: Option<usize>,
}

impl ListMatcher {
    /// Build a new matcher.
    ///
    /// `query` — the raw user input.
    /// `kind` — substring or regex interpretation.
    /// `mode` — filter (hide) or search (highlight).
    pub fn new(query: impl Into<String>, kind: QueryKind, mode: MatchMode) -> Self {
        Self {
            text: TextMatcher::new(query, kind),
            mode,
            match_indices: Vec::new(),
            current_match: None,
        }
    }

    /// The raw query string the user typed.
    pub fn query(&self) -> &str {
        self.text.query()
    }

    /// Whether the regex failed to compile (bad user regex).
    pub fn is_error(&self) -> bool {
        self.text.is_error()
    }

    /// Number of matching items.
    pub fn match_count(&self) -> usize {
        self.match_indices.len()
    }

    /// The compiled regex (for highlight rendering).
    pub fn compiled_regex(&self) -> &regex::Regex {
        self.text.compiled_regex()
    }

    /// Rebuild `match_indices` from the given items.
    ///
    /// Called by `prepare_layout` when items or the matcher change.
    pub fn rebuild_matches<T: super::ListItem>(&mut self, items: &[T]) {
        self.match_indices.clear();
        for (i, item) in items.iter().enumerate() {
            if self.text.is_match(item.search_text()) {
                self.match_indices.push(i);
            }
        }
        if self.match_indices.is_empty() {
            self.current_match = None;
        } else if self.current_match.is_none() {
            self.current_match = Some(0);
        } else if let Some(cm) = self.current_match
            && cm >= self.match_indices.len()
        {
            self.current_match = Some(self.match_indices.len() - 1);
        }
    }

    /// Advance to the next match after the given physical index.
    /// Returns the physical index of the next match (wraps around).
    pub fn next_match_after(&mut self, current_pi: usize) -> Option<usize> {
        let idx = crate::search::next_index_after(&self.match_indices, current_pi)?;
        self.current_match = Some(idx);
        Some(self.match_indices[idx])
    }

    /// Go to the previous match before the given physical index.
    /// Returns the physical index of the previous match (wraps around).
    pub fn prev_match_before(&mut self, current_pi: usize) -> Option<usize> {
        let idx = crate::search::prev_index_before(&self.match_indices, current_pi)?;
        self.current_match = Some(idx);
        Some(self.match_indices[idx])
    }
}

// ---------------------------------------------------------------------------
// Backward-compatible aliases
// ---------------------------------------------------------------------------

/// Backward-compatible alias for [`ListMatcher`].
///
/// Existing code that constructs `FilterMatcher::substring(...)` or
/// `FilterMatcher::regex(...)` continues to work via these helper methods.
/// New code should use [`ListMatcher::new`] directly.
pub type FilterMatcher = ListMatcher;

impl ListMatcher {
    /// Build a substring filter matcher (backward-compatible).
    pub fn substring(query: impl Into<String>) -> Self {
        Self::new(query, QueryKind::Substring, MatchMode::Filter)
    }

    /// Build a regex filter matcher (backward-compatible).
    pub fn regex(pattern: impl Into<String>) -> Self {
        Self::new(pattern, QueryKind::Regex, MatchMode::Filter)
    }

    /// Whether this is a regex that failed to compile (backward-compatible).
    pub fn is_regex_error(&self) -> bool {
        self.is_error()
    }
}

/// Active filter state.  Wraps a [`ListMatcher`] with a match count.
///
/// Kept for backward compatibility with code that reads `filter().match_count`.
/// The match count is now derived from `matcher.match_indices.len()`.
#[derive(Debug, Clone)]
pub struct ListFilter {
    /// The unified matcher.
    pub matcher: ListMatcher,
}

impl ListFilter {
    /// Number of items matching the filter.
    pub fn match_count(&self) -> usize {
        self.matcher.match_count()
    }
}

// ---------------------------------------------------------------------------
// InputBarMode — search vs filter
// ---------------------------------------------------------------------------

/// Which mode the input bar is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBarMode {
    /// `/` — regex search.  All items visible, matches highlighted.
    Search,
    /// `f` — regex filter.  Non-matching items hidden.
    Filter,
    /// `:` — go to line number (or line range).
    GotoLine,
    Comment,
}

impl InputBarMode {
    /// The prompt prefix shown in the input bar.
    pub fn prompt(self) -> &'static str {
        match self {
            InputBarMode::Search => "/",
            InputBarMode::Filter => "f>",
            InputBarMode::GotoLine => ":",
            InputBarMode::Comment => "comment: ",
        }
    }

    /// The corresponding `MatchMode` for building a `ListMatcher`.
    fn match_mode(self) -> MatchMode {
        match self {
            InputBarMode::Search => MatchMode::Search,
            InputBarMode::Filter => MatchMode::Filter,
            InputBarMode::GotoLine => MatchMode::Search, // unused, but needed for exhaustive match
            InputBarMode::Comment => MatchMode::Search,
        }
    }
}

/// Snapshot of list state before entering goto-line mode.
/// Used to restore on Esc/cancel.
#[derive(Debug, Clone)]
struct GotoLineSnapshot {
    scroll_offset: usize,
    selected_id: Option<u64>,
    visual_mode: bool,
    visual_anchor_id: Option<u64>,
}

// ---------------------------------------------------------------------------
// LayoutStamp — dirty-tracking for prepare_layout
// ---------------------------------------------------------------------------

/// Snapshot of parameters used to build the current layout cache.
///
/// Stored in [`ListPaneState`] and compared each frame to decide whether
/// the layout cache can be reused, extended incrementally, or must be
/// fully rebuilt.
#[derive(Debug, Clone, Copy)]
struct LayoutStamp {
    /// Width at which the cache was built.
    width: u16,
    /// Number of visible items the cache was built for.
    count: usize,
    /// Wrap mode for which the cache was built.
    wrap: WrapMode,
}

// ---------------------------------------------------------------------------
// ListPaneState
// ---------------------------------------------------------------------------

/// View state for a scrollable list pane.
///
/// **Non-generic** — the item type `T: ListItem` only appears at the
/// boundaries: [`prepare_layout`] and `ListPane<'a, T>` (the widget).
#[derive(Debug)]
pub struct ListPaneState {
    // -- Scroll ---------------------------------------------------------------
    /// Scroll offset in visual lines from the top of the content.
    scroll_offset: usize,

    /// Viewport height in terminal rows (set by [`prepare_layout`]).
    viewport_height: u16,

    // -- Selection (stable IDs) -----------------------------------------------
    /// Currently selected item, stored as a stable ID.
    /// Resolved to `selected_index` in [`prepare_layout`].
    selected_id: Option<u64>,

    /// The resolved index of `selected_id` after the last [`prepare_layout`].
    /// Ephemeral — valid only until the next item mutation.
    selected_index: Option<usize>,

    /// Multi-selection as a contiguous range of stable IDs (anchor, end).
    /// Resolved to `multi_range` in [`prepare_layout`].
    multi_selected_ids: Option<(u64, u64)>,

    /// Resolved index range of the multi-selection.
    multi_range: Option<Range<usize>>,

    /// Whether visual selection mode is active (`v`).
    pub visual_mode: bool,

    /// Stable ID of the visual mode anchor (the item where `v` was pressed).
    /// The range is [anchor, cursor] (cursor = `selected_id`).
    visual_anchor_id: Option<u64>,

    // -- Layout ---------------------------------------------------------------
    /// Layout cache (heights + prefix sums, or fixed-height).
    layout: ListLayoutCache,

    /// Current wrap mode.
    wrap_mode: WrapMode,

    // -- Layout dirty tracking ------------------------------------------------
    /// Parameters of the last layout build (for incremental / skip logic).
    /// `None` if no layout has been computed yet.
    last_stamp: Option<LayoutStamp>,

    /// Whether the filter has changed since the last layout build.
    filter_dirty: bool,

    /// Total item count from last `prepare_layout` call (for detecting length
    /// changes that require refilter).
    last_item_count: usize,

    /// Scroll anchor: after the next layout rebuild (e.g. wrap toggle, resize),
    /// place the selected item at this screen-y offset.  Consumed by
    /// `prepare_layout` and reset to `None`.
    scroll_anchor: Option<usize>,

    /// Pinned screen-y for viewport scrolling.
    ///
    /// When scrolling the viewport (Ctrl-d/u, mouse wheel, etc.), we want
    /// the selection to stay at a fixed screen row.  If the target item is
    /// non-selectable (separator), we pick a neighbor — but that shifts the
    /// *actual* screen-y by 1.  Without this pin, the drift accumulates
    /// across every separator crossing.
    ///
    /// Set on the first viewport scroll after a selection change.  Cleared
    /// by any intentional selection movement (j/k, click, g/G, etc.).
    scroll_screen_y: Option<usize>,

    // -- Modes ----------------------------------------------------------------
    /// Follow mode: auto-scroll to bottom when new items appear.
    /// In follow mode the cursor is hidden (no selection highlight).
    pub follow_mode: bool,

    // -- Follow / NAV edge tracking -------------------------------------------
    /// NAV only: true when the last downward action was clamped at the
    /// content bottom (viewport or selection at the end).  The next
    /// downward action with this flag set engages follow ("one-past").
    at_content_edge: bool,

    /// NAV only: mouse-wheel overscroll counter.  Incremented on each
    /// wheel-down event that is clamped at the bottom while
    /// `at_content_edge` is true.  When it reaches
    /// [`MOUSE_OVERSCROLL_THRESHOLD`], follow mode engages.
    overscroll_ticks: u8,

    /// Active matcher (filter or search).  `None` = show all, no highlights.
    matcher: Option<ListMatcher>,

    /// Visible-index → physical-index mapping.
    /// `None` = identity (no filter active, vis[i] == i).
    /// `Some(vec)` = filtered mapping.
    /// Built in `prepare_layout`, consumed by the renderer.
    vis_map: Option<Vec<usize>>,

    /// Scroll margin: minimum lines of context above/below selection.
    scroll_margin: u16,

    /// Scrollbar screen area (set by the renderer, used for click/scroll hit-testing).
    /// `None` when scrollbar is not shown (content fits viewport).
    last_scrollbar_area: Option<Rect>,

    // -- Highlight visibility -------------------------------------------------
    /// Whether the highlight post-pass should render match inversions.
    ///
    /// Callers set this to `false` after accepting a filter (Enter) to avoid
    /// visual noise when every visible line matches.  Set back to `true` when
    /// re-entering the input bar or switching to search mode.
    ///
    /// Defaults to `true` (highlights always shown).
    pub show_highlights: bool,

    // -- Height cache (Wrap mode) ---------------------------------------------
    /// Per-physical-item height cache for Wrap mode.
    ///
    /// Indexed by physical item index.  Computed once when the width changes
    /// (or on first layout), reused across filter changes.  This avoids
    /// calling `desired_height` (which runs word-wrapping) on every filter
    /// keystroke for 100K+ items.
    ///
    /// Invalidated when width changes.  Extended incrementally on appends.
    height_cache: Vec<u16>,

    /// Width at which `height_cache` was computed.
    height_cache_width: u16,

    // -- Config ---------------------------------------------------------------
    /// Feature flags controlling which behaviors are active.
    config: ListPaneConfig,

    // -- Input bar (search / filter) ------------------------------------------
    /// Active input bar mode, or `None` if the bar is closed.
    input_mode: Option<InputBarMode>,

    /// Text input widget for the search/filter bar.
    input_textarea: TextArea,

    /// Scroll state for the input textarea (always 0 for single-line).
    input_textarea_state: TextAreaState,

    /// Cached screen position of the input bar cursor (set during render).
    input_cursor_screen_pos: Option<(u16, u16)>,

    // -- Mouse / scrollbar ----------------------------------------------------
    /// Whether a scrollbar drag is in progress.
    scrollbar_dragging: bool,

    // -- Clipboard ------------------------------------------------------------
    /// Clipboard provider for `y` (copy).  Default is `InternalClipboard`
    /// (in-memory).  Host app can inject system clipboard via
    /// [`set_clipboard_provider`].
    clipboard: Box<dyn ClipboardProvider>,

    /// When the last successful copy happened (for toast notification).
    copy_toast_until: Option<std::time::Instant>,

    /// Snapshot taken when entering goto-line mode, restored on cancel.
    goto_line_snapshot: Option<GotoLineSnapshot>,

    /// Whether visual mode was active when goto-line was opened.
    /// Used to decide if single-number input extends the existing range
    /// or just jumps.
    goto_line_had_visual: bool,
}

/// Mouse-wheel overscroll ticks required to snap into follow mode.
/// Tunable — start with 1 (easy to trigger), increase if too twitchy.
const MOUSE_OVERSCROLL_THRESHOLD: u8 = 1;

// ---------------------------------------------------------------------------
// ListPaneConfig — feature flags
// ---------------------------------------------------------------------------

/// Configuration flags for a `ListPaneState`.
///
/// Controls which features are available.  Use-case examples:
///
/// | Use case       | follow | wrap_toggle |
/// |----------------|--------|-------------|
/// | Tracing pane   | ✓      | ✓           |
/// | Todo list      | ✗      | ✗           |
/// | Background tasks | ✗    | ✗           |
#[derive(Debug, Clone, Copy)]
pub struct ListPaneConfig {
    /// Whether follow mode is available.
    ///
    /// When `false`:
    /// - `follow_mode` is always `false` regardless of constructor arg.
    /// - `G`/`End` selects the last item (no follow engage).
    /// - One-past / overscroll logic is disabled.
    /// - `toggle_follow()` is a no-op.
    pub follow_enabled: bool,

    /// Whether `w` toggles wrap mode.
    ///
    /// When `false`, `w` is not consumed by `handle_key_event`.
    pub wrap_toggle_enabled: bool,

    /// Whether `/` (search) and `f` (filter) are available.
    ///
    /// When `false`, these keys are not consumed and the input bar
    /// never opens.
    pub search_enabled: bool,

    /// Whether `y` copies the selected item(s) to clipboard.
    ///
    /// When `false`, `y` is not consumed by `handle_key_event`.
    pub copy_enabled: bool,

    /// Whether to show the selection highlight when the pane is unfocused.
    ///
    /// When `true` (default), the selection bg is always painted.
    /// When `false`, the selection bg is only painted when `ListPane::focused(true)`.
    /// Use `false` in multi-pane layouts where unfocused panes should dim.
    pub show_selection_when_unfocused: bool,

    /// Whether `v` / `Shift-j` / `Shift-k` visual selection is available.
    ///
    /// When `false`, these keys are not consumed by `handle_key_event`.
    pub visual_select_enabled: bool,

    /// Whether `f` (filter) is available.
    ///
    /// When `false`, `f` is not consumed. `/` (search) is still controlled
    /// by `search_enabled`. This allows search-only without filter.
    pub filter_enabled: bool,

    /// Whether `:` (go to line) is available.
    ///
    /// When `false`, `:` is not consumed by `handle_key_event`.
    pub goto_line_enabled: bool,
}

impl Default for ListPaneConfig {
    /// Default config: follow and search disabled, wrap toggle enabled.
    ///
    /// This is the safe default for static lists (todo items, background
    /// tasks) where follow mode and search don't make sense.  For
    /// streaming/append-only lists (tracing pane), use
    /// `ListPaneConfig::streaming()`.
    fn default() -> Self {
        Self {
            follow_enabled: false,
            wrap_toggle_enabled: true,
            search_enabled: false,
            copy_enabled: false,
            show_selection_when_unfocused: true,
            visual_select_enabled: false,
            filter_enabled: false,
            goto_line_enabled: false,
        }
    }
}

impl ListPaneConfig {
    /// Config preset for streaming / append-only lists.
    ///
    /// Enables follow mode and all features.
    pub fn streaming() -> Self {
        Self {
            follow_enabled: true,
            wrap_toggle_enabled: true,
            search_enabled: true,
            copy_enabled: true,
            show_selection_when_unfocused: true,
            visual_select_enabled: true,
            filter_enabled: true,
            goto_line_enabled: false,
        }
    }
}

mod methods;

// ===========================================================================
// Goto-line input parsing
// ===========================================================================

/// Parsed result of goto-line input.
enum GotoTarget {
    /// Single line number (1-based, clamped to item count).
    Single(usize),
    /// Line range (1-based, clamped, start <= end).
    Range(usize, usize),
    /// Input can't be parsed as a line number or range.
    Invalid,
}

/// Parse goto-line input text into a target.
///
/// Supports:
/// - `N` — single line number (clamped to 1..=max)
/// - `N-M` — range (clamped, M >= N enforced)
///
/// Partial inputs like `12-` (separator typed, no end yet) are treated as
/// a single line jump to the start number (the end will update live as
/// the user types more digits).
fn parse_goto_input(text: &str, max_lines: usize) -> GotoTarget {
    let text = text.trim();
    if text.is_empty() {
        return GotoTarget::Invalid;
    }

    if let Some((start_s, end_s)) = text.split_once('-') {
        // Range mode: N-M
        let start: usize = match start_s.trim().parse::<usize>() {
            Ok(n) => n.max(1).min(max_lines),
            _ => return GotoTarget::Invalid,
        };
        let end_s = end_s.trim();
        if end_s.is_empty() {
            // "N-" with no end yet — treat as single jump to N.
            return GotoTarget::Single(start);
        }
        match end_s.parse::<usize>() {
            Ok(end) => {
                let end = end.max(1).min(max_lines).max(start);
                GotoTarget::Range(start, end)
            }
            _ => GotoTarget::Invalid,
        }
    } else {
        // Single number.
        match text.parse::<usize>() {
            Ok(n) => GotoTarget::Single(n.max(1).min(max_lines)),
            _ => GotoTarget::Invalid,
        }
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
