//! End-to-end integration tests for the settings modal.
//!
//! Every new `SettingMeta` in `default_settings()` MUST add an entry to
//! `ALL_SETTINGS_EXERCISED` and a test for its keyboard AND mouse paths.
//!
//! Drives the modal directly through `SettingsModalState` + public
//! key/mouse handlers (same dispatch path as runtime, without chrome).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::layout::Rect;
use std::sync::Arc;

use echo_agent_pager::app::actions::Action;
use echo_agent_pager::settings::{
    EnumChoice, PagerLocalSnapshot, SettingCategory, SettingKind, SettingMeta, SettingOwner,
    SettingValue, SettingsRegistry,
};
use echo_agent_pager::views::settings_modal::{
    RowEntry, SettingsKeyOutcome, SettingsModalMode, SettingsModalState, handle_settings_key,
    handle_settings_mouse,
};
use echo_agent_runtime::agent::config::UiConfig;

// ---------------------------------------------------------------------------
// Compile-time exhaustive matrix
// ---------------------------------------------------------------------------

/// Every setting exercised by this file. Must stay in sync with
/// `SettingsRegistry::defaults().all()`.
const ALL_SETTINGS_EXERCISED: &[&str] = &[
    "compact_mode",
    "screen_mode",
    "show_timestamps",
    "show_timeline",
    "page_flip_on_send",
    "confirm_before_rewind",
    "combine_queued_prompts",
    "follow_up_behavior",
    "simple_mode",
    "vim_mode",
    "remember_tool_approvals",
    "toolset.ask_user_question.timeout_enabled",
    "keep_text_selection",
    "theme",
    "auto_dark_theme",
    "auto_light_theme",
    "render_mermaid",
    "multiline_mode",
    "permission_mode",
    "default_model",
    "max_thoughts_width",
    "scroll_speed",
    "scroll_mode",
    "scroll_lines",
    "invert_scroll",
    "display_refresh_auto_cadence",
    "coding_data_sharing",
    "default_selected_permission",
    "plan_mode",
    "show_tips",
    "auto_update",
    "fork_secondary_model",
    "show_thinking_blocks",
    "prompt_suggestions",
    "group_tool_verbs",
    "collapsed_edit_blocks",
    "respect_manual_folds",
    "hunk_tracker_mode",
    "voice_keybind_enabled",
    "voice_capture_mode",
    "voice_stt_language",
    // Contextual-hints group + its per-tip child toggles (exercised via the
    // group sub-sheet, not as top-level rows).
    "contextual_hints",
    "contextual_hints.undo",
    "contextual_hints.plan_mode",
    "contextual_hints.image_input",
    "contextual_hints.send_now",
    "contextual_hints.small_screen",
    "contextual_hints.word_select",
    "contextual_hints.ssh_wrap",
];

#[test]
fn every_registered_setting_is_exercised() {
    let reg = SettingsRegistry::defaults();
    let mut missing: Vec<&str> = Vec::new();
    for meta in reg.all() {
        if !ALL_SETTINGS_EXERCISED.contains(&meta.key) {
            missing.push(meta.key);
        }
    }
    assert!(
        missing.is_empty(),
        "settings registered but not exercised in tests/settings_e2e.rs: {missing:?}\n\
         Add a row to ALL_SETTINGS_EXERCISED + a keyboard test + a mouse test."
    );
}

#[test]
fn matrix_is_subset_of_registry() {
    // Reverse: catches stale entries left after a setting is removed.
    let reg = SettingsRegistry::defaults();
    let registered: std::collections::HashSet<&str> = reg.all().iter().map(|m| m.key).collect();
    for &key in ALL_SETTINGS_EXERCISED {
        assert!(
            registered.contains(key),
            "ALL_SETTINGS_EXERCISED contains `{key}` but no setting is registered with that key"
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_state() -> SettingsModalState {
    // Voice rows are hidden when the process gate is off (default until startup).
    echo_agent_pager::app::set_voice_mode_enabled_for_test(true);
    SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        // auto_mode_gate on so the permission_mode picker shows the full catalog
        // (including Auto); the gate-off filtering is covered by a dedicated test.
        PagerLocalSnapshot {
            auto_mode_gate: true,
            ..PagerLocalSnapshot::default()
        },
    )
}

fn press(key: KeyCode) -> KeyEvent {
    KeyEvent {
        code: key,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }
}

fn press_with(key: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code: key,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }
}

/// Find the row index of a given setting key in the modal's row list.
fn row_idx_for(state: &SettingsModalState, target: &str) -> usize {
    state
        .rows
        .iter()
        .position(|r| matches!(r, RowEntry::Setting { key, .. } if *key == target))
        .unwrap_or_else(|| panic!("setting `{target}` not present in modal rows"))
}

/// Whether `key` is hidden inside a group sub-sheet (a `SettingKind::Group`
/// child). Such keys are NOT emitted as top-level rows (`build_rows` skips
/// them), so they can't be navigated to or `d`-reset directly — only toggled
/// inside the sub-sheet. Mirrors `build_rows`'s group-child skip.
fn is_group_child(reg: &SettingsRegistry, key: &str) -> bool {
    reg.all().iter().any(|m| match &m.kind {
        SettingKind::Group { children } => children.contains(&key),
        _ => false,
    })
}

/// Move keyboard selection forward to a given setting key via `j`.
/// Target must be at-or-after current selection (forward-only).
fn navigate_to(state: &mut SettingsModalState, target: &str) {
    let goal = row_idx_for(state, target);
    assert!(
        state.selected <= goal,
        "navigate_to(`{target}`): test misconfigured — target row {goal} is BEFORE current \
         selection {}; navigate_to only walks forward",
        state.selected,
    );
    let mut guard = 0;
    while state.selected != goal {
        let outcome = handle_settings_key(state, &press(KeyCode::Char('j')));
        if matches!(outcome, SettingsKeyOutcome::Unchanged) {
            panic!(
                "navigate_to(`{target}`): walked off the bottom of the row list \
                 without finding the target (currently at row {})",
                state.selected,
            );
        }
        guard += 1;
        if guard > 100 {
            panic!("navigate_to(`{target}`): runaway navigation (100+ keystrokes)");
        }
    }
}

/// Assert that `outcome` is the typed setter Action for the given bool key.
fn assert_set_bool_action(outcome: SettingsKeyOutcome, key: &str, expected: bool) {
    let action = match outcome {
        SettingsKeyOutcome::Action(a) => a,
        other => panic!(
            "expected typed setter Action for `{key}={expected}`, but no Action was emitted \
             — outcome was {other:?}. Likely causes: focused row isn't a Bool, registry skew, \
             or click target outside the row's hit-rect."
        ),
    };
    match (key, action) {
        ("compact_mode", Action::SetCompactMode(b)) => {
            assert_eq!(b, expected, "SetCompactMode value differs from expected")
        }
        ("show_timestamps", Action::SetTimestamps(b)) => {
            assert_eq!(b, expected, "SetTimestamps value differs from expected")
        }
        ("show_timeline", Action::SetTimeline(b)) => {
            assert_eq!(b, expected, "SetTimeline value differs from expected")
        }
        ("page_flip_on_send", Action::SetPageFlipOnSend(b)) => {
            assert_eq!(b, expected, "SetPageFlipOnSend value differs from expected")
        }
        ("confirm_before_rewind", Action::SetConfirmBeforeRewind(b)) => {
            assert_eq!(
                b, expected,
                "SetConfirmBeforeRewind value differs from expected"
            )
        }
        ("combine_queued_prompts", Action::SetCombineQueuedPrompts(b)) => {
            assert_eq!(
                b, expected,
                "SetCombineQueuedPrompts value differs from expected"
            )
        }

        ("simple_mode", Action::SetSimpleMode(b)) => {
            assert_eq!(b, expected, "SetSimpleMode value differs from expected")
        }
        ("multiline_mode", Action::SetMultilineMode(b)) => {
            assert_eq!(b, expected, "SetMultilineMode value differs from expected")
        }
        ("vim_mode", Action::SetVimMode(b)) => {
            assert_eq!(b, expected, "SetVimMode value differs from expected")
        }
        ("remember_tool_approvals", Action::SetRememberToolApprovals(b)) => {
            assert_eq!(
                b, expected,
                "SetRememberToolApprovals value differs from expected"
            )
        }
        ("voice_keybind_enabled", Action::SetVoiceKeybindEnabled(b)) => {
            assert_eq!(
                b, expected,
                "SetVoiceKeybindEnabled value differs from expected"
            )
        }
        (
            "toolset.ask_user_question.timeout_enabled",
            Action::SetAskUserQuestionTimeoutEnabled(b),
        ) => {
            assert_eq!(
                b, expected,
                "SetAskUserQuestionTimeoutEnabled value differs from expected"
            )
        }

        ("show_tips", Action::SetShowTips(b)) => {
            assert_eq!(b, expected, "SetShowTips value differs from expected")
        }
        ("auto_update", Action::SetAutoUpdate(b)) => {
            assert_eq!(b, expected, "SetAutoUpdate value differs from expected")
        }
        ("respect_manual_folds", Action::SetRespectManualFolds(b)) => {
            assert_eq!(
                b, expected,
                "SetRespectManualFolds value differs from expected"
            )
        }
        ("show_thinking_blocks", Action::SetShowThinkingBlocks(b)) => {
            assert_eq!(
                b, expected,
                "SetShowThinkingBlocks value differs from expected"
            )
        }
        ("prompt_suggestions", Action::SetPromptSuggestions(b)) => {
            assert_eq!(
                b, expected,
                "SetPromptSuggestions value differs from expected"
            )
        }
        ("group_tool_verbs", Action::SetGroupToolVerbs(b)) => {
            assert_eq!(b, expected, "SetGroupToolVerbs value differs from expected")
        }
        ("collapsed_edit_blocks", Action::SetCollapsedEditBlocks(b)) => {
            assert_eq!(
                b, expected,
                "SetCollapsedEditBlocks value differs from expected"
            )
        }
        ("invert_scroll", Action::SetInvertScroll(b)) => {
            assert_eq!(b, expected, "SetInvertScroll value differs from expected")
        }
        ("display_refresh_auto_cadence", Action::SetDisplayRefreshAutoCadence(b)) => {
            assert_eq!(
                b, expected,
                "SetDisplayRefreshAutoCadence value differs from expected"
            )
        }
        (key, action) => panic!(
            "expected typed setter for `{key}={expected}`, got wrong Action variant: {action:?}"
        ),
    }
}

// ---------------------------------------------------------------------------
// Modal lifecycle — F2 / Esc / Ctrl+,
// ---------------------------------------------------------------------------

#[test]
fn f2_closes_modal_from_any_browse_position() {
    let mut s = make_state();
    let outcome = handle_settings_key(&mut s, &press(KeyCode::F(2)));
    assert!(matches!(outcome, SettingsKeyOutcome::Close));

    // Same expectation when focus is on a later row.
    let mut s = make_state();
    navigate_to(&mut s, "simple_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::F(2)));
    assert!(matches!(outcome, SettingsKeyOutcome::Close));
}

/// Esc in Browse mode is handled by the modal chrome, not the settings
/// handler — so the handler returns `Unchanged`.
#[test]
fn esc_in_browse_mode_is_chrome_handled_not_modal_handled() {
    let mut s = make_state();
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Esc should fall through to the chrome, not be matched here: {outcome:?}"
    );
}

#[test]
fn ctrl_comma_closes_modal_in_kitty_terminals() {
    let mut s = make_state();
    let outcome = handle_settings_key(
        &mut s,
        &press_with(KeyCode::Char(','), KeyModifiers::CONTROL),
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Close));
}

#[test]
fn cmd_comma_closes_modal_on_macos() {
    // SUPER == Cmd on macOS.
    let mut s = make_state();
    let outcome = handle_settings_key(&mut s, &press_with(KeyCode::Char(','), KeyModifiers::SUPER));
    assert!(matches!(outcome, SettingsKeyOutcome::Close));
}

#[test]
fn esc_in_filter_mode_exits_filter_not_modal() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    assert!(matches!(s.mode(), SettingsModalMode::FilterFocused));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    // Esc inside filter should NOT close the modal.
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

// ---------------------------------------------------------------------------
// Per-setting keyboard paths
// ---------------------------------------------------------------------------

#[test]
fn space_on_compact_mode_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "compact_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "compact_mode", true);
}

#[test]
fn space_on_show_timestamps_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "show_timestamps");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "show_timestamps", false);
}

#[test]
fn space_on_show_timeline_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "show_timeline");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    // Toggling flips the default; derived so it follows SHOW_TIMELINE_DEFAULT.
    let default_on = UiConfig::default().show_timeline_enabled();
    assert_set_bool_action(outcome, "show_timeline", !default_on);
}

#[test]
fn space_on_page_flip_on_send_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "page_flip_on_send");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    let default_on = UiConfig::default().page_flip_on_send_enabled();
    assert_set_bool_action(outcome, "page_flip_on_send", !default_on);
}

#[test]
fn space_on_confirm_before_rewind_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "confirm_before_rewind");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    let default_on = UiConfig::default().confirm_before_rewind_enabled();
    assert_set_bool_action(outcome, "confirm_before_rewind", !default_on);
}

#[test]
fn space_on_combine_queued_prompts_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "combine_queued_prompts");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    let default_on = UiConfig::default().combine_queued_prompts.unwrap_or(false);
    assert_set_bool_action(outcome, "combine_queued_prompts", !default_on);
}

#[test]
fn enter_on_follow_up_behavior_row_enters_picking_enum() {
    let mut s = make_state();
    navigate_to(&mut s, "follow_up_behavior");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on follow_up_behavior row must transition to PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(*key, "follow_up_behavior");
            assert_eq!(
                original_value,
                &SettingValue::Enum("queue"),
                "default follow_up_behavior is queue"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

#[test]
fn follow_up_behavior_picker_enter_dispatches_set_commit() {
    let mut s = make_state();
    navigate_to(&mut s, "follow_up_behavior");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // Default is queue (index 0); Down → steer.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetFollowUpBehavior(mode)) => {
            assert_eq!(mode, echo_agent_pager::appearance::FollowUpBehavior::Steer);
        }
        other => panic!("expected SetFollowUpBehavior(Steer), got {other:?}"),
    }
}

#[test]
fn space_on_simple_mode_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "simple_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "simple_mode", false);
}

#[test]
fn space_on_remember_tool_approvals_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "remember_tool_approvals");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    // Default is true, so toggling flips it off.
    assert_set_bool_action(outcome, "remember_tool_approvals", false);
}

/// The Ask-Question timeout row renders in Agent & Approval directly above
/// Plan Mode, reads the resolved default ON, and Space dispatches the typed
/// setter toggling it off.
#[test]
fn space_on_ask_user_question_timeout_dispatches_typed_setter() {
    let mut s = make_state();
    let row = row_idx_for(&s, "toolset.ask_user_question.timeout_enabled");
    assert_eq!(
        row_idx_for(&s, "plan_mode"),
        row + 1,
        "Ask-Question timeout must render directly above Plan Mode"
    );
    navigate_to(&mut s, "toolset.ask_user_question.timeout_enabled");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    // Default is true (timer armed), so toggling flips it off.
    assert_set_bool_action(outcome, "toolset.ask_user_question.timeout_enabled", false);
}

#[test]
fn enter_on_bool_row_also_toggles() {
    let mut s = make_state();
    navigate_to(&mut s, "compact_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert_set_bool_action(outcome, "compact_mode", true);
}

/// The contextual-hints group: Enter opens the sub-sheet, j navigates the three
/// child toggles, Space toggles the focused child (default ON → false), Esc
/// returns to Browse. Exercises the group row + all three children.
#[test]
fn enter_on_contextual_hints_group_opens_sub_sheet_and_toggles_children() {
    let mut s = make_state();
    navigate_to(&mut s, "contextual_hints");

    let out = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(matches!(out, SettingsKeyOutcome::Changed));
    assert!(matches!(
        s.mode(),
        SettingsModalMode::PickingGroup { child_idx: 0, .. }
    ));

    // child 0: undo.
    let out = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert!(
        matches!(
            out,
            SettingsKeyOutcome::Action(Action::SetContextualHintUndo(false))
        ),
        "Space on undo must toggle it off, got {out:?}",
    );
    // child 1: plan_mode.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('j')));
    let out = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert!(
        matches!(
            out,
            SettingsKeyOutcome::Action(Action::SetContextualHintPlanMode(false))
        ),
        "Space on plan_mode must toggle it off, got {out:?}",
    );
    // child 2: image_input.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('j')));
    let out = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert!(
        matches!(
            out,
            SettingsKeyOutcome::Action(Action::SetContextualHintImageInput(false))
        ),
        "Space on image_input must toggle it off, got {out:?}",
    );

    // Esc returns to Browse.
    let out = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(matches!(out, SettingsKeyOutcome::Changed));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

/// Mouse parity for the group: clicking the group row's value column opens the
/// sub-sheet, and clicking a child row toggles it in one click.
#[test]
fn mouse_click_on_contextual_hints_group_opens_sub_sheet_and_toggles_child() {
    let mut s = make_state();
    synth_rects(&mut s);
    let group_row = row_idx_for(&s, "contextual_hints") as u16;

    // Click the value column (chevron) → opens the sub-sheet in one click.
    let out = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        group_row,
    );
    assert!(matches!(out, SettingsKeyOutcome::Changed));
    assert!(
        matches!(s.mode(), SettingsModalMode::PickingGroup { .. }),
        "click on the group value column must open the sub-sheet, got {:?}",
        s.mode(),
    );

    // Synthesize child hit-rects (the renderer doesn't run in tests) and click
    // the first child → toggles undo off in one click.
    s.picker_choice_rects = (0..3)
        .map(|i| Rect {
            x: 0,
            y: i as u16,
            width: 80,
            height: 1,
        })
        .collect();
    let out = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        1,
        0,
    );
    assert!(
        matches!(
            out,
            SettingsKeyOutcome::Action(Action::SetContextualHintUndo(false))
        ),
        "click on the first child must toggle undo off, got {out:?}",
    );
}

// ---------------------------------------------------------------------------
// Per-setting MOUSE paths (keyboard ↔ mouse parity)
// ---------------------------------------------------------------------------

/// Lay out enough row_rects so that `handle_settings_mouse` can resolve
/// a click to the desired row index. We bypass the renderer here because
/// the test doesn't run inside a real terminal.
fn synth_rects(state: &mut SettingsModalState) {
    state.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: state.rows.len() as u16,
    };
    state.row_rects = (0..state.rows.len())
        .map(|i| Rect {
            x: 0,
            y: i as u16,
            width: 80,
            height: 1,
        })
        .collect();
    // Value column hit-rects on the right side of each row.
    state.value_hit_rects = (0..state.rows.len())
        .map(|i| Rect {
            x: 70,
            y: i as u16,
            width: 8,
            height: 1,
        })
        .collect();
}

/// Click on already-selected `compact_mode` toggles immediately.
#[test]
fn mouse_click_on_compact_mode_dispatches_typed_setter() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "compact_mode") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        5,
        row_y,
    );
    assert_set_bool_action(outcome, "compact_mode", true);
}

/// Click on an unselected row only selects it; a second click toggles.
#[test]
fn mouse_click_on_show_timestamps_two_stage_select_then_toggle() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "show_timestamps") as u16;

    // First click: select-only.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first click on a different row body should only select, got: {outcome:?}"
    );

    // Second click on the SAME row should now toggle (already focused).
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_set_bool_action(outcome, "show_timestamps", false);
}

/// Value-column click toggles `show_timeline` in one click.
#[test]
fn mouse_click_on_show_timeline_indicator_toggles_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "show_timeline") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    let default_on = UiConfig::default().show_timeline_enabled();
    assert_set_bool_action(outcome, "show_timeline", !default_on);
}

#[test]
fn mouse_click_on_page_flip_on_send_indicator_toggles_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "page_flip_on_send") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    let default_on = UiConfig::default().page_flip_on_send_enabled();
    assert_set_bool_action(outcome, "page_flip_on_send", !default_on);
}

#[test]
fn mouse_click_on_combine_queued_prompts_indicator_toggles_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "combine_queued_prompts") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    let default_on = UiConfig::default().combine_queued_prompts.unwrap_or(false);
    assert_set_bool_action(outcome, "combine_queued_prompts", !default_on);
}

#[test]
fn mouse_click_on_follow_up_behavior_indicator_opens_picker() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "follow_up_behavior") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "click on follow_up_behavior indicator should open picker, got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::PickingEnum { key, .. } if key == "follow_up_behavior"),
        "expected PickingEnum(follow_up_behavior), got {:?}",
        s.mode()
    );
}

#[test]
fn mouse_click_on_confirm_before_rewind_indicator_toggles_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "confirm_before_rewind") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    let default_on = UiConfig::default().confirm_before_rewind_enabled();
    assert_set_bool_action(outcome, "confirm_before_rewind", !default_on);
}

/// Value-column click toggles `remember_tool_approvals` in one click.
#[test]
fn mouse_click_on_remember_tool_approvals_indicator_toggles_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "remember_tool_approvals") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert_set_bool_action(outcome, "remember_tool_approvals", false);
}

/// Value-column click toggles the Ask-Question timeout in one click.
#[test]
fn mouse_click_on_ask_user_question_timeout_indicator_toggles_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "toolset.ask_user_question.timeout_enabled") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert_set_bool_action(outcome, "toolset.ask_user_question.timeout_enabled", false);
}

/// Click on the value column toggles in one click regardless of selection.
#[test]
fn mouse_click_on_simple_mode_indicator_toggles_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "simple_mode") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert_set_bool_action(outcome, "simple_mode", false);
}

/// Click on the already-selected row body toggles.
#[test]
fn mouse_click_on_already_selected_row_body_toggles() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "compact_mode") as u16;
    // compact_mode is the initial selection, so a body click toggles.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_set_bool_action(outcome, "compact_mode", true);
}

/// Click anywhere on the focused row toggles.
#[test]
fn mouse_click_at_row_edge_on_focused_row_toggles() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "compact_mode") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        70, // far right, within list_area width=80
        row_y,
    );
    assert_set_bool_action(outcome, "compact_mode", true);
}

#[test]
fn mouse_click_on_header_is_a_no_op() {
    let mut s = make_state();
    synth_rects(&mut s);
    // The first header (Appearance) is at row index 0.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        5,
        0,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
}

#[test]
fn mouse_click_outside_list_is_a_no_op() {
    let mut s = make_state();
    synth_rects(&mut s);
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        100, // outside list_area (width=80)
        0,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
}

#[test]
fn mouse_scroll_down_advances_selection() {
    let mut s = make_state();
    synth_rects(&mut s);
    // Click inside list_area — synth_rects sets height = rows.len().
    let outcome = handle_settings_mouse(&mut s, MouseEventKind::ScrollDown, 5, 1);
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    // Scroll-down emits 3 advances. From the initial selection
    // (compact_mode), this lands 3 settings later. Later changes add more
    // settings between compact_mode and the end, so we compute the
    // expected landing row by inspecting the row list — keeps the
    // test stable across such additions.
    let setting_keys: Vec<&str> = s
        .rows
        .iter()
        .filter_map(|r| match r {
            RowEntry::Setting { key, .. } => Some(*key),
            _ => None,
        })
        .collect();
    let compact_pos = setting_keys
        .iter()
        .position(|k| *k == "compact_mode")
        .unwrap();
    let expected_key = setting_keys
        .get(compact_pos + 3)
        // If there are fewer than 3 settings after compact_mode, the
        // last setting absorbs all extra advances.
        .copied()
        .unwrap_or(*setting_keys.last().unwrap());
    match &s.rows[s.selected] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, expected_key),
        _ => panic!("expected setting row after scroll"),
    }
}

#[test]
fn mouse_scroll_up_returns_selection_to_first() {
    let mut s = make_state();
    synth_rects(&mut s);
    // First advance to the bottom.
    let _ = handle_settings_mouse(&mut s, MouseEventKind::ScrollDown, 5, 1);
    // Then scroll back up.
    let outcome = handle_settings_mouse(&mut s, MouseEventKind::ScrollUp, 5, 1);
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    match &s.rows[s.selected] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, "compact_mode"),
        _ => panic!("expected setting row at top"),
    }
}

// ---------------------------------------------------------------------------
// Filter mode
// ---------------------------------------------------------------------------

/// Filter mode accepts chars into the query editor and must never leak
/// an `Action`.
#[test]
fn slash_enters_filter_mode_and_chars_go_to_query_no_action_leak() {
    let mut s = make_state();
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(matches!(s.mode(), SettingsModalMode::FilterFocused));

    for c in "compact density".chars() {
        let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
        match outcome {
            SettingsKeyOutcome::Changed | SettingsKeyOutcome::Unchanged => {}
            SettingsKeyOutcome::Action(a) => {
                panic!("filter mode leaked Action({a:?}) for char {c:?}");
            }
            SettingsKeyOutcome::ActionPair(a, b) => {
                panic!("filter mode leaked ActionPair({a:?}, {b:?}) for char {c:?}");
            }
            SettingsKeyOutcome::ActionThenClose(a) => {
                panic!("filter mode leaked ActionThenClose({a:?}) for char {c:?}");
            }
            SettingsKeyOutcome::Close => {
                panic!("filter mode unexpectedly closed on char {c:?}");
            }
        }
    }
    assert_eq!(s.query(), "compact density");

    let reg = SettingsRegistry::defaults();
    let hits = reg.search(s.query());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key, "compact_mode");
}

/// `Ctrl+,` closes the modal from FilterFocused mode.
#[test]
fn ctrl_comma_from_filter_mode_closes_modal() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    assert!(matches!(s.mode(), SettingsModalMode::FilterFocused));
    let outcome = handle_settings_key(
        &mut s,
        &press_with(KeyCode::Char(','), KeyModifiers::CONTROL),
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Close));
}

/// F2 from FilterFocused mode also closes.
#[test]
fn f2_from_filter_mode_closes_modal() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    assert!(matches!(s.mode(), SettingsModalMode::FilterFocused));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::F(2)));
    assert!(matches!(outcome, SettingsKeyOutcome::Close));
}

/// "stamp" matches only `show_timestamps` — narrows to header + setting.
#[test]
fn filter_query_stamp_matches_show_timestamps_only() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    let filtered = s.filtered_indices();
    assert_eq!(
        filtered.len(),
        2,
        "expected exactly 2 visible rows (Appearance header + show_timestamps), \
         got {filtered:?}"
    );
    // Header must come before its child setting.
    match &s.rows[filtered[0]] {
        RowEntry::Header { category } => assert_eq!(*category, SettingCategory::Appearance),
        other => panic!("expected Header at filtered[0], got {other:?}"),
    }
    match &s.rows[filtered[1]] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, "show_timestamps"),
        other => panic!("expected Setting at filtered[1], got {other:?}"),
    }
}

/// Non-matching query produces an empty filtered set.
#[test]
fn filter_query_nonexistent_shows_zero_settings() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "xyzzy-no-match".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    assert!(
        s.filtered_indices().is_empty(),
        "nonexistent query should produce empty filtered set, got {:?}",
        s.filtered_indices(),
    );
}

/// Empty query shows all rows.
#[test]
fn filter_empty_query_shows_all_rows() {
    let s = make_state();
    let filtered = s.filtered_indices();
    let expected: Vec<usize> = (0..s.rows.len()).collect();
    assert_eq!(
        filtered, expected,
        "empty-query filtered set must equal (0..rows.len())"
    );
}

/// Esc in filter mode clears the query and returns to Browse.
#[test]
fn filter_esc_clears_query_and_returns_to_browse() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    assert_eq!(s.query(), "stamp");
    assert_eq!(s.filtered_indices().len(), 2);

    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
    assert_eq!(s.query(), "", "Esc must clear the query");
    // Filter is inert again — full set restored in original order.
    let expected: Vec<usize> = (0..s.rows.len()).collect();
    assert_eq!(
        s.filtered_indices(),
        expected.as_slice(),
        "Esc must restore filtered_indices to (0..rows.len()) — full set in row-order"
    );
}

/// Navigation under filter walks only the filtered subset.
/// Up must not land on a header.
#[test]
fn filter_navigation_lands_on_filtered_subset_only() {
    let mut s = make_state();
    // Sanity check: initial selection is compact_mode (row 1).
    let compact_idx = row_idx_for(&s, "compact_mode");
    let show_ts_idx = row_idx_for(&s, "show_timestamps");
    assert_eq!(s.selected, compact_idx);

    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    // After filtering for "stamp", compact_mode is hidden — selection
    // snaps to the only remaining setting (show_timestamps).
    assert_eq!(
        s.selected, show_ts_idx,
        "selection should snap to show_timestamps when compact_mode is filtered out"
    );

    // Down arrow shouldn't move (only one setting in the filter).
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Down));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Down at last visible setting should be Unchanged, got {outcome:?}"
    );
    assert_eq!(s.selected, show_ts_idx);

    // Up arrow shouldn't move either (only one setting in the filter,
    // and headers are not selectable).
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Up));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Up at first visible setting should be Unchanged, got {outcome:?}"
    );
    assert_eq!(s.selected, show_ts_idx);

    // Up must not land on a header.
    let appearance_header_idx = s
        .rows
        .iter()
        .position(|r| {
            matches!(
                r,
                RowEntry::Header {
                    category: SettingCategory::Appearance
                }
            )
        })
        .expect("Appearance header must exist in default registry");
    assert_ne!(
        s.selected, appearance_header_idx,
        "Up arrow must NOT land on a header (regression: header became selectable)"
    );
}

/// Backspace pops a char and re-broadens the visible set.
#[test]
fn filter_backspace_broadens_visible_set() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    let narrowed = s.filtered_indices().len();
    assert_eq!(
        narrowed, 2,
        "expected narrowed filter (header + show_timestamps)"
    );

    // Pop trailing 'p' → "stam". Still matches only show_timestamps
    // (substring of "timestamps"); same 2 visible rows.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Backspace));
    assert_eq!(s.query(), "stam");
    assert_eq!(s.filtered_indices().len(), 2);

    // Empty the query out — now everything is visible. We pop one at
    // a time and check at each step that the cache regenerates
    // (rather than just shrinks).
    let _ = handle_settings_key(&mut s, &press(KeyCode::Backspace)); // → "sta"
    assert_eq!(s.query(), "sta");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Backspace)); // → "st"
    assert_eq!(s.query(), "st");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Backspace)); // → "s"
    assert_eq!(s.query(), "s");
    // "s" matches multiple settings (compact_mode via "messages" in
    // its description, show_timestamps via "show"/"timestamps",
    // simple_mode via "simple"/"ascii"). So the filtered set MUST be
    // larger than 2 here — proving each Backspace re-broadens.
    let setting_count_at_s = s
        .filtered_indices()
        .iter()
        .filter(|&&i| matches!(s.rows[i], RowEntry::Setting { .. }))
        .count();
    assert!(
        setting_count_at_s >= 2,
        "popping query from 'stam' to 's' must re-broaden — \
         expected >=2 visible settings, got {setting_count_at_s} \
         (filtered = {:?})",
        s.filtered_indices()
    );

    // Final pop → "". Filter inert, full set restored in order.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Backspace));
    assert_eq!(s.query(), "");
    let expected: Vec<usize> = (0..s.rows.len()).collect();
    assert_eq!(
        s.filtered_indices(),
        expected.as_slice(),
        "empty query must re-broaden to (0..rows.len()) in row order",
    );
}

#[test]
fn programmatic_filter_query_is_single_line_and_cursor_ends() {
    let mut state = make_state();
    state.set_query("sta\r\nmp\n");
    assert_eq!(state.query(), "stamp");
    assert_eq!(state.query_cursor(), state.query().len());
    assert_eq!(state.filtered_indices().len(), 2);
}

/// Multi-keyword AND query narrows correctly.
#[test]
fn filter_with_multiple_matches_navigates_between_settings() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    // "compact" is a keyword on compact_mode; "simple" on simple_mode.
    // Use an OR-of-substrings approach: the query "mode" alone matches
    // too many things now (theme descriptions contain "mode"). We
    // instead use two distinct keyword matches separated by a single
    // word that's not in the theme catalog — testing the multi-word
    // AND behavior on a tight set.
    for c in "ascii minimal".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    // Both keywords are on simple_mode → exactly one setting matches.
    let filtered = s.filtered_indices();
    let setting_keys: Vec<&str> = filtered
        .iter()
        .filter_map(|&i| match &s.rows[i] {
            RowEntry::Setting { key, .. } => Some(*key),
            _ => None,
        })
        .collect();
    assert_eq!(setting_keys, vec!["simple_mode"]);

    // Selection should snap to the only visible setting.
    let simple_idx = row_idx_for(&s, "simple_mode");
    assert_eq!(s.selected, simple_idx);

    // Down at the only visible setting is Unchanged.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Down));
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
    assert_eq!(s.selected, simple_idx);

    // Up at the only visible setting is Unchanged (headers are not
    // selectable).
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Up));
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
    assert_eq!(s.selected, simple_idx);

    // Drop the second keyword (Backspace x8 to remove "minimal" — 7
    // chars + 1 space). Now "ascii" alone still matches only
    // simple_mode but we're back to a single-keyword filter that
    // doesn't ambiguously broaden. Asserts the "filter still narrows
    // correctly when one keyword drops out" property.
    for _ in 0..8 {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Backspace));
    }
    assert_eq!(s.query(), "ascii");
    let filtered_after_pop = s.filtered_indices();
    let after_pop_keys: Vec<&str> = filtered_after_pop
        .iter()
        .filter_map(|&i| match &s.rows[i] {
            RowEntry::Setting { key, .. } => Some(*key),
            _ => None,
        })
        .collect();
    assert_eq!(after_pop_keys, vec!["simple_mode"]);
}

// ---------------------------------------------------------------------------
// Filter mode — Enter commits with preserved query
// ---------------------------------------------------------------------------

/// Enter in FilterFocused exits filter focus and preserves the query.
#[test]
fn filter_enter_commits_and_preserves_query() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    let show_ts_idx = row_idx_for(&s, "show_timestamps");
    assert!(matches!(s.mode(), SettingsModalMode::FilterFocused));
    assert_eq!(s.query(), "stamp");
    assert_eq!(s.selected, show_ts_idx);

    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter must exit FilterFocused"
    );
    assert_eq!(
        s.query(),
        "stamp",
        "Enter must PRESERVE the query (Issue 1)"
    );
    assert_eq!(
        s.filtered_indices().len(),
        2,
        "filter must remain active after Enter-commit"
    );
    assert_eq!(s.selected, show_ts_idx);

    // And now the user can toggle directly without re-navigating.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "show_timestamps", false);
}

/// Backspace in Browse continues editing the preserved query.
#[test]
fn browse_backspace_pops_query_after_filter_commit() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    let _ = handle_settings_key(&mut s, &press(KeyCode::Home));
    assert_eq!(s.query_cursor(), 0);
    // Commit
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
    assert_eq!(s.query(), "stamp");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Left));
    assert_eq!(
        s.query(),
        "stamp",
        "unfocused Browse navigation must not edit the query",
    );

    // Backspace in Browse pops one char, stays in Browse, re-runs
    // invalidate_filter.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Backspace));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
    assert_eq!(s.query(), "stam");
    assert_eq!(s.filtered_indices().len(), 2);

    let grapheme = "👩🏽\u{200d}💻";
    for modifiers in [
        KeyModifiers::ALT,
        KeyModifiers::CONTROL,
        KeyModifiers::SUPER,
        KeyModifiers::SHIFT,
    ] {
        s.set_query(format!("x{grapheme}"));
        let outcome = handle_settings_key(&mut s, &press_with(KeyCode::Backspace, modifiers));
        assert!(matches!(outcome, SettingsKeyOutcome::Changed));
        assert_eq!(
            s.query(),
            "x",
            "{modifiers:?}+Backspace must remove exactly one trailing grapheme",
        );
        assert!(matches!(s.mode(), SettingsModalMode::Browse));
    }

    // Backspace on empty query is Unchanged (and the query stays "").
    s.set_query("");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Backspace));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Backspace on empty query must be Unchanged"
    );
}

#[test]
fn filter_uses_canonical_word_keys_without_cursor_side_effects() {
    let mut state = make_state();
    let _ = handle_settings_key(&mut state, &press(KeyCode::Char('/')));

    for key in [
        press_with(KeyCode::Left, KeyModifiers::ALT),
        press_with(KeyCode::Char('b'), KeyModifiers::ALT),
        press_with(KeyCode::Left, KeyModifiers::CONTROL),
    ] {
        state.set_query("alpha-beta");
        let outcome = handle_settings_key(&mut state, &key);
        assert!(matches!(outcome, SettingsKeyOutcome::Changed));
        assert_eq!(state.query(), "alpha-beta");
        assert_eq!(state.query_cursor(), "alpha-".len());
    }
    for key in [
        press_with(KeyCode::Right, KeyModifiers::ALT),
        press_with(KeyCode::Char('f'), KeyModifiers::ALT),
    ] {
        state.set_query("alpha-beta");
        let _ = handle_settings_key(&mut state, &press(KeyCode::Home));
        let outcome = handle_settings_key(&mut state, &key);
        assert!(matches!(outcome, SettingsKeyOutcome::Changed));
        assert_eq!(state.query_cursor(), "alpha".len());
    }

    state.set_query("stamp");
    let compact_idx = row_idx_for(&state, "compact_mode");
    let show_timestamps_idx = row_idx_for(&state, "show_timestamps");
    let filtered_before = state.filtered_indices().to_vec();
    state.selected = compact_idx;
    let _ = handle_settings_key(&mut state, &press_with(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(state.filtered_indices(), filtered_before.as_slice());
    assert_eq!(
        state.selected, compact_idx,
        "cursor-only motion must not clamp selection",
    );
    let _ = handle_settings_key(&mut state, &press(KeyCode::End));
    let _ = handle_settings_key(&mut state, &press(KeyCode::Backspace));
    assert_eq!(state.query(), "stam");
    assert_eq!(
        state.selected, show_timestamps_idx,
        "text mutation must recompute filtering and clamp selection",
    );

    state.set_query("alpha-beta");
    let _ = handle_settings_key(
        &mut state,
        &press_with(KeyCode::Backspace, KeyModifiers::ALT),
    );
    assert_eq!(state.query(), "alpha-");
}

#[test]
fn filter_grapheme_and_line_commands_are_canonical() {
    let mut state = make_state();
    let _ = handle_settings_key(&mut state, &press(KeyCode::Char('/')));
    let grapheme = "👩🏽\u{200d}💻";
    state.set_query(format!("a{grapheme}b"));

    let _ = handle_settings_key(&mut state, &press(KeyCode::Home));
    assert_eq!(state.query_cursor(), 0);
    let _ = handle_settings_key(&mut state, &press(KeyCode::Right));
    assert_eq!(state.query_cursor(), 1);
    let _ = handle_settings_key(&mut state, &press(KeyCode::Delete));
    assert_eq!(state.query(), "ab");
    let _ = handle_settings_key(&mut state, &press(KeyCode::End));
    assert_eq!(state.query_cursor(), state.query().len());
    let _ = handle_settings_key(&mut state, &press_with(KeyCode::Left, KeyModifiers::SUPER));
    assert_eq!(state.query_cursor(), 0);
    let _ = handle_settings_key(&mut state, &press_with(KeyCode::Right, KeyModifiers::SUPER));
    assert_eq!(state.query_cursor(), state.query().len());
}

#[test]
fn filter_ctrl_kill_keys_and_unsafe_insert_policy() {
    let mut state = make_state();
    let _ = handle_settings_key(&mut state, &press(KeyCode::Char('/')));

    state.set_query("alpha beta");
    let _ = handle_settings_key(
        &mut state,
        &press_with(KeyCode::Char('w'), KeyModifiers::CONTROL),
    );
    assert_eq!(state.query(), "alpha ");

    state.set_query("alpha beta");
    let _ = handle_settings_key(
        &mut state,
        &press_with(KeyCode::Char('u'), KeyModifiers::CONTROL),
    );
    assert!(state.query().is_empty());

    state.set_query("alpha beta");
    let _ = handle_settings_key(&mut state, &press(KeyCode::Home));
    let _ = handle_settings_key(
        &mut state,
        &press_with(KeyCode::Char('k'), KeyModifiers::CONTROL),
    );
    assert!(state.query().is_empty());

    let outcome = handle_settings_key(&mut state, &press(KeyCode::Char('\u{202e}')));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(
        state.query().is_empty(),
        "unsafe display characters must be consumed without insertion",
    );
}

/// PageDown/PageUp navigate in filter mode (advance x10 per press).
#[test]
fn filter_pageup_pagedown_navigates_in_filter_mode() {
    let mut s = make_state();
    // Enter filter mode without typing — filtered_cache stays full.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    assert!(matches!(s.mode(), SettingsModalMode::FilterFocused));

    // PageDown from compact_mode advances toward the last row.
    let compact_idx = row_idx_for(&s, "compact_mode");
    let last_idx = s
        .rows
        .iter()
        .rposition(|r| matches!(r, RowEntry::Setting { .. }))
        .expect("default registry must contain at least one setting");
    assert_eq!(s.selected, compact_idx);

    // Compute how many PageDown presses are needed to reach the
    // last row (each press advances by 10 selectable rows; round
    // up so we definitely reach it). A single press isn't enough
    // once the registry exceeds ~10 settings.
    let total_selectable = s
        .rows
        .iter()
        .filter(|r| matches!(r, RowEntry::Setting { .. }))
        .count();
    let presses = total_selectable.div_ceil(10).max(1);
    for _ in 0..presses {
        let _ = handle_settings_key(&mut s, &press(KeyCode::PageDown));
    }
    assert_eq!(
        s.selected, last_idx,
        "PageDown × {presses} in filter must reach last selectable row",
    );

    // PageUp returns to the top.
    for _ in 0..presses {
        let _ = handle_settings_key(&mut s, &press(KeyCode::PageUp));
    }
    assert_eq!(s.selected, compact_idx);
}

// ---------------------------------------------------------------------------
// Filter mode — g/G filter-aware navigation
// ---------------------------------------------------------------------------

/// `g` lands on the first selectable row (not a header).
#[test]
fn g_jumps_to_first_visible_setting() {
    let mut s = make_state();
    // Move to the last row first so g actually has to navigate.
    navigate_to(&mut s, "simple_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char('g')));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    let first_setting_idx = row_idx_for(&s, "compact_mode");
    assert_eq!(
        s.selected, first_setting_idx,
        "`g` must land on first selectable row (compact_mode), not a header"
    );
}

/// `G` lands on the last selectable row.
#[test]
fn shift_g_jumps_to_last_visible_setting() {
    let mut s = make_state();
    // Default selection is compact_mode (first setting).
    let outcome = handle_settings_key(&mut s, &press_with(KeyCode::Char('G'), KeyModifiers::SHIFT));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    // Compute the last setting row in row-order at runtime.
    let last_setting_idx = s
        .rows
        .iter()
        .rposition(|r| matches!(r, RowEntry::Setting { .. }))
        .expect("default registry must contain at least one setting");
    assert_eq!(
        s.selected, last_setting_idx,
        "`G` must land on last selectable row in the registry"
    );
}

/// `g` respects the active filter — lands on the first visible setting.
#[test]
fn g_jumps_to_first_filtered_row_under_active_filter() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    // Commit filter into Browse with query preserved.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));

    // Selection is already on show_timestamps (snapped by
    // clamp_selected_to_visible). g should be a no-op (Unchanged)
    // because the first visible setting IS the current selection.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char('g')));
    let show_ts_idx = row_idx_for(&s, "show_timestamps");
    assert_eq!(s.selected, show_ts_idx);
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "g on the already-first-visible row should be Unchanged"
    );
}

/// `G` respects the active filter — lands on the last visible setting.
#[test]
fn shift_g_jumps_to_last_filtered_row_under_active_filter() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));

    let outcome = handle_settings_key(&mut s, &press_with(KeyCode::Char('G'), KeyModifiers::SHIFT));
    let show_ts_idx = row_idx_for(&s, "show_timestamps");
    assert_eq!(s.selected, show_ts_idx);
    // G must NOT land on compact_mode or simple_mode (both hidden).
    let compact_idx = row_idx_for(&s, "compact_mode");
    let simple_idx = row_idx_for(&s, "simple_mode");
    assert_ne!(s.selected, compact_idx, "G must not land on hidden row");
    assert_ne!(s.selected, simple_idx, "G must not land on hidden row");
    // Outcome is Unchanged because show_timestamps is already last.
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
}

// ---------------------------------------------------------------------------
// Selection stability
// ---------------------------------------------------------------------------

/// Filter keeps selection when the focused row remains visible.
#[test]
fn filter_keeps_selection_when_currently_selected_row_remains_visible() {
    let mut s = make_state();
    navigate_to(&mut s, "show_timestamps");
    let show_ts_idx = s.selected;

    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    assert_eq!(
        s.selected, show_ts_idx,
        "selection should NOT move — show_timestamps was already focused and matches the filter"
    );
}

// ---------------------------------------------------------------------------
// Multi-word AND semantics
// ---------------------------------------------------------------------------

/// One unmatched word in a multi-word query empties the result (AND).
#[test]
fn filter_multi_word_with_one_unmatched_word_shows_zero_settings() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    // "compact" alone matches compact_mode; appending an unmatched
    // word forces AND→empty.
    for c in "compact xyzzy".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    assert!(
        s.filtered_indices().is_empty(),
        "AND across words: one unmatched word should empty the result, got {:?}",
        s.filtered_indices()
    );
}

/// AND narrows strictly: adding an unmatched word yields 0 results.
#[test]
fn filter_and_semantics_narrow_strictly() {
    let reg = SettingsRegistry::defaults();
    // "ascii" matches only simple_mode (keyword).
    let single = reg.search("ascii");
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].key, "simple_mode");
    // "ascii minimal" — both simple_mode keywords. Still 1 match.
    let conjunction = reg.search("ascii minimal");
    assert_eq!(conjunction.len(), 1);
    assert_eq!(conjunction[0].key, "simple_mode");
    // Adding an unmatched word must yield 0.
    let with_unmatched = reg.search("ascii xyzzy");
    assert!(
        with_unmatched.is_empty(),
        "AND with unmatched word must yield empty, got {:?}",
        with_unmatched.iter().map(|m| m.key).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Mouse parity under active filter
// ---------------------------------------------------------------------------

/// Lay out `row_rects` for filtered state — only visible rows get rects.
fn synth_rects_filtered(state: &mut SettingsModalState) {
    let filter: Vec<usize> = state.filtered_indices().to_vec();
    state.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: filter.len() as u16,
    };
    state.row_rects = vec![Rect::default(); state.rows.len()];
    state.value_hit_rects = vec![Rect::default(); state.rows.len()];
    for (pos, &row_idx) in filter.iter().enumerate() {
        state.row_rects[row_idx] = Rect {
            x: 0,
            y: pos as u16,
            width: 80,
            height: 1,
        };
        state.value_hit_rects[row_idx] = Rect {
            x: 70,
            y: pos as u16,
            width: 8,
            height: 1,
        };
    }
}

/// Mouse click on a visible filtered row toggles it.
#[test]
fn mouse_click_on_visible_filtered_row_toggles() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    // Commit so we can click without typing.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    synth_rects_filtered(&mut s);
    // show_timestamps is selected (snapped). filtered layout:
    // pos 0 = Appearance header, pos 1 = show_timestamps.
    let y_in_filter = 1u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        y_in_filter,
    );
    assert_set_bool_action(outcome, "show_timestamps", false);
}

/// Click at a filtered-out row position is a no-op.
#[test]
fn mouse_click_at_filtered_out_row_position_is_no_op() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    synth_rects_filtered(&mut s);
    // y=2 would be compact_mode in an unfiltered layout but is outside
    // the filtered list_area (height=2). Click should be Unchanged.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        2,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "click outside filtered list_area must be Unchanged, got {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// Filter-rebuild timing — cache stability
// ---------------------------------------------------------------------------

/// `filtered_indices()` is a stable borrow, not regenerated per call.
#[test]
fn filter_cache_pointer_is_stable_across_reads() {
    let s = make_state();
    let ptr1 = s.filtered_indices().as_ptr();
    let ptr2 = s.filtered_indices().as_ptr();
    assert_eq!(
        ptr1, ptr2,
        "filtered_indices() should be a stable borrow, not a fresh allocation per read"
    );
}

/// `filtered_cache` is regenerated when the query mutates.
#[test]
fn filter_cache_pointer_changes_on_query_mutation() {
    let mut s = make_state();
    let ptr_before = s.filtered_indices().as_ptr();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    // Filter mode entry alone doesn't mutate query. Type a char to
    // force `invalidate_filter`.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('s')));
    let ptr_after = s.filtered_indices().as_ptr();
    assert_ne!(
        ptr_before, ptr_after,
        "filtered_cache must be regenerated on query mutation"
    );
}

// ---------------------------------------------------------------------------
// Render with filter — translation + "No matches"
// ---------------------------------------------------------------------------

/// `scroll_offset` stays within `filtered_indices()` bounds under filter.
#[test]
fn render_with_filter_active_and_small_viewport_clamps_scroll() {
    use ratatui::buffer::Buffer;
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "stamp".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    echo_agent_pager::views::settings_modal::render_settings_modal(
        &mut buf, area, &mut s, false, None,
    );
    let visible = s.filtered_indices().len();
    assert!(
        s.scroll_offset <= visible.saturating_sub(1).max(0),
        "scroll_offset ({}) must be within filtered_indices bounds ({})",
        s.scroll_offset,
        visible
    );
}

/// "No matches" placeholder renders and echoes the query.
#[test]
fn render_no_matches_placeholder_includes_query() {
    use ratatui::buffer::Buffer;
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "xyzzy".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    assert!(s.filtered_indices().is_empty());
    // Larger area than the modal-min so the chrome + search bar +
    // footer all fit and the empty-state placeholder lands in the
    // remaining content area.
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    echo_agent_pager::views::settings_modal::render_settings_modal(
        &mut buf, area, &mut s, false, None,
    );
    // Scan all cells for the substring "No matches" and "xyzzy".
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
        all_text.contains("No matches"),
        "rendered buffer must contain 'No matches' placeholder, got:\n{all_text}"
    );
    assert!(
        all_text.contains("xyzzy"),
        "rendered buffer must echo the query, got:\n{all_text}"
    );
}

// ---------------------------------------------------------------------------
// PickingEnum / EditingValue Esc routing
// ---------------------------------------------------------------------------

/// Esc in `PickingEnum` returns to Browse.
#[test]
fn esc_in_picking_enum_mode_returns_to_browse() {
    let mut s = make_state();
    navigate_to(&mut s, "scroll_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

/// Esc in `EditingValue` returns to Browse.
#[test]
fn esc_in_editing_value_mode_returns_to_browse() {
    let mut s = make_state();
    navigate_to(&mut s, "max_thoughts_width");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

// ---------------------------------------------------------------------------
// Registry contracts
// ---------------------------------------------------------------------------

/// Pins which keys belong to each SettingKind.
#[test]
fn registry_kind_membership_through_pr_14() {
    let reg = SettingsRegistry::defaults();
    let mut by_kind: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for meta in reg.all() {
        let kind_tag = match &meta.kind {
            SettingKind::Bool { .. } => "Bool",
            SettingKind::String { .. } => "String",
            SettingKind::Enum { .. } => "Enum",
            SettingKind::Int { .. } => "Int",

            SettingKind::DynamicEnum { .. } => "DynamicEnum",
            SettingKind::Group { .. } => "Group",
            other => panic!(
                "registry_kind_membership: setting `{}` has unknown kind {:?} — \
                 add an arm here AND a kind-membership assertion below",
                meta.key, other,
            ),
        };
        by_kind.entry(kind_tag).or_default().push(meta.key);
    }
    for keys in by_kind.values_mut() {
        keys.sort();
    }

    let bool_keys = by_kind.remove("Bool").unwrap_or_default();
    assert_eq!(
        bool_keys,
        vec![
            "compact_mode",
            "group_tool_verbs",
            "collapsed_edit_blocks",
            "invert_scroll",
            "display_refresh_auto_cadence",
            "multiline_mode",
            "prompt_suggestions",
            "respect_manual_folds",
            "show_thinking_blocks",
            "show_timeline",
            "show_timestamps",
            "page_flip_on_send",
            "confirm_before_rewind",
            "combine_queued_prompts",
            "simple_mode",
            "vim_mode",
            "remember_tool_approvals",
            "toolset.ask_user_question.timeout_enabled",
            "auto_update",
            "show_tips",
            "voice_keybind_enabled",
            // Per-tip contextual-hint children (hidden from the top-level list,
            // toggled inside the group sub-sheet) are still Bool settings.
            "contextual_hints.undo",
            "contextual_hints.plan_mode",
            "contextual_hints.image_input",
            "contextual_hints.send_now",
            "contextual_hints.small_screen",
            "contextual_hints.word_select",
            "contextual_hints.ssh_wrap",
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>(),
        "Bool kind membership drift",
    );

    let enum_keys = by_kind.remove("Enum").unwrap_or_default();
    assert_eq!(
        enum_keys,
        vec![
            "auto_dark_theme",
            "auto_light_theme",
            "coding_data_sharing",
            "default_selected_permission",
            "follow_up_behavior",
            "hunk_tracker_mode",
            "keep_text_selection",
            "permission_mode",
            "plan_mode",
            "render_mermaid",
            "screen_mode",
            "scroll_mode",
            "theme",
            "voice_capture_mode",
            "voice_stt_language",
        ],
        "Enum kind membership drift",
    );

    let string_keys = by_kind.remove("String").unwrap_or_default();
    assert!(
        string_keys.is_empty(),
        "no String-kind settings should remain — `default_model` + `fork_secondary_model` \
         migrated to DynamicEnum; got: {string_keys:?}",
    );

    let dynamic_enum_keys = by_kind.remove("DynamicEnum").unwrap_or_default();
    assert_eq!(
        dynamic_enum_keys,
        vec!["default_model", "fork_secondary_model",],
        "DynamicEnum kind membership drift",
    );

    let int_keys = by_kind.remove("Int").unwrap_or_default();
    let mut sorted_int = int_keys.clone();
    sorted_int.sort();
    assert_eq!(
        sorted_int,
        vec!["max_thoughts_width", "scroll_lines", "scroll_speed"],
        "Int kind membership drift (PR 8)",
    );

    let group_keys = by_kind.remove("Group").unwrap_or_default();
    assert_eq!(
        group_keys,
        vec!["contextual_hints"],
        "Group kind membership drift",
    );

    // No unexpected kinds.
    assert!(
        by_kind.is_empty(),
        "registry contains unexpected SettingKind variants: {by_kind:?}",
    );
}

/// Pins the set of Enum-kind settings (sorted, order-independent).
#[test]
fn enum_settings_membership_through_pr_14() {
    let reg = SettingsRegistry::defaults();
    let mut enum_keys: Vec<&str> = reg
        .all()
        .iter()
        .filter(|m| matches!(m.kind, SettingKind::Enum { .. }))
        .map(|m| m.key)
        .collect();
    enum_keys.sort();
    assert_eq!(
        enum_keys,
        vec![
            "auto_dark_theme",
            "auto_light_theme",
            "coding_data_sharing",
            "default_selected_permission",
            "follow_up_behavior",
            "hunk_tracker_mode",
            "keep_text_selection",
            "permission_mode",
            "plan_mode",
            "render_mermaid",
            "screen_mode",
            "scroll_mode",
            "theme",
            "voice_capture_mode",
            "voice_stt_language",
        ],
    );
}

/// `current_value_for` and `default_value_for` must agree at
/// `UiConfig::default()` with independently hard-coded expectations.
#[test]
fn defaults_round_trip_through_registry() {
    use echo_agent_pager::settings::{SettingValue, current_value_for};
    let reg = SettingsRegistry::defaults();
    let ui = UiConfig::default();
    let pager = PagerLocalSnapshot::default();

    // `current_value_for` for these keys reads process-wide caches, not `ui`.
    // Reset to defaults so a sibling test on this worker thread can't leak in.
    echo_agent_pager::appearance::cache::set_keep_text_selection(
        echo_agent_pager::appearance::TextSelection::Flash,
    );
    echo_agent_pager::appearance::cache::set_show_thinking_blocks(true);
    echo_agent_pager::appearance::cache::set_prompt_suggestions(true);
    echo_agent_pager::appearance::cache::set_group_tool_verbs(true);
    echo_agent_pager::appearance::cache::set_page_flip_on_send(true);
    echo_agent_pager::appearance::cache::set_combine_queued_prompts(false);
    echo_agent_pager::appearance::cache::set_follow_up_behavior(
        echo_agent_pager::appearance::FollowUpBehavior::Queue,
    );
    echo_agent_pager::appearance::cache::set_scroll_mode(
        echo_agent_pager::appearance::ScrollMode::Auto,
    );
    echo_agent_pager::appearance::cache::set_invert_scroll(false);
    // 3 = the registry default shown while the profile is in charge.
    echo_agent_pager::appearance::cache::set_scroll_lines(3);

    // Hard-coded per-key expectations (independent of registry).
    let expected = |key: &str| -> SettingValue {
        match key {
            "compact_mode" => SettingValue::Bool(false),
            "screen_mode" => SettingValue::Enum("fullscreen"),
            "show_timestamps" => SettingValue::Bool(true),
            "show_timeline" => SettingValue::Bool(false),
            "page_flip_on_send" => SettingValue::Bool(true),
            "confirm_before_rewind" => SettingValue::Bool(true),
            "combine_queued_prompts" => SettingValue::Bool(false),
            "follow_up_behavior" => SettingValue::Enum("queue"),
            "simple_mode" => SettingValue::Bool(true),
            "vim_mode" => SettingValue::Bool(false),
            "remember_tool_approvals" => SettingValue::Bool(true),
            "toolset.ask_user_question.timeout_enabled" => SettingValue::Bool(true),
            "keep_text_selection" => SettingValue::Enum("flash"),
            "theme" => SettingValue::Enum("echo-agent-night"),
            "auto_dark_theme" => SettingValue::Enum("echo-agent-night"),
            "auto_light_theme" => SettingValue::Enum("echo-agent-day"),
            "render_mermaid" => SettingValue::Enum("auto"),
            "multiline_mode" => SettingValue::Bool(false),
            "permission_mode" => SettingValue::Enum("ask"),
            "default_model" => SettingValue::String(String::new()),
            "max_thoughts_width" => SettingValue::Int(120),
            "scroll_speed" => SettingValue::Int(50),
            "scroll_mode" => SettingValue::Enum("auto"),
            "scroll_lines" => SettingValue::Int(3),
            "invert_scroll" => SettingValue::Bool(false),
            "display_refresh_auto_cadence" => SettingValue::Bool(true),
            "coding_data_sharing" => SettingValue::Enum("opt-out"),
            "default_selected_permission" => SettingValue::Enum("always_allow_all_sessions"),
            "hunk_tracker_mode" => SettingValue::Enum("off"),
            "voice_keybind_enabled" => SettingValue::Bool(true),
            "voice_capture_mode" => SettingValue::Enum("hold"),
            "voice_stt_language" => SettingValue::Enum("en"),
            "plan_mode" => SettingValue::Enum("off"),
            "show_tips" => SettingValue::Bool(true),
            "auto_update" => SettingValue::Bool(true),
            "fork_secondary_model" => SettingValue::String(String::new()),
            "show_thinking_blocks" => SettingValue::Bool(true),
            "prompt_suggestions" => SettingValue::Bool(true),
            "group_tool_verbs" => SettingValue::Bool(true),
            "collapsed_edit_blocks" => SettingValue::Bool(false),
            "respect_manual_folds" => SettingValue::Bool(false),
            // Per-tip contextual-hint children default ON (inherit → true).
            "contextual_hints.undo" => SettingValue::Bool(true),
            "contextual_hints.plan_mode" => SettingValue::Bool(true),
            "contextual_hints.image_input" => SettingValue::Bool(true),
            "contextual_hints.send_now" => SettingValue::Bool(true),
            "contextual_hints.small_screen" => SettingValue::Bool(true),
            "contextual_hints.word_select" => SettingValue::Bool(true),
            "contextual_hints.ssh_wrap" => SettingValue::Bool(true),
            other => panic!("test must list expected default for `{other}`"),
        }
    };

    for meta in reg.all() {
        // Group rows carry no scalar value/default to round-trip.
        if matches!(meta.kind, SettingKind::Group { .. }) {
            continue;
        }
        let live_value = current_value_for(meta.key, &ui, &pager)
            .unwrap_or_else(|| panic!("current_value_for(`{}`) returned None", meta.key));
        let default_value = echo_agent_pager::settings::default_value_for(meta);
        let expected_value = expected(meta.key);

        assert_eq!(
            live_value, expected_value,
            "current_value_for(`{}`) drifted from expected",
            meta.key
        );
        assert_eq!(
            default_value, expected_value,
            "default_value_for(`{}`) drifted from expected",
            meta.key
        );
    }
}

/// Initial modal-open state selects the first setting row.
#[test]
fn initial_state_selects_first_setting_row() {
    let s = make_state();
    match &s.rows[s.selected] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, "compact_mode"),
        _ => panic!("initial selection must land on a setting row, not a header"),
    }
}

#[test]
fn settings_value_payload_matches_kind() {
    // Every Bool setting's typed setter carries a Bool value.
    let reg = SettingsRegistry::defaults();
    for meta in reg.all() {
        if !matches!(meta.kind, SettingKind::Bool { .. }) {
            continue;
        }
        // Group children are toggled inside the sub-sheet, not as top-level
        // rows (covered by the group sub-sheet e2e test), so skip navigation.
        if is_group_child(&reg, meta.key) {
            continue;
        }
        let mut state = make_state();
        navigate_to(&mut state, meta.key);
        let outcome = handle_settings_key(&mut state, &press(KeyCode::Char(' ')));
        match outcome {
            SettingsKeyOutcome::Action(Action::SetCompactMode(_))
            | SettingsKeyOutcome::Action(Action::SetTimestamps(_))
            | SettingsKeyOutcome::Action(Action::SetTimeline(_))
            | SettingsKeyOutcome::Action(Action::SetPageFlipOnSend(_))
            | SettingsKeyOutcome::Action(Action::SetConfirmBeforeRewind(_))
            | SettingsKeyOutcome::Action(Action::SetCombineQueuedPrompts(_))
            | SettingsKeyOutcome::Action(Action::SetSimpleMode(_))
            | SettingsKeyOutcome::Action(Action::SetMultilineMode(_))
            | SettingsKeyOutcome::Action(Action::SetVimMode(_))
            | SettingsKeyOutcome::Action(Action::SetRememberToolApprovals(_))
            | SettingsKeyOutcome::Action(Action::SetAskUserQuestionTimeoutEnabled(_))
            | SettingsKeyOutcome::Action(Action::SetShowTips(_))
            | SettingsKeyOutcome::Action(Action::SetAutoUpdate(_))
            | SettingsKeyOutcome::Action(Action::SetRespectManualFolds(_))
            | SettingsKeyOutcome::Action(Action::SetShowThinkingBlocks(_))
            | SettingsKeyOutcome::Action(Action::SetPromptSuggestions(_))
            | SettingsKeyOutcome::Action(Action::SetGroupToolVerbs(_))
            | SettingsKeyOutcome::Action(Action::SetCollapsedEditBlocks(_))
            | SettingsKeyOutcome::Action(Action::SetInvertScroll(_))
            | SettingsKeyOutcome::Action(Action::SetDisplayRefreshAutoCadence(_))
            | SettingsKeyOutcome::Action(Action::SetVoiceKeybindEnabled(_)) => {}
            other => panic!(
                "expected a typed bool setter for `{}`, got {:?}",
                meta.key, other
            ),
        }
    }
}

/// `SettingValue` variants with same payload must not compare equal.
#[test]
fn setting_value_variants_are_distinct() {
    let b = SettingValue::Bool(true);
    let e = SettingValue::Enum("true");
    let s = SettingValue::String("true".to_string());
    let i = SettingValue::Int(1);
    assert_ne!(b, e);
    assert_ne!(b, s);
    assert_ne!(b, i);
    assert_ne!(e, s);
    assert_ne!(e, i);
    assert_ne!(s, i);
}

// ---------------------------------------------------------------------------
// KeyEventKind filtering
// ---------------------------------------------------------------------------

/// Release events are dropped (kitty-keyboard protocol parity).
#[test]
fn release_event_kind_is_dropped() {
    let mut s = make_state();
    let key = KeyEvent {
        code: KeyCode::Char(' '),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: crossterm::event::KeyEventState::NONE,
    };
    let outcome = handle_settings_key(&mut s, &key);
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Release event must produce Unchanged, got: {outcome:?}"
    );
}

/// Repeat Space is dropped (prevents disk thrash from held key).
#[test]
fn repeat_space_is_dropped_no_disk_thrash() {
    let mut s = make_state();
    let key = KeyEvent {
        code: KeyCode::Char(' '),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Repeat,
        state: crossterm::event::KeyEventState::NONE,
    };
    let outcome = handle_settings_key(&mut s, &key);
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Repeat on Space must not dispatch an Action, got: {outcome:?}"
    );
}

#[test]
fn repeat_enter_is_dropped() {
    let mut s = make_state();
    let key = KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Repeat,
        state: crossterm::event::KeyEventState::NONE,
    };
    let outcome = handle_settings_key(&mut s, &key);
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
}

/// Repeat on j (navigation) is allowed — held arrow keys feel snappy.
#[test]
fn repeat_j_navigation_is_processed() {
    let mut s = make_state();
    let key = KeyEvent {
        code: KeyCode::Char('j'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Repeat,
        state: crossterm::event::KeyEventState::NONE,
    };
    let outcome = handle_settings_key(&mut s, &key);
    // From the initial state (compact_mode), Repeat j advances to the next
    // Appearance row: screen_mode.
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    match &s.rows[s.selected] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, "screen_mode"),
        _ => panic!("expected setting row after Repeat j"),
    }
}

// ---------------------------------------------------------------------------
// d-key reset-to-default
// ---------------------------------------------------------------------------

/// `d` dispatches `Action::OpenResetConfirm` on the focused row.
#[test]
fn d_key_emits_open_reset_confirm_action_for_compact_mode() {
    let mut s = make_state();
    // Default selection lands on `compact_mode`.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char('d')));
    match outcome {
        SettingsKeyOutcome::Action(Action::OpenResetConfirm { key }) => {
            assert_eq!(key, "compact_mode");
        }
        other => panic!("expected Action::OpenResetConfirm, got {other:?}"),
    }
}

/// `d` fires `OpenResetConfirm` with the right key for every setting.
#[test]
fn d_key_emits_open_reset_confirm_for_every_setting() {
    let reg = SettingsRegistry::defaults();
    for meta in reg.all() {
        // Group rows have no scalar value to reset (consistent with the registry
        // reset-arm coverage test), and their children are hidden from the
        // top-level list — neither is `d`-resettable directly.
        if matches!(meta.kind, SettingKind::Group { .. }) || is_group_child(&reg, meta.key) {
            continue;
        }
        let mut s = make_state();
        // Some rows are terminal-gated (e.g. `voice_capture_mode` is hidden
        // without key-release reporting, which tests run without). Skip settings
        // with no visible row; their reset path is covered by the dispatch
        // round-trip tests.
        let has_row = s
            .rows
            .iter()
            .any(|r| matches!(r, RowEntry::Setting { key, .. } if *key == meta.key));
        if !has_row {
            continue;
        }
        navigate_to(&mut s, meta.key);
        let outcome = handle_settings_key(&mut s, &press(KeyCode::Char('d')));
        match outcome {
            SettingsKeyOutcome::Action(Action::OpenResetConfirm { key }) => {
                assert_eq!(
                    key, meta.key,
                    "d on `{}` must target `{}`",
                    meta.key, meta.key
                );
            }
            other => panic!(
                "expected Action::OpenResetConfirm for `{}`, got {other:?}",
                meta.key
            ),
        }
    }
}

/// `d` on a header row is a no-op.
#[test]
fn d_key_on_header_row_is_unchanged() {
    let mut s = make_state();
    let header_idx = s
        .rows
        .iter()
        .position(|r| {
            matches!(
                r,
                RowEntry::Header {
                    category: SettingCategory::Appearance
                }
            )
        })
        .expect("Appearance header must exist");
    s.selected = header_idx;
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char('d')));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "d on header row must be Unchanged, got: {outcome:?}",
    );
}

// ---------------------------------------------------------------------------
// Mouse hit-rect edge cases
// ---------------------------------------------------------------------------

/// Click with empty `row_rects` (partial render) is a no-op.
#[test]
fn mouse_click_inside_list_with_empty_row_rects_is_no_op() {
    let mut s = make_state();
    s.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    // No row_rects populated — simulates a partial render where the
    // chrome drew but row layout was aborted.
    s.row_rects.clear();
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        5,
        5,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
}

/// Scroll-down at last row returns `Unchanged`.
#[test]
fn scroll_down_at_last_row_is_unchanged() {
    let mut s = make_state();
    synth_rects(&mut s);
    // Move to the last selectable row first.
    let last_idx = s
        .rows
        .iter()
        .rposition(|r| matches!(r, RowEntry::Setting { .. }))
        .unwrap();
    s.selected = last_idx;
    let outcome = handle_settings_mouse(&mut s, MouseEventKind::ScrollDown, 5, 1);
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "scroll-down at last row should be Unchanged, got: {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// Stub tests — `#[ignore] + unimplemented!()` until wired up.
// ---------------------------------------------------------------------------

/// Multi-word AND filter narrows to matching settings + section headers.
#[test]
fn pr2_filter_matches_multi_word_and() {
    let mut s = make_state();
    let _ = handle_settings_key(&mut s, &press(KeyCode::Char('/')));
    for c in "compact density".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(c)));
    }
    let filtered = s.filtered_indices();
    assert_eq!(
        filtered.len(),
        2,
        "expected exactly 2 visible rows (Appearance header + compact_mode), \
         got {filtered:?}"
    );
    // First entry: Appearance section header.
    match &s.rows[filtered[0]] {
        RowEntry::Header { category } => assert_eq!(*category, SettingCategory::Appearance),
        other => panic!("expected Header at filtered[0], got {other:?}"),
    }
    // Second entry: compact_mode setting.
    match &s.rows[filtered[1]] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, "compact_mode"),
        other => panic!("expected Setting at filtered[1], got {other:?}"),
    }
}

/// Enum chooser sub-mode: Esc inside `PickingEnum` reverts
/// to the original value AND transitions back to Browse. This e2e
/// exercises the FULL production path:
///
///   Browse (synthetic Enum row focused)
///     → Enter      → try_enter_picking_enum() → PickingEnum
///     → Down       → choices_idx 0 → 1 (preview dispatch)
///     → Esc        → action_for_enum(key, original) → Browse
///
/// Unlike a version that set `state.mode = PickingEnum {...}`
/// directly, this proves the *production* entry path
/// (`handle_browse::Enter` → `try_enter_picking_enum`) — the only
/// place `try_enter_picking_enum` is reachable in production code.
///
/// This verifies the *structural* outcome (mode + Changed); the
/// Action variant assertion lands once
/// `action_for_enum("theme", _)` ships.
#[test]
fn pr3_esc_in_picker_reverts_to_original() {
    // Synthetic Enum registry — `action_for_enum` returns None for
    // this key, so the outcome is structural (Changed + Browse mode)
    // rather than an Action variant assertion.
    let registry = SettingsRegistry::from_entries(vec![SettingMeta {
        key: "test_enum",
        category: SettingCategory::Appearance,
        owner: SettingOwner::Shared,
        label: "Test enum",
        description: "Synthetic Enum entry for PR 3 picker revert path.",
        keywords: &["test"],
        kind: SettingKind::Enum {
            default: "first",
            choices: &[
                EnumChoice {
                    canonical: "first",
                    display: "First",
                    description: "First option.",
                },
                EnumChoice {
                    canonical: "second",
                    display: "Second",
                    description: "Second option.",
                },
            ],
            supports_preview: true,
        },
        restart_required: false,
        hidden_in_minimal: false,
    }]);
    let mut s = SettingsModalState::new(
        Arc::new(registry),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );

    // Sanity: modal starts in Browse on the synthetic Enum row.
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
    match &s.rows[s.selected] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, "test_enum"),
        _ => panic!("initial selection must be the synthetic Enum row"),
    }

    // Step 1: Enter on the Enum row → try_enter_picking_enum() fires
    // and seeds choices_idx + original_value from the row's current
    // value (None → fallback to first canonical).
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
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
            assert_eq!(choices_idx, 0);
            assert_eq!(original_value, &SettingValue::Enum("first"));
        }
        ref other => panic!("expected PickingEnum mode after Enter, got {other:?}"),
    }

    // Step 2: Down → preview-navigate to choice 1 (live preview
    // dispatch via action_for_enum, returns None here → Changed).
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Down));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    match s.mode() {
        SettingsModalMode::PickingEnum { choices_idx, .. } => assert_eq!(choices_idx, 1),
        ref other => panic!("expected PickingEnum mode after Down, got {other:?}"),
    }

    // Step 3: Esc → revert. action_for_enum returns None (no Enum
    // arms yet), so the outcome is Changed. A later change will tighten
    // the assertion to `SettingsKeyOutcome::Action(Action::SetTheme("first"))`
    // once the theme arm ships.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Esc revert outcome should be Changed (or Action when arms exist), got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc must transition the modal back to Browse"
    );
}

/// Full theme picker e2e via the production entry path.
///
///   Browse (theme row focused)
///     → Enter      → try_enter_picking_enum() → PickingEnum
///     → Down       → choices_idx default → next, dispatches
///                    `Action::PreviewTheme(...)` (preview-only,
///                    no persist Effect, no toast)
///     → Up         → preview-revert
///     → Down       → preview to next
///     → Enter      → dispatches `Action::SetTheme(current)` COMMIT
///                    (single persist + toast per picker cycle)
///
/// The test (a) verifies the
/// PREVIEW vs COMMIT split (Up/Down emit Preview Actions, Enter
/// emits a Set/commit Action), and (b) derives expected canonicals
/// from the registry — a future catalog reorder doesn't break the
/// test for a non-bug reason.
#[test]
fn pr4_theme_preview_and_commit_e2e() {
    let reg = SettingsRegistry::defaults();
    let theme_meta = reg
        .find("theme")
        .expect("registry must contain `theme` for PR 4");
    let (default_canonical, default_idx, choices_count, next_canonical, next_idx) =
        match &theme_meta.kind {
            SettingKind::Enum {
                default, choices, ..
            } => {
                let default_idx = choices
                    .iter()
                    .position(|c| c.canonical == *default)
                    .expect("theme default must exist in choices");
                assert!(
                    default_idx + 1 < choices.len(),
                    "test requires at least one choice AFTER the default; reorder?"
                );
                let next = choices[default_idx + 1].canonical;
                (*default, default_idx, choices.len(), next, default_idx + 1)
            }
            other => panic!("expected Enum kind for `theme`, got {other:?}"),
        };
    assert!(
        choices_count >= 3,
        "PR 4 test requires ≥3 theme choices, got {choices_count}",
    );

    let mut s = make_state();
    navigate_to(&mut s, "theme");
    match &s.rows[s.selected] {
        RowEntry::Setting { key, .. } => assert_eq!(*key, "theme"),
        _ => panic!("expected to land on theme row"),
    }

    // Enter on Enum row → PickingEnum, seeded to the default.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on theme row should transition to PickingEnum, got {outcome:?}"
    );
    let original_canonical = match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            choices_idx,
            original_value,
            ..
        } => {
            assert_eq!(*key, "theme");
            // choices_idx points at the registry's default (derived
            // dynamically — no hardcoded "1").
            assert_eq!(*choices_idx, default_idx);
            match original_value {
                SettingValue::Enum(s) => *s,
                other => panic!("expected Enum original_value, got {other:?}"),
            }
        }
        ref other => panic!("expected PickingEnum mode, got {other:?}"),
    };
    assert_eq!(original_canonical, default_canonical);

    // Down → preview-navigate to next choice. The dispatched Action
    // is now a PREVIEW (no persist).
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Down));
    match outcome {
        SettingsKeyOutcome::Action(Action::PreviewTheme(name)) => {
            assert_eq!(
                name, next_canonical,
                "preview dispatch must carry the canonical of the new focused choice",
            );
        }
        other => panic!("expected Action::PreviewTheme(\"{next_canonical}\"), got {other:?}"),
    }
    match s.mode() {
        SettingsModalMode::PickingEnum { choices_idx, .. } => assert_eq!(choices_idx, next_idx),
        ref other => panic!("expected PickingEnum after Down, got {other:?}"),
    }

    // Up → preview-revert to default.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Up));
    match outcome {
        SettingsKeyOutcome::Action(Action::PreviewTheme(name)) => {
            assert_eq!(name, default_canonical);
        }
        other => panic!("expected Action::PreviewTheme(\"{default_canonical}\"), got {other:?}"),
    }

    // Down again so commit lands on a non-default canonical.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));

    // Enter → COMMIT. Dispatches `Action::SetTheme(current_canonical)`
    // — a typed Action variant carrying the current preview value.
    // The dispatcher's `set_theme` emits Effect::PersistSetting +
    // toast (exercised by the strangler-fig e2e test below).
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetTheme(name)) => {
            assert_eq!(
                name, next_canonical,
                "Enter must commit the current preview, not the original"
            );
        }
        other => panic!("expected Action::SetTheme(\"{next_canonical}\") commit, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter commit must return to Browse"
    );
}

// Strangler-fig dispatch-layer tests for the typed Actions are in
// `terminal/echo-agent-pager/src/app/dispatch.rs::tests` (next to
// the `set_compact_mode_emits_persist_setting_with_correct_payload`
// family) — see `set_theme_emits_persist_setting_with_correct_payload`
// and friends. The dispatch tests live there because the `AppView`
// test fixture (`test_app_with_agent`) isn't exported across the
// crate boundary.

/// Esc inside the theme picker dispatches a PREVIEW
/// Action (not a commit) — Esc revert
/// is a preview-style restore, not a re-persist.
#[test]
fn pr4_theme_picker_esc_dispatches_revert_action() {
    let reg = SettingsRegistry::defaults();
    let default_canonical = match &reg.find("theme").unwrap().kind {
        SettingKind::Enum { default, .. } => *default,
        _ => panic!("theme must be Enum"),
    };

    let mut s = make_state();
    navigate_to(&mut s, "theme");

    // Enter PickingEnum.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));

    // Preview-navigate so the original/current distinction is visible.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));

    // Esc → must dispatch PreviewTheme(original) AND return to Browse.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    match outcome {
        SettingsKeyOutcome::Action(Action::PreviewTheme(name)) => {
            assert_eq!(
                name, default_canonical,
                "Esc must carry the original canonical as a PREVIEW (not commit)",
            );
        }
        other => {
            panic!("expected Action::PreviewTheme(\"{default_canonical}\") on Esc, got {other:?}")
        }
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc must return to Browse"
    );
}

/// `action_for_enum` (preview) and
/// `action_for_enum_commit` map every theme-family key to the
/// matching typed Action variant:
/// parameterised across keys AND derives expected next-canonical
/// from the registry (catalog-reorder-resilient).
///
/// Also exercises EVERY choice (not just first Down), so
/// a refactor that routes correctly for choice 0 but breaks for
/// choice N>0 gets caught.
#[test]
fn pr4_picker_dispatches_each_theme_settings_action_variant() {
    let reg = SettingsRegistry::defaults();

    for key in &["theme", "auto_dark_theme", "auto_light_theme"] {
        let meta = reg
            .find(key)
            .unwrap_or_else(|| panic!("registry missing `{key}`"));
        let (default_idx, choices) = match &meta.kind {
            SettingKind::Enum {
                default, choices, ..
            } => {
                let i = choices
                    .iter()
                    .position(|c| c.canonical == *default)
                    .unwrap();
                (i, *choices)
            }
            _ => panic!("`{key}` must be Enum"),
        };

        let mut s = make_state();
        navigate_to(&mut s, key);
        let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));

        // Navigate forward through every remaining choice, asserting
        // the variant on each Preview dispatch.
        for (next_idx, choice) in choices.iter().enumerate().skip(default_idx + 1) {
            let expected = choice.canonical;
            let outcome = handle_settings_key(&mut s, &press(KeyCode::Down));
            match (*key, outcome) {
                ("theme", SettingsKeyOutcome::Action(Action::PreviewTheme(name))) => {
                    assert_eq!(name, expected, "theme preview at idx {next_idx}");
                }
                (
                    "auto_dark_theme",
                    SettingsKeyOutcome::Action(Action::PreviewAutoDarkTheme(name)),
                ) => {
                    assert_eq!(name, expected, "auto_dark preview at idx {next_idx}");
                }
                (
                    "auto_light_theme",
                    SettingsKeyOutcome::Action(Action::PreviewAutoLightTheme(name)),
                ) => {
                    assert_eq!(name, expected, "auto_light preview at idx {next_idx}");
                }
                (k, other) => panic!(
                    "Down on `{k}` at idx {next_idx} should dispatch matching Preview Action, got {other:?}"
                ),
            }
        }

        // Enter at the LAST choice → COMMIT Action variant for that
        // canonical.
        let last_canonical = choices.last().unwrap().canonical;
        let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
        match (*key, outcome) {
            ("theme", SettingsKeyOutcome::Action(Action::SetTheme(name))) => {
                assert_eq!(name, last_canonical);
            }
            ("auto_dark_theme", SettingsKeyOutcome::Action(Action::SetAutoDarkTheme(name))) => {
                assert_eq!(name, last_canonical);
            }
            ("auto_light_theme", SettingsKeyOutcome::Action(Action::SetAutoLightTheme(name))) => {
                assert_eq!(name, last_canonical);
            }
            (k, other) => panic!(
                "Enter on `{k}` last choice should dispatch matching commit Action, got {other:?}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Mouse-path coverage for the new Enum settings.
// The `every_registered_setting_is_exercised`
// test's docstring promises "keyboard test + mouse test" per
// registered key. Earlier only keyboard tests shipped for the 3 new
// enums; these tests close that gap.
// ---------------------------------------------------------------------------

/// Clicking on an Enum row in Browse mode selects it without firing
/// any Action — Enum rows require an explicit Enter to open the
/// picker (mouse picker-entry is deferred to a future change). The body
/// click outside the indicator hit-rect (cols 0-4) is a
/// select-only event.
#[test]
fn pr4_mouse_click_on_theme_row_selects_without_emitting_action() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "theme") as u16;
    // Body click (col 20 is well outside the 0-4 indicator hit-rect).
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        20,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first click on theme row should only select (no Action), got {outcome:?}",
    );
    assert_eq!(
        s.selected, row_y as usize,
        "selection must move to theme row"
    );
}

#[test]
fn pr4_mouse_click_on_auto_dark_theme_row_selects_without_emitting_action() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "auto_dark_theme") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        20,
        row_y,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(s.selected, row_y as usize);
}

#[test]
fn pr4_mouse_click_on_auto_light_theme_row_selects_without_emitting_action() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "auto_light_theme") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        20,
        row_y,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert_eq!(s.selected, row_y as usize);
}

/// In picker mode (PickingEnum), mouse events are no-ops — clicks
/// don't select picker choices yet. Pins the contract so a
/// future change that adds click-to-pick must update this test
/// explicitly.
#[test]
fn pr4_mouse_click_in_theme_picker_is_no_op() {
    let mut s = make_state();
    navigate_to(&mut s, "theme");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    synth_rects(&mut s);
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        20,
        5,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "picker mode must ignore mouse clicks in PR 4, got {outcome:?}",
    );
    assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
}

// ---------------------------------------------------------------------------
// `multiline_mode` (first PAGER-owned setting)
//
// Unlike the SHARED bools (which round-trip through
// `Effect::PersistSetting` and the shell), `multiline_mode` is
// PAGER-owned: state lives on `AgentView.multiline_mode`, the modal
// reads from `PagerLocalSnapshot`, and the dispatcher's
// `set_multiline_mode` is the single mutation owner. No disk persist,
// no `Effect`, no toast on the no-op fast path.
//
// These tests mirror the keyboard + mouse coverage promised by
// `ALL_SETTINGS_EXERCISED` — same rigor as `compact_mode` et al.
// ---------------------------------------------------------------------------

/// Keyboard Space on the multiline row dispatches the typed setter
/// with the inverted snapshot value (default false → true). The modal
/// builds the bool from `PagerLocalSnapshot.multiline_mode` via the
/// `current_value_for` arm.
#[test]
fn pr5_space_on_multiline_mode_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "multiline_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "multiline_mode", true);
}

/// Enter on the multiline row also toggles (same Bool semantics as
/// compact_mode / show_timestamps / simple_mode). Pins the contract
/// that Bool row Enter and Space behave identically across both
/// SHELL/SHARED and PAGER-owned settings.
#[test]
fn pr5_enter_on_multiline_mode_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "multiline_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert_set_bool_action(outcome, "multiline_mode", true);
}

/// Two-stage select-then-toggle for `multiline_mode` mouse path.
#[test]
fn pr5_mouse_click_on_multiline_mode_two_stage_toggles() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "multiline_mode") as u16;

    // First click: select-only (initial focus is on compact_mode).
    // Click at column=10 (outside the indicator hit-rect 0..5).
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first click on a different row body should only select, got: {outcome:?}"
    );
    assert_eq!(
        s.selected, row_y as usize,
        "first click must move selection to multiline_mode row",
    );

    // Second click on the now-focused row: toggle.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_set_bool_action(outcome, "multiline_mode", true);
}

/// Value-column click on `multiline_mode` toggles in one click.
#[test]
fn pr5_mouse_click_on_multiline_indicator_toggles_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "multiline_mode") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert_set_bool_action(outcome, "multiline_mode", true);
}

/// Snapshot `multiline_mode: true` → Space dispatches `SetMultilineMode(false)`.
#[test]
fn pr5_snapshot_when_on_dispatches_off() {
    let snapshot = PagerLocalSnapshot {
        multiline_mode: true,
        yolo_mode: false,
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        snapshot,
    );
    navigate_to(&mut s, "multiline_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "multiline_mode", false);
}

/// `multiline_mode` lives under Editor, not Appearance.
#[test]
fn pr5_multiline_mode_renders_under_editor_category() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("multiline_mode")
        .expect("multiline_mode must be registered");
    assert_eq!(
        meta.category,
        SettingCategory::Editor,
        "multiline_mode must live under Editor"
    );
    assert_eq!(
        meta.owner,
        SettingOwner::Pager,
        "multiline_mode must be PAGER-owned"
    );
}

// ---------------------------------------------------------------------------
// permission_mode (security-relevant Enum, no preview)
// ---------------------------------------------------------------------------

/// `permission_mode` lives under the `Agent` section.
#[test]
fn pr6_permission_mode_renders_under_agent_category() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("permission_mode")
        .expect("permission_mode must be registered");
    assert_eq!(
        meta.category,
        SettingCategory::Agent,
        "permission_mode must live under Agent"
    );
    assert_eq!(
        meta.owner,
        SettingOwner::Shell,
        "permission_mode is SHELL-owned (persists via shell + ACP)"
    );
}

/// `permission_mode` must be `supports_preview: false` — toggling
/// drains the permission queue (irreversible side effect).
#[test]
fn pr6_permission_mode_does_not_support_preview() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("permission_mode")
        .expect("permission_mode must be registered");
    match &meta.kind {
        SettingKind::Enum {
            supports_preview, ..
        } => {
            assert!(
                !supports_preview,
                "permission_mode MUST be supports_preview: false — toggling YOLO has \
                 irreversible side effects (drains permission_queue) so per-keystroke \
                 preview is unsafe",
            );
        }
        other => panic!("expected Enum kind for permission_mode, got {other:?}"),
    }
}

/// `permission_mode` reads from pager snapshot, not `ui` (live state).
#[test]
fn pr6_current_value_for_reads_pager_snapshot() {
    use echo_agent_pager::settings::current_value_for;

    let ui = UiConfig::default();

    let off_snap = PagerLocalSnapshot {
        multiline_mode: false,
        yolo_mode: false,
        ..PagerLocalSnapshot::default()
    };
    let on_snap = PagerLocalSnapshot {
        multiline_mode: false,
        yolo_mode: true,
        ..PagerLocalSnapshot::default()
    };

    assert_eq!(
        current_value_for("permission_mode", &ui, &off_snap),
        Some(SettingValue::Enum("ask")),
        "yolo=false → 'ask'",
    );
    assert_eq!(
        current_value_for("permission_mode", &ui, &on_snap),
        Some(SettingValue::Enum("always-approve")),
        "yolo=true → 'always-approve'",
    );

    // Defensive: even when `ui.permission_mode` says one thing,
    // the snapshot wins. Pins the LIVE-state-over-disk contract.
    let conflicting_ui = UiConfig {
        permission_mode: Some("ask".into()),
        ..UiConfig::default()
    };
    assert_eq!(
        current_value_for("permission_mode", &conflicting_ui, &on_snap),
        Some(SettingValue::Enum("always-approve")),
        "snapshot must win over `ui.permission_mode` when they disagree — \
         the snapshot is the LIVE state, `ui` is the at-startup persisted state",
    );
}

/// Enter on `permission_mode` opens the picker seeded to current state.
#[test]
fn pr6_enter_on_permission_mode_row_enters_picking_enum() {
    let mut s = make_state();
    navigate_to(&mut s, "permission_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on permission_mode row must transition to PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(*key, "permission_mode");
            assert_eq!(
                original_value,
                &SettingValue::Enum("ask"),
                "default snapshot yolo_mode=false → original 'ask'"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// Nav in `permission_mode` picker must NOT dispatch preview Actions
/// (would drain permission queue on every keystroke).
#[test]
fn pr6_permission_mode_picker_nav_does_not_dispatch_preview() {
    // Two-key navigation: open the picker, then exercise both
    // "advance" keys (Down, j) and both "retreat" keys (Up, k).
    // We re-open the picker between key probes so each key starts
    // at a known-position. The test must
    // catch a hypothetical j/k path that bypasses set_picker_idx.
    for nav_key in &[
        KeyCode::Down,
        KeyCode::Char('j'),
        KeyCode::Up,
        KeyCode::Char('k'),
    ] {
        let mut s = make_state();
        navigate_to(&mut s, "permission_mode");
        let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));

        // For "retreat" keys (Up/k) at choices_idx=0, the outcome is
        // Unchanged (clamp at first). We pre-navigate down so retreat
        // keys have something to retreat from.
        if matches!(nav_key, KeyCode::Up | KeyCode::Char('k')) {
            let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
        }

        let outcome = handle_settings_key(&mut s, &press(*nav_key));
        assert!(
            matches!(outcome, SettingsKeyOutcome::Changed),
            "Nav key {nav_key:?} in permission_mode picker MUST NOT dispatch a preview \
             Action — that would persist on every keystroke and drain the \
             permission_queue. Got {outcome:?}",
        );
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
    }
}

/// Enter on "always-approve" commits `SetPermissionMode(AlwaysApprove)`.
#[test]
fn pr6_permission_mode_picker_enter_dispatches_set_permission_mode_commit() {
    use echo_agent_pager::app::actions::PermissionModeKind;
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("permission_mode").unwrap();
    let choices = match &meta.kind {
        SettingKind::Enum { choices, .. } => *choices,
        _ => panic!("permission_mode must be Enum"),
    };
    let always_idx = choices
        .iter()
        .position(|c| c.canonical == "always-approve")
        .expect("permission_mode must include the 'always-approve' choice");
    let default_canonical = match &meta.kind {
        SettingKind::Enum { default, .. } => *default,
        _ => unreachable!(),
    };
    let initial_idx = choices
        .iter()
        .position(|c| c.canonical == default_canonical)
        .expect("registered default must be present in catalog");

    let mut s = make_state();
    navigate_to(&mut s, "permission_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));

    assert!(
        matches!(s.mode(), SettingsModalMode::PickingEnum { key, .. } if key == "permission_mode"),
        "Enter on permission_mode row must open the picker, got {:?}",
        s.mode(),
    );
    // Navigate from the seeded default canonical to "always-approve".
    let steps = always_idx as isize - initial_idx as isize;
    let nav_key = if steps > 0 {
        KeyCode::Down
    } else {
        KeyCode::Up
    };
    for _ in 0..steps.unsigned_abs() {
        let _ = handle_settings_key(&mut s, &press(nav_key));
    }
    // Enter → commit.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetPermissionMode(
            PermissionModeKind::AlwaysApprove,
        )) => {}
        other => panic!(
            "Enter on 'always-approve' must commit Action::SetPermissionMode(AlwaysApprove), \
             got {other:?}"
        ),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter commit must return to Browse"
    );
}

/// Esc in non-preview picker returns to Browse without Action.
#[test]
fn pr6_permission_mode_picker_esc_does_not_dispatch_action() {
    let mut s = make_state();
    navigate_to(&mut s, "permission_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // Navigate so the original/current distinction would matter for
    // a preview-supporting Enum.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));

    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Esc on non-preview Enum picker must NOT emit an Action — \
         doing so would re-persist on every Esc. Got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc must return to Browse"
    );
}

/// Picker seeds at "always-approve" when `yolo_mode: true`.
#[test]
fn pr6_picker_seeds_choices_idx_from_pager_snapshot_yolo_true() {
    let snapshot = PagerLocalSnapshot {
        multiline_mode: false,
        yolo_mode: true,
        auto_mode_gate: true,
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        snapshot,
    );
    navigate_to(&mut s, "permission_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let reg = SettingsRegistry::defaults();
    let always_idx = match &reg.find("permission_mode").unwrap().kind {
        SettingKind::Enum { choices, .. } => choices
            .iter()
            .position(|c| c.canonical == "always-approve")
            .expect("permission_mode must have 'always-approve' choice"),
        _ => panic!("permission_mode must be Enum"),
    };
    match s.mode() {
        SettingsModalMode::PickingEnum {
            choices_idx,
            ref original_value,
            ..
        } => {
            assert_eq!(
                choices_idx, always_idx,
                "picker must seed at the 'always-approve' index when snapshot says yolo=true"
            );
            assert_eq!(
                original_value,
                &SettingValue::Enum("always-approve"),
                "original_value must match the live snapshot"
            );
        }
        ref other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// Exactly 4 canonical choices: {ask, auto, always-approve, default}.
#[test]
fn pr6_permission_mode_choices_use_canonical_strings() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("permission_mode").unwrap();
    let canonicals: Vec<&str> = match &meta.kind {
        SettingKind::Enum { choices, .. } => choices.iter().map(|c| c.canonical).collect(),
        _ => panic!("permission_mode must be Enum"),
    };
    assert_eq!(
        canonicals.len(),
        4,
        "permission_mode catalog must be exactly {{ask, auto, always-approve, default}} — adding a \
         choice requires updating action_for_enum_commit, apply_setting_rollback, \
         PermissionModeKind, AND load_permission_mode (PR 11 contract)",
    );
    assert!(
        canonicals.contains(&"auto"),
        "permission_mode must include 'auto' canonical (auto permission mode feature)"
    );
    assert!(
        canonicals.contains(&"ask"),
        "permission_mode must include 'ask' canonical (shell schema)"
    );
    assert!(
        canonicals.contains(&"always-approve"),
        "permission_mode must include 'always-approve' canonical (shell schema)"
    );
    assert!(
        canonicals.contains(&"default"),
        "permission_mode must include 'default' canonical (PR 11 — agent's \
         default permission behavior)"
    );
}

/// Search "yolo" finds exactly `permission_mode`.
#[test]
fn pr6_search_yolo_matches_permission_mode() {
    let reg = SettingsRegistry::defaults();
    let hits = reg.search("yolo");
    assert_eq!(
        hits.len(),
        1,
        "search('yolo') must return exactly one result (permission_mode) — \
         found {} results: {:?}",
        hits.len(),
        hits.iter().map(|m| m.key).collect::<Vec<_>>(),
    );
    assert_eq!(
        hits[0].key, "permission_mode",
        "search('yolo') unique result must be permission_mode"
    );
}

// ---------------------------------------------------------------------------
// Mouse path tests for permission_mode (keyboard ↔ mouse parity)
// ---------------------------------------------------------------------------

/// First click on unselected `permission_mode` row only selects.
#[test]
fn pr6_mouse_click_on_unselected_permission_mode_row_only_selects() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "permission_mode") as u16;

    // First click: selects only.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first body-click on unselected permission_mode row should only select, got: {outcome:?}",
    );
    assert_eq!(
        s.selected, row_y as usize,
        "selection must move to permission_mode row",
    );

    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "first body-click must NOT enter the picker (two-stage UX), mode is {:?}",
        s.mode(),
    );
}

/// Second click on selected `permission_mode` opens the picker.
#[test]
fn pr6_mouse_click_on_selected_permission_mode_row_opens_picker() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "permission_mode") as u16;

    // First click: select.
    let _ = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_eq!(s.selected, row_y as usize);
    assert!(matches!(s.mode(), SettingsModalMode::Browse));

    // Second click on the now-focused row: open the picker.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on focused Enum row must open picker (Changed), got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => {
            assert_eq!(*key, "permission_mode");
        }
        other => panic!(
            "second click on focused permission_mode row must enter PickingEnum, got {other:?}",
        ),
    }
}

/// Value-column click opens picker in one click.
#[test]
fn pr6_mouse_click_on_permission_mode_indicator_opens_picker_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "permission_mode") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "value click must open picker in one click, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => {
            assert_eq!(*key, "permission_mode");
        }
        other => {
            panic!("value click on permission_mode must enter PickingEnum, got {other:?}",)
        }
    }
}

// ---------------------------------------------------------------------------
// permission_mode 3-state tests (default/ask/always-approve)
// ---------------------------------------------------------------------------

/// Picking "Default" dispatches `SetPermissionMode(Default)`.
#[test]
fn pr11_picker_commit_for_default_dispatches_set_permission_mode_default() {
    use echo_agent_pager::app::actions::PermissionModeKind;
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("permission_mode").unwrap();
    let choices = match &meta.kind {
        SettingKind::Enum { choices, .. } => *choices,
        _ => panic!("permission_mode must be Enum"),
    };
    let default_idx = choices
        .iter()
        .position(|c| c.canonical == "default")
        .expect("permission_mode must include the 'default' choice (PR 11)");
    let initial_idx = choices
        .iter()
        .position(|c| c.canonical == "ask")
        .expect("'ask' canonical must be present");
    assert_ne!(
        default_idx, initial_idx,
        "test invariant: 'default' must be a distinct choice from 'ask'"
    );

    let mut s = make_state();
    navigate_to(&mut s, "permission_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(s.mode(), SettingsModalMode::PickingEnum { key, .. } if key == "permission_mode"),
        "Enter on permission_mode row must open the picker, got {:?}",
        s.mode(),
    );
    let steps = default_idx as isize - initial_idx as isize;
    let nav_key = if steps > 0 {
        KeyCode::Down
    } else {
        KeyCode::Up
    };
    for _ in 0..steps.unsigned_abs() {
        let _ = handle_settings_key(&mut s, &press(nav_key));
    }
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetPermissionMode(PermissionModeKind::Default)) => {}
        other => panic!(
            "Enter on 'default' must commit Action::SetPermissionMode(Default), got {other:?}"
        ),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter commit must return to Browse"
    );
}

/// Picking "Ask" dispatches `SetPermissionMode(Ask)`.
#[test]
fn pr11_picker_commit_for_ask_dispatches_set_permission_mode_ask() {
    use echo_agent_pager::app::actions::PermissionModeKind;
    // Set snapshot so the picker opens seeded at "always-approve",
    // then navigate to "ask" to commit a non-default selection.
    let snapshot = PagerLocalSnapshot {
        yolo_mode: true,
        auto_mode_gate: true,
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        snapshot,
    );
    navigate_to(&mut s, "permission_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(s.mode(), SettingsModalMode::PickingEnum { key, .. } if key == "permission_mode"),
        "Enter on permission_mode row must open the picker, got {:?}",
        s.mode(),
    );

    let reg = SettingsRegistry::defaults();
    let meta = reg.find("permission_mode").unwrap();
    let choices = match &meta.kind {
        SettingKind::Enum { choices, .. } => *choices,
        _ => panic!("permission_mode must be Enum"),
    };
    let always_idx = choices
        .iter()
        .position(|c| c.canonical == "always-approve")
        .unwrap();
    let ask_idx = choices.iter().position(|c| c.canonical == "ask").unwrap();
    let steps = ask_idx as isize - always_idx as isize;
    let nav_key = if steps > 0 {
        KeyCode::Down
    } else {
        KeyCode::Up
    };
    for _ in 0..steps.unsigned_abs() {
        let _ = handle_settings_key(&mut s, &press(nav_key));
    }
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetPermissionMode(PermissionModeKind::Ask)) => {}
        other => {
            panic!("Enter on 'ask' must commit Action::SetPermissionMode(Ask), got {other:?}")
        }
    }
}

/// Returns "default" when `ui.permission_mode == "default"` and yolo=false.
#[test]
fn pr11_current_value_for_returns_default_when_ui_says_default() {
    use echo_agent_pager::settings::current_value_for;
    let ui = UiConfig {
        permission_mode: Some("default".into()),
        ..UiConfig::default()
    };
    let pager = PagerLocalSnapshot {
        yolo_mode: false,
        ..PagerLocalSnapshot::default()
    };
    assert_eq!(
        current_value_for("permission_mode", &ui, &pager),
        Some(SettingValue::Enum("default")),
        "ui.permission_mode = 'default' + yolo=false → 'default'"
    );
}

/// Live yolo_mode=true overrides ui.permission_mode → "always-approve".
#[test]
fn pr11_current_value_for_pager_yolo_overrides_default_canonical() {
    use echo_agent_pager::settings::current_value_for;
    let ui = UiConfig {
        permission_mode: Some("default".into()),
        ..UiConfig::default()
    };
    let pager = PagerLocalSnapshot {
        yolo_mode: true,
        ..PagerLocalSnapshot::default()
    };
    assert_eq!(
        current_value_for("permission_mode", &ui, &pager),
        Some(SettingValue::Enum("always-approve")),
        "pager.yolo_mode=true wins over ui.permission_mode='default' — LIVE state precedence",
    );
}

/// yolo=true + ui=None → "always-approve" (--yolo startup baseline).
#[test]
fn pr11_current_value_for_yolo_true_with_ui_none_returns_always_approve() {
    use echo_agent_pager::settings::current_value_for;
    let ui = UiConfig {
        permission_mode: None,
        ..UiConfig::default()
    };
    let pager = PagerLocalSnapshot {
        yolo_mode: true,
        ..PagerLocalSnapshot::default()
    };
    assert_eq!(
        current_value_for("permission_mode", &ui, &pager),
        Some(SettingValue::Enum("always-approve")),
        "pager.yolo_mode=true + ui.permission_mode=None → 'always-approve' \
         (LIVE-state baseline for `--yolo` startup with no config setting)"
    );
}

/// Non-"default" values with yolo=false fall through to "ask".
#[test]
fn pr11_current_value_for_falls_through_to_ask() {
    use echo_agent_pager::settings::current_value_for;
    let pager = PagerLocalSnapshot {
        yolo_mode: false,
        ..PagerLocalSnapshot::default()
    };
    // Explicit "ask" → "ask"
    let ui_ask = UiConfig {
        permission_mode: Some("ask".into()),
        ..UiConfig::default()
    };
    assert_eq!(
        current_value_for("permission_mode", &ui_ask, &pager),
        Some(SettingValue::Enum("ask")),
        "explicit 'ask' → 'ask'"
    );
    // None → "ask" (the registry default)
    let ui_none = UiConfig {
        permission_mode: None,
        ..UiConfig::default()
    };
    assert_eq!(
        current_value_for("permission_mode", &ui_none, &pager),
        Some(SettingValue::Enum("ask")),
        "None → 'ask' (registry default)"
    );
    // Garbage → "ask" (defensive fallthrough)
    let ui_garbage = UiConfig {
        permission_mode: Some("nonexistent".into()),
        ..UiConfig::default()
    };
    assert_eq!(
        current_value_for("permission_mode", &ui_garbage, &pager),
        Some(SettingValue::Enum("ask")),
        "garbage canonical → 'ask' (defensive fallthrough)"
    );
}

/// `PermissionModeKind` canonical strings round-trip.
#[test]
fn pr11_permission_mode_kind_canonical_round_trip() {
    use echo_agent_pager::app::actions::PermissionModeKind;
    for kind in [
        PermissionModeKind::Default,
        PermissionModeKind::Ask,
        PermissionModeKind::AlwaysApprove,
    ] {
        let canonical = kind.as_canonical();
        let recovered = PermissionModeKind::from_canonical(canonical)
            .unwrap_or_else(|| panic!("from_canonical('{canonical}') must round-trip"));
        assert_eq!(
            recovered, kind,
            "PermissionModeKind::from_canonical(as_canonical({kind:?})) must round-trip"
        );
    }
    // Garbage input → None
    assert!(PermissionModeKind::from_canonical("nonexistent").is_none());
    assert!(PermissionModeKind::from_canonical("").is_none());
}

/// Catalog canonicals match `PermissionModeKind::as_canonical`.
#[test]
fn pr11_permission_mode_kind_canonical_strings_match_choices_catalog() {
    use echo_agent_pager::app::actions::PermissionModeKind;
    let catalog_canonicals: std::collections::HashSet<&str> = SettingsRegistry::defaults()
        .find("permission_mode")
        .and_then(|m| match &m.kind {
            SettingKind::Enum { choices, .. } => Some(*choices),
            _ => None,
        })
        .map(|c| c.iter().map(|c| c.canonical).collect())
        .expect("permission_mode must be registered");

    for kind in [
        PermissionModeKind::Default,
        PermissionModeKind::Ask,
        PermissionModeKind::Auto,
        PermissionModeKind::AlwaysApprove,
    ] {
        assert!(
            catalog_canonicals.contains(kind.as_canonical()),
            "catalog must contain `{}` (from PermissionModeKind::{kind:?})",
            kind.as_canonical(),
        );
    }
    assert_eq!(
        catalog_canonicals.len(),
        4,
        "catalog must be exactly {{ask, auto, always-approve, default}} — adding a fifth \
         choice requires adding a PermissionModeKind variant AND updating action_for_enum_commit \
         + apply_setting_rollback + load_permission_mode + this test (PR 11 contract)",
    );
}

/// Only `AlwaysApprove` projects to `true`.
#[test]
fn pr11_permission_mode_kind_is_always_approve_projection() {
    use echo_agent_pager::app::actions::PermissionModeKind;
    assert!(PermissionModeKind::AlwaysApprove.is_always_approve());
    assert!(!PermissionModeKind::Ask.is_always_approve());
    assert!(
        !PermissionModeKind::Default.is_always_approve(),
        "PR 11: Default must project onto yolo=false — it's an alias for Ask at runtime, \
         NOT an alias for AlwaysApprove"
    );
}

// cycle_mode delegation tests live in `dispatch.rs::tests`.

// The previous
// `pr7_d_key_opens_reset_confirmation_modal` was a duplicate of
// `d_key_emits_open_reset_confirm_action_for_compact_mode` at L1617.
// Removed to eliminate redundancy — the canonical d→OpenResetConfirm
// contract is asserted there + by the parameterised
// `d_key_emits_open_reset_confirm_for_every_setting` test.
//
// The full y/n-via-handle_modal_key dispatch path is exercised by
// the dispatch.rs::tests family
// (dispatch_confirm_reset_setting_reset_dispatches_typed_setter_for_*
// + dispatch_confirm_reset_setting_cancel_preserves_modal_state).

// ---------------------------------------------------------------------------
// Render-side tests for the reset-confirm overlay.
//
// These tests assert that the rendered buffer contains the
// confirmation prompt text + breadcrumb + y/n shortcuts. Without
// them, a future change that breaks the overlay's rendering layer
// would silently regress to "user can't see the dialog".
// ---------------------------------------------------------------------------

/// User-feedback follow-up: the reset-confirm overlay applies a
/// uniform "being reset" dim style to **every cell** of the focused
/// row's rect — label cells, value cells, AND description cells —
/// so the visual emphasis is the whole row about to be reset, not
/// biased toward the description column (which already had the
/// dimmest fg before the overlay's blend was applied).
#[test]
fn reset_overlay_dims_all_rows_except_target() {
    use echo_agent_pager::views::settings_modal::ResetConfirmOverlay;
    use ratatui::buffer::Buffer;
    use ratatui::style::Modifier;
    // Set up a state with at least 3 rows visible AND navigate to a
    // specific target (NOT the initially-selected row) so we can
    // assert dim-vs-full-intensity for both target and non-target
    // rows.
    let mut s = make_state();
    navigate_to(&mut s, "show_timestamps");
    let target_idx = s.selected;
    assert!(
        target_idx > 0,
        "test setup must select a non-first row so we can sample at least one earlier non-target row"
    );

    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    let overlay = ResetConfirmOverlay {
        prompt: "Reset 'Show timestamps' to default (on)?",
        breadcrumb_suffix: "Reset 'Show timestamps'",
    };
    echo_agent_pager::views::settings_modal::render_settings_modal(
        &mut buf,
        area,
        &mut s,
        false,
        Some(&overlay),
    );

    // Locate the target row and a non-target row from the rendered
    // row_rects. Pick the row before the target so it's clearly on a
    // different y-line.
    let target_rect = s.row_rects[target_idx];
    let non_target_idx = (0..target_idx)
        .rev()
        .find(|&i| {
            let r = s.row_rects[i];
            r.width > 0 && r.height > 0
        })
        .expect("at least one visible non-target row above the target");
    let non_target_rect = s.row_rects[non_target_idx];

    assert!(
        target_rect.width > 0 && target_rect.height > 0,
        "target row's hit-rect must be populated by the render path"
    );
    assert!(
        non_target_rect.width > 0 && non_target_rect.height > 0,
        "non-target row's hit-rect must be populated by the render path"
    );
    assert_ne!(
        target_rect.y, non_target_rect.y,
        "target + non-target rows must be on distinct y-lines"
    );

    // **Spotlight invariant.** The reset overlay applies
    // `Modifier::DIM` to every cell outside the target row's
    // y-range inside the list area, and leaves the target row at
    // full intensity. We assert both sides of that contract:
    //   - every cell in the non-target row's rect has DIM
    //   - no cell in the target row's rect has DIM
    let has_dim = |x: u16, y: u16| -> bool {
        buf.cell((x, y))
            .map(|c| c.modifier.contains(Modifier::DIM))
            .unwrap_or(false)
    };

    let mut non_target_dim_count = 0usize;
    for dx in 0..non_target_rect.width {
        if has_dim(non_target_rect.x + dx, non_target_rect.y) {
            non_target_dim_count += 1;
        }
    }
    assert_eq!(
        non_target_dim_count, non_target_rect.width as usize,
        "every cell in the non-target row's rect must carry Modifier::DIM \
         (got {} out of {})",
        non_target_dim_count, non_target_rect.width
    );

    let mut target_dim_count = 0usize;
    for dx in 0..target_rect.width {
        if has_dim(target_rect.x + dx, target_rect.y) {
            target_dim_count += 1;
        }
    }
    assert_eq!(
        target_dim_count, 0,
        "no cell in the target row's rect may carry Modifier::DIM \
         (got {} dimmed out of {})",
        target_dim_count, target_rect.width
    );

    // **Action-element invariant.** The prompt row
    // (rendered ABOVE the row list) and the y/n action footer
    // shortcuts must stay at full intensity — they're the entire
    // point of the overlay. Earlier revisions only asserted dim/no-dim
    // inside `list_area`, so a future refactor that widened the dim
    // sweep to include the prompt or the action footer would have
    // silently regressed without test feedback.
    //
    // The prompt row sits at `area.y` (line 0 of the modal's content
    // area). Sample multiple x-positions to defend against a
    // future regression that only dims a sub-region of the prompt
    // line.
    let prompt_y = area.y;
    let mut prompt_dim_count = 0usize;
    for dx in 0..area.width {
        if has_dim(area.x + dx, prompt_y) {
            prompt_dim_count += 1;
        }
    }
    assert_eq!(
        prompt_dim_count, 0,
        "no cell on the prompt row (y={prompt_y}) may carry Modifier::DIM — \
         the prompt is an action element and must stay at full intensity",
    );

    // The action footer's `y reset` / `n cancel` shortcuts render
    // at the modal's bottom edge. Locate them via the rendered row
    // text and assert no cell on those lines carries DIM.
    let find_row_y = |needle: &str| -> Option<u16> {
        for y in area.y..area.y + area.height {
            let mut row_text = String::new();
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    row_text.push_str(cell.symbol());
                }
            }
            if row_text.contains(needle) {
                return Some(y);
            }
        }
        None
    };
    let action_rows: Vec<u16> = ["reset", "cancel"]
        .iter()
        .filter_map(|n| find_row_y(n))
        .collect();
    assert!(
        !action_rows.is_empty(),
        "reset/cancel action footer must be visible — found neither row",
    );
    for action_y in action_rows {
        let mut action_dim_count = 0usize;
        for dx in 0..area.width {
            if has_dim(area.x + dx, action_y) {
                action_dim_count += 1;
            }
        }
        assert_eq!(
            action_dim_count, 0,
            "no cell on the action footer row (y={action_y}) may carry \
             Modifier::DIM — y/n shortcuts are action elements and must \
             stay at full intensity",
        );
    }
}

/// User-feedback follow-up: the settings modal renders a 1-line
/// "Ask EchoAgent" tip footer at the bottom of the content area in
/// Browse, FilterFocused, and PickingEnum modes (always-on tip).
/// The footer is suppressed in `EditingValue` because the editor
/// needs every line for input + validation. This pins the
/// discoverability contract.
#[test]
fn docs_footer_renders_for_browse_and_picker() {
    use ratatui::buffer::Buffer;
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    for fixture_label in ["browse", "picker"] {
        let mut s = make_state();
        if fixture_label == "picker" {
            // Navigate to a row with an Enum kind (theme).
            navigate_to(&mut s, "theme");
            let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
            assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
        }
        let mut buf = Buffer::empty(area);
        echo_agent_pager::views::settings_modal::render_settings_modal(
            &mut buf, area, &mut s, false, None,
        );
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
            all_text.contains("Ask EchoAgent"),
            "[{fixture_label}] docs footer (`Ask EchoAgent`) must appear in the rendered modal:\n\
             {all_text}"
        );
        assert!(
            all_text.contains("change theme to echo-agent-day"),
            "[{fixture_label}] docs footer must include the example phrasing"
        );
    }
}

// ---------------------------------------------------------------------------
// User-feedback follow-up: expandable rows + restart pill on
// expand/edit.
//
// Right/`l` expands the focused row's description inline below the
// label line; Left/`h` collapses it. Multiple rows can be expanded
// simultaneously. The "restart" pill renders only while the row is
// expanded (change-time feedback is the toast's job).
// ---------------------------------------------------------------------------

/// Helper: render the modal into a sized buffer and return the full
/// rendered text as a single newline-joined string. Used by the
/// expand/collapse tests to detect description text in the buffer.
fn render_modal_to_string(s: &mut SettingsModalState, width: u16, height: u16) -> String {
    use ratatui::buffer::Buffer;
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let mut buf = Buffer::empty(area);
    echo_agent_pager::views::settings_modal::render_settings_modal(&mut buf, area, s, false, None);
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

#[test]
fn right_arrow_expands_focused_row() {
    let mut s = make_state();
    // The default focus is `compact_mode`. Press Right.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Right));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Right on focused row must transition to Changed (expanded), got {outcome:?}"
    );
    assert!(
        s.expanded_keys.contains("compact_mode"),
        "expanded_keys must contain `compact_mode` after Right, got {:?}",
        s.expanded_keys
    );

    let rendered = render_modal_to_string(&mut s, 120, 30);
    // `compact_mode`'s description starts with "Reduce padding".
    assert!(
        rendered.contains("Reduce padding"),
        "expanded row's description must appear in the rendered modal, got:\n{rendered}"
    );
}

#[test]
fn left_arrow_collapses_focused_row() {
    let mut s = make_state();
    // Pre-expand via Right.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Right));
    assert!(s.expanded_keys.contains("compact_mode"));

    let outcome = handle_settings_key(&mut s, &press(KeyCode::Left));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Left on expanded row must transition to Changed (collapsed), got {outcome:?}"
    );
    assert!(
        !s.expanded_keys.contains("compact_mode"),
        "expanded_keys must NOT contain `compact_mode` after Left, got {:?}",
        s.expanded_keys
    );

    let rendered = render_modal_to_string(&mut s, 120, 30);
    assert!(
        !rendered.contains("Reduce padding"),
        "collapsed row's description must NOT appear in the rendered modal, got:\n{rendered}"
    );
}

/// Restart-required setting at its registered default, not expanded:
/// the pill is HIDDEN. User-feedback gate keeps the modal clean for
/// the common "browsing only" case.
#[test]
fn restart_pill_hidden_when_not_expanded_and_not_edited() {
    let mut s = make_state();
    // `show_tips` is restart_required: true and its registered
    // default is `true` (matches the snapshot's None → true fallback
    // in current_value_for). Not expanded → no pill.
    navigate_to(&mut s, "show_tips");
    assert!(!s.expanded_keys.contains("show_tips"));

    let rendered = render_modal_to_string(&mut s, 120, 30);
    let restart_lines: Vec<&str> = rendered
        .lines()
        .filter(|l| l.contains("Show tips") && l.contains("restart"))
        .collect();
    assert!(
        restart_lines.is_empty(),
        "restart pill must NOT render on at-default + collapsed restart_required row, \
         found: {restart_lines:?}"
    );
}

/// Same setting, after Right → expanded. Pill renders.
#[test]
fn restart_pill_visible_when_expanded() {
    let mut s = make_state();
    navigate_to(&mut s, "show_tips");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Right));
    assert!(s.expanded_keys.contains("show_tips"));

    let rendered = render_modal_to_string(&mut s, 120, 30);
    let restart_lines: Vec<&str> = rendered
        .lines()
        .filter(|l| l.contains("Show tips") && l.contains("restart"))
        .collect();
    assert!(
        !restart_lines.is_empty(),
        "restart pill must render on expanded restart_required row, got:\n{rendered}"
    );
}

/// Edited value (differs from registered default) but collapsed:
/// NO pill. A collapsed non-default row showing it forever misreads
/// as "restart pending" — the exact repro a user hit with a
/// previously-set Off value in a fresh session.
#[test]
fn restart_pill_hidden_when_edited_but_collapsed() {
    use echo_agent_pager::settings::{PagerLocalSnapshot, SettingsRegistry};
    use echo_agent_runtime::agent::config::UiConfig;

    // Construct a state where `show_tips` is NOT at its registered
    // default of `true`.
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        PagerLocalSnapshot {
            show_tips: Some(false),
            ..PagerLocalSnapshot::default()
        },
    );
    navigate_to(&mut s, "show_tips");
    assert!(!s.expanded_keys.contains("show_tips"));

    let rendered = render_modal_to_string(&mut s, 120, 30);
    let restart_lines: Vec<&str> = rendered
        .lines()
        .filter(|l| l.contains("Show tips") && l.contains("restart"))
        .collect();
    assert!(
        restart_lines.is_empty(),
        "restart pill must NOT render on edited-but-collapsed row, found: {restart_lines:?}"
    );
}

/// Long descriptions wrap to the modal's content width on multiple
/// lines. The wrapped output covers the description verbatim
/// modulo whitespace normalization.
#[test]
fn expanded_description_wraps_to_modal_width() {
    let mut s = make_state();
    // `permission_mode`'s description is long enough to wrap at 80
    // cols. Expand and check that the entire description text is
    // present in the buffer.
    //
    // **Width.** The `→ expand` shortcut was added to the
    // Browse footer which can push the footer onto an extra line
    // at narrower widths; we render at 80 cols to keep the full
    // wrapped description visible.
    navigate_to(&mut s, "permission_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Right));

    // **Height bump.** Each non-first section header
    // earns a 1-line gap above it. With permission_mode focused
    // (Agent & Approval), two such gaps sit between Appearance
    // and the expanded row's wrapped description, which would
    // squeeze the 3rd wrapped line off the bottom at height=30.
    // Render at 34 lines so the existing assertion about
    // "automatically" still holds.
    let rendered = render_modal_to_string(&mut s, 80, 34);
    // Distinctive phrases from the description text:
    assert!(
        rendered.contains("Default") || rendered.contains("default"),
        "wrapped description must include the 'Default uses' phrase"
    );
    assert!(
        rendered.contains("Always") && rendered.contains("automatically"),
        "wrapped description must include the 'Always approve grants all \
         permissions automatically' phrase, got:\n{rendered}"
    );
}

/// Mouse click on the expand-triangle glyph (col 0 of a setting
/// row) toggles expansion — keyboard ↔ mouse parity for the
/// new expand affordance.
#[test]
fn click_on_expand_glyph_toggles_expansion() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "compact_mode") as u16;

    // First click on col 0 (triangle) — expand.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        0,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "triangle click must transition to Changed (expanded), got {outcome:?}"
    );
    assert!(
        s.expanded_keys.contains("compact_mode"),
        "triangle click on collapsed row must expand it"
    );

    // Second click on col 0 — collapse.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        0,
        row_y,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(
        !s.expanded_keys.contains("compact_mode"),
        "triangle click on expanded row must collapse it"
    );
}

/// The `l` / `h` vim aliases mirror the Right / Left arrows.
#[test]
fn vim_l_h_keys_toggle_expansion() {
    let mut s = make_state();
    // l → expand.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char('l')));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(s.expanded_keys.contains("compact_mode"));

    // h → collapse.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char('h')));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(!s.expanded_keys.contains("compact_mode"));
}

/// The confirmation overlay renders the prompt text inline at the
/// top of the content area — fixes the critical UX bug
/// where the dialog was invisible.
#[test]
fn reset_confirm_overlay_renders_prompt_with_setting_label_and_default() {
    use echo_agent_pager::views::settings_modal::ResetConfirmOverlay;
    use ratatui::buffer::Buffer;
    let mut s = make_state();
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    let overlay = ResetConfirmOverlay {
        prompt: "Reset 'Compact mode' to default (off)?",
        breadcrumb_suffix: "Reset 'Compact mode'",
    };
    echo_agent_pager::views::settings_modal::render_settings_modal(
        &mut buf,
        area,
        &mut s,
        false,
        Some(&overlay),
    );
    let mut all_text = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                all_text.push_str(cell.symbol());
            }
        }
        all_text.push('\n');
    }
    // The prompt text appears in the rendered buffer.
    assert!(
        all_text.contains("Reset 'Compact mode' to default (off)?"),
        "overlay prompt must appear in the rendered buffer:\n{all_text}"
    );
    // The chrome breadcrumb shows the abbreviated form.
    assert!(
        all_text.contains("Reset 'Compact mode'"),
        "breadcrumb suffix must appear in the chrome title:\n{all_text}"
    );
    // The confirmation footer shortcuts are visible.
    assert!(
        all_text.contains("y reset"),
        "y reset shortcut must appear in the rendered buffer:\n{all_text}",
    );
    assert!(
        all_text.contains("n cancel"),
        "n cancel shortcut must appear in the rendered buffer:\n{all_text}",
    );
}

/// Verify the modal helper `reset_confirm_prompt` constructs a
/// well-formed prompt for each registered setting. Catches a
/// formatter regression where a registry catalog reorder or a
/// missing display string would render an empty or garbled prompt.
#[test]
fn reset_confirm_prompt_helper_builds_well_formed_string_for_every_setting() {
    use echo_agent_pager::settings::{PagerLocalSnapshot, SettingsRegistry};
    use echo_agent_pager::views::modal::{ActiveModal, ModalConfirmation, reset_confirm_prompt};
    use echo_agent_runtime::agent::config::UiConfig;
    let reg = SettingsRegistry::defaults();
    for meta in reg.all() {
        let state = Box::new(SettingsModalState::new(
            Arc::new(reg.clone()),
            UiConfig::default(),
            PagerLocalSnapshot::default(),
        ));
        let modal = ActiveModal::ResetSettingsConfirm {
            modal: ModalConfirmation::reset_settings(),
            key: meta.key,
            settings_state: state,
        };
        let prompt = reset_confirm_prompt(&modal).unwrap_or_else(|| {
            panic!(
                "reset_confirm_prompt returned None for `{}` — registry skew",
                meta.key
            )
        });
        assert!(
            prompt.contains(meta.label),
            "prompt for `{}` must contain its label `{}`, got: {prompt:?}",
            meta.key,
            meta.label,
        );
        assert!(
            prompt.starts_with("Reset"),
            "prompt for `{}` must start with 'Reset', got: {prompt:?}",
            meta.key,
        );
        assert!(
            prompt.ends_with("?"),
            "prompt for `{}` must be phrased as a question, got: {prompt:?}",
            meta.key,
        );
    }
}

// ---------------------------------------------------------------------------
// String + Int editor + validators
// ---------------------------------------------------------------------------

/// Int stepper: Enter opens, Up/Down/Left/Right step+clamp, Enter commits.
#[test]
fn pr15_int_stepper_commit_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "max_thoughts_width");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on Int row must transition to EditingValue, got {outcome:?}"
    );
    assert_eq!(
        s.editing_buffer(),
        Some("120"),
        "buffer must seed from default",
    );

    // Stepper: Up = +5 → 125. Right = +10 → 135. Down x3 = -15
    // → 120. Up x16 = +80 → 200.
    for _ in 0..16 {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Up));
    }
    assert_eq!(s.editing_buffer(), Some("200"));

    // Enter commits at 200.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetMaxThoughtsWidth(200)) => {}
        other => panic!("expected SetMaxThoughtsWidth(200), got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "successful commit must return to Browse"
    );
}

/// `default_model` DynamicEnum picker: Enter opens, catalog rows
/// dispatch `SetDefaultModel(<ModelId>)` resolved from snapshot.
#[test]
fn pr14_default_model_picker_commits_resolved_model_id() {
    let snapshot = PagerLocalSnapshot {
        available_models: vec![
            (
                "EchoAgent 4.5".to_string(),
                agent_client_protocol::ModelId::new(std::sync::Arc::from("echo-agent-4.5")),
            ),
            (
                "EchoAgent 3".to_string(),
                agent_client_protocol::ModelId::new(std::sync::Arc::from("echo-agent-3")),
            ),
        ],
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        snapshot,
    );
    navigate_to(&mut s, "default_model");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on DynamicEnum row must transition to PickingEnum, got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::PickingEnum { key, .. } if key == "default_model"),
        "Enter must transition to PickingEnum for default_model"
    );

    // Walk down past row 0 ("(no override)") to row 1 ("EchoAgent 4.5").
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Down));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Down on picker must move the focus, got {outcome:?}"
    );

    // Enter commits the selected model.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetDefaultModel(id)) => {
            assert_eq!(
                id.0.as_ref(),
                "echo-agent-4.5",
                "committed id must match snapshot"
            );
        }
        other => panic!("expected SetDefaultModel(<id>) on commit, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "successful commit must return to Browse"
    );
}

/// Row-0 "(no override)" dispatches `ClearDefaultModel`.
#[test]
fn pr14_default_model_picker_row_zero_commits_clear_action() {
    let snapshot = PagerLocalSnapshot {
        available_models: vec![(
            "EchoAgent 3".to_string(),
            agent_client_protocol::ModelId::new(std::sync::Arc::from("echo-agent-3")),
        )],
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        snapshot,
    );
    navigate_to(&mut s, "default_model");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // Picker opens with choices_idx at the snapshot's current model,
    // OR at 0 when current_model_name is None. The fixture above
    // leaves current_model_name as None → picker opens on row 0.
    match &s.mode() {
        SettingsModalMode::PickingEnum { choices_idx, .. } => {
            assert_eq!(
                *choices_idx, 0,
                "picker must seed at row 0 when no current model is set"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
    // Enter on row 0 → ClearDefaultModel.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::ClearDefaultModel) => {}
        other => panic!("expected ClearDefaultModel from row-0 commit, got {other:?}"),
    }
}

/// Mouse click on `default_model` opens picker (keyboard ↔ mouse parity).
#[test]
fn pr14_mouse_click_on_dynamic_enum_row_opens_picker() {
    let snapshot = PagerLocalSnapshot {
        available_models: vec![(
            "EchoAgent 3".to_string(),
            agent_client_protocol::ModelId::new(std::sync::Arc::from("echo-agent-3")),
        )],
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        snapshot,
    );
    s.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 80,
    };
    s.row_rects.resize(s.rows.len(), Rect::default());
    let row_idx = row_idx_for(&s, "default_model");
    s.row_rects[row_idx] = Rect {
        x: 0,
        y: row_idx as u16,
        width: 80,
        height: 1,
    };
    // First click: select.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        20,
        row_idx as u16,
    );
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    // Second click on the same row: opens picker.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        20,
        row_idx as u16,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on DynamicEnum row must open picker, got {outcome:?}",
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::PickingEnum { key, .. } if key == "default_model"),
        "second click on DynamicEnum row must transition to PickingEnum, got {:?}",
        s.mode(),
    );
}

/// Mouse click on `max_thoughts_width` opens the inline editor.
#[test]
fn pr8_mouse_click_on_int_row_opens_editor() {
    let mut s = make_state();
    s.list_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 80,
    };
    s.row_rects.resize(s.rows.len(), Rect::default());
    let row_idx = row_idx_for(&s, "max_thoughts_width");
    s.row_rects[row_idx] = Rect {
        x: 0,
        y: row_idx as u16,
        width: 80,
        height: 1,
    };
    // First click selects.
    let _ = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        20,
        row_idx as u16,
    );
    // Second click opens editor.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        20,
        row_idx as u16,
    );

    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on Int row must be Changed, got {outcome:?}",
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::EditingValue { key, .. } if key == "max_thoughts_width"),
        "second click on Int row must transition to EditingValue, got {:?}",
        s.mode(),
    );
}

/// Int stepper: Up/Down ±5, Left/Right ±10, clamped to [min, max].
#[test]
fn pr15_int_stepper_up_down_left_right_steps_and_clamps() {
    let mut s = make_state();
    navigate_to(&mut s, "max_thoughts_width");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // Default 120. Up: 120 + 5 = 125.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Up));
    assert_eq!(s.editing_buffer(), Some("125"));
    // Right: 125 + 10 = 135.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Right));
    assert_eq!(s.editing_buffer(), Some("135"));
    // Down 20x: 135 - 100 = 35 → clamps to min (40).
    for _ in 0..20 {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    }
    assert_eq!(s.editing_buffer(), Some("40"), "must clamp to min");
    // Right 100x: 40 + 1000 = 1040 → clamps to max (500).
    for _ in 0..100 {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Right));
    }
    assert_eq!(s.editing_buffer(), Some("500"), "must clamp to max");
    // Left 100x: 500 - 1000 = -500 → clamps to min (40).
    for _ in 0..100 {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Left));
    }
    assert_eq!(s.editing_buffer(), Some("40"), "Left must clamp to min",);
}

/// Int stepper rejects all text-input keys.
#[test]
fn pr15_int_stepper_rejects_text_input_keys() {
    let mut s = make_state();
    navigate_to(&mut s, "max_thoughts_width");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let initial_buffer = s
        .editing_buffer()
        .expect("expected EditingValue")
        .to_owned();
    assert_eq!(initial_buffer, "120", "buffer seeds from default");

    let reject_keys = &[
        KeyCode::Char('5'),
        KeyCode::Char('a'),
        KeyCode::Char('-'),
        // Extended reject-set.
        KeyCode::Char(' '), // Space (the Bool-toggle key in Browse mode)
        KeyCode::Char('+'), // Plus (would be a naïve numpad expectation)
        KeyCode::Char('.'), // Decimal point
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Insert,
        KeyCode::PageUp,
        KeyCode::PageDown,
    ];
    for k in reject_keys {
        let outcome = handle_settings_key(&mut s, &press(*k));
        assert!(
            matches!(outcome, SettingsKeyOutcome::Unchanged),
            "Int stepper must reject {k:?} (got {outcome:?})",
        );
        assert_eq!(
            s.editing_buffer(),
            Some(initial_buffer.as_str()),
            "buffer must stay at {initial_buffer:?} after {k:?}",
        );
    }

    // Ctrl-modifier chords are also rejected.
    use crossterm::event::KeyModifiers;
    let press_with =
        |code: KeyCode, mods: KeyModifiers| crossterm::event::KeyEvent::new(code, mods);
    let outcome = handle_settings_key(
        &mut s,
        &press_with(KeyCode::Char('c'), KeyModifiers::CONTROL),
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Unchanged),
        "Int stepper must reject Ctrl+c (got {outcome:?})",
    );
    assert_eq!(
        s.editing_buffer(),
        Some(initial_buffer.as_str()),
        "buffer must stay unchanged after Ctrl+c",
    );
}

#[test]
fn int_stepper_rejects_modified_esc_but_plain_esc_cancels() {
    let mut state = make_state();
    navigate_to(&mut state, "max_thoughts_width");
    let _ = handle_settings_key(&mut state, &press(KeyCode::Enter));
    for modifiers in [
        KeyModifiers::ALT,
        KeyModifiers::CONTROL,
        KeyModifiers::SUPER,
    ] {
        let outcome = handle_settings_key(&mut state, &press_with(KeyCode::Esc, modifiers));
        assert!(matches!(outcome, SettingsKeyOutcome::Unchanged));
        assert!(matches!(
            state.mode(),
            SettingsModalMode::EditingValue { .. }
        ));
        assert_eq!(state.editing_buffer(), Some("120"));
    }

    let outcome = handle_settings_key(&mut state, &press(KeyCode::Esc));
    assert!(matches!(outcome, SettingsKeyOutcome::Changed));
    assert!(matches!(state.mode(), SettingsModalMode::Browse));
}

/// Esc in EditingValue returns to Browse without dispatching.
#[test]
fn pr8_esc_in_editing_value_cancels_without_dispatch() {
    let mut s = make_state();
    navigate_to(&mut s, "max_thoughts_width");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // Type some chars to dirty the buffer.
    for ch in "999".chars() {
        let _ = handle_settings_key(&mut s, &press(KeyCode::Char(ch)));
    }
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Esc in EditingValue must be Changed (mode swap), got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc must return to Browse"
    );
}

/// `default_model` and `max_thoughts_width` defaults round-trip
/// against hard-coded literals.
#[test]
fn pr8_default_model_and_max_thoughts_width_defaults_roundtrip() {
    use echo_agent_pager::settings::current_value_for;
    let reg = SettingsRegistry::defaults();
    let ui = UiConfig::default();
    let pager = PagerLocalSnapshot::default();

    // default_model: registered default is the empty-string sentinel
    // (no UiConfig mirror — cfg.models.default is resolved
    // dynamically). `current_value_for` reads from
    // `pager.current_model_name` which is None by default →
    // `unwrap_or_default()` produces the empty string. Both paths
    // converge on `SettingValue::String("")`.
    let dm_meta = reg.find("default_model").unwrap();
    assert_eq!(
        echo_agent_pager::settings::default_value_for(dm_meta),
        SettingValue::String(String::new()),
        "default_model registered default must be the empty string",
    );
    assert_eq!(
        current_value_for("default_model", &ui, &pager).unwrap(),
        SettingValue::String(String::new()),
        "default_model current_value_for with empty pager snapshot must be empty",
    );

    // max_thoughts_width: registered default is 120 (matches
    // UiConfig::default()'s DEFAULT_MAX_THOUGHTS_WIDTH constant).
    let mt_meta = reg.find("max_thoughts_width").unwrap();
    assert_eq!(
        echo_agent_pager::settings::default_value_for(mt_meta),
        SettingValue::Int(120),
        "max_thoughts_width registered default must be 120",
    );
    assert_eq!(
        current_value_for("max_thoughts_width", &ui, &pager).unwrap(),
        SettingValue::Int(120),
        "max_thoughts_width current_value_for must be 120 (UiConfig::default())",
    );
}

// ---------------------------------------------------------------------------
// coding_data_sharing (Privacy Enum, no preview — async ACP)
// ---------------------------------------------------------------------------

/// `coding_data_sharing` lives under `Privacy`.
#[test]
fn pr9_coding_data_sharing_renders_under_privacy_category() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("coding_data_sharing")
        .expect("coding_data_sharing must be registered");
    assert_eq!(
        meta.category,
        SettingCategory::Privacy,
        "coding_data_sharing must live under Privacy"
    );
    assert_eq!(
        meta.owner,
        SettingOwner::Shell,
        "coding_data_sharing is SHELL-owned (auth-metadata-backed, persists via ACP)"
    );
}

/// `coding_data_sharing` must be `supports_preview: false` (async ACP).
#[test]
fn pr9_coding_data_sharing_does_not_support_preview() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("coding_data_sharing")
        .expect("coding_data_sharing must be registered");
    match &meta.kind {
        SettingKind::Enum {
            supports_preview, ..
        } => {
            assert!(
                !supports_preview,
                "coding_data_sharing MUST be supports_preview: false — every preview \
                 would fire an async ACP round-trip OR commit-on-every-nav, both \
                 unacceptable",
            );
        }
        other => panic!("expected Enum kind for coding_data_sharing, got {other:?}"),
    }
}

/// Reads from pager snapshot; inverts `_opt_out` bool.
#[test]
fn pr9_current_value_for_reads_pager_snapshot_inverts_opt_out() {
    use echo_agent_pager::settings::current_value_for;

    let ui = UiConfig::default();

    let opted_in_snap = PagerLocalSnapshot {
        coding_data_sharing_opt_out: false,
        ..PagerLocalSnapshot::default()
    };
    let opted_out_snap = PagerLocalSnapshot {
        coding_data_sharing_opt_out: true,
        ..PagerLocalSnapshot::default()
    };

    assert_eq!(
        current_value_for("coding_data_sharing", &ui, &opted_in_snap),
        Some(SettingValue::Enum("opt-in")),
        "opt_out=false → canonical 'opt-in' (user IS sharing data)",
    );
    assert_eq!(
        current_value_for("coding_data_sharing", &ui, &opted_out_snap),
        Some(SettingValue::Enum("opt-out")),
        "opt_out=true → canonical 'opt-out' (user opted OUT of sharing)",
    );
}

/// Enter opens picker seeded to current state.
#[test]
fn pr9_enter_on_coding_data_sharing_row_enters_picking_enum() {
    let mut s = make_state();
    navigate_to(&mut s, "coding_data_sharing");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on coding_data_sharing row must transition to PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(*key, "coding_data_sharing");
            assert_eq!(
                original_value,
                &SettingValue::Enum("opt-out"),
                "default snapshot opt_out=true → original 'opt-out'"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// Nav in picker must NOT dispatch preview (async ACP).
#[test]
fn pr9_coding_data_sharing_picker_nav_does_not_dispatch_preview() {
    for nav_key in &[
        KeyCode::Down,
        KeyCode::Char('j'),
        KeyCode::Up,
        KeyCode::Char('k'),
    ] {
        let mut s = make_state();
        navigate_to(&mut s, "coding_data_sharing");
        let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));

        // Pre-position so the nav key under test has room to move no matter
        // which choice the registry default opens the picker on (Up needs
        // idx > 0, Down needs idx < last).
        if matches!(nav_key, KeyCode::Up | KeyCode::Char('k')) {
            let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
        } else {
            let _ = handle_settings_key(&mut s, &press(KeyCode::Up));
        }

        let outcome = handle_settings_key(&mut s, &press(*nav_key));
        assert!(
            matches!(outcome, SettingsKeyOutcome::Changed),
            "Nav key {nav_key:?} in coding_data_sharing picker MUST NOT dispatch a preview \
             Action — that would fire a network round-trip per keystroke. Got {outcome:?}",
        );
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
    }
}

/// Enter commits `SetCodingDataSharing { opted_in }` (opt-in→true).
#[test]
fn pr9_coding_data_sharing_picker_enter_dispatches_set_commit() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("coding_data_sharing").unwrap();
    let (default_canonical, choices) = match &meta.kind {
        SettingKind::Enum {
            default, choices, ..
        } => (*default, *choices),
        _ => panic!("coding_data_sharing must be Enum"),
    };
    // Resolve "the other" canonical from the registry rather than
    // hardcoding — robust against future catalog additions.
    let other_canonical = choices
        .iter()
        .map(|c| c.canonical)
        .find(|c| *c != default_canonical)
        .expect("coding_data_sharing must have ≥2 choices");
    let expected_opted_in = match other_canonical {
        "opt-in" => true,
        "opt-out" => false,
        _ => panic!("unexpected canonical: {other_canonical:?}"),
    };

    let mut s = make_state();
    navigate_to(&mut s, "coding_data_sharing");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // Nav to the OTHER choice — direction depends on where the registry
    // default opened the picker, so derive it instead of hardcoding Down.
    let default_idx = choices
        .iter()
        .position(|c| c.canonical == default_canonical)
        .expect("default must be a registry choice");
    let other_idx = choices
        .iter()
        .position(|c| c.canonical == other_canonical)
        .expect("other choice must be in the registry");
    let nav = if other_idx > default_idx {
        KeyCode::Down
    } else {
        KeyCode::Up
    };
    let _ = handle_settings_key(&mut s, &press(nav));
    // Enter → commit.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetCodingDataSharing { opted_in }) => {
            assert_eq!(
                opted_in, expected_opted_in,
                "Enter must commit `{other_canonical}` → SetCodingDataSharing(opted_in={expected_opted_in})"
            );
        }
        other => panic!("expected Action::SetCodingDataSharing commit, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter commit must return to Browse"
    );
}

/// Esc in non-preview picker returns to Browse without Action.
#[test]
fn pr9_coding_data_sharing_picker_esc_does_not_dispatch_action() {
    let mut s = make_state();
    navigate_to(&mut s, "coding_data_sharing");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));

    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Esc on non-preview Enum picker must NOT emit an Action — \
         doing so would fire an ACP round-trip on every Esc. Got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc must return to Browse"
    );
}

/// Picker seeds at "opt-out" when `coding_data_sharing_opt_out: true`.
#[test]
fn pr9_picker_seeds_choices_idx_from_pager_snapshot_opt_out_true() {
    let snapshot = PagerLocalSnapshot {
        coding_data_sharing_opt_out: true,
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        snapshot,
    );
    navigate_to(&mut s, "coding_data_sharing");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let reg = SettingsRegistry::defaults();
    let opt_out_idx = match &reg.find("coding_data_sharing").unwrap().kind {
        SettingKind::Enum { choices, .. } => choices
            .iter()
            .position(|c| c.canonical == "opt-out")
            .expect("coding_data_sharing must have 'opt-out' choice"),
        _ => panic!("coding_data_sharing must be Enum"),
    };
    match s.mode() {
        SettingsModalMode::PickingEnum {
            choices_idx,
            ref original_value,
            ..
        } => {
            assert_eq!(
                choices_idx, opt_out_idx,
                "picker must seed at the 'opt-out' index when snapshot says opt_out=true"
            );
            assert_eq!(
                original_value,
                &SettingValue::Enum("opt-out"),
                "original_value must match the live snapshot"
            );
        }
        ref other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// Exactly 2 canonical choices: {opt-in, opt-out}.
#[test]
fn pr9_coding_data_sharing_choices_use_canonical_strings() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("coding_data_sharing").unwrap();
    let canonicals: Vec<&str> = match &meta.kind {
        SettingKind::Enum { choices, .. } => choices.iter().map(|c| c.canonical).collect(),
        _ => panic!("coding_data_sharing must be Enum"),
    };
    assert_eq!(
        canonicals.len(),
        2,
        "coding_data_sharing catalog must be exactly {{opt-in, opt-out}} — adding a \
         choice requires updating the action_for_enum_commit arm in \
         views/settings_modal.rs AND the action_for_reset arm in dispatch.rs",
    );
    assert!(
        canonicals.contains(&"opt-in"),
        "coding_data_sharing must include 'opt-in' canonical"
    );
    assert!(
        canonicals.contains(&"opt-out"),
        "coding_data_sharing must include 'opt-out' canonical"
    );
}

/// Search "privacy" finds exactly `coding_data_sharing`.
#[test]
fn pr9_search_privacy_matches_coding_data_sharing() {
    let reg = SettingsRegistry::defaults();
    let hits = reg.search("privacy");
    // The category label "Privacy" appears as a header but is not
    // part of `search()`'s haystack (search ignores categories);
    // matches come from the meta's keywords + label + description.
    let hit_keys: Vec<&str> = hits.iter().map(|m| m.key).collect();
    assert_eq!(
        hits.len(),
        1,
        "search('privacy') must return EXACTLY one result (coding_data_sharing). \
         Found {} results: {hit_keys:?}. \
         If this fails because another setting added 'privacy' to its keywords/label/\
         description, decide: (a) is 'privacy' a real keyword for that setting? If yes, \
         loosen this assertion to a presence-only check `hit_keys.contains(&\"coding_data_sharing\")`. \
         (b) If no, remove 'privacy' from the other setting's haystack — search relevance \
         is more important than tag promiscuity.",
        hits.len(),
    );
    assert_eq!(
        hits[0].key, "coding_data_sharing",
        "search('privacy') unique result must be coding_data_sharing"
    );
}

// ---------------------------------------------------------------------------
// Mouse path tests for coding_data_sharing
// ---------------------------------------------------------------------------

/// First click on unselected row only selects.
#[test]
fn pr9_mouse_click_on_unselected_coding_data_sharing_row_only_selects() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "coding_data_sharing") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first body-click on unselected coding_data_sharing row should only select, got: {outcome:?}",
    );
    assert_eq!(s.selected, row_y as usize);
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

/// Second click on selected row opens picker.
#[test]
fn pr9_mouse_click_on_selected_coding_data_sharing_row_opens_picker() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "coding_data_sharing") as u16;

    // First click: select.
    let _ = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_eq!(s.selected, row_y as usize);

    // Second click on the focused row: open the picker.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on focused Enum row must open picker, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => {
            assert_eq!(*key, "coding_data_sharing");
        }
        _ => panic!("second click on focused coding_data_sharing row must enter PickingEnum"),
    }
}

/// Value-column click opens picker in one click.
#[test]
fn pr9_mouse_click_on_coding_data_sharing_indicator_opens_picker_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "coding_data_sharing") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "value click must open picker in one click, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => {
            assert_eq!(*key, "coding_data_sharing");
        }
        _ => {
            panic!("value click on coding_data_sharing must enter PickingEnum")
        }
    }
}

// ---------------------------------------------------------------------------
// default_selected_permission (Agent Enum, no preview — SHELL-owned, persists)
// ---------------------------------------------------------------------------

/// `default_selected_permission` lives under `Agent` and is SHELL-owned.
#[test]
fn default_selected_permission_renders_under_agent_category() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("default_selected_permission")
        .expect("default_selected_permission must be registered");
    assert_eq!(
        meta.category,
        SettingCategory::Agent,
        "default_selected_permission must live under Agent"
    );
    assert_eq!(
        meta.owner,
        SettingOwner::Shell,
        "default_selected_permission is SHELL-owned (persists to [ui].default_selected_permission)"
    );
}

/// `default_selected_permission` must be `supports_preview: false` —
/// permission prompts aren't open in the modal background, so there is
/// no live preview surface to drive.
#[test]
fn default_selected_permission_does_not_support_preview() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("default_selected_permission")
        .expect("default_selected_permission must be registered");
    match &meta.kind {
        SettingKind::Enum {
            supports_preview, ..
        } => {
            assert!(
                !supports_preview,
                "default_selected_permission MUST be supports_preview: false",
            );
        }
        other => panic!("expected Enum kind for default_selected_permission, got {other:?}"),
    }
}

/// `current_value_for` maps `UiConfig::default()` (None on disk) onto the
/// `always_allow_all_sessions` canonical (the effective default).
#[test]
fn default_selected_permission_current_value_defaults_to_always_allow_all_sessions() {
    use echo_agent_pager::settings::current_value_for;
    let ui = UiConfig::default();
    let pager = PagerLocalSnapshot::default();
    assert_eq!(
        current_value_for("default_selected_permission", &ui, &pager),
        Some(SettingValue::Enum("always_allow_all_sessions")),
        "None on disk → canonical 'always_allow_all_sessions' (effective default)",
    );
}

/// Enter opens the picker seeded to the current ("always_allow_all_sessions") value.
#[test]
fn default_selected_permission_enter_enters_picking_enum() {
    let mut s = make_state();
    navigate_to(&mut s, "default_selected_permission");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on default_selected_permission row must transition to PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(*key, "default_selected_permission");
            assert_eq!(
                original_value,
                &SettingValue::Enum("always_allow_all_sessions"),
                "default UiConfig → original 'always_allow_all_sessions'"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// Nav in the picker must NOT dispatch an Action — there is no preview.
#[test]
fn default_selected_permission_picker_nav_does_not_dispatch_preview() {
    for nav_key in &[
        KeyCode::Down,
        KeyCode::Char('j'),
        KeyCode::Up,
        KeyCode::Char('k'),
    ] {
        let mut s = make_state();
        navigate_to(&mut s, "default_selected_permission");
        let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));

        if matches!(nav_key, KeyCode::Up | KeyCode::Char('k')) {
            let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
        }

        let outcome = handle_settings_key(&mut s, &press(*nav_key));
        assert!(
            matches!(outcome, SettingsKeyOutcome::Changed),
            "Nav key {nav_key:?} in default_selected_permission picker MUST NOT dispatch an \
             Action (no preview). Got {outcome:?}",
        );
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
    }
}

/// Enter commits `SetDefaultSelectedPermission(canonical)` carrying the
/// selected choice's canonical string, then returns to Browse.
#[test]
fn default_selected_permission_picker_enter_dispatches_set_commit() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("default_selected_permission").unwrap();
    let (default_canonical, choices) = match &meta.kind {
        SettingKind::Enum {
            default, choices, ..
        } => (*default, *choices),
        _ => panic!("default_selected_permission must be Enum"),
    };
    // Picker seeds at the current value ("always_allow_all_sessions"); navigate one row down
    // and resolve that choice's canonical from the registry rather than
    // hardcoding it — robust against future catalog reordering.
    let seed_idx = choices
        .iter()
        .position(|c| c.canonical == default_canonical)
        .expect("default canonical must exist in choices");
    let target_canonical = choices
        .get(seed_idx + 1)
        .map(|c| c.canonical)
        .expect("default_selected_permission must have a choice after the default");

    let mut s = make_state();
    navigate_to(&mut s, "default_selected_permission");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // Nav to the next choice.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    // Enter → commit.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetDefaultSelectedPermission(canonical)) => {
            assert_eq!(
                canonical, target_canonical,
                "Enter must commit the selected canonical via SetDefaultSelectedPermission"
            );
        }
        other => panic!("expected Action::SetDefaultSelectedPermission commit, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter commit must return to Browse"
    );
}

/// Esc in the non-preview picker returns to Browse without an Action.
#[test]
fn default_selected_permission_picker_esc_does_not_dispatch_action() {
    let mut s = make_state();
    navigate_to(&mut s, "default_selected_permission");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));

    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Esc on non-preview Enum picker must NOT emit an Action. Got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc must return to Browse"
    );
}

// ---------------------------------------------------------------------------
// Mouse path tests for default_selected_permission
// ---------------------------------------------------------------------------

/// First click on unselected row only selects.
#[test]
fn default_selected_permission_mouse_click_on_unselected_row_only_selects() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "default_selected_permission") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first body-click on unselected default_selected_permission row should only select, got: {outcome:?}",
    );
    assert_eq!(s.selected, row_y as usize);
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

/// Second click on selected row opens picker.
#[test]
fn default_selected_permission_mouse_click_on_selected_row_opens_picker() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "default_selected_permission") as u16;

    // First click: select.
    let _ = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_eq!(s.selected, row_y as usize);

    // Second click on the focused row: open the picker.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on focused Enum row must open picker, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => {
            assert_eq!(*key, "default_selected_permission");
        }
        _ => {
            panic!("second click on focused default_selected_permission row must enter PickingEnum")
        }
    }
}

/// Value-column click opens picker in one click.
#[test]
fn default_selected_permission_mouse_click_on_indicator_opens_picker_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "default_selected_permission") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "value click must open picker in one click, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => {
            assert_eq!(*key, "default_selected_permission");
        }
        _ => {
            panic!("value click on default_selected_permission must enter PickingEnum")
        }
    }
}

/// `/privacy` takes no arguments: it opens the settings page and nothing
/// else. The alias parser it used to carry (`opt-in`, `share`, `out`, …) is
/// gone — a one-word prompt alias could flip a privacy preference with none
/// of the disclosure copy in front of the user, and the ambiguous forms
/// (`on`/`off`) risked landing on the opposite of the intent.
#[test]
fn pr9_privacy_slash_command_takes_no_arguments() {
    use echo_agent_pager::slash::commands::builtin_commands;
    use echo_agent_pager::slash::registry::CommandRegistry;

    let reg = CommandRegistry::new(builtin_commands());
    let cmd = reg.get("privacy").expect("/privacy must be registered");
    assert!(
        !cmd.takes_args(),
        "/privacy must not advertise an argument slot"
    );
    assert_eq!(cmd.usage(), "/privacy");
}

// ---------------------------------------------------------------------------
// `plan_mode` (Agent-category Enum, PAGER-owned + ACP-mediated,
// supports_preview: false)
//
// Migrated from the per-Action `Action::EnterPlanMode` (no-description
// case) to the typed `Action::SetPlanMode(PlanModeKind)` going through
// the unified `set_plan_mode` dispatch path. The dispatcher is the
// single source of truth for idempotency, optimistic mutation
// (`plan_mode_pending`), modal-snapshot refresh, toast, and the
// `Effect::SetSessionMode` ACP emit.
//
// **Why `supports_preview: false`**: toggling fires an ACP
// `session/set_mode` request that mutates per-agent state and gates
// tool dispatch. Per-keystroke preview would either fire N round-trips
// per nav OR commit on every keystroke. Both are unacceptable.
// ---------------------------------------------------------------------------

/// `plan_mode` lives under the `Agent` section:
/// pins the category against drift.
#[test]
fn pr10_plan_mode_renders_under_agent_category() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("plan_mode").expect("plan_mode must be registered");
    assert_eq!(
        meta.category,
        SettingCategory::Agent,
        "plan_mode must live under Agent"
    );
    assert_eq!(
        meta.owner,
        SettingOwner::Pager,
        "plan_mode is PAGER-owned (per-session, NOT persisted to config.toml; \
         shell drives transitions via ACP CurrentModeUpdate)"
    );
}

/// `plan_mode` is an Enum with `supports_preview: false`. Toggling
/// fires an ACP `session/set_mode` request; per-keystroke preview
/// would either fire N round-trips OR commit on every nav.
#[test]
fn pr10_plan_mode_does_not_support_preview() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("plan_mode").expect("plan_mode must be registered");
    match &meta.kind {
        SettingKind::Enum {
            supports_preview, ..
        } => {
            assert!(
                !supports_preview,
                "plan_mode MUST be supports_preview: false — preview would \
                 fire an ACP round-trip per nav OR commit on every keystroke; \
                 both are unacceptable for an agent-mode transition",
            );
        }
        other => panic!("expected Enum kind for plan_mode, got {other:?}"),
    }
}

/// `current_value_for("plan_mode", _, pager)` reads from the pager
/// snapshot's `plan_mode_active` field (NOT from `UiConfig` — there
/// is no such UiConfig field by design; plan mode is per-session
/// only). Canonical mapping: `true → "on"`, `false → "off"`.
#[test]
fn pr10_current_value_for_reads_pager_snapshot() {
    use echo_agent_pager::settings::current_value_for;

    let ui = UiConfig::default();

    let off_snap = PagerLocalSnapshot {
        plan_mode_active: false,
        ..PagerLocalSnapshot::default()
    };
    let on_snap = PagerLocalSnapshot {
        plan_mode_active: true,
        ..PagerLocalSnapshot::default()
    };

    assert_eq!(
        current_value_for("plan_mode", &ui, &off_snap),
        Some(SettingValue::Enum("off")),
        "plan_mode_active=false → canonical 'off' (default state)",
    );
    assert_eq!(
        current_value_for("plan_mode", &ui, &on_snap),
        Some(SettingValue::Enum("on")),
        "plan_mode_active=true → canonical 'on'",
    );
}

/// Enter on the `plan_mode` row → PickingEnum mode (mirroring the
/// theme/permission_mode/coding_data_sharing picker), seeded to the
/// canonical of the current live state.
#[test]
fn pr10_enter_on_plan_mode_row_enters_picking_enum() {
    let mut s = make_state();
    navigate_to(&mut s, "plan_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on plan_mode row must transition to PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(*key, "plan_mode");
            assert_eq!(
                original_value,
                &SettingValue::Enum("off"),
                "default snapshot plan_mode_active=false → original 'off'"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// **Regression test.** Up/Down/j/k nav in the `plan_mode` picker
/// MUST NOT dispatch a preview Action — that would fire an ACP
/// round-trip per keystroke (the ACP path is eager). Mirror of
/// `pr6_permission_mode_picker_nav_does_not_dispatch_preview` and
/// `pr9_coding_data_sharing_picker_nav_does_not_dispatch_preview`.
#[test]
fn pr10_plan_mode_picker_nav_does_not_dispatch_preview() {
    for nav_key in &[
        KeyCode::Down,
        KeyCode::Char('j'),
        KeyCode::Up,
        KeyCode::Char('k'),
    ] {
        let mut s = make_state();
        navigate_to(&mut s, "plan_mode");
        let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));

        if matches!(nav_key, KeyCode::Up | KeyCode::Char('k')) {
            let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
        }

        let outcome = handle_settings_key(&mut s, &press(*nav_key));
        assert!(
            matches!(outcome, SettingsKeyOutcome::Changed),
            "Nav key {nav_key:?} in plan_mode picker MUST NOT dispatch a preview \
             Action — that would fire an ACP round-trip per keystroke. Got {outcome:?}",
        );
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
    }
}

/// Enter on the focused picker choice commits via
/// `Action::SetPlanMode(PlanModeKind)` — the typed setter, not a
/// preview variant. Pins the canonical-to-PlanModeKind mapping
/// (on→On, off→Off).
#[test]
fn pr10_plan_mode_picker_enter_dispatches_set_commit() {
    use echo_agent_pager::app::actions::PlanModeKind;

    let mut s = make_state();
    navigate_to(&mut s, "plan_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // Default snapshot has plan_mode_active=false → picker seeds at
    // "off". Down nav moves to "on".
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    // Enter → commit.
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetPlanMode(kind)) => {
            assert_eq!(
                kind,
                PlanModeKind::On,
                "Enter must commit `on` → SetPlanMode(PlanModeKind::On)"
            );
        }
        other => panic!("expected Action::SetPlanMode commit, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter commit must return to Browse"
    );
}

/// Esc inside the picker for a non-preview Enum returns to Browse
/// without dispatching any Action. Mirror of
/// `pr9_coding_data_sharing_picker_esc_does_not_dispatch_action`.
/// Since `plan_mode` has no preview, Esc must NOT re-persist.
#[test]
fn pr10_plan_mode_picker_esc_does_not_dispatch_action() {
    let mut s = make_state();
    navigate_to(&mut s, "plan_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));

    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Esc on non-preview Enum picker must NOT emit an Action — \
         doing so would fire an ACP round-trip on every Esc. Got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc must return to Browse"
    );
}

/// Snapshot-driven seeding: `PagerLocalSnapshot { plan_mode_active:
/// true }` makes Enter on the row open the picker seeded at the
/// "on" choice.
#[test]
fn pr10_picker_seeds_choices_idx_from_pager_snapshot_plan_mode_active() {
    let snapshot = PagerLocalSnapshot {
        plan_mode_active: true,
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        snapshot,
    );
    navigate_to(&mut s, "plan_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let reg = SettingsRegistry::defaults();
    let on_idx = match &reg.find("plan_mode").unwrap().kind {
        SettingKind::Enum { choices, .. } => choices
            .iter()
            .position(|c| c.canonical == "on")
            .expect("plan_mode must have 'on' choice"),
        _ => panic!("plan_mode must be Enum"),
    };
    match s.mode() {
        SettingsModalMode::PickingEnum {
            choices_idx,
            ref original_value,
            ..
        } => {
            assert_eq!(
                choices_idx, on_idx,
                "picker must seed at the 'on' index when snapshot says plan_mode_active=true"
            );
            assert_eq!(
                original_value,
                &SettingValue::Enum("on"),
                "original_value must match the live snapshot"
            );
        }
        ref other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// The choices catalog includes EXACTLY "off" and "on" with canonical
/// strings matching the dispatcher contract. Pins the canonical
/// contract — anything else here breaks the `action_for_enum_commit`
/// arm in `views/settings_modal.rs`.
#[test]
fn pr10_plan_mode_choices_use_canonical_strings() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("plan_mode").unwrap();
    let canonicals: Vec<&str> = match &meta.kind {
        SettingKind::Enum { choices, .. } => choices.iter().map(|c| c.canonical).collect(),
        _ => panic!("plan_mode must be Enum"),
    };
    assert_eq!(
        canonicals.len(),
        2,
        "plan_mode catalog must be exactly {{off, on}} — adding a choice requires \
         updating the action_for_enum_commit arm in views/settings_modal.rs AND \
         the action_for_reset arm in dispatch.rs AND PlanModeKind in actions.rs",
    );
    assert!(
        canonicals.contains(&"off"),
        "plan_mode must include 'off' canonical"
    );
    assert!(
        canonicals.contains(&"on"),
        "plan_mode must include 'on' canonical"
    );
}

// ---------------------------------------------------------------------------
// Mouse path tests for plan_mode (keyboard ↔ mouse parity).
//
// Mirrors the permission_mode / coding_data_sharing mouse tests. Every
// keyboard interaction has a mouse equivalent.
// ---------------------------------------------------------------------------

/// First mouse-click on a DIFFERENT (non-selected) `plan_mode` row
/// only SELECTS the row (no picker entry, no Action). Mirrors the
/// two-stage Bool-row select-then-toggle UX.
#[test]
fn pr10_mouse_click_on_unselected_plan_mode_row_only_selects() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "plan_mode") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first body-click on unselected plan_mode row should only select, got: {outcome:?}",
    );
    assert_eq!(s.selected, row_y as usize);
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

/// Second mouse-click on the selected row opens the picker for the
/// Enum row — mirroring the keyboard Enter path.
#[test]
fn pr10_mouse_click_on_selected_plan_mode_row_opens_picker() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "plan_mode") as u16;

    // First click: select.
    let _ = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_eq!(s.selected, row_y as usize);

    // Second click on the focused row: open the picker.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on focused Enum row must open picker, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => {
            assert_eq!(*key, "plan_mode");
        }
        _ => panic!("second click on focused plan_mode row must enter PickingEnum"),
    }
}

/// **Value-column layout.** Value-column click on the
/// plan_mode row opens the picker in ONE click — replaces the
/// previous left-edge indicator hit-rect.
#[test]
fn pr10_mouse_click_on_plan_mode_indicator_opens_picker_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "plan_mode") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "value click must open picker in one click, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => {
            assert_eq!(*key, "plan_mode");
        }
        _ => {
            panic!("value click on plan_mode must enter PickingEnum")
        }
    }
}

// ---------------------------------------------------------------------------
// `render_mermaid` (SHELL-owned Enum, Appearance).
//
// Unlike `plan_mode` (PAGER-owned, snapshot-seeded), `render_mermaid` is
// SHELL-owned: the live value comes from the process-wide cache mirror
// (`appearance::cache::load_render_mermaid`, default `auto`), mirroring how
// `vim_mode` reads its cache. The picker commits the typed
// `Action::SetRenderMermaid(RenderMermaid)` (canonical → enum via
// `RenderMermaid::from_canonical`). `supports_preview: false`, so picker nav
// and Esc must never dispatch an Action.
//
// These tests honor the `ALL_SETTINGS_EXERCISED` contract — keyboard AND
// mouse coverage, same rigor as `plan_mode` / `coding_data_sharing`.
// ---------------------------------------------------------------------------

/// `render_mermaid` lives under `Appearance` and is SHELL-owned (persisted to
/// `[ui].render_mermaid`). Pins the category + owner against drift.
#[test]
fn render_mermaid_renders_under_appearance_category() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("render_mermaid")
        .expect("render_mermaid must be registered");
    assert_eq!(
        meta.category,
        SettingCategory::Appearance,
        "render_mermaid must live under Appearance"
    );
    assert_eq!(
        meta.owner,
        SettingOwner::Shell,
        "render_mermaid is SHELL-owned (persisted to `[ui].render_mermaid`)"
    );
}

/// `render_mermaid` is an Enum with `supports_preview: false` — toggling
/// flips the render path for every Mermaid block, so per-keystroke preview
/// would thrash the scrollback. Commit-on-Enter only.
#[test]
fn render_mermaid_does_not_support_preview() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("render_mermaid")
        .expect("render_mermaid must be registered");
    match &meta.kind {
        SettingKind::Enum {
            supports_preview, ..
        } => assert!(
            !supports_preview,
            "render_mermaid MUST be supports_preview: false",
        ),
        other => panic!("expected Enum kind for render_mermaid, got {other:?}"),
    }
}

/// Enter on the `render_mermaid` row → PickingEnum, seeded to the live cache
/// value. With a fresh cache (no `[ui].render_mermaid` override) the default
/// is `auto`.
#[test]
fn enter_on_render_mermaid_row_enters_picking_enum() {
    let mut s = make_state();
    navigate_to(&mut s, "render_mermaid");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on render_mermaid row must transition to PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(*key, "render_mermaid");
            assert_eq!(
                original_value,
                &SettingValue::Enum("auto"),
                "default cache render_mermaid → original 'auto'"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// **Regression test.** Up/Down/j/k nav in the `render_mermaid` picker MUST
/// NOT dispatch a preview Action — `supports_preview: false`. Mirror of
/// `pr10_plan_mode_picker_nav_does_not_dispatch_preview`.
#[test]
fn render_mermaid_picker_nav_does_not_dispatch_preview() {
    for nav_key in &[
        KeyCode::Down,
        KeyCode::Char('j'),
        KeyCode::Up,
        KeyCode::Char('k'),
    ] {
        let mut s = make_state();
        navigate_to(&mut s, "render_mermaid");
        let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));

        if matches!(nav_key, KeyCode::Up | KeyCode::Char('k')) {
            let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
        }

        let outcome = handle_settings_key(&mut s, &press(*nav_key));
        assert!(
            matches!(outcome, SettingsKeyOutcome::Changed),
            "Nav key {nav_key:?} in render_mermaid picker MUST NOT dispatch a preview \
             Action. Got {outcome:?}",
        );
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
    }
}

/// Enter on the focused picker choice commits via
/// `Action::SetRenderMermaid(RenderMermaid)` — the typed setter. Default
/// seed is `auto` (index 0); one Down moves to `on` (index 1). Pins the
/// canonical-to-RenderMermaid mapping.
#[test]
fn render_mermaid_picker_enter_dispatches_set_commit() {
    use echo_agent_pager::appearance::RenderMermaid;

    let mut s = make_state();
    navigate_to(&mut s, "render_mermaid");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // Fresh cache seeds the picker at "auto"; Down moves to "on".
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetRenderMermaid(kind)) => {
            assert_eq!(
                kind,
                RenderMermaid::On,
                "Enter must commit `on` → SetRenderMermaid(RenderMermaid::On)"
            );
        }
        other => panic!("expected Action::SetRenderMermaid commit, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter commit must return to Browse"
    );
}

/// Esc inside the picker for a non-preview Enum returns to Browse without
/// dispatching any Action. Mirror of
/// `pr10_plan_mode_picker_esc_does_not_dispatch_action`.
#[test]
fn render_mermaid_picker_esc_does_not_dispatch_action() {
    let mut s = make_state();
    navigate_to(&mut s, "render_mermaid");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));

    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Esc on non-preview Enum picker must NOT emit an Action. Got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Esc must return to Browse"
    );
}

/// The choices catalog is EXACTLY {auto, on, off} with canonical strings
/// matching `RenderMermaid::as_canonical` and the `action_for_enum_commit`
/// arm in `views/settings_modal.rs`.
#[test]
fn render_mermaid_choices_use_canonical_strings() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("render_mermaid").unwrap();
    let canonicals: Vec<&str> = match &meta.kind {
        SettingKind::Enum { choices, .. } => choices.iter().map(|c| c.canonical).collect(),
        _ => panic!("render_mermaid must be Enum"),
    };
    assert_eq!(
        canonicals,
        vec!["auto", "on", "off"],
        "render_mermaid catalog must be exactly [auto, on, off] in order — \
         changing it requires updating RenderMermaid::from_canonical AND the \
         action_for_enum_commit arm in views/settings_modal.rs",
    );
}

// ---------------------------------------------------------------------------
// Mouse path tests for render_mermaid (keyboard ↔ mouse
// parity). Mirrors the plan_mode mouse tests.
// ---------------------------------------------------------------------------

/// First mouse-click on a DIFFERENT (non-selected) `render_mermaid` row only
/// SELECTS the row (no picker entry, no Action).
#[test]
fn mouse_click_on_unselected_render_mermaid_row_only_selects() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "render_mermaid") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first body-click on unselected render_mermaid row should only select, got: {outcome:?}",
    );
    assert_eq!(s.selected, row_y as usize);
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

/// Second mouse-click on the selected row opens the picker for the Enum row —
/// mirroring the keyboard Enter path.
#[test]
fn mouse_click_on_selected_render_mermaid_row_opens_picker() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "render_mermaid") as u16;

    let _ = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_eq!(s.selected, row_y as usize);

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on focused Enum row must open picker, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => assert_eq!(*key, "render_mermaid"),
        _ => panic!("second click on focused render_mermaid row must enter PickingEnum"),
    }
}

/// Value-column click on the render_mermaid row opens the picker in ONE click.
#[test]
fn mouse_click_on_render_mermaid_indicator_opens_picker_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "render_mermaid") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "value click must open picker in one click, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => assert_eq!(*key, "render_mermaid"),
        _ => panic!("value click on render_mermaid must enter PickingEnum"),
    }
}

// ---------------------------------------------------------------------------
// screen_mode (SHELL Enum, Appearance, restart_required, no preview).
// Catalog [fullscreen, minimal]; product default when unset is fullscreen.
// Session-only switches stay on /minimal and /fullscreen (do not write config).
// ---------------------------------------------------------------------------

/// Enter on the `screen_mode` row opens the picker seeded at the product
/// default `fullscreen` (UiConfig.screen_mode is None → canonical fullscreen).
#[test]
fn enter_on_screen_mode_row_enters_picking_enum() {
    let mut s = make_state();
    navigate_to(&mut s, "screen_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on screen_mode row must transition to PickingEnum, got {outcome:?}"
    );
    match s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(key, "screen_mode");
            assert_eq!(
                original_value,
                SettingValue::Enum("fullscreen"),
                "default UiConfig screen_mode=None → original 'fullscreen'"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// **Regression test.** Up/Down/j/k nav in the `screen_mode` picker MUST NOT
/// dispatch a preview Action — `supports_preview: false` (restart-required).
#[test]
fn screen_mode_picker_nav_does_not_dispatch_preview() {
    for nav_key in &[
        KeyCode::Down,
        KeyCode::Char('j'),
        KeyCode::Up,
        KeyCode::Char('k'),
    ] {
        let mut s = make_state();
        navigate_to(&mut s, "screen_mode");
        let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));

        if matches!(nav_key, KeyCode::Up | KeyCode::Char('k')) {
            let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
        }

        let outcome = handle_settings_key(&mut s, &press(*nav_key));
        assert!(
            matches!(outcome, SettingsKeyOutcome::Changed),
            "Nav key {nav_key:?} in screen_mode picker MUST NOT dispatch a preview \
             Action. Got {outcome:?}",
        );
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
    }
}

/// Enter on the focused picker choice commits via
/// `Action::SetScreenMode(String)` carrying the registry canonical. Seed is
/// `fullscreen` (index 0); one Down moves to `minimal` (index 1).
#[test]
fn screen_mode_picker_enter_dispatches_set_commit() {
    let mut s = make_state();
    navigate_to(&mut s, "screen_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetScreenMode(mode)) => {
            assert_eq!(
                mode, "minimal",
                "Enter must commit `minimal` → SetScreenMode(\"minimal\")"
            );
        }
        other => panic!("expected Action::SetScreenMode commit, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter commit must return to Browse"
    );
}

/// The choices catalog is EXACTLY {fullscreen, minimal} in order — contract
/// with `canonical_screen_mode` and the settings UI labels.
#[test]
fn screen_mode_choices_use_canonical_strings() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("screen_mode").unwrap();
    let canonicals: Vec<&str> = match &meta.kind {
        SettingKind::Enum { choices, .. } => choices.iter().map(|c| c.canonical).collect(),
        _ => panic!("screen_mode must be Enum"),
    };
    assert_eq!(
        canonicals,
        vec!["fullscreen", "minimal"],
        "screen_mode catalog must be exactly [fullscreen, minimal] in order — \
         changing it requires updating canonical_screen_mode and the chooser",
    );
    match &meta.kind {
        SettingKind::Enum {
            supports_preview, ..
        } => {
            assert!(
                !*supports_preview,
                "screen_mode is restart-required — no live preview"
            );
        }
        _ => unreachable!(),
    }
    assert!(meta.restart_required, "screen_mode requires restart");
}

/// Value-column click on the screen_mode row opens the picker in ONE click
/// (mouse ↔ keyboard parity).
#[test]
fn mouse_click_on_screen_mode_indicator_opens_picker_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "screen_mode") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "value click must open picker in one click, got: {outcome:?}",
    );
    match s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => assert_eq!(key, "screen_mode"),
        _ => panic!("value click on screen_mode must enter PickingEnum"),
    }
}

// ---------------------------------------------------------------------------
// hunk_tracker_mode (SHELL Enum, Advanced, restart_required, no preview).
// Catalog [agent_only, all_dirty, off]; `disabled` aliases `off` at parse
// time. Mirrors the render_mermaid enum tests (keyboard ↔ mouse parity).
// ---------------------------------------------------------------------------

/// Enter on the `hunk_tracker_mode` row opens the picker seeded at the
/// default `off`.
#[test]
fn enter_on_hunk_tracker_mode_row_enters_picking_enum() {
    let mut s = make_state();
    navigate_to(&mut s, "hunk_tracker_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on hunk_tracker_mode row must transition to PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(*key, "hunk_tracker_mode");
            assert_eq!(
                original_value,
                &SettingValue::Enum("off"),
                "default UiConfig hunk_tracker_mode → original 'off'"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// **Regression test.** Up/Down/j/k nav in the `hunk_tracker_mode` picker MUST
/// NOT dispatch a preview Action — `supports_preview: false`. Mirror of
/// `render_mermaid_picker_nav_does_not_dispatch_preview`.
#[test]
fn hunk_tracker_mode_picker_nav_does_not_dispatch_preview() {
    for nav_key in &[
        KeyCode::Down,
        KeyCode::Char('j'),
        KeyCode::Up,
        KeyCode::Char('k'),
    ] {
        let mut s = make_state();
        navigate_to(&mut s, "hunk_tracker_mode");
        let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));

        // Seed is `off` (last choice), so step off the bottom before a Down/j.
        if matches!(nav_key, KeyCode::Down | KeyCode::Char('j')) {
            let _ = handle_settings_key(&mut s, &press(KeyCode::Up));
        }

        let outcome = handle_settings_key(&mut s, &press(*nav_key));
        assert!(
            matches!(outcome, SettingsKeyOutcome::Changed),
            "Nav key {nav_key:?} in hunk_tracker_mode picker MUST NOT dispatch a preview \
             Action. Got {outcome:?}",
        );
        assert!(matches!(s.mode(), SettingsModalMode::PickingEnum { .. }));
    }
}

/// Enter on the focused picker choice commits via
/// `Action::SetHunkTrackerMode(String)` carrying the registry canonical. Seed
/// is `off` (index 2); one Up moves to `all_dirty` (index 1). Pins the
/// canonical-string payload that `action_for_enum_commit` forwards.
#[test]
fn hunk_tracker_mode_picker_enter_dispatches_set_commit() {
    let mut s = make_state();
    navigate_to(&mut s, "hunk_tracker_mode");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // Fresh state seeds the picker at "off"; Up moves to "all_dirty".
    let _ = handle_settings_key(&mut s, &press(KeyCode::Up));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetHunkTrackerMode(mode)) => {
            assert_eq!(
                mode, "all_dirty",
                "Enter must commit `all_dirty` → SetHunkTrackerMode(\"all_dirty\")"
            );
        }
        other => panic!("expected Action::SetHunkTrackerMode commit, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter commit must return to Browse"
    );
}

/// The choices catalog is EXACTLY {agent_only, all_dirty, off} in order. The
/// commit forwards `choice.to_string()` verbatim, so these canonicals are the
/// contract with the shell-side `canonical_hunk_tracker_mode` parser.
#[test]
fn hunk_tracker_mode_choices_use_canonical_strings() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("hunk_tracker_mode").unwrap();
    let canonicals: Vec<&str> = match &meta.kind {
        SettingKind::Enum { choices, .. } => choices.iter().map(|c| c.canonical).collect(),
        _ => panic!("hunk_tracker_mode must be Enum"),
    };
    assert_eq!(
        canonicals,
        vec!["agent_only", "all_dirty", "off"],
        "hunk_tracker_mode catalog must be exactly [agent_only, all_dirty, off] in order — \
         changing it requires updating the shell-side canonical_hunk_tracker_mode parser",
    );
}

/// Value-column click on the hunk_tracker_mode row opens the picker in ONE
/// click (mouse ↔ keyboard parity).
#[test]
fn mouse_click_on_hunk_tracker_mode_indicator_opens_picker_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "hunk_tracker_mode") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "value click must open picker in one click, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => assert_eq!(*key, "hunk_tracker_mode"),
        _ => panic!("value click on hunk_tracker_mode must enter PickingEnum"),
    }
}

// ---------------------------------------------------------------------------
// voice_stt_language (SHELL Enum, Editor)
// ---------------------------------------------------------------------------

/// Enter on the voice_stt_language row opens the picker seeded at the
/// default `en`.
#[test]
fn enter_on_voice_stt_language_row_enters_picking_enum() {
    let mut s = make_state();
    navigate_to(&mut s, "voice_stt_language");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on voice_stt_language row must transition to PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(*key, "voice_stt_language");
            assert_eq!(
                original_value,
                &SettingValue::Enum("en"),
                "default UiConfig voice_stt_language → original 'en'"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

/// Enter on a picker choice commits via `Action::SetVoiceSttLanguage(String)`
/// carrying the canonical code. Seed is `en` (index 0); one Down moves to
/// `auto` (System).
#[test]
fn voice_stt_language_picker_enter_dispatches_set_commit() {
    let mut s = make_state();
    navigate_to(&mut s, "voice_stt_language");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetVoiceSttLanguage(code)) => {
            assert_eq!(code, "auto", "second choice is System (`auto`)");
        }
        other => panic!("expected Action::SetVoiceSttLanguage commit, got {other:?}"),
    }
    assert!(
        matches!(s.mode(), SettingsModalMode::Browse),
        "Enter commit must return to Browse"
    );
}

/// Space-toggle on `voice_keybind_enabled` dispatches the typed setter.
/// Default is ON (the chord works out of the box), so toggling flips it off.
#[test]
fn space_on_voice_keybind_enabled_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "voice_keybind_enabled");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "voice_keybind_enabled", false);
}

/// Value-column click toggles `voice_keybind_enabled` in one click.
#[test]
fn mouse_click_on_voice_keybind_enabled_indicator_toggles_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "voice_keybind_enabled") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert_set_bool_action(outcome, "voice_keybind_enabled", false);
}

/// Value-column click on the voice_stt_language row opens the picker in ONE
/// click (mouse ↔ keyboard parity).
#[test]
fn mouse_click_on_voice_stt_language_indicator_opens_picker_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "voice_stt_language") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "value click must open picker in one click, got: {outcome:?}",
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => assert_eq!(*key, "voice_stt_language"),
        _ => panic!("value click on voice_stt_language must enter PickingEnum"),
    }
}

// ---------------------------------------------------------------------------
// CLI batch: show_tips, auto_update (SHELL Bool, restart_required)
// ---------------------------------------------------------------------------

/// Space-toggle on `show_tips` dispatches typed setter.
#[test]
fn pr13_space_on_show_tips_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "show_tips");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "show_tips", false);
}

#[test]
fn pr13_space_on_auto_update_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "auto_update");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "auto_update", false);
}

/// Value-column click on `show_tips` toggles in one click.
#[test]
fn pr13_mouse_click_on_show_tips_indicator_toggles_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "show_tips") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        72,
        row_y,
    );
    assert_set_bool_action(outcome, "show_tips", false);
}

/// Two-stage select-then-toggle on `auto_update`.
#[test]
fn pr13_mouse_click_on_auto_update_two_stage_select_then_toggle() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "auto_update") as u16;

    // First click: select-only (the focused row was compact_mode).
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first click on a different row body should only select, got: {outcome:?}"
    );

    // Second click on the SAME row should now toggle.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_set_bool_action(outcome, "auto_update", false);
}

/// CLI-batch settings are all `restart_required: true`.
#[test]
fn pr13_cli_batch_all_settings_are_restart_required() {
    let reg = SettingsRegistry::defaults();
    for key in ["show_tips", "auto_update"] {
        let meta = reg
            .find(key)
            .unwrap_or_else(|| panic!("registry must contain `{key}` (PR 13)"));
        assert!(
            meta.restart_required,
            "PR-13 setting `{key}` must have restart_required: true \
             (consumer reads value once at startup; the modal renders \
             the restart pill when this row is expanded)",
        );
        // Sanity: these CLI-batch settings are all SHELL-owned Bool.
        assert_eq!(meta.owner, SettingOwner::Shell);
        assert!(matches!(meta.kind, SettingKind::Bool { .. }));
    }
}

/// CLI-batch defaults round-trip through `current_value_for`.
#[test]
fn pr13_cli_batch_defaults_roundtrip_via_current_value_for() {
    use echo_agent_pager::settings::current_value_for;
    let ui = UiConfig::default();
    let pager = PagerLocalSnapshot::default();
    for (key, expected) in [("show_tips", true), ("auto_update", true)] {
        let value = current_value_for(key, &ui, &pager)
            .unwrap_or_else(|| panic!("current_value_for(`{key}`) must resolve"));
        assert_eq!(
            value,
            SettingValue::Bool(expected),
            "PR 13: `{key}` defaults to {expected} (matches the consumer's \
             .unwrap_or(...) at the original read site)",
        );
    }
}

/// CLI-batch settings are discoverable via search.
#[test]
fn pr13_cli_batch_settings_are_discoverable_via_search() {
    let reg = SettingsRegistry::defaults();
    let cases = [("tip", "show_tips"), ("auto-update", "auto_update")];
    for (query, expected_key) in cases {
        let hits = reg.search(query);
        assert!(
            hits.iter().any(|m| m.key == expected_key),
            "search(`{query}`) must include `{expected_key}` — hit keys: {:?}",
            hits.iter().map(|m| m.key).collect::<Vec<_>>(),
        );
    }
}

// ---------------------------------------------------------------------------
// fork_secondary_model (DynamicEnum, restart_required: false)
// ---------------------------------------------------------------------------

/// `fork_secondary_model` lives under Models.
#[test]
fn pr14_model_family_renders_under_models_category() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("fork_secondary_model")
        .expect("`fork_secondary_model` must be registered");
    assert_eq!(
        meta.category,
        SettingCategory::Models,
        "`fork_secondary_model` must live under Models"
    );
    assert_eq!(
        meta.owner,
        SettingOwner::Shell,
        "`fork_secondary_model` is SHELL-owned (persisted via util::config)"
    );
}

/// `fork_secondary_model` is `restart_required: false`.
#[test]
fn pr14_restart_required_split() {
    let reg = SettingsRegistry::defaults();
    let fork = reg.find("fork_secondary_model").unwrap();
    assert!(
        !fork.restart_required,
        "fork_secondary_model must be restart_required: false — the shell's \
         config_reloader rebroadcasts the new default on the next fork"
    );
}

/// Model settings use `DynamicEnum` with `ActiveModelCatalog`.
#[test]
fn pr14_string_settings_use_known_model_validator() {
    use echo_agent_pager::settings::DynamicEnumSource;
    let reg = SettingsRegistry::defaults();
    for key in ["default_model", "fork_secondary_model"] {
        let meta = reg
            .find(key)
            .unwrap_or_else(|| panic!("`{key}` must be registered"));
        match &meta.kind {
            SettingKind::DynamicEnum { source, .. } => {
                assert_eq!(
                    *source,
                    DynamicEnumSource::ActiveModelCatalog,
                    "`{key}` must pull choices from the active model catalog \
                     so the picker matches `/model`'s UX"
                );
            }
            other => panic!("expected DynamicEnum kind for `{key}`, got {other:?}"),
        }
    }
}

/// Defaults round-trip through `current_value_for`.
#[test]
fn pr14_model_family_defaults_roundtrip_via_current_value_for() {
    use echo_agent_pager::settings::current_value_for;
    let ui = UiConfig::default();
    let pager = PagerLocalSnapshot::default();

    // Baseline value folds to empty (no-opinion sentinel).
    let value = current_value_for("fork_secondary_model", &ui, &pager).unwrap();
    assert_eq!(
        value,
        SettingValue::String(String::new()),
        "PR 14: `fork_secondary_model` defaults to empty string (no-opinion sentinel)",
    );
}

/// Non-baseline `fork_secondary_model` surfaces verbatim.
#[test]
fn pr14_fork_secondary_model_reads_ui_config_non_baseline() {
    use echo_agent_pager::settings::current_value_for;
    let ui = UiConfig {
        fork_secondary_model: "Custom Fork Model".to_string(),
        ..UiConfig::default()
    };
    let pager = PagerLocalSnapshot::default();
    assert_eq!(
        current_value_for("fork_secondary_model", &ui, &pager),
        Some(SettingValue::String("Custom Fork Model".to_string())),
        "non-baseline `fork_secondary_model` must surface verbatim — the \
         baseline-equality fold only kicks in for the default value",
    );
}

/// Model-family settings are discoverable via search.
#[test]
fn pr14_model_family_settings_are_discoverable_via_search() {
    let reg = SettingsRegistry::defaults();
    let cases = [("fork", "fork_secondary_model")];
    for (query, expected_key) in cases {
        let hits = reg.search(query);
        assert!(
            hits.iter().any(|m| m.key == expected_key),
            "search(`{query}`) must include `{expected_key}` — hit keys: {:?}",
            hits.iter().map(|m| m.key).collect::<Vec<_>>(),
        );
    }
}

// ---------------------------------------------------------------------------
// vim_mode (scrollback navigation) — PAGER-owned, paired with simple_mode
// ---------------------------------------------------------------------------

/// Keyboard Space on the vim_mode row dispatches the typed setter
/// with the inverted snapshot value (default false → true). Same
/// shape as the `multiline_mode` test above; both rows are
/// PAGER-owned Bool settings.
#[test]
fn vim_mode_space_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "vim_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "vim_mode", true);
}

#[test]
fn vim_mode_enter_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "vim_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert_set_bool_action(outcome, "vim_mode", true);
}

#[test]
fn vim_mode_mouse_click_two_stage_toggles() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "vim_mode") as u16;

    // First click: select-only.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first click on a different row body should only select, got: {outcome:?}"
    );
    assert_eq!(s.selected, row_y as usize);

    // Second click: toggle.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_set_bool_action(outcome, "vim_mode", true);
}

#[test]
fn vim_mode_snapshot_on_dispatches_off() {
    let snapshot = PagerLocalSnapshot {
        vim_mode: true,
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        snapshot,
    );
    navigate_to(&mut s, "vim_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "vim_mode", false);
}

#[test]
fn vim_mode_renders_under_appearance_category_pager_owned() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("vim_mode").expect("vim_mode must be registered");
    assert_eq!(
        meta.category,
        SettingCategory::Appearance,
        "vim_mode lives under Appearance (paired with simple_mode)"
    );
    assert_eq!(meta.owner, SettingOwner::Shell, "vim_mode is SHELL-owned");
}

#[test]
fn simple_mode_label_distinguishes_input_from_scrollback() {
    // The pair (`simple_mode`, `vim_mode`) controls vim behaviour in
    // two different surfaces. The label rename in this rebase makes
    // that explicit so neither row is ambiguous when both are shown.
    let reg = SettingsRegistry::defaults();
    let simple = reg.find("simple_mode").expect("simple_mode registered");
    let vim = reg.find("vim_mode").expect("vim_mode registered");
    assert_eq!(simple.label, "Disable vim input mode");
    assert_eq!(vim.label, "Vim scrollback navigation");
    // Keyword sanity-check so search('vim') still finds both.
    assert!(simple.keywords.contains(&"vim"));
    assert!(vim.keywords.contains(&"vim"));
}

// ---------------------------------------------------------------------------
// keep_text_selection — SHELL-owned Mouse Enum (`flash` | `hold`)
//
// Mirrors `render_mermaid`: `supports_preview: false`, Enter opens picker,
// commit dispatches `Action::SetKeepTextSelection(TextSelection)`.
// ---------------------------------------------------------------------------

#[test]
fn keep_text_selection_renders_under_mouse_shell_owned() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("keep_text_selection")
        .expect("keep_text_selection must be registered");
    assert_eq!(meta.category, SettingCategory::Mouse);
    assert_eq!(meta.owner, SettingOwner::Shell);
    assert_eq!(meta.label, "Text selection");
    assert!(
        meta.description.contains("Shift"),
        "description should mention Shift-drag for native terminal copy"
    );
}

#[test]
fn keep_text_selection_does_not_support_preview() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("keep_text_selection")
        .expect("keep_text_selection must be registered");
    match &meta.kind {
        SettingKind::Enum {
            supports_preview, ..
        } => assert!(
            !supports_preview,
            "keep_text_selection MUST be supports_preview: false",
        ),
        other => panic!("expected Enum kind for keep_text_selection, got {other:?}"),
    }
}

#[test]
fn enter_on_keep_text_selection_row_enters_picking_enum() {
    // The picker's `original_value` is read from the process-wide cache; pin it
    // to the default so a sibling test's `set_keep_text_selection` can't leak in.
    echo_agent_pager::appearance::cache::set_keep_text_selection(
        echo_agent_pager::appearance::TextSelection::Flash,
    );
    let mut s = make_state();
    navigate_to(&mut s, "keep_text_selection");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on keep_text_selection row must transition to PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(*key, "keep_text_selection");
            assert_eq!(
                original_value,
                &SettingValue::Enum("flash"),
                "default keep_text_selection → original 'flash'"
            );
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
}

#[test]
fn keep_text_selection_picker_nav_does_not_dispatch_preview() {
    // Pin the cache-backed live value so the picker seeds at flash (idx 0)
    // regardless of a sibling test that set hold/word_select on this thread.
    // (word_select is the last choice, so Down would clamp; flash gives room.)
    echo_agent_pager::appearance::cache::set_keep_text_selection(
        echo_agent_pager::appearance::TextSelection::Flash,
    );
    for nav_key in &[
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::Char('j'),
        KeyCode::Char('k'),
    ] {
        let mut s = make_state();
        navigate_to(&mut s, "keep_text_selection");
        let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
        // Retreat keys at choices_idx=0 are clamped → Unchanged; step down first.
        if matches!(nav_key, KeyCode::Up | KeyCode::Char('k')) {
            let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
        }
        let outcome = handle_settings_key(&mut s, &press(*nav_key));
        assert!(
            matches!(outcome, SettingsKeyOutcome::Changed),
            "Nav key {nav_key:?} in keep_text_selection picker MUST NOT dispatch a preview \
             Action (supports_preview: false), got {outcome:?}"
        );
    }
}

#[test]
fn keep_text_selection_picker_enter_dispatches_set_commit() {
    use echo_agent_pager::appearance::TextSelection;

    // Pin the cache-backed live value so the picker seeds at flash (idx 0)
    // regardless of any sibling test that set hold/word_select on this thread.
    echo_agent_pager::appearance::cache::set_keep_text_selection(TextSelection::Flash);
    let mut s = make_state();
    navigate_to(&mut s, "keep_text_selection");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    // flash (idx 0); Down → hold (idx 1), Enter commits.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetKeepTextSelection(kind)) => {
            assert_eq!(kind, TextSelection::Hold);
        }
        other => panic!("expected SetKeepTextSelection(Hold), got {other:?}"),
    }
}

#[test]
fn keep_text_selection_picker_esc_does_not_dispatch_action() {
    let mut s = make_state();
    navigate_to(&mut s, "keep_text_selection");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Esc));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Esc with supports_preview: false must not dispatch Action, got {outcome:?}"
    );
}

#[test]
fn keep_text_selection_choices_use_canonical_strings() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("keep_text_selection").unwrap();
    let SettingKind::Enum { choices, .. } = &meta.kind else {
        panic!("keep_text_selection must be Enum");
    };
    let canonicals: Vec<_> = choices.iter().map(|c| c.canonical).collect();
    assert_eq!(
        canonicals,
        vec!["flash", "hold", "word_select"],
        "keep_text_selection catalog must be exactly [flash, hold, word_select] in order"
    );
}

#[test]
fn mouse_click_on_unselected_keep_text_selection_row_only_selects() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "keep_text_selection") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first body-click on unselected keep_text_selection row should only select, got: {outcome:?}",
    );
    assert_eq!(s.selected, row_y as usize);
}

#[test]
fn mouse_click_on_selected_keep_text_selection_row_opens_picker() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "keep_text_selection") as u16;
    let _ = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on focused keep_text_selection row must enter PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => assert_eq!(*key, "keep_text_selection"),
        _ => panic!("second click on focused keep_text_selection row must enter PickingEnum"),
    }
}

#[test]
fn mouse_click_on_keep_text_selection_indicator_opens_picker_in_one_click() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "keep_text_selection") as u16;
    // Value column is on the right; use a high x to hit the indicator.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        70,
        row_y,
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => assert_eq!(*key, "keep_text_selection"),
        _ => panic!("value click on keep_text_selection must enter PickingEnum, got {outcome:?}"),
    }
}

#[test]
fn keep_text_selection_hold_snapshot_seeds_picker_at_hold() {
    use echo_agent_pager::appearance::TextSelection;
    // Live value is the process-wide cache (like render_mermaid), not UiConfig alone.
    echo_agent_pager::appearance::cache::set_keep_text_selection(TextSelection::Hold);
    let ui = UiConfig {
        keep_text_selection: Some("hold".into()),
        ..UiConfig::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        ui,
        PagerLocalSnapshot::default(),
    );
    navigate_to(&mut s, "keep_text_selection");
    let _ = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            original_value,
            choices_idx,
            ..
        } => {
            assert_eq!(original_value, &SettingValue::Enum("hold"));
            assert_eq!(*choices_idx, 1, "hold is the second choice");
        }
        other => panic!("expected PickingEnum, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// scroll_speed — SHELL-owned Int under Mouse, no preview
// ---------------------------------------------------------------------------

/// Int stepper open/step/commit for scroll_speed. Defaults to 50;
/// mid-range policy: Up/Down ±1, Left/Right ±5.
#[test]
fn scroll_speed_int_stepper_commit_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "scroll_speed");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on Int row must transition to EditingValue, got {outcome:?}"
    );
    assert_eq!(
        s.editing_buffer(),
        Some("50"),
        "buffer must seed from default 50",
    );

    // Up = +1 → 51. Right = +5 → 56.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Up));
    let _ = handle_settings_key(&mut s, &press(KeyCode::Right));
    assert_eq!(s.editing_buffer(), Some("56"));

    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetScrollSpeed(56)) => {}
        other => panic!("expected SetScrollSpeed(56), got {other:?}"),
    }
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

#[test]
fn scroll_speed_mouse_click_opens_editor() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "scroll_speed") as u16;

    // First click — select.
    let _ = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    // Second click on the focused row opens the editor.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on focused Int row must enter the editor, got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::EditingValue { key, .. } if key == "scroll_speed"),
        "mode must be EditingValue(scroll_speed) after Enter-equivalent click, got {:?}",
        s.mode(),
    );
}

#[test]
fn scroll_speed_renders_under_mouse_shell_owned_bounds_1_to_100() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("scroll_speed")
        .expect("scroll_speed must be registered");
    assert_eq!(meta.category, SettingCategory::Mouse);
    assert_eq!(meta.owner, SettingOwner::Shell);
    match &meta.kind {
        SettingKind::Int { default, min, max } => {
            assert_eq!(*default, 50);
            assert_eq!(*min, 1);
            assert_eq!(*max, 100);
        }
        other => panic!("expected Int kind for scroll_speed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// scroll_mode — SHELL-owned Mouse Enum (`auto` | `wheel` | `trackpad`),
// no preview (mirrors keep_text_selection).
// ---------------------------------------------------------------------------

#[test]
fn scroll_mode_renders_under_mouse_shell_owned_no_preview() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("scroll_mode").expect("scroll_mode registered");
    assert_eq!(meta.category, SettingCategory::Mouse);
    assert_eq!(meta.owner, SettingOwner::Shell);
    match &meta.kind {
        SettingKind::Enum {
            default,
            choices,
            supports_preview,
        } => {
            assert_eq!(*default, "auto");
            assert!(!supports_preview, "scroll_mode must not preview");
            let canonicals: Vec<_> = choices.iter().map(|c| c.canonical).collect();
            assert_eq!(
                canonicals,
                vec!["auto", "wheel", "trackpad"],
                "catalog must match ScrollMode::as_canonical order"
            );
        }
        other => panic!("expected Enum kind for scroll_mode, got {other:?}"),
    }
}

#[test]
fn scroll_mode_picker_enter_dispatches_set_commit() {
    use echo_agent_pager::appearance::ScrollMode;

    // Pin the cache-backed live value so the picker seeds at auto (idx 0)
    // regardless of sibling tests on this thread.
    echo_agent_pager::appearance::cache::set_scroll_mode(ScrollMode::Auto);
    let mut s = make_state();
    navigate_to(&mut s, "scroll_mode");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on scroll_mode row must transition to PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum {
            key,
            original_value,
            ..
        } => {
            assert_eq!(*key, "scroll_mode");
            assert_eq!(original_value, &SettingValue::Enum("auto"));
        }
        other => panic!("expected PickingEnum mode, got {other:?}"),
    }
    // auto (idx 0); Down → wheel (idx 1), Enter commits the typed setter.
    let _ = handle_settings_key(&mut s, &press(KeyCode::Down));
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetScrollMode(mode)) => {
            assert_eq!(mode, ScrollMode::Wheel);
        }
        other => panic!("expected SetScrollMode(Wheel), got {other:?}"),
    }
}

#[test]
fn mouse_click_on_selected_scroll_mode_row_opens_picker() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "scroll_mode") as u16;
    // First click selects, second opens the picker.
    let _ = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on focused scroll_mode row must enter PickingEnum, got {outcome:?}"
    );
    match &s.mode() {
        SettingsModalMode::PickingEnum { key, .. } => assert_eq!(*key, "scroll_mode"),
        _ => panic!("second click on focused scroll_mode row must enter PickingEnum"),
    }
}

// ---------------------------------------------------------------------------
// scroll_lines — SHELL-owned Int under Mouse (1-10), no preview
// ---------------------------------------------------------------------------

#[test]
fn scroll_lines_renders_under_mouse_shell_owned_bounds_1_to_10() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("scroll_lines").expect("scroll_lines registered");
    assert_eq!(meta.category, SettingCategory::Mouse);
    assert_eq!(meta.owner, SettingOwner::Shell);
    match &meta.kind {
        SettingKind::Int { default, min, max } => {
            assert_eq!(*default, 3);
            assert_eq!(*min, 1);
            assert_eq!(*max, 10);
        }
        other => panic!("expected Int kind for scroll_lines, got {other:?}"),
    }
}

/// Int stepper open/step/commit for scroll_lines (the scroll_speed pattern).
#[test]
fn scroll_lines_int_stepper_commit_dispatches_typed_setter() {
    // Pin the live cache so the buffer seeds at the default 3.
    echo_agent_pager::appearance::cache::set_scroll_lines(3);
    let mut s = make_state();
    navigate_to(&mut s, "scroll_lines");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "Enter on Int row must transition to EditingValue, got {outcome:?}"
    );
    assert_eq!(
        s.editing_buffer(),
        Some("3"),
        "buffer must seed from default 3",
    );

    // Narrow-range policy: Up = +1 → 4 (unit steps so every 1..=10 is reachable).
    let _ = handle_settings_key(&mut s, &press(KeyCode::Up));
    assert_eq!(s.editing_buffer(), Some("4"));

    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    match outcome {
        SettingsKeyOutcome::Action(Action::SetScrollLines(4)) => {}
        other => panic!("expected SetScrollLines(4), got {other:?}"),
    }
    assert!(matches!(s.mode(), SettingsModalMode::Browse));
}

#[test]
fn scroll_lines_mouse_click_opens_editor() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "scroll_lines") as u16;
    let _ = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "second click on focused Int row must enter the editor, got {outcome:?}"
    );
    assert!(
        matches!(s.mode(), SettingsModalMode::EditingValue { key, .. } if key == "scroll_lines"),
        "mode must be EditingValue(scroll_lines), got {:?}",
        s.mode(),
    );
}

// ---------------------------------------------------------------------------
// invert_scroll — SHELL-owned Bool (Mouse, default false)
// ---------------------------------------------------------------------------

#[test]
fn invert_scroll_space_dispatches_typed_setter() {
    echo_agent_pager::appearance::cache::set_invert_scroll(false);
    let mut s = make_state();
    navigate_to(&mut s, "invert_scroll");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "invert_scroll", true);
    echo_agent_pager::appearance::cache::set_invert_scroll(false);
}

#[test]
fn invert_scroll_enter_dispatches_typed_setter() {
    echo_agent_pager::appearance::cache::set_invert_scroll(true);
    let mut s = make_state();
    navigate_to(&mut s, "invert_scroll");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert_set_bool_action(outcome, "invert_scroll", false);
    echo_agent_pager::appearance::cache::set_invert_scroll(false);
}

#[test]
fn invert_scroll_mouse_click_two_stage_toggles() {
    echo_agent_pager::appearance::cache::set_invert_scroll(false);
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "invert_scroll") as u16;
    // First click — select only.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first body-click should only select, got {outcome:?}"
    );
    // Second click — toggles.
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_set_bool_action(outcome, "invert_scroll", true);
    echo_agent_pager::appearance::cache::set_invert_scroll(false);
}

#[test]
fn invert_scroll_renders_under_mouse_shell_owned_default_false() {
    let reg = SettingsRegistry::defaults();
    let meta = reg.find("invert_scroll").expect("invert_scroll registered");
    assert_eq!(meta.category, SettingCategory::Mouse);
    assert_eq!(meta.owner, SettingOwner::Shell);
    match &meta.kind {
        SettingKind::Bool { default } => assert!(!default, "invert_scroll must default OFF"),
        other => panic!("expected Bool kind for invert_scroll, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// display_refresh_auto_cadence — SHELL-owned Bool (Appearance, default ON)
// ---------------------------------------------------------------------------

#[test]
fn display_refresh_auto_cadence_space_dispatches_typed_setter() {
    // Default is on; space toggles off.
    let mut s = make_state();
    navigate_to(&mut s, "display_refresh_auto_cadence");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "display_refresh_auto_cadence", false);
}

#[test]
fn display_refresh_auto_cadence_enter_dispatches_typed_setter() {
    // Seed off so Enter toggles on.
    let mut ui = UiConfig::default();
    ui.display_refresh.auto_cadence_enabled = Some(false);
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        ui,
        PagerLocalSnapshot {
            auto_mode_gate: true,
            ..PagerLocalSnapshot::default()
        },
    );
    navigate_to(&mut s, "display_refresh_auto_cadence");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert_set_bool_action(outcome, "display_refresh_auto_cadence", true);
}

#[test]
fn display_refresh_auto_cadence_mouse_click_two_stage_toggles() {
    // Default is on; second body-click toggles off.
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "display_refresh_auto_cadence") as u16;
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first body-click should only select, got {outcome:?}"
    );
    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_set_bool_action(outcome, "display_refresh_auto_cadence", false);
}

#[test]
fn display_refresh_auto_cadence_meta_appearance_shell_restart_hidden_minimal() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("display_refresh_auto_cadence")
        .expect("display_refresh_auto_cadence registered");
    assert_eq!(meta.category, SettingCategory::Appearance);
    assert_eq!(meta.owner, SettingOwner::Shell);
    assert!(meta.restart_required);
    assert!(meta.hidden_in_minimal);
    assert_eq!(meta.label, "Match display refresh rate");
    match &meta.kind {
        SettingKind::Bool { default } => {
            assert!(*default, "display_refresh_auto_cadence must default ON")
        }
        other => panic!("expected Bool kind for display_refresh_auto_cadence, got {other:?}"),
    }
}

#[test]
fn display_refresh_auto_cadence_defaults_roundtrip_via_current_value_for() {
    use echo_agent_pager::settings::current_value_for;
    let ui = UiConfig::default();
    let pager = PagerLocalSnapshot::default();
    let value = current_value_for("display_refresh_auto_cadence", &ui, &pager)
        .expect("current_value_for(display_refresh_auto_cadence) must resolve");
    assert_eq!(value, SettingValue::Bool(true));

    let mut ui_off = UiConfig::default();
    ui_off.display_refresh.auto_cadence_enabled = Some(false);
    let value = current_value_for("display_refresh_auto_cadence", &ui_off, &pager)
        .expect("current_value_for(display_refresh_auto_cadence) must resolve");
    assert_eq!(value, SettingValue::Bool(false));
}

// ---------------------------------------------------------------------------
// show_thinking_blocks — SHELL-owned Bool (Appearance, default true)
// ---------------------------------------------------------------------------

#[test]
fn show_thinking_blocks_space_dispatches_typed_setter() {
    // Pin off so space toggles to true.
    echo_agent_pager::appearance::cache::set_show_thinking_blocks(false);
    let mut s = make_state();
    navigate_to(&mut s, "show_thinking_blocks");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "show_thinking_blocks", true);
    echo_agent_pager::appearance::cache::set_show_thinking_blocks(true);
}

#[test]
fn show_thinking_blocks_enter_dispatches_typed_setter() {
    echo_agent_pager::appearance::cache::set_show_thinking_blocks(false);
    let mut s = make_state();
    navigate_to(&mut s, "show_thinking_blocks");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert_set_bool_action(outcome, "show_thinking_blocks", true);
    echo_agent_pager::appearance::cache::set_show_thinking_blocks(true);
}

#[test]
fn show_thinking_blocks_mouse_click_two_stage_toggles() {
    echo_agent_pager::appearance::cache::set_show_thinking_blocks(false);
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "show_thinking_blocks") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first click on a different row body should only select, got: {outcome:?}"
    );
    assert_eq!(s.selected, row_y as usize);

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    // Cache pinned off above → toggle dispatches true.
    assert_set_bool_action(outcome, "show_thinking_blocks", true);
    echo_agent_pager::appearance::cache::set_show_thinking_blocks(true);
}

#[test]
fn show_thinking_blocks_cache_on_dispatches_off() {
    // When the live cache is on, toggle should turn it off.
    echo_agent_pager::appearance::cache::set_show_thinking_blocks(true);
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    navigate_to(&mut s, "show_thinking_blocks");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "show_thinking_blocks", false);
    // Restore client default (on) for other tests that share the process cache.
    echo_agent_pager::appearance::cache::set_show_thinking_blocks(true);
}

#[test]
fn show_thinking_blocks_renders_under_appearance_category_shell_owned() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("show_thinking_blocks")
        .expect("show_thinking_blocks must be registered");
    assert_eq!(meta.category, SettingCategory::Appearance);
    assert_eq!(meta.owner, SettingOwner::Shell);
    match &meta.kind {
        SettingKind::Bool { default } => assert!(*default, "default must be true"),
        other => panic!("expected Bool kind for show_thinking_blocks, got {other:?}"),
    }
    // Must sit immediately above respect_manual_folds in the registry order.
    let keys: Vec<&str> = reg
        .all()
        .iter()
        .filter(|m| m.category == SettingCategory::Appearance)
        .map(|m| m.key)
        .collect();
    let show_idx = keys
        .iter()
        .position(|k| *k == "show_thinking_blocks")
        .expect("show_thinking_blocks in Appearance");
    let respect_idx = keys
        .iter()
        .position(|k| *k == "respect_manual_folds")
        .expect("respect_manual_folds in Appearance");
    assert_eq!(
        show_idx + 1,
        respect_idx,
        "show_thinking_blocks must be immediately above respect_manual_folds; \
         Appearance order: {keys:?}"
    );
}

// ---------------------------------------------------------------------------
// prompt_suggestions — SHELL-owned Bool (Editor, default true)
// ---------------------------------------------------------------------------

#[test]
fn prompt_suggestions_space_dispatches_typed_setter() {
    // Pin off so space toggles to true.
    echo_agent_pager::appearance::cache::set_prompt_suggestions(false);
    let mut s = make_state();
    navigate_to(&mut s, "prompt_suggestions");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "prompt_suggestions", true);
    echo_agent_pager::appearance::cache::set_prompt_suggestions(true);
}

#[test]
fn prompt_suggestions_enter_dispatches_typed_setter() {
    echo_agent_pager::appearance::cache::set_prompt_suggestions(false);
    let mut s = make_state();
    navigate_to(&mut s, "prompt_suggestions");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert_set_bool_action(outcome, "prompt_suggestions", true);
    echo_agent_pager::appearance::cache::set_prompt_suggestions(true);
}

#[test]
fn prompt_suggestions_mouse_click_two_stage_toggles() {
    echo_agent_pager::appearance::cache::set_prompt_suggestions(false);
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "prompt_suggestions") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first click on a different row body should only select, got: {outcome:?}"
    );
    assert_eq!(s.selected, row_y as usize);

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    // Cache pinned off above → toggle dispatches true.
    assert_set_bool_action(outcome, "prompt_suggestions", true);
    echo_agent_pager::appearance::cache::set_prompt_suggestions(true);
}

#[test]
fn prompt_suggestions_cache_on_dispatches_off() {
    // Default is on; when the live cache is on, toggle should turn it off.
    echo_agent_pager::appearance::cache::set_prompt_suggestions(true);
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    navigate_to(&mut s, "prompt_suggestions");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "prompt_suggestions", false);
    // Restore client default (on) for other tests that share the process cache.
    echo_agent_pager::appearance::cache::set_prompt_suggestions(true);
}

#[test]
fn prompt_suggestions_renders_under_editor_category_shell_owned() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("prompt_suggestions")
        .expect("prompt_suggestions must be registered");
    assert_eq!(meta.category, SettingCategory::Editor);
    assert_eq!(meta.owner, SettingOwner::Shell);
    match &meta.kind {
        SettingKind::Bool { default } => assert!(*default, "default must be true"),
        other => panic!("expected Bool kind for prompt_suggestions, got {other:?}"),
    }
    // Must sit immediately below multiline_mode in the registry order.
    let keys: Vec<&str> = reg
        .all()
        .iter()
        .filter(|m| m.category == SettingCategory::Editor)
        .map(|m| m.key)
        .collect();
    let multiline_idx = keys
        .iter()
        .position(|k| *k == "multiline_mode")
        .expect("multiline_mode in Editor");
    let prompt_idx = keys
        .iter()
        .position(|k| *k == "prompt_suggestions")
        .expect("prompt_suggestions in Editor");
    assert_eq!(
        multiline_idx + 1,
        prompt_idx,
        "prompt_suggestions must be immediately below multiline_mode; \
         Editor order: {keys:?}"
    );
}

// ---------------------------------------------------------------------------
// respect_manual_folds — PAGER-owned Bool
// ---------------------------------------------------------------------------

#[test]
fn respect_manual_folds_space_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "respect_manual_folds");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "respect_manual_folds", true);
}

#[test]
fn respect_manual_folds_enter_dispatches_typed_setter() {
    let mut s = make_state();
    navigate_to(&mut s, "respect_manual_folds");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert_set_bool_action(outcome, "respect_manual_folds", true);
}

#[test]
fn respect_manual_folds_mouse_click_two_stage_toggles() {
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "respect_manual_folds") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first click on a different row body should only select, got: {outcome:?}"
    );
    assert_eq!(s.selected, row_y as usize);

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert_set_bool_action(outcome, "respect_manual_folds", true);
}

#[test]
fn respect_manual_folds_snapshot_on_dispatches_off() {
    let snapshot = PagerLocalSnapshot {
        respect_manual_folds: true,
        ..PagerLocalSnapshot::default()
    };
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        snapshot,
    );
    navigate_to(&mut s, "respect_manual_folds");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "respect_manual_folds", false);
}

#[test]
fn respect_manual_folds_renders_under_appearance_category_pager_owned() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("respect_manual_folds")
        .expect("respect_manual_folds must be registered");
    assert_eq!(meta.category, SettingCategory::Appearance);
    assert_eq!(meta.owner, SettingOwner::Pager);
    match &meta.kind {
        SettingKind::Bool { default } => assert!(!default),
        other => panic!("expected Bool kind for respect_manual_folds, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// group_tool_verbs — SHELL-owned Bool (Appearance, default true)
// ---------------------------------------------------------------------------

#[test]
fn group_tool_verbs_space_dispatches_typed_setter() {
    // Default is true; space toggles to false.
    echo_agent_pager::appearance::cache::set_group_tool_verbs(true);
    let mut s = make_state();
    navigate_to(&mut s, "group_tool_verbs");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "group_tool_verbs", false);
}

#[test]
fn group_tool_verbs_enter_dispatches_typed_setter() {
    echo_agent_pager::appearance::cache::set_group_tool_verbs(true);
    let mut s = make_state();
    navigate_to(&mut s, "group_tool_verbs");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert_set_bool_action(outcome, "group_tool_verbs", false);
}

#[test]
fn group_tool_verbs_mouse_click_two_stage_toggles() {
    echo_agent_pager::appearance::cache::set_group_tool_verbs(true);
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "group_tool_verbs") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first click on a different row body should only select, got: {outcome:?}"
    );
    assert_eq!(s.selected, row_y as usize);

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    // Default is true → toggle dispatches false.
    assert_set_bool_action(outcome, "group_tool_verbs", false);
}

#[test]
fn group_tool_verbs_cache_off_dispatches_on() {
    // When the live cache is off, toggle should turn it on.
    echo_agent_pager::appearance::cache::set_group_tool_verbs(false);
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    navigate_to(&mut s, "group_tool_verbs");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "group_tool_verbs", true);
    // Restore default (on) for other tests that share the process cache.
    echo_agent_pager::appearance::cache::set_group_tool_verbs(true);
}

#[test]
fn group_tool_verbs_renders_under_appearance_category_shell_owned() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("group_tool_verbs")
        .expect("group_tool_verbs must be registered");
    assert_eq!(meta.category, SettingCategory::Appearance);
    assert_eq!(meta.owner, SettingOwner::Shell);
    match &meta.kind {
        SettingKind::Bool { default } => assert!(*default, "default must be true"),
        other => panic!("expected Bool kind for group_tool_verbs, got {other:?}"),
    }
    // Must sit immediately below respect_manual_folds in the registry order.
    let keys: Vec<&str> = reg
        .all()
        .iter()
        .filter(|m| m.category == SettingCategory::Appearance)
        .map(|m| m.key)
        .collect();
    let respect_idx = keys
        .iter()
        .position(|k| *k == "respect_manual_folds")
        .expect("respect_manual_folds in Appearance");
    let group_idx = keys
        .iter()
        .position(|k| *k == "group_tool_verbs")
        .expect("group_tool_verbs in Appearance");
    assert_eq!(
        respect_idx + 1,
        group_idx,
        "group_tool_verbs must be immediately below respect_manual_folds; \
         Appearance order: {keys:?}"
    );
}

// ---------------------------------------------------------------------------
// collapsed_edit_blocks — SHELL-owned Bool (Appearance, default false)
// ---------------------------------------------------------------------------

#[test]
fn collapsed_edit_blocks_space_dispatches_typed_setter() {
    // Seed the live cache to the shipped default (bypasses the disk seed so a
    // host [ui] override can't flip the expected toggle direction).
    echo_agent_pager::appearance::cache::set_collapsed_edit_blocks(false);
    let mut s = make_state();
    navigate_to(&mut s, "collapsed_edit_blocks");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "collapsed_edit_blocks", true);
}

#[test]
fn collapsed_edit_blocks_enter_dispatches_typed_setter() {
    echo_agent_pager::appearance::cache::set_collapsed_edit_blocks(false);
    let mut s = make_state();
    navigate_to(&mut s, "collapsed_edit_blocks");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Enter));
    assert_set_bool_action(outcome, "collapsed_edit_blocks", true);
}

#[test]
fn collapsed_edit_blocks_mouse_click_two_stage_toggles() {
    echo_agent_pager::appearance::cache::set_collapsed_edit_blocks(false);
    let mut s = make_state();
    synth_rects(&mut s);
    let row_y = row_idx_for(&s, "collapsed_edit_blocks") as u16;

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    assert!(
        matches!(outcome, SettingsKeyOutcome::Changed),
        "first click on a different row body should only select, got: {outcome:?}"
    );
    assert_eq!(s.selected, row_y as usize);

    let outcome = handle_settings_mouse(
        &mut s,
        MouseEventKind::Down(crossterm::event::MouseButton::Left),
        10,
        row_y,
    );
    // Default is false → toggle dispatches true.
    assert_set_bool_action(outcome, "collapsed_edit_blocks", true);
}

#[test]
fn collapsed_edit_blocks_cache_on_dispatches_off() {
    // When the live cache is on (remote settings/team enable), toggle turns it off.
    echo_agent_pager::appearance::cache::set_collapsed_edit_blocks(true);
    let mut s = SettingsModalState::new(
        Arc::new(SettingsRegistry::defaults()),
        UiConfig::default(),
        PagerLocalSnapshot::default(),
    );
    navigate_to(&mut s, "collapsed_edit_blocks");
    let outcome = handle_settings_key(&mut s, &press(KeyCode::Char(' ')));
    assert_set_bool_action(outcome, "collapsed_edit_blocks", false);
    // Restore default (off) for other tests that share the process cache.
    echo_agent_pager::appearance::cache::set_collapsed_edit_blocks(false);
}

#[test]
fn collapsed_edit_blocks_renders_under_appearance_category_shell_owned() {
    let reg = SettingsRegistry::defaults();
    let meta = reg
        .find("collapsed_edit_blocks")
        .expect("collapsed_edit_blocks must be registered");
    assert_eq!(meta.category, SettingCategory::Appearance);
    assert_eq!(meta.owner, SettingOwner::Shell);
    match &meta.kind {
        SettingKind::Bool { default } => {
            assert!(!*default, "default must be false (rollout flag ships OFF)")
        }
        other => panic!("expected Bool kind for collapsed_edit_blocks, got {other:?}"),
    }
    // Must sit immediately below group_tool_verbs in the registry order.
    let keys: Vec<&str> = reg
        .all()
        .iter()
        .filter(|m| m.category == SettingCategory::Appearance)
        .map(|m| m.key)
        .collect();
    let group_idx = keys
        .iter()
        .position(|k| *k == "group_tool_verbs")
        .expect("group_tool_verbs in Appearance");
    let collapsed_idx = keys
        .iter()
        .position(|k| *k == "collapsed_edit_blocks")
        .expect("collapsed_edit_blocks in Appearance");
    assert_eq!(
        group_idx + 1,
        collapsed_idx,
        "collapsed_edit_blocks must be immediately below group_tool_verbs; \
         Appearance order: {keys:?}"
    );
}
