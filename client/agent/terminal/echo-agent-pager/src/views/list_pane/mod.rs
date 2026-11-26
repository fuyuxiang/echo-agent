//! Generic scrollable list pane widget.
//!
//! `ListPaneState` + `ListPane<T>` provide a reusable, scrollable, selectable
//! list component. The state is non-generic and owns only scroll/selection/layout
//! data; item data lives in an external model and is borrowed via
//! [`ListPaneState::prepare_layout`].
//!
//! Designed for three concrete use cases:
//! - **Tracing pane** (100K+ entries, append-only, NoWrap, follow mode)
//! - **Todo pane** (<10 items, random mutations, Wrap)
//! - **Background task pane** (<10 items, random mutations, NoWrap)

mod layout;
mod render;
mod state;

pub use crate::search::QueryKind;
pub use layout::{ListLayoutCache, WrapMode};
pub use render::ListPane;
pub use state::{
    FilterMatcher, InputBarMode, ListFilter, ListMatcher, ListPaneConfig, ListPaneState, MatchMode,
};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::Line;

// ---------------------------------------------------------------------------
// ListPaneStyle — configurable colors for the framework's post-pass overlays
// ---------------------------------------------------------------------------

/// Visual style configuration for a `ListPane`.
///
/// Controls colors for selection highlighting, input bar, and other
/// framework-level overlays.  Match highlights use style inversion
/// (REVERSED modifier) and don't need configurable colors.
///
/// Items do **not** need to know about these — the framework applies them
/// as post-passes after each item renders.
#[derive(Debug, Clone, Copy)]
pub struct ListPaneStyle {
    /// Background color for the selected item row (cursor line).
    pub selection_bg: Color,

    /// Background color for the visual selection range (not the cursor line).
    /// Slightly distinct from cursor bg, distinguishing range from cursor.
    pub visual_select_bg: Color,

    /// Background color for the input bar (search/filter).
    pub input_bar_bg: Color,

    /// Foreground color for the prompt prefix (`/`, `f>`).
    pub input_bar_prompt_fg: Color,

    /// Foreground color for the typed query text.
    pub input_bar_text_fg: Color,

    /// Scrollbar track background color.
    pub scrollbar_bg: Color,

    /// Scrollbar thumb foreground color.
    pub scrollbar_fg: Color,

    /// Corner indicator color (▲ ▼ for scroll position hints).
    pub indicator_fg: Color,

    /// Follow mode indicator color (▶ in bottom-right when following).
    /// Distinct from `indicator_fg` so it's visible against content.
    pub follow_indicator_fg: Color,

    /// "Copied!" toast foreground color.
    pub toast_fg: Color,

    /// When true, the cursor line uses `visual_select_bg` when inside a
    /// visual selection (uniform range appearance). The cursor is then
    /// distinguished only by the `prefix_cursor` style, not by background.
    ///
    /// When false (default), the cursor line always uses `selection_bg`,
    /// even within a visual selection.
    pub uniform_visual_bg: bool,

    /// When false, the right-corner scroll indicators (▲/▼) are suppressed.
    /// Used by panes that draw their own scroll affordance (e.g. the tasks
    /// pane draws the same ▲/▼ centered on dedicated rows). Defaults to `true`.
    pub show_corner_indicators: bool,
}

impl Default for ListPaneStyle {
    fn default() -> Self {
        let theme = crate::theme::Theme::current();
        Self {
            // Palette defaults — sourced from theme to ensure quantization.
            selection_bg: theme.bg_highlight,
            visual_select_bg: theme.bg_visual,
            input_bar_bg: theme.bg_base,
            input_bar_prompt_fg: theme.command,
            input_bar_text_fg: theme.text_secondary,
            scrollbar_bg: theme.bg_base,
            scrollbar_fg: theme.scrollbar_fg,
            indicator_fg: theme.gray,
            follow_indicator_fg: theme.command,
            toast_fg: theme.accent_user,
            uniform_visual_bg: false,
            show_corner_indicators: true,
        }
    }
}

// ---------------------------------------------------------------------------
// ListItem trait
// ---------------------------------------------------------------------------

/// Trait that items in a `ListPane` must implement.
///
/// Items are owned by the **model** (not the view). The view borrows them
/// through `&[T]` in [`ListPaneState::prepare_layout`] and
/// [`ListPane::new`].
///
/// ## Rendering: two modes
///
/// **Content-based (preferred):** implement [`content()`] and optionally
/// [`prefix()`].  The framework handles wrapping, truncation, highlighting,
/// and selection overlays automatically.  This is the right choice for most
/// items.
///
/// **Custom rendering (escape hatch):** override [`render()`] to paint
/// directly into a buffer.  Use this only when the content/prefix model
/// doesn't fit (e.g. diff hunks with side-by-side layout).  You must also
/// override [`desired_height()`] when using custom rendering.
///
/// Items that implement [`content()`] (non-empty Line) get framework
/// rendering; the default [`render()`] and [`desired_height()`] are derived
/// automatically.  Items that override [`render()`] bypass the framework.
pub trait ListItem {
    // =======================================================================
    // Content-based API (preferred)
    // =======================================================================

    /// The styled content to display — one logical line of text.
    ///
    /// The framework handles wrapping (Wrap mode) and truncation (NoWrap mode)
    /// based on this content.  Return a reference to a stored `Line`.
    ///
    /// Default returns an empty `Line` (signals "use custom `render()`").
    fn content(&self) -> &Line<'_> {
        static EMPTY: std::sync::LazyLock<Line<'static>> = std::sync::LazyLock::new(Line::default);
        &EMPTY
    }

    /// Optional prefix column (checkbox, spinner, timestamp, etc.).
    ///
    /// Rendered in a fixed-width column at the left edge of the item.
    /// In Wrap mode, continuation lines are indented by the prefix width.
    ///
    /// Returned by value since prefixes are small and often constructed
    /// dynamically (spinner frame, elapsed timer, checkbox toggle).
    fn prefix(&self) -> Option<Line<'_>> {
        None
    }

    /// Optional prefix for items in the visual selection range (not the cursor line).
    ///
    /// Called instead of `prefix()` when the item is in the visual selection
    /// range but NOT the cursor line. Default falls back to `prefix()`.
    fn prefix_in_selection(&self) -> Option<Line<'_>> {
        self.prefix()
    }

    /// Optional prefix for the cursor line (the focused/active item).
    ///
    /// Called instead of `prefix()` when the item is the cursor line.
    /// Default falls back to `prefix()`.
    fn prefix_cursor(&self) -> Option<Line<'_>> {
        self.prefix()
    }

    /// Optional full-width background color for this item.
    ///
    /// When `Some(color)`, the framework fills the entire item row(s) with
    /// this background color before rendering content. Used for code blocks
    /// in markdown viewers.
    fn background(&self) -> Option<Color> {
        None
    }

    // =======================================================================
    // Custom rendering API (escape hatch)
    // =======================================================================

    /// Render this item into the given area.
    ///
    /// Override this **only** when the content/prefix model doesn't fit.
    /// When using the content-based API, leave this as the default (no-op).
    ///
    /// The framework calls this only when `content()` returns an empty Line.
    fn render(&self, _area: Rect, _buf: &mut Buffer, _selected: bool, _focused: bool) {}

    /// Height in visual lines at the given `width` when soft-wrapping.
    ///
    /// In `NoWrap` mode the pane ignores this and uses height = 1.
    /// Must be ≥ 1.
    ///
    /// Default implementation computes from [`content()`] and [`prefix()`].
    /// Override only when using custom [`render()`].
    fn desired_height(&self, width: u16) -> u16 {
        if width == 0 {
            return 1;
        }
        let prefix_w = self.prefix().map(|p| line_display_width(&p)).unwrap_or(0);
        let content_w = line_display_width(self.content());
        if content_w == 0 {
            return 1;
        }
        let text_area = (width as usize).saturating_sub(prefix_w);
        if text_area == 0 {
            return 1;
        }
        // Use actual word-wrap line count via textwrap (not character-count
        // division). The cheap ceil(chars/width) estimate underestimates
        // because word-aware wrapping produces more lines when words can't
        // fit at line boundaries.
        //
        // We use textwrap::wrap directly (cheap — just computes break
        // positions) rather than word_wrap_line (expensive — builds styled
        // Lines). Uses the same FirstFit options as the rendering pipeline.
        let flat: String = self
            .content()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        let opts = textwrap::Options::new(text_area)
            .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit)
            .break_words(true);
        (textwrap::wrap(&flat, opts).len() as u16).max(1)
    }

    // =======================================================================
    // Identity & behavior
    // =======================================================================

    /// Stable identity that survives insertions, removals, and reordering.
    ///
    /// Must be unique within the list.  Used so that selection state persists
    /// across mutations without index arithmetic.
    fn stable_id(&self) -> u64;

    /// Whether this item can be selected. Return `false` for separator rows.
    fn is_selectable(&self) -> bool {
        true
    }

    /// Source line number for goto-line (`:N`) navigation.
    ///
    /// When items have a meaningful source line number (e.g., file viewer
    /// lines), return `Some(n)` so goto-line targets the correct item even
    /// when the visual index differs (e.g., interleaved comment lines).
    /// Return `None` (default) to use the visual index.
    fn goto_line_number(&self) -> Option<usize> {
        None
    }

    /// Whether this item needs periodic tick updates (e.g. elapsed timer).
    fn needs_tick(&self) -> bool {
        false
    }

    // =======================================================================
    // Search / filter
    // =======================================================================

    /// Plain text for search/filter matching.
    ///
    /// The framework calls `regex.is_match(item.search_text())` during
    /// filtering and `regex.find_iter(item.search_text())` for highlight
    /// rendering.  Byte offsets in this string correspond to the text
    /// content rendered starting at column [`search_text_col_offset`].
    ///
    /// Default returns `""` (item not searchable/filterable).
    fn search_text(&self) -> &str {
        ""
    }

    /// Column offset where `search_text()` content begins in the rendered output.
    ///
    /// The framework uses this to position match highlights correctly.
    ///
    /// Default derives from [`prefix()`] display width.  Override only
    /// when using custom [`render()`] with a non-standard layout.
    fn search_text_col_offset(&self) -> u16 {
        self.prefix()
            .map(|p| line_display_width(&p) as u16)
            .unwrap_or(0)
    }

    /// Text to copy when `y` is pressed.
    ///
    /// Default extracts plain text from `content()`. Override for items
    /// that use custom `render()` with empty `content()`.
    fn copy_text(&self) -> String {
        self.content()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }
}

/// Compute the display width of a ratatui `Line` (sum of span display widths).
pub(crate) fn line_display_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}
