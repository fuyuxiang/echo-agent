//! Pure data types for modal window chrome state.
//!
//! Extracted from `views::modal_window` so lower layers (e.g.
//! `prompt_images`) can reference these plain-data types without depending
//! on the `views` layer. The rendering/input logic stays in
//! `views::modal_window`, which re-exports these for existing call sites.

use ratatui::layout::Rect;

/// Persistent state for a modal window's chrome. Stored by the caller
/// alongside their domain-specific content state.
#[derive(Debug, Clone)]
pub struct ModalWindowState {
    /// Whether the `[✗]` close button is currently hovered.
    pub close_hovered: bool,
    /// Hit-test rect for the top-right `[✗]` close button.
    pub close_button_rect: Option<Rect>,
    /// Full popup area (for click-outside-to-close detection).
    pub popup_area: Option<Rect>,

    // -- Tabs (optional) --
    /// Currently active tab index.
    pub active_tab: usize,
    /// Number of tabs (0 = no tab bar).
    pub tab_count: usize,
    /// Hit-test rects for each tab label.
    pub tab_rects: Vec<Option<Rect>>,
    /// Whether the tab bar region has keyboard focus. When true, Left/Right
    pub tabs_focused: bool,

    // -- Footer shortcuts --
    /// Hit-test areas for clickable footer shortcuts.
    pub shortcut_hits: Vec<ShortcutHitArea>,
    /// Which footer shortcut (by index) is currently hovered.
    pub hovered_shortcut: Option<usize>,
}

impl ModalWindowState {
    /// Create a new modal window state with no tabs.
    pub fn new() -> Self {
        Self {
            close_hovered: false,
            close_button_rect: None,
            popup_area: None,
            active_tab: 0,
            tab_count: 0,
            tab_rects: Vec::new(),
            shortcut_hits: Vec::new(),
            hovered_shortcut: None,
            tabs_focused: false,
        }
    }

    /// Create a new modal window state with a given number of tabs.
    pub fn with_tabs(tab_count: usize) -> Self {
        Self {
            tab_count,
            tab_rects: vec![None; tab_count],
            ..Self::new()
        }
    }
}

impl Default for ModalWindowState {
    fn default() -> Self {
        Self::new()
    }
}

/// Hit-test area for a rendered footer shortcut.
#[derive(Debug, Clone)]
pub struct ShortcutHitArea {
    /// Screen rect occupied by this shortcut label.
    pub rect: Rect,
    /// Caller-defined identifier matching [`Shortcut::id`].
    pub id: usize,
    /// Index within the full `shortcuts` slice passed to
    /// [`render_modal_shortcuts`]. Used by [`handle_modal_mouse`] to
    /// track hover state in the same index space that the renderer uses.
    pub shortcuts_idx: usize,
    /// Whether clicking this shortcut dispatches `ShortcutActivated`.
    /// All shortcuts get hover highlights regardless of this flag.
    pub clickable: bool,
}
