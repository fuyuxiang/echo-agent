use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

use super::input::*;
use super::render::*;
use super::state::*;
use crate::app::actions::Action;
use crate::input::line_editor::LineEditor;
use crate::settings::{
    CodingDataSharingLock, EnumChoice, PagerLocalSnapshot, SettingCategory, SettingKey,
    SettingKind, SettingMeta, SettingOwner, SettingValue, SettingsRegistry, StringValidator,
};
use crate::theme::Theme;
use echo_agent_runtime::agent::config::UiConfig;

fn make_state() -> SettingsModalState {
    SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    )
}

/// The contextual-hints group renders as a single top-level row (children
/// hidden); Enter opens the sub-sheet, Space there toggles the focused
/// child via the typed action, and Esc returns to Browse.
#[test]
fn contextual_hints_group_sub_sheet_flow() {
    let mut s = make_state();
    // Group row present; child rows hidden from the top-level list.
    let group_idx = s
        .rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key, .. } if *key == "contextual_hints"))
        .expect("group row present");
    assert!(
        !s.rows.iter().any(|r| matches!(
            r,
            RowEntry::Setting { key, .. } if key.starts_with("contextual_hints.")
        )),
        "child rows must be hidden from the top-level list",
    );

    // Focus the group, Enter → PickingGroup on the first child.
    s.selected = group_idx;
    let out = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(out, SettingsKeyOutcome::Changed));
    assert!(matches!(
        s.mode(),
        SettingsModalMode::PickingGroup { child_idx: 0, .. }
    ));

    // Space toggles the focused child (undo defaults ON → set false).
    let out = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
    );
    assert!(
        matches!(
            out,
            SettingsKeyOutcome::Action(Action::SetContextualHintUndo(false))
        ),
        "Space must toggle the focused child via the typed action, got {out:?}",
    );

    // Esc returns to Browse.
    let out = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(out, SettingsKeyOutcome::Changed));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

/// The permission_mode picker hides the "Auto" choice when the auto feature
/// gate is off (matching the Shift+Tab cycle, which skips Auto when gated),
/// and shows it when the gate is on. Other choices are unaffected.
#[test]
fn effective_enum_choices_hides_auto_for_permission_mode_when_gated_off() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("permission_mode")
        .expect("permission_mode registered");
    let SettingKind::Enum { choices, .. } = &meta.kind else {
        panic!("permission_mode must be Enum");
    };

    let gated_off = PagerLocalSnapshot {
        auto_mode_gate: false,
        ..PagerLocalSnapshot::default()
    };
    let filtered = effective_enum_choices("permission_mode", choices, &gated_off);
    assert!(
        !filtered.iter().any(|c| c.canonical == "auto"),
        "Auto must be hidden from the permission_mode picker when the gate is off"
    );
    assert!(
        filtered.iter().any(|c| c.canonical == "ask"),
        "non-Auto choices must remain"
    );

    let gated_on = PagerLocalSnapshot {
        auto_mode_gate: true,
        ..PagerLocalSnapshot::default()
    };
    let full = effective_enum_choices("permission_mode", choices, &gated_on);
    assert!(
        full.iter().any(|c| c.canonical == "auto"),
        "Auto must be selectable when the gate is on"
    );

    // A non-gated key is never filtered.
    let theme = reg.find("theme").expect("theme registered");
    if let SettingKind::Enum {
        choices: theme_choices,
        ..
    } = &theme.kind
    {
        assert_eq!(
            effective_enum_choices("theme", theme_choices, &gated_off).len(),
            theme_choices.len(),
            "non-permission_mode keys are never filtered"
        );
    }
}

/// `voice_capture_mode`'s "hold" choice is gated off without key releases and
/// available with them; "toggle" is never gated. Permission_mode's "auto"
/// gating is preserved. Pure — no process-global mutation.
#[test]
fn enum_choice_gated_off_covers_voice_and_permission() {
    // voice "hold": gated iff no key releases.
    assert!(enum_choice_gated_off(
        "voice_capture_mode",
        "hold",
        true,
        false
    ));
    assert!(!enum_choice_gated_off(
        "voice_capture_mode",
        "hold",
        true,
        true
    ));
    // voice "toggle": never gated.
    assert!(!enum_choice_gated_off(
        "voice_capture_mode",
        "toggle",
        true,
        false
    ));
    // permission_mode "auto": gated iff the auto gate is off.
    assert!(enum_choice_gated_off(
        "permission_mode",
        "auto",
        false,
        true
    ));
    assert!(!enum_choice_gated_off(
        "permission_mode",
        "auto",
        true,
        true
    ));
}

/// Look up a setting's registered metadata by key (test helper).
fn meta_for(reg: &SettingsRegistry, key: SettingKey) -> &SettingMeta {
    reg.all()
        .iter()
        .find(|m| m.key == key)
        .unwrap_or_else(|| panic!("`{key}` not registered"))
}

/// The whole `voice_capture_mode` row is hidden without key-release reporting
/// (only `toggle` is possible, so there's no choice) and shown with it.
/// Other settings are always visible. Pure — no process-global mutation.
#[test]
fn setting_row_visible_gates_voice_capture_on_key_releases() {
    let reg = SettingsRegistry::defaults();
    let voice = meta_for(&reg, "voice_capture_mode");
    let vim = meta_for(&reg, "vim_mode");
    // voice_mode = true; kitty_releases varies.
    assert!(!setting_row_visible(voice, false, false, true));
    assert!(setting_row_visible(voice, true, false, true));
    assert!(setting_row_visible(vim, false, false, true));
}

#[test]
fn setting_row_visible_hides_voice_rows_when_voice_mode_off() {
    let reg = SettingsRegistry::defaults();
    let keybind = meta_for(&reg, "voice_keybind_enabled");
    let capture = meta_for(&reg, "voice_capture_mode");
    let language = meta_for(&reg, "voice_stt_language");
    let vim = meta_for(&reg, "vim_mode");
    // Gate off: all voice rows gone even with kitty releases + full TUI.
    assert!(!setting_row_visible(keybind, true, false, false));
    assert!(!setting_row_visible(capture, true, false, false));
    assert!(!setting_row_visible(language, true, false, false));
    // Non-voice rows unaffected.
    assert!(setting_row_visible(vim, true, false, false));
    // Gate on: all visible (kitty releases for capture).
    assert!(setting_row_visible(keybind, true, false, true));
    assert!(setting_row_visible(capture, true, false, true));
    assert!(setting_row_visible(language, true, false, true));
    // The keybind row (unlike capture) doesn't need key-release reporting.
    assert!(setting_row_visible(keybind, false, false, true));
}

#[test]
fn rebuild_rows_drops_voice_settings_when_gate_turns_off() {
    let prev = crate::app::voice_mode_enabled();
    crate::app::set_voice_mode_enabled_for_test(true);
    let mut state = make_state();
    let has_voice_lang = |s: &SettingsModalState| {
        s.rows.iter().any(|r| {
            matches!(
                r,
                RowEntry::Setting {
                    key: "voice_stt_language",
                    ..
                }
            )
        })
    };
    assert!(
        has_voice_lang(&state),
        "voice_stt_language should be listed with gate on"
    );

    crate::app::set_voice_mode_enabled_for_test(false);
    state.rebuild_rows();
    assert!(
        !has_voice_lang(&state),
        "rebuild after gate off must hide voice_stt_language"
    );
    crate::app::set_voice_mode_enabled_for_test(prev);
}

#[test]
fn setting_row_visible_hides_theme_rows_in_minimal() {
    let reg = SettingsRegistry::defaults();
    for key in [
        "theme",
        "auto_dark_theme",
        "auto_light_theme",
        "display_refresh_auto_cadence",
    ] {
        let meta = meta_for(&reg, key);
        assert!(meta.hidden_in_minimal, "{key} must declare the flag");
        assert!(
            !setting_row_visible(meta, true, true, true),
            "{key} in minimal"
        );
        assert!(
            setting_row_visible(meta, true, false, true),
            "{key} in full TUI"
        );
    }
    assert!(setting_row_visible(
        meta_for(&reg, "vim_mode"),
        true,
        true,
        true
    ));
}

/// `action_for_bool` mirrors `current_value_for`: every registered
/// Bool setting must have an arm here too. Without this test, a
/// future PR could register a Bool setting that the modal can read
/// (via `current_value_for`) but not toggle (because `action_for_bool`
/// silently returns `None`).
#[test]
fn every_setting_has_action_for_bool_arm() {
    let reg = SettingsRegistry::defaults();
    for meta in reg.all() {
        if !matches!(meta.kind, SettingKind::Bool { .. }) {
            continue;
        }
        assert!(
            action_for_bool(meta.key, true).is_some(),
            "Bool setting `{}` has no action_for_bool arm — \
             modal would toggle silently. Add an arm in views/settings_modal.rs::action_for_bool.",
            meta.key,
        );
        assert!(
            action_for_bool(meta.key, false).is_some(),
            "action_for_bool(`{}`, false) returned None",
            meta.key,
        );
    }
}

/// Mirror of `every_setting_has_action_for_bool_arm` for Enum
/// settings: every preview-supporting registered Enum + every one
/// of its canonical choices must have a matching `action_for_enum`
/// arm. Without this guard, a change could register `theme` in
/// `default_settings()` and forget to add the
/// `"theme" => Some(Action::SetTheme(...))` arm — the picker
/// would silently degrade (nav advances, no Action emitted,
/// preview never fires).
///
/// Non-preview Enums (e.g.
/// `permission_mode` where `supports_preview: false`) are
/// excluded from this check — their picker nav skips
/// `action_for_enum` entirely (gated by `supports_preview` in
/// `set_picker_idx`), so returning `None` is the correct
/// behaviour and would otherwise look like a missing arm. The
/// commit-arm test below covers them.
///
/// With no Enum entries registered the loop body never
/// executes and the check passes vacuously; registering an Enum
/// forces the matching arm in `action_for_enum` to land alongside it.
#[test]
fn every_preview_enum_setting_has_action_for_enum_arm() {
    let reg = SettingsRegistry::defaults();
    for meta in reg.all() {
        let SettingKind::Enum {
            choices,
            supports_preview,
            ..
        } = &meta.kind
        else {
            continue;
        };
        if !*supports_preview {
            continue;
        }
        for c in *choices {
            assert!(
                action_for_enum(meta.key, c.canonical).is_some(),
                "Preview-enabled Enum setting `{}` choice `{}` has no action_for_enum arm — \
                 picker would silently degrade (no preview/revert Action emitted). \
                 Add an arm in views/settings_modal.rs::action_for_enum.",
                meta.key,
                c.canonical,
            );
        }
    }
}

/// Mirror of `every_setting_has_action_for_bool_arm` for
/// `SettingKind::String`. Every registered String setting must
/// have a matching arm in `action_for_string`, otherwise the
/// editor's commit path returns `None` and the Enter keystroke
/// silently no-ops (after exiting EditingValue mode).
///
/// The empty-string path is
/// the canonical "clear default" entry-point — every
/// registered String setting must produce SOME action for
/// empty input (even if it's `ClearDefaultModel` rather than a
/// `SetX` variant).
///
/// **Vacuous-passing note**: today no
/// production setting uses `SettingKind::String` — both
/// `default_model` and `fork_secondary_model` use
/// `DynamicEnum`, and `coding_data_sharing` / `permission_mode`
/// / `plan_mode` are `Enum`. The loop body skips every meta, so
/// this assertion passes vacuously today. It STILL fires as a
/// CI guard the first time a future change registers a String
/// setting without an action arm. Renamed in spirit to
/// `if_a_string_setting_is_added_it_has_an_action_arm` via the
/// panic message; the test fn name keeps `every_*` for
/// consistency with the sibling guards.
#[test]
fn every_string_setting_has_action_for_string_arm() {
    let reg = SettingsRegistry::defaults();
    let snapshot = PagerLocalSnapshot::default();
    for meta in reg.all() {
        if !matches!(meta.kind, SettingKind::String { .. }) {
            continue;
        }
        // Empty buffer must produce some action (clear or
        // similar) — not None.
        assert!(
            action_for_string(meta.key, String::new(), &snapshot).is_some(),
            "If you just added a String setting, it has no \
             action_for_string arm for empty input — editor would \
             silently degrade on the user's first Enter. Add an arm \
             in views/settings_modal.rs::action_for_string for `{}`.",
            meta.key,
        );
    }
}

/// Every registered
/// `SettingKind::DynamicEnum` setting must have a matching arm
/// in `action_for_string` for the picker's Enter (commit) path,
/// including the empty-canonical sentinel (row 0 of the picker
/// is always "(no override)").
#[test]
fn every_dynamic_enum_setting_has_action_for_string_arm() {
    let reg = SettingsRegistry::defaults();
    // Seed a synthetic catalog so the resolver path can produce
    // a non-empty SetX action — empty-only would mask a missing
    // SetX arm.
    use agent_client_protocol as acp;
    use std::sync::Arc;
    let snapshot = PagerLocalSnapshot {
        available_models: vec![(
            "Test Model".to_string(),
            acp::ModelId::new(Arc::from("test-model")),
        )],
        ..PagerLocalSnapshot::default()
    };
    for meta in reg.all() {
        if !matches!(meta.kind, SettingKind::DynamicEnum { .. }) {
            continue;
        }
        // Discriminate on the Action variant, not
        // just `is_some()`. A future refactor that swallowed the
        // typed `SetDefaultModel` / `SetForkSecondaryModel` into
        // a generic `Action::DynamicSettingChanged(...)` would
        // pass `is_some()` while breaking the typed dispatch.
        let empty_action = action_for_string(meta.key, String::new(), &snapshot);
        let nonempty_action = action_for_string(meta.key, "Test Model".to_string(), &snapshot);
        match meta.key {
            "default_model" => {
                assert!(
                    matches!(empty_action, Some(Action::ClearDefaultModel)),
                    "default_model empty canonical must produce ClearDefaultModel, \
                     got {empty_action:?}",
                );
                assert!(
                    matches!(nonempty_action, Some(Action::SetDefaultModel(_))),
                    "default_model non-empty canonical must produce \
                     SetDefaultModel(_), got {nonempty_action:?}",
                );
            }
            "fork_secondary_model" => {
                assert!(
                    matches!(empty_action, Some(Action::ClearForkSecondaryModel)),
                    "fork_secondary_model empty canonical must produce \
                     ClearForkSecondaryModel, got {empty_action:?}",
                );
                assert!(
                    matches!(nonempty_action, Some(Action::SetForkSecondaryModel(_))),
                    "fork_secondary_model non-empty canonical must produce \
                     SetForkSecondaryModel(_), got {nonempty_action:?}",
                );
            }
            other => panic!(
                "Unknown DynamicEnum key `{other}` — add a discriminating arm in \
                 every_dynamic_enum_setting_has_action_for_string_arm so future \
                 additions can't silently rely on the generic is_some() check.",
            ),
        }
    }
}

/// Mirror of `every_setting_has_action_for_bool_arm` for
/// `SettingKind::Int`. Every registered Int setting must have a
/// matching arm in `action_for_int`.
#[test]
fn every_int_setting_has_action_for_int_arm() {
    let reg = SettingsRegistry::defaults();
    for meta in reg.all() {
        if !matches!(meta.kind, SettingKind::Int { .. }) {
            continue;
        }
        assert!(
            action_for_int(meta.key, 0).is_some(),
            "Int setting `{}` has no action_for_int arm — \
             editor would silently degrade. Add an arm in \
             views/settings_modal.rs::action_for_int.",
            meta.key,
        );
    }
}

/// Mirror of `every_preview_enum_setting_has_action_for_enum_arm`
/// for the COMMIT path: every registered Enum + every canonical
/// choice must have a matching `action_for_enum_commit` arm.
/// Unlike the preview arm, this applies to ALL Enums (preview
/// and non-preview) — Enter (commit) is the structural mutation
/// path regardless of preview support.
///
/// `permission_mode` has
/// `supports_preview: false`; this test ensures its commit arm
/// is wired.
#[test]
fn every_enum_setting_has_action_for_enum_commit_arm() {
    let reg = SettingsRegistry::defaults();
    for meta in reg.all() {
        let SettingKind::Enum { choices, .. } = &meta.kind else {
            continue;
        };
        for c in *choices {
            assert!(
                action_for_enum_commit(meta.key, c.canonical).is_some(),
                "Enum setting `{}` choice `{}` has no action_for_enum_commit arm — \
                 Enter on the picker would no-op silently. Add an arm in \
                 views/settings_modal.rs::action_for_enum_commit.",
                meta.key,
                c.canonical,
            );
        }
    }
}

/// The previous behaviour truncated labels
/// at `max_label_w` regardless of available area width. The new
/// behaviour prefers to render the FULL label whenever the row's
/// label + value will fit on one logical line; truncation is
/// reserved for the pathological case where even the label alone
/// is wider than the row (covered by
/// `pathologically_narrow_truncates_label_with_ellipsis`).
///
/// At an 80-col area, a 42-col label easily fits on one line so
/// the full label renders without an ellipsis — the regression
/// the user reported is gone.
#[test]
fn render_setting_row_shows_full_label_when_one_line_fits() {
    let meta = SettingMeta {
        key: "test-key",
        category: SettingCategory::Appearance,
        owner: crate::settings::SettingOwner::Shared,
        label: "A very long label that exceeds the budget",
        description: "Test description.",
        keywords: &["test"],
        kind: SettingKind::Bool { default: false },
        restart_required: false,
        hidden_in_minimal: false,
    };
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_setting_row(
        &mut buf,
        area,
        &meta,
        &SettingValue::Bool(false),
        15, // max_label_w — kept for API compatibility, no longer used.
        false,
        &theme,
        false, // is_expanded
        false, // is_hovered
        None,
    );
    let mut rendered = String::new();
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, 0)) {
            rendered.push_str(cell.symbol());
        }
    }
    assert!(
        !rendered.contains('\u{2026}'),
        "Commit 13: a row whose label + value fits on one line \
         should NOT show a truncation ellipsis: {rendered:?}"
    );
    assert!(
        rendered.contains("A very long label that exceeds the budget"),
        "full label must be visible when one-line layout fits: {rendered:?}"
    );
}

/// The default registry contains Appearance settings
/// (3 bools + 3 enums + 1 int = 7 entries), the Editor entry
/// `multiline_mode`, the Agent entries `permission_mode` and
/// `plan_mode`, the Privacy entry `coding_data_sharing`, the
/// Models entry `default_model`, and the Advanced entries
/// `show_tips` and `auto_update`. `default_reasoning_effort` and
/// `auto_compact_threshold_percent` are not exposed in the modal.
#[test]
fn rows_contain_categories_and_settings_through_pr_14() {
    let prev_voice = crate::app::voice_mode_enabled();
    crate::app::set_voice_mode_enabled_for_test(false);
    let s = make_state();
    let headers: Vec<&SettingCategory> = s
        .rows
        .iter()
        .filter_map(|r| {
            if let RowEntry::Header { category } = r {
                Some(category)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        headers,
        vec![
            &SettingCategory::Appearance,
            &SettingCategory::Mouse,
            &SettingCategory::Editor,
            &SettingCategory::Agent,
            &SettingCategory::Privacy,
            &SettingCategory::Models,
            // The Session category has no registered settings, so its
            // header is not emitted.
            // Advanced category (first entries:
            // `show_tips`, `auto_update`).
            &SettingCategory::Advanced,
        ]
    );

    let settings: Vec<SettingKey> = s
        .rows
        .iter()
        .filter_map(|r| {
            if let RowEntry::Setting { key, .. } = r {
                Some(*key)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        settings,
        vec![
            // Booleans.
            "compact_mode",
            "screen_mode",
            "show_timestamps",
            "show_timeline",
            // PAGER-owned page_flip_on_send (Appearance).
            "page_flip_on_send",
            "simple_mode",
            // PAGER-owned vim_mode (Appearance,
            // paired with simple_mode).
            "vim_mode",
            // Theme enums.
            "theme",
            "auto_dark_theme",
            "auto_light_theme",
            // SHELL-owned render_mermaid (Appearance,
            // declared after the theme enums).
            "render_mermaid",
            // Int in Appearance category.
            "max_thoughts_width",
            // SHELL-owned show_thinking_blocks (Appearance; live cache).
            "show_thinking_blocks",
            // PAGER-owned respect_manual_folds (Appearance,
            // persisted to pager.toml).
            "respect_manual_folds",
            // SHELL-owned group_tool_verbs (Appearance; live cache).
            "group_tool_verbs",
            // SHELL-owned collapsed_edit_blocks (Appearance; live cache,
            // default OFF rollout flag).
            "collapsed_edit_blocks",
            // SHELL-owned display_refresh_auto_cadence (Appearance).
            "display_refresh_auto_cadence",
            // Mouse — scroll + drag selection. The scroll
            // classification/lines/direction knobs follow scroll_speed.
            "scroll_speed",
            "scroll_mode",
            "scroll_lines",
            "invert_scroll",
            "keep_text_selection",
            // SHARED-owned combine_queued_prompts (Editor category; read by
            // both the pager drain and the shell promote. Registered before
            // multiline_mode, so it renders first).
            "combine_queued_prompts",
            "follow_up_behavior",
            "confirm_before_rewind",
            // PAGER-owned multiline (Editor category).
            "multiline_mode",
            // SHELL-owned prompt_suggestions (Editor; tab autocomplete
            // ghost text, live cache).
            "prompt_suggestions",
            // voice_keybind_enabled + voice_capture_mode + voice_stt_language
            // hidden when the voice gate is off.
            // SHELL-owned permission_mode (Agent category).
            "permission_mode",
            // SHELL-owned remember_tool_approvals (Agent category,
            // registered right after permission_mode).
            "remember_tool_approvals",
            // SHELL-owned default_selected_permission (Agent category,
            // colocated with permission_mode / plan_mode).
            "default_selected_permission",
            // SHELL-owned ask_user_question timeout (Agent category,
            // registered directly above plan_mode).
            "toolset.ask_user_question.timeout_enabled",
            // PAGER-owned plan_mode (Agent category).
            "plan_mode",
            // SHELL-owned coding_data_sharing (Privacy category).
            "coding_data_sharing",
            // SHELL-owned default_model (Models category).
            "default_model",
            // Models category. `default_reasoning_effort`,
            // `web_search_model`, and `session_summary_model` are
            // not exposed in the modal.
            "fork_secondary_model",
            // `auto_compact_threshold_percent` (Session category) is
            // not exposed in the modal.
            // Advanced category.
            "show_tips",
            // Per-tip contextual-hints GROUP row, repositioned right after
            // `show_tips`. Its 3 child toggles
            // (`contextual_hints.{undo,plan_mode,image_input}`) are hidden
            // from the top-level list and reached via the sub-sheet.
            "contextual_hints",
            "auto_update",
            // SHELL-owned hunk_tracker_mode (Advanced; `off` disables it).
            "hunk_tracker_mode",
        ]
    );
    crate::app::set_voice_mode_enabled_for_test(prev_voice);
}

#[test]
fn initial_selection_skips_header() {
    let s = make_state();
    match &s.rows[s.selected] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, "compact_mode"),
        RowEntry::Header { .. } => panic!("selection landed on a header"),
    }
}

/// `j` advances through the row list one selectable entry at a
/// time and is a no-op at the last visible setting.
///
/// Resilient to future setting additions: the test walks `j` to the
/// end of the registry dynamically rather than hardcoding row
/// counts.
#[test]
fn j_advances_past_setting_rows() {
    let mut s = make_state();
    let key1 = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
    // The initial selection is the first selectable row.
    let setting_keys: Vec<SettingKey> = s
        .rows
        .iter()
        .filter_map(|r| {
            if let RowEntry::Setting { key, .. } = r {
                Some(*key)
            } else {
                None
            }
        })
        .collect();
    assert!(setting_keys.len() >= 2, "test requires at least 2 settings");
    // Walk j to the last setting; each step must be Changed.
    for expected in setting_keys.iter().skip(1) {
        assert!(matches!(
            handle_settings_key(&mut s, &key1),
            SettingsKeyOutcome::Changed
        ));
        match &s.rows[s.selected] {
            RowEntry::Setting { key, .. } => assert_eq!(*key, *expected),
            _ => panic!("expected setting row after j"),
        }
    }
    // At the last row, j is a no-op.
    assert!(matches!(
        handle_settings_key(&mut s, &key1),
        SettingsKeyOutcome::Unchanged
    ));
}

#[test]
fn space_on_compact_mode_dispatches_set_compact_mode_true() {
    let mut s = make_state();
    // Default compact_mode is false → Space dispatches Set(true).
    let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
    let outcome = handle_settings_key(&mut s, &space);
    match outcome {
        SettingsKeyOutcome::Action(Action::SetCompactMode(true)) => {}
        other => panic!("expected SetCompactMode(true), got {other:?}"),
    }
}

#[test]
fn enter_on_compact_mode_also_toggles() {
    let mut s = make_state();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let outcome = handle_settings_key(&mut s, &enter);
    match outcome {
        SettingsKeyOutcome::Action(Action::SetCompactMode(true)) => {}
        other => panic!("expected SetCompactMode(true) from Enter, got {other:?}"),
    }
}

#[test]
fn f2_closes_modal() {
    let mut s = make_state();
    let f2 = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
    assert!(matches!(
        handle_settings_key(&mut s, &f2),
        SettingsKeyOutcome::Close
    ));
}

#[test]
fn esc_in_browse_mode_falls_through_to_chrome() {
    // Esc is intercepted UPSTREAM by `ModalWindow::handle_modal_key`;
    // `handle_settings_key` does not match Esc anymore. See module
    // docstring.
    let mut s = make_state();
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(
        handle_settings_key(&mut s, &esc),
        SettingsKeyOutcome::Unchanged
    ));
}

#[test]
fn ctrl_comma_closes_modal() {
    let mut s = make_state();
    let key = KeyEvent::new(KeyCode::Char(','), KeyModifiers::CONTROL);
    assert!(matches!(
        handle_settings_key(&mut s, &key),
        SettingsKeyOutcome::Close
    ));
}

#[test]
fn cmd_comma_closes_modal_on_macos() {
    let mut s = make_state();
    let key = KeyEvent::new(KeyCode::Char(','), KeyModifiers::SUPER);
    assert!(matches!(
        handle_settings_key(&mut s, &key),
        SettingsKeyOutcome::Close
    ));
}

#[test]
fn filter_mode_swallows_chars_into_query() {
    let mut s = make_state();
    let slash = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(matches!(
        handle_settings_key(&mut s, &slash),
        SettingsKeyOutcome::Changed
    ));
    assert!(matches!(s.mode(), SettingsModalMode::FilterFocused));
    for c in "compact".chars() {
        let k = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let _ = handle_settings_key(&mut s, &k);
    }
    assert_eq!(s.query(), "compact");

    // Esc exits filter, doesn't close modal.
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(
        handle_settings_key(&mut s, &esc),
        SettingsKeyOutcome::Changed
    ));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

/// `i` aliases `/` without modifiers: from Browse it enters FilterFocused
/// exactly like `/` (vim-nav "press i to search").
#[test]
fn i_key_enters_filter_like_slash() {
    let mut s = make_state();
    let i = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
    assert!(matches!(
        handle_settings_key(&mut s, &i),
        SettingsKeyOutcome::Changed
    ));
    assert!(matches!(s.mode(), SettingsModalMode::FilterFocused));
}

/// The `modifiers.is_empty()` guard: Ctrl+i / Alt+i must NOT enter filter.
#[test]
fn modified_i_does_not_enter_filter() {
    for mods in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        let mut s = make_state();
        let k = KeyEvent::new(KeyCode::Char('i'), mods);
        assert!(matches!(
            handle_settings_key(&mut s, &k),
            SettingsKeyOutcome::Unchanged
        ));
        assert!(matches!(s.mode(), SettingsModalMode::Browse));
    }
}

/// Wiring check: the Browse footer carries the shared `i search` hint
/// under vim nav mode. The gate itself is covered centrally by
/// `modal_window::tests::vim_nav_search_hint_only_in_vim_nav_mode`. The
/// explicit `set_vim_mode` pin (a thread-local that, once set, blocks
/// disk-seeding) keeps this independent of the dev's on-disk `[ui].vim_mode`;
/// reset afterward since libtest reuses worker threads.
#[test]
fn browse_footer_advertises_i_search_under_vim() {
    crate::appearance::cache::set_vim_mode(true);
    let s = make_state();
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
    assert!(
        build_shortcuts(&s).iter().any(|sc| sc.label == "i search"),
        "vim-mode Browse footer must advertise `i search`"
    );
    crate::appearance::cache::set_vim_mode(false);
}

#[test]
fn mouse_click_on_bool_row_dispatches_toggle() {
    // We can't render to a real Buffer here without a real terminal,
    // but we can hand-set the row_rects to simulate the post-render
    // state.
    let mut s = make_state();
    s.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    s.row_rects.resize(s.rows.len(), Rect::default());
    // Row 0 is the Appearance header.
    s.row_rects[0] = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };
    // Row 1 is compact_mode.
    s.row_rects[1] = Rect {
        x: 0,
        y: 1,
        width: 80,
        height: 1,
    };

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        5,
        1,
    );
    match outcome {
        SettingsKeyOutcome::Action(Action::SetCompactMode(true)) => {}
        other => panic!("expected SetCompactMode(true) from click, got {other:?}"),
    }
}

#[test]
fn mouse_click_on_header_is_no_op() {
    let mut s = make_state();
    s.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    s.row_rects.resize(s.rows.len(), Rect::default());
    s.row_rects[0] = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        5,
        0,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
}

// ---------- mouse hover highlight ----------

#[test]
fn selected_browse_row_label_is_bold() {
    let state = make_state();
    let meta = state
        .registry
        .find("compact_mode")
        .expect("compact mode registered");
    let area = Rect::new(0, 0, 80, 1);
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();

    render_setting_row(
        &mut buf,
        area,
        meta,
        &SettingValue::Bool(false),
        40,
        true,
        &theme,
        false,
        false,
        None,
    );

    assert!(
        buf.cell((2, 0))
            .expect("label cell")
            .style()
            .add_modifier
            .contains(Modifier::BOLD),
    );
}

#[test]
fn settings_list_row_bg_terminal_native_elevates_selection() {
    let theme = Theme::terminal_default();
    assert!(matches!(theme.bg_visual, Color::Reset));
    assert_eq!(settings_list_row_bg(&theme, true, false), Color::DarkGray);
    assert_eq!(settings_list_row_bg(&theme, false, true), Color::DarkGray);
    assert_eq!(settings_list_row_bg(&theme, false, false), Color::Reset);
    assert_eq!(settings_list_row_bg(&theme, true, true), Color::DarkGray);
}

/// `MouseEventKind::Moved` over a setting row's hit-rect sets
/// `state.hover_row` to that row's index and reports `Changed`
/// so the next render paints the highlight. Mirrors the
/// scrollback's `hovered_entry` plumbing.
#[test]
fn mouse_moved_over_row_sets_hover_row() {
    let mut s = make_state();
    s.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    s.row_rects.resize(s.rows.len(), Rect::default());
    // Row 0 is a header (Appearance); row 1 is the first
    // setting (compact_mode). Place row 1 at y=1 so the
    // hover-row position math is unambiguous.
    s.row_rects[0] = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };
    s.row_rects[1] = Rect {
        x: 0,
        y: 1,
        width: 80,
        height: 1,
    };

    assert_eq!(s.hover_row, None, "fresh state must have no hover");

    let outcome = handle_settings_mouse(&mut s, MouseEventKind::Moved, 5, 1);
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Moved into a row's rect must report Changed, got {outcome:?}"
    );
    assert_eq!(
        s.hover_row,
        Some(1),
        "Moved at (5,1) must land on row 1 (compact_mode)",
    );

    // A second Moved event at the same coordinates is a no-op
    // (hover_row already matches → no Changed).
    let outcome = handle_settings_mouse(&mut s, MouseEventKind::Moved, 5, 1);
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "repeat Moved at the same row must be Unchanged, got {outcome:?}"
    );
}

/// `Moved` outside every row's hit-rect clears `state.hover_row`
/// to `None`. Mirrors the scrollback's behaviour when the mouse
/// drifts off the entry list.
#[test]
fn mouse_moved_outside_modal_clears_hover() {
    let mut s = make_state();
    s.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    s.row_rects.resize(s.rows.len(), Rect::default());
    s.row_rects[1] = Rect {
        x: 0,
        y: 1,
        width: 80,
        height: 1,
    };

    // Seed hover at row 1.
    let _ = handle_settings_mouse(&mut s, MouseEventKind::Moved, 5, 1);
    assert_eq!(s.hover_row, Some(1));

    // Move far outside any row's rect.
    let outcome = handle_settings_mouse(&mut s, MouseEventKind::Moved, 5, 50);
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(
        s.hover_row, None,
        "Moved outside all row rects must clear hover_row",
    );
}

/// `Moved` over a header row's hit-rect does NOT set hover_row
/// (headers aren't selectable; painting hover on them would be
/// misleading).
#[test]
fn mouse_moved_over_header_does_not_set_hover() {
    let mut s = make_state();
    s.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    s.row_rects.resize(s.rows.len(), Rect::default());
    s.row_rects[0] = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };

    let outcome = handle_settings_mouse(&mut s, MouseEventKind::Moved, 5, 0);
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Moved over a header must be Unchanged",
    );
    assert_eq!(
        s.hover_row, None,
        "header rows must not register as hovered",
    );
}

/// `state.hover_row = Some(idx)` paints the hovered row's bg
/// with the theme's `bg_hover` color. (Mirrors the existing
/// `picker_separates_focus_highlight_from_committed_marker` test's
/// pattern: the `assert_eq` against `theme.bg_hover` survives both
/// colored and quantize-to-Reset color levels.)
#[test]
fn hover_row_renders_with_hover_style() {
    let mut s = make_state();
    let theme = Theme::current();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    // Pick a setting row that is NOT the selected row so the
    // hover bg vs selection bg distinction is observable.
    let setting_indices: Vec<usize> = s
        .rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, RowEntry::Setting { .. }).then_some(i))
        .collect();
    assert!(
        setting_indices.len() >= 2,
        "test requires at least 2 selectable rows",
    );
    let initial_selected = s.selected;
    let hover_idx = *setting_indices
        .iter()
        .find(|&&i| i != initial_selected)
        .expect("at least one non-selected setting row must exist");
    s.hover_row = Some(hover_idx);

    render_rows(&mut buf, area, &mut s, &theme);

    // Read back the bg of the painted hover row from the
    // first column of the row's allocated area.
    let row_rect = s.row_rects[hover_idx];
    assert!(
        row_rect.width > 0 && row_rect.height > 0,
        "hover row must have a non-zero rect after render",
    );
    let cell = buf
        .cell((row_rect.x, row_rect.y))
        .expect("rendered cell must exist");
    assert_eq!(
        cell.style().bg,
        Some(settings_list_row_bg(&theme, false, true)),
        "hover row must paint with the list hover background, got {:?}",
        cell.style().bg,
    );
}

/// In `PickingEnum` mode, a Moved event over a choice's hit-rect
/// sets `state.hover_row` (semantically: hovered choice index)
/// and the next render paints THAT choice with the hover bg
/// without affecting other choices.
#[test]
fn picker_choice_mouse_hover_highlights_choice() {
    let mut s = picker_test_state();
    // Populate per-choice rects by rendering once, then drain
    // the thread-local stash into `state.picker_choice_rects`.
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);
    s.picker_choice_rects = take_picker_choice_rects();
    assert!(
        s.picker_choice_rects.len() >= 2,
        "picker_test_state must produce at least 2 choice rects",
    );

    // Pre-condition: choices_idx = 0 (the initial focus). Hover
    // over choice 1 — distinct from the focused choice.
    let target_rect = s.picker_choice_rects[1];
    assert!(
        target_rect.height > 0,
        "choice 1 must be visible after render",
    );
    let click_y = target_rect.y;
    let click_x = target_rect.x + target_rect.width / 2;

    let outcome = handle_settings_mouse(&mut s, MouseEventKind::Moved, click_x, click_y);
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Moved over choice 1 must be Changed, got {outcome:?}",
    );
    assert_eq!(
        s.hover_row,
        Some(1),
        "Moved over choice 1 must set hover_row = Some(1)",
    );

    // Re-render and observe the shared list hover background on choice 1.
    let mut buf2 = Buffer::empty(area);
    render_picking_enum(&mut buf2, area, &s, &theme);
    let new_rects = take_picker_choice_rects();
    let rect1 = new_rects[1];
    let cell1 = buf2
        .cell((rect1.x, rect1.y))
        .expect("choice 1 cell must exist");
    assert_eq!(
        cell1.style().bg,
        Some(settings_list_row_bg(&theme, false, true)),
        "hovered choice must paint the list hover background, got {:?}",
        cell1.style().bg,
    );
    // Focused choice (index 0) keeps bg_visual — selection wins
    // over hover. Verifies the `is_focused` branch precedence.
    let rect0 = new_rects[0];
    let cell0 = buf2
        .cell((rect0.x, rect0.y))
        .expect("choice 0 cell must exist");
    assert_eq!(
        cell0.style().bg,
        Some(settings_list_row_bg(&theme, true, false)),
        "focused choice must keep the list selection background when hover is elsewhere",
    );
}

/// Mode transitions (Browse → PickingEnum and Browse →
/// EditingValue) clear `hover_row` so a stale row-index from
/// the old mode doesn't paint a wrong choice/row in the new
/// mode before the next Moved event arrives.
#[test]
fn mode_transition_browse_to_picking_enum_clears_hover_row() {
    let mut s = make_state();
    navigate_to_enum_row(&mut s);
    s.hover_row = Some(s.selected);
    let _ = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(s.mode(), SettingsModalMode::PickingEnum { .. }),
        "Enter on enum row must transition to PickingEnum",
    );
    assert_eq!(
        s.hover_row, None,
        "Browse → PickingEnum transition must clear hover_row",
    );
}

/// Sibling of `mode_transition_browse_to_picking_enum_clears_hover_row` —
/// the same hover-clear contract applies to the
/// `Browse → EditingValue` transition (Enter on an `Int`-kind
/// row). The prior `mode_transition_clears_hover_row`
/// docstring claimed to cover both transitions but only exercised
/// `PickingEnum`.
#[test]
fn mode_transition_browse_to_editing_value_clears_hover_row() {
    let mut s = make_state();
    navigate_to_int_row(&mut s);
    s.hover_row = Some(s.selected);
    let _ = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(s.mode(), SettingsModalMode::EditingValue { .. }),
        "Enter on Int row must transition to EditingValue, got {:?}",
        s.mode(),
    );
    assert_eq!(
        s.hover_row, None,
        "Browse → EditingValue transition must clear hover_row",
    );
}

/// Sibling for the reverse direction: PickingEnum → Browse (Esc)
/// also clears any stale hover that landed during the picker.
#[test]
fn mode_transition_picking_enum_to_browse_clears_hover_row() {
    let mut s = make_state();
    navigate_to_enum_row(&mut s);
    let _ = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
    // Seed picker-mode hover (e.g. mouse moved over a non-focused
    // choice while in the picker).
    s.hover_row = Some(2);
    let _ = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc in PickingEnum must transition to Browse",
    );
    assert_eq!(
        s.hover_row, None,
        "PickingEnum → Browse transition must clear hover_row",
    );
}

/// Helper for `mode_transition_browse_to_editing_value_clears_hover_row`:
/// walks selection forward to the first `Int`-kind row registered
/// in defaults (currently `max_thoughts_width`).
fn navigate_to_int_row(s: &mut SettingsModalState) {
    let key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
    for _ in 0..s.rows.len() {
        if let Some((_, meta)) = s.focused_setting()
            && matches!(meta.kind, SettingKind::Int { .. })
        {
            return;
        }
        let outcome = handle_settings_key(s, &key);
        if matches!(outcome, SettingsKeyOutcome::Unchanged) {
            break;
        }
    }
    panic!("no Int-kind row found in default registry");
}

/// Helper for `mode_transition_clears_hover_row`: walks selection
/// forward to the first Enum-kind row registered in defaults.
fn navigate_to_enum_row(s: &mut SettingsModalState) {
    let key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
    for _ in 0..s.rows.len() {
        if let Some((_, meta)) = s.focused_setting()
            && matches!(meta.kind, SettingKind::Enum { .. })
        {
            return;
        }
        let outcome = handle_settings_key(s, &key);
        if matches!(outcome, SettingsKeyOutcome::Unchanged) {
            break;
        }
    }
    panic!("no Enum-kind row found in default registry");
}

/// Scroll-wheel down emits 3 advances. From the initial selection
/// (first setting), 3 settings forward must land on whatever is at
/// position [first_setting + 3] in the registry. Resilient to PR-N
/// additions.
#[test]
fn scroll_wheel_advances_selection() {
    let mut s = make_state();
    s.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let setting_keys: Vec<SettingKey> = s
        .rows
        .iter()
        .filter_map(|r| {
            if let RowEntry::Setting { key, .. } = r {
                Some(*key)
            } else {
                None
            }
        })
        .collect();
    let outcome = handle_settings_mouse(&mut s, MouseEventKind::ScrollDown, 5, 5);
    // 3 advances from the first setting → setting at position 3
    // (or the last setting if fewer than 4 are registered — the
    // advance_next no-op at the boundary absorbs extra advances).
    let expected = setting_keys
        .get(3)
        .copied()
        .unwrap_or(*setting_keys.last().unwrap());
    match &s.rows[s.selected] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, expected),
        _ => panic!("expected setting after scroll"),
    }
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
}

// -- routing scaffold tests --
//
// The enum chooser and string/int editor declare their
// mode variants alongside Browse and route Esc → Browse so the
// scaffold doesn't ship dead `unimplemented!()` panics. These
// tests pin the routing.

/// `render_setting_row` produces the "restart" pill when
/// `meta.restart_required == true` AND the row is expanded. When
/// no registered setting sets `restart_required`, this test still
/// exercises the render path via a synthetic setting with the flag
/// set.
///
/// The pill is a property
/// label, not a "restart pending" tracker — a collapsed
/// non-default row showing it forever misreads as pending, so
/// the old `is_edited` trigger is gone and only `is_expanded`
/// gates it (the "(restart to apply)" toast covers change-time
/// feedback). Arms: expanded (pill) and edited-but-collapsed
/// (no pill — the exact reported repro).
#[test]
fn render_setting_row_emits_restart_pill_when_required() {
    let meta = SettingMeta {
        key: "test-key",
        category: SettingCategory::Appearance,
        owner: crate::settings::SettingOwner::Shared,
        label: "Test setting",
        description: "Test description.",
        keywords: &["test"],
        kind: SettingKind::Bool { default: false },
        restart_required: true,
        hidden_in_minimal: false,
    };
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };
    // Arm 1: expanded → pill renders even at default value.
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_setting_row(
        &mut buf,
        area,
        &meta,
        &SettingValue::Bool(false),
        10,    // max_label_w
        false, // is_selected
        &theme,
        true,  // is_expanded — gate on
        false, // is_hovered
        None,
    );
    let mut rendered = String::new();
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, 0)) {
            rendered.push_str(cell.symbol());
        }
    }
    assert!(
        rendered.contains("restart"),
        "expanded row must contain the 'restart' pill: {rendered:?}"
    );

    // Arm 2: edited but collapsed → NO pill (the reported repro).
    let mut buf = Buffer::empty(area);
    render_setting_row(
        &mut buf,
        area,
        &meta,
        &SettingValue::Bool(true), // edited from default `false`
        10,
        false,
        &theme,
        false, // is_expanded — off
        false, // is_hovered
        None,
    );
    let mut rendered = String::new();
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, 0)) {
            rendered.push_str(cell.symbol());
        }
    }
    assert!(
        !rendered.contains("restart"),
        "edited-but-collapsed row must NOT contain the 'restart' pill: {rendered:?}"
    );
}

/// Counterpart to `render_setting_row_emits_restart_pill_when_required`:
/// the pill is HIDDEN on any collapsed row. User-feedback
/// follow-up — keeps the modal uncluttered for the common
/// "I'm just browsing" case.
#[test]
fn render_setting_row_hides_restart_pill_when_at_default_and_collapsed() {
    let meta = SettingMeta {
        key: "test-key",
        category: SettingCategory::Appearance,
        owner: crate::settings::SettingOwner::Shared,
        label: "Test setting",
        description: "Test description.",
        keywords: &["test"],
        kind: SettingKind::Bool { default: false },
        restart_required: true,
        hidden_in_minimal: false,
    };
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_setting_row(
        &mut buf,
        area,
        &meta,
        &SettingValue::Bool(false),
        10,
        false,
        &theme,
        false, // is_expanded
        false, // is_hovered
        None,
    );
    let mut rendered = String::new();
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, 0)) {
            rendered.push_str(cell.symbol());
        }
    }
    assert!(
        !rendered.contains("restart"),
        "at-default, not-expanded row must NOT contain the 'restart' pill: {rendered:?}"
    );
}

// -- render-buffer tests for the
// editor's cursor and validation-error display. --

/// Build an editor state pre-positioned at the start of a known
/// buffer for `default_model`. Catalog populated so the
/// `KnownModel` validator has data to validate against.
fn editor_render_fixture(buffer: &str, cursor_byte: usize) -> SettingsModalState {
    use agent_client_protocol as acp;
    use std::sync::Arc;
    // An earlier fixture used `default_model` (a SHELL `String`
    // setting) to exercise the inline editor. `default_model` is
    // now a `SettingKind::DynamicEnum`, so the
    // String editor no longer has a registered consumer in the
    // production catalog. We construct a synthetic registry
    // with a single `KnownModel`-validated String entry to keep
    // the editor-render contract under test — the production
    // editor code path stays exercised even though no live
    // setting wires it up today.
    let synthetic_meta = SettingMeta {
        key: "default_model",
        category: SettingCategory::Models,
        owner: crate::settings::SettingOwner::Shell,
        label: "Default model (synthetic)",
        // Short description so it fits in 1 line even at the
        // narrowest test width (the editor's word-wrap path
        // would otherwise push the input
        // row down on narrow widths, breaking the cursor-pan
        // tests' hardcoded input-row y position).
        description: "Test.",
        keywords: &["test"],
        kind: SettingKind::String {
            default: "",
            validator: StringValidator::KnownModel,
        },
        restart_required: false,
        hidden_in_minimal: false,
    };
    let registry = SettingsRegistry::from_entries(vec![synthetic_meta]);
    let snapshot = PagerLocalSnapshot {
        available_models: vec![(
            "EchoAgent Test".to_string(),
            acp::ModelId::new(Arc::from("echo-agent-test")),
        )],
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(Arc::new(registry), UiConfig::default(), snapshot);
    let mut editor = LineEditor::default();
    editor.set_text(buffer);
    let _ = editor.set_cursor_byte(cursor_byte);
    let validation_error = validate_string(
        StringValidator::KnownModel,
        editor.text(),
        &s.pager_snapshot.available_models,
    );
    s.transition_to_editing_string(
        "default_model",
        editor,
        StringValidator::KnownModel,
        validation_error,
    );
    s
}

/// Cursor lands at the visual column matching `cursor_byte` for
/// buffers that fit entirely within the visible window.
#[test]
fn render_editing_value_cursor_at_logical_position_when_buffer_fits() {
    let mut s = editor_render_fixture("EchoAgent Test", 4); // cursor between "EchoAgent" and " Test"
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);

    // The input row is at y = header_rows = 3 (title + desc + gap).
    // Cursor glyph is ▏ (left one-eighth block).
    let row_y = 3u16;
    let mut found_cursor_col: Option<u16> = None;
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, row_y))
            && cell.symbol() == "\u{258F}"
        {
            found_cursor_col = Some(x);
            break;
        }
    }
    // For a non-overflow buffer, cursor column == cursor_byte
    // (the buffer is ASCII so byte == col).
    assert_eq!(
        found_cursor_col,
        Some(4),
        "cursor must render at column 4 (= cursor_byte) for in-window buffer",
    );
}

/// When the buffer overflows the visible window AND the cursor
/// is at the start (Home or Left), the visible window pans so
/// the start of the buffer is visible. The cursor renders at
/// the LEFT edge, NOT the right.
#[test]
fn render_editing_value_cursor_pans_to_left_on_overflow_at_start() {
    // Build a buffer wide enough to overflow a narrow window.
    let buffer = "A".repeat(80);
    let mut s = editor_render_fixture(&buffer, 0);
    let area = Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);

    let row_y = 3u16;
    let mut found_cursor_col: Option<u16> = None;
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, row_y))
            && cell.symbol() == "\u{258F}"
        {
            found_cursor_col = Some(x);
            break;
        }
    }
    assert_eq!(
        found_cursor_col,
        Some(0),
        "cursor must pan to column 0 when cursor_byte is at the start, even with overflow",
    );
}

/// When the buffer overflows AND the cursor is at the
/// end, the visible window pans so the cursor sits at the
/// rightmost-but-one column (column = `buffer_room - 1` =
/// `visible_buffer_w`) — the last col is the cursor-reserve
/// space, the cursor itself renders just inside it.
#[test]
fn render_editing_value_cursor_pans_to_right_on_overflow_at_end() {
    let buffer = "A".repeat(80);
    let cursor = buffer.len();
    let mut s = editor_render_fixture(&buffer, cursor);
    let area = Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);

    let row_y = 3u16;
    let mut found_cursor_col: Option<u16> = None;
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, row_y))
            && cell.symbol() == "\u{258F}"
        {
            found_cursor_col = Some(x);
            break;
        }
    }
    // The cursor lands at `visible_buffer_w` = `buffer_room - 1`
    // — one cell before the rightmost edge, which is the
    // cursor-reserve column. Strictly greater than 0 is the
    // important regression-prevention assertion (the pre-R2
    // bug was the cursor being pinned at the right edge
    // REGARDLESS of cursor_byte).
    let col = found_cursor_col.expect("cursor must render");
    assert!(
        col > 0,
        "cursor must be panned past the start when cursor is at end of overflowing buffer \
         (got col {col})",
    );
    assert!(
        col >= area.width - 2,
        "cursor must be near the rightmost visible column for end-of-buffer position \
         (got col {col}, area.width = {})",
        area.width,
    );
}

#[test]
fn render_string_editor_keeps_narrow_graphemes_and_cursor_aligned() {
    let grapheme = "👩🏽\u{200d}💻";
    let combining = "e\u{301}";
    let text = format!("a{grapheme}{combining}");
    let mut state = editor_render_fixture(&text, text.len());
    let area = Rect {
        x: 0,
        y: 0,
        width: 4,
        height: 12,
    };
    let mut buffer = Buffer::empty(area);
    render_editing_value(&mut buffer, area, &mut state, &Theme::current());

    let mut rendered = String::new();
    let mut cursor = None;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = buffer.cell((x, y)).expect("rendered cell");
            rendered.push_str(cell.symbol());
            if cell.symbol() == crate::glyphs::selection_bar() {
                cursor = Some((x, y));
            }
        }
    }
    assert!(
        rendered.contains(grapheme),
        "ZWJ grapheme split: {rendered:?}"
    );
    assert!(
        rendered.contains(combining),
        "combining grapheme split: {rendered:?}",
    );
    assert_eq!(cursor.map(|(x, _)| x), Some(3));
}

/// When the validator returns a
/// non-None error, the buffer foreground turns red
/// (`accent_error`), AND the validation-error row at y =
/// header_rows + 1 renders the error message in accent_error.
#[test]
fn render_editing_value_paints_validation_error_row_and_buffer_red() {
    // Use a buffer that fails KnownModel ("xyz" not in catalog).
    let mut s = editor_render_fixture("xyz", 3);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);

    // Row 3 is the input. The buffer "xyz" sits at cols 0..3.
    // Each char's foreground must be accent_error.
    for x in 0..3 {
        let cell = buf.cell((x, 3)).expect("cell must exist");
        assert_eq!(
            cell.style().fg,
            Some(theme.accent_error),
            "input row col {x} must use accent_error fg, got {:?}",
            cell.style().fg,
        );
    }

    // Row 4 is the validation error.
    let mut err_row = String::new();
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, 4)) {
            err_row.push_str(cell.symbol());
        }
    }
    assert!(
        err_row.to_lowercase().contains("unknown model"),
        "validation-error row must contain 'unknown model' (case-insensitive), got {err_row:?}",
    );
}

/// Empty buffer renders a
/// low-contrast placeholder hint. The cursor block (`▏`) lands
/// at col 0 and overdraws the placeholder's leading `<`, so the
/// assertion targets the unique "empty: uses shell default"
/// substring that survives the cursor overdraw.
#[test]
fn render_editing_value_empty_buffer_shows_placeholder() {
    let mut s = editor_render_fixture("", 0);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);

    let mut input_row = String::new();
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, 3)) {
            input_row.push_str(cell.symbol());
        }
    }
    // Placeholder for KnownModel ends with "uses shell default>".
    // The cursor at col 0 overdraws the leading `<`, so we match
    // on the body substring.
    assert!(
        input_row.contains("uses shell default"),
        "empty buffer must render the KnownModel placeholder, got {input_row:?}",
    );
}

/// Rendering an Int stepper wide enough for the
/// `‹` / `›` arrow glyphs populates `editor_adornment_rects`
/// so `handle_settings_mouse` can hit-test arrow clicks. The
/// arrows flank a centered value, NOT the old `[-]` / `[+]`
/// adornments flush against the area edges.
#[test]
fn render_editing_value_int_populates_adornment_hit_rects() {
    // Use a settings registry containing the real
    // `max_thoughts_width` Int entry.
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_editing_int("max_thoughts_width", "120".to_string(), 40, 500);
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);

    let (dec_rect, inc_rect) = s.editor_adornment_rects;
    assert!(
        dec_rect.width > 0 && dec_rect.height > 0,
        "decrement (left arrow) rect must be non-zero (got {dec_rect:?})",
    );
    assert!(
        inc_rect.width > 0 && inc_rect.height > 0,
        "increment (right arrow) rect must be non-zero (got {inc_rect:?})",
    );
    // The arrows flank a centered value. The left
    // arrow sits LEFT of the value, the right arrow RIGHT of
    // it; both are strictly inside the area.
    assert!(
        dec_rect.x > 0 && dec_rect.x < inc_rect.x,
        "left arrow must be inside the area, before the right arrow \
         (got dec.x={}, inc.x={})",
        dec_rect.x,
        inc_rect.x,
    );
    assert!(
        inc_rect.x + inc_rect.width < area.width,
        "right arrow must fit inside the area",
    );
    // Both arrows live on the same stepper row. The
    // description word-wraps, so the input
    // row's y position depends on how many lines the
    // description consumes. The previous hardcoded `== 3`
    // assertion was a side-effect of the truncate-only render.
    assert_eq!(
        dec_rect.y, inc_rect.y,
        "left + right arrow must share the same stepper row",
    );
}

// ---------- Int stepper key + render contracts ----------

/// Helper: build a `SettingsModalState` directly in EditingValue
/// mode for a registered Int setting with the given starting value.
fn int_stepper_fixture_for(key: &'static str, value: i64) -> SettingsModalState {
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    let (min, max) = match s.registry.find(key).map(|meta| &meta.kind) {
        Some(SettingKind::Int { min, max, .. }) => (*min, *max),
        _ => panic!("expected registered Int setting"),
    };
    s.transition_to_editing_int(key, value.to_string(), min, max);
    s
}

/// Wide-range Int fixture (`max_thoughts_width`, steps ±5/±10).
fn int_stepper_fixture(value: i64) -> SettingsModalState {
    int_stepper_fixture_for("max_thoughts_width", value)
}

fn int_stepper_buffer(s: &SettingsModalState) -> String {
    s.editing_buffer()
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("expected EditingValue, got {:?}", s.mode()))
}

#[test]
fn int_step_sizes_table_pins_range_policy() {
    // (min, max, expected_small, expected_large)
    let cases = [
        (1, 10, 1, 1),    // scroll_lines (span 9)
        (1, 100, 1, 5),   // scroll_speed (span 99)
        (40, 500, 5, 10), // max_thoughts_width (span 460)
        (0, 0, 1, 1),     // degenerate span
        (1, 21, 1, 4),    // span 20 still narrow: large = span/5
        (1, 22, 1, 5),    // span 21 → mid band
        (1, 101, 1, 5),   // span 100 still mid
        (1, 102, 5, 10),  // span 101 → wide band
    ];
    for (min, max, want_small, want_large) in cases {
        assert_eq!(
            int_step_sizes(min, max),
            (want_small, want_large),
            "int_step_sizes({min}, {max})",
        );
    }
}

/// Up arrow steps the wide-range Int by small step (+5).
#[test]
fn int_editing_value_up_arrow_increments_by_small_step() {
    let mut s = int_stepper_fixture(50);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "55");
}

/// Down arrow steps the wide-range Int by small step (−5).
#[test]
fn int_editing_value_down_arrow_decrements_by_small_step() {
    let mut s = int_stepper_fixture(50);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "45");
}

/// Right arrow steps the wide-range Int by large step (+10).
#[test]
fn int_editing_value_right_arrow_increments_by_large_step() {
    let mut s = int_stepper_fixture(50);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "60");
}

/// Left arrow steps the wide-range Int by large step (−10).
#[test]
fn int_editing_value_left_arrow_decrements_by_large_step() {
    let mut s = int_stepper_fixture(50);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "40");
}

/// Narrow-range Int (`scroll_lines` 1..=10) uses unit steps on all arrows.
#[test]
fn scroll_lines_int_stepper_uses_unit_steps() {
    let mut s = int_stepper_fixture_for("scroll_lines", 3);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "4");
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "3");
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "4");
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "3");
}

/// Mid-range Int (`scroll_speed` 1..=100): Up/Down ±1, Left/Right ±5.
#[test]
fn scroll_speed_int_stepper_uses_unit_fine_and_five_coarse() {
    let mut s = int_stepper_fixture_for("scroll_speed", 50);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "51");
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "56");
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "55");
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "50");
}

/// k / j vim aliases mirror Up / Down.
#[test]
fn int_editing_value_vim_k_j_step_by_small() {
    let mut s = int_stepper_fixture(50);
    let _ = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
    );
    assert_eq!(int_stepper_buffer(&s), "55");
    let _ = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    assert_eq!(int_stepper_buffer(&s), "50");
}

/// l / h vim aliases mirror Right / Left.
#[test]
fn int_editing_value_vim_l_h_step_by_large() {
    let mut s = int_stepper_fixture(50);
    let _ = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
    );
    assert_eq!(int_stepper_buffer(&s), "60");
    let _ = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
    );
    assert_eq!(int_stepper_buffer(&s), "50");
}

/// Stepping past `max` is clamped to `max`; the outcome is
/// `Unchanged` (no visible step) so a UI consumer can avoid
/// re-render churn.
#[test]
fn int_editing_value_clamps_to_max() {
    // max_thoughts_width registered max = 500.
    let mut s = int_stepper_fixture(500);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Up at max must be Unchanged (no movement), got {outcome:?}",
    );
    assert_eq!(int_stepper_buffer(&s), "500");
}

/// Stepping below `min` is clamped to `min`.
#[test]
fn int_editing_value_clamps_to_min() {
    // max_thoughts_width registered min = 40.
    let mut s = int_stepper_fixture(40);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Down at min must be Unchanged (no movement), got {outcome:?}",
    );
    assert_eq!(int_stepper_buffer(&s), "40");
}

/// Digit keys are silently dropped — the stepper is not a
/// text input.
#[test]
fn int_editing_value_ignores_digit_keys() {
    let mut s = int_stepper_fixture(50);
    let outcome = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('7'), KeyModifiers::NONE),
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
    assert_eq!(int_stepper_buffer(&s), "50");
}

/// Backspace is silently dropped — the stepper has no
/// editable buffer to backspace into.
#[test]
fn int_editing_value_ignores_backspace() {
    let mut s = int_stepper_fixture(50);
    let outcome = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
    assert_eq!(int_stepper_buffer(&s), "50");
}

/// Delete / Home / End / Tab are likewise dropped.
#[test]
fn int_editing_value_ignores_other_text_input_keys() {
    let mut s = int_stepper_fixture(50);
    for code in [KeyCode::Delete, KeyCode::Home, KeyCode::End, KeyCode::Tab] {
        let outcome = handle_settings_key(&mut s, &KeyEvent::new(code, KeyModifiers::NONE));
        assert!(
            matches!(outcome, SettingsKeyOutcome::Unchanged),
            "stepper must reject {code:?}, got {outcome:?}",
        );
        assert_eq!(int_stepper_buffer(&s), "50");
    }
}

/// Render the stepper and assert the `‹` / `›` arrow glyphs
/// AND the value text are present on the input row.
#[test]
fn int_editing_value_renders_stepper_ui() {
    let mut s = int_stepper_fixture(125);
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);

    // Find the stepper row dynamically — the description
    // word-wraps so the input row's y is no
    // longer fixed at 3. Scan rows top-down for the one that
    // contains both stepper glyphs.
    let mut stepper_row: Option<String> = None;
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        if row.contains('\u{2039}') && row.contains('\u{203A}') {
            stepper_row = Some(row);
            break;
        }
    }
    let row = stepper_row.expect("must find the stepper row");
    assert!(
        row.contains('\u{2039}'),
        "stepper row must contain `‹` glyph, got {row:?}"
    );
    assert!(
        row.contains('\u{203A}'),
        "stepper row must contain `›` glyph, got {row:?}"
    );
    assert!(
        row.contains("125"),
        "stepper row must contain the value text, got {row:?}"
    );
}

/// Enter commits the typed Action and transitions back to
/// Browse. The `action_for_int` arm for `max_thoughts_width`
/// produces `Action::SetMaxThoughtsWidth(<i64>)`.
#[test]
fn int_editing_value_enter_commits() {
    let mut s = int_stepper_fixture(75);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetMaxThoughtsWidth(75)) => {}
        other => panic!("expected SetMaxThoughtsWidth(75) on Enter, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter must return to Browse"
    );
}

/// Esc cancels — no Action is emitted, and mode returns to
/// Browse. The user's in-progress step is dropped; the
/// underlying setting value (`UiConfig.max_thoughts_width`)
/// stays at whatever it was before the editor opened.
#[test]
fn int_editing_value_esc_reverts() {
    let mut s = int_stepper_fixture(75);
    // Take a step so the buffer diverges from the original.
    let _ = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(int_stepper_buffer(&s), "80");

    // Esc.
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Esc must be Changed (mode swap), got {outcome:?}"
    );
    assert!(
        !matches!(outcome, SettingsKeyOutcome::Action(_)),
        "Esc must NOT emit any Action — the underlying value was \
         never live-previewed",
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc must return to Browse"
    );
}

/// Mouse click on the `‹` left-arrow rect synthesizes a Down
/// step (small step down). The click maps to small steps to
/// match the spinner convention — repeated clicks let the
/// user fine-tune.
#[test]
fn int_editing_value_left_arrow_click_decrements_by_small_step() {
    let mut s = int_stepper_fixture(120);
    // Render once to populate `editor_adornment_rects`.
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);
    let (dec_rect, _) = s.editor_adornment_rects;
    assert!(dec_rect.width > 0, "left arrow rect must be populated");

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        dec_rect.x,
        dec_rect.y,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "115");
}

/// Mouse click on the `›` right-arrow rect synthesizes an Up
/// step. Mirror of the left-arrow test.
#[test]
fn int_editing_value_right_arrow_click_increments_by_small_step() {
    let mut s = int_stepper_fixture(120);
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);
    let (_, inc_rect) = s.editor_adornment_rects;
    assert!(inc_rect.width > 0, "right arrow rect must be populated");

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        inc_rect.x,
        inc_rect.y,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(int_stepper_buffer(&s), "125");
}

/// Mouse click on the value text (between the arrows) is a
/// no-op — clicks here shouldn't accidentally commit or step.
#[test]
fn int_editing_value_click_on_value_text_is_noop() {
    let mut s = int_stepper_fixture(120);
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);
    let (dec_rect, inc_rect) = s.editor_adornment_rects;
    // Click between the two arrow rects.
    let middle_x = (dec_rect.x + inc_rect.x) / 2;
    let middle_y = dec_rect.y;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        middle_x,
        middle_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "click on value text must be Unchanged, got {outcome:?}"
    );
    assert_eq!(int_stepper_buffer(&s), "120");
}

/// Esc in the theme picker dispatches a PREVIEW Action with the
/// original canonical AND returns to Browse:
/// preview-revert restores the live visual without persisting,
/// since the picker's preview navs never persisted in the first
/// place. Parameterised across all 3 theme enum keys.
#[test]
fn picking_enum_esc_dispatches_preview_revert_for_each_key() {
    let cases: &[(&str, &str)] = &[
        ("theme", "echo-agent-night"),
        ("auto_dark_theme", "echo-agent-night"),
        ("auto_light_theme", "echo-agent-day"),
    ];
    for &(key, original) in cases {
        let mut s = make_state();
        s.transition_to_picking_enum(key, 0, SettingValue::Enum(original), true);
        let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        match (key, outcome) {
            ("theme", SettingsKeyOutcome::Action(Action::PreviewTheme(name))) => {
                assert_eq!(name, original);
            }
            ("auto_dark_theme", SettingsKeyOutcome::Action(Action::PreviewAutoDarkTheme(name))) => {
                assert_eq!(name, original);
            }
            (
                "auto_light_theme",
                SettingsKeyOutcome::Action(Action::PreviewAutoLightTheme(name)),
            ) => {
                assert_eq!(name, original);
            }
            (k, other) => {
                panic!("Esc on `{k}` picker should dispatch matching Preview Action, got {other:?}")
            }
        }
        assert!(
            matches!(s.mode(), SettingsModalMode::Browse),
            "Esc must transition back to Browse for key `{key}`",
        );
    }
}

/// Backwards-compat alias for an earlier test name. Asserts
/// the same behaviour for the `theme` key only — kept so a future
/// reader grep'ing the older test name still finds working
/// coverage. The parameterised version above is the canonical test.
#[test]
fn picking_enum_esc_returns_to_browse() {
    let mut s = make_state();
    s.transition_to_picking_enum("theme", 0, SettingValue::Enum("echo-agent-night"), true);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    match outcome {
        SettingsKeyOutcome::Action(Action::PreviewTheme(name)) => {
            assert_eq!(
                name, "echo-agent-night",
                "Esc revert must dispatch the original canonical"
            );
        }
        other => {
            panic!("expected Action::PreviewTheme(\"echo_agent_night\") on Esc, got {other:?}")
        }
    }
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

// -- picker machinery tests --
//
// When the chooser sub-pane ships with no Enum entries in
// `default_settings()`, these tests build a
// synthetic registry containing one Enum entry and exercise the
// picker's render + keypress paths directly. When
// `action_for_enum` returns `None` for a key, the
// preview/revert dispatch returns `SettingsKeyOutcome::Changed`
// rather than `Action(_)`; a concrete `action_for_enum("theme", _)`
// arm makes it emit an Action.

/// Static synthetic Enum metadata for picker tests. Choice display
/// names are deliberately mid-length so the default-width (80) tests
/// cover the happy path and dedicated narrow-width tests cover the
/// truncation paths — sized close to the real theme catalog widths.
const TEST_ENUM_CHOICES: &[EnumChoice] = &[
    EnumChoice {
        canonical: "first",
        display: "First Option",
        description: "First option description.",
    },
    EnumChoice {
        canonical: "second",
        display: "Second Option",
        description: "Second option description.",
    },
    EnumChoice {
        canonical: "third",
        display: "Third Option",
        description: "Third option description.",
    },
];

fn synthetic_enum_meta() -> SettingMeta {
    SettingMeta {
        key: "test_enum",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Test enum",
        description: "Synthetic Enum entry for PR 3 picker tests.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "first",
            choices: TEST_ENUM_CHOICES,
            supports_preview: true,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }
}

/// Build a registry containing exactly one synthetic Enum entry
/// and place the modal directly in PickingEnum mode at idx 0.
/// Useful for testing the picker handler in isolation —
/// `try_enter_picking_enum` is tested separately by
/// `picker_test_state_in_browse()` + Enter dispatch.
fn picker_test_state() -> SettingsModalState {
    let entries = vec![synthetic_enum_meta()];
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("test_enum", 0, SettingValue::Enum("first"), true);
    s
}

/// Same registry as `picker_test_state` but the modal stays in
/// Browse mode with the Enum row focused — used to exercise
/// `try_enter_picking_enum` via the normal Browse-Enter path.
fn picker_test_state_in_browse() -> SettingsModalState {
    let entries = vec![synthetic_enum_meta()];
    SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    )
    // Selection lands on the first selectable row (the only
    // setting, "test_enum") by `SettingsModalState::new`.
}

/// Up/Down (and j/k aliases) in the picker advance `choices_idx`
/// and clamp at list bounds. With no real Enum
/// `action_for_enum` arms, the outcome is `Changed` (state
/// change only); a sibling test
/// `picker_arrow_keys_emit_preview_actions` can assert Action
/// emission once `action_for_enum("theme", _)` lands.
///
/// Renamed from `picker_arrow_keys_emit_preview_actions` — the
/// previous name promised something the test cannot actually verify
/// without a real Enum arm.
#[test]
fn picker_arrow_keys_advance_choices_idx() {
    let mut s = picker_test_state();

    // Down: 0 → 1.
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Down should produce Changed (state mutation), got {outcome:?}"
    );
    match s.mode() {
        SettingsModalMode::PickingEnum { choices_idx, .. } => assert_eq!(choices_idx, 1),
        ref other => panic!("expected PickingEnum mode after Down, got {other:?}"),
    }

    // j: 1 → 2 (last choice).
    let outcome = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    match s.mode() {
        SettingsModalMode::PickingEnum { choices_idx, .. } => assert_eq!(choices_idx, 2),
        _ => panic!("expected PickingEnum mode after j"),
    }

    // Down past the last choice is Unchanged (clamp).
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Down at last choice should be Unchanged"
    );

    // Up: 2 → 1.
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    match s.mode() {
        SettingsModalMode::PickingEnum { choices_idx, .. } => assert_eq!(choices_idx, 1),
        _ => panic!("expected PickingEnum mode after Up"),
    }

    // k: 1 → 0.
    let outcome = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    match s.mode() {
        SettingsModalMode::PickingEnum { choices_idx, .. } => assert_eq!(choices_idx, 0),
        _ => panic!("expected PickingEnum mode after k"),
    }

    // Up at first choice is Unchanged.
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
}

/// Enter commits the current preview by dispatching a typed
/// COMMIT Action AND transitioning back to Browse. The synthetic
/// `test_enum` key has no `action_for_enum_commit` arm, so the
/// outcome is `Changed` (state mutation only). The theme keys
/// land real `Action::SetTheme(...)` Action variants — exercised
/// by the e2e tests at `tests/settings_e2e.rs`.
///
/// Enter used to be a no-op
/// (relying on the most-recent preview being the committed
/// value); now it explicitly emits a commit Action so the
/// persist path runs once per picker open → close cycle.
#[test]
fn picker_enter_returns_to_browse() {
    let mut s = picker_test_state();
    // Navigate to the second choice so commit lands on a non-default.
    let _ = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    // The synthetic `test_enum` key has no commit Action arm in
    // `action_for_enum_commit`, so the outcome is `Changed`. For
    // real theme keys, the outcome is `Action(...)` — see the
    // theme preview/commit e2e coverage in the e2e crate.
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter for synthetic-key must produce Changed (no commit arm), got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter must return to Browse"
    );
}

/// Esc transitions PickingEnum → Browse AND, once a real Enum arm
/// exists, will dispatch `action_for_enum(setting_key, original_canonical)`.
/// When `action_for_enum` returns None for every key, the
/// outcome is `Changed` rather than `Action(_)` — but the
/// revert *call site* is exercised, and the assertion can be
/// tightened to verify `Action::SetTheme("first")` once the arm
/// ships.
///
/// The test cannot currently verify that the
/// *original* canonical (not the *current* preview) is passed to
/// `action_for_enum`. A real theme arm would make the distinction
/// visible via the Action variant.
#[test]
fn picker_esc_returns_to_browse_after_preview_nav() {
    let mut s = picker_test_state();
    // Preview-navigate to a non-original choice so the revert
    // path's "original vs current" distinction is meaningful.
    let _ = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let _ = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    match s.mode() {
        SettingsModalMode::PickingEnum { choices_idx, .. } => assert_eq!(choices_idx, 2),
        _ => panic!("expected PickingEnum mode after 2x Down"),
    }

    // Esc: revert. Outcome is Changed when there are no Enum action
    // arms — with a real theme arm this would unpack as Action::SetTheme("first").
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Esc revert outcome should be Changed (or Action when arms exist), got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc must return to Browse"
    );
}

/// `/privacy` deep-link: focus + enter picker with `close_on_picker_exit`,
/// then Esc closes the modal entirely (not Browse).
#[test]
fn deep_link_picker_esc_closes_modal() {
    let mut s = make_state();
    assert!(s.focus_key("coding_data_sharing"));
    assert!(s.try_enter_picking_enum());
    s.close_on_picker_exit = true;
    assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));

    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Close),
        "deep-link Esc must Close, got {outcome:?}"
    );
    assert!(
        !s.close_on_picker_exit,
        "flag must clear after Esc even when closing"
    );
}

/// Settings → Privacy row → Enter into chooser: Esc returns to Browse.
#[test]
fn browse_enter_picker_esc_returns_to_browse() {
    let mut s = make_state();
    assert!(s.focus_key("coding_data_sharing"));
    assert!(s.try_enter_picking_enum());
    assert!(!s.close_on_picker_exit);
    assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));

    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "browse-path Esc must stay open (Changed), got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "browse-path Esc must return to Browse, got {:?}",
        s.mode()
    );
}

/// Deep-link Enter commits the choice and closes the modal (not Browse).
#[test]
fn deep_link_commit_closes_modal() {
    let mut s = make_state();
    assert!(s.focus_key("coding_data_sharing"));
    assert!(s.try_enter_picking_enum());
    s.close_on_picker_exit = true;

    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match outcome {
        SettingsKeyOutcome::ActionThenClose(Action::SetCodingDataSharing { opted_in }) => {
            assert!(!opted_in, "default snapshot is opt-out");
        }
        other => panic!("expected ActionThenClose(SetCodingDataSharing), got {other:?}"),
    }
    assert!(!s.close_on_picker_exit);
}

/// Browse-path Enter commits and returns to Browse (not Close).
#[test]
fn browse_path_enter_commit_returns_to_browse() {
    let mut s = make_state();
    assert!(s.focus_key("coding_data_sharing"));
    assert!(s.try_enter_picking_enum());
    assert!(!s.close_on_picker_exit);

    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetCodingDataSharing { opted_in }) => {
            assert!(!opted_in, "default snapshot is opt-out");
        }
        other => panic!("expected Action(SetCodingDataSharing), got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "browse-path Enter must return to Browse, got {:?}",
        s.mode()
    );
    assert!(!s.close_on_picker_exit);
}

/// Deep-link Enter on a preview enum commits via Set* and closes.
#[test]
fn deep_link_theme_commit_closes_with_set() {
    let mut s = make_state();
    s.transition_to_picking_enum("theme", 0, SettingValue::Enum("echo-agent-night"), true);
    s.close_on_picker_exit = true;

    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match outcome {
        SettingsKeyOutcome::ActionThenClose(Action::SetTheme(name)) => {
            assert_eq!(name, "auto");
        }
        other => panic!("expected ActionThenClose(SetTheme), got {other:?}"),
    }
    assert!(!s.close_on_picker_exit);
}

/// Deep-link Esc on a preview enum reverts the live preview and closes.
#[test]
fn deep_link_picker_esc_reverts_preview_and_closes() {
    let mut s = make_state();
    s.transition_to_picking_enum("theme", 0, SettingValue::Enum("echo-agent-night"), true);
    s.close_on_picker_exit = true;

    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    match outcome {
        SettingsKeyOutcome::ActionThenClose(Action::PreviewTheme(name)) => {
            assert_eq!(name, "echo-agent-night");
        }
        other => panic!("expected ActionThenClose(PreviewTheme), got {other:?}"),
    }
    assert!(!s.close_on_picker_exit);
}

/// The picker renders every choice in declaration order, top to
/// bottom. Asserts each choice's `display` string and description
/// appears on the expected row with the documented spacing.
/// Layout: row 0 = title, row 1 = description (subtitle), row 2 =
/// gap, rows 3..6 = choices.
#[test]
fn picker_renders_choices_in_order() {
    let s = picker_test_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    let row_text = |y: u16| -> String {
        let mut s = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                s.push_str(cell.symbol());
            }
        }
        s
    };

    // Row 0: title.
    assert!(
        row_text(0).contains("Test enum"),
        "title row must contain 'Test enum', got: {:?}",
        row_text(0)
    );
    // Row 1: setting description subtitle.
    assert!(
        row_text(1).contains("Synthetic Enum entry"),
        "row 1 must contain setting description, got: {:?}",
        row_text(1)
    );
    // Row 2: blank gap — strict assertion.
    assert!(
        row_text(2).trim().is_empty(),
        "row 2 must be the blank gap, got: {:?}",
        row_text(2)
    );
    // Rows 3..6: choices. Use full rendered layout for precise
    // pinning (tighten substring match).
    // " ○  First Option · First option description."
    let r3 = row_text(3);
    assert!(
        r3.contains("First Option") && r3.contains("First option description"),
        "row 3 must contain display+description for 'first', got: {r3:?}"
    );
    let r4 = row_text(4);
    assert!(
        r4.contains("Second Option") && r4.contains("Second option description"),
        "row 4 must contain display+description for 'second', got: {r4:?}"
    );
    let r5 = row_text(5);
    assert!(
        r5.contains("Third Option") && r5.contains("Third option description"),
        "row 5 must contain display+description for 'third', got: {r5:?}"
    );
}

/// Focus (BG + bold) tracks `choices_idx`; the filled-disc marker
/// tracks the committed `original_value` until Enter.
#[test]
fn picker_separates_focus_highlight_from_committed_marker() {
    let mut s = picker_test_state();
    // Focus the second choice while committed value remains "first".
    s.transition_to_picking_enum("test_enum", 1, SettingValue::Enum("first"), true);
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    // Marker glyph at column `area.x + 1` of each choice row.
    // Using `area.x + 1` rather than `1` so the helper survives
    // a future renderer that passes a non-zero `area.x`.
    let marker_at = |y: u16| -> String {
        buf.cell((area.x + 1, y))
            .map(|c| c.symbol().to_string())
            .unwrap_or_default()
    };
    let marker_fg = |buf: &Buffer, y: u16| -> Option<ratatui::style::Color> {
        buf.cell((area.x + 1, y)).and_then(|c| c.style().fg)
    };
    // Layout: rows 3..5 are choices (with subtitle on row 1).
    // Row 3 = committed "first" (unfocused), row 4 = focused "second".
    assert_eq!(
        marker_at(3),
        "\u{25CF}",
        "row 3 (committed, unfocused) should be ●"
    );
    assert_eq!(
        marker_at(4),
        "\u{25CB}",
        "row 4 (focused, not committed) should be ○"
    );
    assert_eq!(marker_at(5), "\u{25CB}", "row 5 (neither) should be ○");

    // Marker accent follows committed state, not focus.
    if theme.accent_user != theme.gray {
        assert_eq!(
            marker_fg(&buf, 3),
            Some(theme.accent_user),
            "committed marker must use accent_user"
        );
        assert_eq!(
            marker_fg(&buf, 4),
            Some(theme.gray),
            "focused-but-uncommitted marker must use gray"
        );
    }

    // Cell at the LAST column of each row carries the row bg
    // independent of prefix-width tweaks. Compare via
    // `settings_list_row_bg` so terminal-native themes (Reset
    // tokens elevated to DarkGray) pass too.
    let bg_at = |y: u16| -> Option<ratatui::style::Color> {
        buf.cell((area.x + area.width - 1, y))
            .and_then(|c| c.style().bg)
    };
    assert_eq!(
        bg_at(4),
        Some(settings_list_row_bg(&theme, true, false)),
        "focused row must use selection background"
    );
    assert_eq!(
        bg_at(3),
        Some(settings_list_row_bg(&theme, false, false)),
        "committed-but-unfocused row must use base background"
    );

    // Display text on focused row carries BOLD; committed alone does not.
    let focused_modifier = buf
        .cell((area.x + PICKER_PREFIX_W, 4))
        .map(|c| c.style().add_modifier)
        .unwrap_or_default();
    assert!(
        focused_modifier.contains(Modifier::BOLD),
        "focused row's display must be BOLD, got modifiers {focused_modifier:?}"
    );
    let committed_unfocused_modifier = buf
        .cell((area.x + PICKER_PREFIX_W, 3))
        .map(|c| c.style().add_modifier)
        .unwrap_or_default();
    assert!(
        !committed_unfocused_modifier.contains(Modifier::BOLD),
        "committed-but-unfocused row must NOT be BOLD, got modifiers {committed_unfocused_modifier:?}"
    );

    // Committed + focused: filled dot and selection bg/bold together.
    s.transition_to_picking_enum("test_enum", 0, SettingValue::Enum("first"), true);
    let mut buf2 = Buffer::empty(area);
    render_picking_enum(&mut buf2, area, &s, &theme);
    let marker_at2 = |y: u16| -> String {
        buf2.cell((area.x + 1, y))
            .map(|c| c.symbol().to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        marker_at2(3),
        "\u{25CF}",
        "committed+focused row should be ●"
    );
    assert_eq!(
        marker_at2(4),
        "\u{25CB}",
        "uncommitted unfocused row should be ○"
    );
    let bg_at2 = |y: u16| -> Option<ratatui::style::Color> {
        buf2.cell((area.x + area.width - 1, y))
            .and_then(|c| c.style().bg)
    };
    assert_eq!(
        bg_at2(3),
        Some(settings_list_row_bg(&theme, true, false)),
        "committed+focused row must use selection background"
    );
    assert_eq!(
        bg_at2(4),
        Some(settings_list_row_bg(&theme, false, false)),
        "uncommitted unfocused row must use base background"
    );
    let both_modifier = buf2
        .cell((area.x + PICKER_PREFIX_W, 3))
        .map(|c| c.style().add_modifier)
        .unwrap_or_default();
    assert!(
        both_modifier.contains(Modifier::BOLD),
        "committed+focused row must be BOLD, got modifiers {both_modifier:?}"
    );
    let unfocused_modifier = buf2
        .cell((area.x + PICKER_PREFIX_W, 4))
        .map(|c| c.style().add_modifier)
        .unwrap_or_default();
    assert!(
        !unfocused_modifier.contains(Modifier::BOLD),
        "uncommitted unfocused row must NOT be BOLD, got modifiers {unfocused_modifier:?}"
    );
}

/// DynamicEnum commits use `SettingValue::String`; the marker must
/// resolve that arm the same way as static `Enum`, including the
/// empty-canonical clear sentinel (`""` / "(no override)").
#[test]
fn picker_string_original_value_fills_committed_marker() {
    let mut s = picker_test_state();
    s.transition_to_picking_enum("test_enum", 1, SettingValue::String("first".into()), true);
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    let marker_at = |y: u16| -> String {
        buf.cell((area.x + 1, y))
            .map(|c| c.symbol().to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        marker_at(3),
        "\u{25CF}",
        "String original_value \"first\" must fill row 3"
    );
    assert_eq!(
        marker_at(4),
        "\u{25CB}",
        "focused non-committed row must stay hollow"
    );

    // Empty-canonical clear sentinel (DynamicEnum "(no override)" shape):
    // empty String must fill the "" choice, not be skipped as "no value".
    const CLEAR_SENTINEL_CHOICES: &[EnumChoice] = &[
        EnumChoice {
            canonical: "",
            display: "(no override)",
            description: "Clear override.",
        },
        EnumChoice {
            canonical: "model-a",
            display: "Model A",
            description: "A model.",
        },
    ];
    let entries = vec![SettingMeta {
        key: "test_dynamic_like",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Test clear sentinel",
        description: "Catalog with empty-canonical choice.",
        keywords: &[],
        kind: SettingKind::Enum {
            default: "",
            choices: CLEAR_SENTINEL_CHOICES,
            supports_preview: false,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let mut s2 = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    // Commit empty; focus the non-empty choice so marker ≠ focus.
    s2.transition_to_picking_enum(
        "test_dynamic_like",
        1,
        SettingValue::String(String::new()),
        false,
    );
    let mut buf2 = Buffer::empty(area);
    render_picking_enum(&mut buf2, area, &s2, &theme);
    let marker_at2 = |y: u16| -> String {
        buf2.cell((area.x + 1, y))
            .map(|c| c.symbol().to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        marker_at2(3),
        "\u{25CF}",
        "empty String must fill empty-canonical clear row"
    );
    assert_eq!(
        marker_at2(4),
        "\u{25CB}",
        "focused non-empty choice must stay hollow while clear is committed"
    );
}

// -- try_enter_picking_enum coverage --

/// Browse-mode Enter on an Enum row transitions to PickingEnum
/// mode with `choices_idx` seeded from the row's current value
/// (resolved by `current_value_for`).
///
/// Since the synthetic "test_enum" key has no
/// `current_value_for` arm, `value_for` returns None and the
/// fallback path picks `choices_idx = 0` + `original_value =
/// SettingValue::Enum(first_canonical)`. This pins the
/// fallback behavior.
#[test]
fn browse_enter_on_enum_row_transitions_to_picking_enum() {
    let mut s = picker_test_state_in_browse();
    // Sanity: initial state.
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
    match &s.rows[s.selected] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, "test_enum"),
        _ => panic!("expected synthetic Enum row at initial selection"),
    }

    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on Enum row should produce Changed, got {outcome:?}"
    );
    match s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            choices_idx,
            ref original_value,
            ..
        } => {
            assert_eq!(key, "test_enum");
            // No current_value_for arm → fallback to idx 0.
            assert_eq!(choices_idx, 0);
            assert_eq!(original_value, &SettingValue::Enum("first"));
        }
        ref other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// Browse-mode Enter on a non-Enum (Bool) row does NOT enter
/// PickingEnum — `try_enter_picking_enum` returns false and the
/// fallthrough Bool-toggle path takes over.
#[test]
fn browse_enter_on_bool_row_does_not_enter_picking_enum() {
    let mut s = make_state(); // default registry — all Bool.
    // compact_mode is the initial selection.
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    // Bool toggle dispatches an Action (not Changed).
    match outcome {
        SettingsKeyOutcome::Action(_) => {}
        other => panic!("expected Action from Bool toggle, got {other:?}"),
    }
    // Mode must NOT have changed to PickingEnum.
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Bool toggle must stay in Browse mode, got {:?}",
        s.mode(),
    );
}

/// `try_enter_picking_enum` directly: with a synthetic Enum
/// where `value_for` returns the second choice's canonical, the
/// `choices_idx` must seed to 1 (the position of that canonical).
/// This exercises the `choices.iter().position(...)` resolution
/// branch that's structurally unreachable when no real Enum
/// entries are registered but is hit by the theme path.
#[test]
fn try_enter_picking_enum_seeds_choices_idx_from_current_value() {
    // The synthetic Enum key "test_enum" isn't in
    // `current_value_for`, so we can't drive this end-to-end via
    // ui_snapshot. Instead, build a registry where the key
    // matches `current_value_for("compact_mode", ...)` and override
    // SettingKind to Enum. Practical: we directly verify the
    // function returns false for non-Enum, and idx 0 + first
    // canonical for the unknown-key fallback.
    //
    // The "value-recognized → idx > 0" case requires the
    // canonical to be in `current_value_for`'s match arms; with
    // no Enum keys in those arms, this case is
    // unreachable. Adding `"theme" => SettingValue::Enum(...)`
    // and a sibling test that uses a non-default theme would verify
    // idx > 0 seeding.
    let mut s = picker_test_state_in_browse();
    assert!(s.try_enter_picking_enum());
    match s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            choices_idx,
            ref original_value,
            ..
        } => {
            assert_eq!(key, "test_enum");
            assert_eq!(choices_idx, 0, "fallback to idx 0 when no current value");
            assert_eq!(
                original_value,
                &SettingValue::Enum("first"),
                "original_value should be the first canonical fallback"
            );
        }
        ref other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// `try_enter_picking_enum` on a non-Enum focused row returns
/// false and leaves mode unchanged. The Bool case in
/// `make_state()` is the canonical non-Enum scenario.
#[test]
fn try_enter_picking_enum_returns_false_for_non_enum_row() {
    let mut s = make_state();
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
    assert!(
        !s.try_enter_picking_enum(),
        "non-Enum focused row should return false"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "mode must not change on non-Enum row"
    );
}

/// A persisted `fork_secondary_model` slug renders as the catalog display
/// name and seeds the picker on that model's row — not the stale-value
/// fallback (index 1). The catalog carries two models so a fallback seed
/// and a genuine match land on different indices.
#[test]
fn fork_secondary_model_picker_opens_on_persisted_model() {
    use agent_client_protocol as acp;
    // Must differ from the baseline slug or the empty-fold arm hides the lookup.
    let slug = "echo-agent-4.5-fast";
    assert_ne!(slug, echo_agent_runtime::models::default_model());
    let snapshot = PagerLocalSnapshot {
        available_models: vec![
            (
                "EchoAgent 3".to_string(),
                acp::ModelId::new(Arc::from("echo-agent-3")),
            ),
            (
                "EchoAgent 4.5 Fast".to_string(),
                acp::ModelId::new(Arc::from(slug)),
            ),
        ],
        ..PagerLocalSnapshot::default()
    };
    let ui = UiConfig {
        fork_secondary_model: slug.to_string(),
        ..UiConfig::default()
    };
    let mut s = SettingsModalState::new(Arc::new(SettingsRegistry::defaults()), ui, snapshot);

    // Row value shows the display name, matching the default_model row.
    assert_eq!(
        s.value_for("fork_secondary_model"),
        Some(SettingValue::String("EchoAgent 4.5 Fast".to_string())),
    );

    assert!(s.focus_key("fork_secondary_model"));
    assert!(s.try_enter_picking_enum());
    match s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            choices_idx,
            ref original_value,
            ..
        } => {
            assert_eq!(key, "fork_secondary_model");
            // Choices: [(no override), EchoAgent 3, EchoAgent 4.5 Fast] → idx 2.
            assert_eq!(
                choices_idx, 2,
                "picker must open on the persisted model, not the stale fallback",
            );
            assert_eq!(
                original_value,
                &SettingValue::String("EchoAgent 4.5 Fast".to_string()),
                "original_value must carry the display name so Esc-revert round-trips",
            );
        }
        ref other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

// -- render_picking_enum narrow-terminal coverage --

#[test]
fn render_picker_with_zero_height_is_noop() {
    let s = picker_test_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 0,
    };
    let mut buf = Buffer::empty(Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    });
    let theme = Theme::current();
    // Must not panic and must not touch the buffer.
    render_picking_enum(&mut buf, area, &s, &theme);
}

#[test]
fn render_picker_with_zero_width_is_noop() {
    let s = picker_test_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 0,
        height: 10,
    };
    let mut buf = Buffer::empty(Rect {
        x: 0,
        y: 0,
        width: 1,
        height: 10,
    });
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);
}

/// At height=2, the description-skipped path (`area.height < 4`)
/// kicks in (header_rows=2 = title+gap), and the choice-render
/// early-return fires (`area.height <= header_rows`). The title
/// must render, no choices must render, no panic.
#[test]
fn render_picker_at_height_2_renders_title_no_choices() {
    let s = picker_test_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 2,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    let row_text = |y: u16| -> String {
        let mut s = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                s.push_str(cell.symbol());
            }
        }
        s
    };
    assert!(row_text(0).contains("Test enum"));
    // Row 1 is the gap; must be blank.
    assert!(
        row_text(1).trim().is_empty(),
        "no choices should render at height=2 (only title fits), got: {:?}",
        row_text(1)
    );
}

/// Narrow-viewport edge case
/// for the word-wrap path. When the picker is given a height
/// where a fully-wrapped description would exceed the area,
/// the `has_description = … >= 2 + desc_rows` gate skips the
/// description so the choices stay renderable. This test
/// pins that fallback at an intermediate height.
#[test]
fn render_picker_drops_description_when_wrap_block_exceeds_height() {
    // Synthetic registry with a description that, at width=20,
    // would wrap to ≥ 5 lines.
    let long_desc = "This description is intentionally long enough \
                     that at narrow widths the wrap block will not \
                     fit alongside the choices.";
    let synthetic_meta = SettingMeta {
        key: "test-narrow-wrap",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "X",
        description: long_desc,
        keywords: &[],
        kind: SettingKind::Enum {
            default: "first",
            choices: TEST_ENUM_CHOICES,
            supports_preview: false,
        },
        restart_required: false,
        hidden_in_minimal: false,
    };
    let registry = SettingsRegistry::from_entries(vec![synthetic_meta]);
    let mut s = SettingsModalState::new(
        Arc::new(registry),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    assert!(s.try_enter_picking_enum());

    // Height=4 at narrow width: title (1) + desc_would_be (≥5)
    // overflows; the gate drops the description. The choices
    // should still render.
    let area = Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 4,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);
    let mut all_text = String::new();
    for y in 0..area.height {
        all_text.push_str(&buf_row_text(&buf, y, area.x, area.width));
        all_text.push('\n');
    }
    // At minimum the title and the first choice must render.
    assert!(
        all_text.contains('X'),
        "title `X` must render at narrow height: {all_text:?}",
    );
    // The fallback dropped the description, freeing space for
    // the choice marker `\u{25CB}` or `\u{25CF}` (`○`/`●`).
    let has_choice_marker = all_text.contains('\u{25CB}') || all_text.contains('\u{25CF}');
    assert!(
        has_choice_marker,
        "at least one choice marker must render when desc is dropped: {all_text:?}",
    );
}

/// Long descriptions WRAP across multiple lines (no `…`
/// truncation). User-feedback commit: word-wrap the description
/// in the picker so opinion-shaping choice copy stays readable
/// at typical terminal widths.
///
/// History note: this test originally asserted truncation
/// (`…` glyph at the right edge); the new behavior is
/// word-wrap, so the assertion inverts — `…` must NOT appear
/// and the full description text must be in the buffer.
#[test]
fn render_picker_long_description_wraps_no_ellipsis() {
    let entries = vec![SettingMeta {
        key: "long_enum",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Long",
        description: "Short.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "wide",
            choices: &[EnumChoice {
                canonical: "wide",
                display: "Wide",
                description: "A deliberately verbose description that will overflow the column budget.",
            }],
            supports_preview: true,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("long_enum", 0, SettingValue::Enum("wide"), true);
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    // Concatenate ALL rendered rows. The full description must
    // be present (no `…` ellipsis anywhere on the choice rows).
    let mut all_choice_text = String::new();
    for y in 3..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                all_choice_text.push_str(cell.symbol());
            }
        }
        all_choice_text.push('\n');
    }
    assert!(
        !all_choice_text.contains('\u{2026}'),
        "wrapped description must NOT contain `…`, got:\n{all_choice_text}"
    );
    // Words appear across wrapped lines (with trailing-padding
    // whitespace between them), so we check word-presence
    // rather than substring contiguity.
    for word in [
        "A",
        "deliberately",
        "verbose",
        "description",
        "overflow",
        "budget",
    ] {
        assert!(
            all_choice_text.contains(word),
            "wrapped description must contain word {word:?}, got:\n{all_choice_text}"
        );
    }
}

/// Word-wrap detail check: the choice symbol + display name stay
/// on line 1, the description wraps across ≥2 lines, AND
/// continuation lines are indented to the description column
/// (column 0 holds whitespace, NOT a marker glyph).
///
/// Uses the production `coding_data_sharing` "Opt out" choice
/// (a long description that wraps at width=60). Pinning against
/// the real catalog keeps the test honest about the bug report
/// — the screenshot in the user-feedback PR showed exactly this
/// choice clipped with `…`.
/// Visual smoke debugging helper. Renders the wrap fixture and
/// prints the buffer so a human can eyeball the layout. Ignored
/// by default; run with `cargo test -- --ignored picker_visual_smoke_debug
/// --nocapture`.
#[test]
#[ignore]
fn picker_visual_smoke_debug() {
    let entries = vec![SettingMeta {
        key: "wrap_enum",
        category: SettingCategory::Privacy,
        owner: SettingOwner::Shared,
        label: "Coding data sharing",
        description: "Controls whether EchoAgent may retain and train on coding data.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "opt-out",
            choices: &[
                EnumChoice {
                    canonical: "opt-in",
                    display: "Opt in",
                    description: "Allow EchoAgent to retain and use coding session data for training and product improvement.",
                },
                EnumChoice {
                    canonical: "opt-out",
                    display: "Opt out",
                    description: "Do not retain coding session data. Code requests will not be used for training.",
                },
            ],
            supports_preview: false,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("wrap_enum", 1, SettingValue::Enum("opt-out"), false);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);
    println!("\nPicker visual smoke at width=60:");
    println!("{}", "─".repeat(area.width as usize));
    for y in 0..area.height {
        let mut row = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                row.push_str(cell.symbol());
            }
        }
        println!("{row}");
    }
    println!("{}", "─".repeat(area.width as usize));
}

#[test]
fn picker_long_description_wraps_to_multiple_lines() {
    let entries = vec![SettingMeta {
        key: "wrap_enum",
        category: SettingCategory::Privacy,
        owner: SettingOwner::Shared,
        label: "Coding data sharing",
        description: "Controls whether EchoAgent may retain and train on coding data.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "opt-out",
            choices: &[
                EnumChoice {
                    canonical: "opt-in",
                    display: "Opt in",
                    description: "Allow EchoAgent to retain and use coding session data for training and product improvement.",
                },
                EnumChoice {
                    canonical: "opt-out",
                    display: "Opt out",
                    description: "Do not retain coding session data. Code requests will not be used for training.",
                },
            ],
            supports_preview: false,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("wrap_enum", 1, SettingValue::Enum("opt-out"), false);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 16,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    let row_text = |y: u16| -> String {
        let mut s = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                s.push_str(cell.symbol());
            }
        }
        s
    };

    // Line 1 of choice 0 ("Opt in"): symbol + display + sep +
    // start-of-description. The setting-level description above the
    // choices can wrap to a variable number of rows depending on copy
    // length, so locate choice 0's line 1 dynamically instead of
    // assuming a fixed row.
    let opt_in_row = (0..area.height)
        .find(|&y| row_text(y).contains("Opt in"))
        .expect("choice 0 line 1 ('Opt in') must render within the picker area");
    let r3 = row_text(opt_in_row);
    assert!(
        r3.contains("Opt in"),
        "choice 0 line 1 must contain 'Opt in' display, got: {r3:?}"
    );
    assert!(
        r3.contains('\u{00B7}'),
        "choice 0 line 1 must contain the `·` separator, got: {r3:?}"
    );
    assert!(
        r3.contains("Allow EchoAgent"),
        "choice 0 line 1 must start the description, got: {r3:?}"
    );

    // The Opt-in description wraps to ≥ 2 lines at width 60.
    // Continuation rows live BELOW choice 0's line 1 until the
    // next choice starts.
    let cont_row = opt_in_row + 1;
    let r4 = row_text(cont_row);
    // Continuation line must be indented past the description
    // column. Column 0 + column 1 (symbol cells on line 1)
    // should be whitespace on the continuation line.
    assert_eq!(
        buf.cell((0u16, cont_row)).map(|c| c.symbol()),
        Some(" "),
        "continuation row col 0 must be whitespace, got row: {r4:?}"
    );
    assert_eq!(
        buf.cell((1u16, cont_row)).map(|c| c.symbol()),
        Some(" "),
        "continuation row col 1 (where marker would sit on line 1) must be whitespace, got row: {r4:?}"
    );

    // The full Opt-in description must appear across the wrapped
    // rows — no `…` truncation.
    let mut opt_in_full = String::new();
    for y in opt_in_row..area.height {
        let line = row_text(y);
        // Stop at the start of the Opt-out choice (line that
        // contains the second display).
        if y > opt_in_row && line.contains("Opt out") {
            break;
        }
        opt_in_full.push_str(&line);
        opt_in_full.push('\n');
    }
    assert!(
        !opt_in_full.contains('\u{2026}'),
        "wrapped Opt-in description must NOT contain `…`, got:\n{opt_in_full}"
    );
    for word in [
        "Allow",
        "EchoAgent",
        "retain",
        "session",
        "training",
        "improvement",
    ] {
        assert!(
            opt_in_full.contains(word),
            "Opt-in description must include word {word:?}, got:\n{opt_in_full}"
        );
    }
}

/// Short descriptions stay on ONE line — no continuation rows.
/// Asserts the row directly below a choice's line 1 is either
/// the next choice's line 1 (when there are more choices) or
/// blank.
#[test]
fn picker_short_description_stays_one_line() {
    let entries = vec![SettingMeta {
        key: "short_enum",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Short",
        description: "Short.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "a",
            choices: &[
                EnumChoice {
                    canonical: "a",
                    display: "Alpha",
                    description: "A.",
                },
                EnumChoice {
                    canonical: "b",
                    display: "Bravo",
                    description: "B.",
                },
            ],
            supports_preview: true,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("short_enum", 0, SettingValue::Enum("a"), true);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    let row_text = |y: u16| -> String {
        let mut s = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                s.push_str(cell.symbol());
            }
        }
        s
    };

    // Choice 0 on row 3, choice 1 on row 4 (one line each).
    assert!(
        row_text(3).contains("Alpha") && row_text(3).contains("A."),
        "choice 0 must be one line, got row 3: {:?}",
        row_text(3)
    );
    assert!(
        row_text(4).contains("Bravo") && row_text(4).contains("B."),
        "choice 1 must start at row 4 (no continuation rows from choice 0), got row 4: {:?}",
        row_text(4)
    );
}

/// Choices with an empty description render the symbol + display
/// ONLY — no `·` separator, no trailing stray cells.
#[test]
fn picker_no_description_renders_symbol_and_display_only() {
    let entries = vec![SettingMeta {
        key: "nodesc_enum",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "No desc",
        description: "Short.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "a",
            choices: &[
                EnumChoice {
                    canonical: "a",
                    display: "Alpha",
                    description: "",
                },
                EnumChoice {
                    canonical: "b",
                    display: "Bravo",
                    description: "",
                },
            ],
            supports_preview: true,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("nodesc_enum", 0, SettingValue::Enum("a"), true);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    let row_text = |y: u16| -> String {
        let mut s = String::new();
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                s.push_str(cell.symbol());
            }
        }
        s
    };

    let r3 = row_text(3);
    assert!(
        r3.contains("Alpha"),
        "choice 0 must render its display, got: {r3:?}"
    );
    assert!(
        !r3.contains('\u{00B7}'),
        "choice 0 with empty description must NOT render the `·` separator, got: {r3:?}"
    );
}

/// Multi-line choice hit-rect spans ALL its lines. Clicking on
/// the continuation line of a wrapped choice moves the picker
/// focus to that choice (same as clicking line 1). Mirrors the
/// commit-13 fix for two-line row hit-rects in Browse mode.
#[test]
fn picker_multi_line_choice_hit_rect_spans_all_lines() {
    // Reuse the wrap fixture: long descriptions on both choices.
    let entries = vec![SettingMeta {
        key: "wrap_enum",
        category: SettingCategory::Privacy,
        owner: SettingOwner::Shared,
        label: "Coding data sharing",
        description: "Controls whether EchoAgent may retain coding data.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "opt-in",
            choices: &[
                EnumChoice {
                    canonical: "opt-in",
                    display: "Opt in",
                    description: "Allow EchoAgent to retain and use coding session data for training and product improvement.",
                },
                EnumChoice {
                    canonical: "opt-out",
                    display: "Opt out",
                    description: "Do not retain coding session data. Code requests will not be used for training.",
                },
            ],
            supports_preview: false,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("wrap_enum", 0, SettingValue::Enum("opt-in"), false);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 16,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);
    // Drain the per-choice rects (the production caller does
    // this via `render_settings_modal`; we mirror it here so the
    // mouse handler sees the rects).
    s.picker_choice_rects = take_picker_choice_rects();

    // Sanity: each choice's rect has height ≥ 2 at width 60.
    assert_eq!(
        s.picker_choice_rects.len(),
        2,
        "expected 2 choice hit-rects, got {}",
        s.picker_choice_rects.len()
    );
    let rect0 = s.picker_choice_rects[0];
    let rect1 = s.picker_choice_rects[1];
    assert!(
        rect0.height >= 2,
        "choice 0 should span ≥ 2 lines (wrapped description), got rect {rect0:?}"
    );
    assert!(
        rect1.height >= 2,
        "choice 1 should span ≥ 2 lines (wrapped description), got rect {rect1:?}"
    );
    // Rects must NOT overlap vertically.
    assert!(
        rect0.y + rect0.height <= rect1.y,
        "choice rects must not overlap: rect0={rect0:?}, rect1={rect1:?}"
    );

    // The initial focus is on choice 0. Click on the last line
    // (a continuation line) of choice 1 — the click should
    // move focus to choice 1.
    let click_y = rect1.y + rect1.height - 1;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        click_y,
    );
    match (outcome, &s.mode()) {
        (
            SettingsKeyOutcome::Changed | SettingsKeyOutcome::Action(_),
            SettingsModalMode::PickingEnum { choices_idx, .. },
        ) => {
            assert_eq!(
                *choices_idx, 1,
                "click on continuation row of choice 1 must focus choice 1, got idx {}",
                *choices_idx
            );
        }
        (other, mode) => panic!(
            "click on continuation line should change focus, got outcome {other:?} in mode {mode:?}"
        ),
    }
}

/// Picker scroll math accounts for variable per-choice height.
/// With 5 choices each ~3 lines tall in a ~8-line viewport,
/// focusing the LAST choice scrolls so it's visible (and the
/// earlier choices may shift off the top).
#[test]
fn picker_scroll_offset_accounts_for_variable_height() {
    // Each description is ≥ 3 wrap lines wide at width=40.
    let entries = vec![SettingMeta {
        key: "many_wrap",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Many",
        description: "Many.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "c0",
            choices: &[
                EnumChoice {
                    canonical: "c0",
                    display: "C0",
                    description: "Choice zero description that is verbose enough to span three lines at width 40.",
                },
                EnumChoice {
                    canonical: "c1",
                    display: "C1",
                    description: "Choice one description that is verbose enough to span three lines at width 40.",
                },
                EnumChoice {
                    canonical: "c2",
                    display: "C2",
                    description: "Choice two description that is verbose enough to span three lines at width 40.",
                },
                EnumChoice {
                    canonical: "c3",
                    display: "C3",
                    description: "Choice three description that is verbose enough to span three lines at width 40.",
                },
                EnumChoice {
                    canonical: "c4",
                    display: "C4",
                    description: "Choice four description that is verbose enough to span three lines at width 40.",
                },
            ],
            supports_preview: true,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let registry = Arc::new(SettingsRegistry::from_entries(entries));
    // Focus the LAST choice (c4) — the scroll math must keep it
    // in view.
    let mut s = SettingsModalState::new(
        registry.clone(),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("many_wrap", 4, SettingValue::Enum("c4"), true);
    // Viewport: title + desc + gap = 3 rows of chrome + 8 rows
    // of choices = 11 total. With 5 choices × 3 lines = 15 total
    // wrap-rows of content, only ~2 choices can fit per page.
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 11,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);
    s.picker_choice_rects = take_picker_choice_rects();

    // The focused choice (c4) MUST have a non-zero hit rect (it
    // got rendered).
    let rect_c4 = s.picker_choice_rects[4];
    assert!(
        rect_c4.width > 0 && rect_c4.height > 0,
        "focused choice c4 must be visible after scroll, got rect {rect_c4:?}"
    );
    // The focused choice's rect must fit inside the area's
    // height bounds.
    assert!(
        rect_c4.y + rect_c4.height <= area.y + area.height,
        "focused choice c4 must fit inside the viewport, got rect {rect_c4:?} vs area {area:?}"
    );
    // Choice 0 (c0) should be scrolled off the top (rect zero).
    let rect_c0 = s.picker_choice_rects[0];
    assert_eq!(
        (rect_c0.width, rect_c0.height),
        (0, 0),
        "choice c0 must be scrolled off-screen, got rect {rect_c0:?}"
    );
}

/// Long choice display names get truncated with `…`. The bug was
/// that `display` rendered via
/// raw `set_span` without `truncate_str`, producing mid-character
/// clips. Same shape as the description test above.
#[test]
fn render_picker_truncates_long_display_with_ellipsis() {
    let entries = vec![SettingMeta {
        key: "long_enum",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Long",
        description: "Short.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "wide",
            choices: &[EnumChoice {
                canonical: "wide",
                display: "An absurdly long display name designed to overflow",
                description: "Short desc.",
            }],
            supports_preview: true,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("long_enum", 0, SettingValue::Enum("wide"), true);
    let area = Rect {
        x: 0,
        y: 0,
        width: 24,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    let mut row_text = String::new();
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, 3)) {
            row_text.push_str(cell.symbol());
        }
    }
    assert!(
        row_text.contains('\u{2026}'),
        "long display must truncate with `…`, got: {row_text:?}"
    );
}

/// Long setting label in the title row truncates with `…`.
#[test]
fn render_picker_truncates_long_title_with_ellipsis() {
    let entries = vec![SettingMeta {
        key: "long_enum",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "An exceptionally verbose setting label that overflows",
        description: "Short.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "a",
            choices: &[EnumChoice {
                canonical: "a",
                display: "A",
                description: "A.",
            }],
            supports_preview: true,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("long_enum", 0, SettingValue::Enum("a"), true);
    let area = Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    let mut title_text = String::new();
    for x in 0..area.width {
        if let Some(cell) = buf.cell((x, 0)) {
            title_text.push_str(cell.symbol());
        }
    }
    assert!(
        title_text.contains('\u{2026}'),
        "long title must truncate with `…`, got: {title_text:?}"
    );
}

/// When choices > visible_h, the picker renders an overflow
/// indicator `… N more` on the last visible row.
#[test]
fn render_picker_shows_more_indicator_when_choices_overflow() {
    // Build a registry with 8 choices (exceeds 4-row viewport
    // at height=8).
    let entries = vec![SettingMeta {
        key: "long_enum",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Many",
        description: "Many choices.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "c0",
            choices: &[
                EnumChoice {
                    canonical: "c0",
                    display: "C0",
                    description: "0",
                },
                EnumChoice {
                    canonical: "c1",
                    display: "C1",
                    description: "1",
                },
                EnumChoice {
                    canonical: "c2",
                    display: "C2",
                    description: "2",
                },
                EnumChoice {
                    canonical: "c3",
                    display: "C3",
                    description: "3",
                },
                EnumChoice {
                    canonical: "c4",
                    display: "C4",
                    description: "4",
                },
                EnumChoice {
                    canonical: "c5",
                    display: "C5",
                    description: "5",
                },
            ],
            supports_preview: true,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }];
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::from_entries(entries)),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_picking_enum("long_enum", 0, SettingValue::Enum("c0"), true);
    // Total height 7 → header_rows=3 (title+desc+gap) + 4 choices
    // rows. With 6 choices, 4 fit in viewport - 1 (reserved for
    // overflow). So 3 visible, 3 hidden → "… 3 more".
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 7,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_picking_enum(&mut buf, area, &s, &theme);

    // Scan all rows for the overflow indicator.
    let mut all_text = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                all_text.push_str(cell.symbol());
            }
        }
        all_text.push('\n');
    }
    assert!(
        all_text.contains('\u{2026}') && all_text.contains("more"),
        "overflow indicator '… N more' must render, got:\n{all_text}"
    );
}

// -- render_settings_modal routing coverage --

/// `render_settings_modal` branches on mode → picker render path.
/// Verifies that the search-bar placeholder text is NOT present
/// (proves the picker branch fired and the Browse path was
/// skipped) AND that hit-test rects are reset on entry.
#[test]
fn render_settings_modal_routes_to_picker_when_mode_is_picking_enum() {
    let mut s = picker_test_state();
    // Pre-populate row_rects so we can verify reset_hit_rects().
    s.row_rects = vec![
        Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 1,
        };
        s.rows.len()
    ];
    s.list_area = Rect {
        x: 0,
        y: 0,
        width: 10,
        height: 10,
    };

    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);

    // Scan the buffer for the Browse-mode search-bar placeholder.
    // If the picker branch fired, this string should NOT appear.
    let mut all_text = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                all_text.push_str(cell.symbol());
            }
        }
        all_text.push('\n');
    }
    assert!(
        !all_text.contains("/ to search"),
        "picker mode must not render the Browse-mode search bar"
    );
    // The picker's setting label should be visible.
    assert!(
        all_text.contains("Test enum"),
        "picker mode must render the setting label"
    );
    // Hit-test rects must be reset.
    assert!(
        s.row_rects.iter().all(|r| r == &Rect::default()),
        "row_rects should be reset to default on picker entry, got: {:?}",
        s.row_rects
    );
}

// -- mouse + catch-all coverage --

/// Scroll wheel during PickingEnum mode is a no-op AND does NOT
/// mutate `state.selected` (the underlying Browse selection).
/// Regression test.
#[test]
fn picker_mode_scroll_wheel_is_noop_and_preserves_browse_selection() {
    let mut s = picker_test_state();
    let selected_before = s.selected;
    let outcome = handle_settings_mouse(&mut s, MouseEventKind::ScrollDown, 10, 5);
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Scroll in picker mode must be Unchanged, got {outcome:?}"
    );
    assert_eq!(
        s.selected, selected_before,
        "scroll in picker mode must NOT mutate Browse selection"
    );

    let outcome = handle_settings_mouse(&mut s, MouseEventKind::ScrollUp, 10, 5);
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
    assert_eq!(s.selected, selected_before);
}

/// Mouse click in PickingEnum mode is a no-op (click-to-pick is
/// handled elsewhere).
#[test]
fn picker_mode_mouse_click_is_noop() {
    let mut s = picker_test_state();
    let selected_before = s.selected;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        5,
        5,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
    assert_eq!(s.selected, selected_before);
}

/// Random keypresses in PickingEnum mode are Unchanged and don't
/// leak to other handlers (e.g., the filter query).
#[test]
fn picker_ignores_random_keypress() {
    let mut s = picker_test_state();
    let outcome = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
    assert!(
        matches!(
            s.mode(),
            SettingsModalMode::PickingEnum { choices_idx: 0, .. }
        ),
        "mode must remain PickingEnum after random keypress"
    );
}

/// EditingValue: char keys mutate the buffer (Changed),
/// Enter on an empty / invalid buffer is a no-op (the validator
/// gate refuses commit), and Esc returns to Browse.
///
/// **History note:** an earlier
/// scaffold test (`editing_value_ignores_non_esc_keys`) asserted
/// every non-Esc key returned `Unchanged`. The editor is now wired
/// for real, so chars mutate the buffer and the test
/// inverts: chars produce `Changed` (buffer mutation), Enter on
/// an invalid buffer produces `Unchanged` (validator refuses).
///
/// Uses a snapshot with a populated `available_models` list
/// so the `KnownModel` validator has data to validate against —
/// otherwise an empty catalog short-circuits to "valid" (defense
/// in depth — the dispatcher's resolution step is the
/// belt-and-suspenders backstop).
#[test]
fn editing_value_chars_mutate_buffer_and_invalid_enter_is_noop() {
    // `default_model` is now a `SettingKind::DynamicEnum`, so the
    // production catalog no
    // longer wires the String editor. We construct a synthetic
    // registry to keep the editor-mode contract under test —
    // `editor_render_fixture` uses the same pattern.
    let mut s = editor_render_fixture("", 0);
    // Char 'a' goes into the buffer → Changed.
    let outcome = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "char insert in EditingValue must be Changed, got {outcome:?}"
    );
    assert_eq!(s.editing_buffer(), Some("a"));
    assert!(
        s.editing_validation_error().is_some(),
        "validation_error must be Some for unknown model 'a' \
         (catalog has 'EchoAgent 4 Fast' only)",
    );

    // Enter on a buffer that fails the KnownModel validator
    // (catalog has 'EchoAgent 4 Fast'; "a" doesn't match) is
    // Unchanged — commit refused.
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Enter on invalid buffer must be Unchanged, got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::EditingValue { .. }),
        "Enter on invalid buffer must keep EditingValue mode (no commit)"
    );
}

#[test]
fn string_editor_uses_canonical_edits_policy_and_live_validation() {
    let mut state = editor_render_fixture("alpha-beta", "alpha-beta".len());
    let outcome = handle_settings_key(
        &mut state,
        &KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(state.editing_buffer(), Some("alpha-"));

    let mut state = editor_render_fixture("EchoAgent Tes", "EchoAgent Tes".len());
    assert!(state.editing_validation_error().is_some());
    let _ = handle_settings_key(
        &mut state,
        &KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
    );
    assert!(
        state.editing_validation_error().is_some(),
        "cursor motion must preserve validation state",
    );
    let _ = handle_settings_key(&mut state, &KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    let _ = handle_settings_key(
        &mut state,
        &KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
    );
    assert_eq!(state.editing_buffer(), Some("EchoAgent Test"));
    assert!(state.editing_validation_error().is_none());

    let cursor = state.editing_cursor_byte();
    let outcome = handle_settings_key(
        &mut state,
        &KeyEvent::new(KeyCode::Char('\u{202e}'), KeyModifiers::NONE),
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(state.editing_buffer(), Some("EchoAgent Test"));
    assert_eq!(state.editing_cursor_byte(), cursor);

    let outcome = handle_settings_key(
        &mut state,
        &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(matches!(
        outcome,
        SettingsKeyOutcome::Action(Action::SetDefaultModel(_))
    ));
    assert!(matches!(state.mode(), SettingsModalMode::Browse));
}

// -- helper-function coverage --

/// `picker_choices_len` returns 0 for an unknown key, a non-Enum
/// key, and a zero-choice Enum.
#[test]
fn picker_choices_len_handles_missing_and_non_enum() {
    let s = picker_test_state_in_browse();
    // Unknown key → 0.
    assert_eq!(picker_choices_len(&s, "unknown-key-xyzzy"), 0);
    // The synthetic registry contains only "test_enum" — there's
    // no Bool to test against without rebuilding. Test the
    // non-Enum case via the default registry instead.
    let bool_state = make_state();
    assert_eq!(picker_choices_len(&bool_state, "compact_mode"), 0);
}

#[test]
fn picker_choice_at_returns_none_for_oob_and_missing() {
    let s = picker_test_state_in_browse();
    assert_eq!(picker_choice_at(&s, "test_enum", 0), Some("first"));
    assert_eq!(picker_choice_at(&s, "test_enum", 99), None);
    assert_eq!(picker_choice_at(&s, "unknown-key", 0), None);

    let bool_state = make_state();
    assert_eq!(picker_choice_at(&bool_state, "compact_mode", 0), None);
}

#[test]
fn editing_value_esc_returns_to_browse() {
    let mut s = int_stepper_fixture(120);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

// -- Direct unit tests for compute_filtered --
//
// The free function is module-private; integration tests can only
// reach it through the key-press surface. These unit tests pin
// structural cases that are not naturally reachable through that
// surface (e.g.,
// a category whose settings all fail the filter — header excluded).

#[test]
fn compute_filtered_empty_query_returns_identity() {
    let rows = vec![
        RowEntry::Header {
            category: SettingCategory::Appearance,
        },
        RowEntry::Setting {
            key: "compact_mode",
            meta_index: 0,
        },
        RowEntry::Setting {
            key: "show_timestamps",
            meta_index: 1,
        },
    ];
    let registry = SettingsRegistry::defaults();
    let result = compute_filtered(&rows, &registry, "");
    assert_eq!(result, vec![0, 1, 2]);
}

#[test]
fn compute_filtered_excludes_header_when_no_children_match() {
    // Synthetic rows: header + setting that doesn't match.
    // Header MUST be excluded — emitting an orphan header is a
    // visual bug.
    let rows = vec![
        RowEntry::Header {
            category: SettingCategory::Appearance,
        },
        RowEntry::Setting {
            key: "compact_mode",
            meta_index: 0,
        },
    ];
    let registry = SettingsRegistry::defaults();
    let result = compute_filtered(&rows, &registry, "xyzzy-no-match");
    assert!(
        result.is_empty(),
        "header must be excluded when no settings in its section match, got {result:?}"
    );
}

#[test]
fn compute_filtered_single_word_match_emits_header_then_setting() {
    let rows = vec![
        RowEntry::Header {
            category: SettingCategory::Appearance,
        },
        RowEntry::Setting {
            key: "compact_mode",
            meta_index: 0,
        },
        RowEntry::Setting {
            key: "show_timestamps",
            meta_index: 1,
        },
        RowEntry::Setting {
            key: "simple_mode",
            meta_index: 2,
        },
    ];
    let registry = SettingsRegistry::defaults();
    // "density" is a compact_mode keyword only.
    let result = compute_filtered(&rows, &registry, "density");
    assert_eq!(result, vec![0, 1], "header then compact_mode in order");
}

#[test]
fn compute_filtered_multi_word_and_match_narrows_further() {
    let rows = vec![
        RowEntry::Header {
            category: SettingCategory::Appearance,
        },
        RowEntry::Setting {
            key: "compact_mode",
            meta_index: 0,
        },
    ];
    let registry = SettingsRegistry::defaults();
    // Both "compact" and "density" are compact_mode keywords.
    let result = compute_filtered(&rows, &registry, "compact density");
    assert_eq!(result, vec![0, 1]);
}

/// `advance_next`'s `None`-arm defensive path: when `selected`
/// is manually mutated to a row hidden by the filter, the next
/// navigation jumps to the FIRST visible setting (Down → top).
/// This exercises the asymmetric-defensive-path documented in
/// `advance_next`/`advance_prev`.
#[test]
fn advance_next_recovers_when_selection_is_hidden() {
    let mut s = make_state();
    // Apply a filter that hides compact_mode.
    s.set_query("stamp");
    // Manually corrupt selected to a HIDDEN row (compact_mode is
    // row 1, hidden by "stamp"). This bypasses
    // clamp_selected_to_visible and exercises the defensive arm.
    let compact_idx = s
        .rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key, .. } if *key == "compact_mode"))
        .unwrap();
    s.selected = compact_idx;
    // Advance: lands on the first visible setting (show_timestamps).
    let moved = s.advance_next();
    assert!(moved);
    let show_ts_idx = s
        .rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key, .. } if *key == "show_timestamps"))
        .unwrap();
    assert_eq!(s.selected, show_ts_idx);
}

/// Counterpart to `advance_next_recovers_when_selection_is_hidden`:
/// when `selected` is on a hidden row, Up lands on the LAST
/// visible setting. Asymmetric by design (each picks the nearest
/// end of the filter from the user's perspective).
#[test]
fn advance_prev_recovers_when_selection_is_hidden() {
    let mut s = make_state();
    // The filter must match exactly one setting, so the "LAST visible"
    // target is unambiguous. `ascii` is a simple_mode keyword and hits
    // nothing else (settings_e2e pins that). Corrupt `selected` to the
    // now-hidden compact_mode; Up must land on simple_mode.
    s.set_query("ascii");
    let compact_idx = s
        .rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key, .. } if *key == "compact_mode"))
        .unwrap();
    s.selected = compact_idx;
    let moved = s.advance_prev();
    assert!(moved);
    let simple_idx = s
        .rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key, .. } if *key == "simple_mode"))
        .unwrap();
    assert_eq!(s.selected, simple_idx);
}

// -- blank line above category section headers --
//
// The renderer reserves one empty visual line ABOVE every section
// header EXCEPT the one that lands first in the viewport. These
// tests render the modal directly to a buffer and inspect the
// y-positions of the rendered category labels.

/// Scan one row of the buffer and return its text content (no
/// styles) with leading/trailing whitespace preserved.
fn buf_row_text(buf: &Buffer, y: u16, x: u16, width: u16) -> String {
    let mut s = String::new();
    for col in x..x.saturating_add(width) {
        if let Some(cell) = buf.cell((col, y)) {
            s.push_str(cell.symbol());
        }
    }
    s
}

/// Find the absolute column index where `needle` begins on row
/// `y` of `buf`, scanning from `x_start` to `x_end - 1`. Walks
/// cells one at a time and compares each cell's symbol so the
/// returned column is the actual buffer position — not a byte
/// offset projected onto width as `text.find(needle)` would
/// give. Returns `None` if `needle` doesn't appear on the row.
///
/// Use this in any test that needs to assert per-cell style for
/// a text fragment. Earlier tests cast
/// `string.find(needle) as u16` which only matched the cell
/// position for ASCII content; once a unicode glyph entered the
/// row's label/value, the byte offset diverged from the column.
fn find_text_col(buf: &Buffer, y: u16, needle: &str) -> Option<u16> {
    if needle.is_empty() {
        return None;
    }
    // Sweep the row. For each starting column, compare the
    // sequence of cell symbols against the needle's chars.
    let area = buf.area;
    let needle_chars: Vec<&str> =
        unicode_segmentation::UnicodeSegmentation::graphemes(needle, true).collect();
    let x_start = area.x;
    let x_end = area.x.saturating_add(area.width);
    for x in x_start..x_end {
        let mut col = x;
        let mut all_match = true;
        for &grapheme in &needle_chars {
            let Some(cell) = buf.cell((col, y)) else {
                all_match = false;
                break;
            };
            if cell.symbol() != grapheme {
                all_match = false;
                break;
            }
            col = col.saturating_add(grapheme.width() as u16);
            if col >= x_end {
                // Needle is partially past the right edge.
                all_match = false;
                break;
            }
        }
        if all_match {
            return Some(x);
        }
    }
    None
}

/// Render the row list with default registry; assert that every
/// section header AFTER the first has a blank line immediately
/// above it.
#[test]
fn section_headers_have_blank_line_above_except_first() {
    let mut s = make_state();
    // Allocate a generous viewport so every category fits — the
    // default registry contains 6 categories with 16 settings;
    // the blank lines push us to ~23 lines, fits in 60.
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 60,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_rows(&mut buf, area, &mut s, &theme);

    // Collect each category's expected label + the y position of
    // its rendered label line. Headers render with their full
    // label (e.g. "Appearance"); we locate each header by
    // searching for its label as the row content.
    let mut header_ys: Vec<(u16, &'static str)> = Vec::new();
    for cat in SettingCategory::ALL {
        // Skip categories the default registry doesn't populate
        // (e.g. Session — no settings registered).
        let has_setting = s
            .rows
            .iter()
            .any(|r| matches!(r, RowEntry::Header { category } if category == cat));
        if !has_setting {
            continue;
        }
        let label = cat.label();
        for y in 0..area.height {
            let row_text = buf_row_text(&buf, y, area.x, area.width);
            if row_text.trim_start().starts_with(label) {
                header_ys.push((y, label));
                break;
            }
        }
    }
    assert!(
        header_ys.len() >= 2,
        "this test requires ≥2 rendered headers, got: {header_ys:?}"
    );
    // First header has NO blank line above it — it hugs the top.
    let (first_y, _) = header_ys[0];
    assert_eq!(
        first_y, area.y,
        "first section header must sit at the top of the list area, got y={first_y}"
    );

    // Every subsequent header has an EMPTY visual line immediately above.
    for &(y, label) in header_ys.iter().skip(1) {
        assert!(
            y > area.y,
            "section header `{label}` rendered above the top of the area (y={y})"
        );
        let above = buf_row_text(&buf, y - 1, area.x, area.width);
        assert!(
            above.chars().all(|c| c == ' '),
            "line above section header `{label}` must be blank, got: {above:?}"
        );
    }
}

/// When the viewport begins at a section header, we do NOT
/// reserve a leading blank line above it — the header hugs the
/// top of the row-list area.
#[test]
fn first_section_header_has_no_leading_gap() {
    let mut s = make_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 60,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_rows(&mut buf, area, &mut s, &theme);

    // The very first row of the area must contain the first
    // category's label (Appearance is first in
    // `SettingCategory::ALL` and is registered by
    // `default_settings()`).
    let first_row = buf_row_text(&buf, area.y, area.x, area.width);
    let appearance = SettingCategory::Appearance.label();
    assert!(
        first_row.trim_start().starts_with(appearance),
        "first rendered line must be the `{appearance}` header, got: {first_row:?}"
    );
}

/// Row hit-rects must remain aligned with the actually-rendered
/// y positions when blank lines are inserted above section
/// headers. Click on a setting row's y-coordinate should match
/// the rect stored in `state.row_rects`.
#[test]
fn row_rects_shift_down_for_blank_lines_above_headers() {
    let mut s = make_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 60,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_rows(&mut buf, area, &mut s, &theme);

    // For every row, the stored rect's y must match the actual
    // rendered y. We verify this by checking that the row's
    // content (Header label or Setting label) appears at the
    // rect's y position. Skip rows with default Rect (off-screen).
    for (i, r) in s.rows.iter().enumerate() {
        let rect = s.row_rects[i];
        if rect.width == 0 {
            continue;
        }
        let row_text = buf_row_text(&buf, rect.y, area.x, area.width);
        let expected_substring = match r {
            RowEntry::Header { category } => category.label().to_string(),
            RowEntry::Setting { meta_index, .. } => s.registry.all()[*meta_index].label.to_string(),
        };
        assert!(
            row_text.contains(&expected_substring),
            "row {i} rect.y={} should contain `{expected_substring}` but got: {row_text:?}",
            rect.y,
        );
    }
}

// -- two-line layout when label + value don't fit --
//
// The `row_layout` helper decides one-line vs two-line vs
// two-line-with-label-truncation based on the full label width.
// These tests pin the behaviour at three width budgets that
// exercise each layout variant.

/// Unit test for the extracted
/// `wrap_description` helper — pins its behavior in one place
/// so each of the three callers doesn't have to assert it
/// indirectly through a modal-render check.
#[test]
fn wrap_description_empty_and_zero_width_return_empty() {
    assert!(wrap_description("", 80).is_empty());
    assert!(wrap_description("anything", 0).is_empty());
}

#[test]
fn wrap_description_single_line_when_fits() {
    let wrapped = wrap_description("Short text.", 80);
    assert_eq!(wrapped.len(), 1);
    assert_eq!(wrapped[0], "Short text.");
}

#[test]
fn wrap_description_splits_long_text_at_word_boundaries() {
    let long = "alpha beta gamma delta epsilon zeta eta theta iota";
    let wrapped = wrap_description(long, 15);
    // Total visible chars (no `…`) reassembles the original.
    assert!(wrapped.len() >= 2, "must wrap at narrow width: {wrapped:?}");
    let joined: String = wrapped.join(" ");
    for word in long.split_whitespace() {
        assert!(
            joined.contains(word),
            "wrap must preserve word `{word}` (no mid-word truncation): {joined:?}",
        );
    }
    // No `…` truncation marker in any line.
    for line in &wrapped {
        assert!(
            !line.contains('\u{2026}'),
            "wrap line must not contain `…`: {line:?}",
        );
    }
}

fn synthetic_long_label_meta() -> SettingMeta {
    // Fixed 31-cell label kept for the two-line threshold tests
    // below. Previously matched the literal `simple_mode` label
    // (now renamed to "Disable vim input mode" — 22 cells); the
    // longer literal stays to exercise the wrap path that the
    // shorter rename no longer triggers organically.
    SettingMeta {
        key: "test-long-label",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Disable vim mode (experimental)",
        description: "Long-label test for Commit 13 two-line layout.",
        keywords: &["test"],
        kind: SettingKind::Bool { default: false },
        restart_required: false,
        hidden_in_minimal: false,
    }
}

fn synthetic_enum_chevron_meta() -> SettingMeta {
    SettingMeta {
        key: "test-enum-with-chevron",
        category: SettingCategory::Privacy,
        owner: SettingOwner::Shared,
        label: "Coding data sharing",
        description: "Enum row that opens a picker — chevron suffix applies.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "opt-out",
            choices: TEST_ENUM_CHOICES,
            supports_preview: true,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }
}

/// Render a long label at a narrow area; expect the value to drop
/// to line 2 right-aligned, while the full label stays on line 1.
///
/// "Disable vim mode (experimental)" = 31 cells. One-line total
/// (with `off` + chrome = 38 cells) doesn't fit at width=35, so
/// the row picks `TwoLine`. The label alone (with triangle +
/// right pad = 34 cells) DOES fit, so it stays on line 1
/// without truncation.
#[test]
fn narrow_terminal_drops_value_to_second_line() {
    let meta = synthetic_long_label_meta();
    let area = Rect {
        x: 0,
        y: 0,
        width: 35,
        height: 2,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    let value_rect = render_setting_row(
        &mut buf,
        area,
        &meta,
        &SettingValue::Bool(false),
        24, // max_label_w — ignored for layout.
        false,
        &theme,
        false,
        false, // is_hovered
        None,
    );
    let line1 = buf_row_text(&buf, 0, area.x, area.width);
    let line2 = buf_row_text(&buf, 1, area.x, area.width);
    assert!(
        line1.contains("Disable vim mode (experimental)"),
        "line 1 must contain the FULL label (no truncation): {line1:?}"
    );
    assert!(
        !line1.contains("off"),
        "value `off` must NOT render on line 1 — it should drop to line 2. line1={line1:?}"
    );
    assert!(
        line2.contains("off"),
        "value `off` must render on line 2 (right-aligned): {line2:?}"
    );
    // Value should be right-aligned: the `off` text ends just
    // before the (reserved-but-empty) chevron column AND the
    // 1-cell right pad. Line-2
    // reserves the same `ROW_RIGHT_PAD_W + ROW_CHEVRON_COL_W`
    // suffix as line 1, so the chevron column is at a constant
    // right offset from the area's right edge regardless of
    // whether a row went one-line or two-line. We allow 1
    // extra cell of slack for the gap between value and the
    // chevron column.
    let last_idx = line2.rfind("off").expect("line2 contains `off`");
    let slack = (ROW_CHEVRON_COL_W as usize) + (ROW_RIGHT_PAD_W as usize) + 1;
    assert!(
        last_idx + "off".len() >= (area.width as usize).saturating_sub(slack),
        "value must be right-aligned (within {slack} cells of right edge): \
         last_idx={last_idx}, area.width={}",
        area.width,
    );
    assert_eq!(
        value_rect.y,
        area.y + 1,
        "value_rect.y must be on line 2 (area.y + 1), got y={}",
        value_rect.y
    );
}

/// At wide area widths, the row collapses to a single line — full
/// label + value on the same line.
#[test]
fn wide_terminal_keeps_value_on_first_line() {
    let meta = synthetic_long_label_meta();
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 2,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    let value_rect = render_setting_row(
        &mut buf,
        area,
        &meta,
        &SettingValue::Bool(false),
        24,
        false,
        &theme,
        false,
        false, // is_hovered
        None,
    );
    let line1 = buf_row_text(&buf, 0, area.x, area.width);
    let line2 = buf_row_text(&buf, 1, area.x, area.width);
    assert!(
        line1.contains("Disable vim mode (experimental)") && line1.contains("off"),
        "wide-area one-line layout must render label + value on line 1: {line1:?}"
    );
    assert!(
        line2.chars().all(|c| c == ' '),
        "line 2 must be blank in wide-area one-line layout: {line2:?}"
    );
    assert_eq!(
        value_rect.y, area.y,
        "value_rect.y must be line 1 (area.y) in one-line layout"
    );
}

/// `row_layout`'s pathological-truncation branch: when even the
/// label alone exceeds the row width, the label gets truncated
/// with `…` on line 1 and the value still drops to line 2.
#[test]
fn pathologically_narrow_truncates_label_with_ellipsis() {
    let meta = synthetic_long_label_meta();
    let area = Rect {
        x: 0,
        y: 0,
        width: 25,
        height: 2,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_setting_row(
        &mut buf,
        area,
        &meta,
        &SettingValue::Bool(false),
        24,
        false,
        &theme,
        false,
        false, // is_hovered
        None,
    );
    let line1 = buf_row_text(&buf, 0, area.x, area.width);
    let line2 = buf_row_text(&buf, 1, area.x, area.width);
    assert!(
        line1.contains('\u{2026}'),
        "line 1 must contain the `…` ellipsis when label is too wide: {line1:?}"
    );
    assert!(
        line2.contains("off"),
        "value `off` must still drop to line 2 even when label is truncated: {line2:?}"
    );
}

/// Two-line rows expand `state.row_rects` to span BOTH lines so
/// mouse clicks on either line trigger the same default action.
///
/// `coding_data_sharing`'s label plus the value "Opt out", the chevron,
/// and the row chrome are far wider than the width=28 we render at, so
/// the row drops to two lines.
#[test]
fn two_line_row_hit_rect_spans_both_lines() {
    let mut s = make_state();
    let row_idx = s
        .rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key, .. } if *key == "coding_data_sharing"))
        .expect("coding_data_sharing must be registered");
    // Render at a narrow width so coding_data_sharing forces a
    // two-line layout.
    let area = Rect {
        x: 0,
        y: 0,
        width: 28,
        height: 60,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    s.selected = row_idx;
    render_rows(&mut buf, area, &mut s, &theme);

    let rect = s.row_rects[row_idx];
    assert!(
        rect.height >= 2,
        "two-line row hit-rect must span ≥2 lines, got height={}",
        rect.height
    );

    // Synthesize a click on line 2 of the row. The mouse handler
    // should fire the default action (open the enum picker for
    // coding_data_sharing).
    s.list_area = area;
    let click_y = rect.y + 1;
    // Click somewhere in the middle of line 2.
    let click_x = rect.x + rect.width / 2;
    // First click: only selects (since selection might not match).
    // Force selection on first to make it a direct activation.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        click_x,
        click_y,
    );
    // Selection already matches, so click activates: enum picker
    // opens (mode flips to PickingEnum).
    match outcome {
        SettingsKeyOutcome::Changed => {
            assert!(
                matches!(s.mode(), SettingsModalMode::PickingEnum { .. }),
                "click on line 2 of a two-line Enum row must open the picker, \
                 got mode {:?}",
                s.mode()
            );
        }
        other => panic!(
            "click on line 2 must produce Changed (selection or activation), \
             got {other:?}"
        ),
    }
}

/// Expanded two-line rows render label (line 1), value (line 2),
/// and the wrapped description on subsequent lines.
#[test]
fn two_line_row_with_expansion_renders_three_segments() {
    let mut s = make_state();
    // The coding-data row's label + value (with chevron) won't
    // fit on a 28-col line, forcing two-line layout.
    let row_idx = s
        .rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key, .. } if *key == "coding_data_sharing"))
        .expect("coding_data_sharing must be registered");
    s.selected = row_idx;
    s.expanded_keys.insert("coding_data_sharing");

    let area = Rect {
        x: 0,
        y: 0,
        width: 28,
        height: 60,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_rows(&mut buf, area, &mut s, &theme);

    let rect = s.row_rects[row_idx];
    assert!(
        rect.height >= 2,
        "expanded two-line row must allocate ≥2 lines for the row itself, got height={}",
        rect.height
    );
    // The row label is on line 1. A 28-col row truncates a long label, so
    // match the head of the live copy rather than the whole string.
    let label_line = buf_row_text(&buf, rect.y, area.x, area.width);
    let label = s
        .registry
        .find("coding_data_sharing")
        .expect("registered")
        .label;
    let head: String = label
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        label_line.contains(&head),
        "line 1 must contain the row label (head {head:?}): {label_line:?}"
    );
    // The value (display: "Opt out" or similar) is on line 2.
    let value_line = buf_row_text(&buf, rect.y + 1, area.x, area.width);
    // Value comes from displaying the canonical → display mapping,
    // which uses the synthetic enum's "Third Option" canonical of
    // "opt-out". The display fallback returns the canonical when
    // the lookup misses — registry has the real `CodingDataSharing`
    // choices, so display should be "Opt out".
    assert!(
        value_line.contains("Opt") || value_line.contains("opt") || value_line.contains("out"),
        "line 2 must contain the value text: {value_line:?}"
    );
    // The expanded description renders on line 3 and below.
    let desc_line = buf_row_text(&buf, rect.y + 2, area.x, area.width);
    assert!(
        !desc_line.chars().all(|c| c == ' '),
        "line 3 must contain wrapped description text (non-blank): {desc_line:?}"
    );
}

/// The contextual-hints group row carries no value, but when its key is in
/// `expanded_keys` (Right/l) it must still paint its description below the
/// chevron row — mirroring how normal rows surface an expanded description.
/// Regression guard: before the fix the group short-circuited out of both
/// the height + render loops, so Right/l set `expanded_keys` but painted
/// nothing.
#[test]
fn group_row_renders_expanded_description() {
    let mut s = make_state();
    let row_idx = s
        .rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key, .. } if *key == "contextual_hints"))
        .expect("contextual_hints group must be registered");
    s.selected = row_idx;
    s.expanded_keys.insert("contextual_hints");

    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 60,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_rows(&mut buf, area, &mut s, &theme);

    let rect = s.row_rects[row_idx];
    // Line 1 is the group's chevron row (its label).
    let label_line = buf_row_text(&buf, rect.y, area.x, area.width);
    assert!(
        label_line.contains("Show contextual hints"),
        "line 1 must contain the group label: {label_line:?}"
    );
    // The description renders on the line below the chevron row (non-blank).
    let desc_line = buf_row_text(&buf, rect.y + 1, area.x, area.width);
    assert!(
        !desc_line.chars().all(|c| c == ' '),
        "expanded group must paint its description below the chevron row \
         (non-blank): {desc_line:?}"
    );
    // The painted text matches the registered description (derive a token
    // from the live copy so this stays green across description edits).
    let desc = s
        .registry
        .find("contextual_hints")
        .expect("group registered")
        .description;
    let token = desc
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric());
    assert!(
        !token.is_empty() && desc_line.contains(token),
        "expanded group description must render its text (token `{token}`): {desc_line:?}"
    );
}

/// `row_layout`'s width threshold: a label + value that exactly
/// fits picks `OneLine`; one cell narrower picks `TwoLine`.
#[test]
fn row_layout_threshold_is_exact() {
    let label = "Coding data sharing"; // 19 cells
    let value = "Opt out"; // 7 cells
    // chrome (triangle + gap + chevron + right pad) = 2 + 1 + 2 + 1 = 6
    // total = 19 + 7 + 6 = 32 cells (chevron-enabled).
    assert_eq!(row_layout(32, label, value, false), RowLayout::OneLine);
    assert_eq!(row_layout(31, label, value, false), RowLayout::TwoLine);
}

/// Sanity: `row_layout` handles bool-without-chevron rows
/// (Bool kind, no `›` suffix). The chevron
/// column is reserved even for Bool rows, so the chrome cost
/// is the same with and without the glyph.
///
/// The dead
/// `has_chevron` parameter has been removed; `row_layout` now
/// always reserves the chevron column. The Bool / Enum
/// distinction at the renderer is purely whether to paint
/// the `›` glyph in the (always-reserved) column.
#[test]
fn row_layout_bool_without_chevron() {
    let label = "Disable vim mode (experimental)"; // 31 cells
    let value = "off"; // 3 cells
    // chrome (triangle + gap + reserved chevron col + right pad)
    // = 2 + 1 + 2 + 1 = 6 cells, identical to the
    // chevron-enabled case.
    // total = 31 + 3 + 6 = 40 cells.
    assert_eq!(row_layout(40, label, value, false), RowLayout::OneLine);
    assert_eq!(row_layout(39, label, value, false), RowLayout::TwoLine);
}

/// Sanity: `_ = synthetic_enum_chevron_meta` reference so the test
/// helper isn't flagged unused while the two-line fixtures stabilise.
#[test]
fn synthetic_enum_chevron_meta_constructs() {
    let m = synthetic_enum_chevron_meta();
    assert_eq!(m.key, "test-enum-with-chevron");
}

// -- User-feedback follow-up: always reserve a blank line between
//    the "Tip · Ask EchoAgent…" docs footer and the keybindings hints.
//
// Before this fix, when the hints wrapped to 2 lines (narrow modal
// widths) the chrome's 2-row footer was fully consumed by hint
// rows, so the tip sat directly above the first hint line with no
// gap. The fix bumps `footer_lines` to `predicted_hint_rows + 1`
// so the chrome footer always reserves a blank row above the
// hints — preserving the visual hierarchy at any width.
//
// The "don't wrap" fixture uses FilterFocused mode because its
// shortcut set is short enough (~76 cells) to fit on a single row
// at the modal's max_width=110. Browse-mode hints (~114 cells)
// wrap at every modal width supported by `render_settings_modal`,
// so they're useless as a "no wrap" fixture.

/// Find the y of the buffer row containing `needle` (first match,
/// scanning top to bottom). Returns `None` if no row matches.
fn find_row_y(buf: &Buffer, area: Rect, needle: &str) -> Option<u16> {
    for y in area.y..area.y.saturating_add(area.height) {
        let row = buf_row_text(buf, y, area.x, area.width);
        if row.contains(needle) {
            return Some(y);
        }
    }
    None
}

/// Return true if every cell strictly INSIDE the modal popup's
/// vertical borders on the given row is whitespace. The modal
/// borders (`│` at popup_area.x and at popup_area.x + width - 1)
/// are excluded from the check so we test the gap-line interior,
/// not the chrome glyphs.
fn modal_interior_row_is_blank(buf: &Buffer, popup_area: Rect, y: u16) -> bool {
    let left = popup_area.x.saturating_add(1);
    let right = popup_area
        .x
        .saturating_add(popup_area.width)
        .saturating_sub(1);
    for x in left..right {
        if let Some(cell) = buf.cell((x, y))
            && cell.symbol() != " "
        {
            return false;
        }
    }
    true
}

/// Narrow modal: the Browse-mode hint string
/// `↑/↓/j/k nav | g/G top/btm | …` wraps to 2+ lines. Without
/// the fix the tip would sit directly above the first hint
/// line; with the fix there's exactly one blank row between
/// them.
#[test]
fn footer_has_blank_line_between_tip_and_hints_when_hints_wrap() {
    let mut s = make_state();
    // 70-col viewport caps modal_width at max(70*0.70, 44) = 49,
    // so footer_width ≈ 45. Browse-mode hints are ~114 cells so
    // they wrap to at least 2 rows.
    let area = Rect {
        x: 0,
        y: 0,
        width: 70,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    let popup_area = s.window.popup_area.expect("modal must have rendered");

    let tip_y = find_row_y(&buf, area, "Tip").expect("tip row must render");
    // Sanity-check that the hints actually wrap — if a future PR
    // trims the hint string enough that they fit on one row at
    // this width the test passes for the wrong reason. Look for
    // the first hint label (`nav`) AND the last (`F2/Esc`); they
    // must land on different y if the hints wrapped.
    // Use `j/k nav` (hint-unique) rather than `nav` alone, which
    // also matches the `vim_mode` row's "navigation" keyword.
    let first_hint_y = find_row_y(&buf, area, "j/k nav").expect("first hint line must render");
    let last_hint_y = find_row_y(&buf, area, "F2/Esc").expect("close hint must render");
    assert!(
        last_hint_y > first_hint_y,
        "this test requires the hints to wrap to ≥2 lines; got first_hint_y={first_hint_y} \
         last_hint_y={last_hint_y} — pick a narrower width if the hint string shrank"
    );

    // Tip → blank gap → first hint stacked contiguously at the
    // bottom of the modal.
    assert_eq!(
        first_hint_y,
        tip_y + 2,
        "tip → gap → hints must stack with exactly one blank line between tip and the \
         first hint line; tip_y={tip_y} first_hint_y={first_hint_y}"
    );
    let gap_y = tip_y + 1;
    assert!(
        modal_interior_row_is_blank(&buf, popup_area, gap_y),
        "row between tip and hints must be entirely blank inside the modal interior, \
         got: {:?}",
        buf_row_text(&buf, gap_y, popup_area.x, popup_area.width)
    );
}

/// Wide modal in FilterFocused mode: hints fit on a single line.
/// Same blank-gap contract as the wrap case — the chrome's
/// baseline `footer_lines: 2` already provides this gap, but the
/// test pins the contract so a future change that drops the
/// baseline (or shrinks `predicted_rows + 1` to `predicted_rows`)
/// is caught.
#[test]
fn footer_has_blank_line_between_tip_and_hints_when_hints_dont_wrap() {
    let mut s = make_state();
    // FilterFocused mode has 5 shortcuts totalling ~76 cells —
    // fits on one row at any modal width supported by
    // `render_settings_modal` (max_width=110).
    s.focus_filter();
    let area = Rect {
        x: 0,
        y: 0,
        width: 150,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    let popup_area = s.window.popup_area.expect("modal must have rendered");

    let tip_y = find_row_y(&buf, area, "Tip").expect("tip row must render");
    // FilterFocused-mode hints: `type to filter | ↑/↓ nav |
    // Backspace edit | Enter commit | Esc clear`. Verify both
    // ends land on the SAME row (proves no wrap).
    let first_hint_y = find_row_y(&buf, area, "type to filter").expect("first hint must render");
    let last_hint_y = find_row_y(&buf, area, "Esc clear").expect("last hint must render");
    assert_eq!(
        first_hint_y, last_hint_y,
        "this test requires the hints to fit on a single line; at width=150 + \
         FilterFocused mode we expect one row, got first={first_hint_y} last={last_hint_y}"
    );

    // Same tip → blank gap → hint contract as the wrap case.
    assert_eq!(
        first_hint_y,
        tip_y + 2,
        "tip → gap → hints must stack with exactly one blank line between tip and the \
         hint line; tip_y={tip_y} first_hint_y={first_hint_y}"
    );
    let gap_y = tip_y + 1;
    assert!(
        modal_interior_row_is_blank(&buf, popup_area, gap_y),
        "row between tip and hints must be entirely blank inside the modal interior, \
         got: {:?}",
        buf_row_text(&buf, gap_y, popup_area.x, popup_area.width)
    );
}

/// When the hints transition from 1 row to 2 rows (e.g. a width
/// reduction), the row-list area shrinks by exactly 1 row to
/// make room for the second hint row. Pinned so anyone
/// "reclaiming" the gap row later sees the spec violation.
///
/// Uses FilterFocused mode for both renders because Browse-mode
/// hints don't fit on a single row at any width supported by the
/// modal — there'd be no "1-row" baseline to compare against. The
/// narrow viewport (100 cols → modal_width=70, footer_width=64)
/// is tuned so FilterFocused hints (~76 cells incl. separators)
/// wrap to exactly 2 rows (not 3+) — a more-aggressive narrow
/// would split this into 3+ rows and the assertion below would
/// trip on the multi-row delta.
#[test]
fn footer_total_height_grows_when_hints_wrap() {
    // Wide modal: FilterFocused-mode hints fit on 1 row.
    // footer_lines = 1 (hints) + 1 (gap) = 2 → matches baseline.
    let wide_area = Rect {
        x: 0,
        y: 0,
        width: 150,
        height: 30,
    };
    let mut s_wide = make_state();
    s_wide.focus_filter();
    let mut buf_wide = Buffer::empty(wide_area);
    render_settings_modal(&mut buf_wide, wide_area, &mut s_wide, false, None);
    let wide_list_height = s_wide.list_area.height;

    // Narrow modal: same mode, hints wrap to exactly 2 rows.
    // footer_lines = 2 (hints) + 1 (gap) = 3 → 1 more than wide.
    let narrow_area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 30,
    };
    let mut s_narrow = make_state();
    s_narrow.focus_filter();
    let mut buf_narrow = Buffer::empty(narrow_area);
    render_settings_modal(&mut buf_narrow, narrow_area, &mut s_narrow, false, None);
    let narrow_list_height = s_narrow.list_area.height;

    // Verify the wrap actually happens at the narrow width AND
    // doesn't over-wrap to 3+ rows (the assertion below would
    // also fire if both renders had the same wrap count OR the
    // narrow case wrapped further, which would be a silent test
    // bug).
    let narrow_first_hint =
        find_row_y(&buf_narrow, narrow_area, "type to filter").expect("first hint");
    let narrow_last_hint = find_row_y(&buf_narrow, narrow_area, "Esc clear").expect("last hint");
    assert_eq!(
        narrow_last_hint,
        narrow_first_hint + 1,
        "narrow fixture must wrap the hints to exactly 2 rows; got \
         first={narrow_first_hint} last={narrow_last_hint}"
    );
    let wide_first_hint = find_row_y(&buf_wide, wide_area, "type to filter").expect("first");
    let wide_last_hint = find_row_y(&buf_wide, wide_area, "Esc clear").expect("last");
    assert_eq!(
        wide_first_hint, wide_last_hint,
        "wide fixture must NOT wrap the hints; got first={wide_first_hint} \
         last={wide_last_hint}"
    );

    // The narrow render reserves one extra row at the bottom for
    // the wrapped hint, so the row-list area is exactly one row
    // shorter. Equality (not "<=") rules out off-by-N
    // regressions where future code reserves 2 rows instead of 1.
    assert_eq!(
        narrow_list_height + 1,
        wide_list_height,
        "row-list area must shrink by exactly 1 row when hints wrap (narrow={}, wide={})",
        narrow_list_height,
        wide_list_height,
    );
}

// -- palette consistency --

/// Section headers render in the palette's style: ` {label} `
/// in `gray + BOLD` followed by `─` separator cells in
/// `gray_dim`. Asserts that (a) the header label cell carries
/// the gray foreground and BOLD modifier matching the
/// palette's `render_picker_entry` Header arm, and (b) at
/// least one trailing cell renders a `─` glyph.
#[test]
fn section_header_style_matches_palette() {
    let mut s = make_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 60,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_rows(&mut buf, area, &mut s, &theme);

    // Find the row containing the "Appearance" header.
    let label = SettingCategory::Appearance.label();
    let mut header_y: Option<u16> = None;
    for y in 0..area.height {
        let txt = buf_row_text(&buf, y, area.x, area.width);
        if txt.trim_start().starts_with(label) {
            header_y = Some(y);
            break;
        }
    }
    let header_y = header_y.expect("must find Appearance header");

    // The label is rendered at col 1 (after the leading space)
    // in the gray + BOLD style — matches the palette.
    let cell = buf.cell((area.x + 1, header_y)).expect("cell at label col");
    assert_eq!(
        cell.fg, theme.gray,
        "section header label fg must be theme.gray (palette parity)"
    );
    assert!(
        cell.modifier.contains(Modifier::BOLD),
        "section header label must be BOLD (palette parity)"
    );

    // At least one trailing `─` separator cell must render after
    // the label — find the first `─` in the row after the label.
    let row_text = buf_row_text(&buf, header_y, area.x, area.width);
    assert!(
        row_text.contains('\u{2500}'),
        "section header row must contain `─` separator cells: {row_text:?}"
    );

    // The separator cells must
    // render in `theme.gray_dim` for palette parity. Walk the
    // row and find the first `─` cell; assert its fg color.
    // Mirrors the `search_bar_renders_divider_below` pattern.
    let mut sep_cell_fg = None;
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell((x, header_y))
            && cell.symbol() == "\u{2500}"
        {
            sep_cell_fg = Some(cell.fg);
            break;
        }
    }
    let sep_fg = sep_cell_fg.expect("must find at least one `─` separator cell");
    assert_eq!(
        sep_fg, theme.gray_dim,
        "section header `─` separator must render in theme.gray_dim \
         (palette parity)",
    );
}

/// Search bar renders the palette's prefix + cursor style:
///   * ` search: ` prefix in `gray`.
///   * Inverse-video cursor (bg = text_primary, fg = bg_base)
///     at the next-input position when focused.
///
/// Hint path renders ` / to search` in `gray_dim`.
#[test]
fn search_bar_focused_style_matches_palette() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    let editor = LineEditor::default();
    crate::views::picker::render_line_editor_search_bar(
        &mut buf,
        area.x,
        area.y,
        area.width,
        &theme,
        &editor,
        true,
        true,
        Some(theme.bg_base),
    );

    // First label cell carries the `gray` fg.
    let first = buf.cell((area.x + 1, area.y)).expect("col 1 cell");
    assert_eq!(
        first.fg, theme.gray,
        "search bar label prefix must use theme.gray"
    );

    // Cursor cell at the input position (label is ` search: ` =
    // 9 cells; cursor lands at col 9) is inverse-video: bg =
    // text_primary, fg = bg_base.
    let cursor_x = " search: ".width() as u16;
    let cursor_cell = buf.cell((area.x + cursor_x, area.y)).expect("cursor cell");
    assert_eq!(
        cursor_cell.bg, theme.text_primary,
        "cursor cell bg must be text_primary (inverse-video)"
    );
    assert_eq!(
        cursor_cell.fg, theme.bg_base,
        "cursor cell fg must be bg_base (inverse-video)"
    );
}

/// Empty + unfocused search bar shows ` / to search` in
/// `gray_dim` — same wording the palette uses.
///
/// Sample multiple cells of
/// the hint span (not just col 1) so a regression that styled
/// only the first few cells in gray_dim and left the rest at
/// default is caught.
#[test]
fn search_bar_placeholder_matches_palette() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    let editor = LineEditor::default();
    crate::views::picker::render_line_editor_search_bar(
        &mut buf,
        area.x,
        area.y,
        area.width,
        &theme,
        &editor,
        false,
        true,
        Some(theme.bg_base),
    );

    let txt: String = (area.x..area.x + area.width)
        .filter_map(|x| buf.cell((x, area.y)).map(|c| c.symbol().to_string()))
        .collect();
    assert!(
        txt.contains("/ to search"),
        "search bar placeholder must read `/ to search`, got: {txt:?}"
    );
    // Sample BOTH ends of the hint span. The hint is
    // " / to search" (12 cells); the first slash is at col 1
    // and the trailing `h` is at col 11.
    let slash_cell = buf.cell((area.x + 1, area.y)).expect("col 1 cell (/)");
    assert_eq!(
        slash_cell.fg, theme.gray_dim,
        "first hint cell (`/`) must render in theme.gray_dim"
    );
    let hint = " / to search";
    let last_col = area.x + (hint.width() as u16 - 1);
    let last_cell = buf.cell((last_col, area.y)).expect("last hint cell");
    assert_eq!(
        last_cell.fg, theme.gray_dim,
        "LAST hint cell ({last_col}) must also be theme.gray_dim — \
         a regression that styled only the prefix would slip past a \
         single-cell sample",
    );
}

#[test]
fn ctrl_u_clears_the_entire_filter_from_mid_query() {
    let mut state = make_state();
    state.focus_filter();
    state.set_query("alpha beta");
    set_filter_cursor(&mut state, "alpha".len());

    let outcome = handle_settings_key(
        &mut state,
        &KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(state.query().is_empty());
    assert_eq!(state.query_cursor(), 0);
}

#[test]
fn string_editor_paste_sanitizes_validates_and_consumes_rejected_text() {
    let mut state = editor_render_fixture("EchoAgent Tst", "EchoAgent T".len());
    let outcome = handle_settings_paste(&mut state, "e\r\n");
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(state.editing_buffer(), Some("EchoAgent Test"));
    assert!(state.editing_validation_error().is_none());

    let outcome = handle_settings_paste(&mut state, "\u{202e}\r\n");
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(state.editing_buffer(), Some("EchoAgent Test"));
    assert!(state.editing_validation_error().is_none());
}

#[test]
fn filter_search_bar_keeps_narrow_graphemes_and_cursor_aligned() {
    let grapheme = "👩🏽\u{200d}💻";
    let combining = "e\u{301}";
    let mut state = make_state();
    state.focus_filter();
    state.set_query(format!("a{grapheme}{combining}"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 13,
        height: 3,
    };
    let mut buffer = Buffer::empty(area);
    let theme = Theme::current();
    render_row_list_with_search_bar(&mut buffer, area, &mut state, &theme);

    let mut row = String::new();
    for x in 0..area.width {
        row.push_str(buffer.cell((x, 0)).expect("search cell").symbol());
    }
    assert!(row.contains(grapheme), "ZWJ grapheme split: {row:?}");
    assert!(row.contains(combining), "combining grapheme split: {row:?}",);
    assert_eq!(
        buffer.cell((12, 0)).expect("cursor cell").bg,
        theme.text_primary,
    );
}

// -- value color + chevron column + docs footer --

/// Bool `off` values render in the muted `gray` color while
/// Bool `on` values keep the active `accent_user`: the inactive
/// state should read as visually subordinate.
#[test]
fn bool_off_value_renders_in_dim_color() {
    let meta = SettingMeta {
        key: "test-bool-dim",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Test bool",
        description: "",
        keywords: &[],
        kind: SettingKind::Bool { default: false },
        restart_required: false,
        hidden_in_minimal: false,
    };
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };
    let theme = Theme::current();

    // Render with `off` — value cells must be styled with
    // `theme.gray`.
    let mut buf_off = Buffer::empty(area);
    render_setting_row(
        &mut buf_off,
        area,
        &meta,
        &SettingValue::Bool(false),
        15,
        false,
        &theme,
        false,
        false,
        None,
    );
    // Use `find_text_col` so the
    // column index is the actual buffer position, not a byte
    // offset cast to u16 (which only works for ASCII content).
    let off_col = find_text_col(&buf_off, 0, "off").expect("must find `off` substring");
    let off_cell = buf_off.cell((off_col, 0)).expect("off cell");
    assert_eq!(
        off_cell.fg, theme.gray,
        "Bool(false) value must render in theme.gray (Misha/Kevin Fix 4)",
    );
    // Also assert the second `f` cell carries the same style —
    // a regression that only styled the first cell would slip
    // past a one-cell sample.
    let off_col_2 = off_col + 2;
    let off_cell_2 = buf_off.cell((off_col_2, 0)).expect("off second-f cell");
    assert_eq!(
        off_cell_2.fg, theme.gray,
        "ALL cells of `off` must carry theme.gray (consistency check)",
    );

    // Render with `on` — value cells stay at `accent_user`.
    let mut buf_on = Buffer::empty(area);
    render_setting_row(
        &mut buf_on,
        area,
        &meta,
        &SettingValue::Bool(true),
        15,
        false,
        &theme,
        false,
        false,
        None,
    );
    let on_col = find_text_col(&buf_on, 0, "on").expect("must find `on` substring");
    let on_cell = buf_on.cell((on_col, 0)).expect("on cell");
    assert_eq!(
        on_cell.fg, theme.accent_user,
        "Bool(true) value must keep theme.accent_user color (active state)",
    );
    // **Asymmetry assertion**: the
    // active and inactive states must use distinct theme
    // tokens. The PER-CELL asserts above already pin
    // `on.fg == theme.accent_user` and `off.fg == theme.gray`
    // — verifying the THEME tokens are also distinct catches
    // the orthogonal regression where someone flips one of
    // the tokens to match the other (rendering would then
    // make on and off visually identical despite the
    // per-cell asserts continuing to pass).
    //
    // Conditional on the test environment's color quantization
    // exposing the distinction: in extreme low-color
    // terminals both tokens can collapse to `Color::Reset`,
    // in which case the asymmetry is unobservable. We
    // assert-only when the tokens differ, matching the
    // theme-rendered-distinct contract.
    if theme.accent_user != theme.gray {
        assert_ne!(
            on_cell.fg, off_cell.fg,
            "Bool(true) and Bool(false) must use DIFFERENT colors \
             (asymmetry that Fix 4 introduced; theme tokens differ \
             so the per-cell distinction should be observable)",
        );
    }
}

/// The chevron column is at the same right offset for ALL
/// row kinds — Bool rows leave it empty, Enum/String rows
/// fill it with `" ›"`, but the column position (and
/// therefore the value's right edge) is constant.
#[test]
fn chevron_column_is_at_constant_right_offset() {
    let bool_meta = SettingMeta {
        key: "test-bool-col",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Bool row",
        description: "",
        keywords: &[],
        kind: SettingKind::Bool { default: false },
        restart_required: false,
        hidden_in_minimal: false,
    };
    let enum_meta = synthetic_enum_chevron_meta();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 1,
    };
    let theme = Theme::current();

    // Bool row — chevron column is empty (no `›` glyph).
    let mut buf_bool = Buffer::empty(area);
    let bool_rect = render_setting_row(
        &mut buf_bool,
        area,
        &bool_meta,
        &SettingValue::Bool(true),
        10,
        false,
        &theme,
        false,
        false,
        None,
    );

    // Enum row — chevron column contains the `›` glyph.
    let mut buf_enum = Buffer::empty(area);
    let enum_rect = render_setting_row(
        &mut buf_enum,
        area,
        &enum_meta,
        &SettingValue::Enum("choice_a"),
        10,
        false,
        &theme,
        false,
        false,
        None,
    );

    // The chevron column is a 2-cell block at
    // `area.right - ROW_RIGHT_PAD_W - ROW_CHEVRON_COL_W` (i.e.
    // `area.right - 3` in this fixture). The `›` glyph occupies
    // the SECOND cell of the column (the first cell is a
    // leading space that doubles as gap from the value). For
    // Bool rows the column stays empty.
    let glyph_x = area.x + area.width - ROW_RIGHT_PAD_W - 1;
    let bool_cell = buf_bool.cell((glyph_x, 0)).expect("bool col cell");
    assert_eq!(
        bool_cell.symbol().trim(),
        "",
        "Bool row's chevron column must be empty (no `›` glyph): \
         cell symbol = {:?}",
        bool_cell.symbol(),
    );
    let enum_cell = buf_enum.cell((glyph_x, 0)).expect("enum col cell");
    assert_eq!(
        enum_cell.symbol(),
        "\u{203A}",
        "Enum row's chevron column must contain the `›` glyph at \
         area.right - {} (constant right offset across rows), got: {:?}",
        ROW_RIGHT_PAD_W + 1,
        enum_cell.symbol(),
    );

    // The value hit-rect's right edge must be the same for
    // both rows — that's the required visual alignment.
    // Both rects end at `value_rect.x + value_rect.width`
    // which equals `chevron_col_x + ROW_CHEVRON_COL_W`.
    let bool_right = bool_rect.x + bool_rect.width;
    let enum_right = enum_rect.x + enum_rect.width;
    assert_eq!(
        bool_right, enum_right,
        "Bool and Enum value hit-rects must share the same right \
         edge (bool_right={bool_right}, enum_right={enum_right})",
    );

    // Also exercise the
    // SAME buffer with multiple rows stacked so we catch a
    // regression where, e.g., only Bool rows compute the wrong
    // chevron column. Render Bool then Enum on consecutive
    // rows and assert the chevron column lands at the same
    // column for both.
    let multi_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 4,
    };
    let mut buf_multi = Buffer::empty(multi_area);
    let bool_area = Rect {
        height: 1,
        ..multi_area
    };
    let enum_area = Rect {
        y: 2,
        height: 1,
        ..multi_area
    };
    let _ = render_setting_row(
        &mut buf_multi,
        bool_area,
        &bool_meta,
        &SettingValue::Bool(false),
        10,
        false,
        &theme,
        false,
        false,
        None,
    );
    let _ = render_setting_row(
        &mut buf_multi,
        enum_area,
        &enum_meta,
        &SettingValue::Enum("choice_a"),
        10,
        false,
        &theme,
        false,
        false,
        None,
    );
    // Bool row's `off` ends at column N; Enum row's `›` glyph
    // lands at column M. The contract: N == M's column
    // minus 1 (the gap between value and chevron column) — OR
    // equivalently, the chevron-column glyph position is the
    // same on both rows.
    let glyph_x_multi = multi_area.x + multi_area.width - ROW_RIGHT_PAD_W - 1;
    let enum_glyph_cell = buf_multi
        .cell((glyph_x_multi, 2))
        .expect("enum row chevron glyph cell");
    assert_eq!(
        enum_glyph_cell.symbol(),
        "\u{203A}",
        "Enum row's chevron glyph must land at glyph_x={glyph_x_multi}",
    );
    let bool_glyph_cell = buf_multi
        .cell((glyph_x_multi, 0))
        .expect("bool row chevron column cell");
    assert_eq!(
        bool_glyph_cell.symbol().trim(),
        "",
        "Bool row's chevron column must be empty at the SAME \
         glyph_x as Enum's (constant right offset across rows)",
    );
}

/// Two-line rows anchor the
/// chevron column at the SAME right offset as one-line rows.
/// Before the fix, line-2's chevron landed 1 cell further
/// right than line-1's, producing a staircase in mixed-layout
/// row lists.
#[test]
fn chevron_column_aligns_across_one_and_two_line_layouts() {
    let theme = Theme::current();
    // `synthetic_enum_chevron_meta` has label "Coding data
    // sharing" (19 chars) + value "choice_a" (8 chars). At
    // width=25 the one-line total (2 + 19 + 1 + 8 + 2 + 1 =
    // 33) exceeds the width, so the layout flips to TwoLine.
    // At width=60 the same row fits one-line.
    let area_two = Rect {
        x: 0,
        y: 0,
        width: 25,
        height: 2,
    };
    let mut buf_two = Buffer::empty(area_two);
    let _ = render_setting_row(
        &mut buf_two,
        area_two,
        &synthetic_enum_chevron_meta(),
        &SettingValue::Enum("choice_a"),
        10,
        false,
        &theme,
        false,
        false,
        None,
    );
    let area_one = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 1,
    };
    let mut buf_one = Buffer::empty(area_one);
    let _ = render_setting_row(
        &mut buf_one,
        area_one,
        &synthetic_enum_chevron_meta(),
        &SettingValue::Enum("choice_a"),
        10,
        false,
        &theme,
        false,
        false,
        None,
    );
    // The column offset from the area's right edge is constant:
    // `area.right - ROW_RIGHT_PAD_W - 1` is the `›` glyph
    // position (the chevron span " ›" is 2 cells; the second
    // cell holds the glyph). Computed independently for each
    // area so the offset semantics is what we're testing.
    let glyph_x_two = area_two.x + area_two.width - ROW_RIGHT_PAD_W - 1;
    let glyph_x_one = area_one.x + area_one.width - ROW_RIGHT_PAD_W - 1;
    let two_line_cell = buf_two
        .cell((glyph_x_two, 1))
        .expect("two-line chevron cell on line 2");
    assert_eq!(
        two_line_cell.symbol(),
        "\u{203A}",
        "Two-line row's chevron must land at \
         `area.right - ROW_RIGHT_PAD_W - 1` on LINE 2 (UX Issue 2)",
    );
    let one_line_cell = buf_one
        .cell((glyph_x_one, 0))
        .expect("one-line chevron cell");
    assert_eq!(
        one_line_cell.symbol(),
        "\u{203A}",
        "One-line row's chevron must land at `area.right - ROW_RIGHT_PAD_W - 1`",
    );
    // The offset from the right edge is the same — pin that
    // explicitly so a future refactor that changes one anchor
    // independently trips the test.
    assert_eq!(
        area_two.x + area_two.width - glyph_x_two,
        area_one.x + area_one.width - glyph_x_one,
        "chevron offset-from-right must be constant across layouts",
    );
}

/// The docs-footer tip text centers itself horizontally
/// within its row.
///
/// Also exercise the SHORT-
/// fallback path (narrow widths where the LONG message
/// doesn't fit) and the truncation path (extreme narrow
/// where even SHORT doesn't fit), so a regression that moved
/// the centering math into the LONG branch only would fail.
#[test]
fn docs_footer_tip_is_centered() {
    let theme = Theme::current();
    // Helper: render at `width` and return (full row text,
    // tip start col, leading_ws, trailing_ws).
    let render = |width: u16| -> (String, usize, usize) {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height: 1,
        };
        let mut buf = Buffer::empty(area);
        render_docs_footer(&mut buf, area, &theme);
        let row: String = (area.x..area.x + area.width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();
        let tip_start = row.find("Tip").expect("docs footer must contain `Tip`");
        let trailing_ws = row.chars().rev().take_while(|c| *c == ' ').count();
        (row, tip_start, trailing_ws)
    };

    // LONG path: width=80 fits the full message.
    let (row_long, tip_start_long, trailing_long) = render(80);
    assert!(
        tip_start_long > 0,
        "LONG tip must be centered (start > col 0); row={row_long:?}",
    );
    assert!(
        tip_start_long.abs_diff(trailing_long) <= 1,
        "LONG tip leading_ws={tip_start_long} vs trailing_ws={trailing_long}",
    );

    // SHORT path: width that fits SHORT but not LONG.
    // SHORT = "Tip · Ask EchoAgent to change a setting" (34 cells);
    // LONG ≈ 73 cells. width=40 lands in the SHORT band.
    let (row_short, tip_start_short, trailing_short) = render(40);
    assert!(
        row_short.contains("change a setting"),
        "width=40 must render SHORT path (contains `change a setting`): {row_short:?}",
    );
    assert!(
        !row_short.contains("echo-agent-day"),
        "width=40 must NOT render LONG path (no `echo-agent-day`): {row_short:?}",
    );
    assert!(
        tip_start_short.abs_diff(trailing_short) <= 1,
        "SHORT tip must also be centered (Round-3 tests Issue 10): \
         leading_ws={tip_start_short} vs trailing_ws={trailing_short}",
    );

    // Truncated path: width too narrow even for SHORT. The
    // truncation prefix `Tip · …` should still render; the
    // centering math operates on the truncated SHORT.
    let (row_tiny, tip_start_tiny, _trailing_tiny) = render(15);
    assert!(
        row_tiny.contains("Tip"),
        "even at width=15 the `Tip` prefix must render: {row_tiny:?}",
    );
    // At width=15, the truncated SHORT fills most/all of the
    // row; leading_ws could be 0 if the truncation is exactly
    // 15 cells. The contract: centering math doesn't crash
    // and starts at a valid column (not negative).
    assert!(
        tip_start_tiny < 15,
        "tip start must be inside the row at width=15: {tip_start_tiny}",
    );
}

/// `render_settings_modal` reserves a 1-row blank gap above
/// the tip line — so the tip has air on both top (this gap)
/// and bottom (the chrome's `predicted_hint_rows + 1` gap).
#[test]
fn tip_line_has_blank_row_above() {
    let mut s = make_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 40,
    };
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    // Find the tip row.
    let mut tip_y: Option<u16> = None;
    for y in 0..area.height {
        let txt = buf_row_text(&buf, y, area.x, area.width);
        if txt.contains("Tip") && txt.contains("Ask EchoAgent") {
            tip_y = Some(y);
            break;
        }
    }
    let tip_y = tip_y.expect("must find tip row");
    // The row immediately above the tip must be blank inside
    // the modal's content area. Modal borders (`│`) at the
    // left/right edges are expected; we strip leading/trailing
    // border characters before checking that the interior is
    // all spaces.
    assert!(
        tip_y > 0,
        "tip row must not be at y=0 (no row above to check)",
    );
    let above = buf_row_text(&buf, tip_y - 1, area.x, area.width);
    let interior: String = above
        .trim_matches(|c: char| c == ' ' || c == '\u{2502}')
        .to_string();
    assert!(
        interior.is_empty(),
        "row above tip must be blank inside the modal interior \
         (Misha/Kevin Fix 5): full row = {above:?}, interior = {interior:?}",
    );
}

// -- sub-pane polish --

/// Helper: open the picker for the named enum/dyn-enum key in
/// `make_state()`. Returns the state with PickingEnum mode
/// armed. Panics if the key isn't found or isn't an enum.
fn enter_picker_for(key: &'static str) -> SettingsModalState {
    let mut s = make_state();
    let row_idx = s
        .rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key: k, .. } if *k == key))
        .unwrap_or_else(|| panic!("no row for key `{key}` in default registry"));
    assert!(s.select_at(row_idx), "select_at({row_idx})");
    assert!(
        s.try_enter_picking_enum(),
        "try_enter_picking_enum failed for {key} — non-enum?",
    );
    s
}

/// Long enum description word-wraps across multiple buffer
/// rows in the picker sub-pane — no `…` truncation.
///
/// The previous version of this
/// test used the live `theme` setting whose description
/// (`"Color theme for the pager UI."`) is 29 chars and fits on
/// a single line at width=30 — the word-wrap code path was
/// never engaged. Fixed by:
/// 1. Constructing a synthetic registry with a deliberately
///    long description that MUST wrap.
/// 2. Asserting the wrap produces ≥ 2 buffer rows containing
///    description fragments — proves multi-line rendering
///    actually occurred.
/// 3. Asserting the description's LAST word renders — proves
///    the wrap reached the end (no mid-sentence truncation).
/// 4. Replacing the brittle `s…`/`.…` substring check with
///    a clean "no `…` anywhere in the description region"
///    check.
#[test]
fn picker_description_word_wraps_no_ellipsis() {
    // Synthetic enum setting with a description forced to wrap.
    // The description is ~140 chars; at width=30 with a small
    // amount of chrome on either side, it MUST produce ≥ 4
    // description rows.
    let long_desc = "This is a deliberately long description \
                     designed to force the word-wrap renderer \
                     across multiple rows so the test exercises \
                     the wrap logic instead of trivially fitting.";
    let synthetic_meta = SettingMeta {
        key: "test-wrap-desc",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Wrap test",
        description: long_desc,
        keywords: &[],
        kind: SettingKind::Enum {
            default: "choice_a",
            choices: TEST_ENUM_CHOICES,
            supports_preview: false,
        },
        restart_required: false,
        hidden_in_minimal: false,
    };
    let registry = SettingsRegistry::from_entries(vec![synthetic_meta]);
    let mut s = SettingsModalState::new(
        Arc::new(registry),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    // `SettingsModalState::new` snaps `selected` to the first
    // non-header row; with our single-entry registry that's
    // already the synthetic setting. Skip `select_at` since it
    // would no-op (and return false) when called on the
    // already-selected row.
    assert!(
        matches!(
            s.rows.get(s.selected),
            Some(RowEntry::Setting {
                key: "test-wrap-desc",
                ..
            })
        ),
        "selected row must be the synthetic test entry",
    );
    assert!(
        s.try_enter_picking_enum(),
        "synthetic must be picker-eligible"
    );

    let theme = Theme::current();
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 25,
    };
    let mut buf = Buffer::empty(area);
    render_picking_enum(&mut buf, area, &s, &theme);

    // Collect every row of buffer text.
    let rows: Vec<String> = (0..area.height)
        .map(|y| buf_row_text(&buf, y, area.x, area.width))
        .collect();
    let all_text: String = rows.join("\n");

    // 1. First word of description renders somewhere.
    let first_word = long_desc.split_whitespace().next().unwrap_or("");
    assert!(
        all_text.contains(first_word),
        "picker must render the description's first word `{first_word}`",
    );

    // 2. LAST word of description renders too — proves wrap
    // reached the end (no mid-sentence truncation). The
    // description ends with "fitting." so we look for that.
    let last_word = long_desc.split_whitespace().last().unwrap_or("");
    assert!(
        all_text.contains(last_word),
        "picker must render the description's LAST word `{last_word}` \
         (proves word-wrap reached the end): {all_text}",
    );

    // 3. Multiple description rows render. Find the title row
    // (first row containing "Wrap test"), then count
    // consecutive subsequent rows that contain non-empty text
    // that's not a choice marker — those are the description
    // rows. We expect ≥ 2.
    let title_y = rows
        .iter()
        .position(|r| r.contains("Wrap test"))
        .expect("must find title row");
    let mut desc_row_count = 0usize;
    for (i, row) in rows.iter().enumerate().skip(title_y + 1) {
        // Choice markers are `\u{25CB}` (○) or `\u{25CF}` (●).
        if row.contains('\u{25CB}') || row.contains('\u{25CF}') {
            break;
        }
        let interior = row.trim();
        if interior.is_empty() {
            continue;
        }
        // Sanity: stop walking if we hit a row that has
        // weirdly long whitespace runs without any description
        // chars (defensive).
        if i > title_y + 20 {
            break;
        }
        desc_row_count += 1;
    }
    assert!(
        desc_row_count >= 2,
        "wrapped description must span ≥ 2 buffer rows; got {desc_row_count}\n{all_text}",
    );

    // 4. No `…` truncation marker anywhere in the buffer
    // (replaced the
    // ".\u{2026}" / "s\u{2026}" coupled-to-last-char check
    // with a clean absence-of-ellipsis check).
    assert!(
        !all_text.contains('\u{2026}'),
        "picker description must not contain any `…` truncation \
         marker (Misha/Kevin Fix 6a): {all_text}",
    );
}

/// `render_settings_modal` populates `settings_breadcrumb_rect`
/// in sub-pane modes; clears it in Browse / FilterFocused.
///
/// Exercises both
/// `PickingEnum` AND `EditingValue` since the field name says
/// "sub_pane_modes" (plural). Also pins the rect's x and y
/// so a future modal-chrome refactor that
/// shifts the title origin trips a test rather than silently
/// breaking the breadcrumb. The hit-rect
/// spans the FULL breadcrumb (`Settings › <label>`) so any
/// click on the breadcrumb routes back to Browse.
#[test]
fn settings_breadcrumb_rect_set_in_sub_pane_modes() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let mut s = make_state();
    let mut buf = Buffer::empty(area);
    // Browse — no breadcrumb rect.
    render_settings_modal(&mut buf, area, &mut s, false, None);
    assert!(
        s.settings_breadcrumb_rect.is_none(),
        "Browse mode must NOT populate settings_breadcrumb_rect",
    );
    // Enter PickingEnum for `theme`.
    let mut s = enter_picker_for("theme");
    let mut buf2 = Buffer::empty(area);
    render_settings_modal(&mut buf2, area, &mut s, false, None);
    let rect = s
        .settings_breadcrumb_rect
        .expect("PickingEnum must populate settings_breadcrumb_rect");
    let popup = s.window.popup_area.expect("popup_area must be set");
    assert_eq!(
        rect.height, 1,
        "breadcrumb rect must be 1 row tall (sits on the chrome's top border)",
    );
    // Width = full breadcrumb `Settings › <label>`. The leaf
    // label varies by setting — assert it's strictly wider
    // than `Settings` alone (proof that the rect extends past
    // the prefix) AND at least MODAL_TITLE + " › ".
    let prefix_w = MODAL_TITLE.width() + " \u{203A} ".width();
    assert!(
        (rect.width as usize) > MODAL_TITLE.width(),
        "rect width must extend past `Settings` alone for theme picker, got {}",
        rect.width,
    );
    assert!(
        (rect.width as usize) >= prefix_w,
        "rect width must include the `Settings › ` prefix at minimum, got {}",
        rect.width,
    );
    // Pin x/y so a chrome refactor that shifts the title
    // origin trips a test. Origin is
    // `popup.x + 1 (left border) + 2 ("─ " title decoration)`.
    assert_eq!(
        rect.x,
        popup.x + 3,
        "breadcrumb x = popup.x + 1 (border) + 2 (`─ ` title decoration)",
    );
    assert_eq!(rect.y, popup.y, "breadcrumb sits on the top border row",);

    // EditingValue mode — same shape.
    let mut s2 = int_stepper_fixture(75);
    let mut buf3 = Buffer::empty(area);
    render_settings_modal(&mut buf3, area, &mut s2, false, None);
    let rect2 = s2
        .settings_breadcrumb_rect
        .expect("EditingValue must populate settings_breadcrumb_rect");
    assert_eq!(rect2.height, 1, "EditingValue rect height must be 1");
    assert!(
        (rect2.width as usize) > MODAL_TITLE.width(),
        "EditingValue rect must extend past `Settings` alone, got {}",
        rect2.width,
    );
}

/// Clicking inside the breadcrumb rect while in PickingEnum
/// mode dispatches the preview-revert action AND transitions
/// back to Browse.
///
/// Two test variants pin both
/// (a) the no-preview path where original == current value, and
/// (b) the preview-then-click path where the user navigated to
/// a different choice — the revert Action MUST carry the
/// original value, not the navigated-to value. The previous
/// version accepted `Action(_) | Changed` which masked a
/// regression where the revert was forgotten entirely.
#[test]
fn click_settings_breadcrumb_collapses_picker_to_browse() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let mut s = enter_picker_for("theme");
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    let rect = s
        .settings_breadcrumb_rect
        .expect("PickingEnum must populate breadcrumb rect");

    // Synthesize a click at the rect's center.
    let click_x = rect.x + rect.width / 2;
    let click_y = rect.y;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        click_x,
        click_y,
    );
    // For preview-supporting enums (theme), the breadcrumb-
    // click revert dispatches `Action::PreviewTheme(original)`.
    // The original canonical for the default theme is
    // `"echo-agent-night"`. Tightened from the previous `Action(_) |
    // Changed` to lock in the revert contract.
    match outcome {
        SettingsKeyOutcome::Action(Action::PreviewTheme(orig)) => {
            assert_eq!(
                orig, "echo-agent-night",
                "breadcrumb-click revert must carry the original canonical",
            );
        }
        other => panic!(
            "expected Action(PreviewTheme(\"echo_agent_night\")) — the keyboard \
             Esc-equivalent revert — got {other:?}",
        ),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "after the breadcrumb click the mode must be Browse, got {:?}",
        s.mode(),
    );
}

/// Breadcrumb is hierarchical up: even with deep-link `close_on_picker_exit`,
/// click returns to Browse (never Close / ActionThenClose).
#[test]
fn click_settings_breadcrumb_ignores_close_on_picker_exit() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let mut s = enter_picker_for("theme");
    s.close_on_picker_exit = true;
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    let rect = s
        .settings_breadcrumb_rect
        .expect("PickingEnum must populate breadcrumb rect");

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        rect.x + rect.width / 2,
        rect.y,
    );
    assert!(
        !matches!(
            outcome,
            SettingsKeyOutcome::Close | SettingsKeyOutcome::ActionThenClose(_)
        ),
        "breadcrumb must not dismiss the modal, got {outcome:?}"
    );
    match outcome {
        SettingsKeyOutcome::Action(Action::PreviewTheme(orig)) => {
            assert_eq!(orig, "echo-agent-night");
        }
        other => panic!("expected preview revert Action, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "breadcrumb must return to Browse, got {:?}",
        s.mode()
    );
    assert!(!s.close_on_picker_exit);
}

/// Sibling of `click_settings_breadcrumb_collapses_picker_to_browse`
/// that exercises the preview-then-click path: user navigates
/// to a different theme via Down arrow (Action::PreviewTheme
/// dispatched live), then clicks the breadcrumb. The revert
/// Action MUST carry the ORIGINAL value (default theme), not
/// the navigated-to value.
#[test]
fn click_settings_breadcrumb_after_nav_reverts_to_original() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let mut s = enter_picker_for("theme");
    // Navigate to a different theme so original != current.
    // The picker exposes `choices_idx`; the registry's theme
    // choices include at least 2 entries so we can safely
    // advance.
    let (orig_canonical_owned, advanced_idx) = match &s.mode() {
        SettingsModalMode::PickingEnum {
            choices_idx,
            original_value,
            ..
        } => {
            let orig = match original_value {
                SettingValue::Enum(c) => c.to_string(),
                other => panic!("expected SettingValue::Enum, got {other:?}"),
            };
            (orig, *choices_idx)
        }
        other => panic!("expected PickingEnum, got {other:?}"),
    };
    // Pick a different index. The default theme is `echo-agent-night`
    // (index 1 per the registry); advance to index 0 to ensure
    // we're navigating to a different value.
    let target_idx = if advanced_idx == 0 { 1 } else { 0 };
    match s.mode() {
        SettingsModalMode::PickingEnum {
            ref mut choices_idx,
            ..
        } => {
            *choices_idx = target_idx;
        }
        _ => unreachable!(),
    }

    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    let rect = s
        .settings_breadcrumb_rect
        .expect("PickingEnum must populate breadcrumb rect");

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        rect.x + rect.width / 2,
        rect.y,
    );
    match outcome {
        SettingsKeyOutcome::Action(Action::PreviewTheme(orig)) => {
            assert_eq!(
                orig, orig_canonical_owned,
                "breadcrumb-click revert must carry the ORIGINAL canonical \
                 (not the navigated-to value)",
            );
        }
        other => panic!("expected Action(PreviewTheme(<original>)), got {other:?}"),
    }
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

/// `d` in PickingEnum for a preview-supporting Enum dispatches
/// an `ActionPair` that (a) reverts the live preview and (b)
/// opens the reset confirm overlay. The modal transitions to
/// Browse so the dispatch arm finds an
/// `ActiveModal::Settings { state: Browse }`.
#[test]
fn d_key_in_picking_enum_dispatches_open_reset_confirm() {
    let mut s = enter_picker_for("theme");
    let outcome = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    );
    match outcome {
        SettingsKeyOutcome::ActionPair(
            Action::PreviewTheme(orig),
            Action::OpenResetConfirm { key },
        ) => {
            assert_eq!(
                key, "theme",
                "OpenResetConfirm key must be the active picker setting",
            );
            // Default theme is `echo-agent-night`; entering the picker
            // captures `original_value = current value = echo_agent_night`,
            // so the revert dispatches with that canonical.
            assert_eq!(
                orig, "echo-agent-night",
                "PreviewTheme revert must carry the original canonical",
            );
        }
        other => {
            panic!("expected ActionPair(PreviewTheme(_), OpenResetConfirm), got {other:?}")
        }
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "picker must collapse to Browse before dispatching reset \
         (dispatch arm panics in debug if it sees a sub-pane mode)",
    );
}

/// `d` in the Int stepper dispatches `Action::OpenResetConfirm`
/// for the active setting AND transitions back to Browse.
///
/// Also verifies the pending
/// buffer is discarded — a user who stepped to a new value
/// then pressed `d` should not have the in-flight value leak
/// past the mode transition. Asserts the mode is structurally
/// `Browse` with no lingering pending buffer.
#[test]
fn d_key_in_int_stepper_dispatches_open_reset_confirm() {
    let mut s = int_stepper_fixture(75);
    assert!(
        matches!(s.mode(), SettingsModalMode::EditingValue { .. }),
        "fixture must start in EditingValue",
    );
    // Step Up so the pending buffer diverges from the default
    // (`max_thoughts_width` default is 100; this leaves the
    // buffer at "80" — not at default, not at the seeded 75).
    let _ = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(
        int_stepper_buffer(&s),
        "80",
        "Up arrow must have stepped from 75 to 80",
    );
    let outcome = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    );
    match outcome {
        SettingsKeyOutcome::Action(Action::OpenResetConfirm { key }) => {
            assert_eq!(
                key, "max_thoughts_width",
                "OpenResetConfirm key must be the active stepper setting",
            );
        }
        other => panic!("expected OpenResetConfirm action, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "stepper must collapse to Browse before dispatching reset",
    );
    // Pending buffer must be discarded — no lingering
    // EditingValue payload. The matches! above checks the
    // discriminant; this `!matches!` is the explicit
    // assertion that we did NOT carry the in-flight buffer
    // through the mode change.
    assert!(
        !matches!(&s.mode(), SettingsModalMode::EditingValue { .. }),
        "stepper's pending edit must NOT survive the d-reset \
         transition, got {:?}",
        s.mode(),
    );
}

/// `d` in the String editor is
/// a typeable character — NOT a reset shortcut (the picker
/// and Int stepper get `d reset`; the String editor doesn't,
/// a deliberate asymmetry). This test
/// guards against a future change that "adds `d` interception for
/// consistency" without realizing it would silently break
/// String editing of any value containing a `d`.
#[test]
fn d_key_in_string_editor_inserts_into_buffer() {
    let mut s = editor_render_fixture("Gro", 3);
    let outcome = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "`d` in String editor must mutate the buffer (Changed); \
         got {outcome:?}",
    );
    assert!(
        !matches!(
            outcome,
            SettingsKeyOutcome::Action(_) | SettingsKeyOutcome::ActionPair(_, _)
        ),
        "`d` in String editor MUST NOT dispatch a reset (no Action)",
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::EditingValue { .. }),
        "mode must STILL be EditingValue (no transition); got {:?}",
        s.mode(),
    );
    assert_eq!(s.editing_buffer(), Some("Grod"));
    assert_eq!(
        s.editing_cursor_byte(),
        Some(4),
        "cursor must advance past the inserted `d`",
    );
}

/// Clicks OUTSIDE the
/// breadcrumb rect — to the immediate left of the leading
/// `─ ` decoration, or past the right edge — must be no-ops.
/// An earlier version tested "click outside
/// Settings" before the rect was widened to span
/// the FULL breadcrumb; the widening shifted the "outside"
/// boundaries but the no-op contract is unchanged.
#[test]
fn click_outside_settings_breadcrumb_is_noop() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let mut s = enter_picker_for("theme");
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    let rect = s
        .settings_breadcrumb_rect
        .expect("PickingEnum must populate breadcrumb rect");
    // Click 1 cell PAST the rect's right edge — on the
    // trailing ` ─` decoration or the empty modal interior.
    let past_right_x = rect.x + rect.width + 2;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        past_right_x,
        rect.y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "click past the right edge of breadcrumb must be Unchanged; \
         got {outcome:?}",
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::PickingEnum { .. }),
        "mode must STILL be PickingEnum (no transition fired); \
         got {:?}",
        s.mode(),
    );
    // Click 1 cell BEFORE the rect's left edge — on the
    // leading `─ ` decoration.
    let before_left_x = rect.x.saturating_sub(1);
    let outcome2 = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        before_left_x,
        rect.y,
    );
    assert!(
        matches!(outcome2, SettingsKeyOutcome::Unchanged),
        "click before the left edge of breadcrumb must be Unchanged; \
         got {outcome2:?}",
    );
}

/// Hovering the breadcrumb flips `breadcrumb_hovered` and the
/// mouse handler returns `Changed` so the renderer repaints.
/// Color assertions removed — they depend on whichever theme is
/// loaded in the thread-local cache, which varies between local
/// runs and Bazel CI (theme preview for the "theme" picker can
/// swap the active theme mid-test).
#[test]
fn hover_breadcrumb_flips_state_and_returns_changed() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let mut s = enter_picker_for("theme");
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    let rect = s
        .settings_breadcrumb_rect
        .expect("PickingEnum must populate breadcrumb rect");
    assert!(!s.breadcrumb_hovered, "initially not hovered");
    // Move onto breadcrumb.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Moved,
        rect.x + rect.width / 2,
        rect.y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Moved onto breadcrumb must return Changed; got {outcome:?}",
    );
    assert!(s.breadcrumb_hovered, "must be hovered after Moved");
    // Move off.
    let _ = handle_settings_mouse(&mut s, MouseEventKind::Moved, area.x, area.y);
    assert!(
        !s.breadcrumb_hovered,
        "moving outside must clear breadcrumb_hovered",
    );
}

/// `d` must be inert on a consent chooser, not merely hidden from the
/// footer.
#[test]
fn consent_chooser_drops_tip_and_reset() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 40,
    };
    let screen = |s: &mut SettingsModalState| {
        let mut buf = Buffer::empty(area);
        render_settings_modal(&mut buf, area, s, false, None);
        (0..area.height)
            .map(|y| buf_row_text(&buf, y, area.x, area.width))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let mut consent = enter_picker_for("coding_data_sharing");
    let text = screen(&mut consent);
    assert!(
        !text.contains("Ask EchoAgent"),
        "consent chooser must not render the docs tip:\n{text}"
    );
    assert!(
        !text.contains("d reset"),
        "consent chooser must not offer reset:\n{text}"
    );
    assert!(
        text.contains("Enter select"),
        "the other footer hints must survive:\n{text}"
    );

    let outcome = handle_settings_key(
        &mut consent,
        &KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "`d` must be inert on a consent chooser, got {outcome:?}"
    );
    assert!(
        matches!(consent.mode(), SettingsModalMode::PickingEnum { .. }),
        "`d` must leave the chooser open, got {:?}",
        consent.mode()
    );

    let mut ordinary = enter_picker_for("theme");
    let text = screen(&mut ordinary);
    assert!(
        text.contains("d reset") && text.contains("Ask EchoAgent"),
        "ordinary pickers keep the tip and the reset hint:\n{text}"
    );
    assert!(
        text.contains("Enter select") && !text.contains("Enter commit"),
        "every chooser selects an answer rather than committing a value:\n{text}"
    );
}

/// The row-list-with-search-bar layout reserves row 1 (below
/// the search bar) for a `─` divider in `gray_dim` — palette
/// parity.
#[test]
fn search_bar_renders_divider_below() {
    let mut s = make_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    // Use the full settings render so we exercise the same
    // layout the user sees.
    render_settings_modal(&mut buf, area, &mut s, false, None);

    // Find the row containing ` search:` (the search bar) — the
    // divider row is the next row.
    let mut search_y: Option<u16> = None;
    for y in 0..area.height {
        let txt = buf_row_text(&buf, y, area.x, area.width);
        if txt.contains("/ to search") || txt.contains(" search: ") {
            search_y = Some(y);
            break;
        }
    }
    let search_y = search_y.expect("must find search bar row");
    let divider_y = search_y + 1;

    // The divider must span
    // the full row width, not just the first cell. Count `─`
    // cells across the row's interior (excluding the modal
    // borders) and assert ≥ half the width is `─`. Also pin
    // the color across multiple cells, not just the first.
    let mut box_count = 0usize;
    let mut wrong_color_cells = 0usize;
    let mut first_box_cell_fg = None;
    let mut last_box_cell_fg = None;
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell((x, divider_y))
            && cell.symbol() == "\u{2500}"
        {
            box_count += 1;
            if cell.fg != theme.gray_dim {
                wrong_color_cells += 1;
            }
            if first_box_cell_fg.is_none() {
                first_box_cell_fg = Some(cell.fg);
            }
            last_box_cell_fg = Some(cell.fg);
        }
    }
    assert!(
        box_count > 0,
        "row immediately below search bar must contain `─` divider cells"
    );
    // The divider spans the inner area between the modal
    // borders; expect a substantial fraction of the row.
    // A regression that only painted the first 5 cells as `─`
    // would fail this — palette parity is "full-width divider".
    assert!(
        box_count >= (area.width as usize) / 4,
        "divider must span ≥ 1/4 of the row width, got {box_count} cells \
         of width {}",
        area.width,
    );
    assert_eq!(
        wrong_color_cells, 0,
        "ALL `─` cells in divider must use theme.gray_dim (palette parity); \
         {wrong_color_cells} cells diverged",
    );
    // First and last `─` cells share the same fg — defensive.
    assert_eq!(
        first_box_cell_fg, last_box_cell_fg,
        "divider color must be consistent across the row",
    );
}

// ---------- max_thoughts_width live wrap preview ----------
//
// The preview block renders below the Int stepper inside the
// EditingValue sub-pane when the active setting key is
// `max_thoughts_width`. Tests cover render presence, the
// title/content style contracts (bold-italic-lowercase title,
// italic content), the bg-tint distinction between title and
// content rows, the wrap-width contract (no content row wider
// than `pending_value`), the clamp behaviour when the terminal
// is narrower than the pending value, omission on too-narrow /
// too-short viewports, gating to the `max_thoughts_width` key
// alone, and the live re-wrap on stepper change.

/// Helper: render the EditingValue sub-pane for
/// `max_thoughts_width` at a given starting buffer value into a
/// fresh `Buffer` of the supplied area, and return `(buf, state)`.
/// The stepper renders at the top of `area`; the preview (if
/// rendered) anchors to the bottom of `area`.
fn render_max_thoughts_width_at(value: i64, area: Rect) -> (Buffer, SettingsModalState) {
    let mut s = int_stepper_fixture(value);
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);
    (buf, s)
}

/// Find the first row containing the exact `needle` substring.
fn find_text_row(buf: &Buffer, area: Rect, needle: &str) -> Option<u16> {
    for y in area.y..area.y + area.height {
        let row = buf_row_text(buf, y, area.x, area.width);
        if row.contains(needle) {
            return Some(y);
        }
    }
    None
}

/// Test 1: the preview renders directly below the stepper with
/// exactly 1 blank row of separation. The implementation no
/// longer renders in-pane stepper hints, so the spec's
/// "1 blank row above preview" anchors to the stepper row
/// itself: `preview_title_y == stepper_y + 2`. This replaces a
/// prior bottom-anchor placement; the assertion locks the corrected
/// top-anchor in place.
#[test]
fn max_thoughts_width_preview_renders_below_stepper() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let (buf, s) = render_max_thoughts_width_at(85, area);

    // Locate the stepper row (the row containing the `‹` arrow).
    let stepper_y =
        find_text_row(&buf, area, int_stepper_left_glyph()).expect("stepper row must render");
    // Locate the preview title row.
    let preview_y = find_text_row(&buf, area, "preview")
        .expect("`preview` title row must render below the stepper");
    // Exact placement: 1 blank row between stepper and preview
    // title, regardless of `area.height` (no bottom-anchoring).
    assert_eq!(
        preview_y,
        stepper_y + 2,
        "preview title must sit exactly 1 blank row below the stepper \
         (stepper_y={stepper_y}, preview_y={preview_y}); a multi-row gap \
         indicates a regression to the prior bottom-anchor placement",
    );
    // Row between stepper and preview is blank — no leakage
    // from either side.
    let gap_row = buf_row_text(&buf, stepper_y + 1, area.x, area.width);
    assert!(
        gap_row.trim().is_empty(),
        "the row between the stepper and the preview must be blank; \
         got {gap_row:?}",
    );
    // And the adornment hit-rects should still be populated
    // (the preview MUST NOT cannibalize the stepper render).
    let (dec_rect, inc_rect) = s.editor_adornment_rects;
    assert!(
        dec_rect.width > 0 && inc_rect.width > 0,
        "stepper arrows must still populate adornment rects after preview render",
    );
}

/// Test 2: the title row is bold + italic + lowercase
/// `preview`. We sample the cell at the title row's `p`
/// (column 0 of the preview block, since the preview is
/// left-aligned within `area`) and assert the modifiers.
#[test]
fn max_thoughts_width_preview_title_is_bold_italic_lowercase() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let (buf, _) = render_max_thoughts_width_at(85, area);
    let preview_y = find_text_row(&buf, area, "preview").expect("preview title must render");
    // Assert the exact lowercase substring (the row must NOT
    // contain "Preview" or "PREVIEW").
    let row = buf_row_text(&buf, preview_y, area.x, area.width);
    assert!(
        row.contains("preview"),
        "title row must contain lowercase `preview`; row={row:?}",
    );
    assert!(
        !row.contains("Preview") && !row.contains("PREVIEW"),
        "title row must NOT contain capitalised forms; row={row:?}",
    );
    // Sample the first cell of the title text.
    let cell = buf
        .cell((area.x, preview_y))
        .expect("preview title cell at column 0");
    assert_eq!(cell.symbol(), "p", "expected `p` at title column 0");
    assert!(
        cell.modifier.contains(Modifier::BOLD),
        "title cell must carry Modifier::BOLD; got {:?}",
        cell.modifier,
    );
    assert!(
        cell.modifier.contains(Modifier::ITALIC),
        "title cell must carry Modifier::ITALIC; got {:?}",
        cell.modifier,
    );
}

/// Test 3: content rows carry `Modifier::ITALIC` (matches the
/// scrollback's thinking-text style convention).
#[test]
fn max_thoughts_width_preview_content_is_italic() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let (buf, _) = render_max_thoughts_width_at(85, area);
    let preview_y = find_text_row(&buf, area, "preview").expect("preview title must render");
    // The first content row is the row immediately below the
    // title. Sample column 0 — the first character of the
    // wrapped sample text (`L` from "Let me trace through ...").
    let content_y = preview_y + 1;
    let cell = buf
        .cell((area.x, content_y))
        .expect("preview content cell at column 0");
    assert_eq!(
        cell.symbol(),
        "L",
        "expected `L` from sample text at column 0"
    );
    assert!(
        cell.modifier.contains(Modifier::ITALIC),
        "content cell must carry Modifier::ITALIC; got {:?}",
        cell.modifier,
    );
    // And NOT bold — content is italic-only (bold is title-only).
    assert!(
        !cell.modifier.contains(Modifier::BOLD),
        "content cell must NOT carry Modifier::BOLD (title-only); got {:?}",
        cell.modifier,
    );
}

/// Test 4: the title row visually distinguishes itself from the
/// content rows via two independent signals — (1) a different
/// `bg` token (`bg_visual` vs `bg_highlight`) and (2) a
/// `Modifier::UNDERLINED` that gives consistent visual weight
/// regardless of how much the bg tokens differ in luma.
///
/// The previous name `_title_bg_is_darker_than_content_bg` was
/// misleading: on dark themes (EchoAgentNight, TokyoNight, RosePine
/// Moon) `bg_visual` is actually *lighter* than `bg_highlight`;
/// only on the EchoAgentDay light theme is title darker. The
/// contract that the rendering code actually relies on is "title
/// uses the heavier / more-saturated `bg_visual` token, content
/// uses `bg_highlight`, plus an UNDERLINED title modifier for
/// themes where the luma delta is small (TokyoNight)". The test
/// now matches that contract.
#[test]
fn max_thoughts_width_preview_title_styling_distinguishes_from_content() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let (buf, _) = render_max_thoughts_width_at(85, area);
    let preview_y = find_text_row(&buf, area, "preview").expect("preview title must render");
    let title_cell = buf
        .cell((area.x, preview_y))
        .expect("title cell at column 0");
    let content_cell = buf
        .cell((area.x, preview_y + 1))
        .expect("content cell at column 0");
    // Wiring assertion: rendered cells use the current-theme
    // bg tokens (tautological under NO_COLOR; meaningful when
    // truecolor is on).
    let theme = Theme::current();
    assert_eq!(
        title_cell.bg, theme.bg_visual,
        "title bg must be theme.bg_visual; got {:?}",
        title_cell.bg,
    );
    assert_eq!(
        content_cell.bg, theme.bg_highlight,
        "content bg must be theme.bg_highlight; got {:?}",
        content_cell.bg,
    );
    // The title carries UNDERLINED in addition to BOLD + ITALIC.
    // This is the theme-neutral cue that demarcates the title
    // when the bg luma delta is small (TokyoNight).
    assert!(
        title_cell.modifier.contains(Modifier::UNDERLINED),
        "title cell must carry Modifier::UNDERLINED for theme-neutral \
         visual weight; got {:?}",
        title_cell.modifier,
    );
    assert!(
        !content_cell.modifier.contains(Modifier::UNDERLINED),
        "content cell must NOT carry Modifier::UNDERLINED (title-only); \
         got {:?}",
        content_cell.modifier,
    );
    // Contrast assertion: regardless of the active palette,
    // the raw / un-quantized theme tokens differ. We use the
    // raw theme directly so this assertion survives `NO_COLOR`
    // / 256-color quantization.
    let raw_theme = match crate::theme::Theme::current_kind() {
        crate::theme::ThemeKind::EchoAgentNight => crate::theme::Theme::echo_agent_night(),
        crate::theme::ThemeKind::TokyoNight => crate::theme::Theme::tokyonight(),
        crate::theme::ThemeKind::EchoAgentDay => crate::theme::Theme::echo_agent_day(),
        crate::theme::ThemeKind::RosePineMoon => crate::theme::Theme::rosepine_moon(),
        // Resolved via `Theme::current()` rather than a constructor
        // because `theme::oscura` is a private module.
        crate::theme::ThemeKind::OscuraMidnight => crate::theme::Theme::current(),
        crate::theme::ThemeKind::Auto => crate::theme::Theme::echo_agent_night(),
    };
    assert_ne!(
        raw_theme.bg_visual, raw_theme.bg_highlight,
        "raw theme tokens bg_visual + bg_highlight must be distinct so the preview \
         reads as a contained block with two-tone bg",
    );
}

/// Test 5: content rows wrap at the pending value — no
/// rendered content row's text width exceeds the pending
/// stepper value.
#[test]
fn max_thoughts_width_preview_wraps_at_pending_value() {
    // `area.width` is comfortably wider than the pending value
    // so the clamp path doesn't fire — we want to exercise the
    // pure-pending wrap path.
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 24,
    };
    let (buf, _) = render_max_thoughts_width_at(50, area);
    let preview_y = find_text_row(&buf, area, "preview").expect("preview title must render");
    // Walk content rows below the title until a blank row
    // (signals end of preview block).
    let mut content_lines: Vec<String> = Vec::new();
    for y in (preview_y + 1)..area.height {
        let row = buf_row_text(&buf, y, area.x, area.width);
        let trimmed = row.trim_end();
        if trimmed.is_empty() {
            break;
        }
        content_lines.push(trimmed.to_string());
    }
    // Strengthen the wrap-shape assertion.
    // A regression that disabled wrap and rendered a single
    // truncated line at 50 cols would satisfy `w <= 50` but
    // fail `len >= 2`. The ~189-char sample at pending=50
    // wraps to ≥ 3 rows in practice; we assert ≥ 2 to leave
    // headroom for word-boundary jitter.
    assert!(
        content_lines.len() >= 2,
        "wrap must produce at least 2 content rows at pending=50 for the \
         ~189-char sample; got {} rows: {content_lines:?}",
        content_lines.len(),
    );
    for line in &content_lines {
        // `UnicodeWidthStr::width` counts display columns.
        let w = line.width();
        assert!(
            w <= 50,
            "content line {line:?} has display width {w} > pending_value 50",
        );
    }
}

/// Test 6: when the terminal area is narrower than the pending
/// value, the preview clamps to `area.width`. The title stays
/// plain `preview` (no suffix) — the clamp signal has been
/// moved to a note row rendered below the content; see
/// `clamped_preview_renders_note_below_content` for the note
/// assertion. This test focuses on the wrap shape itself.
#[test]
fn max_thoughts_width_preview_clamps_when_terminal_narrower_than_value() {
    // 60-wide area, pending = 85 → clamp to 60.
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 24,
    };
    let (buf, _) = render_max_thoughts_width_at(85, area);
    let preview_y = find_text_row(&buf, area, "preview").expect("preview title must render");
    let title_row = buf_row_text(&buf, preview_y, area.x, area.width);
    // Title must NOT carry the legacy `clamped to N cols`
    // suffix — that signal lives in the note row now.
    assert!(
        !title_row.contains("clamped"),
        "title row must NOT contain the `clamped` suffix anymore — the clamp \
         indicator moved to a note row below the content; title_row={title_row:?}",
    );
    // No content line may exceed area.width = 60 cols. Also
    // assert ≥ 2 content rows to guard against a regression
    // that swapped wrap for truncation. We
    // stop scanning at the first non-content line — either a
    // blank gap or the new `note: clamped at …` row.
    let mut clamped_lines: Vec<String> = Vec::new();
    for y in (preview_y + 1)..area.height {
        let row = buf_row_text(&buf, y, area.x, area.width);
        let trimmed = row.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with("note:") {
            break;
        }
        let w = trimmed.width();
        assert!(
            w <= 60,
            "clamped content line {trimmed:?} has display width {w} > clamp 60",
        );
        clamped_lines.push(trimmed.to_string());
    }
    assert!(
        clamped_lines.len() >= 2,
        "clamped wrap must produce ≥ 2 content rows at width=60; got {} rows: \
         {clamped_lines:?}",
        clamped_lines.len(),
    );
}

/// When the preview is clamped (pending > terminal width) AND
/// the modal has enough vertical room, the title stays plain
/// `preview` (no suffix) AND a `note: clamped at N cols` row
/// renders immediately below the wrap content. The note uses
/// `theme.text_secondary` fg, no bg tint, no modifier — it
/// reads as chrome-level text aligned with the preview's
/// left edge.
#[test]
fn clamped_preview_renders_note_below_content() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 24,
    };
    let (buf, _) = render_max_thoughts_width_at(85, area);
    let preview_y = find_text_row(&buf, area, "preview").expect("preview title must render");

    // Title row carries the lowercase `preview` text and no
    // `clamped` suffix.
    let title_row = buf_row_text(&buf, preview_y, area.x, area.width);
    assert!(
        title_row.contains("preview"),
        "title row must contain lowercase `preview`; row={title_row:?}",
    );
    assert!(
        !title_row.contains("clamped"),
        "title row must NOT contain `clamped`; the clamp note lives below \
         the wrap content now; row={title_row:?}",
    );

    // Walk forward to find the note. Wrap content runs on
    // consecutive non-empty rows; then exactly one blank row
    // serves as the visual gap; then the `note:` row sits
    // below that gap. Stop at the first row that breaks the
    // "content then gap then note" pattern.
    let mut note_y: Option<u16> = None;
    let mut last_content_y: Option<u16> = None;
    let mut saw_blank_gap = false;
    for y in (preview_y + 1)..(area.y + area.height) {
        let row = buf_row_text(&buf, y, area.x, area.width);
        let trimmed = row.trim_end();
        if trimmed.starts_with("note:") {
            note_y = Some(y);
            break;
        }
        if trimmed.is_empty() {
            if saw_blank_gap {
                // Two consecutive blank rows — content ended
                // and we're past the note's slot too. Bail.
                break;
            }
            saw_blank_gap = true;
            continue;
        }
        // Non-blank, non-note row. If we already saw the blank
        // gap and now see content again, the layout violated
        // the contract — bail without finding the note.
        if saw_blank_gap {
            break;
        }
        last_content_y = Some(y);
    }
    let note_y = note_y.expect("clamped preview must render a `note:` row below content");
    let last_content_y =
        last_content_y.expect("clamped preview must render ≥ 1 content row before the note");
    assert_eq!(
        note_y,
        last_content_y + 2,
        "the `note:` row must sit one blank row below the last content row; \
         last_content_y={last_content_y} note_y={note_y}",
    );

    // The note text reports the actual clamp width (area.width = 60).
    let note_row = buf_row_text(&buf, note_y, area.x, area.width);
    assert!(
        note_row.contains("note: clamped at 60 cols"),
        "note row must read `note: clamped at 60 cols`; got {note_row:?}",
    );

    // Style assertions: the note cell at column 0 (the `n` of
    // "note:") must carry `theme.text_secondary` fg, no bg
    // tint past `theme.bg_base`, and no modifier. We sample
    // the modifier directly; the fg/bg colors are theme-
    // dependent but compared symbolically to the theme tokens
    // in use.
    let theme = Theme::current();
    let cell = buf
        .cell((area.x, note_y))
        .expect("note cell at column 0 must exist");
    assert_eq!(cell.symbol(), "n", "expected `n` at note column 0");
    assert_eq!(
        cell.fg, theme.text_secondary,
        "note fg must be theme.text_secondary; got {:?}",
        cell.fg,
    );
    assert_eq!(
        cell.bg, theme.bg_base,
        "note bg must be theme.bg_base (no block tint); got {:?}",
        cell.bg,
    );
    assert!(
        cell.modifier.is_empty(),
        "note cell must carry no modifier; got {:?}",
        cell.modifier,
    );
}

/// Boundary: when the preview is clamped but the modal area
/// is too short to fit an extra row below the content for the
/// note, the note is omitted. The wrap content keeps rendering
/// at the full vertical budget — content takes priority.
#[test]
fn clamped_note_omitted_when_insufficient_height() {
    // The stepper consumes the first ~5 rows (title + 1-line
    // desc + gap + stepper). At width 60 the wrap of the
    // sample text produces ≥ 6 wrap lines; we pick an area
    // height that allows the stepper + gap + title + content
    // to fill every remaining row with NO slack for the note.
    //
    // Concretely: total area height = stepper_header (~4) +
    // preview_block (gap 1 + title 1 + content N). We want
    // content_rows == area.height - 2 (no slack). Pick a
    // height that's exactly tight enough.
    //
    // We sweep upward to find the largest height at which no
    // note renders, and assert the area's wrap content fills
    // every available content row.
    let mut tight_height: Option<u16> = None;
    for h in 5u16..30u16 {
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: h,
        };
        let (buf, _) = render_max_thoughts_width_at(85, area);
        // Skip heights at which the preview is omitted
        // entirely (too short).
        let Some(preview_y) = find_text_row(&buf, area, "preview") else {
            continue;
        };
        let note_present = find_text_row(&buf, area, "note: clamped").is_some();
        if !note_present {
            tight_height = Some(h);
            // Sanity: verify wrap content still rendered for
            // ≥ 1 row below the title — i.e. the preview did
            // render, the note was just omitted for room.
            let content_y = preview_y + 1;
            let content = buf_row_text(&buf, content_y, area.x, area.width);
            assert!(
                !content.trim().is_empty(),
                "wrap content row directly below title must render even when the \
                 note is omitted; content={content:?}",
            );
        }
    }
    // Sanity-check: we found at least one height at which the
    // note was omitted (otherwise the boundary fixture is
    // useless — every height fits the note). At the tightest
    // such height verify the note really IS absent (a final
    // explicit assertion in case the loop's discovery state
    // bit-rots).
    let tight = tight_height.expect(
        "the height sweep must find at least one short-but-rendered-preview height \
         at which the note is omitted — adjust the sweep range if the fixture changes",
    );
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: tight,
    };
    let (buf, _) = render_max_thoughts_width_at(85, area);
    assert!(
        find_text_row(&buf, area, "note: clamped").is_none(),
        "at the tight boundary height (h={tight}) the clamped note must NOT render",
    );
}

/// When the preview is NOT clamped (modal width >= pending
/// value), there's no note row anywhere in the buffer.
#[test]
fn unclamped_preview_omits_note() {
    // 120-col area, pending = 50 → no clamp.
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 24,
    };
    let (buf, _) = render_max_thoughts_width_at(50, area);
    assert!(
        find_text_row(&buf, area, "preview").is_some(),
        "preview must render at this size",
    );
    assert!(
        find_text_row(&buf, area, "note:").is_none(),
        "unclamped preview must not render a `note:` row",
    );
    assert!(
        find_text_row(&buf, area, "clamped").is_none(),
        "unclamped preview must not contain the word `clamped` anywhere",
    );
}

/// Test 7: when the modal area is too short to fit the
/// preview's minimum vertical budget (stepper header + 5 rows
/// below = gap + title + 2 content + gap), the preview is
/// omitted. The stepper still renders alone.
#[test]
fn max_thoughts_width_preview_omitted_when_modal_too_short() {
    // The stepper alone consumes ~5 rows (title + wrapped desc
    // + gap + stepper). Total area height = 7 leaves 2 rows
    // remaining — below the 5-row preview minimum.
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 7,
    };
    let (buf, _) = render_max_thoughts_width_at(85, area);
    // The stepper still renders.
    assert!(
        find_text_row(&buf, area, int_stepper_left_glyph()).is_some(),
        "stepper must still render at short heights",
    );
    // The preview is omitted.
    assert!(
        find_text_row(&buf, area, "preview").is_none(),
        "preview must be omitted when remaining height < 5 rows",
    );
}

/// Test 8: when the modal area is narrower than 30 cols, the
/// preview is omitted. The stepper still renders alone.
#[test]
fn max_thoughts_width_preview_omitted_when_modal_too_narrow() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 28,
        height: 24,
    };
    let (buf, _) = render_max_thoughts_width_at(85, area);
    assert!(
        find_text_row(&buf, area, "preview").is_none(),
        "preview must be omitted when area.width < 30 cols",
    );
}

/// Test 9: the preview is gated on
/// `setting_key == "max_thoughts_width"`. We build a
/// synthetic Int setting under a different key and assert no
/// preview renders even though the editor sub-pane opens
/// successfully.
///
/// This guards future Int settings from accidentally inheriting
/// the preview behaviour.
#[test]
fn max_thoughts_width_preview_only_renders_for_max_thoughts_width_key() {
    let synthetic_meta = SettingMeta {
        key: "synthetic_int",
        category: SettingCategory::Advanced,
        owner: crate::settings::SettingOwner::Shared,
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
    let registry = SettingsRegistry::from_entries(vec![synthetic_meta]);
    let mut s = SettingsModalState::new(
        Arc::new(registry),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    s.transition_to_editing_int("synthetic_int", "50".to_string(), 0, 200);
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let mut buf = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf, area, &mut s, &theme);
    // Stepper still renders (the key is a registered Int).
    assert!(
        find_text_row(&buf, area, int_stepper_left_glyph()).is_some(),
        "stepper must render for synthetic Int setting",
    );
    // No preview because the key is NOT max_thoughts_width.
    assert!(
        find_text_row(&buf, area, "preview").is_none(),
        "preview must be hidden for non-max_thoughts_width Int settings",
    );
}

/// Test 10: the preview re-wraps when the stepper value
/// changes. We render at pending=50, capture the wrap shape
/// (the set of content row strings), dispatch an Up keystroke
/// to step to 55, re-render, and assert the wrap shape
/// differs.
#[test]
fn max_thoughts_width_preview_updates_when_stepper_changes() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 24,
    };
    // Capture wrap shape at pending = 50.
    let (buf_50, _) = render_max_thoughts_width_at(50, area);
    let preview_y_50 =
        find_text_row(&buf_50, area, "preview").expect("preview must render at pending 50");
    let mut wrap_50: Vec<String> = Vec::new();
    for y in (preview_y_50 + 1)..area.height {
        let row = buf_row_text(&buf_50, y, area.x, area.width);
        let trimmed = row.trim_end().to_string();
        if trimmed.is_empty() {
            break;
        }
        wrap_50.push(trimmed);
    }

    // Step Up (small step: +5) → pending becomes 55.
    let mut s = int_stepper_fixture(50);
    let outcome = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Up arrow must step the stepper; got {outcome:?}",
    );
    assert_eq!(
        int_stepper_buffer(&s),
        "55",
        "buffer must reflect the +5 step",
    );
    // Re-render at pending = 55.
    let mut buf_55 = Buffer::empty(area);
    let theme = Theme::current();
    render_editing_value(&mut buf_55, area, &mut s, &theme);
    let preview_y_55 =
        find_text_row(&buf_55, area, "preview").expect("preview must render at pending 55");
    let mut wrap_55: Vec<String> = Vec::new();
    for y in (preview_y_55 + 1)..area.height {
        let row = buf_row_text(&buf_55, y, area.x, area.width);
        let trimmed = row.trim_end().to_string();
        if trimmed.is_empty() {
            break;
        }
        wrap_55.push(trimmed);
    }
    // The wrap shape must differ — at width 55 the line breaks
    // land on different words than at width 50.
    assert_ne!(
        wrap_50, wrap_55,
        "wrap shape at pending=50 must differ from wrap shape at pending=55",
    );
    // Assert both renders actually wrap (≥ 2
    // content rows). Without this a stub that returned a
    // single truncated row at each width would pass the
    // `assert_ne!` above (different truncation points) yet
    // skip the wrap mechanism entirely.
    assert!(
        wrap_50.len() >= 2,
        "pending=50 wrap must produce ≥ 2 rows; got {}: {wrap_50:?}",
        wrap_50.len(),
    );
    assert!(
        wrap_55.len() >= 2,
        "pending=55 wrap must produce ≥ 2 rows; got {}: {wrap_55:?}",
        wrap_55.len(),
    );
}

/// Test 7b (boundary companion to Test 7): the preview *renders*
/// at `area.height == header_rows + MAX_THOUGHTS_WIDTH_PREVIEW_MIN_HEIGHT`,
/// and *omits* one row below that threshold:
/// without the just-above-threshold companion, a regression
/// that bumped `MIN_HEIGHT` to 6 would leave Test 7 passing
/// silently.
#[test]
fn max_thoughts_width_preview_renders_at_just_fits_height() {
    // The stepper header (title + 1-row desc + gap + stepper)
    // is 4 rows at width=80, so total area height needs at
    // least 4 + 5 = 9 rows for the preview to render. We test
    // both sides of the boundary.
    let just_fits = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 9,
    };
    let (buf_fit, _) = render_max_thoughts_width_at(85, just_fits);
    assert!(
        find_text_row(&buf_fit, just_fits, "preview").is_some(),
        "preview must render at the just-fits boundary height (header_rows + 5)",
    );
    // One row below the threshold: preview omitted, stepper
    // still renders.
    let just_short = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 8,
    };
    let (buf_short, _) = render_max_thoughts_width_at(85, just_short);
    assert!(
        find_text_row(&buf_short, just_short, int_stepper_left_glyph()).is_some(),
        "stepper must still render one row below the preview threshold",
    );
    assert!(
        find_text_row(&buf_short, just_short, "preview").is_none(),
        "preview must omit at one row below the just-fits boundary",
    );
}

/// Test 8b (boundary companion to Test 8): the preview
/// *renders* at `area.width == MAX_THOUGHTS_WIDTH_PREVIEW_MIN_WIDTH`
/// (= 30), and *omits* at 29.
#[test]
fn max_thoughts_width_preview_renders_at_just_fits_width() {
    let just_fits = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 24,
    };
    let (buf_fit, _) = render_max_thoughts_width_at(85, just_fits);
    assert!(
        find_text_row(&buf_fit, just_fits, "preview").is_some(),
        "preview must render at the MIN_WIDTH (30 cols) boundary",
    );
    let just_narrow = Rect {
        x: 0,
        y: 0,
        width: 29,
        height: 24,
    };
    let (buf_narrow, _) = render_max_thoughts_width_at(85, just_narrow);
    assert!(
        find_text_row(&buf_narrow, just_narrow, "preview").is_none(),
        "preview must omit one column below MIN_WIDTH",
    );
}

// ──────────────────────────────────────────────────────────────
// Auto-widen tests for max_thoughts_width EditingValue mode.
// ──────────────────────────────────────────────────────────────

/// At a wide terminal (200 cols), entering EditingValue mode
/// for `max_thoughts_width` widens the rendered modal so that
/// its popup width is `terminal_width - MAX_THOUGHTS_WIDTH_WIDENED_MARGIN`
/// (i.e. 192). The default sizing would otherwise produce a
/// 70%-of-terminal = 140-wide modal.
#[test]
fn modal_widens_when_editing_max_thoughts_width() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 200,
        height: 40,
    };
    let mut s = int_stepper_fixture(120);
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    let popup = s.window.popup_area.expect("modal must have rendered");
    let expected = area.width - MAX_THOUGHTS_WIDTH_WIDENED_MARGIN;
    assert_eq!(
        popup.width, expected,
        "widened modal width must be terminal_width - WIDENED_MARGIN (= {expected}); \
         got {} at terminal_width={}",
        popup.width, area.width,
    );
    // Sanity: the widened width is strictly greater than the
    // standard cap.
    assert!(
        popup.width > STANDARD_MAX_WIDTH,
        "widened modal must be strictly wider than STANDARD_MAX_WIDTH ({}); got {}",
        STANDARD_MAX_WIDTH,
        popup.width,
    );
}

/// Transitioning from `EditingValue { max_thoughts_width }` back
/// to `Browse` snaps the modal back to its standard width on the
/// next render frame — the widening lives in the render-time
/// active-state match, not in any persistent layout state.
#[test]
fn modal_returns_to_default_width_when_leaving_edit_mode() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 200,
        height: 40,
    };
    // Render once in widened mode to capture the wide width.
    let mut s = int_stepper_fixture(120);
    let mut buf_wide = Buffer::empty(area);
    render_settings_modal(&mut buf_wide, area, &mut s, false, None);
    let wide = s.window.popup_area.expect("wide modal must render").width;
    assert!(
        wide > STANDARD_MAX_WIDTH,
        "preconditional sanity: widened path must produce a wider modal; got {wide}",
    );

    // Transition back to Browse — re-render at the same
    // terminal size and assert the modal width capped at
    // STANDARD_MAX_WIDTH (the standard width_pct=0.70 path
    // produces 140, which is also above the cap, so the
    // binding constraint is STANDARD_MAX_WIDTH).
    s.transition_to_browse();
    let mut buf_std = Buffer::empty(area);
    render_settings_modal(&mut buf_std, area, &mut s, false, None);
    let std_w = s
        .window
        .popup_area
        .expect("standard modal must render")
        .width;
    assert_eq!(
        std_w, STANDARD_MAX_WIDTH,
        "Browse-mode modal must use STANDARD_MAX_WIDTH (= {STANDARD_MAX_WIDTH}); got {std_w}",
    );
    assert!(
        std_w < wide,
        "after exiting EditingValue, modal must SHRINK back to standard width; \
         wide={wide} std={std_w}",
    );
}

/// On a narrow terminal (100 cols < STANDARD_MAX_WIDTH +
/// WIDENED_MARGIN = 118), entering EditingValue for
/// max_thoughts_width must NOT widen the modal below the
/// standard sizing — the widening gate is conditional on
/// `widened_candidate > STANDARD_MAX_WIDTH`, otherwise we fall
/// through to the standard path. This preserves the
/// "never shrink" guarantee for narrow terminals.
#[test]
fn modal_widening_respects_terminal_width_minimum() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 40,
    };
    // Reference render in Browse mode at the same terminal
    // size — we want the widened path to produce the SAME
    // width.
    let mut s_browse = make_state();
    let mut buf_browse = Buffer::empty(area);
    render_settings_modal(&mut buf_browse, area, &mut s_browse, false, None);
    let browse_w = s_browse
        .window
        .popup_area
        .expect("browse render must produce a popup")
        .width;

    let mut s_edit = int_stepper_fixture(120);
    let mut buf_edit = Buffer::empty(area);
    render_settings_modal(&mut buf_edit, area, &mut s_edit, false, None);
    let edit_w = s_edit
        .window
        .popup_area
        .expect("edit render must produce a popup")
        .width;

    assert_eq!(
        edit_w, browse_w,
        "at narrow terminal width ({}) the EditingValue modal must match the Browse \
         modal width — widening is disabled when it would shrink below the standard \
         sizing; got edit={edit_w} browse={browse_w}",
        area.width,
    );
}

/// At a wide terminal (180 cols) with a small pending value
/// (85), the widened modal accommodates the full pending value
/// without clamping — the preview renders at `pending_value`
/// cells AND the `note: clamped at …` row is absent.
#[test]
fn preview_renders_at_full_width_when_modal_widened() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 180,
        height: 40,
    };
    let mut s = int_stepper_fixture(85);
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    let popup = s.window.popup_area.expect("modal must have rendered");
    // The widened modal must accommodate pending=85 with no
    // clamp. The interior width = popup.width - 2 (borders),
    // and the preview's effective width = min(pending,
    // interior). With popup.width = 172 (180-8) the interior
    // is 170 > 85, so no clamp.
    assert!(
        popup.width > 85 + 2,
        "widened modal interior must be wider than pending=85 cells; \
         popup.width={}",
        popup.width,
    );
    // No `note:` row anywhere in the buffer.
    assert!(
        find_text_row(&buf, area, "note:").is_none(),
        "widened modal must not render the clamped note when pending < interior width",
    );
    assert!(
        find_text_row(&buf, area, "clamped").is_none(),
        "widened modal must not contain the word `clamped` anywhere when not clamping",
    );
    // Sanity: the preview itself still renders.
    assert!(
        find_text_row(&buf, area, "preview").is_some(),
        "preview must render at the wide terminal size",
    );
}

/// Even with the modal widened, a pending value larger than
/// the widened interior still clamps — the widening doesn't
/// magically uncap the preview. On a 100-col terminal the
/// widening is disabled (Test 3) so the modal stays standard;
/// at the standard width the interior is ~94 cells, and a
/// pending of 200 still clamps.
#[test]
fn preview_remains_clamped_when_pending_exceeds_widened_width() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 40,
    };
    // Pending = 200 (the registered MAX). Widening is disabled
    // at terminal_width=100 (Test 3), so the modal stays at
    // its standard width; either way, pending > interior so
    // the preview clamps and the note must render.
    let mut s = int_stepper_fixture(200);
    let mut buf = Buffer::empty(area);
    render_settings_modal(&mut buf, area, &mut s, false, None);
    let popup = s
        .window
        .popup_area
        .expect("modal must have rendered at terminal_width=100");
    let interior = popup.width.saturating_sub(2);
    assert!(
        (interior as i64) < 200,
        "interior ({interior}) must be < pending(200) for this fixture to exercise \
         the clamped path",
    );
    // Preview still renders, AND the clamped note is present.
    assert!(
        find_text_row(&buf, area, "preview").is_some(),
        "preview must render even when clamping",
    );
    assert!(
        find_text_row(&buf, area, "note: clamped").is_some(),
        "clamped note must render when pending > interior, even after widening",
    );
}

// ---------------------------------------------------------------------------
// Locked coding_data_sharing row (ZDR / team non-admin)
// ---------------------------------------------------------------------------

fn make_locked_state(lock: CodingDataSharingLock) -> SettingsModalState {
    SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        PagerLocalSnapshot {
            coding_data_sharing_lock: Some(lock),
            ..PagerLocalSnapshot::default()
        },
    )
}

fn coding_data_sharing_row_idx(s: &SettingsModalState) -> usize {
    s.rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key, .. } if *key == "coding_data_sharing"))
        .expect("coding_data_sharing must be registered")
}

/// A locked `coding_data_sharing` row must NOT open the enum picker —
/// neither via `try_enter_picking_enum` directly (the shared entry point
/// for Enter, mouse value clicks, and the `focus_key` auto-open path) nor
/// via the Browse Enter key. With no lock, the same row opens the picker.
#[test]
fn locked_coding_data_sharing_row_does_not_open_picker() {
    for lock in [
        CodingDataSharingLock::Zdr,
        CodingDataSharingLock::TeamManaged,
    ] {
        let mut s = make_locked_state(lock);
        s.selected = coding_data_sharing_row_idx(&s);
        assert!(
            !s.try_enter_picking_enum(),
            "try_enter_picking_enum must return false for a locked row ({lock:?})"
        );
        assert!(
            matches!(s.mode(), SettingsModalMode::Browse),
            "mode must stay Browse for a locked row ({lock:?}), got {:?}",
            s.mode()
        );
        let out = handle_settings_key(&mut s, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(out, SettingsKeyOutcome::Unchanged),
            "Enter on a locked row must be a no-op ({lock:?}), got {out:?}"
        );
        assert!(matches!(s.mode(), SettingsModalMode::Browse));
    }

    // Control arm: no lock → the picker opens (existing behavior).
    let mut s = make_state();
    s.selected = coding_data_sharing_row_idx(&s);
    assert!(s.try_enter_picking_enum());
    assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
}

/// `d` on a locked row must not open the confirm dialog: the dispatch-time
/// guard would refuse the reset anyway, but only after walking the user
/// through a confirmation for a change that cannot happen.
#[test]
fn locked_coding_data_sharing_row_refuses_reset() {
    for lock in [
        CodingDataSharingLock::Zdr,
        CodingDataSharingLock::TeamManaged,
    ] {
        let mut s = make_locked_state(lock);
        s.selected = coding_data_sharing_row_idx(&s);
        let out = handle_settings_key(
            &mut s,
            &KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert!(
            matches!(out, SettingsKeyOutcome::Unchanged),
            "`d` on a locked row must be a no-op ({lock:?}), got {out:?}"
        );
    }

    // Control arm: no lock → `d` still opens the confirm dialog.
    let mut s = make_state();
    s.selected = coding_data_sharing_row_idx(&s);
    let out = handle_settings_key(
        &mut s,
        &KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
    );
    assert!(
        matches!(
            out,
            SettingsKeyOutcome::Action(Action::OpenResetConfirm {
                key: "coding_data_sharing"
            })
        ),
        "`d` on an unlocked row must still offer reset, got {out:?}"
    );
}

/// `→ expand` stays on a locked row — that is how the lock reason is read.
#[test]
fn locked_row_footer_drops_the_keys_it_refuses() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 40,
    };
    let screen = |s: &mut SettingsModalState| {
        let mut buf = Buffer::empty(area);
        render_settings_modal(&mut buf, area, s, false, None);
        (0..area.height)
            .map(|y| buf_row_text(&buf, y, area.x, area.width))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let mut locked = make_locked_state(CodingDataSharingLock::Zdr);
    locked.selected = coding_data_sharing_row_idx(&locked);
    let text = screen(&mut locked);
    for hint in ["d reset", "Enter edit", "Space toggle"] {
        assert!(
            !text.contains(hint),
            "a locked row must not advertise `{hint}`:\n{text}"
        );
    }
    assert!(
        text.contains("expand"),
        "`→ expand` reads the lock reason and must survive:\n{text}"
    );

    let mut unlocked = make_state();
    unlocked.selected = coding_data_sharing_row_idx(&unlocked);
    let text = screen(&mut unlocked);
    assert!(
        text.contains("d reset") && text.contains("Enter edit"),
        "an unlocked row keeps the full footer:\n{text}"
    );
}

/// Locked rows drop the `›` enter-affordance and render a per-variant
/// value: ZDR replaces opt-in/out with "ZDR"; team-managed keeps the
/// value with an " · Admin Managed" suffix. Unlocked rows keep the plain
/// value + chevron.
#[test]
fn locked_coding_data_sharing_row_renders_locked_value_without_chevron() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 60,
    };
    let theme = Theme::current();
    let chevron = crate::glyphs::chevron();

    let mut s = make_locked_state(CodingDataSharingLock::Zdr);
    let idx = coding_data_sharing_row_idx(&s);
    s.selected = idx;
    let mut buf = Buffer::empty(area);
    render_rows(&mut buf, area, &mut s, &theme);
    let rect = s.row_rects[idx];
    let line = buf_row_text(&buf, rect.y, area.x, area.width);
    assert!(
        line.contains("ZDR") && !line.contains("Opt"),
        "ZDR lock must replace the opt-in/out value with `ZDR`: {line:?}"
    );
    assert!(
        !line.contains(chevron),
        "locked row must not render the `{chevron}` enter affordance: {line:?}"
    );

    let mut s = make_locked_state(CodingDataSharingLock::TeamManaged);
    s.selected = idx;
    let mut buf = Buffer::empty(area);
    render_rows(&mut buf, area, &mut s, &theme);
    let rect = s.row_rects[idx];
    let line = buf_row_text(&buf, rect.y, area.x, area.width);
    assert!(
        line.contains("Opt out \u{00B7} Admin Managed"),
        "team-managed lock must append ` · Admin Managed`: {line:?}"
    );
    assert!(
        !line.contains(chevron),
        "locked row must not render the `{chevron}` enter affordance: {line:?}"
    );

    // Control arm: unlocked row shows the plain value + chevron.
    let mut s = make_state();
    s.selected = idx;
    let mut buf = Buffer::empty(area);
    render_rows(&mut buf, area, &mut s, &theme);
    let rect = s.row_rects[idx];
    let line = buf_row_text(&buf, rect.y, area.x, area.width);
    assert!(
        line.contains("Opt out") && !line.contains("locked"),
        "unlocked row must show the plain value: {line:?}"
    );
    assert!(
        line.contains(chevron),
        "unlocked row must keep the `{chevron}` enter affordance: {line:?}"
    );
}

/// Expanding a locked row replaces the registry description with the lock
/// reason; the unlocked expansion shows the description.
#[test]
fn locked_coding_data_sharing_expanded_description_replaces_with_reason() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 60,
    };
    let theme = Theme::current();
    // Word-wrap may split the reason across lines; normalize the whole
    // buffer to a single whitespace-collapsed string before matching.
    let flatten = |buf: &Buffer| -> String {
        (0..area.height)
            .map(|y| buf_row_text(buf, y, area.x, area.width))
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };

    let mut s = make_locked_state(CodingDataSharingLock::TeamManaged);
    let idx = coding_data_sharing_row_idx(&s);
    s.selected = idx;
    s.expanded_keys.insert("coding_data_sharing");
    let mut buf = Buffer::empty(area);
    render_rows(&mut buf, area, &mut s, &theme);
    let text = flatten(&buf);
    assert!(
        text.contains("Managed by your team admin."),
        "expanded locked row must show the lock reason: {text:?}"
    );
    // Token from the live description so this survives copy edits.
    let desc = s
        .registry
        .find("coding_data_sharing")
        .expect("registered")
        .description;
    let desc_head: String = desc
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        !text.contains(&desc_head),
        "locked expansion must replace the description, not append to it: {text:?}"
    );

    // Control arm: unlocked expansion shows the description only.
    let mut s = make_state();
    s.selected = idx;
    s.expanded_keys.insert("coding_data_sharing");
    let mut buf = Buffer::empty(area);
    render_rows(&mut buf, area, &mut s, &theme);
    let text = flatten(&buf);
    assert!(
        text.contains(&desc_head),
        "expanded row must render the registry description: {text:?}"
    );
    assert!(
        !text.contains("Managed by your team admin."),
        "unlocked expansion must not mention the team-admin lock: {text:?}"
    );
}
