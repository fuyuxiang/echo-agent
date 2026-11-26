use echo_agent_config_types::DisplayRefreshSettings;
use serde::{Deserialize, Serialize};

use echo_agent_status_line::StatusLineConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub max_thoughts_width: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Model ID to use for the secondary agent when forking.
    /// Defaults to the main default model (from default_models.json).
    pub fork_secondary_model: String,
    /// YOLO mode. Read by `util::config`, declared here for `serde_ignored`.
    #[serde(default)]
    pub yolo: bool,
    /// UI theme alias. Read by `util::config`, declared here for `serde_ignored`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_theme: Option<String>,
    /// Compact mode. Read by pager, declared here for `serde_ignored`.
    #[serde(default)]
    pub compact_mode: bool,
    /// Simple mode. Read by pager, declared here for `serde_ignored`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simple_mode: Option<bool>,
    /// Read by `load_permission_mode()`. Declared for `serde_ignored`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// Legacy name for `permission_mode`. Declared for `serde_ignored`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<String>,
    /// Which permission option the cursor preselects on the **first**
    /// permission prompt of a session. One of `allow_once`, `allow_always`,
    /// or `reject`. After the first prompt, the cursor sticks to the user's
    /// last-used option kind. When unset, the first prompt preselects the
    /// "Always allow on all sessions" (enable-always-approve) row. Read by
    /// the pager's permission view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_selected_permission: Option<String>,
    /// Written by the pager's appearance persist module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_timestamps: Option<bool>,
    /// Timeline sidebar (per-turn tick rail in place of the scrollbar).
    /// `None` = off (client default; opt-in). Written by the pager's settings modal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_timeline: Option<bool>,
    /// Snap a just-sent prompt to the viewport top. `None` = on (default).
    /// Written by the pager's settings modal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_flip_on_send: Option<bool>,
    /// Ask before rewinding conversation history. `None` = on (default).
    /// Written by the pager's settings modal / rewind "Yes, and don't ask again".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_before_rewind: Option<bool>,
    /// Theme to use when the OS is in dark mode. Written by the pager's theme persist module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_dark_theme: Option<String>,
    /// Theme to use when the OS is in light mode. Written by the pager's theme persist module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_light_theme: Option<String>,
    /// Mouse-wheel and trackpad scroll speed multiplier (1–100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_speed: Option<u8>,
    /// Force scroll input classification (`auto` | `wheel` | `trackpad`).
    /// Written by the pager's settings modal; unset defaults to `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_mode: Option<String>,
    /// Invert vertical scroll direction ("natural" scrolling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invert_scroll: Option<bool>,
    /// Lines per scroll tick, applied to BOTH wheel and trackpad pricing
    /// (1–10). Unset keeps the per-terminal scroll profile's values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_lines: Option<u8>,
    /// Vim-style scrollback navigation (hjkl, gg/G, /).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vim_mode: Option<bool>,
    /// How ` ```mermaid ` code blocks are rendered (`auto` | `on` | `off`).
    /// Written by the pager's settings modal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_mermaid: Option<String>,
    /// Hunk-tracker mode the pager advertises to the agent (`agent_only` |
    /// `all_dirty` | `off`). Written by the pager's settings modal; read at
    /// connect time (CLI `--hunk-tracker-mode` / `ECHO_AGENT_HUNK_TRACKER` override
    /// it). Unset defaults to `off`, which disables hunk tracking entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunk_tracker_mode: Option<String>,
    /// Voice capture chord behavior: `toggle` or `hold` (hold-to-talk; needs a
    /// Kitty-protocol terminal, else falls back to toggle). Written by the
    /// settings modal; unset defaults to `hold`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_capture_mode: Option<String>,
    /// Speech-to-text language preference for voice dictation. A EchoAgent STT
    /// catalog code (`en`, `es`, `ja`, … — see EchoAgent STT supported languages) or
    /// `auto` (system locale, resolved at connect). Written by the settings
    /// modal; unset leaves `[voice].language` / default `en`. When set, overrides
    /// `[voice].language` for the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_stt_language: Option<String>,
    /// Whether the Ctrl+Space / F8 voice-dictation shortcut is active. Written
    /// by the settings modal; unset defaults to `true` (shortcut on). When
    /// `false` the chord is ignored — `/voice` still starts dictation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_keybind_enabled: Option<bool>,
    /// When `true`, registers `Ctrl+R` (while scrollback is focused) to toggle
    /// terminal mouse reporting (mouse capture) so users can hand selection back
    /// to the terminal for native click-drag copy/paste. Opt-in only; unset/false
    /// leaves mouse reporting always on with no toggle shortcut. The prompt keeps
    /// `Ctrl+R` for history search — focus scrollback (Esc/Tab) first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse_reporting_toggle: Option<bool>,
    /// When cancelling a parent turn with running subagents: `always_stop` stops
    /// them without prompting, `always_continue` leaves them running without
    /// prompting. Unset/`ask` shows the cancel-turn picker. Written by the pager
    /// when the user picks "Always stop" / "Always continue".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_subagents_on_turn_cancel: Option<String>,
    /// User knob for the `remember_tool_approvals` gate: per-tool "Always
    /// allow …" prompt options (resolver default: on). Written by the settings
    /// modal; requirements/env/managed/remote settings also feed the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember_tool_approvals: Option<bool>,
    /// In-app drag selection highlight: `flash` | `hold` (legacy bool accepted).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_keep_text_selection"
    )]
    pub keep_text_selection: Option<String>,
    /// Legacy TTL ms; only `Some(0)` counts when `keep_text_selection` is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_highlight_duration_ms: Option<u64>,
    /// Show agent thinking/reasoning blocks in the TUI scrollback.
    /// `None` = on (client default). Written by the pager's settings modal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_thinking_blocks: Option<bool>,
    /// Fold runs of consecutive non-destructive tool calls (reads, searches,
    /// lists) into one transcript row. `None` = on (client default). Written
    /// by the pager's settings modal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_tool_verbs: Option<bool>,
    /// Show Edit tool calls as a collapsed one-line `+N/-M` diffstat summary
    /// by default (expand for the diff). `None` = off (client default).
    /// Written by the pager's settings modal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed_edit_blocks: Option<bool>,
    /// Next-prompt suggestions (tab autocomplete ghost text) after each turn.
    /// `None` = on (client default). Written by the pager's settings modal;
    /// the `ECHO_AGENT_PROMPT_SUGGESTIONS` env var overrides at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_suggestions: Option<bool>,
    /// Startup cursor style: `None` (default) inherits the terminal's own
    /// style; `Some(true)` forces the legacy blinking block, `Some(false)` a
    /// steady block. Config-file-only knob (no /settings row).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_blink: Option<bool>,
    /// `"fullscreen"` | `"minimal"`; unset → product default fullscreen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_mode: Option<String>,
    /// Retired hidden opt-in for terminal-like double/triple-click word/line
    /// selection. Superseded by `keep_text_selection = "word_select"`. Still
    /// read only when `keep_text_selection` is unset; Settings clears this on
    /// write. `"word_select"` | unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub double_click_action: Option<String>,
    /// Per-tip contextual-hint opt-outs (`[ui.contextual_hints]`). Each `None`
    /// inherits the remote/default (on); `Some` is a user-explicit choice that
    /// beats the remote tier. Skipped on the wire when untouched so the section
    /// only appears once a user toggles a tip.
    #[serde(default, skip_serializing_if = "ContextualHints::is_default")]
    pub contextual_hints: ContextualHints,
    /// Combine consecutive queued follow-ups into one turn. `None` = off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combine_queued_prompts: Option<bool>,
    /// Mid-turn follow-up routing: `"queue"` (default) or `"steer"`. `None`
    /// behaves as queue. Steer promotes server-queued follow-ups as
    /// interjections at the next tool or model safe point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_behavior: Option<String>,
    /// Display-refresh probe + auto-cadence (`[ui.display_refresh]`). Per-field
    /// `None` inherits remote/default; skipped when untouched.
    #[serde(default, skip_serializing_if = "DisplayRefreshSettings::is_default")]
    pub display_refresh: DisplayRefreshSettings,
    /// `[ui.status_line]`. Disabled by default.
    #[serde(default, skip_serializing_if = "status_line_should_not_be_saved")]
    pub status_line: StatusLineConfig,
}

fn status_line_should_not_be_saved(status_line: &StatusLineConfig) -> bool {
    status_line.is_default() || status_line.problem().is_some()
}

/// User-config opt-outs for the per-tip contextual hints, serialized as
/// `[ui.contextual_hints]`. Per-field `None` means "inherit remote/default";
/// `Some(bool)` is a user-explicit choice (needed so the resolver can let it
/// beat the remote tier).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextualHints {
    /// Undo tip (Ctrl+Z after a substantial draft wipe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<bool>,
    /// Plan-mode nudge (typing a planning keyword).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode: Option<bool>,
    /// Clipboard-image input tip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_input: Option<bool>,
    /// Send-now tip after queuing a mid-turn follow-up (InterjectPrompt chord).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_now: Option<bool>,
    /// Small-screen tip (`/compact-mode` hint on smallish terminals).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_screen: Option<bool>,
    /// Word-select tip after double-clicking scrollback while Text selection
    /// is still fold/nav (`flash` / `hold`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_select: Option<bool>,
    /// SSH wrap session-load tip (recommend `echoagent wrap ssh` when the session
    /// runs over SSH without an OSC 52 sink).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_wrap: Option<bool>,
}

impl ContextualHints {
    /// True when no tip has a user-explicit value (all inherit). Lets the
    /// section stay absent from `config.toml` until the user toggles a tip.
    pub fn is_default(&self) -> bool {
        self.undo.is_none()
            && self.plan_mode.is_none()
            && self.image_input.is_none()
            && self.send_now.is_none()
            && self.small_screen.is_none()
            && self.word_select.is_none()
            && self.ssh_wrap.is_none()
    }
}

const DEFAULT_MAX_THOUGHTS_WIDTH: u16 = 120;

fn deserialize_keep_text_selection<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Bool(bool),
        Str(String),
    }

    Ok(
        Option::<Raw>::deserialize(deserializer)?.map(|raw| match raw {
            Raw::Bool(true) => "hold".to_string(),
            Raw::Bool(false) => "flash".to_string(),
            Raw::Str(s) => s,
        }),
    )
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            max_thoughts_width: DEFAULT_MAX_THOUGHTS_WIDTH,
            theme: None,
            fork_secondary_model: echo_agent_models::default_model().to_string(),
            yolo: false,
            ui_theme: None,
            compact_mode: false,
            simple_mode: None,
            permission_mode: None,
            approval_mode: None,
            default_selected_permission: None,
            show_timestamps: None,
            show_timeline: None,
            page_flip_on_send: None,
            confirm_before_rewind: None,
            auto_dark_theme: None,
            auto_light_theme: None,
            scroll_speed: None,
            scroll_mode: None,
            invert_scroll: None,
            scroll_lines: None,
            vim_mode: None,
            render_mermaid: None,
            hunk_tracker_mode: None,
            voice_capture_mode: None,
            voice_stt_language: None,
            voice_keybind_enabled: None,
            mouse_reporting_toggle: None,
            remember_tool_approvals: None,
            cancel_subagents_on_turn_cancel: None,
            keep_text_selection: None,
            selection_highlight_duration_ms: None,
            show_thinking_blocks: None,
            group_tool_verbs: None,
            collapsed_edit_blocks: None,
            prompt_suggestions: None,
            cursor_blink: None,
            screen_mode: None,
            double_click_action: None,
            contextual_hints: ContextualHints::default(),
            combine_queued_prompts: None,
            follow_up_behavior: None,
            display_refresh: DisplayRefreshSettings::default(),
            status_line: StatusLineConfig::default(),
        }
    }
}

impl UiConfig {
    /// The single source of truth for the timeline-sidebar default (opt-in).
    /// Flip this one line to change the default everywhere.
    ///
    // TODO: migrate the other boolean UI settings (show_timestamps,
    // simple_mode, show_thinking_blocks, …) to the same const + resolver
    // pattern. They currently duplicate their default literal across
    // cache.rs / config.rs / defs.rs / setters.rs / registry.rs and rely on
    // the registry drift-guard test to catch mismatches.
    pub const SHOW_TIMELINE_DEFAULT: bool = false;

    /// Resolved timeline-sidebar setting: the configured value, or
    /// [`Self::SHOW_TIMELINE_DEFAULT`] when unset. The one place the default
    /// is applied — every layer (cache, appearance config, settings modal)
    /// reads through here so they cannot drift.
    pub fn show_timeline_enabled(&self) -> bool {
        self.show_timeline.unwrap_or(Self::SHOW_TIMELINE_DEFAULT)
    }

    /// Default for [`Self::page_flip_on_send`] when unset.
    pub const PAGE_FLIP_ON_SEND_DEFAULT: bool = true;

    pub fn page_flip_on_send_enabled(&self) -> bool {
        self.page_flip_on_send
            .unwrap_or(Self::PAGE_FLIP_ON_SEND_DEFAULT)
    }

    /// Default for [`Self::confirm_before_rewind`] when unset.
    pub const CONFIRM_BEFORE_REWIND_DEFAULT: bool = true;

    pub fn confirm_before_rewind_enabled(&self) -> bool {
        self.confirm_before_rewind
            .unwrap_or(Self::CONFIRM_BEFORE_REWIND_DEFAULT)
    }

    /// Canonical default for `[ui].follow_up_behavior`.
    pub const FOLLOW_UP_BEHAVIOR_DEFAULT: &'static str = "queue";

    /// Resolved follow-up behavior: `"queue"` or `"steer"`.
    /// Unknown values fall back to queue.
    pub fn follow_up_behavior(&self) -> &'static str {
        match self.follow_up_behavior.as_deref() {
            Some("steer") => "steer",
            _ => Self::FOLLOW_UP_BEHAVIOR_DEFAULT,
        }
    }

    /// True when mid-turn follow-ups should promote as interjections (Steer).
    pub fn follow_up_steer_enabled(&self) -> bool {
        self.follow_up_behavior() == "steer"
    }

    /// True when the highlight should not timer-dismiss (`hold` / `word_select`,
    /// or legacy duration 0).
    pub fn keep_text_selection_enabled(&self) -> bool {
        if let Some(ref s) = self.keep_text_selection {
            return s == "hold" || s == "word_select";
        }
        matches!(self.selection_highlight_duration_ms, Some(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The leniency lives in `StatusLineConfig`'s own `Deserialize`; this pins
    /// that the real `[ui]` table gets it, and that `skip_serializing_if` keeps
    /// a section we misread out of a save that merges per key.
    #[test]
    fn one_typo_in_the_status_line_cannot_fail_the_rest_of_the_ui_table() {
        let ui: UiConfig = serde_json::from_str(
            r#"{"theme": "kanagawa", "status_line": {"type": "builtin", "items": "cwd"}}"#,
        )
        .expect("[ui] must survive whatever the status line says");

        assert_eq!(ui.theme.as_deref(), Some("kanagawa"));
        assert!(ui.status_line.problem().is_some());

        let saved = serde_json::to_value(&ui).expect("[ui] serializes");
        assert_eq!(saved["theme"], "kanagawa");
        assert!(
            saved.get("status_line").is_none(),
            "a section we misread must not be written back over"
        );
    }

    /// A settings write merges per key, so a section the parse could not read in
    /// full must stay out of it.
    #[test]
    fn only_a_status_line_we_read_in_full_is_written_back() {
        for json in [
            r#"{"status_line": {"type": "enabled"}}"#,
            // A type this build removed reads like any other unknown one.
            r#"{"status_line": {"type": "static", "text": "hi"}}"#,
            r#"{"status_line": {"type": "builtin", "items": "cwd"}}"#,
            r#"{"status_line": "builtin"}"#,
            r#"{"status_line": {"padding": 2}}"#,
            "{}",
        ] {
            let ui: UiConfig = serde_json::from_str(json).expect("[ui] survives it");
            let saved = serde_json::to_value(&ui).expect("[ui] serializes");
            assert!(saved.get("status_line").is_none(), "{json}");
        }

        // An unknown key is preserved by the merge, so the section still
        // persists. `off` is a spelling of `disabled`, so it is a choice that
        // was read rather than a value that was not, and it saves as the
        // canonical name.
        for json in [
            r#"{"status_line": {"type": "command", "command": "x"}}"#,
            r#"{"status_line": {"type": "off"}}"#,
        ] {
            let ui: UiConfig = serde_json::from_str(json).expect("[ui] survives it");
            let saved = serde_json::to_value(&ui).expect("[ui] serializes");
            assert!(saved.get("status_line").is_some(), "{json}");
        }
    }

    #[test]
    fn page_flip_on_send_defaults_on() {
        assert!(UiConfig::default().page_flip_on_send_enabled());
        let off = UiConfig {
            page_flip_on_send: Some(false),
            ..Default::default()
        };
        assert!(!off.page_flip_on_send_enabled());
    }

    #[test]
    fn confirm_before_rewind_defaults_on() {
        assert!(UiConfig::default().confirm_before_rewind_enabled());
        let off = UiConfig {
            confirm_before_rewind: Some(false),
            ..Default::default()
        };
        assert!(!off.confirm_before_rewind_enabled());
    }

    #[test]
    fn keep_text_selection_enabled_precedence() {
        let mut ui = UiConfig::default();
        assert!(!ui.keep_text_selection_enabled());

        ui.selection_highlight_duration_ms = Some(0);
        assert!(ui.keep_text_selection_enabled());

        ui.selection_highlight_duration_ms = Some(150);
        assert!(!ui.keep_text_selection_enabled());

        ui.selection_highlight_duration_ms = Some(0);
        ui.keep_text_selection = Some("flash".into());
        assert!(!ui.keep_text_selection_enabled());

        ui.keep_text_selection = Some("hold".into());
        ui.selection_highlight_duration_ms = Some(999);
        assert!(ui.keep_text_selection_enabled());

        ui.keep_text_selection = Some("hold".into());
        ui.selection_highlight_duration_ms = None;
        assert!(ui.keep_text_selection_enabled());

        // `word_select` implies hold (persistent highlight).
        ui.keep_text_selection = Some("word_select".into());
        ui.selection_highlight_duration_ms = None;
        assert!(ui.keep_text_selection_enabled());
    }

    #[test]
    fn keep_text_selection_deserializes_legacy_bool_and_string() {
        let from_true: UiConfig = serde_json::from_str(r#"{"keep_text_selection": true}"#).unwrap();
        assert_eq!(from_true.keep_text_selection.as_deref(), Some("hold"));

        let from_false: UiConfig =
            serde_json::from_str(r#"{"keep_text_selection": false}"#).unwrap();
        assert_eq!(from_false.keep_text_selection.as_deref(), Some("flash"));

        let from_hold: UiConfig =
            serde_json::from_str(r#"{"keep_text_selection": "hold"}"#).unwrap();
        assert_eq!(from_hold.keep_text_selection.as_deref(), Some("hold"));

        let from_flash: UiConfig =
            serde_json::from_str(r#"{"keep_text_selection": "flash"}"#).unwrap();
        assert_eq!(from_flash.keep_text_selection.as_deref(), Some("flash"));
    }

    #[test]
    fn display_refresh_nested_deserialize() {
        let ui: UiConfig = serde_json::from_str(
            r#"{"display_refresh": {"auto_cadence_enabled": true, "floor_ms": 7, "probe_enabled": false}}"#,
        )
        .unwrap();
        assert_eq!(ui.display_refresh.auto_cadence_enabled, Some(true));
        assert_eq!(ui.display_refresh.floor_ms, Some(7));
        assert_eq!(ui.display_refresh.probe_enabled, Some(false));
        assert!(!ui.display_refresh.is_default());
    }

    #[test]
    fn display_refresh_default_is_skipped_shape() {
        assert!(DisplayRefreshSettings::default().is_default());
        let ui = UiConfig::default();
        assert!(ui.display_refresh.is_default());
    }
}
