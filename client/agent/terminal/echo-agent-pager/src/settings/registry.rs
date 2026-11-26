//! Settings registry — pure-metadata data model.
//!
//! See the module-level docs in `mod.rs` for the architectural rationale.

use agent_client_protocol as acp;
use echo_agent_runtime::agent::config::UiConfig;
use echo_agent_runtime::util::config::DISPLAY_REFRESH_DEFAULT_AUTO_CADENCE_ENABLED;
use echo_agent_tools::implementations::echo_agent_build::ask_user_question;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Stable identity for a setting. The string id matches the `UiConfig`
/// serde field name (for SHELL/SHARED settings) and is the canonical key
/// referenced by tests, telemetry, and registry lookups.
///
/// We deliberately do NOT use a `SettingId` enum: enum renames would
/// ripple through call sites, while `&'static str` ties the registry's
/// vocabulary directly to the shell schema.
pub type SettingKey = &'static str;

/// Ownership class for a setting — the pager vs. shell ownership taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingOwner {
    /// In-memory pager state; no disk write (e.g. `multiline_mode`).
    Pager,
    /// Shell schema, shell-mediated write, no pager-side cache.
    Shell,
    /// Shell schema + pager-side thread-local cache for render hot path.
    Shared,
}

/// Categorization mirroring `shortcuts_help::CATEGORY_ORDER` —
/// the settings modal renders one section per category in `ALL` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingCategory {
    Appearance,
    Mouse,
    Editor,
    Agent,
    Privacy,
    Models,
    Session,
    Advanced,
}

impl SettingCategory {
    /// Render order — Appearance first (most-touched), then Mouse, then the rest.
    pub const ALL: &'static [Self] = &[
        Self::Appearance,
        Self::Mouse,
        Self::Editor,
        Self::Agent,
        Self::Privacy,
        Self::Models,
        Self::Session,
        Self::Advanced,
    ];

    /// Section-header label as rendered in the modal.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Mouse => "Mouse",
            Self::Editor => "Editor & Input",
            Self::Agent => "Agent & Approval",
            Self::Privacy => "Privacy",
            Self::Models => "Models",
            Self::Session => "Session",
            Self::Advanced => "Advanced",
        }
    }
}

/// One choice in an `Enum` setting.
#[derive(Debug, Clone, Copy)]
pub struct EnumChoice {
    /// Canonical persisted value (e.g. `"echo-agent-night"`).
    pub canonical: &'static str,
    /// Display label shown in the chooser (e.g. `"EchoAgent Night"`).
    pub display: &'static str,
    /// Sub-text shown in the chooser sheet (e.g. `"Dark + magenta accent"`).
    pub description: &'static str,
}

/// Runtime-built enum choice for `SettingKind::DynamicEnum` settings
/// whose choices come from a `PagerLocalSnapshot` at picker-open time.
/// Owned `String` fields because values are sourced from runtime catalogs.
#[derive(Debug, Clone)]
pub struct OwnedEnumChoice {
    pub canonical: String,
    pub display: String,
    pub description: String,
}

/// Source of runtime choices for a `SettingKind::DynamicEnum`.
/// `#[non_exhaustive]` allows adding new sources without breaking matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DynamicEnumSource {
    /// Models from the active session's catalog. Prepends a
    /// `"(no override)"` sentinel so the user can clear the setting.
    ActiveModelCatalog,
}

/// Build the owned choice list for a `DynamicEnum` at picker-open time.
/// `ActiveModelCatalog` prepends an empty-canonical "(no override)"
/// choice at index 0 for clearing the setting.
pub fn dynamic_enum_choices(
    source: DynamicEnumSource,
    snapshot: &PagerLocalSnapshot,
) -> Vec<OwnedEnumChoice> {
    match source {
        DynamicEnumSource::ActiveModelCatalog => {
            let mut out = Vec::with_capacity(snapshot.available_models.len() + 1);
            out.push(OwnedEnumChoice {
                canonical: String::new(),
                display: "(no override)".to_string(),
                description: "Inherit the default model (no per-user override).".to_string(),
            });
            for (name, _id) in &snapshot.available_models {
                out.push(OwnedEnumChoice {
                    canonical: name.clone(),
                    display: name.clone(),
                    description: String::new(),
                });
            }
            out
        }
    }
}

/// String validator applied at write time.
///
/// **SECURITY:** The editor's char filter rejects both Cc and Cf
/// Unicode categories to prevent Trojan-Source visual spoofing.
/// New input surfaces (e.g. paste) must re-apply this filter.
#[derive(Debug, Clone, Copy)]
pub enum StringValidator {
    /// Non-empty, no whitespace. Used for model ids.
    NonEmptyToken,
    /// Validated against the live model catalog at commit time.
    /// Empty input is accepted as a "clear-default" sentinel.
    KnownModel,
    /// No constraint (any UTF-8 accepted).
    Any,
}

/// Value-kind metadata, including per-kind defaults.
/// `#[non_exhaustive]` allows adding new kinds without breaking matches.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SettingKind {
    Bool {
        default: bool,
    },
    /// Free-form (or validated) string.
    String {
        default: &'static str,
        validator: StringValidator,
    },
    /// Stringly-typed choice from a static catalog.
    Enum {
        default: &'static str,
        choices: &'static [EnumChoice],
        /// When `true`, navigating the chooser previews changes live;
        /// Esc reverts to the original value.
        supports_preview: bool,
    },
    /// Bounded integer. Modal stepper step sizes are derived from
    /// `(max - min)` (see `int_step_sizes` in the settings modal).
    Int {
        default: i64,
        min: i64,
        max: i64,
    },
    /// Choice from a runtime-resolved catalog (e.g. available models).
    /// Persists as `SettingValue::String(canonical)`.
    DynamicEnum {
        default: &'static str,
        source: DynamicEnumSource,
        supports_preview: bool,
    },
    /// A navigational row that opens a sub-sheet of `children` (other
    /// registered settings, by key). Carries no scalar value of its own:
    /// `current_value_for`/`default_value_for` skip it and the modal renders
    /// it as a chevron row whose Enter opens the sub-sheet. Children are
    /// hidden from the top-level list (rendered only inside the sub-sheet).
    Group {
        children: &'static [SettingKey],
    },
}

/// One row in the registry. Pure metadata — no function pointers, no
/// closures, no heap allocations beyond the static `keywords` slice.
#[derive(Debug, Clone)]
pub struct SettingMeta {
    /// Stable id; also the `UiConfig` serde field name and TOML key for
    /// SHELL/SHARED settings.
    pub key: SettingKey,
    pub category: SettingCategory,
    pub owner: SettingOwner,
    pub label: &'static str,
    pub description: &'static str,
    /// Free-form keywords for the search/filter. All lowercase,
    /// no empty strings — `keywords_lowercase_and_non_empty` enforces.
    pub keywords: &'static [&'static str],
    pub kind: SettingKind,
    /// When `true`, the value takes effect only on next session start.
    /// Renders a "restart" pill on the row while it is expanded.
    pub restart_required: bool,
    /// When `true`, the row is hidden in minimal mode (the setting still
    /// exists and applies to the full TUI).
    pub hidden_in_minimal: bool,
}

/// A typed value carried by `Action::Set*` payloads, modal preview state,
/// and the rollback path on persist failure.
///
/// Each variant aligns 1:1 with a `SettingKind` variant.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    String(String),
    Enum(&'static str),
    Int(i64),
}

/// Why `coding_data_sharing` cannot be changed in the settings modal.
/// Computed by `AppView::coding_data_sharing_lock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingDataSharingLock {
    Zdr,
    TeamManaged,
}

impl CodingDataSharingLock {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Zdr => "Your team has Zero Data Retention.",
            Self::TeamManaged => "Managed by your team admin.",
        }
    }
}

/// Snapshot of pager-local state captured when the modal opens.
/// Used by `current_value_for` to render against LIVE state rather
/// than the on-disk `UiConfig`. Refreshed by
/// `refresh_open_settings_modals` after every mutation.
#[derive(Debug, Clone)]
pub struct PagerLocalSnapshot {
    /// Whether multiline input mode is active.
    pub multiline_mode: bool,
    /// Whether YOLO mode (always-approve) is active on the active agent.
    pub yolo_mode: bool,
    /// Whether Auto (LLM classifier) mode is active on the active agent.
    /// Mutually exclusive with `yolo_mode` in practice (yolo wins); read by
    /// `/auto` so it can toggle off when already on.
    pub auto_mode: bool,
    /// Currently-selected model's display name, or `None` if no catalog
    /// has loaded yet.
    pub current_model_name: Option<String>,
    /// `(display_name, ModelId)` pairs from the active session's catalog.
    /// Cloned into the snapshot so the modal's validator/resolver is
    /// self-contained (the modal outlives the borrow on `app.agents`).
    pub available_models: Vec<(String, acp::ModelId)>,
    /// Whether the user has opted OUT of coding data sharing.
    /// Lives in auth metadata (no `UiConfig` field). Inverted mapping:
    /// `opt_out == false` → canonical "opt-in". Snapshot default is
    /// `true` (opted out) to match the safer consumer default.
    pub coding_data_sharing_opt_out: bool,
    /// Why `coding_data_sharing` cannot be changed here (`None` = editable).
    pub coding_data_sharing_lock: Option<CodingDataSharingLock>,
    /// Whether plan mode is active. Uses effective state
    /// (`pending.unwrap_or(active)`) so rapid toggles don't double-send.
    /// Refreshed on all mutation paths including ACP `CurrentModeUpdate`.
    pub plan_mode_active: bool,
    /// `[cli].show_tips` mirror. `None` = no TOML override → default `true`.
    pub show_tips: Option<bool>,
    /// `[cli].auto_update` mirror. `None` = no TOML override → default `false`.
    pub auto_update: Option<bool>,
    /// Process-wide vim-mode scrollback flag. Mirrors
    /// `appearance::cache::load_vim_mode()` at snapshot time.
    pub vim_mode: bool,
    /// Process-wide mouse-wheel scroll speed (1-100). Mirrors
    /// `appearance::cache::load_scroll_speed()` at snapshot time.
    pub scroll_speed: u8,
    /// Mirrors `AppView::appearance.scrollback.scroll.respect_manual_folds`
    /// at snapshot time.
    pub respect_manual_folds: bool,
    /// Mirrors `AppView::auto_mode_gate` at snapshot time. When false the
    /// permission-mode picker hides the "Auto" choice (matches the Shift+Tab
    /// cycle, which skips Auto when the feature gate is off).
    pub auto_mode_gate: bool,
    /// `[toolset.ask_user_question].timeout_enabled` mirror (effective TOML
    /// merge, like `show_tips`). `None` = unset in TOML → default `true`.
    pub ask_user_question_timeout_enabled: Option<bool>,
    /// Live `voice_config.language` at snapshot time. Lets the modal show the
    /// language actually in effect when `[ui].voice_stt_language` is unset but
    /// an explicit `[voice].language` applies.
    pub voice_stt_language: String,
    /// Mirrors `AgentView::scheduler_background_loops` — the value the shell
    /// pinned for THIS session — falling back to
    /// `AppView::scheduler_background_loops_seed` before the session response
    /// lands. `/loop` reads it to describe where a scheduled fire runs.
    pub scheduler_background_loops: bool,
}

impl Default for PagerLocalSnapshot {
    fn default() -> Self {
        Self {
            multiline_mode: false,
            yolo_mode: false,
            auto_mode: false,
            current_model_name: None,
            available_models: Vec::new(),
            coding_data_sharing_opt_out: true,
            coding_data_sharing_lock: None,
            plan_mode_active: false,
            show_tips: None,
            auto_update: None,
            vim_mode: false,
            // Matches the registry default and
            // `appearance::cache::SCROLL_SPEED_DEFAULT`. Bare `u8::default()`
            // would be `0` (out-of-range) so we override.
            scroll_speed: 50,
            respect_manual_folds: crate::appearance::ScrollConfig::default().respect_manual_folds,
            auto_mode_gate: false,
            ask_user_question_timeout_enabled: None,
            voice_stt_language: echo_agent_voice::STT_LANGUAGE_DEFAULT.to_string(),
            // Matches `resolve_scheduler_background_loops`'s default.
            scheduler_background_loops: true,
        }
    }
}

/// Canonicalize a raw voice-capture mode to a registry choice. Case-insensitive
/// and trimmed; unknown/blank/`None` → `hold` (the default).
pub fn canonical_voice_capture_mode(value: Option<&str>) -> &'static str {
    let raw = value.unwrap_or_default().trim();
    if raw.eq_ignore_ascii_case("toggle") {
        "toggle"
    } else {
        "hold"
    }
}

/// Canonicalize a raw voice STT language to a settings choice.
///
/// Delegates to [`echo_agent_voice::canonicalize_stt_language`] so the pager and
/// the STT client share one catalog (official EchoAgent STT languages + client-only
/// `auto`). Unknown/blank/`None` → `en`.
pub fn canonical_voice_stt_language(value: Option<&str>) -> &'static str {
    echo_agent_voice::canonicalize_stt_language(value)
}

/// Canonicalize a raw hunk-tracker mode to a registry choice. Case-insensitive
/// and trimmed; `disabled` aliases `off`; unknown/blank/`None` → `off`.
pub fn canonical_hunk_tracker_mode(value: Option<&str>) -> &'static str {
    let raw = value.unwrap_or_default().trim();
    if raw.eq_ignore_ascii_case("all_dirty") {
        "all_dirty"
    } else if raw.eq_ignore_ascii_case("agent_only") {
        "agent_only"
    } else {
        "off"
    }
}

/// `minimal` stays; everything else (including unset / legacy `default`) → `fullscreen`.
pub fn canonical_screen_mode(value: Option<&str>) -> &'static str {
    let raw = value.unwrap_or_default().trim();
    if raw.eq_ignore_ascii_case("minimal") {
        "minimal"
    } else {
        "fullscreen"
    }
}

impl PagerLocalSnapshot {
    /// Iterate over just the display names. Convenience helper for
    /// validator paths that don't need the ids.
    pub fn available_model_names(&self) -> impl Iterator<Item = &str> {
        self.available_models.iter().map(|(name, _)| name.as_str())
    }

    /// Resolve a user-supplied name to a `ModelId` via the snapshot.
    /// Case-insensitive ASCII match against display names only (ids
    /// aren't carried in the snapshot's primary key — callers needing
    /// id-based resolution should reach for `ModelState::resolve_by_name_or_id`).
    pub fn resolve_model_name(&self, query: &str) -> Option<acp::ModelId> {
        self.available_models.iter().find_map(|(name, id)| {
            if name.eq_ignore_ascii_case(query) {
                Some(id.clone())
            } else {
                None
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Process-wide settings registry. Built in `main` and stored on
/// `AppView::settings_registry: Arc<SettingsRegistry>`.
#[derive(Debug, Clone)]
pub struct SettingsRegistry {
    entries: Vec<SettingMeta>,
}

impl SettingsRegistry {
    /// Build the default registry from `crate::settings::defs::default_settings()`.
    pub fn defaults() -> Self {
        let entries = crate::settings::defs::default_settings();
        assert_unique_keys(&entries);
        Self { entries }
    }

    /// Build a registry from a caller-supplied list of `SettingMeta`.
    /// Test seam — `#[doc(hidden)]` to discourage production use.
    /// Panics on duplicate keys.
    #[doc(hidden)]
    pub fn from_entries(entries: Vec<SettingMeta>) -> Self {
        assert_unique_keys(&entries);
        Self { entries }
    }

    /// All registered settings, in declaration order.
    pub fn all(&self) -> &[SettingMeta] {
        &self.entries
    }

    /// Look up a setting by key.
    pub fn find(&self, key: SettingKey) -> Option<&SettingMeta> {
        self.entries.iter().find(|m| m.key == key)
    }

    /// Iterate the settings in a given category, preserving declaration
    /// order.
    pub fn by_category(&self, cat: SettingCategory) -> impl Iterator<Item = &SettingMeta> {
        self.entries.iter().filter(move |m| m.category == cat)
    }

    /// Multi-word AND match against label, description, key, and keywords.
    pub fn search(&self, query: &str) -> Vec<&SettingMeta> {
        let q = query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        if words.is_empty() {
            return self.entries.iter().collect();
        }
        self.entries
            .iter()
            .filter(|m| {
                let haystack = build_search_haystack(m);
                words.iter().all(|w| haystack.contains(w))
            })
            .collect()
    }
}

/// Panic if `entries` contains duplicate keys. Called from both
/// `defaults()` and `from_entries()` — downstream string-equality
/// gates rely on each key mapping to exactly one entry.
fn assert_unique_keys(entries: &[SettingMeta]) {
    use std::collections::HashSet;
    let mut seen: HashSet<&str> = HashSet::with_capacity(entries.len());
    let mut dupes: Vec<&str> = Vec::new();
    for m in entries {
        if !seen.insert(m.key) {
            dupes.push(m.key);
        }
    }
    assert!(
        dupes.is_empty(),
        "duplicate setting keys in registry: {dupes:?} — every \
         registered key must be globally unique so string-equality \
         gates in the view layer match at most one entry",
    );
}

fn build_search_haystack(m: &SettingMeta) -> String {
    let mut s = String::new();
    s.push_str(&m.label.to_lowercase());
    s.push(' ');
    s.push_str(&m.description.to_lowercase());
    s.push(' ');
    s.push_str(m.key);
    for kw in m.keywords {
        s.push(' ');
        s.push_str(kw);
    }
    s
}

// ---------------------------------------------------------------------------
// Snapshot reads — the one place that maps SettingKey → live field.
// ---------------------------------------------------------------------------

/// Read the current value of `key` from `UiConfig` (SHELL/SHARED) or
/// pager snapshot (PAGER-owned). Returns `None` for unknown keys.
/// Adding a new Bool setting requires arms here, in `action_for_bool`,
/// a `Action::SetX` variant, a shell helper, and an e2e test row.
pub fn current_value_for(
    key: SettingKey,
    ui: &UiConfig,
    pager: &PagerLocalSnapshot,
) -> Option<SettingValue> {
    match key {
        // SHARED — UiConfig source of truth, pager keeps a cache.
        "compact_mode" => Some(SettingValue::Bool(ui.compact_mode)),
        "show_timestamps" => Some(SettingValue::Bool(ui.show_timestamps.unwrap_or(true))),
        "show_timeline" => Some(SettingValue::Bool(ui.show_timeline_enabled())),
        // Cache is the send-path source of truth (same pattern as group_tool_verbs).
        "page_flip_on_send" => Some(SettingValue::Bool(
            crate::appearance::cache::load_page_flip_on_send(),
        )),
        // Cache is the drain-path source of truth (same pattern as page_flip_on_send).
        "combine_queued_prompts" => Some(SettingValue::Bool(
            crate::appearance::cache::load_combine_queued_prompts(),
        )),
        "follow_up_behavior" => Some(SettingValue::Enum(
            crate::appearance::cache::load_follow_up_behavior().as_canonical(),
        )),
        "confirm_before_rewind" => Some(SettingValue::Bool(ui.confirm_before_rewind_enabled())),
        "simple_mode" => Some(SettingValue::Bool(ui.simple_mode.unwrap_or(true))),
        // Per-tip contextual hints — `None` (inherit) reads as the default ON.
        "contextual_hints.undo" => {
            Some(SettingValue::Bool(ui.contextual_hints.undo.unwrap_or(true)))
        }
        "contextual_hints.plan_mode" => Some(SettingValue::Bool(
            ui.contextual_hints.plan_mode.unwrap_or(true),
        )),
        "contextual_hints.image_input" => Some(SettingValue::Bool(
            ui.contextual_hints.image_input.unwrap_or(true),
        )),
        "contextual_hints.send_now" => Some(SettingValue::Bool(
            ui.contextual_hints.send_now.unwrap_or(true),
        )),
        "contextual_hints.small_screen" => Some(SettingValue::Bool(
            ui.contextual_hints.small_screen.unwrap_or(true),
        )),
        "contextual_hints.word_select" => Some(SettingValue::Bool(
            ui.contextual_hints.word_select.unwrap_or(true),
        )),
        "contextual_hints.ssh_wrap" => Some(SettingValue::Bool(
            ui.contextual_hints.ssh_wrap.unwrap_or(true),
        )),
        "keep_text_selection" => Some(SettingValue::Enum(
            crate::appearance::cache::load_keep_text_selection().as_canonical(),
        )),
        // PAGER — read from snapshot.
        "multiline_mode" => Some(SettingValue::Bool(pager.multiline_mode)),
        // PAGER — read from process-wide cache (snapshot mirror keeps
        // the modal in sync with the live cache value).
        "vim_mode" => Some(SettingValue::Bool(pager.vim_mode)),
        "scroll_speed" => Some(SettingValue::Int(pager.scroll_speed as i64)),
        // Live caches (like `group_tool_verbs`); scroll_lines shows the
        // registry default 3 while unset (profile-default state).
        "scroll_mode" => Some(SettingValue::Enum(
            crate::appearance::cache::load_scroll_mode().as_canonical(),
        )),
        "invert_scroll" => Some(SettingValue::Bool(
            crate::appearance::cache::load_invert_scroll(),
        )),
        // Nested `[ui.display_refresh].auto_cadence_enabled`; None → compiled default.
        "display_refresh_auto_cadence" => Some(SettingValue::Bool(
            ui.display_refresh
                .auto_cadence_enabled
                .unwrap_or(DISPLAY_REFRESH_DEFAULT_AUTO_CADENCE_ENABLED),
        )),
        "scroll_lines" => Some(SettingValue::Int(
            crate::appearance::cache::load_scroll_lines()
                .map(i64::from)
                .unwrap_or(3),
        )),
        // Live cache (like `render_mermaid`).
        "show_thinking_blocks" => Some(SettingValue::Bool(
            crate::appearance::cache::load_show_thinking_blocks(),
        )),
        // Live cache (like `show_thinking_blocks`).
        "group_tool_verbs" => Some(SettingValue::Bool(
            crate::appearance::cache::load_group_tool_verbs(),
        )),
        // Live cache (like `group_tool_verbs`).
        "collapsed_edit_blocks" => Some(SettingValue::Bool(
            crate::appearance::cache::load_collapsed_edit_blocks(),
        )),
        // Live cache; `ECHO_AGENT_PROMPT_SUGGESTIONS` env overrides at the gate.
        "prompt_suggestions" => Some(SettingValue::Bool(
            crate::appearance::cache::load_prompt_suggestions(),
        )),
        "respect_manual_folds" => Some(SettingValue::Bool(pager.respect_manual_folds)),
        // SHELL — canonicalized from `[ui].hunk_tracker_mode`.
        "hunk_tracker_mode" => Some(SettingValue::Enum(canonical_hunk_tracker_mode(
            ui.hunk_tracker_mode.as_deref(),
        ))),
        "screen_mode" => Some(SettingValue::Enum(canonical_screen_mode(
            ui.screen_mode.as_deref(),
        ))),
        // SHELL — whether the Ctrl+Space / F8 chord is active; None → true.
        "voice_keybind_enabled" => {
            Some(SettingValue::Bool(ui.voice_keybind_enabled.unwrap_or(true)))
        }
        // SHELL — canonicalized from `[ui].voice_capture_mode`; None → "hold".
        "voice_capture_mode" => Some(SettingValue::Enum(canonical_voice_capture_mode(
            ui.voice_capture_mode.as_deref(),
        ))),
        // SHELL — canonicalized from `[ui].voice_stt_language`. When unset,
        // fall back to the live `voice_config.language` (snapshot mirror) so
        // an explicit `[voice].language` shows as the current choice instead
        // of the registry default.
        "voice_stt_language" => Some(SettingValue::Enum(canonical_voice_stt_language(Some(
            ui.voice_stt_language
                .as_deref()
                .unwrap_or(&pager.voice_stt_language),
        )))),
        // Theme: unknown disk values fall through to canonical default.
        // auto_dark/light additionally filter out "auto" (circular ref).
        "theme" => Some(SettingValue::Enum(
            ui.theme
                .as_deref()
                .and_then(crate::theme::canonical_name)
                .unwrap_or("echo-agent-night"),
        )),
        "auto_dark_theme" => Some(SettingValue::Enum(
            ui.auto_dark_theme
                .as_deref()
                .and_then(crate::theme::canonical_name)
                .filter(|s| *s != "auto")
                .unwrap_or("echo-agent-night"),
        )),
        "auto_light_theme" => Some(SettingValue::Enum(
            ui.auto_light_theme
                .as_deref()
                .and_then(crate::theme::canonical_name)
                .filter(|s| *s != "auto")
                .unwrap_or("echo-agent-day"),
        )),
        // render_mermaid: SHELL-owned (persisted to `[ui].render_mermaid`).
        // Read from the process-wide cache mirror, which reflects the live value
        // the render path uses — the `vim_mode` snapshot field plays the same
        // role for its setting.
        "render_mermaid" => Some(SettingValue::Enum(
            crate::appearance::cache::load_render_mermaid().as_canonical(),
        )),
        // permission_mode: live snapshot wins over on-disk value.
        // yolo=true → "always-approve"; else honor ui ("auto" / "default" / "ask").
        "permission_mode" => Some(SettingValue::Enum(if pager.yolo_mode {
            "always-approve"
        } else if matches!(ui.permission_mode.as_deref(), Some("auto")) {
            "auto"
        } else if matches!(ui.permission_mode.as_deref(), Some("default")) {
            "default"
        } else {
            "ask"
        })),
        // remember_tool_approvals: reflects the user-config layer the modal
        // toggles. None → the resolver-shared default.
        "remember_tool_approvals" => Some(SettingValue::Bool(
            ui.remember_tool_approvals
                .unwrap_or(echo_agent_runtime::util::config::DEFAULT_REMEMBER_TOOL_APPROVALS),
        )),
        // ask_user_question timeout: reflects the effective TOML merge; the
        // toggle writes the user layer, and env/remote settings tiers feed the
        // final gate at agent build. None → the resolver-shared default (ON).
        "toolset.ask_user_question.timeout_enabled" => Some(SettingValue::Bool(
            pager
                .ask_user_question_timeout_enabled
                .unwrap_or(ask_user_question::DEFAULT_ASK_USER_QUESTION_TIMEOUT_ENABLED),
        )),
        // default_selected_permission: maps `[ui].default_selected_permission`
        // onto one of the four registry canonicals. `None` / unrecognised on
        // disk → `always_allow_all_sessions` (the effective default — the
        // cursor lands on the "Always allow on all sessions" row, picked
        // explicitly in `enqueue_permission`).
        "default_selected_permission" => Some(SettingValue::Enum(
            crate::appearance::permission_cursor::DefaultSelectedPermission::from_config_value(
                ui.default_selected_permission
                    .as_deref()
                    .unwrap_or_default(),
            )
            .as_canonical(),
        )),
        // default_model: reads from pager snapshot (not UiConfig).
        // None (no catalog yet) → empty string.
        "default_model" => Some(SettingValue::String(
            pager.current_model_name.clone().unwrap_or_default(),
        )),
        // max_thoughts_width: `u16` widened to `i64`.
        "max_thoughts_width" => Some(SettingValue::Int(ui.max_thoughts_width as i64)),
        // coding_data_sharing: inverts the `_opt_out` bool.
        "coding_data_sharing" => Some(SettingValue::Enum(if pager.coding_data_sharing_opt_out {
            "opt-out"
        } else {
            "opt-in"
        })),
        // plan_mode: canonical via `PlanModeKind::from_bool().as_canonical()`.
        "plan_mode" => Some(SettingValue::Enum(
            crate::app::actions::PlanModeKind::from_bool(pager.plan_mode_active).as_canonical(),
        )),
        // CLI batch: snapshot mirrors use their effective defaults.
        "show_tips" => Some(SettingValue::Bool(pager.show_tips.unwrap_or(true))),
        "auto_update" => Some(SettingValue::Bool(pager.auto_update.unwrap_or(false))),
        // fork_secondary_model: baseline value folds to empty string. The
        // mirror persists the ModelId slug but the DynamicEnum canonicals
        // are catalog display names, so resolve via the snapshot; a stale
        // id passes through raw.
        "fork_secondary_model" => Some(SettingValue::String({
            let baseline = echo_agent_runtime::models::default_model();
            if ui.fork_secondary_model == baseline {
                String::new()
            } else {
                pager
                    .available_models
                    .iter()
                    .find(|(_, id)| id.0.as_ref() == ui.fork_secondary_model.as_str())
                    .map(|(name, _)| name.clone())
                    .unwrap_or_else(|| ui.fork_secondary_model.clone())
            }
        })),

        _ => None,
    }
}

/// Consent chooser: no docs tip, and no `d` reset (hint or key).
pub fn is_consent_chooser(key: &str) -> bool {
    key == "coding_data_sharing"
}

/// Default value for `key`, derived from the registry metadata.
pub fn default_value_for(meta: &SettingMeta) -> SettingValue {
    match &meta.kind {
        SettingKind::Bool { default } => SettingValue::Bool(*default),
        SettingKind::String { default, .. } => SettingValue::String((*default).to_string()),
        SettingKind::Enum { default, .. } => SettingValue::Enum(default),
        SettingKind::Int { default, .. } => SettingValue::Int(*default),
        // `DynamicEnum` widens to `String` for runtime catalog values.
        SettingKind::DynamicEnum { default, .. } => SettingValue::String((*default).to_string()),
        // Group rows carry no scalar value; the render/reset paths special-case
        // them before calling this, so the returned value is never observed.
        SettingKind::Group { .. } => SettingValue::Bool(false),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Every SHELL/SHARED setting's default must match `UiConfig::default()`.
    /// PAGER-owned settings are covered by `defaults_match_pager_state`.
    #[test]
    fn defaults_match_ui_config_default() {
        let reg = SettingsRegistry::defaults();
        let ui = UiConfig::default();
        for meta in reg.all() {
            if meta.owner == SettingOwner::Pager {
                continue;
            }
            // Group rows have no scalar default to compare.
            if matches!(meta.kind, SettingKind::Group { .. }) {
                continue;
            }
            match (meta.key, &meta.kind) {
                ("compact_mode", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default, ui.compact_mode,
                        "compact_mode default drifts from UiConfig::default()"
                    );
                }
                // Per-tip contextual hints: `None` (inherit) → default ON.
                ("contextual_hints.undo", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.contextual_hints.undo.unwrap_or(true),
                        "contextual_hints.undo default drifts from UiConfig::default()"
                    );
                }
                ("contextual_hints.plan_mode", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.contextual_hints.plan_mode.unwrap_or(true),
                        "contextual_hints.plan_mode default drifts from UiConfig::default()"
                    );
                }
                ("contextual_hints.image_input", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.contextual_hints.image_input.unwrap_or(true),
                        "contextual_hints.image_input default drifts from UiConfig::default()"
                    );
                }
                ("contextual_hints.send_now", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.contextual_hints.send_now.unwrap_or(true),
                        "contextual_hints.send_now default drifts from UiConfig::default()"
                    );
                }
                ("contextual_hints.small_screen", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.contextual_hints.small_screen.unwrap_or(true),
                        "contextual_hints.small_screen default drifts from UiConfig::default()"
                    );
                }
                ("contextual_hints.word_select", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.contextual_hints.word_select.unwrap_or(true),
                        "contextual_hints.word_select default drifts from UiConfig::default()"
                    );
                }
                ("contextual_hints.ssh_wrap", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.contextual_hints.ssh_wrap.unwrap_or(true),
                        "contextual_hints.ssh_wrap default drifts from UiConfig::default()"
                    );
                }
                ("show_timestamps", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.show_timestamps.unwrap_or(true),
                        "show_timestamps default drifts from UiConfig::default()"
                    );
                }
                ("show_timeline", SettingKind::Bool { default }) => {
                    // Single-sourced via UiConfig::show_timeline_enabled(); this
                    // guards that defs.rs wired the resolver, not a stray literal.
                    assert_eq!(
                        *default,
                        ui.show_timeline_enabled(),
                        "show_timeline default drifts from UiConfig::default()"
                    );
                }
                ("page_flip_on_send", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.page_flip_on_send_enabled(),
                        "page_flip_on_send default drifts from UiConfig::default()"
                    );
                }
                ("confirm_before_rewind", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.confirm_before_rewind_enabled(),
                        "confirm_before_rewind default drifts from UiConfig::default()"
                    );
                }
                ("combine_queued_prompts", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.combine_queued_prompts.unwrap_or(false),
                        "combine_queued_prompts default drifts from UiConfig::default()"
                    );
                }
                ("follow_up_behavior", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        *default,
                        ui.follow_up_behavior(),
                        "follow_up_behavior default drifts from UiConfig::default()"
                    );
                }
                ("simple_mode", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.simple_mode.unwrap_or(true),
                        "simple_mode default drifts from UiConfig::default()"
                    );
                }
                ("theme", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.theme, None,
                        "test assumes UiConfig::default().theme is None",
                    );
                    let expected = ui
                        .theme
                        .as_deref()
                        .and_then(crate::theme::canonical_name)
                        .unwrap_or("echo-agent-night");
                    assert_eq!(
                        *default, expected,
                        "theme default drifts from UiConfig::default()",
                    );
                }
                ("auto_dark_theme", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.auto_dark_theme, None,
                        "test assumes UiConfig::default().auto_dark_theme is None",
                    );
                    let expected = ui
                        .auto_dark_theme
                        .as_deref()
                        .and_then(crate::theme::canonical_name)
                        .filter(|s| *s != "auto")
                        .unwrap_or("echo-agent-night");
                    assert_eq!(
                        *default, expected,
                        "auto_dark_theme default drifts from UiConfig::default()",
                    );
                }
                ("auto_light_theme", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.auto_light_theme, None,
                        "test assumes UiConfig::default().auto_light_theme is None",
                    );
                    let expected = ui
                        .auto_light_theme
                        .as_deref()
                        .and_then(crate::theme::canonical_name)
                        .filter(|s| *s != "auto")
                        .unwrap_or("echo-agent-day");
                    assert_eq!(
                        *default, expected,
                        "auto_light_theme default drifts from UiConfig::default()",
                    );
                }
                // permission_mode: None on disk → "ask" fallback.
                ("permission_mode", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.permission_mode, None,
                        "test assumes UiConfig::default().permission_mode is None",
                    );
                    let expected = "ask";
                    assert_eq!(
                        *default, expected,
                        "permission_mode default drifts from UiConfig::default()'s \
                         None → 'ask' fallback (load_permission_mode contract)",
                    );
                }
                // default_model: no UiConfig mirror, resolved dynamically.
                // Registry default is empty string ("no opinion").
                ("default_model", SettingKind::DynamicEnum { default, .. }) => {
                    assert_eq!(
                        *default, "",
                        "default_model registry default must be empty string — \
                         the live default is resolved dynamically from \
                         cfg.models.default at session start",
                    );
                }
                // max_thoughts_width: `u16` widened to `i64`.
                ("max_thoughts_width", SettingKind::Int { default, .. }) => {
                    assert_eq!(
                        *default, ui.max_thoughts_width as i64,
                        "max_thoughts_width default drifts from UiConfig::default()",
                    );
                }
                // coding_data_sharing: no UiConfig field; default pinned
                // against auth metadata (opt_out=true → "opt-out").
                ("coding_data_sharing", SettingKind::Enum { default, .. }) => {
                    let expected = "opt-out";
                    assert_eq!(
                        *default, expected,
                        "coding_data_sharing registry default must be 'opt-out' — \
                         the on-disk source of truth is `AuthEntry::coding_data_retention_opt_out: \
                         bool` (defaults to `true`, i.e. user has opted out until they \
                         explicitly share or the server opts them in)",
                    );
                }
                // CLI batch: fields live on CliConfig, not UiConfig.
                // Defaults pinned literally.
                ("show_tips", SettingKind::Bool { default }) => {
                    assert!(*default, "show_tips registry default must be true");
                }
                ("auto_update", SettingKind::Bool { default }) => {
                    assert!(
                        !*default,
                        "auto_update registry default must be false \
                         (matches the embedded runtime update policy)"
                    );
                }
                // vim_mode: Option<bool>; None → false.
                ("vim_mode", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.vim_mode.unwrap_or(false),
                        "vim_mode default drifts from UiConfig::default()"
                    );
                }
                // remember_tool_approvals: anchored on the resolver-shared
                // const so the modal cannot drift from the gate.
                ("remember_tool_approvals", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        echo_agent_runtime::util::config::DEFAULT_REMEMBER_TOOL_APPROVALS,
                        "remember_tool_approvals default drifts from the shared \
                         resolver const in echo-agent-runtime"
                    );
                }
                // ask_user_question timeout: no UiConfig mirror (lives under
                // `[toolset]`); default anchored on the resolver-shared const.
                ("toolset.ask_user_question.timeout_enabled", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ask_user_question::DEFAULT_ASK_USER_QUESTION_TIMEOUT_ENABLED,
                        "toolset.ask_user_question.timeout_enabled default drifts from the \
                         shared resolver const in echo-agent-tools"
                    );
                }
                // show_thinking_blocks: Option<bool>; None → true (client default).
                ("show_thinking_blocks", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.show_thinking_blocks.unwrap_or(true),
                        "show_thinking_blocks default drifts from UiConfig::default()"
                    );
                }
                // group_tool_verbs: Option<bool>; None → true (client default).
                ("group_tool_verbs", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.group_tool_verbs.unwrap_or(true),
                        "group_tool_verbs default drifts from UiConfig::default()"
                    );
                }
                // collapsed_edit_blocks: Option<bool>; None → false (rollout
                // flag ships OFF; the cache const is pinned in cache.rs).
                ("collapsed_edit_blocks", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.collapsed_edit_blocks.unwrap_or(false),
                        "collapsed_edit_blocks default drifts from UiConfig::default()"
                    );
                    assert!(!*default, "collapsed_edit_blocks must default OFF");
                }
                // prompt_suggestions: Option<bool>; None → true (client default).
                ("prompt_suggestions", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.prompt_suggestions.unwrap_or(true),
                        "prompt_suggestions default drifts from UiConfig::default()"
                    );
                }
                ("keep_text_selection", SettingKind::Enum { default, .. }) => {
                    // The compile-time default is flash; the `word_select`
                    // default is a startup-applied remote rollout flag, not part
                    // of this static registry default.
                    let expected = if ui.keep_text_selection_enabled() {
                        "hold"
                    } else {
                        "flash"
                    };
                    assert_eq!(*default, expected);
                }
                // voice_keybind_enabled: Option<bool>; None → true.
                ("voice_keybind_enabled", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.voice_keybind_enabled.unwrap_or(true),
                        "voice_keybind_enabled default drifts from UiConfig::default()",
                    );
                }
                // voice_capture_mode: Option<String>; None → "hold".
                ("voice_capture_mode", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.voice_capture_mode, None,
                        "test assumes UiConfig::default().voice_capture_mode is None",
                    );
                    assert_eq!(
                        *default,
                        canonical_voice_capture_mode(ui.voice_capture_mode.as_deref()),
                        "voice_capture_mode default drifts from UiConfig::default()",
                    );
                }
                // voice_stt_language: Option<String>; None → "en".
                ("voice_stt_language", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.voice_stt_language, None,
                        "test assumes UiConfig::default().voice_stt_language is None",
                    );
                    assert_eq!(
                        *default,
                        canonical_voice_stt_language(ui.voice_stt_language.as_deref()),
                        "voice_stt_language default drifts from UiConfig::default()",
                    );
                }
                // hunk_tracker_mode: Option<String>; None → "off".
                ("hunk_tracker_mode", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.hunk_tracker_mode, None,
                        "test assumes UiConfig::default().hunk_tracker_mode is None",
                    );
                    assert_eq!(
                        *default,
                        canonical_hunk_tracker_mode(ui.hunk_tracker_mode.as_deref()),
                        "hunk_tracker_mode default drifts from UiConfig::default()",
                    );
                }
                ("screen_mode", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.screen_mode, None,
                        "test assumes UiConfig::default().screen_mode is None",
                    );
                    assert_eq!(
                        *default,
                        canonical_screen_mode(ui.screen_mode.as_deref()),
                        "screen_mode default drifts from UiConfig::default()",
                    );
                    assert_eq!(*default, "fullscreen");
                }
                // render_mermaid: Option<String>; None → "auto".
                ("render_mermaid", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.render_mermaid, None,
                        "test assumes UiConfig::default().render_mermaid is None",
                    );
                    let expected = ui
                        .render_mermaid
                        .as_deref()
                        .and_then(crate::appearance::RenderMermaid::from_canonical)
                        .map(|k| k.as_canonical())
                        .unwrap_or("auto");
                    assert_eq!(
                        *default, expected,
                        "render_mermaid default drifts from UiConfig::default()",
                    );
                }
                // scroll_speed: Option<u8>; None → 50.
                ("scroll_speed", SettingKind::Int { default, .. }) => {
                    assert_eq!(
                        *default,
                        ui.scroll_speed.unwrap_or(50) as i64,
                        "scroll_speed default drifts from UiConfig::default()"
                    );
                }
                // scroll_mode: Option<String>; None → "auto".
                ("scroll_mode", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.scroll_mode, None,
                        "test assumes UiConfig::default().scroll_mode is None",
                    );
                    assert_eq!(
                        *default,
                        crate::appearance::ScrollMode::default().as_canonical(),
                        "scroll_mode default drifts from UiConfig::default()",
                    );
                }
                // invert_scroll: Option<bool>; None → false.
                ("invert_scroll", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.invert_scroll.unwrap_or(false),
                        "invert_scroll default drifts from UiConfig::default()"
                    );
                }
                // display_refresh.auto_cadence_enabled: Option<bool>; None →
                // DISPLAY_REFRESH_DEFAULT_AUTO_CADENCE_ENABLED.
                ("display_refresh_auto_cadence", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default,
                        ui.display_refresh
                            .auto_cadence_enabled
                            .unwrap_or(DISPLAY_REFRESH_DEFAULT_AUTO_CADENCE_ENABLED),
                        "display_refresh_auto_cadence default drifts from resolve default"
                    );
                }
                // scroll_lines: Option<u8>; None → registry default 3 (the
                // display value while the per-terminal profile is in charge).
                ("scroll_lines", SettingKind::Int { default, .. }) => {
                    assert_eq!(
                        *default,
                        ui.scroll_lines.map(i64::from).unwrap_or(3),
                        "scroll_lines default drifts from UiConfig::default()"
                    );
                }
                // default_selected_permission: Option<String>; None →
                // "always_allow_all_sessions" (the effective default; first
                // prompt's cursor lands on the "Always allow on all sessions"
                // row, picked explicitly in `enqueue_permission`).
                ("default_selected_permission", SettingKind::Enum { default, .. }) => {
                    assert_eq!(
                        ui.default_selected_permission, None,
                        "test assumes UiConfig::default().default_selected_permission is None",
                    );
                    assert_eq!(
                        *default,
                        crate::appearance::permission_cursor::DefaultSelectedPermission::AlwaysAllowAllSessions
                            .as_canonical(),
                        "default_selected_permission registry default must be \
                         `always_allow_all_sessions` — the on-disk source of truth is \
                         `UiConfig::default_selected_permission: Option<String>` (defaults to \
                         None, mapped to the `always_allow_all_sessions` canonical)",
                    );
                }
                // fork_secondary_model: empty-string default = "no opinion".
                ("fork_secondary_model", SettingKind::DynamicEnum { default, .. }) => {
                    assert_eq!(
                        *default, "",
                        "fork_secondary_model registry default must be empty string — \
                         the live default is `crate::models::default_model()` and the \
                         current_value_for arm folds matching values to the empty sentinel",
                    );
                    // Cross-check: the UiConfig field IS the built-in default.
                    assert_eq!(
                        ui.fork_secondary_model,
                        echo_agent_runtime::models::default_model(),
                        "UiConfig::default().fork_secondary_model must equal \
                         models::default_model() — drift here breaks the empty-fold contract",
                    );
                }

                _ => panic!(
                    "settings::defs::default_settings() contains entry `{}` with no \
                     matching arm in defaults_match_ui_config_default. Add an arm.",
                    meta.key
                ),
            }
        }
    }

    /// Every PAGER-owned setting's default must match
    /// `PagerLocalSnapshot::default()`. Three-way alignment (registry
    /// → snapshot → `AgentView::new`) is enforced across multiple tests.
    #[test]
    fn defaults_match_pager_state() {
        let reg = SettingsRegistry::defaults();
        let pager = PagerLocalSnapshot::default();
        for meta in reg.all() {
            if meta.owner != SettingOwner::Pager {
                continue;
            }
            match (meta.key, &meta.kind) {
                ("multiline_mode", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default, pager.multiline_mode,
                        "multiline_mode default drifts from PagerLocalSnapshot::default()"
                    );
                }
                ("respect_manual_folds", SettingKind::Bool { default }) => {
                    assert_eq!(
                        *default, pager.respect_manual_folds,
                        "respect_manual_folds default drifts from PagerLocalSnapshot::default()"
                    );
                    assert_eq!(
                        *default,
                        crate::appearance::ScrollConfig::default().respect_manual_folds,
                        "respect_manual_folds default drifts from ScrollConfig::default() — \
                         the appearance config is the source of truth"
                    );
                }
                // plan_mode: per-session, not persisted.
                ("plan_mode", SettingKind::Enum { default, .. }) => {
                    let expected = if pager.plan_mode_active { "on" } else { "off" };
                    assert_eq!(
                        *default, expected,
                        "plan_mode default `{default}` drifts from PagerLocalSnapshot::default() \
                         (plan_mode_active: {})",
                        pager.plan_mode_active,
                    );
                }
                _ => panic!(
                    "settings::defs::default_settings() contains PAGER entry `{}` with no \
                     matching arm in defaults_match_pager_state. Add an arm.",
                    meta.key
                ),
            }
        }
    }

    /// Every registered setting must resolve to `Some(_)` with a
    /// variant matching its `SettingKind`. Catches registry/dispatch skew.
    #[test]
    fn every_setting_has_dispatch_arm() {
        let reg = SettingsRegistry::defaults();
        let ui = UiConfig::default();
        let pager = PagerLocalSnapshot::default();
        for meta in reg.all() {
            // Group rows carry no scalar value; they are not read via this path.
            if matches!(meta.kind, SettingKind::Group { .. }) {
                continue;
            }
            let value = current_value_for(meta.key, &ui, &pager).unwrap_or_else(|| {
                panic!(
                    "current_value_for(`{}`) returned None. Add a match arm.",
                    meta.key
                )
            });
            let kind_matches = matches!(
                (&meta.kind, &value),
                (SettingKind::Bool { .. }, SettingValue::Bool(_))
                    | (SettingKind::String { .. }, SettingValue::String(_))
                    | (SettingKind::Enum { .. }, SettingValue::Enum(_))
                    | (SettingKind::Int { .. }, SettingValue::Int(_))
                    // `DynamicEnum` uses `SettingValue::String`.
                    | (SettingKind::DynamicEnum { .. }, SettingValue::String(_))
            );
            assert!(
                kind_matches,
                "current_value_for(`{}`) returned {value:?} which doesn't match the \
                 registered SettingKind. Wrong variant in the match arm.",
                meta.key
            );
        }
    }

    /// Unregistered keys return `None`.
    #[test]
    fn unmapped_key_returns_none() {
        let ui = UiConfig::default();
        let pager = PagerLocalSnapshot::default();
        assert!(current_value_for("never-registered-key-xyzzy", &ui, &pager).is_none());
    }

    #[test]
    fn canonical_voice_capture_mode_maps_unknowns_to_hold() {
        assert_eq!(canonical_voice_capture_mode(Some("toggle")), "toggle");
        assert_eq!(canonical_voice_capture_mode(Some("hold")), "hold");
        // Case-insensitive + whitespace-tolerant.
        assert_eq!(canonical_voice_capture_mode(Some("  TOGGLE ")), "toggle");
        // Unknown / blank / None all fall back to the default `hold`.
        assert_eq!(canonical_voice_capture_mode(Some("hold_send")), "hold");
        assert_eq!(canonical_voice_capture_mode(Some("")), "hold");
        assert_eq!(canonical_voice_capture_mode(None), "hold");
    }

    /// With the UI key unset, `current_value_for` shows the live language
    /// (snapshot mirror of `voice_config.language` — e.g. an explicit
    /// `[voice].language`); a set UI key wins.
    #[test]
    fn voice_stt_language_current_value_falls_back_to_live_config() {
        let pager = PagerLocalSnapshot {
            voice_stt_language: "es".into(),
            ..Default::default()
        };
        let ui = UiConfig::default();
        assert_eq!(
            current_value_for("voice_stt_language", &ui, &pager),
            Some(SettingValue::Enum("es")),
        );
        let ui_set = UiConfig {
            voice_stt_language: Some("ja".into()),
            ..Default::default()
        };
        assert_eq!(
            current_value_for("voice_stt_language", &ui_set, &pager),
            Some(SettingValue::Enum("ja")),
        );
    }

    /// Spot-check the delegation to `echo_agent_voice::canonicalize_stt_language`
    /// (exhaustive alias/locale coverage lives in the voice crate's tests).
    #[test]
    fn canonical_voice_stt_language_delegates_to_voice_crate() {
        assert_eq!(canonical_voice_stt_language(Some("auto")), "auto");
        assert_eq!(canonical_voice_stt_language(Some("tl")), "fil");
        assert_eq!(canonical_voice_stt_language(None), "en");
    }

    /// Settings enum choices (minus client-only `auto`) must equal the voice
    /// crate's official STT catalog — prevents offering unsupported codes or
    /// omitting newly documented languages.
    #[test]
    fn voice_stt_language_settings_match_voice_crate_catalog() {
        use std::collections::HashSet;

        let reg = SettingsRegistry::defaults();
        let meta = reg
            .find("voice_stt_language")
            .expect("voice_stt_language must be registered");
        let SettingKind::Enum {
            choices, default, ..
        } = &meta.kind
        else {
            panic!("voice_stt_language must be Enum");
        };
        assert_eq!(*default, "en");

        let mut setting_codes: HashSet<&str> = HashSet::new();
        let mut saw_auto = false;
        for c in choices.iter() {
            if c.canonical == "auto" {
                saw_auto = true;
                assert_eq!(c.display, "System");
                continue;
            }
            assert!(
                setting_codes.insert(c.canonical),
                "duplicate settings language code {}",
                c.canonical
            );
            let lang = echo_agent_voice::stt_language_by_code(c.canonical)
                .unwrap_or_else(|| panic!("settings offers unsupported STT code {}", c.canonical));
            assert_eq!(
                c.display, lang.name,
                "display name for {} must match voice crate",
                c.canonical
            );
        }
        assert!(saw_auto, "settings must offer System (auto)");

        let crate_codes: HashSet<&str> = echo_agent_voice::STT_LANGUAGES
            .iter()
            .map(|l| l.code)
            .collect();
        assert_eq!(
            setting_codes, crate_codes,
            "settings concrete languages must match echo_agent_voice::STT_LANGUAGES exactly"
        );
    }

    #[test]
    fn canonical_hunk_tracker_mode_maps_aliases_and_unknowns() {
        assert_eq!(
            canonical_hunk_tracker_mode(Some("agent_only")),
            "agent_only"
        );
        assert_eq!(canonical_hunk_tracker_mode(Some("all_dirty")), "all_dirty");
        assert_eq!(canonical_hunk_tracker_mode(Some("off")), "off");
        // `disabled` is an accepted alias for `off`.
        assert_eq!(canonical_hunk_tracker_mode(Some("disabled")), "off");
        // Case-insensitive + whitespace-tolerant.
        assert_eq!(canonical_hunk_tracker_mode(Some("  OFF  ")), "off");
        assert_eq!(canonical_hunk_tracker_mode(Some("Disabled")), "off");
        assert_eq!(canonical_hunk_tracker_mode(Some("All_Dirty")), "all_dirty");
        assert_eq!(
            canonical_hunk_tracker_mode(Some("  Agent_Only  ")),
            "agent_only"
        );
        // Unknown / blank / absent → the `off` default.
        assert_eq!(canonical_hunk_tracker_mode(Some("bogus")), "off");
        assert_eq!(canonical_hunk_tracker_mode(Some("")), "off");
        assert_eq!(canonical_hunk_tracker_mode(None), "off");
    }

    #[test]
    fn canonical_screen_mode_maps_aliases_and_unknowns() {
        assert_eq!(canonical_screen_mode(Some("minimal")), "minimal");
        assert_eq!(canonical_screen_mode(Some("fullscreen")), "fullscreen");
        assert_eq!(canonical_screen_mode(Some("full")), "fullscreen");
        assert_eq!(canonical_screen_mode(Some("  MINIMAL ")), "minimal");
        assert_eq!(canonical_screen_mode(Some("default")), "fullscreen");
        assert_eq!(canonical_screen_mode(Some("auto")), "fullscreen");
        assert_eq!(canonical_screen_mode(Some("bogus")), "fullscreen");
        assert_eq!(canonical_screen_mode(Some("")), "fullscreen");
        assert_eq!(canonical_screen_mode(None), "fullscreen");
    }

    /// Corrupted `auto_dark_theme = "auto"` (would cause circular ref)
    /// falls back to canonical default.
    #[test]
    fn current_value_for_auto_dark_theme_filters_auto_value() {
        let ui = UiConfig {
            auto_dark_theme: Some("auto".into()),
            ..UiConfig::default()
        };
        let pager = PagerLocalSnapshot::default();
        let value = current_value_for("auto_dark_theme", &ui, &pager).expect("must resolve");
        assert_eq!(
            value,
            SettingValue::Enum("echo-agent-night"),
            "corrupted `auto_dark_theme = \"auto\"` must fall back to canonical default",
        );
    }

    #[test]
    fn current_value_for_auto_light_theme_filters_auto_value() {
        let ui = UiConfig {
            auto_light_theme: Some("auto".into()),
            ..UiConfig::default()
        };
        let pager = PagerLocalSnapshot::default();
        let value = current_value_for("auto_light_theme", &ui, &pager).expect("must resolve");
        assert_eq!(
            value,
            SettingValue::Enum("echo-agent-day"),
            "corrupted `auto_light_theme = \"auto\"` must fall back to canonical default",
        );
    }

    /// Unknown values in `auto_dark_theme` fall back to default.
    #[test]
    fn current_value_for_auto_dark_theme_unknown_falls_back() {
        let ui = UiConfig {
            auto_dark_theme: Some("nonexistent-theme".into()),
            ..UiConfig::default()
        };
        let pager = PagerLocalSnapshot::default();
        let value = current_value_for("auto_dark_theme", &ui, &pager).expect("must resolve");
        assert_eq!(value, SettingValue::Enum("echo-agent-night"));
    }

    /// The persisted `fork_secondary_model` slug resolves to the catalog
    /// display name (matching the `default_model` row and the DynamicEnum
    /// picker canonicals); the baseline still folds to the empty sentinel
    /// and a slug missing from the catalog passes through raw.
    #[test]
    fn fork_secondary_model_current_value_resolves_display_name() {
        let slug = "echo-agent-4.5-fast";
        assert_ne!(
            slug,
            echo_agent_runtime::models::default_model(),
            "test slug must differ from the baseline or the empty-fold arm masks the lookup",
        );
        let pager = PagerLocalSnapshot {
            available_models: vec![(
                "EchoAgent 4.5 Fast".to_string(),
                acp::ModelId::new(std::sync::Arc::from(slug)),
            )],
            ..Default::default()
        };
        let ui = UiConfig {
            fork_secondary_model: slug.to_string(),
            ..Default::default()
        };
        assert_eq!(
            current_value_for("fork_secondary_model", &ui, &pager),
            Some(SettingValue::String("EchoAgent 4.5 Fast".to_string())),
        );

        // Baseline folds to the empty "no override" sentinel.
        assert_eq!(
            current_value_for("fork_secondary_model", &UiConfig::default(), &pager),
            Some(SettingValue::String(String::new())),
        );

        // Stale slug (not in the catalog) passes through unresolved.
        let stale_ui = UiConfig {
            fork_secondary_model: "retired-model".to_string(),
            ..Default::default()
        };
        assert_eq!(
            current_value_for("fork_secondary_model", &stale_ui, &pager),
            Some(SettingValue::String("retired-model".to_string())),
        );
    }

    /// Keywords must be lowercase and non-empty.
    #[test]
    fn keywords_lowercase_and_non_empty() {
        let reg = SettingsRegistry::defaults();
        for meta in reg.all() {
            for kw in meta.keywords {
                assert!(
                    !kw.is_empty(),
                    "setting `{}` has an empty keyword",
                    meta.key
                );
                assert_eq!(
                    *kw,
                    kw.to_lowercase(),
                    "setting `{}` keyword `{kw}` is not lowercase",
                    meta.key
                );
            }
        }
    }

    /// Keys are globally unique (Vec preserves declaration order).
    #[test]
    fn unique_keys() {
        use std::collections::HashMap;
        let reg = SettingsRegistry::defaults();
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for m in reg.all() {
            *counts.entry(m.key).or_insert(0) += 1;
        }
        let dupes: Vec<&&str> = counts
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(k, _)| k)
            .collect();
        assert!(
            dupes.is_empty(),
            "duplicate setting keys in default_settings(): {dupes:?}",
        );
    }

    /// `from_entries` panics on duplicate keys.
    #[test]
    #[should_panic(expected = "duplicate setting keys in registry")]
    fn from_entries_panics_on_duplicate_keys() {
        let dup_meta = SettingMeta {
            key: "synthetic_int",
            category: SettingCategory::Advanced,
            owner: SettingOwner::Shared,
            label: "Synthetic Int",
            description: "Test fixture.",
            keywords: &["test"],
            kind: SettingKind::Int {
                default: 50,
                min: 0,
                max: 200,
            },
            restart_required: false,
            hidden_in_minimal: false,
        };
        // Same key registered twice → panic.
        let _ = SettingsRegistry::from_entries(vec![dup_meta.clone(), dup_meta]);
    }

    /// Search is a literal substring multi-word AND match.
    #[test]
    fn search_multi_word_and() {
        let reg = SettingsRegistry::defaults();
        let hits = reg.search("compact density");
        assert_eq!(hits.len(), 1, "expected 1 match for 'compact density'");
        assert_eq!(hits[0].key, "compact_mode");

        let empty = reg.search("xyzzy-no-match");
        assert!(empty.is_empty(), "expected no match for 'xyzzy-no-match'");

        // Empty query returns everything.
        let all = reg.search("");
        assert_eq!(all.len(), reg.all().len());
    }

    /// `default_value_for` returns the registered default verbatim.
    #[test]
    fn default_value_for_returns_kind_default() {
        let reg = SettingsRegistry::defaults();
        for meta in reg.all() {
            // Group rows have no meaningful scalar default (sentinel only).
            if matches!(meta.kind, SettingKind::Group { .. }) {
                continue;
            }
            let v = default_value_for(meta);
            match (&meta.kind, &v) {
                (SettingKind::Bool { default }, SettingValue::Bool(b)) => assert_eq!(b, default),
                (SettingKind::String { default, .. }, SettingValue::String(s)) => {
                    assert_eq!(s, default);
                }
                (SettingKind::Enum { default, .. }, SettingValue::Enum(e)) => {
                    assert_eq!(e, default);
                }
                (SettingKind::Int { default, .. }, SettingValue::Int(i)) => {
                    assert_eq!(i, default);
                }
                // `DynamicEnum` widens to `String`.
                (SettingKind::DynamicEnum { default, .. }, SettingValue::String(s)) => {
                    assert_eq!(s, default);
                }
                _ => panic!("default_value_for kind mismatch for `{}`", meta.key),
            }
        }
    }

    #[test]
    fn category_label_round_trip() {
        // Sanity check on the canonical labels (used as section headers
        // in the modal and as filter keywords).
        for cat in SettingCategory::ALL {
            let label = cat.label();
            assert!(!label.is_empty(), "category label cannot be empty");
        }
    }

    /// The contextual-hints group is registered with its child Bool settings,
    /// each defaulting ON, and the children read from `[ui.contextual_hints]`
    /// (None inherits → ON; a user opt-out flips it).
    #[test]
    fn contextual_hints_group_and_children_registered() {
        let reg = SettingsRegistry::defaults();
        let group = reg.find("contextual_hints").expect("group registered");
        let SettingKind::Group { children } = &group.kind else {
            panic!("contextual_hints must be a Group");
        };
        assert_eq!(
            *children,
            &[
                "contextual_hints.undo",
                "contextual_hints.plan_mode",
                "contextual_hints.image_input",
                "contextual_hints.send_now",
                "contextual_hints.small_screen",
                "contextual_hints.word_select",
                "contextual_hints.ssh_wrap",
            ],
        );
        for &key in *children {
            let child = reg
                .find(key)
                .unwrap_or_else(|| panic!("child `{key}` must be registered"));
            assert!(
                matches!(child.kind, SettingKind::Bool { default: true }),
                "child `{key}` must be a Bool defaulting ON",
            );
        }

        let pager = PagerLocalSnapshot::default();
        // Default (inherit) reads as ON.
        assert_eq!(
            current_value_for("contextual_hints.undo", &UiConfig::default(), &pager),
            Some(SettingValue::Bool(true)),
        );
        // A user opt-out flips the read for that tip only.
        let ui = UiConfig {
            contextual_hints: echo_agent_runtime::agent::config::ContextualHints {
                undo: Some(false),
                ..Default::default()
            },
            ..UiConfig::default()
        };
        assert_eq!(
            current_value_for("contextual_hints.undo", &ui, &pager),
            Some(SettingValue::Bool(false)),
        );
        assert_eq!(
            current_value_for("contextual_hints.plan_mode", &ui, &pager),
            Some(SettingValue::Bool(true)),
        );
    }

    /// The `compact_mode` description's row count must track the auto-compact
    /// threshold (`AUTO_COMPACT_MAX_ROWS`). `description` is a static string,
    /// so a threshold move would otherwise silently lie in the settings copy —
    /// same drift class the default-drift assertions above guard.
    #[test]
    fn compact_mode_description_tracks_auto_compact_threshold() {
        let reg = SettingsRegistry::defaults();
        let meta = reg.find("compact_mode").expect("compact_mode registered");
        let expected = format!("{} rows", crate::views::agent::AUTO_COMPACT_MAX_ROWS);
        assert!(
            meta.description.contains(&expected),
            "compact_mode description must mention the `{expected}` auto-compact \
             threshold; got {:?}",
            meta.description
        );
    }

    /// Every static Enum's choice count must stay within `MAX_PICKER_CHOICES`
    /// (product guard; the chooser already scrolls within the viewport).
    #[test]
    fn enum_choice_counts_are_bounded() {
        let reg = SettingsRegistry::defaults();
        for meta in reg.all() {
            let SettingKind::Enum { choices, .. } = &meta.kind else {
                continue;
            };
            assert!(
                choices.len() <= crate::views::settings_modal::MAX_PICKER_CHOICES,
                "Enum setting `{}` has {} choices, exceeds MAX_PICKER_CHOICES \
                 ({}). Raise the cap deliberately for a larger curated catalog \
                 (picker already scrolls).",
                meta.key,
                choices.len(),
                crate::views::settings_modal::MAX_PICKER_CHOICES,
            );
        }
    }
}
