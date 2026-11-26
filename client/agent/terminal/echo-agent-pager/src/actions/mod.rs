//! Action registry — single source of truth for all actions, key bindings, and hints.
//!
//! Three consumers:
//! - **Shortcuts bar**: `registry.hints(contexts)` → filtered, prioritized hints
//! - **Command palette**: `registry.all()` → fuzzy searchable list
//! - **Key dispatch**: `registry.lookup(key, context)` → action to execute
//!
//! ## Input bubbling
//!
//! Each layer in the input chain does an **exact context match**:
//! 1. Pane level: `lookup(key, ScrollbackFocused)` or `lookup(key, PromptFocused)`
//! 2. Agent level: `lookup(key, AgentScreen)`
//! 3. Global level: `lookup(key, Always)`
//!
//! The bubbling is explicit in code, not hidden in `context_matches`.

mod defaults;

use crossterm::event::KeyEvent;

use crate::input::key::KeyShortcut;
use crate::views::shortcuts_bar::HintItem;

pub use defaults::ctrl_dot_unreliable;

#[cfg(test)]
pub(crate) fn default_actions(
    screen_mode: crate::app::ScreenMode,
    mouse_reporting_toggle_enabled: bool,
) -> Vec<ActionDef> {
    defaults::default_actions(screen_mode, mouse_reporting_toggle_enabled)
}

/// Unique action identifier. Compile-time checked, no strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    // Prompt
    SendPrompt,
    InterjectPrompt,
    /// Stash the composer draft; on an empty composer, pop the newest stash.
    StashPrompt,
    /// Enable voice mode and start recording (`/voice`). Not a toggle — it
    /// never turns voice mode off; capture is controlled by [`Self::VoiceToggle`].
    EnableVoiceMode,
    /// Start/stop mic capture (Ctrl+Space / Esc). Starting also enables voice
    /// mode and spawns the pipeline if needed — no `/voice` prerequisite.
    VoiceToggle,

    // Navigation
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    GotoTop,
    GotoBottom,
    SelectNext,
    SelectPrev,
    NextTurn,
    PrevTurn,
    NextResponse,
    PrevResponse,

    // View
    Collapse,
    Expand,
    ToggleFold,
    ToggleExpandAll,
    ExpandAllThinking,
    ToggleRaw,
    ToggleMouseCapture,

    // Agent
    NextModel,
    CancelTurn,
    ToggleYolo,
    ToggleMultiline,

    // Focus
    FocusPrompt,
    FocusScrollback,

    // Block content
    CopyBlockContent,
    CopyBlockMeta,
    OpenBlockViewer,

    // Link navigation
    OpenNextLink,
    OpenPrevLink,

    // Panes
    ToggleTodos,
    ToggleTasks,
    ToggleQueue,
    OpenSessions,
    OpenExtensions,
    SendToBackground,

    // Prompt
    EditPromptExternal,
    CycleMode,
    BashMode,

    // Scrollback (contextual)
    Rewind,
    KillBgTask,

    // Debug
    DumpInputLog,

    // App
    Quit,
    NewSession,
    NewSessionInWorktree,
    ExitSession,
    CommandPalette,
    ModelPicker,
    ShortcutsHelp,

    // Settings
    OpenSettings,

    // Agent Dashboard
    OpenDashboard,
    DashboardSelectNext,
    DashboardSelectPrev,
    DashboardTogglePin,
    DashboardBeginRename,
    DashboardStop,
    DashboardCycleMode,
    DashboardToggleGrouping,
    DashboardReorderUp,
    DashboardReorderDown,
    DashboardShortcutsHelp,
    DashboardExit,
    DashboardOverlayExit,
    DashboardOverlayPrev,
    DashboardOverlayNext,
    DashboardOverlayStop,
    DashboardToggleAutoApprove,
    DashboardOpenLocationPicker,
    DashboardToggleWorktree,
}
/// When an action is available / visible.
///
/// Used for **exact** matching in `registry.lookup()`.
/// Each layer in the input chain queries its own context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Global — checked at the app level after all views.
    Always,
    /// Only when prompt pane is focused.
    PromptFocused,
    /// Only when scrollback pane is focused.
    ScrollbackFocused,
    /// Agent-level — checked after pane routing, before global.
    AgentScreen,
    /// Only on the welcome screen.
    WelcomeScreen,
    /// Only when the Agent Dashboard view is focused.
    DashboardFocused,
    /// Only inside the dashboard's session overlay (a dashboard-spawned agent
    /// rendered fullscreen). Distinguishes the detail-view shortcuts (back to
    /// dashboard, prev/next session) from the dashboard LIST shortcuts so the
    /// cheatsheet can dim whichever set isn't applicable to the current view.
    DashboardOverlay,
}

impl When {
    /// Stable product-event label for this context (`prompt_focused`, …).
    pub fn telemetry_name(self) -> &'static str {
        match self {
            When::Always => "always",
            When::PromptFocused => "prompt_focused",
            When::ScrollbackFocused => "scrollback_focused",
            When::AgentScreen => "agent_screen",
            When::WelcomeScreen => "welcome_screen",
            When::DashboardFocused => "dashboard_focused",
            When::DashboardOverlay => "dashboard_overlay",
        }
    }
}

/// Action category (for grouping in command palette).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    GettingStarted,
    Input,
    ConversationNav,
    ConversationAction,
    Panels,
    Session,
    /// Agent Dashboard shortcuts.
    Dashboard,
}

/// A registered action definition.
#[derive(Debug, Clone)]
pub struct ActionDef {
    pub id: ActionId,
    /// Short label for shortcuts bar: "Send", "Quit", "nav"
    pub label: &'static str,
    /// Longer description for command palette
    pub description: &'static str,
    /// Optional man-style help for the shortcuts cheatsheet detail/expand UI.
    /// Consumers should fall back to `description` when this is `None`.
    pub long_help: Option<&'static str>,
    /// Default key binding
    pub default_key: KeyShortcut,
    /// Optional second key binding (e.g., j/k both shown as "j/k:nav")
    pub alt_keys: Vec<KeyShortcut>,
    /// Category for grouping
    pub category: Category,
    /// When this action is available
    pub context: When,
    /// Priority for shortcuts bar. None = don't show. Some(0) = highest priority.
    pub hint_priority: Option<u8>,
    /// Combined display for shortcuts bar (e.g., "j/k" for SelectNext+SelectPrev pair).
    /// If set, overrides default_key.display().
    pub hint_key_display: Option<&'static str>,
    /// If true, requires double-press (1000ms TTL) to execute.
    /// The first press sets a `PendingAction`; the second press confirms.
    pub requires_confirmation: bool,
}

impl ActionDef {
    /// Convert this action def into a [`HintItem`] for the shortcuts bar.
    ///
    /// Uses `default_key` only. For paired hints (j/k, h/l), the view should
    /// use [`HintItem::paired`] with keys from two related action defs.
    pub fn hint(&self) -> HintItem {
        let mut item = HintItem::new(self.default_key, self.label);
        item.custom_display = self.hint_key_display;
        item.description = Some(std::borrow::Cow::Borrowed(self.description));
        item
    }
}

/// Registry of all actions. Single source of truth.
pub struct ActionRegistry {
    actions: Vec<ActionDef>,
}

impl ActionRegistry {
    /// Create a registry with the given action definitions.
    pub fn new(actions: Vec<ActionDef>) -> Self {
        Self { actions }
    }

    /// Create the default fullscreen/inline registry with all standard actions.
    pub fn defaults() -> Self {
        Self::defaults_for(crate::app::ScreenMode::Fullscreen)
    }

    /// Create the default registry for the process-lifetime screen mode.
    pub(crate) fn defaults_for(screen_mode: crate::app::ScreenMode) -> Self {
        Self::defaults_with_config_for(screen_mode, false)
    }

    /// Create the default fullscreen/inline registry, optionally including
    /// config-gated actions.
    pub fn defaults_with_config(mouse_reporting_toggle_enabled: bool) -> Self {
        Self::defaults_with_config_for(
            crate::app::ScreenMode::Fullscreen,
            mouse_reporting_toggle_enabled,
        )
    }

    /// Create the default registry for a screen mode, optionally including
    /// config-gated actions.
    pub(crate) fn defaults_with_config_for(
        screen_mode: crate::app::ScreenMode,
        mouse_reporting_toggle_enabled: bool,
    ) -> Self {
        Self::new(defaults::default_actions(
            screen_mode,
            mouse_reporting_toggle_enabled,
        ))
    }

    /// Look up an action by key event and current context.
    ///
    /// Uses **exact** context matching — each layer in the input chain
    /// calls this with its own context level.
    ///
    /// Peek/probe only: does **not** emit telemetry.
    pub fn lookup(&self, event: &KeyEvent, context: When) -> Option<ActionId> {
        for def in &self.actions {
            if def.context != context {
                continue;
            }
            if def.default_key.matches(event) {
                return Some(def.id);
            }
            if def.alt_keys.iter().any(|alt| alt.matches(event)) {
                return Some(def.id);
            }
        }
        None
    }

    /// Whether `event` matches `id`'s default or any alt, ignoring `When`.
    /// Used for cross-pane chords that share an action's key set (e.g. queue
    /// force-interject uses the same keys as `InterjectPrompt`).
    pub fn matches_id(&self, id: ActionId, event: &KeyEvent) -> bool {
        let Some(def) = self.find(id) else {
            return false;
        };
        def.default_key.matches(event) || def.alt_keys.iter().any(|k| k.matches(event))
    }

    /// True when the send-now (interject) chord should act or be advertised:
    /// turn running and there is something to send. `has_payload` is true for
    /// non-empty composer text, editing a queued row, or a visible queued
    /// follow-up (empty-composer force-send from the prompt). Idle or no
    /// payload remains a no-op (not send-like-Enter).
    pub fn interjection_possible(turn_running: bool, has_payload: bool) -> bool {
        turn_running && has_payload
    }

    /// Registry pinned to non–VS Code family bindings (host-independent tests).
    #[cfg(test)]
    pub fn non_vscode_for_test() -> Self {
        Self::non_vscode_for_mode_for_test(crate::app::ScreenMode::Fullscreen)
    }

    /// Mode-correct registry pinned to non–VS Code family bindings.
    #[cfg(test)]
    pub(crate) fn non_vscode_for_mode_for_test(screen_mode: crate::app::ScreenMode) -> Self {
        use crate::key;
        let mut actions = defaults::default_actions(screen_mode, false);
        for def in actions.iter_mut() {
            if def.id == ActionId::Quit {
                def.default_key = key!('q', CONTROL);
                def.alt_keys = vec![key!('d', CONTROL)];
            }
            if def.id == ActionId::HalfPageDown {
                def.default_key = key!('d', CONTROL);
            }
            if def.id == ActionId::InterjectPrompt {
                def.default_key = key!(Enter, CONTROL);
                def.alt_keys = vec![key!('i', CONTROL)];
            }
            if def.id == ActionId::OpenExtensions {
                def.default_key = key!('l', CONTROL);
                def.alt_keys = vec![];
            }
        }
        Self::new(actions)
    }

    /// Registry pinned to Apple Terminal's interject binding (Ctrl+O is the
    /// interject chord; kitty keyboard protocol unavailable → Ctrl+Enter does
    /// not arrive). Host-independent stand-in for `default_actions` run under
    /// an Apple Terminal context.
    #[cfg(test)]
    pub fn apple_terminal_for_test() -> Self {
        Self::apple_terminal_for_mode_for_test(crate::app::ScreenMode::Fullscreen)
    }

    /// Mode-correct registry pinned to Apple Terminal's interject binding.
    #[cfg(test)]
    pub(crate) fn apple_terminal_for_mode_for_test(screen_mode: crate::app::ScreenMode) -> Self {
        use crate::key;
        let mut actions = defaults::default_actions(screen_mode, false);
        for def in actions.iter_mut() {
            if def.id == ActionId::InterjectPrompt {
                def.default_key = key!('o', CONTROL);
                def.alt_keys = vec![key!(Enter, CONTROL), key!('i', CONTROL)];
            }
        }
        Self::new(actions)
    }

    /// Registry pinned to VS Code family interject / extensions bindings.
    #[cfg(test)]
    pub fn vscode_family_for_test() -> Self {
        Self::vscode_family_for_mode_for_test(crate::app::ScreenMode::Fullscreen)
    }

    /// Mode-correct registry pinned to VS Code family bindings.
    #[cfg(test)]
    pub(crate) fn vscode_family_for_mode_for_test(screen_mode: crate::app::ScreenMode) -> Self {
        use crate::key;
        let mut actions = defaults::default_actions(screen_mode, false);
        for def in actions.iter_mut() {
            if def.id == ActionId::InterjectPrompt {
                def.default_key = key!('l', CONTROL);
                def.alt_keys = vec![];
            }
            if def.id == ActionId::OpenExtensions {
                def.default_key = key!(Null);
                def.alt_keys = vec![];
            }
        }
        Self::new(actions)
    }

    /// Look up an action like [`Self::lookup`] but optionally suppress
    /// bare-letter (or `Shift+letter`) bindings when `vim_mode == false`,
    /// for contexts where those letters double as text-input keys.
    ///
    /// Applies to [`When::ScrollbackFocused`] and [`When::DashboardFocused`]:
    /// the scrollback `j`/`k` scroll and the dashboard `j`/`k` row-nav
    /// only resolve when vim-mode is on. With vim-mode off the letters
    /// fall through so the caller can type them into its prompt — the
    /// dashboard dispatch input and the agent prompt both rely on this.
    ///
    /// Arrow / Tab / Esc / Space / PgUp / PgDn / `?` and all `Ctrl+letter`
    /// shortcuts always resolve — they come in as either the action's
    /// `default_key` (e.g. `PageUp`, `Esc`) or `alt_keys` (arrows on
    /// `SelectNext` / `Collapse` / etc.). Only the bare-letter primary
    /// or alt is gated; arrow `alt_keys` on the same `ActionDef` still match.
    pub fn lookup_with_mode(
        &self,
        event: &KeyEvent,
        context: When,
        vim_mode: bool,
    ) -> Option<ActionId> {
        // Contexts where a bare letter is also a typeable input key, so
        // the vim-off suppression applies. Both surfaces own a text
        // prompt that `j`/`k` must reach when vim-mode is off.
        let letter_gated = matches!(context, When::ScrollbackFocused | When::DashboardFocused);
        for def in &self.actions {
            if def.context != context {
                continue;
            }
            let suppress_default =
                !vim_mode && letter_gated && def.default_key.is_letter_or_shift_letter();
            if !suppress_default && def.default_key.matches(event) {
                return Some(def.id);
            }
            // Alt keys: when vim_mode is off, also suppress any alt key
            // that is itself a bare letter (e.g. the `j`/`k` alts on the
            // dashboard's SelectNext / SelectPrev). Non-letter alts
            // (arrows, Tab, Space) always match.
            for alt in &def.alt_keys {
                if !vim_mode && letter_gated && alt.is_letter_or_shift_letter() {
                    continue;
                }
                if alt.matches(event) {
                    return Some(def.id);
                }
            }
        }
        None
    }

    /// Find an action definition by ID.
    pub fn find(&self, id: ActionId) -> Option<&ActionDef> {
        self.actions.iter().find(|d| d.id == id)
    }

    /// Get hints for the shortcuts bar, filtered by contexts and sorted by priority.
    ///
    /// Pass multiple contexts to collect hints from all applicable levels.
    /// E.g., for scrollback mode: `&[ScrollbackFocused, AgentScreen, Always]`.
    pub fn hints(&self, contexts: &[When]) -> Vec<&ActionDef> {
        let mut hints: Vec<&ActionDef> = self
            .actions
            .iter()
            .filter(|def| def.hint_priority.is_some() && contexts.contains(&def.context))
            .collect();
        hints.sort_by_key(|def| def.hint_priority.unwrap_or(255));
        hints
    }

    /// Get hint items for the shortcuts bar, filtered by contexts and sorted by priority.
    ///
    /// Convenience method that converts `ActionDef`s to `HintItem`s.
    pub fn hint_items(&self, contexts: &[When]) -> Vec<HintItem> {
        self.hints(contexts).iter().map(|def| def.hint()).collect()
    }

    /// Get the current key binding for an action.
    pub fn key_for(&self, id: ActionId) -> Option<KeyShortcut> {
        self.find(id).map(|def| def.default_key)
    }

    /// Get the effective hint key for an action, accounting for vim mode.
    ///
    /// In non-vim mode, bare-letter scrollback bindings are suppressed.
    /// This returns the first non-letter alt key instead (e.g. arrow keys),
    /// so hints show a key that actually works.
    pub fn key_for_mode(&self, id: ActionId, vim_mode: bool) -> Option<KeyShortcut> {
        let def = self.find(id)?;
        if !vim_mode
            && def.context == When::ScrollbackFocused
            && def.default_key.is_letter_or_shift_letter()
        {
            def.alt_keys
                .iter()
                .find(|k| !k.is_letter_or_shift_letter())
                .copied()
        } else {
            Some(def.default_key)
        }
    }

    /// Get all actions (for command palette).
    pub fn all(&self) -> &[ActionDef] {
        &self.actions
    }
}

/// Emit [`echo_agent_telemetry::events::ShortcutUsed`] for an allowlisted binding.
///
/// See that event's docs for the product contract (intent-only allowlist).
/// `context` is a free-form surface label (`When::telemetry_name()` or e.g.
/// `"queue"` when focus is not a registry `When`). Call only on the commit
/// path that handles the allowlisted id, not peeks.
pub fn log_shortcut_used(key: &KeyEvent, action_id: ActionId, context: &str) {
    let Some(action) = shortcut_used_action_label(action_id) else {
        return;
    };
    echo_agent_telemetry::session_ctx::log_event(echo_agent_telemetry::events::ShortcutUsed {
        key: KeyShortcut::from(*key).display_telemetry(),
        action: action.to_string(),
        context: context.to_string(),
    });
}

/// Stable product-event labels for the allowlisted actions. `None` = do not emit.
fn shortcut_used_action_label(id: ActionId) -> Option<&'static str> {
    match id {
        ActionId::InterjectPrompt => Some("interject_prompt"),
        ActionId::OpenExtensions => Some("open_extensions"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn non_vscode_registry() -> ActionRegistry {
        ActionRegistry::non_vscode_for_test()
    }

    fn vscode_family_interject_registry() -> ActionRegistry {
        ActionRegistry::vscode_family_for_test()
    }

    #[test]
    fn shortcut_display() {
        assert_eq!(key!('q').display(), "q");
        assert_eq!(key!(Enter).display(), "Enter");
        assert_eq!(key!('c', CONTROL).display(), "Ctrl+c");
        assert_eq!(key!('l', CONTROL).display(), "Ctrl+l");
    }

    #[test]
    fn shortcut_used_allowlist_is_ctrl_l_actions_only() {
        assert_eq!(
            shortcut_used_action_label(ActionId::InterjectPrompt),
            Some("interject_prompt")
        );
        assert_eq!(
            shortcut_used_action_label(ActionId::OpenExtensions),
            Some("open_extensions")
        );
        assert_eq!(shortcut_used_action_label(ActionId::OpenDashboard), None);
        assert_eq!(shortcut_used_action_label(ActionId::SendPrompt), None);
        assert_eq!(When::PromptFocused.telemetry_name(), "prompt_focused");
        assert_eq!(When::AgentScreen.telemetry_name(), "agent_screen");
    }

    #[test]
    fn telemetry_chord_is_platform_stable() {
        let ctrl_l = key!('l', CONTROL);
        assert_eq!(ctrl_l.display_telemetry(), "Ctrl+L");
        // UI pretty form lowercases the letter; telemetry must not.
        assert_eq!(ctrl_l.display_pretty(), "Ctrl+l");
        assert_eq!(key!('o', CONTROL).display_telemetry(), "Ctrl+O");
        assert_eq!(key!(Enter, CONTROL).display_telemetry(), "Ctrl+Enter");
    }

    #[test]
    fn shortcut_matches() {
        let ctrl_c = key!('c', CONTROL);
        let ctrl_event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(ctrl_c.matches(&ctrl_event));

        let plain_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(!ctrl_c.matches(&plain_c));
    }

    #[test]
    fn ctrl_l_matches_interject_chord() {
        let chord = key!('l', CONTROL);
        let event = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert!(chord.matches(&event));
        assert!(!chord.matches(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)));
        assert!(!chord.matches(&KeyEvent::new(KeyCode::Null, KeyModifiers::NONE)));
        assert!(!chord.matches(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
    }

    #[test]
    fn vscode_family_interject_lookup_uses_ctrl_l_without_alts() {
        let registry = vscode_family_interject_registry();
        let ctrl_l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(
            registry.lookup(&ctrl_l, When::PromptFocused),
            Some(ActionId::InterjectPrompt)
        );
        assert!(registry.matches_id(ActionId::InterjectPrompt, &ctrl_l));
        // No alt chords on VS family.
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        assert_ne!(
            registry.lookup(&ctrl_enter, When::PromptFocused),
            Some(ActionId::InterjectPrompt)
        );
        assert!(!registry.matches_id(ActionId::InterjectPrompt, &ctrl_enter));
        let ctrl_i = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL);
        assert_ne!(
            registry.lookup(&ctrl_i, When::PromptFocused),
            Some(ActionId::InterjectPrompt)
        );
        // OpenExtensions must not claim Ctrl+L on VS family (plugins via /plugins).
        assert_ne!(
            registry.lookup(&ctrl_l, When::AgentScreen),
            Some(ActionId::OpenExtensions)
        );
        let def = registry
            .find(ActionId::InterjectPrompt)
            .expect("InterjectPrompt");
        assert!(def.alt_keys.is_empty());
    }

    #[test]
    fn interjection_possible_gate() {
        assert!(!ActionRegistry::interjection_possible(false, true));
        assert!(!ActionRegistry::interjection_possible(true, false));
        assert!(ActionRegistry::interjection_possible(true, true));
    }

    #[test]
    fn exact_context_matching() {
        let registry = non_vscode_registry();

        // Quit is When::Always — only found via Always lookup
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(registry.lookup(&ctrl_q, When::Always), Some(ActionId::Quit));
        // NOT found via scrollback or agent lookup (exact match)
        assert_eq!(registry.lookup(&ctrl_q, When::ScrollbackFocused), None);
        assert_eq!(registry.lookup(&ctrl_q, When::AgentScreen), None);

        // Ctrl-D is HalfPageDown at scrollback level, Quit at global level
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(
            registry.lookup(&ctrl_d, When::ScrollbackFocused),
            Some(ActionId::HalfPageDown)
        );
        assert_eq!(registry.lookup(&ctrl_d, When::Always), Some(ActionId::Quit));
    }

    #[test]
    fn screen_mode_registries_own_ctrl_g_and_share_ctrl_b() {
        let ctrl_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);

        for mode in [
            crate::app::ScreenMode::Fullscreen,
            crate::app::ScreenMode::Inline,
            crate::app::ScreenMode::Minimal,
        ] {
            let registry = ActionRegistry::defaults_for(mode);
            assert_eq!(
                registry.lookup(&ctrl_b, When::AgentScreen),
                Some(ActionId::SendToBackground)
            );
            assert!(registry.matches_id(ActionId::SendToBackground, &ctrl_b));
            assert!(!registry.matches_id(ActionId::SendToBackground, &ctrl_g));

            let ctrl_g_actions: Vec<_> = registry
                .all()
                .iter()
                .filter(|def| {
                    def.context == When::AgentScreen
                        && (def.default_key.matches(&ctrl_g)
                            || def.alt_keys.iter().any(|key| key.matches(&ctrl_g)))
                })
                .map(|def| def.id)
                .collect();
            let expected = if mode.is_minimal() {
                ActionId::EditPromptExternal
            } else {
                ActionId::ToggleTasks
            };
            assert_eq!(ctrl_g_actions, vec![expected]);
            assert_eq!(registry.lookup(&ctrl_g, When::AgentScreen), Some(expected));
            assert!(registry.matches_id(expected, &ctrl_g));

            assert_eq!(
                registry.find(ActionId::ToggleTasks).is_some(),
                !mode.is_minimal()
            );
            assert_eq!(
                registry.find(ActionId::EditPromptExternal).is_some(),
                mode.is_minimal()
            );
        }
    }

    #[test]
    fn minimal_registry_omits_unsupported_surfaces_and_dashboard_entry() {
        let minimal = ActionRegistry::defaults_for(crate::app::ScreenMode::Minimal);
        assert!(minimal.find(ActionId::OpenDashboard).is_none());
        assert!(minimal.find(ActionId::FocusScrollback).is_none());
        assert!(minimal.find(ActionId::ToggleMouseCapture).is_none());
        assert!(minimal.all().iter().all(|def| {
            !matches!(
                def.context,
                When::ScrollbackFocused | When::DashboardFocused | When::DashboardOverlay
            )
        }));
        let minimal_with_config =
            ActionRegistry::defaults_with_config_for(crate::app::ScreenMode::Minimal, true);
        assert!(
            minimal_with_config
                .find(ActionId::ToggleMouseCapture)
                .is_none()
        );
        assert_eq!(
            minimal.find(ActionId::SendPrompt).map(|def| def.context),
            Some(When::PromptFocused)
        );
        assert_eq!(
            minimal
                .find(ActionId::SendToBackground)
                .map(|def| def.context),
            Some(When::AgentScreen)
        );
        assert_eq!(
            minimal.find(ActionId::Quit).map(|def| def.context),
            Some(When::Always)
        );
        assert_eq!(
            minimal.find(ActionId::NewSession).map(|def| def.context),
            Some(When::Always)
        );

        let ctrl_backslash = KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL);
        assert_eq!(minimal.lookup(&ctrl_backslash, When::Always), None);

        for mode in [
            crate::app::ScreenMode::Fullscreen,
            crate::app::ScreenMode::Inline,
        ] {
            let registry = ActionRegistry::defaults_for(mode);
            assert!(registry.find(ActionId::OpenDashboard).is_some());
            assert_eq!(
                registry
                    .find(ActionId::FocusScrollback)
                    .map(|def| def.context),
                Some(When::PromptFocused)
            );
            assert_eq!(
                registry.lookup(&ctrl_backslash, When::Always),
                Some(ActionId::OpenDashboard)
            );
            assert!(
                registry
                    .all()
                    .iter()
                    .any(|def| def.context == When::ScrollbackFocused)
            );
            assert!(
                registry
                    .all()
                    .iter()
                    .any(|def| def.context == When::DashboardFocused)
            );
            assert!(
                registry
                    .all()
                    .iter()
                    .any(|def| def.context == When::DashboardOverlay)
            );
        }
    }

    #[test]
    fn send_to_background_help_is_mode_accurate() {
        let fullscreen = ActionRegistry::defaults_for(crate::app::ScreenMode::Fullscreen)
            .find(ActionId::SendToBackground)
            .and_then(|def| def.long_help)
            .expect("fullscreen background help");
        assert!(fullscreen.contains("tasks pane (Ctrl+G)"));
        assert!(!fullscreen.contains("/tasks"));

        let minimal = ActionRegistry::defaults_for(crate::app::ScreenMode::Minimal)
            .find(ActionId::SendToBackground)
            .and_then(|def| def.long_help)
            .expect("minimal background help");
        assert!(minimal.contains("/tasks"));
        assert!(!minimal.contains("tasks pane (Ctrl+G)"));
    }

    #[test]
    fn terminal_family_test_registries_preserve_screen_mode() {
        for registry in [
            ActionRegistry::non_vscode_for_mode_for_test(crate::app::ScreenMode::Minimal),
            ActionRegistry::apple_terminal_for_mode_for_test(crate::app::ScreenMode::Minimal),
            ActionRegistry::vscode_family_for_mode_for_test(crate::app::ScreenMode::Minimal),
        ] {
            assert!(registry.find(ActionId::EditPromptExternal).is_some());
            assert!(registry.find(ActionId::ToggleTasks).is_none());
            assert!(registry.find(ActionId::OpenDashboard).is_none());
            assert!(registry.all().iter().all(|def| {
                !matches!(
                    def.context,
                    When::ScrollbackFocused | When::DashboardFocused | When::DashboardOverlay
                )
            }));
        }
    }

    #[test]
    fn cancel_at_agent_level() {
        let registry = ActionRegistry::defaults();
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            registry.lookup(&ctrl_c, When::AgentScreen),
            Some(ActionId::CancelTurn)
        );
        // Not at scrollback or global level
        assert_eq!(registry.lookup(&ctrl_c, When::ScrollbackFocused), None);
        assert_eq!(registry.lookup(&ctrl_c, When::Always), None);
    }

    #[test]
    fn scrollback_actions_only_at_scrollback_level() {
        let registry = ActionRegistry::defaults();
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(
            registry.lookup(&j, When::ScrollbackFocused),
            Some(ActionId::SelectNext)
        );
        assert_eq!(registry.lookup(&j, When::AgentScreen), None);
        assert_eq!(registry.lookup(&j, When::Always), None);
    }

    #[test]
    fn find_action_def() {
        let registry = ActionRegistry::defaults();
        let def = registry.find(ActionId::Quit).unwrap();
        assert_eq!(def.label, "quit");
        assert!(def.requires_confirmation);
    }

    #[test]
    fn multi_context_hints() {
        let registry = ActionRegistry::defaults();
        // Collect hints from multiple levels (as the shortcuts bar would)
        let hints = registry.hints(&[When::ScrollbackFocused, When::AgentScreen, When::Always]);
        assert!(!hints.is_empty());
        // Should include quit (Always) and scrollback actions
        let ids: Vec<_> = hints.iter().map(|h| h.id).collect();
        assert!(ids.contains(&ActionId::Quit));
        assert!(ids.contains(&ActionId::SelectNext));
        // Sorted by priority
        for window in hints.windows(2) {
            assert!(window[0].hint_priority <= window[1].hint_priority);
        }
    }

    #[test]
    fn quit_requires_confirmation() {
        let registry = ActionRegistry::defaults();
        let def = registry.find(ActionId::Quit).unwrap();
        assert!(def.requires_confirmation);
    }

    #[test]
    fn cancel_does_not_require_confirmation() {
        let registry = ActionRegistry::defaults();
        let def = registry.find(ActionId::CancelTurn).unwrap();
        assert!(!def.requires_confirmation);
    }

    #[test]
    fn toggle_mouse_capture_disabled_by_default() {
        // Opt-in via config.toml; default registry must not register it.
        let registry = ActionRegistry::defaults();
        assert!(registry.find(ActionId::ToggleMouseCapture).is_none());
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(registry.lookup(&ctrl_r, When::ScrollbackFocused), None);
    }

    #[test]
    fn toggle_mouse_capture_bound_on_scrollback_when_enabled() {
        let registry = ActionRegistry::defaults_with_config(true);
        // Registered and discoverable (command palette / cheatsheet) only
        // when config enables the feature.
        let def = registry
            .find(ActionId::ToggleMouseCapture)
            .expect("ToggleMouseCapture must be registered when config-enabled");
        assert_eq!(def.category, Category::Panels);
        assert_eq!(def.context, When::ScrollbackFocused);

        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        let ctrl_m = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL);
        let f9 = KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE);
        let ctrl_shift_m = KeyEvent::new(
            KeyCode::Char('m'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        let ctrl_space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL);

        // Single binding: Ctrl+R while scrollback is focused.
        assert_eq!(
            registry.lookup(&ctrl_r, When::ScrollbackFocused),
            Some(ActionId::ToggleMouseCapture)
        );
        // Not on agent/prompt contexts (Ctrl+R is deliberately unbound there;
        // agent keeps the model picker on Ctrl+M).
        assert_eq!(registry.lookup(&ctrl_r, When::AgentScreen), None);
        assert_eq!(registry.lookup(&ctrl_r, When::PromptFocused), None);
        assert_eq!(
            registry.lookup(&ctrl_m, When::AgentScreen),
            Some(ActionId::ModelPicker)
        );
        assert_eq!(
            registry.lookup(&ctrl_m, When::PromptFocused),
            Some(ActionId::ToggleMultiline)
        );
        // Former mouse-toggle dual bindings removed from scrollback.
        assert_eq!(registry.lookup(&f9, When::ScrollbackFocused), None);
        assert_eq!(registry.lookup(&f9, When::AgentScreen), None);
        // Ctrl+Shift+M is no longer the voice chord — it resolves to nothing.
        assert_eq!(
            registry.lookup(&ctrl_shift_m, When::ScrollbackFocused),
            None
        );
        assert_eq!(registry.lookup(&ctrl_shift_m, When::Always), None);
        // Voice capture is bound to BOTH Ctrl+Space and F8, and is global
        // (`When::Always`) so it resolves on the agent screen and the dashboard
        // alike (distinct from Ctrl+M model picker / multiline). It is not
        // agent-scoped, so an exact AgentScreen lookup misses.
        assert_eq!(
            registry.lookup(&ctrl_space, When::Always),
            Some(ActionId::VoiceToggle)
        );
        assert_eq!(registry.lookup(&ctrl_space, When::AgentScreen), None);
        let f8 = KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE);
        assert_eq!(
            registry.lookup(&f8, When::Always),
            Some(ActionId::VoiceToggle)
        );
    }

    #[test]
    fn exit_session_is_command_only() {
        let registry = ActionRegistry::defaults();
        assert!(registry.find(ActionId::ExitSession).is_none());
    }

    fn binds_ctrl_4(def: &ActionDef) -> bool {
        let ctrl_4 = key!('4', CONTROL);
        def.default_key == ctrl_4 || def.alt_keys.contains(&ctrl_4)
    }

    // Host-default registry: at most one of these two owns Ctrl+4.
    #[test]
    fn open_dashboard_and_toggle_queue_do_not_both_bind_ctrl_4() {
        let registry = ActionRegistry::defaults();
        let dashboard = registry.find(ActionId::OpenDashboard).unwrap();
        let queue = registry.find(ActionId::ToggleQueue).unwrap();
        assert!(!(binds_ctrl_4(dashboard) && binds_ctrl_4(queue)));
        assert_eq!(dashboard.default_key, key!('\\', CONTROL));
        // Exactly one of the two binds Ctrl+4 under host defaults (legacy alt XOR queue primary).
        assert!(
            binds_ctrl_4(dashboard) ^ binds_ctrl_4(queue),
            "exactly one of OpenDashboard/ToggleQueue must bind Ctrl+4 on this host"
        );
    }

    // Crossterm maps C0 FS (physical Ctrl+\) to Char('4')+CONTROL without KKP.
    #[test]
    fn open_dashboard_accepts_legacy_ctrl_4_encoding() {
        let mut actions = default_actions(crate::app::ScreenMode::Fullscreen, false);
        for def in actions.iter_mut() {
            if def.id == ActionId::OpenDashboard {
                def.alt_keys = vec![key!('4', CONTROL)];
            }
            if def.id == ActionId::ToggleQueue {
                def.default_key = key!(';', CONTROL);
                def.alt_keys = vec![key!('\'', CONTROL)];
            }
        }
        let registry = ActionRegistry::new(actions);
        let ctrl_backslash = KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL);
        let ctrl_4 = KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL);
        let ctrl_semicolon = KeyEvent::new(KeyCode::Char(';'), KeyModifiers::CONTROL);

        assert_eq!(
            registry.lookup(&ctrl_backslash, When::Always),
            Some(ActionId::OpenDashboard)
        );
        assert_eq!(
            registry.lookup(&ctrl_4, When::Always),
            Some(ActionId::OpenDashboard)
        );
        assert_eq!(registry.lookup(&ctrl_4, When::AgentScreen), None);
        assert_eq!(
            registry.lookup(&ctrl_semicolon, When::AgentScreen),
            Some(ActionId::ToggleQueue)
        );
    }

    // Mac-VS pin: ToggleQueue primary Ctrl+4; OpenDashboard keeps Ctrl+\ only.
    #[test]
    fn mac_vscode_ctrl_4_stays_toggle_queue_not_open_dashboard() {
        let mut actions = default_actions(crate::app::ScreenMode::Fullscreen, false);
        for def in actions.iter_mut() {
            if def.id == ActionId::ToggleQueue {
                def.default_key = key!('4', CONTROL);
                def.alt_keys = vec![key!(';', CONTROL), key!('\'', CONTROL)];
            }
            if def.id == ActionId::OpenDashboard {
                def.alt_keys = vec![];
            }
        }
        let registry = ActionRegistry::new(actions);
        let ctrl_4 = KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL);
        let ctrl_backslash = KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL);

        assert_eq!(
            registry.lookup(&ctrl_4, When::AgentScreen),
            Some(ActionId::ToggleQueue)
        );
        assert_eq!(registry.lookup(&ctrl_4, When::Always), None);
        assert_eq!(
            registry.lookup(&ctrl_backslash, When::Always),
            Some(ActionId::OpenDashboard)
        );
    }

    #[test]
    fn shortcuts_help_registered_with_ctrl_dot_and_ctrl_x() {
        let registry = ActionRegistry::defaults();
        let def = registry
            .find(ActionId::ShortcutsHelp)
            .expect("ShortcutsHelp action should be registered");
        assert_eq!(def.label, "shortcuts");
        assert!(!def.requires_confirmation);

        // Both Ctrl+. and Ctrl+X should resolve to ShortcutsHelp
        // (one is default_key, the other is alt_key — which is which
        // depends on the terminal brand at runtime).
        let ctrl_dot = KeyEvent::new(KeyCode::Char('.'), KeyModifiers::CONTROL);
        let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(
            registry.lookup(&ctrl_dot, When::AgentScreen),
            Some(ActionId::ShortcutsHelp)
        );
        assert_eq!(
            registry.lookup(&ctrl_x, When::AgentScreen),
            Some(ActionId::ShortcutsHelp)
        );
    }
}
