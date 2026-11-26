//! Individual setting setters with persistence effects and toasts.

use super::ui::{refresh_open_settings_modals, save_success_toast};
use crate::app::actions::Effect;
use crate::app::app_view::{ActiveView, AppView};
use agent_client_protocol as acp;

/// Set multiline input mode — swap Enter and Shift+Enter behavior.
///
/// PAGER-OWNED: ephemeral, no `Effect::PersistSetting`. On the agent
/// view this is per-session (`AgentView::multiline_mode`); on the
/// dashboard it lives on `DashboardState::multiline_mode`. Idempotent.
pub(in crate::app::dispatch) fn set_multiline_mode(app: &mut AppView, new: bool) -> Vec<Effect> {
    if matches!(app.active_view, ActiveView::AgentDashboard) {
        let Some(d) = app.dashboard.as_mut() else {
            return vec![];
        };
        if d.multiline_mode == new {
            return vec![];
        }
        d.multiline_mode = new;
        tracing::info!(
            target: "settings",
            key = "multiline_mode",
            value = new,
            surface = "dashboard",
            "setting changed",
        );
        app.show_toast(&save_success_toast("Multiline", new));
        return vec![];
    }

    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    if agent.multiline_mode == new {
        return vec![];
    }
    agent.multiline_mode = new;
    // Refresh modal snapshot so the indicator reflects the new value.
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "multiline_mode",
        value = new,
        "setting changed",
    );
    app.show_toast(&save_success_toast("Multiline", new));
    vec![]
}

/// State-only mutation for `render_mermaid`: update the process-wide cache so
/// every block picks up the new value on the next frame. The cache is the
/// render-path source of truth; the disk write goes through `PersistSetting`.
pub(super) fn set_render_mermaid_inner(kind: crate::appearance::RenderMermaid) {
    crate::appearance::cache::set_render_mermaid(kind);
}

/// Set how ` ```mermaid ` code blocks render (auto/on/off).
///
/// SHELL-OWNED: persisted to `[ui].render_mermaid` via `Effect::PersistSetting`
/// (parity with `vim_mode`), with the cache mirror updated optimistically.
pub(in crate::app::dispatch) fn set_render_mermaid(
    app: &mut AppView,
    kind: crate::appearance::RenderMermaid,
) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_render_mermaid();
    if prev == kind {
        return vec![];
    }
    set_render_mermaid_inner(kind);
    // Refresh modal snapshot so the picker reflects the new value.
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "render_mermaid",
        value = kind.as_canonical(),
        "setting changed",
    );
    app.show_toast(&format!("\u{2713} Mermaid: {}", kind.as_canonical()));
    vec![Effect::PersistSetting {
        key: "render_mermaid",
        value: crate::settings::SettingValue::Enum(kind.as_canonical()),
        rollback_value: crate::settings::SettingValue::Enum(prev.as_canonical()),
    }]
}

/// Mirror the canonical mode into `app.current_ui` so `current_value_for` stays
/// in sync. Called by the commit path AND by [`apply_setting_rollback`](super::ui::apply_setting_rollback).
pub(super) fn set_hunk_tracker_mode_inner(app: &mut AppView, canonical: &str) {
    app.current_ui.hunk_tracker_mode = Some(canonical.to_string());
}

pub(super) fn set_screen_mode_inner(app: &mut AppView, canonical: &str) {
    app.current_ui.screen_mode = Some(canonical.to_string());
}

/// Persist `[ui].screen_mode` (`fullscreen` | `minimal`). Restart-required.
///
/// Unset is *displayed* as Fullscreen but is not an explicit on-disk value —
/// choosing Fullscreen when missing must still write, or legacy pager.toml /
/// leaky-terminal paths can keep applying after the user confirmed Fullscreen.
pub(in crate::app::dispatch) fn set_screen_mode(app: &mut AppView, value: String) -> Vec<Effect> {
    let canonical = crate::settings::canonical_screen_mode(Some(&value));
    let prev_raw = app.current_ui.screen_mode.as_deref();
    let prev = crate::settings::canonical_screen_mode(prev_raw);
    if screen_mode_raw_matches_canonical(prev_raw, canonical) {
        return vec![];
    }
    set_screen_mode_inner(app, canonical);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "screen_mode", value = canonical, "setting changed");
    app.show_toast(&format!(
        "\u{2713} Screen mode: {canonical} (restart to apply)"
    ));
    vec![Effect::PersistSetting {
        key: "screen_mode",
        value: crate::settings::SettingValue::Enum(canonical),
        rollback_value: crate::settings::SettingValue::Enum(prev),
    }]
}

fn screen_mode_raw_matches_canonical(raw: Option<&str>, canonical: &str) -> bool {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    match canonical {
        "minimal" => raw.eq_ignore_ascii_case("minimal"),
        "fullscreen" => raw.eq_ignore_ascii_case("fullscreen") || raw.eq_ignore_ascii_case("full"),
        _ => false,
    }
}

/// Set the hunk-tracker mode (registry-driven path).
///
/// SHELL-owned, restart-required: persists to `[ui].hunk_tracker_mode` via
/// `Effect::PersistSetting`. The agent re-reads the mode only on the next
/// session connect, so the toast cues a restart.
pub(in crate::app::dispatch) fn set_hunk_tracker_mode(
    app: &mut AppView,
    value: String,
) -> Vec<Effect> {
    let canonical = crate::settings::canonical_hunk_tracker_mode(Some(&value));
    let prev =
        crate::settings::canonical_hunk_tracker_mode(app.current_ui.hunk_tracker_mode.as_deref());
    if prev == canonical {
        return vec![];
    }
    set_hunk_tracker_mode_inner(app, canonical);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "hunk_tracker_mode", value = canonical, "setting changed");
    app.show_toast(&format!(
        "\u{2713} Hunk tracker: {canonical} (restart to apply)"
    ));
    vec![Effect::PersistSetting {
        key: "hunk_tracker_mode",
        value: crate::settings::SettingValue::Enum(canonical),
        rollback_value: crate::settings::SettingValue::Enum(prev),
    }]
}

/// Mirror the canonical voice-capture mode into `app.current_ui` so
/// `current_value_for` stays in sync. Called by the commit path AND by
/// [`apply_setting_rollback`](super::ui::apply_setting_rollback).
pub(super) fn set_voice_capture_mode_inner(app: &mut AppView, canonical: &str) {
    app.current_ui.voice_capture_mode = Some(canonical.to_string());
}

/// Set the voice-capture mode (`toggle` | `hold`). SHELL-owned; persists to
/// `[ui].voice_capture_mode` via `Effect::PersistSetting`. The event loop reads
/// the live value, so it takes effect on the next Ctrl+Space press (no restart).
pub(in crate::app::dispatch) fn set_voice_capture_mode(
    app: &mut AppView,
    value: String,
) -> Vec<Effect> {
    let canonical = crate::settings::canonical_voice_capture_mode(Some(&value));
    let prev =
        crate::settings::canonical_voice_capture_mode(app.current_ui.voice_capture_mode.as_deref());
    if prev == canonical {
        return vec![];
    }
    set_voice_capture_mode_inner(app, canonical);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "voice_capture_mode", value = canonical, "setting changed");
    app.show_toast(&format!("\u{2713} Voice capture: {canonical}"));
    vec![Effect::PersistSetting {
        key: "voice_capture_mode",
        value: crate::settings::SettingValue::Enum(canonical),
        rollback_value: crate::settings::SettingValue::Enum(prev),
    }]
}

/// Mirror the voice-shortcut gate into `app.current_ui` (read live by the
/// event-loop chord intercept) and the process-global mirror (read by key
/// routing / view code without an `AppView`). Called by the commit path AND by
/// [`apply_setting_rollback`](super::ui::apply_setting_rollback).
pub(super) fn set_voice_keybind_enabled_inner(app: &mut AppView, new: bool) {
    app.current_ui.voice_keybind_enabled = Some(new);
    crate::app::VOICE_KEYBIND_ENABLED.store(new, std::sync::atomic::Ordering::Release);
}

/// Enable/disable the Ctrl+Space / F8 voice shortcut. SHELL-owned; persists to
/// `[ui].voice_keybind_enabled` via `Effect::PersistSetting`. Applies on the
/// next keypress (no restart). Only the chord is gated — `/voice`, Esc while
/// listening, and the recording-row `[stop]` keep working.
pub(in crate::app::dispatch) fn set_voice_keybind_enabled(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev_state = app.current_ui.voice_keybind_enabled;
    let prev_effective = prev_state.unwrap_or(true);
    if prev_effective == new && prev_state.is_some() {
        return vec![];
    }
    set_voice_keybind_enabled_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "voice_keybind_enabled", value = new, "setting changed");
    app.show_toast(&save_success_toast("Voice shortcut", new));
    vec![Effect::PersistSetting {
        key: "voice_keybind_enabled",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev_effective),
    }]
}

/// Mirror the STT language preference into `app.current_ui` and
/// `app.voice_config.language` (may be the client-only `"auto"` sentinel; the
/// voice crate resolves it at connect time). Called by the commit path AND by
/// [`apply_setting_rollback`].
pub(super) fn set_voice_stt_language_inner(app: &mut AppView, canonical: &str) {
    // Whether the effective language actually changes. Re-pinning an unset or
    // non-canonical `[ui]` mirror to the language already in effect must not
    // recycle the pipeline (that would cut off active dictation for nothing).
    let language_changed =
        crate::settings::canonical_voice_stt_language(Some(&app.voice_config.language))
            != canonical;
    app.current_ui.voice_stt_language = Some(canonical.to_string());
    // Store the preference, not the resolved wire code — so `auto` re-resolves
    // from the locale on each STT connect.
    app.voice_config.language = canonical.to_string();
    // A running pipeline holds the VoiceConfig it was spawned with; shut it
    // down so the next capture cold-starts one with the new language (the
    // event loop respawns lazily whenever `voice_cmd_tx` is None). Tear down
    // any in-flight session first so the mic indicator clears immediately and
    // the pipeline's channel-close is not misreported as "pipeline ended".
    if language_changed && let Some(tx) = app.voice_cmd_tx.take() {
        app.voice_reset();
        let _ = tx.try_send(echo_agent_voice::VoiceCommand::Shutdown);
    }
}

/// Set the voice STT language. SHELL-owned; persists to `[ui].voice_stt_language`
/// via `Effect::PersistSetting`. Live-applied to `voice_config` so the next
/// capture picks it up (no restart).
pub(in crate::app::dispatch) fn set_voice_stt_language(
    app: &mut AppView,
    value: String,
) -> Vec<Effect> {
    let canonical = crate::settings::canonical_voice_stt_language(Some(&value));
    // `prev` is the live effective language so a failed persist rolls back to
    // what's actually in effect.
    let prev = crate::settings::canonical_voice_stt_language(Some(&app.voice_config.language));
    if prev == canonical && app.current_ui.voice_stt_language.as_deref() == Some(canonical) {
        return vec![];
    }
    set_voice_stt_language_inner(app, canonical);
    refresh_open_settings_modals(app);
    let effective = echo_agent_voice::language_for_api(canonical);
    tracing::info!(
        target: "settings",
        key = "voice_stt_language",
        value = canonical,
        effective,
        "setting changed"
    );
    let toast = if canonical == echo_agent_voice::STT_LANGUAGE_AUTO {
        format!("\u{2713} Voice language: System ({effective})")
    } else {
        let name = echo_agent_voice::stt_language_by_code(canonical).map_or(canonical, |l| l.name);
        format!("\u{2713} Voice language: {name}")
    };
    app.show_toast(&toast);
    vec![Effect::PersistSetting {
        key: "voice_stt_language",
        value: crate::settings::SettingValue::Enum(canonical),
        rollback_value: crate::settings::SettingValue::Enum(prev),
    }]
}

/// State-only mutation for `vim_mode`. Propagates to every in-process
/// agent so background subagents and side panes pick up the change
/// without restart. The cache mirror lets new agents created later
/// read the same value via `cache::load_vim_mode()` in `AgentView::new`.
pub(super) fn set_vim_mode_inner(app: &mut AppView, new: bool) {
    for agent in app.agents.values_mut() {
        // Recursive so open subagent views also pick up the change —
        // otherwise their scrollback j/k stay in the vim-OFF fallback.
        agent.set_vim_mode_recursive(new);
    }
    crate::appearance::cache::set_vim_mode(new);
}

/// Set vim-mode scrollback keybindings (registry-driven path).
///
/// SHELL-OWNED: persisted to `[ui].vim_mode` in config.toml via
/// `Effect::PersistSetting`. Propagates to every in-process agent.
pub(in crate::app::dispatch) fn set_vim_mode(app: &mut AppView, new: bool) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_vim_mode();
    if prev == new && app.agents.values().all(|a| a.vim_mode == new) {
        return vec![];
    }
    set_vim_mode_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "vim_mode",
        value = new,
        "setting changed",
    );
    app.show_toast(&save_success_toast("Vim scrollback", new));
    vec![Effect::PersistSetting {
        key: "vim_mode",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

/// Mirror the user-config layer in `current_ui` so the modal reflects it
/// (the effective gate is resolved shell-side at session spawn).
pub(super) fn set_remember_tool_approvals_inner(app: &mut AppView, new: bool) {
    app.current_ui.remember_tool_approvals = Some(new);
}

/// SHELL-owned setter for `remember_tool_approvals`; persists via
/// `Effect::PersistSetting`. Applies to new sessions (restart-required).
pub(in crate::app::dispatch) fn set_remember_tool_approvals(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev_state = app.current_ui.remember_tool_approvals;
    let prev_effective = prev_state.unwrap_or(false);
    if prev_effective == new && prev_state.is_some() {
        return vec![];
    }
    set_remember_tool_approvals_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "remember_tool_approvals",
        value = new,
        "setting changed",
    );
    app.show_toast(&format!(
        "{} (restart to apply)",
        save_success_toast("Remember tool approvals", new),
    ));
    vec![Effect::PersistSetting {
        key: "remember_tool_approvals",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev_effective),
    }]
}

/// Mirror the just-written TOML value in `app` so the modal reflects it (the
/// effective timeout is re-resolved shell-side at agent build).
pub(super) fn set_ask_user_question_timeout_enabled_inner(app: &mut AppView, new: bool) {
    app.ask_user_question_timeout_enabled = Some(new);
}

/// SHELL-owned setter for the ask_user_question timeout gate; persists via
/// `Effect::PersistSetting`. Applies to new sessions (restart-required).
pub(in crate::app::dispatch) fn set_ask_user_question_timeout_enabled(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question;
    let prev_state = app.ask_user_question_timeout_enabled;
    let prev_effective =
        prev_state.unwrap_or(ask_user_question::DEFAULT_ASK_USER_QUESTION_TIMEOUT_ENABLED);
    if prev_effective == new && prev_state.is_some() {
        return vec![];
    }
    set_ask_user_question_timeout_enabled_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "toolset.ask_user_question.timeout_enabled",
        value = new,
        "setting changed",
    );
    app.show_toast(&format!(
        "{} (restart to apply)",
        save_success_toast("Ask-Question timeout", new),
    ));
    vec![Effect::PersistSetting {
        key: "toolset.ask_user_question.timeout_enabled",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev_effective),
    }]
}

pub(super) fn set_show_thinking_blocks_inner(app: &mut AppView, new: bool) {
    crate::appearance::cache::set_show_thinking_blocks(new);
    // Thinking visibility reshapes verb-group runs (shown thoughts claim
    // into folds) AND dense N-more runs (hidden thoughts drop out of
    // truncation participation), so expansion ids describe the OLD grouping
    // shape even with `group_tool_verbs` off — drop them like
    // `set_group_tool_verbs_inner`.
    for agent in app.agents.values_mut() {
        agent.scrollback.clear_group_expansion();
        agent.scrollback.invalidate_heights();
        for child in agent.subagent_views.values_mut() {
            child.scrollback.clear_group_expansion();
            child.scrollback.invalidate_heights();
        }
    }
}

/// Set whether agent thinking blocks appear in scrollback.
///
/// SHELL-OWNED: cache mirror + `[ui].show_thinking_blocks` via `Effect::PersistSetting`.
/// Live hide/show: layout treats thinking as zero-height when off; create-gate
/// still skips new thinking while off.
pub(in crate::app::dispatch) fn set_show_thinking_blocks(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_show_thinking_blocks();
    if prev == new {
        return vec![];
    }
    set_show_thinking_blocks_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "show_thinking_blocks",
        value = new,
        "setting changed",
    );
    app.show_toast(&save_success_toast("Thinking blocks", new));
    vec![Effect::PersistSetting {
        key: "show_thinking_blocks",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

pub(super) fn set_group_tool_verbs_inner(app: &mut AppView, new: bool) {
    crate::appearance::cache::set_group_tool_verbs(new);
    // Expansion ids describe the OLD grouping shape; drop them so stale ids
    // can't reopen a verb slot expanded or mark a coincident dense group
    // expanded after the re-fold (see `clear_group_expansion`).
    for agent in app.agents.values_mut() {
        agent.scrollback.clear_group_expansion();
        agent.scrollback.invalidate_heights();
        for child in agent.subagent_views.values_mut() {
            child.scrollback.clear_group_expansion();
            child.scrollback.invalidate_heights();
        }
    }
}

/// Set whether runs of consecutive non-destructive tool calls and subagent
/// rows are grouped into one transcript row.
///
/// SHELL-OWNED: cache mirror + `[ui].group_tool_verbs` via `Effect::PersistSetting`.
pub(in crate::app::dispatch) fn set_group_tool_verbs(app: &mut AppView, new: bool) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_group_tool_verbs();
    if prev == new {
        return vec![];
    }
    set_group_tool_verbs_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "group_tool_verbs",
        value = new,
        "setting changed",
    );
    app.show_toast(&save_success_toast("Group tool calls", new));
    vec![Effect::PersistSetting {
        key: "group_tool_verbs",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

pub(super) fn set_collapsed_edit_blocks_inner(app: &mut AppView, new: bool) {
    // Read prev from the cache so a rollback restore walks the same flip.
    let prev = crate::appearance::cache::load_collapsed_edit_blocks();
    crate::appearance::cache::set_collapsed_edit_blocks(new);
    if prev == new {
        return;
    }
    // Re-materialize on-default Edit rows + repaint the live +N/-M suffix
    // (the flip policy lives on ScrollbackState).
    for agent in app.agents.values_mut() {
        agent.scrollback.apply_collapsed_edit_blocks_flip(prev, new);
        for child in agent.subagent_views.values_mut() {
            child.scrollback.apply_collapsed_edit_blocks_flip(prev, new);
        }
    }
}

/// Set whether Edit blocks default to the collapsed one-line `+N/-M`
/// diffstat summary (expand for the diff).
///
/// SHELL-OWNED: cache mirror + `[ui].collapsed_edit_blocks` via
/// `Effect::PersistSetting`. Explicit pager.toml
/// `[scrollback.blocks.edit]` shape keys override the flag.
pub(in crate::app::dispatch) fn set_collapsed_edit_blocks(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_collapsed_edit_blocks();
    if prev == new {
        return vec![];
    }
    set_collapsed_edit_blocks_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "collapsed_edit_blocks",
        value = new,
        "setting changed",
    );
    app.show_toast(&save_success_toast("Collapsed edit blocks", new));
    vec![Effect::PersistSetting {
        key: "collapsed_edit_blocks",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

pub(super) fn set_prompt_suggestions_inner(app: &mut AppView, new: bool) {
    crate::appearance::cache::set_prompt_suggestions(new);
    app.current_ui.prompt_suggestions = Some(new);
}

/// Set whether the predicted-next-prompt ghost (tab autocomplete) is offered.
///
/// SHELL-OWNED: cache mirror + `[ui].prompt_suggestions` via
/// `Effect::PersistSetting`. Read at turn end (fetch gate) and per frame
/// (display gate), so toggling applies without a restart. The
/// `ECHO_AGENT_PROMPT_SUGGESTIONS` env var overrides the effective value.
pub(in crate::app::dispatch) fn set_prompt_suggestions(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_prompt_suggestions();
    if prev == new {
        return vec![];
    }
    set_prompt_suggestions_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "prompt_suggestions",
        value = new,
        "setting changed",
    );
    app.show_toast(&save_success_toast("Prompt suggestions", new));
    vec![Effect::PersistSetting {
        key: "prompt_suggestions",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

pub(super) fn set_keep_text_selection_inner(kind: crate::appearance::TextSelection) {
    crate::appearance::cache::set_keep_text_selection(kind);
}

/// Set the unified scrollback text-selection mode (`flash` | `hold` |
/// `word_select`): highlight lifetime + double-click action, kept in sync.
///
/// SHELL-OWNED: persisted to `[ui].keep_text_selection` via `Effect::PersistSetting`.
pub(in crate::app::dispatch) fn set_keep_text_selection(
    app: &mut AppView,
    kind: crate::appearance::TextSelection,
) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_keep_text_selection();
    if prev == kind {
        return vec![];
    }
    set_keep_text_selection_inner(kind);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "keep_text_selection",
        value = kind.as_canonical(),
        "setting changed",
    );
    app.show_toast(&format!("\u{2713} Text selection: {}", kind.as_canonical()));
    vec![Effect::PersistSetting {
        key: "keep_text_selection",
        value: crate::settings::SettingValue::Enum(kind.as_canonical()),
        rollback_value: crate::settings::SettingValue::Enum(prev.as_canonical()),
    }]
}

/// State-only mutation for `scroll_speed`. Updates the process-wide
/// cache and recomputes `app.scroll_config`.
pub(super) fn set_scroll_speed_inner(app: &mut AppView, clamped: u8) {
    crate::appearance::cache::set_scroll_speed(clamped);
    // Full settings-cache rebuild: preserves the other scroll overrides
    // (mode/invert/lines) instead of resetting them to profile defaults.
    app.scroll_config = crate::input::mouse::ScrollConfig::from_settings();
}

/// Set mouse-wheel scroll speed (registry-driven path).
///
/// SHELL-OWNED: persisted to `[ui].scroll_speed` in config.toml via
/// `Effect::PersistSetting`. Clamps to `[1, 100]` to match the
/// registry's `Int { min: 1, max: 100 }` bounds.
pub(in crate::app::dispatch) fn set_scroll_speed(app: &mut AppView, raw: i64) -> Vec<Effect> {
    let clamped = raw.clamp(1, 100) as u8;
    let prev = crate::appearance::cache::load_scroll_speed();
    if prev == clamped {
        return vec![];
    }
    set_scroll_speed_inner(app, clamped);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "scroll_speed",
        value = clamped,
        "setting changed",
    );
    app.show_toast(&format!("\u{2713} Scroll speed: {clamped}"));
    vec![Effect::PersistSetting {
        key: "scroll_speed",
        value: crate::settings::SettingValue::Int(clamped as i64),
        rollback_value: crate::settings::SettingValue::Int(prev as i64),
    }]
}

/// State-only mutation for `scroll_mode`. Updates the process-wide cache
/// and recomputes `app.scroll_config`.
pub(super) fn set_scroll_mode_inner(app: &mut AppView, mode: crate::appearance::ScrollMode) {
    crate::appearance::cache::set_scroll_mode(mode);
    app.scroll_config = crate::input::mouse::ScrollConfig::from_settings();
}

/// Set the scroll input classification (`auto` | `wheel` | `trackpad`).
///
/// SHELL-OWNED: persisted to `[ui].scroll_mode` via `Effect::PersistSetting`.
pub(in crate::app::dispatch) fn set_scroll_mode(
    app: &mut AppView,
    mode: crate::appearance::ScrollMode,
) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_scroll_mode();
    if prev == mode {
        return vec![];
    }
    set_scroll_mode_inner(app, mode);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "scroll_mode",
        value = mode.as_canonical(),
        "setting changed",
    );
    app.show_toast(&format!("\u{2713} Scroll input: {}", mode.as_canonical()));
    vec![Effect::PersistSetting {
        key: "scroll_mode",
        value: crate::settings::SettingValue::Enum(mode.as_canonical()),
        rollback_value: crate::settings::SettingValue::Enum(prev.as_canonical()),
    }]
}

/// State-only mutation for `invert_scroll`. Updates the process-wide cache
/// and recomputes `app.scroll_config`.
pub(super) fn set_invert_scroll_inner(app: &mut AppView, enabled: bool) {
    crate::appearance::cache::set_invert_scroll(enabled);
    app.scroll_config = crate::input::mouse::ScrollConfig::from_settings();
}

/// Set whether vertical scroll direction is inverted ("natural" scrolling).
///
/// SHELL-OWNED: persisted to `[ui].invert_scroll` via `Effect::PersistSetting`.
pub(in crate::app::dispatch) fn set_invert_scroll(app: &mut AppView, new: bool) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_invert_scroll();
    if prev == new {
        return vec![];
    }
    set_invert_scroll_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "invert_scroll",
        value = new,
        "setting changed",
    );
    app.show_toast(&save_success_toast("Invert scroll", new));
    vec![Effect::PersistSetting {
        key: "invert_scroll",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

/// State-only mutation for `scroll_lines`. Updates the process-wide cache
/// and recomputes `app.scroll_config`.
pub(super) fn set_scroll_lines_inner(app: &mut AppView, clamped: u8) {
    crate::appearance::cache::set_scroll_lines(clamped);
    app.scroll_config = crate::input::mouse::ScrollConfig::from_settings();
}

/// Set the lines-per-tick for both wheel and trackpad scrolling.
///
/// SHELL-OWNED: persisted to `[ui].scroll_lines` via `Effect::PersistSetting`.
/// Clamps to `[1, 10]` to match the registry's `Int { min: 1, max: 10 }`
/// bounds.
pub(in crate::app::dispatch) fn set_scroll_lines(app: &mut AppView, raw: i64) -> Vec<Effect> {
    let clamped = raw.clamp(1, 10) as u8;
    // `None` (never configured → profile default) counts as changed: an
    // explicit user value must stick even when it matches the profile.
    let prev = crate::appearance::cache::load_scroll_lines();
    if prev == Some(clamped) {
        return vec![];
    }
    set_scroll_lines_inner(app, clamped);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "scroll_lines",
        value = clamped,
        "setting changed",
    );
    app.show_toast(&format!("\u{2713} Scroll lines: {clamped}"));
    vec![Effect::PersistSetting {
        key: "scroll_lines",
        value: crate::settings::SettingValue::Int(clamped as i64),
        // prev=None (never configured) can't round-trip through
        // SettingValue::Int — rolling back to an explicit 3 is the same
        // deliberate one-way door as the `d`-reset path.
        rollback_value: crate::settings::SettingValue::Int(prev.unwrap_or(3) as i64),
    }]
}

pub(super) fn set_respect_manual_folds_inner(app: &mut AppView, new: bool) {
    let mut config = app.appearance.clone();
    config.scrollback.scroll.respect_manual_folds = new;
    app.set_appearance(config);
}

/// Set `respect_manual_folds` (registry-driven path).
///
/// PAGER-OWNED: live-applied to every agent via `AppView::set_appearance`
/// and persisted to `[scrollback.scroll]` in pager.toml via
/// `Effect::PersistSetting`.
pub(in crate::app::dispatch) fn set_respect_manual_folds(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = app.appearance.scrollback.scroll.respect_manual_folds;
    if prev == new {
        return vec![];
    }
    set_respect_manual_folds_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "respect_manual_folds",
        value = new,
        "setting changed",
    );
    app.show_toast(&save_success_toast("Respect manual folds", new));
    vec![Effect::PersistSetting {
        key: "respect_manual_folds",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

/// Set the cursor preselection canonical (registry-driven path).
///
/// SHELL-OWNED: persisted to `[ui].default_selected_permission` via
/// `Effect::PersistSetting`. The `always_allow_all_sessions` canonical is the
/// effective default (the cursor falls to the enable-always-approve row).
pub(in crate::app::dispatch) fn set_default_selected_permission(
    app: &mut AppView,
    new: String,
) -> Vec<Effect> {
    use crate::appearance::permission_cursor::DefaultSelectedPermission;
    // All callers pass a registry canonical; parsing is total (unknown →
    // `Default`), so a garbage input degrades to the safe "no preselection"
    // value. `debug_assert` catches a dispatch bug in tests without a parallel
    // validator on the hot path.
    let parsed = DefaultSelectedPermission::from_config_value(&new);
    debug_assert_eq!(
        parsed.as_canonical(),
        new.as_str(),
        "SetDefaultSelectedPermission expects a registry canonical",
    );
    let new_canonical = parsed.as_canonical();
    let prev_canonical = app
        .current_ui
        .default_selected_permission
        .as_deref()
        .map_or(DefaultSelectedPermission::AlwaysAllowAllSessions, |s| {
            DefaultSelectedPermission::from_config_value(s)
        })
        .as_canonical();
    if prev_canonical == new_canonical {
        return vec![];
    }
    set_default_selected_permission_inner(app, parsed);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "default_selected_permission",
        value = new_canonical,
        "setting changed",
    );
    app.show_toast(&format!(
        "\u{2713} Default selected permission: {}",
        parsed.display(),
    ));
    vec![Effect::PersistSetting {
        key: "default_selected_permission",
        value: crate::settings::SettingValue::Enum(new_canonical),
        rollback_value: crate::settings::SettingValue::Enum(prev_canonical),
    }]
}

/// State + cache mutation for `default_selected_permission`. Called by the
/// commit path AND by [`apply_setting_rollback`](super::ui::apply_setting_rollback) on persist failure.
pub(super) fn set_default_selected_permission_inner(
    app: &mut AppView,
    value: crate::appearance::permission_cursor::DefaultSelectedPermission,
) {
    // Mirror to in-memory `UiConfig` so `current_value_for` stays in sync
    // with the cache for the open settings modal.
    app.current_ui.default_selected_permission = Some(value.as_canonical().to_string());
    // Update the process-wide cache so the next permission prompt picks up
    // the new value without a restart.
    crate::appearance::permission_cursor::set_default_selected_permission(value);
}

// ---------------------------------------------------------------------------
// Settings setters — unified dispatch for the settings modal and slash
// commands.
//
// SHARED setters use inner/outer split:
//   - `set_X_inner`: state-only mutation. Called on success AND rollback.
//   - `set_X`: calls inner, refreshes modals, toasts, emits PersistSetting.
//   - Rollback (`apply_setting_rollback`) calls inner only — no re-emit.
//
// PAGER setters (e.g. `set_multiline_mode`) have no persist/rollback,
// so they skip the split.
// ---------------------------------------------------------------------------

/// State-only mutation for `compact_mode`. Updates the in-memory
/// `current_ui` snapshot (read by the modal) and the thread-local cache
/// used by hot reads with the USER value, then re-derives the render
/// value (auto-compact on short terminals) into the appearance snapshot
/// and the agents' prompt widgets. Never touches disk; never emits effects.
pub(super) fn set_compact_mode_inner(app: &mut AppView, new: bool) {
    app.current_ui.compact_mode = new;
    crate::appearance::cache::set(new);
    app.apply_effective_compact();
}

/// Set compact mode. Idempotent: skips if `new == prev`.
pub(in crate::app::dispatch) fn set_compact_mode(app: &mut AppView, new: bool) -> Vec<Effect> {
    let prev = app.current_ui.compact_mode;
    if prev == new {
        return vec![];
    }
    set_compact_mode_inner(app, new);
    // Refresh modal snapshot so the indicator reflects the new value.
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "compact_mode", value = new, "setting changed");
    // Turning the setting off while the short-terminal derivation holds keeps
    // the UI compact; say so instead of implying the layout will loosen.
    if !new && crate::views::agent::effective_compact(false, app.last_known_terminal_rows) {
        app.show_toast("\u{2713} Compact mode: off (auto-compact active on small terminal)");
    } else {
        app.show_toast(&save_success_toast("Compact mode", new));
    }
    vec![Effect::PersistSetting {
        key: "compact_mode",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

/// State-only mutation for `show_timestamps`. Idempotent fast path
/// mirrors `set_compact_mode_inner`.
pub(super) fn set_timestamps_inner(app: &mut AppView, new: bool) {
    app.current_ui.show_timestamps = Some(new);
    if app.appearance.show_timestamps == new {
        crate::appearance::cache::set_timestamps(new);
        return;
    }
    let mut config = app.appearance.clone();
    config.show_timestamps = new;
    app.set_appearance(config);
    crate::appearance::cache::set_timestamps(new);
}

pub(in crate::app::dispatch) fn set_timestamps(app: &mut AppView, new: bool) -> Vec<Effect> {
    let prev = app.current_ui.show_timestamps.unwrap_or(true);
    // Idempotency gate.
    if prev == new {
        return vec![];
    }
    set_timestamps_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "show_timestamps", value = new, "setting changed");
    app.show_toast(&save_success_toast("Timestamps", new));
    vec![Effect::PersistSetting {
        key: "show_timestamps",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

/// State-only mutation for `show_timeline`. Mirrors `set_timestamps_inner`.
pub(super) fn set_timeline_inner(app: &mut AppView, new: bool) {
    app.current_ui.show_timeline = Some(new);
    if app.appearance.show_timeline == new {
        crate::appearance::cache::set_show_timeline(new);
        return;
    }
    let mut config = app.appearance.clone();
    config.show_timeline = new;
    app.set_appearance(config);
    crate::appearance::cache::set_show_timeline(new);
}

pub(in crate::app::dispatch) fn set_timeline(app: &mut AppView, new: bool) -> Vec<Effect> {
    // Gate on the displayed state (`appearance.show_timeline`, what the rail
    // renders from and what `/timeline` toggles against) — not the separately
    // hydrated `current_ui`, which could disagree and make the toggle no-op.
    let prev = app.appearance.show_timeline;
    // Idempotency gate.
    if prev == new {
        return vec![];
    }
    set_timeline_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "show_timeline", value = new, "setting changed");
    app.show_toast(&save_success_toast("Timeline sidebar", new));
    vec![Effect::PersistSetting {
        key: "show_timeline",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

pub(super) fn set_page_flip_on_send_inner(app: &mut AppView, new: bool) {
    app.current_ui.page_flip_on_send = Some(new);
    crate::appearance::cache::set_page_flip_on_send(new);
}

/// SHARED: cache + `[ui].page_flip_on_send` via `Effect::PersistSetting`.
pub(in crate::app::dispatch) fn set_page_flip_on_send(app: &mut AppView, new: bool) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_page_flip_on_send();
    if prev == new {
        return vec![];
    }
    set_page_flip_on_send_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "page_flip_on_send", value = new, "setting changed");
    app.show_toast(&save_success_toast("Snap prompt to top on send", new));
    vec![Effect::PersistSetting {
        key: "page_flip_on_send",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

pub(in crate::app::dispatch) fn set_confirm_before_rewind_inner(app: &mut AppView, new: bool) {
    app.current_ui.confirm_before_rewind = Some(new);
}

/// SHARED: `[ui].confirm_before_rewind` via `Effect::PersistSetting`.
pub(in crate::app::dispatch) fn set_confirm_before_rewind(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = app.current_ui.confirm_before_rewind_enabled();
    if prev == new {
        return vec![];
    }
    set_confirm_before_rewind_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "confirm_before_rewind", value = new, "setting changed");
    app.show_toast(&save_success_toast("Confirm before rewind", new));
    vec![Effect::PersistSetting {
        key: "confirm_before_rewind",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

pub(super) fn set_combine_queued_prompts_inner(app: &mut AppView, new: bool) {
    app.current_ui.combine_queued_prompts = Some(new);
    crate::appearance::cache::set_combine_queued_prompts(new);
}

/// SHARED: cache + `[ui].combine_queued_prompts` via `Effect::PersistSetting`.
pub(in crate::app::dispatch) fn set_combine_queued_prompts(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_combine_queued_prompts();
    if prev == new {
        return vec![];
    }
    set_combine_queued_prompts_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "combine_queued_prompts", value = new, "setting changed");
    app.show_toast(&save_success_toast("Combine queued prompts", new));
    vec![Effect::PersistSetting {
        key: "combine_queued_prompts",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

pub(super) fn set_follow_up_behavior_inner(
    app: &mut AppView,
    new: crate::appearance::FollowUpBehavior,
) {
    app.current_ui.follow_up_behavior = Some(new.as_canonical().to_string());
    crate::appearance::cache::set_follow_up_behavior(new);
    // Same-process atomic only (tests / in-proc shell). The real agent is a
    // separate process; it re-resolves Steer from config.toml mtime after the
    // PersistSetting disk write lands.
    echo_agent_runtime::util::config::set_follow_up_steer_cache(new.is_steer());
}

pub(in crate::app::dispatch) fn set_follow_up_behavior(
    app: &mut AppView,
    new: crate::appearance::FollowUpBehavior,
) -> Vec<Effect> {
    let prev = crate::appearance::cache::load_follow_up_behavior();
    if prev == new {
        return vec![];
    }
    set_follow_up_behavior_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "follow_up_behavior",
        value = new.as_canonical(),
        "setting changed",
    );
    let label = match new {
        crate::appearance::FollowUpBehavior::Queue => "Queue",
        crate::appearance::FollowUpBehavior::Steer => "Steer",
    };
    app.show_toast(&format!("\u{2713} Follow-up behavior: {label}"));
    vec![Effect::PersistSetting {
        key: "follow_up_behavior",
        value: crate::settings::SettingValue::Enum(new.as_canonical()),
        rollback_value: crate::settings::SettingValue::Enum(prev.as_canonical()),
    }]
}

/// State-only mutation for `simple_mode`.
///
/// Propagates to every agent's `input_mode` so the toggle takes
/// effect immediately (not just on new agents).
pub(super) fn set_simple_mode_inner(app: &mut AppView, new: bool) {
    app.current_ui.simple_mode = Some(new);
    crate::appearance::cache::set_simple_mode(new);

    // Reconcile every agent's `input_mode` — global toggle.
    let target_mode = if new {
        crate::views::agent::InputMode::Simple
    } else {
        crate::views::agent::InputMode::Vim
    };
    for agent in app.agents.values_mut() {
        if agent.input_mode != target_mode {
            agent.set_input_mode(target_mode);
        }
    }
}

pub(in crate::app::dispatch) fn set_simple_mode(app: &mut AppView, new: bool) -> Vec<Effect> {
    let prev = app.current_ui.simple_mode.unwrap_or(true);
    // No idempotency gate — fans out to every agent's `input_mode`.
    // A newly-inserted agent may have a stale default, so we always
    // propagate. The inner is per-agent idempotent.
    set_simple_mode_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "simple_mode", value = new, "setting changed");
    // Toast label mirrors the renamed registry label
    // ("Disable vim input mode") so the user sees the same name in the
    // modal and the toast.
    app.show_toast(&save_success_toast("Disable vim input mode", new));
    vec![Effect::PersistSetting {
        key: "simple_mode",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev),
    }]
}

// ---------------------------------------------------------------------------
// Contextual-hint tips: the `contextual_hints.*` per-tip toggles.
//
// SHELL-owned: persisted to `[ui.contextual_hints]`. Each setter writes the
// user-config Option, then re-resolves ALL tips (env master + remote) and
// re-propagates the prompt gates to every agent, so a toggle takes effect at
// runtime (not just on next launch). `write` is a non-capturing closure that
// coerces to `fn` so the tips share one inner.
// ---------------------------------------------------------------------------

/// State-only mutation: write one tip's user-config Option, then re-resolve and
/// fan the resolved gates out to `app` + every agent prompt.
pub(super) fn set_contextual_hint_inner(
    app: &mut AppView,
    write: fn(&mut echo_agent_runtime::agent::config::ContextualHints, Option<bool>),
    new: bool,
) {
    write(&mut app.current_ui.contextual_hints, Some(new));
    let resolved = echo_agent_runtime::util::config::resolve_contextual_hints(
        &app.current_ui.contextual_hints,
        app.remote_contextual_hints.as_ref(),
    );
    app.apply_contextual_hints(resolved);
}

/// Shared outer for the per-tip toggles: idempotency gate, state mutation,
/// modal refresh, toast, and `Effect::PersistSetting`.
fn set_contextual_hint(
    app: &mut AppView,
    key: crate::settings::SettingKey,
    label: &str,
    prev: Option<bool>,
    write: fn(&mut echo_agent_runtime::agent::config::ContextualHints, Option<bool>),
    new: bool,
) -> Vec<Effect> {
    // Skip when the stored user-explicit value already matches (no-op toggle).
    if prev == Some(new) {
        return vec![];
    }
    // `None` (inherit) collapses to the default ON for rollback — same lossy
    // shape the other `Option<bool>` settings use.
    let rollback = prev.unwrap_or(true);
    set_contextual_hint_inner(app, write, new);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key, value = new, "setting changed");
    app.show_toast(&save_success_toast(label, new));
    vec![Effect::PersistSetting {
        key,
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(rollback),
    }]
}

pub(in crate::app::dispatch) fn set_contextual_hint_undo(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = app.current_ui.contextual_hints.undo;
    set_contextual_hint(
        app,
        "contextual_hints.undo",
        "Undo hint",
        prev,
        |h, v| h.undo = v,
        new,
    )
}

pub(in crate::app::dispatch) fn set_contextual_hint_plan_mode(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = app.current_ui.contextual_hints.plan_mode;
    set_contextual_hint(
        app,
        "contextual_hints.plan_mode",
        "Plan mode hint",
        prev,
        |h, v| h.plan_mode = v,
        new,
    )
}

pub(in crate::app::dispatch) fn set_contextual_hint_image_input(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = app.current_ui.contextual_hints.image_input;
    set_contextual_hint(
        app,
        "contextual_hints.image_input",
        "Image input hint",
        prev,
        |h, v| h.image_input = v,
        new,
    )
}

pub(in crate::app::dispatch) fn set_contextual_hint_send_now(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = app.current_ui.contextual_hints.send_now;
    set_contextual_hint(
        app,
        "contextual_hints.send_now",
        "Send now hint",
        prev,
        |h, v| h.send_now = v,
        new,
    )
}

pub(in crate::app::dispatch) fn set_contextual_hint_small_screen(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = app.current_ui.contextual_hints.small_screen;
    set_contextual_hint(
        app,
        "contextual_hints.small_screen",
        "Small screen hint",
        prev,
        |h, v| h.small_screen = v,
        new,
    )
}

pub(in crate::app::dispatch) fn set_contextual_hint_word_select(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = app.current_ui.contextual_hints.word_select;
    set_contextual_hint(
        app,
        "contextual_hints.word_select",
        "Word select hint",
        prev,
        |h, v| h.word_select = v,
        new,
    )
}

pub(in crate::app::dispatch) fn set_contextual_hint_ssh_wrap(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev = app.current_ui.contextual_hints.ssh_wrap;
    set_contextual_hint(
        app,
        "contextual_hints.ssh_wrap",
        "SSH wrap hint",
        prev,
        |h, v| h.ssh_wrap = v,
        new,
    )
}

// ---------------------------------------------------------------------------
// Theme settings: `theme`, `auto_dark_theme`, `auto_light_theme`.
//
// Each has a preview/commit split:
//   - `SetX` (commit): state + visual + persist + toast.
//   - `PreviewX` (preview): visual only, no persist/toast.
//
// Auto-theme setters apply visually only when `theme="auto"` AND the
// system is in the matching mode; otherwise the value is just stored.
//
// Unknown names: `error!` in outer (registry skew), `warn!` in inner
// (rollback of corrupted config).
// ---------------------------------------------------------------------------

/// Format a "✓ <Label>: <value>" toast for theme-family settings.
/// `value` is the user-friendly display name, not the canonical.
fn save_theme_toast(label: &str, value: &str) -> String {
    format!("\u{2713} {label}: {value}")
}

/// Apply a (non-auto) theme to the live display.
///
/// Centralised so `set_theme_inner` and `preview_theme_inner` share the
/// same visual-mutation path. Resolves `Auto` via `theme::cache::resolve_auto`
/// (does NOT toggle `AUTO_MODE`); concrete kinds go through
/// `Theme::apply_kind` directly.
fn apply_theme_kind_for_display(kind: crate::theme::ThemeKind) {
    if kind.is_auto() {
        let resolved = crate::theme::cache::resolve_auto();
        crate::theme::Theme::apply_kind(resolved);
    } else {
        crate::theme::Theme::apply_kind(kind);
    }
}

/// Whether the system is in the appearance mode matching the given
/// auto-* key. Gates visual application of dormant auto-theme values.
fn system_is_in_matching_mode(key: &str) -> bool {
    use crate::theme::system_appearance;
    let Some(appearance) = system_appearance::detect() else {
        // Detection failure: fail closed (don't apply) — matches the
        // `resolve_auto` fallback behaviour.
        return false;
    };
    matches!(
        (key, appearance),
        ("auto_dark_theme", system_appearance::SystemAppearance::Dark)
            | (
                "auto_light_theme",
                system_appearance::SystemAppearance::Light
            )
    )
}

/// Whether the given auto-* setting is currently "live" — `theme="auto"`
/// AND system is in the matching mode.
fn auto_theme_setting_is_live(key: &str) -> bool {
    crate::theme::cache::is_auto_mode() && system_is_in_matching_mode(key)
}

// ── theme (commit path) ─────────────────────────────────────────────

/// State + cache + visual mutation for `theme`. **Commit path.**
/// Updates `app.current_ui.theme`, toggles `AUTO_MODE` based on
/// whether the value is `"auto"`, and applies the live theme.
///
/// Also called from `apply_setting_rollback` — when a disk persist
/// fails, we replay the inner with the prior value so both the
/// snapshot and the visual revert. Unknown / unrecognised names log
/// at `warn` (defensive — a malformed `rollback_value` is a softer
/// failure mode than an unknown commit-time value) and no-op.
pub(super) fn set_theme_inner(app: &mut AppView, value: &str) {
    let Some(kind) = crate::theme::ThemeKind::from_name(value) else {
        tracing::warn!(
            target: "settings",
            key = "theme",
            value = value,
            "unknown theme name — set_theme_inner no-op",
        );
        return;
    };
    let canonical = kind.display_name();
    app.current_ui.theme = Some(canonical.to_string());
    crate::theme::cache::set_auto_mode(kind.is_auto());
    apply_theme_kind_for_display(kind);
}

/// State + cache + persist for `theme` commits.
pub(in crate::app::dispatch) fn set_theme(app: &mut AppView, new: String) -> Vec<Effect> {
    let prev_canonical: &'static str = app
        .current_ui
        .theme
        .as_deref()
        .and_then(crate::theme::canonical_name)
        .unwrap_or_else(|| crate::theme::cache::current_kind().display_name());
    let new_canonical = match crate::theme::canonical_name(&new) {
        Some(c) => c,
        None => {
            tracing::error!(
                target: "settings",
                key = "theme",
                value = %new,
                "Action::SetTheme dispatched with unknown name — no-op",
            );
            return vec![];
        }
    };
    set_theme_inner(app, &new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "theme",
        value = %new_canonical,
        "setting changed",
    );
    app.show_toast(&save_theme_toast(
        "Theme",
        crate::theme::display_name_for_canonical(new_canonical),
    ));
    vec![Effect::PersistSetting {
        key: "theme",
        value: crate::settings::SettingValue::Enum(new_canonical),
        rollback_value: crate::settings::SettingValue::Enum(prev_canonical),
    }]
}

// ── theme (preview path) ────────────────────────────────────────────

/// Preview-only mutation for `theme`. Applies the live visual without
/// modifying state, toggling `AUTO_MODE`, persisting, or toasting.
/// For `"auto"`, resolves and applies the theme but does NOT toggle
/// `AUTO_MODE` (commit-only side effect).
fn preview_theme_inner(value: &str) {
    let Some(kind) = crate::theme::ThemeKind::from_name(value) else {
        tracing::warn!(
            target: "settings",
            key = "theme",
            value = value,
            "unknown theme name — preview_theme_inner no-op",
        );
        return;
    };
    apply_theme_kind_for_display(kind);
}

pub(in crate::app::dispatch) fn preview_theme(_app: &mut AppView, new: String) -> Vec<Effect> {
    if crate::theme::canonical_name(&new).is_none() {
        tracing::error!(
            target: "settings",
            key = "theme",
            value = %new,
            "Action::PreviewTheme dispatched with unknown name — no-op",
        );
        return vec![];
    }
    preview_theme_inner(&new);
    vec![]
}

// ── auto_dark_theme (commit path) ───────────────────────────────────

/// State + cache + visual mutation for `auto_dark_theme`. Commit path.
/// Applies visually only when the setting is live (auto mode + dark).
/// Rejects `"auto"` as an invalid value (log + no-op).
pub(super) fn set_auto_dark_theme_inner(app: &mut AppView, value: &str) {
    let Some(kind) = crate::theme::ThemeKind::from_name(value) else {
        tracing::warn!(
            target: "settings",
            key = "auto_dark_theme",
            value = value,
            "unknown theme name — set_auto_dark_theme_inner no-op",
        );
        return;
    };
    if kind.is_auto() {
        tracing::warn!(
            target: "settings",
            key = "auto_dark_theme",
            "Auto is not a valid auto_dark_theme value — no-op",
        );
        return;
    }
    let canonical = kind.display_name();
    app.current_ui.auto_dark_theme = Some(canonical.to_string());
    crate::theme::cache::invalidate_auto_theme_config();
    if auto_theme_setting_is_live("auto_dark_theme") {
        crate::theme::Theme::apply_kind(kind);
    }
}

pub(in crate::app::dispatch) fn set_auto_dark_theme(app: &mut AppView, new: String) -> Vec<Effect> {
    let prev_canonical: &'static str = app
        .current_ui
        .auto_dark_theme
        .as_deref()
        .and_then(crate::theme::canonical_name)
        .filter(|s| *s != "auto")
        // No prior config: fall back to EchoAgentNight (the default).
        .unwrap_or_else(|| crate::theme::ThemeKind::EchoAgentNight.display_name());
    let new_canonical = match crate::theme::canonical_name(&new) {
        Some(c) if c != crate::theme::ThemeKind::Auto.display_name() => c,
        _ => {
            tracing::error!(
                target: "settings",
                key = "auto_dark_theme",
                value = %new,
                "Action::SetAutoDarkTheme dispatched with invalid name — no-op",
            );
            return vec![];
        }
    };
    set_auto_dark_theme_inner(app, &new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "auto_dark_theme",
        value = %new_canonical,
        "setting changed",
    );
    app.show_toast(&save_theme_toast(
        "Auto dark theme",
        crate::theme::display_name_for_canonical(new_canonical),
    ));
    vec![Effect::PersistSetting {
        key: "auto_dark_theme",
        value: crate::settings::SettingValue::Enum(new_canonical),
        rollback_value: crate::settings::SettingValue::Enum(prev_canonical),
    }]
}

// ── auto_dark_theme (preview path) ──────────────────────────────────

/// Preview-only mutation for `auto_dark_theme`. Visual only when live.
fn preview_auto_dark_theme_inner(value: &str) {
    let Some(kind) = crate::theme::ThemeKind::from_name(value) else {
        tracing::warn!(
            target: "settings",
            key = "auto_dark_theme",
            value = value,
            "unknown theme name — preview_auto_dark_theme_inner no-op",
        );
        return;
    };
    if kind.is_auto() {
        return;
    }
    if auto_theme_setting_is_live("auto_dark_theme") {
        crate::theme::Theme::apply_kind(kind);
    }
}

pub(in crate::app::dispatch) fn preview_auto_dark_theme(
    _app: &mut AppView,
    new: String,
) -> Vec<Effect> {
    match crate::theme::canonical_name(&new) {
        Some(c) if c != crate::theme::ThemeKind::Auto.display_name() => {}
        _ => {
            tracing::error!(
                target: "settings",
                key = "auto_dark_theme",
                value = %new,
                "Action::PreviewAutoDarkTheme dispatched with invalid name — no-op",
            );
            return vec![];
        }
    };
    preview_auto_dark_theme_inner(&new);
    vec![]
}

// ── auto_light_theme (commit path) ──────────────────────────────────

/// State + cache + visual mutation for `auto_light_theme`. Commit path.
/// Mirror of `set_auto_dark_theme_inner` for the light bucket.
pub(super) fn set_auto_light_theme_inner(app: &mut AppView, value: &str) {
    let Some(kind) = crate::theme::ThemeKind::from_name(value) else {
        tracing::warn!(
            target: "settings",
            key = "auto_light_theme",
            value = value,
            "unknown theme name — set_auto_light_theme_inner no-op",
        );
        return;
    };
    if kind.is_auto() {
        tracing::warn!(
            target: "settings",
            key = "auto_light_theme",
            "Auto is not a valid auto_light_theme value — no-op",
        );
        return;
    }
    let canonical = kind.display_name();
    app.current_ui.auto_light_theme = Some(canonical.to_string());
    crate::theme::cache::invalidate_auto_theme_config();
    if auto_theme_setting_is_live("auto_light_theme") {
        crate::theme::Theme::apply_kind(kind);
    }
}

pub(in crate::app::dispatch) fn set_auto_light_theme(
    app: &mut AppView,
    new: String,
) -> Vec<Effect> {
    let prev_canonical: &'static str = app
        .current_ui
        .auto_light_theme
        .as_deref()
        .and_then(crate::theme::canonical_name)
        .filter(|s| *s != "auto")
        .unwrap_or_else(|| crate::theme::ThemeKind::EchoAgentDay.display_name());
    let new_canonical = match crate::theme::canonical_name(&new) {
        Some(c) if c != crate::theme::ThemeKind::Auto.display_name() => c,
        _ => {
            tracing::error!(
                target: "settings",
                key = "auto_light_theme",
                value = %new,
                "Action::SetAutoLightTheme dispatched with invalid name — no-op",
            );
            return vec![];
        }
    };
    set_auto_light_theme_inner(app, &new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "auto_light_theme",
        value = %new_canonical,
        "setting changed",
    );
    app.show_toast(&save_theme_toast(
        "Auto light theme",
        crate::theme::display_name_for_canonical(new_canonical),
    ));
    vec![Effect::PersistSetting {
        key: "auto_light_theme",
        value: crate::settings::SettingValue::Enum(new_canonical),
        rollback_value: crate::settings::SettingValue::Enum(prev_canonical),
    }]
}

// ── auto_light_theme (preview path) ─────────────────────────────────

/// Preview-only mutation for `auto_light_theme`. Mirror of
/// `preview_auto_dark_theme_inner` for the light bucket.
fn preview_auto_light_theme_inner(value: &str) {
    let Some(kind) = crate::theme::ThemeKind::from_name(value) else {
        tracing::warn!(
            target: "settings",
            key = "auto_light_theme",
            value = value,
            "unknown theme name — preview_auto_light_theme_inner no-op",
        );
        return;
    };
    if kind.is_auto() {
        return;
    }
    if auto_theme_setting_is_live("auto_light_theme") {
        crate::theme::Theme::apply_kind(kind);
    }
}

pub(in crate::app::dispatch) fn preview_auto_light_theme(
    _app: &mut AppView,
    new: String,
) -> Vec<Effect> {
    match crate::theme::canonical_name(&new) {
        Some(c) if c != crate::theme::ThemeKind::Auto.display_name() => {}
        _ => {
            tracing::error!(
                target: "settings",
                key = "auto_light_theme",
                value = %new,
                "Action::PreviewAutoLightTheme dispatched with invalid name — no-op",
            );
            return vec![];
        }
    };
    preview_auto_light_theme_inner(&new);
    vec![]
}

// ---------------------------------------------------------------------------
// default_model — resolves display name to `ModelId`, then emits both
// `Effect::SwitchModel` (active session) and `Effect::PersistSetting`
// (next-session default). No live preview (model switch has ACP side
// effects).
// ---------------------------------------------------------------------------

/// State-only mutation for `default_model`: set
/// `agent.session.models.current` to the supplied id. Returns `true`
/// if the catalog contains `id`; `false` otherwise.
pub(in crate::app::dispatch) fn set_default_model_inner(
    app: &mut AppView,
    id: &acp::ModelId,
) -> bool {
    let ActiveView::Agent(aid) = app.active_view else {
        return false;
    };
    {
        let Some(agent) = app.agents.get_mut(&aid) else {
            return false;
        };
        if !agent.session.models.available.contains_key(id) {
            return false;
        }
        // Update the agent's session model state's current pointer so
        // subsequent reads (e.g. `current_model_name` via the pager
        // snapshot) reflect the new selection without waiting for the
        // ACP roundtrip.
        //
        // `set_current(_, None)` resets `reasoning_effort` to model default.
        agent.session.models.set_current(id.clone(), None);
    }
    // Mirror the new default into the app-level model state too. A later `/new`
    // or `/clear` creates a fresh session by cloning `app.models`
    // (`dispatch_new_session_inner_with_id`), so without this the new session —
    // and the welcome card it commits — would show the previous default until
    // the next `echo.agent/models/update` roundtrip.
    if app.models.available.contains_key(id) {
        app.models.set_current(id.clone(), None);
    }
    true
}

/// Toast format for `default_model`. Mirrors `save_theme_toast` —
/// renders the user-friendly model name (NOT the internal id) so the
/// toast text matches what the user typed.
fn save_default_model_toast(value: &str) -> String {
    format!("\u{2713} Default model: {value}")
}

/// Outer dispatcher for `Action::SetDefaultModel`. Switches and persists
/// and toasts. `PersistSetting` emitted first for consistent rollback;
/// `SwitchModel` second. Idempotent: same model already active → no-op.
pub(in crate::app::dispatch) fn set_default_model(
    app: &mut AppView,
    new_id: acp::ModelId,
) -> Vec<Effect> {
    let ActiveView::Agent(aid) = app.active_view else {
        tracing::error!(
            target: "settings",
            key = "default_model",
            "Action::SetDefaultModel dispatched with no active agent — no-op",
        );
        return vec![];
    };

    // Snapshot previous id + display name from the active agent's
    // session (the same source `set_default_model_inner` mutates
    // and the modal reads).
    let (prev_id, session_id, available_has_new, new_display) = {
        let Some(agent) = app.agents.get(&aid) else {
            tracing::error!(
                target: "settings",
                key = "default_model",
                "Action::SetDefaultModel: active_view::Agent points to missing agent",
            );
            return vec![];
        };
        let prev_id = agent.session.models.current.clone();
        let session_id = agent.session.session_id.clone();
        let available_has_new = agent.session.models.available.contains_key(&new_id);
        let new_display = agent.session.models.display_name_for(&new_id);
        (prev_id, session_id, available_has_new, new_display)
    };

    if !available_has_new {
        tracing::error!(
            target: "settings",
            key = "default_model",
            id = ?new_id,
            "Action::SetDefaultModel dispatched with model id not in catalog — \
             validator skew; no-op",
        );
        return vec![];
    }

    // Idempotent: same model already active → no-op.
    if prev_id.as_ref() == Some(&new_id) {
        return vec![];
    }

    let did_mutate = set_default_model_inner(app, &new_id);
    debug_assert!(did_mutate, "available_has_new gate guarantees mutation");
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "default_model",
        new = ?new_display,
        new_id = %new_id.0,
        prev_id = ?prev_id.as_ref().map(|id| id.0.as_ref()),
        "setting changed",
    );
    app.show_toast(&save_default_model_toast(&new_display));

    // Persist the **model ID** (catalog key), not the display name.
    // The shell's `resolve_default_model` matches by slug / map key,
    // so persisting the human-readable name (e.g. "EchoAgent Build")
    // would silently fail to resolve on the next startup.
    //
    // Chat (`--chat` / ECHO_AGENT_CHAT_MODE) catalogs use opaque `/rest/modes`
    // slugs that must not become the global Build `default_model`.
    let mut effects: Vec<Effect> = Vec::new();
    if !echo_agent_runtime::agent::chat_modes::process_chat_mode_enabled() {
        let new_id_str = new_id.0.to_string();
        let prev_id_str = prev_id
            .as_ref()
            .map(|id| id.0.to_string())
            .unwrap_or_default();
        effects.push(Effect::PersistSetting {
            key: "default_model",
            value: crate::settings::SettingValue::String(new_id_str),
            rollback_value: crate::settings::SettingValue::String(prev_id_str),
        });
    }

    // Best-effort session-level switch. The `Effect::SwitchModel`
    // pipeline handles its own deferred-switch semantics for the
    // no-session-id-yet case (see line 583 of this file).
    if let Some(sid) = session_id {
        // We already hold a reference path to the agent above; re-borrow
        // mutably here to flip `model_switch_pending`.
        if let Some(agent) = app.agents.get_mut(&aid) {
            agent.session.model_switch_pending = true;
        }
        effects.push(Effect::SwitchModel {
            agent_id: aid,
            session_id: sid,
            model_id: new_id,
            effort: None,
            prev_model_id: prev_id.clone(),
        });
    } else if let Some(agent) = app.agents.get_mut(&aid) {
        // No session id yet — stash for
        // `EventLoop::on_session_created` to apply once the session
        // id materialises. Mirrors `Action::SwitchModel` line 586.
        agent.session.deferred_model_switch = Some(crate::app::agent::DeferredModelSwitch {
            model_id: new_id,
            effort: None,
            prev_model_id: prev_id,
        });
    }
    effects
}

/// Clear the default model override. Persists `[models].default = None`;
/// does NOT mutate the active session's current model.
pub(in crate::app::dispatch) fn clear_default_model(app: &mut AppView) -> Vec<Effect> {
    // Active-agent snapshot: prev model ID for the rollback payload.
    // Use the model ID (catalog key), not the display name, so that
    // rollback persists a value `resolve_default_model` can match.
    let prev_id_str = if let ActiveView::Agent(aid) = app.active_view
        && let Some(agent) = app.agents.get(&aid)
    {
        agent
            .session
            .models
            .current
            .as_ref()
            .map(|id| id.0.to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };

    // During the startup window `current` is None even if a default
    // is on disk. Emit persist unconditionally — the shell de-dupes.
    if prev_id_str.is_empty() {
        // Cosmetic toast even when the persist may be a no-op.
        tracing::info!(
            target: "settings",
            key = "default_model",
            value = "<cleared>",
            "setting changed (startup-window clear — pager mirror was already None; \
             persist proceeds to ensure disk state matches user intent)",
        );
        app.show_toast("\u{2713} Default model: cleared");
        return vec![Effect::PersistSetting {
            key: "default_model",
            value: crate::settings::SettingValue::String(String::new()),
            rollback_value: crate::settings::SettingValue::String(String::new()),
        }];
    }

    tracing::info!(
        target: "settings",
        key = "default_model",
        value = "<cleared>",
        prev_id = %prev_id_str,
        "setting changed",
    );
    refresh_open_settings_modals(app);
    app.show_toast("\u{2713} Default model: cleared");
    vec![Effect::PersistSetting {
        key: "default_model",
        value: crate::settings::SettingValue::String(String::new()),
        rollback_value: crate::settings::SettingValue::String(prev_id_str),
    }]
}

// ---------------------------------------------------------------------------
// Model-family settings: fork_secondary_model (and formerly
// web_search_model, session_summary_model, default_reasoning_effort).
//
// SHELL-OWNED. Unlike `default_model`, these do NOT mutate live
// runtime state — they update `current_ui` mirrors and persist.
// No live preview. Rollback is purely disk + mirror.
// ---------------------------------------------------------------------------

/// State-only mutation for `fork_secondary_model`. Updates the
/// `app.current_ui.fork_secondary_model` mirror so the modal
/// indicator stays in sync; the shell's config-reloader propagates
/// the disk change to any running agents on the next fork.
pub(super) fn set_fork_secondary_model_inner(app: &mut AppView, value: String) {
    app.current_ui.fork_secondary_model = value;
}

/// Toast format for `fork_secondary_model`. Mirrors
/// `save_default_model_toast` — renders the user-friendly model
/// name (NOT the internal id).
fn save_fork_secondary_model_toast(value: &str) -> String {
    format!("\u{2713} Fork secondary model: {value}")
}

/// Outer dispatcher for `Action::SetForkSecondaryModel`.
/// Mirror + persist + toast. Idempotent: same-id → no-op.
pub(in crate::app::dispatch) fn set_fork_secondary_model(
    app: &mut AppView,
    new_id: acp::ModelId,
) -> Vec<Effect> {
    let ActiveView::Agent(aid) = app.active_view else {
        tracing::error!(
            target: "settings",
            key = "fork_secondary_model",
            "Action::SetForkSecondaryModel dispatched with no active agent — no-op",
        );
        return vec![];
    };
    let (new_display, available_has_new) = {
        let Some(agent) = app.agents.get(&aid) else {
            tracing::error!(
                target: "settings",
                key = "fork_secondary_model",
                "Action::SetForkSecondaryModel: active_view::Agent points to missing agent",
            );
            return vec![];
        };
        let display = agent.session.models.display_name_for(&new_id);
        let has = agent.session.models.available.contains_key(&new_id);
        (display, has)
    };
    if !available_has_new {
        tracing::error!(
            target: "settings",
            key = "fork_secondary_model",
            id = ?new_id,
            "Action::SetForkSecondaryModel dispatched with id not in catalog — \
             validator skew; no-op",
        );
        return vec![];
    }
    // Mirror, idempotency, persist, and rollback all use the model ID
    // (catalog key), not the display name. The shell's
    // `cfg.ui.fork_secondary_model` consumers match by slug / map key,
    // and the `current_value_for` fold-to-empty comparison checks
    // against `models::default_model()` (also a slug).
    let new_id_str = new_id.0.to_string();
    let prev_id_str = app.current_ui.fork_secondary_model.clone();
    if prev_id_str == new_id_str {
        // Idempotent fast-path: no-op for redundant writes.
        return vec![];
    }
    set_fork_secondary_model_inner(app, new_id_str.clone());
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "fork_secondary_model",
        new = ?new_display,
        new_id = %new_id_str,
        prev_id = %prev_id_str,
        "setting changed",
    );
    app.show_toast(&save_fork_secondary_model_toast(&new_display));
    vec![Effect::PersistSetting {
        key: "fork_secondary_model",
        value: crate::settings::SettingValue::String(new_id_str),
        rollback_value: crate::settings::SettingValue::String(prev_id_str),
    }]
}

/// Outer dispatcher for `Action::ClearForkSecondaryModel`. Resets
/// the persisted override to the built-in baseline
/// (`echo_agent_runtime::models::default_model()`).
pub(in crate::app::dispatch) fn clear_fork_secondary_model(app: &mut AppView) -> Vec<Effect> {
    let baseline = echo_agent_runtime::models::default_model().to_string();
    let prev_id_str = app.current_ui.fork_secondary_model.clone();
    if prev_id_str == baseline {
        // Idempotent: already at baseline.
        app.show_toast("\u{2713} Fork secondary model: already at default");
        return vec![];
    }
    tracing::info!(
        target: "settings",
        key = "fork_secondary_model",
        value = "<cleared>",
        prev_id = %prev_id_str,
        "setting changed",
    );
    set_fork_secondary_model_inner(app, baseline);
    refresh_open_settings_modals(app);
    app.show_toast("\u{2713} Fork secondary model: cleared");
    vec![Effect::PersistSetting {
        key: "fork_secondary_model",
        // Persist payload is the empty-sentinel — the shell helper
        // interprets empty as "restore the baseline default", same
        // contract as `default_model`'s empty payload.
        value: crate::settings::SettingValue::String(String::new()),
        rollback_value: crate::settings::SettingValue::String(prev_id_str),
    }]
}

// `web_search_model`, `session_summary_model`, and
// `default_reasoning_effort` setters were removed alongside their
// registry entries. Mirror fields and TOML schema stay for compat.

// ---------------------------------------------------------------------------
// max_thoughts_width — Int-valued setting. Registry surface is `i64`;
// clamped to `(min, max)` bounds and cast to `u16`. Live application
// via `app.current_ui.max_thoughts_width`.
// ---------------------------------------------------------------------------

/// Clamp `i64` to the registered `max_thoughts_width` bounds.
/// Bounds imported from `settings::defs` (single source of truth).
fn clamp_max_thoughts_width(value: i64) -> i64 {
    value.clamp(
        crate::settings::defs::MAX_THOUGHTS_WIDTH_MIN,
        crate::settings::defs::MAX_THOUGHTS_WIDTH_MAX,
    )
}

/// State-only mutation for `max_thoughts_width`. Updates the
/// `app.current_ui.max_thoughts_width` field; the renderer picks up
/// the new value on the next frame.
pub(super) fn set_max_thoughts_width_inner(app: &mut AppView, value: i64) {
    app.current_ui.max_thoughts_width = clamp_max_thoughts_width(value) as u16;
}

/// Outer dispatcher for `Action::SetMaxThoughtsWidth`. State +
/// refresh + persist + toast.
pub(in crate::app::dispatch) fn set_max_thoughts_width(app: &mut AppView, new: i64) -> Vec<Effect> {
    let prev = app.current_ui.max_thoughts_width as i64;
    let clamped = clamp_max_thoughts_width(new);
    if prev == clamped {
        // Idempotent fast-path: no-op for redundant writes. Matches
        // the bool setters' idempotency contract.
        return vec![];
    }
    set_max_thoughts_width_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "max_thoughts_width",
        value = clamped,
        "setting changed",
    );
    app.show_toast(&format!("\u{2713} Max thoughts width: {clamped}"));
    vec![Effect::PersistSetting {
        key: "max_thoughts_width",
        value: crate::settings::SettingValue::Int(clamped),
        rollback_value: crate::settings::SettingValue::Int(prev),
    }]
}

// `auto_compact_threshold_percent` setter was removed alongside its
// registry entry. Mirror field stays for compat.

// ---------------------------------------------------------------------------
// show_tips, auto_update — SHELL-OWNED `Option<bool>` setters.
// Changes take effect on next session start (restart_required: true).
// Standard inner/outer split. First commit of the default value
// persists (so the resolver sees user intent vs managed default).
// Rollback restores `None` when the target equals the effective
// default (keeps mirror in sync with on-disk state after failure).
// ---------------------------------------------------------------------------

/// Effective-default lookup for the `Option<bool>` AppView mirrors
/// (`show_tips`, `auto_update`, ask_user_question timeout).
/// Matches the consumer's `.unwrap_or(...)` fallback.
pub(super) fn pr13_effective_default(key: &str) -> Option<bool> {
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question;
    match key {
        "show_tips" => Some(true),
        "auto_update" => Some(true),
        "toolset.ask_user_question.timeout_enabled" => {
            Some(ask_user_question::DEFAULT_ASK_USER_QUESTION_TIMEOUT_ENABLED)
        }
        _ => None,
    }
}

/// State-only mutation for `show_tips`.
pub(super) fn set_show_tips_inner(app: &mut AppView, value: bool) {
    app.show_tips = Some(value);
}

/// Outer dispatcher for `Action::SetShowTips`.
pub(in crate::app::dispatch) fn set_show_tips(app: &mut AppView, new: bool) -> Vec<Effect> {
    let prev_state = app.show_tips;
    let prev_effective = prev_state.unwrap_or(true);
    if prev_effective == new && prev_state.is_some() {
        // Idempotent fast-path. `.is_some()` lets the first commit of
        // the default value persist (so the resolver sees user intent).
        return vec![];
    }
    set_show_tips_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "show_tips", value = new, "setting changed");
    // Restart-required: cue in the toast.
    app.show_toast(&format!(
        "{} (restart to apply)",
        save_success_toast("Show tips", new),
    ));
    vec![Effect::PersistSetting {
        key: "show_tips",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev_effective),
    }]
}

/// State-only mutation for `auto_update`.
pub(super) fn set_auto_update_inner(app: &mut AppView, value: bool) {
    app.auto_update = Some(value);
}

/// Outer dispatcher for `Action::SetAutoUpdate`.
pub(in crate::app::dispatch) fn set_auto_update(app: &mut AppView, new: bool) -> Vec<Effect> {
    let prev_state = app.auto_update;
    let prev_effective = prev_state.unwrap_or(true);
    if prev_effective == new && prev_state.is_some() {
        return vec![];
    }
    set_auto_update_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(target: "settings", key = "auto_update", value = new, "setting changed");
    app.show_toast(&format!(
        "{} (restart to apply)",
        save_success_toast("Auto-update", new),
    ));
    vec![Effect::PersistSetting {
        key: "auto_update",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev_effective),
    }]
}

// ---------------------------------------------------------------------------
// display_refresh_auto_cadence — SHELL-OWNED nested Option on
// `[ui.display_refresh].auto_cadence_enabled`. Restart-required.
// ---------------------------------------------------------------------------

/// State-only mutation for `display_refresh_auto_cadence`.
pub(super) fn set_display_refresh_auto_cadence_inner(app: &mut AppView, value: bool) {
    app.current_ui.display_refresh.auto_cadence_enabled = Some(value);
}

/// Outer dispatcher for `Action::SetDisplayRefreshAutoCadence`.
pub(in crate::app::dispatch) fn set_display_refresh_auto_cadence(
    app: &mut AppView,
    new: bool,
) -> Vec<Effect> {
    let prev_state = app.current_ui.display_refresh.auto_cadence_enabled;
    let prev_effective = prev_state.unwrap_or(false);
    if prev_effective == new && prev_state.is_some() {
        return vec![];
    }
    set_display_refresh_auto_cadence_inner(app, new);
    refresh_open_settings_modals(app);
    tracing::info!(
        target: "settings",
        key = "display_refresh_auto_cadence",
        value = new,
        "setting changed",
    );
    app.show_toast(&format!(
        "{} (restart to apply)",
        save_success_toast("Match display refresh rate", new),
    ));
    vec![Effect::PersistSetting {
        key: "display_refresh_auto_cadence",
        value: crate::settings::SettingValue::Bool(new),
        rollback_value: crate::settings::SettingValue::Bool(prev_effective),
    }]
}
