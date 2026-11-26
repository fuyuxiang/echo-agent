//! Tests for plan, yolo, auto, and permission mode transitions.

use super::*;

/// `ShowPlanNudge` is a no-op when its per-tip gate is off: no tip shown,
/// no count burned, even on a drawable agent.
#[test]
fn show_plan_nudge_no_op_when_flag_off() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 30);
    app.contextual_hints.plan_mode = false;

    let effects = dispatch(Action::ShowPlanNudge, &mut app);
    assert!(effects.is_empty());
    assert!(app.tip_seen_counts.is_empty(), "no count burned");
    assert!(!app.agents[&id].ephemeral_tip.is_active());
}

/// `ShowPlanNudge` with the tip on and a drawable agent shows the tip and
/// increments the per-session seen count once (in memory, no effects).
#[test]
fn show_plan_nudge_shows_and_counts_when_flag_on() {
    use crate::tips::plan_nudge::PLAN_NUDGE_SEEN_KEY;
    let mut app = test_app_with_agent();
    app.contextual_hints.plan_mode = true;
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 30);

    let effects = dispatch(Action::ShowPlanNudge, &mut app);
    assert!(app.agents[&id].ephemeral_tip.is_active());
    assert_eq!(app.tip_seen_counts.get(PLAN_NUDGE_SEEN_KEY), Some(&1));
    assert!(
        effects.is_empty(),
        "seen count is in-memory; nothing persisted"
    );
}

/// `ShowWordSelectTip` is a no-op when its per-tip gate is off.
#[test]
fn show_word_select_tip_no_op_when_flag_off() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 30);
    app.contextual_hints.word_select = false;

    let effects = dispatch(Action::ShowWordSelectTip, &mut app);
    assert!(effects.is_empty());
    assert!(app.tip_seen_counts.is_empty(), "no count burned");
    assert!(!app.agents[&id].ephemeral_tip.is_active());
}

/// `ShowWordSelectTip` shows and counts when the gate is on and selection
/// is not already `word_select`.
#[test]
fn show_word_select_tip_shows_and_counts_when_flag_on() {
    use crate::appearance::TextSelection;
    use crate::tips::word_select::WORD_SELECT_TIP_SEEN_KEY;
    crate::appearance::cache::set_keep_text_selection(TextSelection::Flash);
    let mut app = test_app_with_agent();
    app.contextual_hints.word_select = true;
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 30);

    let effects = dispatch(Action::ShowWordSelectTip, &mut app);
    assert!(app.agents[&id].ephemeral_tip.is_active());
    assert_eq!(app.tip_seen_counts.get(WORD_SELECT_TIP_SEEN_KEY), Some(&1));
    assert!(
        effects.is_empty(),
        "seen count is in-memory; nothing persisted"
    );
}

/// Already on `word_select` → tip is redundant, skip without burning count.
#[test]
fn show_word_select_tip_no_op_when_already_word_select() {
    use crate::appearance::TextSelection;
    crate::appearance::cache::set_keep_text_selection(TextSelection::WordSelect);
    let mut app = test_app_with_agent();
    app.contextual_hints.word_select = true;
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 30);

    let effects = dispatch(Action::ShowWordSelectTip, &mut app);
    assert!(effects.is_empty());
    assert!(app.tip_seen_counts.is_empty());
    assert!(!app.agents[&id].ephemeral_tip.is_active());
    // Restore default so sibling tests don't inherit word_select.
    crate::appearance::cache::set_keep_text_selection(TextSelection::Flash);
}

/// Accepting the tip (its chord, with the tip on screen) flips the setting
/// to `word_select`, persists it, and retires the tip.
#[test]
fn accept_word_select_tip_flips_setting_and_retires_tip() {
    use crate::appearance::TextSelection;
    use crate::settings::SettingValue;
    crate::appearance::cache::set_keep_text_selection(TextSelection::Flash);
    let mut app = test_app_with_agent();
    app.contextual_hints.word_select = true;
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 30);
    let _ = dispatch(Action::ShowWordSelectTip, &mut app);
    assert!(app.agents[&id].ephemeral_tip.is_active());

    let effects = dispatch(Action::AcceptWordSelectTip, &mut app);
    assert!(
        crate::appearance::cache::load_keep_text_selection().selects_word(),
        "accept must flip the live setting to word_select"
    );
    assert!(
        !app.agents[&id].ephemeral_tip.is_active(),
        "accept must retire the tip"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistSetting {
                key: "keep_text_selection",
                value: SettingValue::Enum("word_select"),
                ..
            }
        )),
        "accept must persist the setting, got: {effects:?}"
    );
    // Restore default so sibling tests don't inherit word_select.
    crate::appearance::cache::set_keep_text_selection(TextSelection::Flash);
}

/// The accept action is tip-scoped: without the tip on screen it must not
/// touch the setting (Ctrl+Y outside the TTL keeps its normal meaning; a
/// stray action must not become a global toggle).
#[test]
fn accept_word_select_tip_no_op_when_tip_not_showing() {
    use crate::appearance::TextSelection;
    crate::appearance::cache::set_keep_text_selection(TextSelection::Flash);
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().last_terminal_size = (80, 30);
    assert!(!app.agents[&id].ephemeral_tip.is_active());

    let effects = dispatch(Action::AcceptWordSelectTip, &mut app);
    assert!(effects.is_empty());
    assert!(
        !crate::appearance::cache::load_keep_text_selection().selects_word(),
        "setting must be untouched without the tip"
    );
}

// ── /plan slash command tests ─────────────────────────────────────

#[test]
fn slash_plan_no_args_not_in_plan_enters_plan_mode() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    assert!(!app.agents[&id].plan_mode_active);
    assert!(app.agents[&id].plan_mode_pending.is_none());

    let effects = dispatch(Action::SendPrompt("/plan".into()), &mut app);

    // Should emit SetSessionMode to enter plan mode.
    assert_eq!(effects.len(), 1);
    assert!(
        matches!(&effects[0], Effect::SetSessionMode { mode_id, .. } if &*mode_id.0 == "plan"),
        "expected SetSessionMode(plan), got: {effects:?}"
    );
    // Optimistic pending state should be set.
    assert_eq!(app.agents[&id].plan_mode_pending, Some(true));
}

#[test]
fn slash_plan_no_args_already_in_plan_shows_plan() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plan_mode_active = true;

    let effects = dispatch(Action::SendPrompt("/plan".into()), &mut app);

    // Should NOT emit SetSessionMode — just show the plan (no async effect).
    assert!(effects.is_empty(), "expected no effects, got: {effects:?}");
}

#[test]
fn slash_plan_with_args_not_in_plan_enters_and_sends_prompt() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    let effects = dispatch(
        Action::SendPrompt("/plan add auth to the app".into()),
        &mut app,
    );

    // Should emit a single SetModeThenPrompt (mode switch + prompt
    // bundled into one sequential task to avoid a race).
    assert_eq!(effects.len(), 1, "expected 1 effect, got: {effects:?}");
    assert!(
        matches!(
            &effects[0],
            Effect::SetModeThenPrompt { mode_id, text, .. }
                if &*mode_id.0 == "plan" && text == "add auth to the app"
        ),
        "expected SetModeThenPrompt(plan, \"add auth to the app\"), got: {effects:?}"
    );
    assert_eq!(app.agents[&id].plan_mode_pending, Some(true));
}

/// The `SendPrompt → SetModeThenPrompt` rewrap must forward the desc-space
/// token ranges, and the drained echo must style them — a silent
/// `Vec::new()` regression in the rewrap (or its handler) would otherwise
/// compile clean and only surface as plain styling on the `/plan <desc>`
/// path.
#[test]
fn slash_plan_desc_forwards_skill_token_ranges() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    super::prompt::register_pr_workflow_skill(&mut app, id);

    let effects = dispatch(
        Action::SendPrompt("/plan great /pr-workflow go".into()),
        &mut app,
    );

    assert_eq!(effects.len(), 1, "expected 1 effect, got: {effects:?}");
    match &effects[0] {
        Effect::SetModeThenPrompt {
            mode_id,
            text,
            skill_token_ranges,
            ..
        } => {
            assert_eq!(&*mode_id.0, "plan");
            assert_eq!(text, "great /pr-workflow go");
            assert_eq!(
                skill_token_ranges,
                &vec![6..18],
                "offsets recomputed against the stripped desc"
            );
        }
        other => panic!("expected SetModeThenPrompt, got {other:?}"),
    }
    // The drained echo block carries the same desc-space ranges.
    match &app.agents[&id].scrollback.get(0).unwrap().block {
        RenderBlock::UserPrompt(b) => {
            assert_eq!(b.skill_token_ranges, vec![6..18]);
        }
        other => panic!("expected UserPrompt, got {other:?}"),
    }
}

#[test]
fn slash_plan_with_args_already_in_plan_is_noop() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().plan_mode_active = true;

    let effects = dispatch(
        Action::SendPrompt("/plan add auth to the app".into()),
        &mut app,
    );

    assert!(effects.is_empty(), "expected no effects, got: {effects:?}");
    assert!(read_toast(&app).contains("/view-plan"));
}

/// Multi-agent fan-out (sibling for `plan_mode`).
/// `Action::SetPlanMode(On)` populates the active agent's
/// `plan_mode_pending` and never touches other agents in the
/// registry. The contract differs from `multiline_mode` in that
/// `plan_mode_pending` is an `Option<bool>` (optimistic stash) —
/// the non-active agent must stay `None`.
#[test]
fn set_plan_mode_mutates_only_active_agent_not_others() {
    let mut app = test_app_with_agent();
    insert_placeholder_agent(&mut app, AgentId(1));
    assert!(app.agents[&AgentId(0)].plan_mode_pending.is_none());
    assert!(app.agents[&AgentId(1)].plan_mode_pending.is_none());

    let _ = dispatch(
        Action::SetPlanMode(crate::app::actions::PlanModeKind::On),
        &mut app,
    );

    assert_eq!(
        app.agents[&AgentId(0)].plan_mode_pending,
        Some(true),
        "active agent must have optimistic plan_mode_pending = Some(true)",
    );
    assert!(
        app.agents[&AgentId(1)].plan_mode_pending.is_none(),
        "non-active agent must NOT receive the plan_mode pending state",
    );
    assert!(
        !app.agents[&AgentId(1)].plan_mode_active,
        "non-active agent's confirmed plan_mode_active must stay false",
    );
}

// ----------------------------------------------------------------
// `set_yolo_mode` dispatcher unit tests (security-relevant)
//
// SHELL-owned, but with rollback semantics: a disk-write failure
// routes through `apply_setting_rollback("permission_mode", _)`
// which calls `set_yolo_mode_inner(app, prev)` to revert. The
// outer setter never re-emits `Effect::PersistPermissionMode` on
// rollback so a persistent disk failure does not loop.
//
// Security invariants the test suite pins:
//   - On YOLO ON: the per-agent permission_queue is drained with
//     `AllowOnce` responses (NOT cancelled — auto-approve).
//   - The drain ALSO runs on a duplicate YOLO=ON dispatch
//     (any permission queued between
//     dispatches must be drained on the second). Only telemetry
//     + the "setting changed" tracing log are gated on
//     transitions.
//   - On no-AllowOnce shape: the drain falls back to `Cancelled`,
//     NOT `AllowAlways` — preserves the safety contract that
//     YOLO never picks a more-permissive option than `AllowOnce`.
//   - `app.current_ui.permission_mode` stays in lock-step with
//     `agent.session.yolo_mode` so the modal snapshot is fresh.
//   - `Effect::PersistPermissionMode { persist:
//     PermissionModePersist::WithRollback(prev) }` emitted
//     exactly once per typed-setter dispatch (see
//     `app::actions::PermissionModePersist`).
//   - Rollback via `apply_setting_rollback("permission_mode",
//     SettingValue::Enum(_))` reverts the in-memory state via
//     `set_yolo_mode_inner` (no re-emit). Refreshes any open
//     settings modal (`rollback_permission_mode_refreshes_open_modal_snapshots`).
//   - Toast format:
//     - ON:  "⚠ Always-approve ON: all tool actions auto-run"
//       (destructive-action variant —
//       differentiated visual + body because enabling YOLO is
//       the single most security-relevant user action in the
//       pager).
//     - OFF: "✓ Always-approve: off" (standard success format
//       — restoring the safe default).
//   - Failure toast: "✗ Could not save permission_mode: {error}"
//     — exact format pinned via `assert_eq!` in
//     `rollback_permission_mode_reverts_state_no_effect`.
// ----------------------------------------------------------------

/// Slash gate sync: both toggles stay offered while modes change; only the
/// auto feature gate suppresses `/auto`.
#[test]
fn permission_mode_slash_gate_offers_toggles_subject_to_auto_feature() {
    use crate::app::actions::PermissionModeKind;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.auto_mode_gate = true;
    app.sync_permission_mode_slash_gate();

    let offered = |app: &AppView, name: &str| {
        app.agents[&id]
            .prompt
            .slash_controller
            .registry()
            .get(name)
            .is_some()
    };

    assert!(offered(&app, "always-approve"));
    assert!(offered(&app, "auto"));

    // Mode changes must not hide either toggle.
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(offered(&app, "always-approve"));
    assert!(offered(&app, "auto"));

    let _ = dispatch(
        Action::SetPermissionMode(PermissionModeKind::Auto),
        &mut app,
    );
    assert!(offered(&app, "always-approve"));
    assert!(offered(&app, "auto"));

    // Gate off → only `/auto` disappears.
    app.auto_mode_gate = false;
    app.sync_permission_mode_slash_gate();
    assert!(offered(&app, "always-approve"));
    assert!(!offered(&app, "auto"));
}

/// End-to-end via slash submission: `/always-approve` and `/auto` toggle off
/// when re-run and cross-switch when the other is active.
#[test]
fn slash_always_approve_and_auto_toggle_and_cross_switch() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.auto_mode_gate = true;
    app.sync_permission_mode_slash_gate();

    // Off → always-approve.
    let _ = dispatch(Action::SendPrompt("/always-approve".into()), &mut app);
    assert!(app.agents[&id].session.is_yolo());
    assert!(!app.agents[&id].session.is_auto());

    // Always-approve → auto (cross-switch).
    let _ = dispatch(Action::SendPrompt("/auto".into()), &mut app);
    assert!(!app.agents[&id].session.is_yolo());
    assert!(app.agents[&id].session.is_auto());

    // Auto → ask (toggle off).
    let _ = dispatch(Action::SendPrompt("/auto".into()), &mut app);
    assert!(!app.agents[&id].session.is_yolo());
    assert!(!app.agents[&id].session.is_auto());

    // Off → auto.
    let _ = dispatch(Action::SendPrompt("/auto".into()), &mut app);
    assert!(app.agents[&id].session.is_auto());

    // Auto → always-approve (cross-switch).
    let _ = dispatch(Action::SendPrompt("/always-approve".into()), &mut app);
    assert!(app.agents[&id].session.is_yolo());
    assert!(!app.agents[&id].session.is_auto());

    // Always-approve → ask (toggle off).
    let _ = dispatch(Action::SendPrompt("/always-approve".into()), &mut app);
    assert!(!app.agents[&id].session.is_yolo());
    assert!(!app.agents[&id].session.is_auto());
}

#[test]
fn set_yolo_mode_off_to_on_emits_persist_with_rollback() {
    let mut app = test_app_with_agent();
    // Default is yolo=false.
    assert!(!app.agents[&AgentId(0)].session.is_yolo());

    let effects = dispatch(Action::SetYoloMode(true), &mut app);

    // In-memory state mutated.
    assert!(
        app.agents[&AgentId(0)].session.is_yolo(),
        "session.yolo_mode must flip to true"
    );
    assert!(app.default_yolo, "app.default_yolo must mirror the toggle");
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("always-approve"),
        "current_ui.permission_mode must mirror the canonical string",
    );

    // Exactly one Effect with the right rollback payload.
    assert_eq!(effects.len(), 1, "expected exactly one Effect");
    match &effects[0] {
        Effect::PersistPermissionMode {
            canonical,
            persist,
            session_id,
        } => {
            assert_eq!(*canonical, "always-approve");
            assert_eq!(
                *persist,
                crate::app::actions::PermissionModePersist::WithRollback("ask"),
                "rollback must revert to the prior canonical (was 'ask')"
            );
            // Explicit session_id assertion
            // (previously hidden behind `..` — a regression that
            // dropped session_id silently broke the ACP
            // notification gate at effects.rs).
            assert!(
                session_id.is_some(),
                "session_id must be threaded through for ACP notification gating"
            );
        }
        other => panic!("expected PersistPermissionMode, got {other:?}"),
    }
}

/// Enabling always-approve while plan mode is active must warn that the
/// plan-mode edit gate stays binding — the standard "all tool actions
/// auto-run" toast would overpromise (the shell rejects non-plan-file edits
/// in plan mode regardless of yolo).
#[test]
fn set_yolo_mode_on_under_plan_uses_plan_aware_toast() {
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().plan_mode_active = true;

    let _ = dispatch(Action::SetYoloMode(true), &mut app);

    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast must be set");
    assert_eq!(toast, YOLO_ON_UNDER_PLAN_TOAST);

    // Pending (optimistic) plan state counts too — same as the flag renderer.
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().plan_mode_pending = Some(true);
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast must be set");
    assert_eq!(toast, YOLO_ON_UNDER_PLAN_TOAST);

    // Without plan mode the standard destructive toast is unchanged.
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast must be set");
    assert_eq!(
        toast,
        "\u{26A0} Always-approve ON: all tool actions auto-run"
    );
}

/// The settings-modal path (`SetPermissionMode(AlwaysApprove)`) gets the same
/// plan-aware toast as the Ctrl+O path.
#[test]
fn set_permission_mode_always_approve_under_plan_uses_plan_aware_toast() {
    use crate::app::actions::PermissionModeKind;
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().plan_mode_active = true;

    let _ = dispatch(
        Action::SetPermissionMode(PermissionModeKind::AlwaysApprove),
        &mut app,
    );

    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast must be set");
    assert_eq!(toast, YOLO_ON_UNDER_PLAN_TOAST);
}

#[test]
fn set_yolo_mode_on_to_off_emits_persist_with_rollback() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    // Pre-set yolo=true via the typed setter so the rollback
    // value is captured correctly.
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(app.agents[&AgentId(0)].session.is_yolo());

    let effects = dispatch(Action::SetYoloMode(false), &mut app);

    assert!(!app.agents[&AgentId(0)].session.is_yolo());
    assert!(!app.default_yolo);
    assert_eq!(app.current_ui.permission_mode.as_deref(), Some("ask"));

    match &effects[0] {
        Effect::PersistPermissionMode {
            canonical,
            persist,
            session_id,
        } => {
            assert_eq!(*canonical, "ask");
            assert_eq!(
                *persist,
                crate::app::actions::PermissionModePersist::WithRollback("always-approve"),
                "rollback must revert to the prior canonical (was 'always-approve')"
            );
            assert!(session_id.is_some(), "session_id must be threaded");
        }
        other => panic!("expected PersistPermissionMode, got {other:?}"),
    }

    // Defense-in-depth — pin that `SettingValue` enum re-import
    // works at the test boundary (catches a hypothetical pruning
    // of the public re-export, which would silently fail the
    // settings_e2e crate).
    let _ = SettingValue::Enum("ask");
}

#[test]
fn yolo_on_drain_clears_double_click_tracker() {
    let mut app = test_app_with_agent();
    let _rx = enqueue_permission_with_enable_always_approve(&mut app);

    app.agents
        .get_mut(&AgentId(0))
        .unwrap()
        .last_permission_click = Some((Instant::now(), 1));

    let _ = dispatch(Action::SetYoloMode(true), &mut app);

    let agent = &app.agents[&AgentId(0)];
    assert!(agent.permission_queue.is_empty());
    assert!(
        agent.last_permission_click.is_none(),
        "YOLO-on drain must invalidate the armed click"
    );
}

/// When the user picks the "enable-always-approve" option:
///
/// 1. The shell receives a `Selected{option_id: ENABLE_ALWAYS_APPROVE_OPTION_ID}`
///    response. The shell's `map_selected_outcome` resolves this to
///    `PromptOutcome::AllowOnce`, so the in-flight tool call is
///    allowed exactly once (no per-tool whitelisting).
/// 2. The dispatcher returns a `PersistPermissionMode` effect with
///    canonical `"always-approve"` — this is what flips
///    `[ui] permission_mode` on disk AND fires the
///    `echo.agent/yolo_mode_changed` ACP notification back to the shell.
/// 3. The agent's per-session `yolo_mode` flag is flipped to true,
///    so subsequent permission requests are auto-approved by
///    `handle_permission_request`.
///
/// A regression in any of these three legs would break the
/// "one click to enable always-approve mode" contract.
#[test]
fn enable_always_approve_sends_response_and_flips_yolo_and_persists() {
    use std::sync::Arc;

    let mut app = test_app_with_agent();
    let mut response_rx = enqueue_permission_with_enable_always_approve(&mut app);

    // Sanity: YOLO is OFF before selecting the option.
    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "precondition: YOLO must be off",
    );

    let effects = dispatch(
        Action::PermissionSelect(acp::PermissionOptionId::new(Arc::from(
            echo_agent_workspace::permission::ENABLE_ALWAYS_APPROVE_OPTION_ID,
        ))),
        &mut app,
    );

    // (1) The shell sees the option_id we picked. The kind is
    //     `AllowOnce` on the wire; the shell's `map_selected_outcome`
    //     resolves the id under the `AllowOnce` branch and returns
    //     `PromptOutcome::AllowOnce`. Verify the id round-trips.
    match response_rx.try_recv() {
        Ok(Ok(acp::RequestPermissionResponse {
            outcome:
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome {
                    option_id,
                    ..
                }),
            ..
        })) => {
            assert_eq!(
                option_id.0.as_ref(),
                echo_agent_workspace::permission::ENABLE_ALWAYS_APPROVE_OPTION_ID,
                "the response must echo the picked option_id",
            );
        }
        other => {
            panic!("enable-always-approve must produce a Selected response, got {other:?}",)
        }
    }

    // (2) The dispatcher returns a PersistPermissionMode effect with
    //     canonical "always-approve". This is the bridge that writes
    //     ~/.echo-agent/config.toml AND fires echo.agent/yolo_mode_changed.
    let persist = effects
        .iter()
        .find_map(|e| match e {
            Effect::PersistPermissionMode { canonical, .. } => Some(*canonical),
            _ => None,
        })
        .expect(
            "enable-always-approve must emit PersistPermissionMode so the toggle persists \
                 across sessions and the shell's permission manager learns about it",
        );
    assert_eq!(
        persist, "always-approve",
        "PersistPermissionMode canonical must be `always-approve` (not `ask`/`default`)",
    );

    // (3) Per-session YOLO flag is flipped — future prompts will be
    //     auto-approved in `handle_permission_request`.
    assert!(
        app.agents[&AgentId(0)].session.is_yolo(),
        "session.yolo_mode must be flipped on after selecting enable-always-approve",
    );
    // Global default mirror also flipped.
    assert!(
        app.default_yolo,
        "app.default_yolo must be flipped on (used as initial value for new agents)",
    );
}

/// If the user picks "enable-always-approve" while YOLO is ALREADY
/// on, the dispatcher must NOT re-emit `PersistPermissionMode`
/// (which would queue a redundant disk write + ACP notification).
/// In practice YOLO-on suppresses the permission panel entirely
/// (`handle_permission_request` auto-approves), so this state is
/// only reachable in tests, but the idempotency guard matters for
/// future code paths that might pre-seed YOLO state.
#[test]
fn enable_always_approve_is_idempotent_when_yolo_already_on() {
    use std::sync::Arc;

    let mut app = test_app_with_agent();

    // Pre-flip YOLO on. We bypass the panel suppression by injecting
    // the permission AFTER the flip — exercises the dispatcher's
    // idempotency guard directly.
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(app.agents[&AgentId(0)].session.is_yolo());

    let mut response_rx = enqueue_permission_with_enable_always_approve(&mut app);

    let effects = dispatch(
        Action::PermissionSelect(acp::PermissionOptionId::new(Arc::from(
            echo_agent_workspace::permission::ENABLE_ALWAYS_APPROVE_OPTION_ID,
        ))),
        &mut app,
    );

    // Response still flows (the current request is allowed once).
    match response_rx.try_recv() {
        Ok(Ok(acp::RequestPermissionResponse {
            outcome: acp::RequestPermissionOutcome::Selected(_),
            ..
        })) => {}
        other => panic!("expected Selected response, got {other:?}"),
    }

    // No redundant PersistPermissionMode. (The initial SetYoloMode
    // dispatch above already produced one for the YOLO-flip.)
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::PersistPermissionMode { .. })),
        "redundant PersistPermissionMode when YOLO already on — the dispatcher \
             must short-circuit to avoid double-writing config.toml and double-firing \
             echo.agent/yolo_mode_changed",
    );
}

/// **Security-critical fallback:**
/// when a queued permission has NO `AllowOnce` option (only
/// `AllowAlways` / `RejectAlways`), the drain MUST send
/// `Cancelled` — NOT silently fall through to `AllowAlways`
/// which would whitelist the operation indefinitely.
///
/// This pins the safety contract: YOLO never auto-picks a
/// more-permissive option than `AllowOnce`. A regression that
/// added an `else if find(AllowAlways)` fallback would
/// dramatically widen the blast radius of a single YOLO toggle.
#[test]
fn set_yolo_mode_on_with_no_allow_once_option_sends_cancelled() {
    use crate::views::permission_view::{PermissionFocus, PermissionViewState};
    use std::sync::Arc;

    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();

    // Inject a permission with only AllowAlways + RejectAlways
    // (NO AllowOnce). The drain must NOT pick AllowAlways even
    // though it's the only "Allow" option — that would breach
    // the safety contract.
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
    let request = acp::RequestPermissionRequest::new(
        acp::SessionId::new(Arc::from("test-sess")),
        acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from("tc-noallow-1")),
            acp::ToolCallUpdateFields::default(),
        ),
        vec![
            acp::PermissionOption::new(
                acp::PermissionOptionId::new(Arc::from("opt-allow-always")),
                "Allow always",
                acp::PermissionOptionKind::AllowAlways,
            ),
            acp::PermissionOption::new(
                acp::PermissionOptionId::new(Arc::from("opt-reject-always")),
                "Reject always",
                acp::PermissionOptionKind::RejectAlways,
            ),
        ],
    );
    let options = request.options.clone();
    agent.permission_queue.push_back(PermissionViewState {
        request: echo_agent_acp::AcpArgs {
            request,
            response_tx,
        },
        id: 1,
        focus: PermissionFocus::Options,
        options,
        active_idx: 0,
        bash_highlights: None,
        bash_selection_count: 0,
        bash_deny_selection_count: 0,
        bash_command_raw: None,
        mcp_scope: None,
        title: "noallow-test".to_string(),
        description: vec![],
        args_expanded: false,
        desc_scroll: 0,
        subagent_label: None,
        options_area_height: 0,
        options_scroll_offset: 0,
    });

    let _ = dispatch(Action::SetYoloMode(true), &mut app);

    // Queue drained.
    assert!(app.agents[&AgentId(0)].permission_queue.is_empty());
    // Cancelled (NOT Selected{AllowAlways}).
    match response_rx.try_recv() {
        Ok(Ok(acp::RequestPermissionResponse {
            outcome: acp::RequestPermissionOutcome::Cancelled,
            ..
        })) => {
            // Correct — preserved the safety contract.
        }
        Ok(Ok(acp::RequestPermissionResponse {
            outcome:
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome {
                    option_id,
                    ..
                }),
            ..
        })) => panic!(
            "drain picked `{option_id:?}` instead of Cancelled — SAFETY CONTRACT \
                 VIOLATION: YOLO must never pick a more-permissive option than AllowOnce. \
                 Either AllowAlways (whitelist forever) or RejectAlways (deny forever) \
                 would be wrong; the drain must Cancel and let the caller's higher level \
                 decide.",
        ),
        other => panic!("expected Cancelled response, got {other:?}"),
    }
}

/// **Security-critical multi-item drain:** the
/// drain loop must fully empty the queue, not stop at the first
/// item. A regression that swapped `drain(..)` for `pop_front()`
/// would silently leak queued permissions on YOLO toggle. With
/// 3 items in the queue, this catches an off-by-N drain bug.
#[test]
fn set_yolo_mode_on_drains_multi_item_queue() {
    use crate::views::permission_view::{PermissionFocus, PermissionViewState};
    use std::sync::Arc;

    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();

    // Inject 3 permissions, each with AllowOnce.
    let mut response_rxs = Vec::new();
    for i in 0..3u32 {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        response_rxs.push(response_rx);
        let request = acp::RequestPermissionRequest::new(
            acp::SessionId::new(Arc::from("test-sess")),
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from(format!("tc-multi-{i}"))),
                acp::ToolCallUpdateFields::default(),
            ),
            vec![acp::PermissionOption::new(
                acp::PermissionOptionId::new(Arc::from(format!("opt-allow-once-{i}"))),
                "Allow once",
                acp::PermissionOptionKind::AllowOnce,
            )],
        );
        let options = request.options.clone();
        agent.permission_queue.push_back(PermissionViewState {
            request: echo_agent_acp::AcpArgs {
                request,
                response_tx,
            },
            id: i as usize + 1,
            focus: PermissionFocus::Options,
            options,
            active_idx: 0,
            bash_highlights: None,
            bash_selection_count: 0,
            bash_deny_selection_count: 0,
            bash_command_raw: None,
            mcp_scope: None,
            title: format!("multi-{i}"),
            description: vec![],
            args_expanded: false,
            desc_scroll: 0,
            subagent_label: None,
            options_area_height: 0,
            options_scroll_offset: 0,
        });
    }
    assert_eq!(agent.permission_queue.len(), 3);

    let _ = dispatch(Action::SetYoloMode(true), &mut app);

    // Queue fully drained.
    assert!(
        app.agents[&AgentId(0)].permission_queue.is_empty(),
        "multi-item drain must fully empty the queue",
    );
    // All 3 channels received the AllowOnce response.
    for (i, mut rx) in response_rxs.into_iter().enumerate() {
        match rx.try_recv() {
            Ok(Ok(acp::RequestPermissionResponse {
                outcome: acp::RequestPermissionOutcome::Selected(_),
                ..
            })) => {} // OK
            other => panic!(
                "item {i} did not receive AllowOnce Selected response: {other:?} — \
                     drain skipped items beyond the first?",
            ),
        }
    }
}

/// **Security-critical:** re-dispatching
/// `SetYoloMode(true)` when already on MUST still drain any
/// permissions that arrived between the two dispatches. A future
/// "optimization" that skipped the drain on no-op redispatch
/// would lose security-critical state.
#[test]
fn set_yolo_mode_on_duplicate_dispatch_still_drains_queue() {
    use crate::views::permission_view::{PermissionFocus, PermissionViewState};
    use std::sync::Arc;

    let mut app = test_app_with_agent();
    // First dispatch: turn YOLO ON. Queue is empty so no drain.
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(app.agents[&AgentId(0)].session.is_yolo());

    // Now inject a permission AFTER the first dispatch.
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
    let request = acp::RequestPermissionRequest::new(
        acp::SessionId::new(Arc::from("test-sess")),
        acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from("tc-dup-1")),
            acp::ToolCallUpdateFields::default(),
        ),
        vec![acp::PermissionOption::new(
            acp::PermissionOptionId::new(Arc::from("opt-allow-once")),
            "Allow once",
            acp::PermissionOptionKind::AllowOnce,
        )],
    );
    let options = request.options.clone();
    app.agents
        .get_mut(&AgentId(0))
        .unwrap()
        .permission_queue
        .push_back(PermissionViewState {
            request: echo_agent_acp::AcpArgs {
                request,
                response_tx,
            },
            id: 1,
            focus: PermissionFocus::Options,
            options,
            active_idx: 0,
            bash_highlights: None,
            bash_selection_count: 0,
            bash_deny_selection_count: 0,
            bash_command_raw: None,
            mcp_scope: None,
            title: "dup-test".to_string(),
            description: vec![],
            args_expanded: false,
            desc_scroll: 0,
            subagent_label: None,
            options_area_height: 0,
            options_scroll_offset: 0,
        });

    // Second dispatch (same value): MUST still drain. A
    // "skip-drain-on-no-op" regression would leak this permission.
    let _ = dispatch(Action::SetYoloMode(true), &mut app);

    assert!(
        app.agents[&AgentId(0)].permission_queue.is_empty(),
        "duplicate YOLO=true dispatch MUST drain any permission that arrived \
             between dispatches — Security Issue 27 regression",
    );
    match response_rx.try_recv() {
        Ok(Ok(acp::RequestPermissionResponse {
            outcome: acp::RequestPermissionOutcome::Selected(_),
            ..
        })) => {} // OK
        other => panic!(
            "duplicate dispatch must auto-approve the newly queued permission, got {other:?}",
        ),
    }
}

/// Idempotent re-dispatch: re-dispatching the same value still
/// emits a `PersistPermissionMode` + re-fires the toast (unlike
/// PAGER setters which short-circuit). The SHARED setter contract
/// is "always toast + always persist on save" so a duplicate
/// dispatch is a no-op on state but still confirms via toast +
/// disk write.
///
/// **Contract:** `persist` is `WithRollback(new)` even on a
/// no-op dispatch (prev == new). The disk write that follows is
/// idempotent on disk so the only observable side effects of a
/// duplicate are the toast + the (no-op) drain.
///
/// Pin EVERY state field
/// after the redispatch, AND prove the toast was actually
/// re-fired (clear it between dispatches so the second toast
/// can't be the first one lingering).
#[test]
fn set_yolo_mode_redispatch_same_value_still_emits_effect_and_toast() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(app.agents[&AgentId(0)].toast.is_some());
    // Clear the toast: prove the second dispatch RE-FIRES the
    // toast (not just "the first toast is still visible").
    app.agents.get_mut(&AgentId(0)).unwrap().toast = None;

    let effects = dispatch(Action::SetYoloMode(true), &mut app);

    assert_eq!(
        effects.len(),
        1,
        "duplicate dispatch must still emit PersistPermissionMode"
    );
    match &effects[0] {
        Effect::PersistPermissionMode {
            canonical,
            persist,
            session_id,
        } => {
            assert_eq!(
                *canonical, "always-approve",
                "Effect.canonical must be 'always-approve' on duplicate YOLO=true",
            );
            assert_eq!(
                *persist,
                crate::app::actions::PermissionModePersist::WithRollback("always-approve"),
                "redispatch from yolo='always-approve' → rollback to 'always-approve' (no-op)"
            );
            assert!(
                session_id.is_some(),
                "session_id must be threaded through on duplicate dispatch"
            );
        }
        other => panic!("expected PersistPermissionMode, got {other:?}"),
    }
    // Pin all state fields explicitly.
    assert!(
        app.agents[&AgentId(0)].session.is_yolo(),
        "session.yolo_mode must remain true",
    );
    assert!(app.default_yolo, "app.default_yolo must remain true");
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("always-approve"),
        "current_ui.permission_mode must remain at always-approve",
    );
    // Toast was cleared between dispatches, so
    // `Some(_)` here proves the second dispatch re-fired the
    // toast (not just "carried over from the first").
    assert!(
        app.agents[&AgentId(0)].toast.is_some(),
        "second dispatch must re-fire the toast (proved by clearing between dispatches)",
    );
}

/// Toast string format: exact-equality pin.
///
/// **Destructive-action toast.**
/// The ON case uses `⚠ Always-approve ON: all tool actions
/// auto-run` (warning glyph + body spelling out the consequence)
/// because enabling YOLO is the single most security-relevant
/// user action in the pager. The OFF case uses the standard `✓`
/// success glyph + "Label: value" format (restoring the safe
/// default).
#[test]
fn set_yolo_mode_toast_format() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast must be set");
    assert_eq!(
        toast,
        "\u{26A0} Always-approve ON: all tool actions auto-run"
    );

    let _ = dispatch(Action::SetYoloMode(false), &mut app);
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast must be set");
    assert_eq!(toast, "\u{2713} Always-approve: off");
}

#[test]
fn set_yolo_mode_on_blocked_by_policy_pin() {
    let mut app = test_app_with_agent();
    app.yolo_policy_block = Some(POLICY_WARNING);

    let effects = dispatch(Action::SetYoloMode(true), &mut app);

    assert!(
        effects.is_empty(),
        "blocked enable must not emit any Effect (no persist), got {effects:?}",
    );
    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "session.yolo_mode must stay off under the pin"
    );
    assert!(!app.default_yolo, "app.default_yolo must stay off");
    assert_eq!(
        app.current_ui.permission_mode, None,
        "canonical mirror must stay untouched"
    );
    assert_eq!(agent_toast(&app).as_deref(), Some(POLICY_WARNING));
}

#[test]
fn set_yolo_mode_off_allowed_under_policy_pin() {
    let mut app = test_app_with_agent();
    // ON while unpinned (e.g. state restored from before the pin landed).
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(app.agents[&AgentId(0)].session.is_yolo());
    app.yolo_policy_block = Some(POLICY_WARNING);

    let effects = dispatch(Action::SetYoloMode(false), &mut app);

    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "the pin must not block flipping always-approve OFF"
    );
    assert_eq!(effects.len(), 1, "OFF persists normally");
    assert!(matches!(
        &effects[0],
        Effect::PersistPermissionMode {
            canonical: "ask",
            ..
        }
    ));
}

/// Shift+Tab cycle: Plan → Auto (always-approve is a later step).
/// Plan exit is pushed; PersistPermissionMode(auto) notifies the agent.
#[test]
fn cycle_mode_plan_to_auto_includes_persist_auto() {
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().plan_mode_pending = Some(true);

    let effects = dispatch(Action::CycleMode, &mut app);

    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "auto mode must not enable yolo"
    );
    assert_eq!(app.agents[&AgentId(0)].plan_mode_pending, Some(false));
    assert_eq!(app.current_ui.permission_mode.as_deref(), Some("auto"));
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                canonical: "auto",
                ..
            }
        )),
        "expected PersistPermissionMode(auto), got {effects:?}"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SetSessionMode { .. })),
        "expected plan exit SetSessionMode, got {effects:?}"
    );
}

/// Feature gate OFF: the Shift+Tab cycle skips Auto entirely — Plan jumps
/// straight to Always-Approve (legacy cycle), and "auto" is never persisted.
/// Drives the real `dispatch_cycle_mode` with `auto_mode_gate = false`.
#[test]
fn cycle_mode_plan_to_always_approve_when_auto_gated_off() {
    let mut app = test_app_with_agent();
    app.auto_mode_gate = false;
    app.agents.get_mut(&AgentId(0)).unwrap().plan_mode_pending = Some(true);

    let effects = dispatch(Action::CycleMode, &mut app);

    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("always-approve"),
        "gate OFF: Plan must skip Auto and land on Always-Approve"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                canonical: "always-approve",
                ..
            }
        )),
        "gate OFF: expected PersistPermissionMode(always-approve), got {effects:?}"
    );
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                canonical: "auto",
                ..
            }
        )),
        "gate OFF: must never persist 'auto'"
    );
}

/// Auto → Always-Approve under policy pin lands on Normal (ask), not yolo.
#[test]
fn cycle_mode_auto_to_always_approve_blocked_by_policy_pin() {
    let mut app = test_app_with_agent();
    app.yolo_policy_block = Some(POLICY_WARNING);
    // "In Auto" = the per-session flag the cycle reads (`is_auto()`), not just
    // the global `current_ui` mirror; gate is on via `test_app`.
    app.current_ui.permission_mode = Some("auto".into());
    app.agents.get_mut(&AgentId(0)).unwrap().session.auto_mode = true;

    let effects = dispatch(Action::CycleMode, &mut app);

    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "the pin must keep yolo off through the cycle"
    );
    assert_eq!(app.current_ui.permission_mode.as_deref(), Some("ask"));
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                canonical: "ask",
                ..
            }
        )),
        "expected PersistPermissionMode(ask) under pin, got {effects:?}"
    );
    assert_eq!(agent_toast(&app).as_deref(), Some(POLICY_WARNING));
}

/// Legacy name kept for callers; Plan no longer jumps straight to Always-Approve.
#[test]
fn cycle_mode_plan_to_always_approve_blocked_by_policy_pin() {
    // With Auto inserted, Plan → Auto first; pin is irrelevant on this step.
    let mut app = test_app_with_agent();
    app.yolo_policy_block = Some(POLICY_WARNING);
    app.agents.get_mut(&AgentId(0)).unwrap().plan_mode_pending = Some(true);

    let effects = dispatch(Action::CycleMode, &mut app);

    assert!(!app.agents[&AgentId(0)].session.is_yolo());
    assert_eq!(app.current_ui.permission_mode.as_deref(), Some("auto"));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::PersistPermissionMode {
            canonical: "auto",
            ..
        }
    )));
}

/// Plan active while already in Auto: Shift+Tab follows Plan → Auto —
/// exit Plan but KEEP the classifier. Must NOT fall to the `_` reset that
/// clears auto back to ask (regression: Plan plus Auto cycle wrong).
#[test]
fn cycle_mode_plan_plus_auto_keeps_auto_not_reset() {
    let mut app = test_app_with_agent();
    app.current_ui.permission_mode = Some("auto".into());
    app.agents.get_mut(&AgentId(0)).unwrap().plan_mode_pending = Some(true);

    let effects = dispatch(Action::CycleMode, &mut app);

    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "Plan+Auto cycle must not enable yolo"
    );
    assert_eq!(app.agents[&AgentId(0)].plan_mode_pending, Some(false));
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("auto"),
        "Plan+Auto must keep Auto, not reset to ask"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SetSessionMode { .. })),
        "expected plan exit SetSessionMode, got {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                canonical: "auto",
                ..
            }
        )),
        "expected PersistPermissionMode(auto), got {effects:?}"
    );
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                canonical: "ask",
                ..
            }
        )),
        "Plan+Auto must not persist 'ask' (would mean a reset), got {effects:?}"
    );
}

/// Security regression (0.2.89): launch with `permission_mode =
/// "always-approve"`, Shift+Tab on the welcome screen (no session yet) to
/// Normal, then start the session. The cycle must persist "ask" to disk or
/// the stale config re-arms yolo on the next launch while the footer shows
/// Normal.
#[test]
fn cycle_mode_pre_session_always_approve_to_normal_persists_ask() {
    let mut app = test_app_with_agent();
    // Launch-seeded always-approve: global default (read by SessionFlags at
    // CreateSession) + the per-agent flag the cycle arm matches on.
    app.default_yolo = true;
    {
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.session.session_id = None;
        agent.session.yolo_mode = true;
    }

    let effects = dispatch(Action::CycleMode, &mut app);

    let agent = &app.agents[&AgentId(0)];
    assert!(
        !agent.session.is_yolo(),
        "Always-Approve → Normal must clear the staged yolo"
    );
    assert!(
        !app.default_yolo,
        "global default must clear so CreateSession seeds yoloMode=false"
    );
    assert_eq!(app.current_ui.permission_mode.as_deref(), Some("ask"));
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                canonical: "ask",
                session_id: None,
                persist: crate::app::actions::PermissionModePersist::BestEffort,
            }
        )),
        "pre-session Always-Approve → Normal must persist 'ask' \
         (stale config.toml relaunches yolo), got {effects:?}"
    );
}

/// Negative control for the pre-session persist: Normal → Plan changes the
/// SESSION mode, not the permission mode — nothing to write to
/// `ui.permission_mode` (matches the with-session Normal → Plan arm).
#[test]
fn cycle_mode_pre_session_normal_to_plan_does_not_persist_permission_mode() {
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().session.session_id = None;

    let effects = dispatch(Action::CycleMode, &mut app);

    assert_eq!(app.agents[&AgentId(0)].plan_mode_pending, Some(true));
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::PersistPermissionMode { .. })),
        "Normal → Plan must not touch the persisted permission mode, got {effects:?}"
    );
}

/// No-active-agent → no-op (no panic, no effect, no mutation).
///
/// **Telemetry-no-op contract.** The
/// `set_yolo_mode_inner` early-return at the `app.active_view`
/// guard MUST precede the `echo_agent_telemetry::log_event` call
/// — otherwise a no-agent dispatch would leak a `YoloToggled`
/// telemetry event for an action that never happened. We can't
/// easily intercept the telemetry library from a unit test, but
/// we DO pin the absence-of-side-effects contract via the
/// SHARED-state defense below. A future refactor that hoists
/// telemetry above the guard would change the testable side
/// effects (Effect emission, default_yolo, current_ui mutation
/// all gated by the same guard), so this test catches the
/// regression class.
#[test]
fn set_yolo_mode_no_op_when_no_active_agent() {
    let mut app = test_app(); // no agent, active_view = Welcome
    let default_yolo_before = app.default_yolo;
    let perm_mode_before = app.current_ui.permission_mode.clone();

    let effects = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(
        effects.is_empty(),
        "no active agent → no Effect, got {effects:?}",
    );
    // Defense-in-depth: SHARED state must NOT mutate.
    assert_eq!(app.default_yolo, default_yolo_before);
    assert_eq!(app.current_ui.permission_mode, perm_mode_before);
}

/// Refresh contract: dispatching `SetYoloMode(true)` while the
/// settings modal is open must refresh the modal's
/// `pager_snapshot.yolo_mode` AND `ui_snapshot.permission_mode`.
/// Without this, the indicator stays stale (the stale-snapshot
/// pattern, applied to permission_mode).
#[test]
fn set_yolo_mode_refreshes_open_modal_snapshots() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::OpenSettings, &mut app);

    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("expected Settings modal after OpenSettings dispatch")
    };
    assert!(
        !state.pager_snapshot.yolo_mode,
        "snapshot at open should be false (agent default)",
    );

    let _ = dispatch(Action::SetYoloMode(true), &mut app);

    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("Settings modal must remain open across the dispatch")
    };
    assert!(
        state.pager_snapshot.yolo_mode,
        "pager_snapshot.yolo_mode must be refreshed to true",
    );
    assert_eq!(
        state.ui_snapshot.permission_mode.as_deref(),
        Some("always-approve"),
        "ui_snapshot.permission_mode must also refresh",
    );

    // Verify the modal would now toggle in the OTHER direction.
    let cur_value = crate::settings::current_value_for(
        "permission_mode",
        &state.ui_snapshot,
        &state.pager_snapshot,
    )
    .expect("permission_mode must resolve");
    assert_eq!(
        cur_value,
        crate::settings::SettingValue::Enum("always-approve"),
        "current_value_for must read the refreshed snapshot",
    );
}

// ----------------------------------------------------------------
// Dispatch-layer integration tests for
// `Action::SetPermissionMode(kind)`.
//
// The kind enum, the picker outcome layer (which Action
// gets dispatched), and the effect-routing layer (route fn) are
// covered elsewhere. The middle layer —
// `dispatch(Action::SetPermissionMode(kind))` →
// `set_permission_mode` → state mutation + effect emission, AND
// the `apply_setting_rollback` arm for the "default" canonical —
// is covered by these tests.
//
// The headline contract pinned here:
//   - `app.current_ui.permission_mode == kind.as_canonical()`
//     after dispatch (including the post-inner override for
//     `Default`, which the inner's bool projection would
//     otherwise collapse onto "ask").
//   - `Effect::PersistPermissionMode { canonical, persist:
//     WithRollback(prev_canonical), .. }` correctly captures the
//     PRIOR canonical via the LIVE-precedence
//     `capture_prev_permission_canonical` helper. The headline
//     case: `Default → AlwaysApprove` must
//     produce `WithRollback("default")` so a disk failure rolls
//     back to "default", not "ask".
//   - `permission_mode_toast(kind)` is the dispatched toast for
//     each kind: `Default → "✓ Permission mode: Default"`,
//     `Ask → "✓ Permission mode: Ask"`, `AlwaysApprove → ⚠
//     yolo_toast(true)`.
//   - `apply_setting_rollback("permission_mode", Enum("default"))`
//     restores `current_ui.permission_mode = Some("default")`
//     (preserves canonical; doesn't re-emit any Effect).
// ----------------------------------------------------------------

#[test]
fn set_permission_mode_default_overrides_canonical_to_default() {
    use crate::app::actions::PermissionModeKind;
    let mut app = test_app_with_agent();
    // Starts at default (yolo=false, permission_mode=None).
    assert!(!app.agents[&AgentId(0)].session.is_yolo());

    let effects = dispatch(
        Action::SetPermissionMode(PermissionModeKind::Default),
        &mut app,
    );

    // Yolo stays false (Default projects onto bool=false).
    assert!(!app.agents[&AgentId(0)].session.is_yolo());
    // Headline contract: the canonical override survives
    // `set_yolo_mode_inner`'s bool-projection write to "ask".
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("default"),
        "PR 11 R1: set_permission_mode(Default) must override current_ui to 'default' \
             — the inner's bool projection would otherwise leave it at 'ask'",
    );

    // Effect carries the canonical "default" + rollback to the
    // pre-dispatch canonical "ask" (the prior `permission_mode`
    // was None, falling through to "ask").
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistPermissionMode {
            canonical,
            persist,
            session_id,
        } => {
            assert_eq!(*canonical, "default");
            assert_eq!(
                *persist,
                crate::app::actions::PermissionModePersist::WithRollback("ask"),
                "rollback target captured from prior canonical (was None → 'ask')",
            );
            assert!(session_id.is_some());
        }
        other => panic!("expected PersistPermissionMode, got {other:?}"),
    }

    // Toast is the dedicated Default string, NOT
    // `yolo_toast(false)` (`"✓ Always-approve: off"` would be
    // wrong-brand for a Permission-mode picker commit).
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast must be set");
    assert_eq!(
        toast, "\u{2713} Permission mode: Default",
        "PR 11 R1 G-3 #12: Default toast is value-neutral; no parenthetical that lies \
             about runtime equivalence",
    );
}

#[test]
fn set_permission_mode_always_approve_from_default_captures_prev_canonical() {
    use crate::app::actions::PermissionModeKind;
    let mut app = test_app_with_agent();
    // Establish prior state: user is in "default" (yolo=false,
    // current_ui.permission_mode = Some("default")). This is the
    // exact starting state the rollback path was designed to preserve
    // across rollback.
    let _ = dispatch(
        Action::SetPermissionMode(PermissionModeKind::Default),
        &mut app,
    );
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("default"),
        "test setup: prior canonical must be 'default'",
    );

    // Now flip to AlwaysApprove.
    let effects = dispatch(
        Action::SetPermissionMode(PermissionModeKind::AlwaysApprove),
        &mut app,
    );

    assert!(app.agents[&AgentId(0)].session.is_yolo());
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("always-approve"),
    );

    // **Headline rollback-preservation contract.** Disk failure
    // here must roll back to "default", NOT "ask" (which a bool
    // projection of the prior yolo=false would produce).
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistPermissionMode {
            canonical, persist, ..
        } => {
            assert_eq!(*canonical, "always-approve");
            assert_eq!(
                *persist,
                crate::app::actions::PermissionModePersist::WithRollback("default"),
                "PR 11 R1 headline: prior canonical 'default' must be preserved in the \
                     rollback payload, NOT collapsed onto 'ask' by a bool projection",
            );
        }
        other => panic!("expected PersistPermissionMode, got {other:?}"),
    }

    // Toast is the destructive ⚠ variant — AlwaysApprove still
    // reuses yolo_toast(true) because the user IS enabling YOLO,
    // and the weight of the destructive warning is correct.
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast must be set");
    assert_eq!(
        toast, "\u{26A0} Always-approve ON: all tool actions auto-run",
        "AlwaysApprove arm preserves the destructive yolo_toast(true) — the warning \
             weight is correct for the YOLO transition",
    );
}

/// Mirror of the above for `SetYoloMode(true)` (Ctrl+O path) —
/// the LIVE-precedence capture must also fix the bool entry point's
/// rollback target when the on-disk mirror diverges from the LIVE
/// state.
#[test]
fn set_yolo_mode_with_live_yolo_and_default_ui_mirror_rolls_back_to_default() {
    let mut app = test_app_with_agent();
    // Manually set up the divergence: agent yolo=true (LIVE),
    // current_ui.permission_mode = Some("default") (mirror says
    // user picked "Default" before something flipped yolo).
    app.agents.get_mut(&AgentId(0)).unwrap().session.yolo_mode = true;
    app.default_yolo = true;
    app.current_ui.permission_mode = Some("default".into());

    // Ctrl+O (SetYoloMode(false)) — exit YOLO. Without the LIVE
    // branch fix, the rollback canonical would be derived purely
    // from the bool `prev` → "always-approve", losing the
    // "default" preference. With the LIVE branch, `prev_yolo=true`
    // returns "always-approve" — which IS what the rollback
    // should restore (the user was effectively in YOLO at
    // dispatch time, regardless of what the mirror said).
    //
    // The key invariant: rollback target == what the modal would
    // have shown via `current_value_for` at dispatch time. Since
    // LIVE yolo wins in `current_value_for`, it must also win
    // here.
    let effects = dispatch(Action::SetYoloMode(false), &mut app);
    match &effects[0] {
        Effect::PersistPermissionMode { persist, .. } => {
            assert_eq!(
                *persist,
                crate::app::actions::PermissionModePersist::WithRollback("always-approve"),
                "LIVE yolo=true must dominate the mirror's 'default' for the rollback \
                     canonical — matches current_value_for's precedence rule",
            );
        }
        other => panic!("expected PersistPermissionMode, got {other:?}"),
    }
}

/// Direct unit test for `permission_mode_toast`
/// at the seam — pins the brand-consistent strings for each arm.
/// Defense-in-depth on top of the dispatch-layer toast assertions
/// above; catches a future refactor that changes the strings
/// without going through dispatch.
#[test]
fn permission_mode_toast_returns_brand_consistent_strings() {
    use crate::app::actions::PermissionModeKind;
    assert_eq!(
        permission_mode_toast(PermissionModeKind::Default),
        "\u{2713} Permission mode: Default",
    );
    assert_eq!(
        permission_mode_toast(PermissionModeKind::Ask),
        "\u{2713} Permission mode: Ask",
    );
    // AlwaysApprove still goes through `yolo_toast(true)` —
    // destructive variant.
    assert_eq!(
        permission_mode_toast(PermissionModeKind::AlwaysApprove),
        "\u{26A0} Always-approve ON: all tool actions auto-run",
    );
}

/// Normal → Plan: cycle_mode requests plan_mode_pending but does
/// NOT touch YOLO state. Pins the no-yolo-mutation invariant.
#[test]
fn dispatch_cycle_mode_normal_to_plan_does_not_touch_yolo() {
    let mut app = test_app_with_agent();
    assert!(!app.agents[&AgentId(0)].session.is_yolo());

    let effects = dispatch(Action::CycleMode, &mut app);

    // Plan mode requested.
    assert_eq!(
        app.agents[&AgentId(0)].plan_mode_pending,
        Some(true),
        "Normal → Plan must set plan_mode_pending"
    );
    // YOLO state unchanged.
    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "Normal → Plan must NOT flip YOLO state",
    );
    assert!(!app.default_yolo, "app.default_yolo must remain false");
    // Single effect: SetSessionMode (no PersistPermissionMode).
    assert_eq!(effects.len(), 1, "Normal → Plan must emit one effect");
    assert!(
        matches!(effects[0], Effect::SetSessionMode { .. }),
        "Normal → Plan effect must be SetSessionMode, got {:?}",
        effects[0],
    );
}

/// `active_agent_plan_nudge_state` reports the plan-nudge visibility and the
/// optimistic plan state — the two inputs to the shift+tab acceptance guard.
#[test]
fn active_agent_plan_nudge_state_tracks_nudge_and_plan() {
    let mut app = test_app_with_agent();
    // No tip, not in plan.
    assert_eq!(active_agent_plan_nudge_state(&app), (false, false));
    // Plan nudge on the active agent, still not in plan.
    let _ = app.agents.get_mut(&AgentId(0)).unwrap().ephemeral_tip.show(
        crate::tips::plan_nudge::plan_nudge_tip(),
        &mut std::collections::HashMap::new(),
    );
    assert_eq!(active_agent_plan_nudge_state(&app), (true, false));
    // Entering plan mode flips the second element (the accept condition:
    // nudge showing && !before && after).
    app.agents.get_mut(&AgentId(0)).unwrap().plan_mode_pending = Some(true);
    assert_eq!(active_agent_plan_nudge_state(&app), (true, true));
}

/// Shift+Tab into plan mode while the nudge shows enters plan mode AND
/// retires the nudge (the accept's clear-on-accept). The co-located clear is
/// the observable side effect that gives this test teeth — the `log_event`
/// itself has no in-process capture sink.
#[test]
fn cycle_into_plan_with_nudge_showing_accepts_and_retires_nudge() {
    let mut app = test_app_with_agent();
    let _ = app.agents.get_mut(&AgentId(0)).unwrap().ephemeral_tip.show(
        crate::tips::plan_nudge::plan_nudge_tip(),
        &mut std::collections::HashMap::new(),
    );

    let _ = dispatch(Action::CycleMode, &mut app);

    assert_eq!(
        app.agents[&AgentId(0)].plan_mode_pending,
        Some(true),
        "shift+tab still enters plan mode when the nudge is showing"
    );
    assert_eq!(
        app.agents[&AgentId(0)].ephemeral_tip.current_key(),
        None,
        "accepting the nudge via shift+tab must retire it (one impression → one accept)"
    );
}

/// A Normal→Plan cycle with a NON-nudge tip on screen still enters plan mode
/// and leaves that tip intact: the accept's clear is keyed to PLAN_NUDGE_KEY,
/// not "any tip". With no telemetry sink this pins plan-entry + keyed-clear
/// correctness, not the emit-gating itself.
#[test]
fn cycle_into_plan_without_nudge_leaves_other_tip_intact() {
    let mut app = test_app_with_agent();
    let _ = app.agents.get_mut(&AgentId(0)).unwrap().ephemeral_tip.show(
        crate::tips::clipboard_focus::clipboard_image_tip(),
        &mut std::collections::HashMap::new(),
    );

    let _ = dispatch(Action::CycleMode, &mut app);

    assert_eq!(
        app.agents[&AgentId(0)].plan_mode_pending,
        Some(true),
        "shift+tab still enters plan mode"
    );
    assert_eq!(
        app.agents[&AgentId(0)].ephemeral_tip.current_key(),
        Some(crate::tips::clipboard_focus::CLIPBOARD_IMAGE_TIP_KEY),
        "a shift+tab with no plan nudge up must not retire another tip",
    );
}

/// Auto → Always-Approve: cycle_mode delegates the YOLO ON
/// transition through `set_yolo_mode_inner`. State mutations
/// happen (yolo_mode flips, current_ui.permission_mode updates,
/// default_yolo flips). Effects include PersistPermissionMode.
#[test]
fn dispatch_cycle_mode_plan_to_always_approve_delegates_through_inner() {
    let mut app = test_app_with_agent();
    // Start in Auto (Plan → Auto is a prior step; see cycle_mode_plan_to_auto).
    // The cycle reads the per-session `is_auto()` flag, not the global mirror.
    app.current_ui.permission_mode = Some("auto".into());
    app.agents.get_mut(&AgentId(0)).unwrap().session.auto_mode = true;

    let effects = dispatch(Action::CycleMode, &mut app);

    // YOLO ON via delegation through set_yolo_mode_inner.
    assert!(
        app.agents[&AgentId(0)].session.is_yolo(),
        "Auto → Always-Approve must flip yolo_mode through the inner",
    );
    assert!(app.default_yolo, "default_yolo must flip in lock-step");
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("always-approve"),
        "current_ui.permission_mode must be updated by the inner",
    );

    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                persist: crate::app::actions::PermissionModePersist::BestEffort,
                canonical: "always-approve",
                ..
            }
        )),
        "expected PersistPermissionMode(always-approve, BestEffort), got {effects:?}"
    );
}

/// **Security-critical:** Auto → Always-Approve via cycle_mode
/// must drain any queued permissions with AllowOnce — same drain
/// semantics as the typed setter. This is the strongest
/// regression guard against a future refactor that breaks the
/// `set_yolo_mode_inner` delegation.
#[test]
fn dispatch_cycle_mode_plan_to_always_approve_drains_queue_via_inner() {
    use crate::views::permission_view::{PermissionFocus, PermissionViewState};
    use std::sync::Arc;

    let mut app = test_app_with_agent();
    // Start in Auto so one CycleMode enables always-approve + drain. The cycle
    // reads the per-session `is_auto()` flag, not the global mirror.
    app.current_ui.permission_mode = Some("auto".into());
    app.agents.get_mut(&AgentId(0)).unwrap().session.auto_mode = true;

    // Inject a queued permission with AllowOnce.
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
    let request = acp::RequestPermissionRequest::new(
        acp::SessionId::new(Arc::from("test-sess")),
        acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from("tc-cycle-1")),
            acp::ToolCallUpdateFields::default(),
        ),
        vec![acp::PermissionOption::new(
            acp::PermissionOptionId::new(Arc::from("opt-allow-once")),
            "Allow once",
            acp::PermissionOptionKind::AllowOnce,
        )],
    );
    let options = request.options.clone();
    app.agents
        .get_mut(&AgentId(0))
        .unwrap()
        .permission_queue
        .push_back(PermissionViewState {
            request: echo_agent_acp::AcpArgs {
                request,
                response_tx,
            },
            id: 1,
            focus: PermissionFocus::Options,
            options,
            active_idx: 0,
            bash_highlights: None,
            bash_selection_count: 0,
            bash_deny_selection_count: 0,
            bash_command_raw: None,
            mcp_scope: None,
            title: "cycle-test".to_string(),
            description: vec![],
            args_expanded: false,
            desc_scroll: 0,
            subagent_label: None,
            options_area_height: 0,
            options_scroll_offset: 0,
        });

    let _ = dispatch(Action::CycleMode, &mut app);

    // Queue drained.
    assert!(
        app.agents[&AgentId(0)].permission_queue.is_empty(),
        "cycle_mode Auto → Always-Approve must drain the queue via set_yolo_mode_inner",
    );
    // AllowOnce was sent (NOT Cancelled).
    match response_rx.try_recv() {
        Ok(Ok(acp::RequestPermissionResponse {
            outcome:
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome {
                    option_id,
                    ..
                }),
            ..
        })) => {
            assert_eq!(
                option_id,
                acp::PermissionOptionId::new(Arc::from("opt-allow-once")),
                "cycle_mode drain must select AllowOnce (NOT Cancelled) — \
                     security regression: queued permissions silently rejected when \
                     user cycles through Always-Approve",
            );
        }
        other => {
            panic!("expected AllowOnce Selected response from cycle_mode drain, got {other:?}",)
        }
    }
}

/// Always-Approve + plan nudge showing: Shift+Tab jumps to Plan (not Normal),
/// clears yolo, retires the nudge, and persists ask.
#[test]
fn cycle_always_approve_with_nudge_jumps_to_plan() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(app.agents[&AgentId(0)].session.is_yolo());
    let _ = app.agents.get_mut(&AgentId(0)).unwrap().ephemeral_tip.show(
        crate::tips::plan_nudge::plan_nudge_tip(),
        &mut std::collections::HashMap::new(),
    );

    let effects = dispatch(Action::CycleMode, &mut app);

    assert_eq!(
        app.agents[&AgentId(0)].plan_mode_pending,
        Some(true),
        "nudge + Always-Approve must jump to Plan"
    );
    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "jump to Plan must clear yolo"
    );
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("ask"),
        "canonical permission mode must leave always-approve"
    );
    assert_eq!(
        app.agents[&AgentId(0)].ephemeral_tip.current_key(),
        None,
        "accepting the nudge must retire it"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SetSessionMode { mode_id, .. } if &*mode_id.0 == "plan"
        )),
        "expected SetSessionMode(plan), got {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                canonical: "ask",
                ..
            }
        )),
        "expected PersistPermissionMode(ask), got {effects:?}"
    );
}

/// Auto + plan nudge showing: Shift+Tab jumps to Plan (not Always-Approve),
/// clears auto, retires the nudge, and persists ask.
#[test]
fn cycle_auto_with_nudge_jumps_to_plan() {
    let mut app = test_app_with_agent();
    app.current_ui.permission_mode = Some("auto".into());
    app.agents.get_mut(&AgentId(0)).unwrap().session.auto_mode = true;
    let _ = app.agents.get_mut(&AgentId(0)).unwrap().ephemeral_tip.show(
        crate::tips::plan_nudge::plan_nudge_tip(),
        &mut std::collections::HashMap::new(),
    );

    let effects = dispatch(Action::CycleMode, &mut app);

    assert_eq!(
        app.agents[&AgentId(0)].plan_mode_pending,
        Some(true),
        "nudge + Auto must jump to Plan"
    );
    assert!(
        !app.agents[&AgentId(0)].session.is_auto(),
        "jump to Plan must clear auto"
    );
    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "jump to Plan must not enable yolo"
    );
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("ask"),
        "canonical permission mode must leave auto"
    );
    assert_eq!(
        app.agents[&AgentId(0)].ephemeral_tip.current_key(),
        None,
        "accepting the nudge must retire it"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SetSessionMode { mode_id, .. } if &*mode_id.0 == "plan"
        )),
        "expected SetSessionMode(plan), got {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                canonical: "ask",
                ..
            }
        )),
        "expected PersistPermissionMode(ask), got {effects:?}"
    );
}

/// Shared/peek cycle body with Always-Approve + nudge must NOT jump to Plan:
/// it takes the ring (→ Normal), leaves the nudge intact, and emits no
/// SetSessionMode. Pins that collapse+accept live only in `dispatch_cycle_mode`.
#[test]
fn dispatch_cycle_mode_and_sync_always_approve_with_nudge_takes_ring_to_normal() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(app.agents[&AgentId(0)].session.is_yolo());
    let _ = app.agents.get_mut(&AgentId(0)).unwrap().ephemeral_tip.show(
        crate::tips::plan_nudge::plan_nudge_tip(),
        &mut std::collections::HashMap::new(),
    );

    let effects = dispatch_cycle_mode_and_sync(&mut app);

    assert_ne!(
        app.agents[&AgentId(0)].plan_mode_pending,
        Some(true),
        "shared body must not enter Plan when nudge is showing"
    );
    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "Always-Approve → Normal must still clear yolo"
    );
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("ask"),
        "ring lands on Normal/ask"
    );
    assert_eq!(
        app.agents[&AgentId(0)].ephemeral_tip.current_key(),
        Some(crate::tips::plan_nudge::PLAN_NUDGE_KEY),
        "shared/peek body must not retire the nudge"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::SetSessionMode { .. })),
        "Always-Approve → Normal must not SetSessionMode, got {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistPermissionMode {
                canonical: "ask",
                ..
            }
        )),
        "expected PersistPermissionMode(ask), got {effects:?}"
    );
}

/// Always-Approve → Normal: cycle_mode delegates the YOLO OFF
/// transition through `set_yolo_mode_inner`. No queue drain (the
/// inner's `if new` guard skips drain on OFF transition).
#[test]
fn dispatch_cycle_mode_always_approve_to_normal_delegates_off() {
    let mut app = test_app_with_agent();
    // Enter Always-Approve state via the typed setter (sets up
    // the lock-step properly).
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(app.agents[&AgentId(0)].session.is_yolo());

    // Clear the toast so we're not confused about which dispatch
    // set it.
    app.agents.get_mut(&AgentId(0)).unwrap().toast = None;

    let effects = dispatch(Action::CycleMode, &mut app);

    // YOLO OFF via delegation.
    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "Always-Approve → Normal must flip yolo_mode off through the inner",
    );
    assert!(!app.default_yolo, "default_yolo must flip in lock-step");
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("ask"),
        "current_ui.permission_mode must update to 'ask'",
    );

    // Single effect: PersistPermissionMode{BestEffort}.
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistPermissionMode {
            persist, canonical, ..
        } => {
            assert_eq!(
                *persist,
                crate::app::actions::PermissionModePersist::BestEffort,
            );
            assert_eq!(*canonical, "ask");
        }
        other => panic!("expected PersistPermissionMode, got {other:?}"),
    }
}

/// `Action::SetTheme("auto")` enables `AUTO_MODE`, persists
/// `"auto"` (the canonical), and applies the resolved theme.
/// Specifically the "auto enablement" branch.
#[test]
fn set_theme_auto_enables_auto_mode_and_persists_auto() {
    use crate::settings::SettingValue;
    with_theme_test_env(|| {
        // Mock the system appearance so resolve_auto deterministically
        // picks a known concrete theme.
        crate::theme::system_appearance::set_mock(Some(
            crate::theme::system_appearance::SystemAppearance::Dark,
        ));

        let mut app = test_app_with_agent();
        assert!(!crate::theme::cache::is_auto_mode());
        let effects = dispatch(Action::SetTheme("auto".into()), &mut app);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::PersistSetting { key, value, .. } => {
                assert_eq!(*key, "theme");
                assert_eq!(
                    *value,
                    SettingValue::Enum("auto"),
                    "auto commit persists `auto` (NOT the resolved concrete theme)",
                );
            }
            other => panic!("expected PersistSetting, got {other:?}"),
        }
        assert_eq!(app.current_ui.theme.as_deref(), Some("auto"));
        assert!(
            crate::theme::cache::is_auto_mode(),
            "auto commit must enable AUTO_MODE",
        );
    });
}

// ────────────────────────────────────────────────────────────────────
// set_plan_mode dispatch-level coverage.
//
// Mirrors the `coding_data_sharing` and `yolo` test
// patterns. These exercise the dispatch path directly (not the
// modal Enter path or the slash-command parser path), so they cover
// the same plumbing every entry point ultimately funnels through.
// ────────────────────────────────────────────────────────────────────

/// Idempotent ON: dispatcher sees `prev == new`,
/// toasts but emits NO Effect (saves a wasted ACP round-trip).
/// State stays unchanged. Mirrors `set_coding_data_sharing_idempotent_opt_in`.
#[test]
fn set_plan_mode_idempotent_on() {
    let mut app = test_app_with_agent();
    // Seed: already in plan mode (effective state).
    app.agents.get_mut(&AgentId(0)).unwrap().plan_mode_pending = Some(true);

    let effects = dispatch(
        Action::SetPlanMode(crate::app::actions::PlanModeKind::On),
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "idempotent ON re-dispatch must NOT emit Effect (wasted ACP round-trip)"
    );
    // State unchanged — neither pending nor active flips.
    let agent = app.agents.get(&AgentId(0)).unwrap();
    assert_eq!(
        agent.plan_mode_pending,
        Some(true),
        "idempotent path must NOT mutate plan_mode_pending"
    );
    assert!(
        !agent.plan_mode_active,
        "idempotent path must NOT touch plan_mode_active"
    );

    let toast = read_toast(&app);
    assert!(
        toast.contains("Plan mode"),
        "idempotent ON must still toast (slash command users typing `/plan` while \
             already in plan mode need confirmation): {toast}",
    );
    assert!(
        toast.contains("on"),
        "idempotent ON toast must surface the value: {toast}",
    );
    assert!(
        toast.contains('\u{2713}'),
        "plan_mode toast uses ✓ (non-destructive in both directions): {toast}",
    );
}

/// Idempotent OFF: dispatcher sees `prev == new`,
/// toasts but emits NO Effect. Mirrors `set_coding_data_sharing_idempotent_opt_out`.
#[test]
fn set_plan_mode_idempotent_off() {
    let mut app = test_app_with_agent();
    // Seed: NOT in plan mode (the default state from test_app_with_agent
    // already satisfies this — both plan_mode_active = false and
    // plan_mode_pending = None — but assert it for clarity).
    {
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(!agent.plan_mode_active);
        assert!(agent.plan_mode_pending.is_none());
    }

    let effects = dispatch(
        Action::SetPlanMode(crate::app::actions::PlanModeKind::Off),
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "idempotent OFF re-dispatch must NOT emit Effect"
    );
    // State unchanged.
    let agent = app.agents.get(&AgentId(0)).unwrap();
    assert!(agent.plan_mode_pending.is_none());
    assert!(!agent.plan_mode_active);

    let toast = read_toast(&app);
    assert!(toast.contains("Plan mode"));
    assert!(toast.contains("off"));
    assert!(toast.contains('\u{2713}'));
}

/// Toast format contract: both directions
/// produce `"✓ Plan mode: <on|off>"`. Mirrors `set_compact_mode_toast_format`.
/// A regression to capital "On"/"Off" or
/// missing the ✓ glyph would fail this test.
#[test]
fn plan_mode_toast_format() {
    let mut app = test_app_with_agent();
    let _ = dispatch(
        Action::SetPlanMode(crate::app::actions::PlanModeKind::On),
        &mut app,
    );
    let toast = read_toast(&app);
    assert!(toast.contains("Plan mode"));
    assert!(
        toast.contains(": on"),
        "ON toast must use lowercase 'on' (consistency with multiline/compact toasts): {toast}",
    );
    assert!(
        !toast.contains(": On"),
        "ON toast must NOT use capital 'On' (PR 10 R1 G-3 #1 fix): {toast}",
    );
    assert!(toast.contains('\u{2713}'));

    // Bring the agent into plan mode for the OFF toast assertion.
    // (The previous SetPlanMode(On) set pending = Some(true); we
    // need the OFF dispatch to go through the real mutation path,
    // so we let the optimistic state stand.)
    let _ = dispatch(
        Action::SetPlanMode(crate::app::actions::PlanModeKind::Off),
        &mut app,
    );
    let toast = read_toast(&app);
    assert!(toast.contains("Plan mode"));
    assert!(
        toast.contains(": off"),
        "OFF toast must use lowercase 'off': {toast}",
    );
    assert!(
        !toast.contains(": Off"),
        "OFF toast must NOT use capital 'Off': {toast}",
    );
}

/// Pending-wins precedence test.
/// The dispatcher reads the EFFECTIVE state as
/// `pending.unwrap_or(active)`. Verify that an idempotent guard
/// keyed off `pending` correctly short-circuits even when
/// `active` disagrees. This locks the "prefer optimistic pending"
/// contract against a future refactor that accidentally swaps
/// the precedence.
#[test]
fn set_plan_mode_idempotency_uses_pending_over_active() {
    let mut app = test_app_with_agent();
    // Seed a divergent state: active=false (the shell hasn't
    // confirmed yet), pending=Some(true) (the user just toggled).
    {
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.plan_mode_active = false;
        agent.plan_mode_pending = Some(true);
    }

    // EFFECTIVE state is true (pending wins). Dispatching ON
    // again must hit the idempotent fast path even though
    // `active` is still false.
    let effects = dispatch(
        Action::SetPlanMode(crate::app::actions::PlanModeKind::On),
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "idempotent guard must read pending (Some(true)) over active (false) — \
             EFFECTIVE state is the contract"
    );

    // Conversely, dispatching OFF in this state should NOT be
    // idempotent — EFFECTIVE state is true, target is false, so
    // a real transition fires.
    let effects = dispatch(
        Action::SetPlanMode(crate::app::actions::PlanModeKind::Off),
        &mut app,
    );
    assert_eq!(
        effects.len(),
        1,
        "OFF from EFFECTIVE-ON must emit Effect::SetSessionMode (not idempotent)"
    );
    assert!(
        matches!(&effects[0], Effect::SetSessionMode { mode_id, .. } if &*mode_id.0 == "default"),
        "OFF transition must emit SetSessionMode(default): {effects:?}"
    );
    let agent = app.agents.get(&AgentId(0)).unwrap();
    assert_eq!(
        agent.plan_mode_pending,
        Some(false),
        "OFF transition must set optimistic pending to Some(false)"
    );
}
