//! Shared overlay pane state machine.
//!
//! [`OverlayState`] encapsulates the three-state visibility/focus/fullscreen
//! logic shared by all toggleable panes (tracing, todo, bg tasks).
//!
//! [`handle_overlay_key`] processes structural keys (Tab, Esc, q, Space,
//! Ctrl-F) consistently across all overlay panes, so each pane only needs
//! to implement its content-specific `handle_key()`.
//!
//! ## State model
//!
//! ```text
//!   ┌────────┐  shortcut  ┌──────────────────┐  shortcut  ┌────────┐
//!   │ Hidden │ ─────────► │ Visible + Focused │ ─────────► │ Hidden │
//!   └────────┘            └──────────────────┘            └────────┘
//!                           │  Tab/Space  ▲                    ▲
//!                           ▼             │ shortcut           │ Esc/q
//!                     ┌──────────────────┐                    │
//!                     │ Visible+Unfocused│ ───────────────────┘
//!                     └──────────────────┘   (Esc/q from unfocused
//!                                             shouldn't happen —
//!                                             keys go to agent view)
//! ```

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the caller should do after an overlay state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAction {
    /// No state change — key not consumed.
    Ignored,
    /// State changed, redraw needed.
    Changed,
    /// Unfocused → move focus to scrollback.
    FocusScrollback,
    /// Unfocused → move focus to prompt.
    FocusPrompt,
}

impl OverlayAction {
    /// Whether this action represents a consumed key event.
    pub fn consumed(self) -> bool {
        !matches!(self, Self::Ignored)
    }
}

/// Shared visibility / focus / fullscreen state for overlay panes.
///
/// Embedded in each toggleable pane (TracingPane, TodoPane, etc.).
/// The pane's shortcut handler calls [`toggle()`], and the shared
/// [`handle_overlay_key()`] handles Tab/Esc/q/Space/Ctrl-F.
#[derive(Debug, Clone, Copy, Default)]
pub struct OverlayState {
    pub visible: bool,
    pub focused: bool,
    pub fullscreen: bool,
}

impl OverlayState {
    /// Start visible but not focused (e.g. todo pane that shows when items arrive).
    pub fn visible() -> Self {
        Self {
            visible: true,
            focused: false,
            fullscreen: false,
        }
    }

    /// Start hidden (e.g. tracing pane).
    pub fn hidden() -> Self {
        Self::default()
    }

    /// Pane shortcut: three-state toggle.
    ///
    /// Hidden → show + focus.
    /// Visible + unfocused → focus.
    /// Visible + focused → hide.
    pub fn toggle(&mut self) -> OverlayAction {
        if !self.visible {
            self.visible = true;
            self.focused = true;
        } else if !self.focused {
            self.focused = true;
        } else {
            self.visible = false;
            self.fullscreen = false;
            self.focused = false;
        }
        OverlayAction::Changed
    }

    /// Tab: exit fullscreen if active, unfocus, keep visible → scrollback.
    pub fn tab_out(&mut self) -> OverlayAction {
        self.fullscreen = false;
        self.focused = false;
        OverlayAction::FocusScrollback
    }

    /// Esc / q: exit one nesting level.
    ///
    /// Fullscreen → exit fullscreen (stay visible + focused).
    /// Non-fullscreen → hide entirely.
    pub fn escape(&mut self) -> OverlayAction {
        if self.fullscreen {
            self.fullscreen = false;
        } else {
            self.visible = false;
            self.focused = false;
        }
        OverlayAction::Changed
    }

    /// Space: exit fullscreen if active, unfocus, keep visible → prompt.
    pub fn space(&mut self) -> OverlayAction {
        self.fullscreen = false;
        self.focused = false;
        OverlayAction::FocusPrompt
    }

    /// Ctrl-F: toggle fullscreen.
    pub fn toggle_fullscreen(&mut self) -> OverlayAction {
        self.fullscreen = !self.fullscreen;
        OverlayAction::Changed
    }

    /// Hide entirely. Used by external callers (e.g. clear on session end).
    pub fn hide(&mut self) -> OverlayAction {
        self.visible = false;
        self.fullscreen = false;
        self.focused = false;
        OverlayAction::Changed
    }

    /// Show and focus (e.g. auto-show when items arrive).
    pub fn show(&mut self) {
        self.visible = true;
    }
}

/// Handle structural keys for any focused overlay pane.
///
/// Processes Tab, Esc, q, Space, and Ctrl-F consistently. Returns
/// `Some(action)` if a structural key was consumed, `None` to let the
/// pane's content handler process the key.
///
/// When `has_input_bar` is true, only Ctrl-F is processed (the input
/// bar handles Esc/Tab/etc. itself).
pub fn handle_overlay_key(state: &mut OverlayState, key: &KeyEvent) -> Option<OverlayAction> {
    // Ctrl-F: toggle fullscreen (works even with input bar open).
    if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(state.toggle_fullscreen());
    }

    None
}

/// Handle structural keys that should only fire when no input bar is open.
///
/// Split from [`handle_overlay_key`] so callers can check `has_input_bar`
/// before calling this.
pub fn handle_overlay_nav_key(state: &mut OverlayState, key: &KeyEvent) -> Option<OverlayAction> {
    match key.code {
        KeyCode::Tab => Some(state.tab_out()),
        KeyCode::Esc => Some(state.escape()),
        // Plain 'q' only (Ctrl-Q is the app-level quit shortcut).
        KeyCode::Char('q') if key.modifiers == KeyModifiers::NONE => Some(state.escape()),
        KeyCode::Char(' ') => Some(state.space()),
        _ => None,
    }
}
