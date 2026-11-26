//! Tests for settings setters, toggles, resets, and rollback.
use super::*;
/// `Action::ToggleVimMode` flips the active agent's `vim_mode` field,
/// updates the in-process pager cache so future agents pick it up
/// via `load_vim_mode`, emits `Effect::PersistSetting` so the new
/// value is written to `[ui].vim_mode` in config.toml, and a second
/// toggle restores the original.
#[test]
fn toggle_vim_mode_flips_state_and_persistence_cache() {
    crate::appearance::cache::set_vim_mode(false);
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    if let Some(agent) = app.agents.get_mut(&id) {
        agent.vim_mode = false;
    }
    let effects = dispatch(Action::ToggleVimMode, &mut app);
    assert_eq!(
        effects.len(),
        1,
        "toggle must emit exactly one Effect::PersistSetting so the \
             new value lands in config.toml, got {effects:?}",
    );
    assert!(
        matches!(
            &effects[0],
            Effect::PersistSetting {
                key: "vim_mode",
                value: crate::settings::SettingValue::Bool(true),
                rollback_value: crate::settings::SettingValue::Bool(false),
            }
        ),
        "unexpected effect: {:?}",
        effects[0],
    );
    assert!(
        app.agents[&id].vim_mode,
        "active agent should be in vim mode after toggle"
    );
    assert!(
        crate::appearance::cache::load_vim_mode(),
        "pager cache must reflect new value so future agents pick it up"
    );
    let effects = dispatch(Action::ToggleVimMode, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::PersistSetting {
                key: "vim_mode",
                value: crate::settings::SettingValue::Bool(false),
                rollback_value: crate::settings::SettingValue::Bool(true),
            }]
        ),
        "second toggle must also persist, got {effects:?}",
    );
    assert!(!app.agents[&id].vim_mode, "toggling again flips it back");
    assert!(
        !crate::appearance::cache::load_vim_mode(),
        "cache must follow the second toggle"
    );
}
/// End-to-end: in vim mode, Tab from the prompt focuses scrollback,
/// and then `j` navigates the scrollback (does NOT bounce back to the
/// prompt). Drives the real handle_input → dispatch loop.
#[test]
fn agent_vim_tab_focuses_scrollback_then_j_navigates() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    crate::appearance::cache::set_vim_mode(true);
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.active_view = ActiveView::Agent(id);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.vim_mode = true;
        agent.active_pane = ActivePane::Prompt;
    }
    let tab = Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    if let crate::app::app_view::InputOutcome::Action(a) = app.handle_input(&tab) {
        let _ = dispatch(a, &mut app);
    }
    assert_eq!(
        app.agents[&id].active_pane,
        ActivePane::Scrollback,
        "Tab in the prompt must focus scrollback",
    );
    let j = Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    let outcome = app.handle_input(&j);
    assert!(
        matches!(
            outcome,
            crate::app::app_view::InputOutcome::Action(Action::SelectNext)
        ),
        "vim j in scrollback must navigate, got {outcome:?}",
    );
    assert_eq!(
        app.agents[&id].active_pane,
        ActivePane::Scrollback,
        "j must keep focus on scrollback, not bounce to the prompt",
    );
    crate::appearance::cache::set_vim_mode(false);
}
/// `/vim-mode` (ToggleVimMode) must propagate to OPEN subagent views,
/// not just top-level agents. Otherwise a user inside a subagent view
/// toggles vim, presses Tab + j, and the keystroke forwards to the
/// prompt (vim-OFF fallback) because the subagent view kept its stale
/// `vim_mode = false`.
#[test]
fn toggle_vim_mode_propagates_to_open_subagent_views() {
    crate::appearance::cache::set_vim_mode(false);
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let child_session = make_test_agent_session(&app, AgentId(0), "child-session");
    let mut child = AgentView::new(child_session, ScrollbackState::new());
    child.vim_mode = false;
    {
        let parent = app.agents.get_mut(&id).unwrap();
        parent.vim_mode = false;
        parent
            .subagent_views
            .insert("child-1".to_string(), Box::new(child));
    }
    let _ = dispatch(Action::ToggleVimMode, &mut app);
    assert!(app.agents[&id].vim_mode, "parent picks up the toggle");
    assert!(
        app.agents[&id].subagent_views["child-1"].vim_mode,
        "an open subagent view must also pick up the vim toggle",
    );
}
/// `/vim-mode` must toggle vim from the DASHBOARD too (not just an
/// agent view) — previously it early-returned unless an agent was
/// active, so it was a silent no-op and the overview's j/k never
/// turned on. Turning vim ON also focuses the overview so j/k
/// navigate immediately; turning it OFF returns focus to the input.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn toggle_vim_mode_works_on_dashboard_and_focuses_overview() {
    crate::appearance::cache::set_vim_mode(false);
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    assert!(matches!(app.active_view, ActiveView::AgentDashboard));
    app.dashboard.as_mut().unwrap().list_focused = false;
    let _ = dispatch(Action::ToggleVimMode, &mut app);
    assert!(
        crate::appearance::cache::load_vim_mode(),
        "/vim-mode must toggle vim ON from the dashboard",
    );
    assert!(
        app.dashboard.as_ref().unwrap().list_focused,
        "turning vim on focuses the overview so j/k navigate immediately",
    );
    let _ = dispatch(Action::ToggleVimMode, &mut app);
    assert!(
        !crate::appearance::cache::load_vim_mode(),
        "second /vim-mode must toggle vim OFF",
    );
    assert!(
        !app.dashboard.as_ref().unwrap().list_focused,
        "turning vim off returns focus to the input",
    );
    crate::appearance::cache::set_vim_mode(false);
}
#[test]
fn plugin_cta_catalog_reload_empty_candidates_resets_matched_phase() {
    use crate::app::agent_view::CtaPhase;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.source_url_or_path =
            Some(echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into());
        cta.candidates = vec![cta_entry("figma", "not_installed")];
        cta.phase = CtaPhase::Matched {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        cta.hit_connect.rect = Some(ratatui::layout::Rect::new(0, 0, 9, 1));
        cta.hit_dismiss.rect = Some(ratatui::layout::Rect::new(10, 0, 3, 1));
    }
    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_NAME.into(),
            source_kind: "git".into(),
            source_url_or_path: echo_agent_plugin_marketplace::OFFICIAL_SOURCE_GIT_URL.into(),
            plugins: vec![cta_entry("figma", "installed")],
            error: None,
        }],
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PluginCtaCatalogLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    let cta = &app.agents[&id].plugin_cta;
    assert!(cta.candidates.is_empty());
    assert_eq!(cta.phase, CtaPhase::Hidden);
    assert!(cta.hit_connect.rect.is_none());
    assert!(cta.hit_dismiss.rect.is_none());
}
#[test]
fn cancel_before_first_activity_resets_state_and_discards_orphan_response() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    dispatch(Action::SendPrompt("rewind me".into()), &mut app);
    assert!(app.agents[&id].session.state.is_turn_running());
    assert_eq!(app.agents[&id].scrollback.len(), 1);
    assert!(app.agents[&id].turn_started_at.is_some());
    let cancelled_pid = app.agents[&id].session.current_prompt_id.clone();
    assert!(cancelled_pid.is_some());
    let effects = dispatch(Action::CancelTurn, &mut app);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::CancelTurn { .. }));
    assert!(app.agents[&id].session.state.is_idle());
    assert_eq!(app.agents[&id].prompt.text(), "rewind me");
    assert_eq!(app.agents[&id].scrollback.len(), 0);
    assert!(app.agents[&id].turn_started_at.is_none());
    assert!(app.agents[&id].activity_started_at.is_none());
    assert!(app.agents[&id].last_activity.is_none());
    assert!(app.agents[&id].session.in_flight_prompt.is_none());
    assert!(app.agents[&id].session.current_prompt_id.is_none());
    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Ok(acp::PromptResponse::new(acp::StopReason::Cancelled).meta(
                serde_json::json!({ "promptId": cancelled_pid })
                    .as_object()
                    .cloned(),
            )),
            http_status: None,
            prompt_id: None,
        }),
        &mut app,
    );
    assert!(app.agents[&id].session.state.is_idle());
    assert_eq!(app.agents[&id].scrollback.len(), 0);
}
#[test]
fn set_default_model_allowed_when_agent_chat_kind() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("auto"));
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .models
        .available
        .insert(
            model_id.clone(),
            acp::ModelInfo::new(model_id.clone(), "Auto".to_string()),
        );
    app.agents.get_mut(&id).unwrap().chat_kind = true;
    let effects = dispatch(Action::SetDefaultModel(model_id.clone()), &mut app);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SwitchModel { model_id: mid, .. } if mid == &model_id
        )),
        "chat_kind must still emit SwitchModel for live chat mode switches"
    );
    assert!(app.agents[&id].session.model_switch_pending);
}
/// `/model <name>` dispatches `SetDefaultModel` which routes
/// through both `PersistSetting` and `SwitchModel`.
#[test]
fn slash_model_valid_dispatches_set_default_model_with_switch_and_persist() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("echo-agent-4.5"));
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .models
        .available
        .insert(
            model_id.clone(),
            acp::ModelInfo::new(model_id.clone(), "EchoAgent 4.5".to_string()),
        );
    let effects = dispatch(Action::SendPrompt("/model EchoAgent 4.5".into()), &mut app);
    assert_eq!(
        effects.len(),
        2,
        "expected PersistSetting + SwitchModel effects, got {effects:?}",
    );
    assert!(
        matches!(
            &effects[0],
            Effect::PersistSetting {
                key: "default_model",
                ..
            }
        ),
        "first effect must be PersistSetting(default_model), got {:?}",
        effects[0],
    );
    assert!(
        matches!(&effects[1], Effect::SwitchModel { model_id: mid, .. } if mid == &model_id),
        "second effect must be SwitchModel(<resolved id>), got {:?}",
        effects[1],
    );
    assert!(app.agents[&id].session.model_switch_pending);
}
#[test]
fn model_switch_pending_resets_correctly_across_success_and_failure() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_a = acp::ModelId::new(std::sync::Arc::from("model-a"));
    let model_b = acp::ModelId::new(std::sync::Arc::from("model-b"));
    dispatch(
        Action::SwitchModel {
            model_id: model_a.clone(),
            effort: None,
        },
        &mut app,
    );
    assert!(app.agents[&id].session.model_switch_pending);
    dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id: model_a,
            effort: None,
            result: Ok(()),
            prev_model_id: None,
        }),
        &mut app,
    );
    assert!(!app.agents[&id].session.model_switch_pending);
    dispatch(
        Action::SwitchModel {
            model_id: model_b.clone(),
            effort: None,
        },
        &mut app,
    );
    assert!(app.agents[&id].session.model_switch_pending);
    dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id: model_b,
            effort: None,
            result: Err(SwitchModelError::Other("network error".into())),
            prev_model_id: None,
        }),
        &mut app,
    );
    assert!(!app.agents[&id].session.model_switch_pending);
}
/// `set_compact_mode(app, new)` emits exactly one
/// `Effect::PersistSetting` with the correct payload — `value`
/// matches `new`, `rollback_value` matches the prior cache value.
#[test]
fn set_compact_mode_emits_persist_setting_with_correct_payload() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetCompactMode(true), &mut app);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistSetting {
            key,
            value,
            rollback_value,
        } => {
            assert_eq!(*key, "compact_mode");
            assert_eq!(value, &SettingValue::Bool(true));
            assert_eq!(rollback_value, &SettingValue::Bool(false));
        }
        other => panic!("expected PersistSetting, got {other:?}"),
    }
    assert!(app.current_ui.compact_mode);
}
/// Resizing to a short terminal (at or below `AUTO_COMPACT_MAX_ROWS`) flips
/// the derived render value (`appearance.prompt.compact`) while the user
/// setting stays untouched; growing back restores it. Resize handling emits
/// no effects at all, so nothing can persist.
#[test]
fn resize_below_threshold_derives_compact_without_touching_user_setting() {
    use crossterm::event::Event;
    let mut app = test_app_with_agent();
    assert!(!app.current_ui.compact_mode);
    assert!(!app.appearance.prompt.compact);
    let _ = app.handle_input(&Event::Resize(100, 14));
    assert!(
        app.appearance.prompt.compact,
        "short terminal must force the render value on"
    );
    assert!(
        !app.current_ui.compact_mode,
        "the user setting must stay off"
    );
    let _ = app.handle_input(&Event::Resize(100, 32));
    assert!(
        !app.appearance.prompt.compact,
        "growing back must restore the user value"
    );
    assert!(!app.current_ui.compact_mode);
}
/// Threshold boundary: 20 rows engages auto-compact, 21 does not — and the
/// deeper `SHORT_TERMINAL_ROWS` degradation zone stays compact too.
#[test]
fn auto_compact_threshold_boundary() {
    use crossterm::event::Event;
    let mut app = test_app_with_agent();
    let _ = app.handle_input(&Event::Resize(100, 21));
    assert!(!app.appearance.prompt.compact, "21 rows stays non-compact");
    let _ = app.handle_input(&Event::Resize(100, 20));
    assert!(
        app.appearance.prompt.compact,
        "20 rows engages auto-compact"
    );
    let _ = app.handle_input(&Event::Resize(100, 16));
    assert!(
        app.appearance.prompt.compact,
        "16 rows (the short-terminal degradation zone) stays compact"
    );
}
/// The resize derivation never writes the thread-local user cache — the
/// cache (like `current_ui` and disk) holds the USER value only. Fresh
/// thread because the cache thread-locals are sticky.
#[test]
fn resize_derivation_never_writes_user_cache() {
    std::thread::spawn(|| {
        use crossterm::event::Event;
        let mut app = test_app_with_agent();
        crate::appearance::cache::set(false);
        let _ = app.handle_input(&Event::Resize(100, 14));
        assert!(app.appearance.prompt.compact);
        assert!(
            !crate::appearance::cache::load(),
            "resize must not write the user cache"
        );
    })
    .join()
    .unwrap();
}
/// User compact ON survives every resize: tiny stays compact (both flags
/// agree) and growing past the threshold keeps compact (the user value wins).
#[test]
fn user_compact_on_survives_growing_past_threshold() {
    use crossterm::event::Event;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetCompactMode(true), &mut app);
    let _ = app.handle_input(&Event::Resize(100, 14));
    assert!(app.appearance.prompt.compact);
    let _ = app.handle_input(&Event::Resize(100, 40));
    assert!(
        app.appearance.prompt.compact,
        "user value must win on tall terminals"
    );
    assert!(app.current_ui.compact_mode);
}
/// The startup seed derives from a pre-hydration disk read; once `current_ui`
/// holds the hydrated layered value, the canonical re-derive hook must
/// correct a divergent render value and fan it out to the prompt widgets —
/// without it the wrong seed sticks until a resize or toggle.
#[test]
fn hydration_rederive_corrects_divergent_startup_seed() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.last_known_terminal_rows = 40;
    assert!(!app.appearance.prompt.compact);
    app.current_ui.compact_mode = true;
    app.apply_effective_compact();
    assert!(
        app.appearance.prompt.compact,
        "hydrated user value must win over the stale seed"
    );
    assert!(
        app.agents[&id].prompt.compact(),
        "the correction must reach the prompt widget"
    );
}
/// Hot-reload keeps the PRE-reload render value in the config it applies and
/// routes any correction through the single writer, because `set_appearance`
/// alone never syncs `PromptWidget.compact`. A pending divergence must land
/// on every agent's widget — background agents included.
#[test]
fn hot_reload_rederive_syncs_prompt_widgets_on_every_agent() {
    let mut app = test_app_with_agent();
    let id_a = AgentId(0);
    let id_b = AgentId(1);
    let session_b = make_test_agent_session(&app, id_b, "test-session-b");
    app.agents
        .insert(id_b, AgentView::new(session_b, ScrollbackState::new()));
    app.next_agent_id = 2;
    app.last_known_terminal_rows = 18;
    assert!(!app.appearance.prompt.compact);
    assert!(!app.agents[&id_a].prompt.compact());
    assert!(!app.agents[&id_b].prompt.compact());
    let mut config = app.appearance.clone();
    config.prompt.compact = app.appearance.prompt.compact;
    app.set_appearance(config);
    app.apply_effective_compact();
    assert!(app.appearance.prompt.compact, "derived value applied");
    assert!(
        app.agents[&id_a].prompt.compact() && app.agents[&id_b].prompt.compact(),
        "the re-derive must sync every agent's prompt widget, not just the active one"
    );
}
/// Toggling the setting OFF while auto-compact is active persists the user
/// value (false) but keeps the derived render value on, and the toast says
/// why the layout stays compact.
#[test]
fn toggle_off_while_short_keeps_render_compact_and_persists_user_value() {
    use crate::settings::SettingValue;
    use crossterm::event::Event;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetCompactMode(true), &mut app);
    let _ = app.handle_input(&Event::Resize(100, 14));
    let effects = dispatch(Action::SetCompactMode(false), &mut app);
    assert!(!app.current_ui.compact_mode, "user setting turned off");
    assert!(
        app.appearance.prompt.compact,
        "auto-compact keeps the render value on at 14 rows"
    );
    match &effects[0] {
        Effect::PersistSetting { key, value, .. } => {
            assert_eq!(*key, "compact_mode");
            assert_eq!(value, &SettingValue::Bool(false), "persists the USER value");
        }
        other => panic!("expected PersistSetting, got {other:?}"),
    }
    let toast = read_toast(&app);
    assert!(
        toast.contains("auto-compact"),
        "toast must note auto-compact stays active, got {toast:?}"
    );
}
#[test]
fn set_timestamps_emits_persist_setting_with_correct_payload() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetTimestamps(false), &mut app);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistSetting {
            key,
            value,
            rollback_value,
        } => {
            assert_eq!(*key, "show_timestamps");
            assert_eq!(value, &SettingValue::Bool(false));
            assert_eq!(rollback_value, &SettingValue::Bool(true));
        }
        other => panic!("expected PersistSetting, got {other:?}"),
    }
    assert_eq!(app.current_ui.show_timestamps, Some(false));
}
#[test]
fn set_timeline_emits_persist_setting_with_correct_payload() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    let default_on = app.current_ui.show_timeline_enabled();
    let effects = dispatch(Action::SetTimeline(!default_on), &mut app);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistSetting {
            key,
            value,
            rollback_value,
        } => {
            assert_eq!(*key, "show_timeline");
            assert_eq!(value, &SettingValue::Bool(!default_on));
            assert_eq!(rollback_value, &SettingValue::Bool(default_on));
        }
        other => panic!("expected PersistSetting, got {other:?}"),
    }
    assert_eq!(app.current_ui.show_timeline, Some(!default_on));
    assert_eq!(
        crate::appearance::cache::load_show_timeline(),
        !default_on,
        "set_timeline must update the appearance cache"
    );
    assert_eq!(
        app.appearance.show_timeline, !default_on,
        "set_timeline must update the live appearance config"
    );
}
#[test]
fn set_timeline_toggles_displayed_state_when_current_ui_diverges() {
    let mut app = test_app_with_agent();
    app.appearance.show_timeline = true;
    crate::appearance::cache::set_show_timeline(true);
    app.current_ui.show_timeline = None;
    assert!(
        !app.current_ui.show_timeline_enabled(),
        "current_ui resolves to the OFF default (the divergence)"
    );
    let new = !crate::appearance::cache::load_show_timeline();
    assert!(!new, "toggle target is OFF");
    let effects = dispatch(Action::SetTimeline(new), &mut app);
    assert_eq!(effects.len(), 1, "toggle must persist, not silently no-op");
    assert!(!app.appearance.show_timeline, "the rail is now hidden");
    assert_eq!(app.current_ui.show_timeline, Some(false));
}
#[test]
fn set_confirm_before_rewind_emits_persist_setting_with_correct_payload() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    let default_on = app.current_ui.confirm_before_rewind_enabled();
    let effects = dispatch(Action::SetConfirmBeforeRewind(!default_on), &mut app);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistSetting {
            key,
            value,
            rollback_value,
        } => {
            assert_eq!(*key, "confirm_before_rewind");
            assert_eq!(value, &SettingValue::Bool(!default_on));
            assert_eq!(rollback_value, &SettingValue::Bool(default_on));
        }
        other => panic!("expected PersistSetting, got {other:?}"),
    }
    assert_eq!(app.current_ui.confirm_before_rewind, Some(!default_on));
}
#[test]
fn set_page_flip_on_send_emits_persist_setting_with_correct_payload() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    let default_on = app.current_ui.page_flip_on_send_enabled();
    crate::appearance::cache::set_page_flip_on_send(default_on);
    let effects = dispatch(Action::SetPageFlipOnSend(!default_on), &mut app);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistSetting {
            key,
            value,
            rollback_value,
        } => {
            assert_eq!(*key, "page_flip_on_send");
            assert_eq!(value, &SettingValue::Bool(!default_on));
            assert_eq!(rollback_value, &SettingValue::Bool(default_on));
        }
        other => panic!("expected PersistSetting, got {other:?}"),
    }
    assert_eq!(app.current_ui.page_flip_on_send, Some(!default_on));
    assert_eq!(
        crate::appearance::cache::load_page_flip_on_send(),
        !default_on
    );
}
#[test]
fn set_simple_mode_emits_persist_setting_with_correct_payload() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetSimpleMode(false), &mut app);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistSetting { key, value, .. } => {
            assert_eq!(*key, "simple_mode");
            assert_eq!(value, &SettingValue::Bool(false));
        }
        other => panic!("expected PersistSetting, got {other:?}"),
    }
    assert_eq!(app.current_ui.simple_mode, Some(false));
}
/// `dispatch_open_settings` runs once: the modal opens on the
/// active agent. A second dispatch — only structurally reachable
/// if input routing breaks — closes the modal defensively
/// (debug_assert guards this in dev builds).
#[test]
fn dispatch_open_settings_opens_then_close_on_reentry() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::OpenSettings, &mut app);
    let agent = app
        .agents
        .get(&AgentId(0))
        .expect("agent should exist after test setup");
    assert!(
        matches!(agent.active_modal, Some(ActiveModal::Settings { .. })),
        "first dispatch should open the modal"
    );
    if !cfg!(debug_assertions) {
        let _ = dispatch(Action::OpenSettings, &mut app);
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent.active_modal.is_none(),
            "second dispatch should close the modal"
        );
    }
}
/// A focused open on an agent whose settings modal is already open must
/// reopen focused on the requested row — not toggle the modal closed.
#[test]
fn dispatch_open_settings_focus_reopens_when_already_open() {
    use crate::views::modal::ActiveModal;
    use crate::views::settings_modal::SettingsModalMode;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::OpenSettings, &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    assert!(matches!(
        agent.active_modal,
        Some(ActiveModal::Settings { .. })
    ));
    let _ = dispatch(
        Action::OpenSettingsFocus {
            key: "coding_data_sharing",
        },
        &mut app,
    );
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("focused re-entry must keep the settings modal open")
    };
    assert_eq!(
        state.focused_setting().map(|(k, _)| k),
        Some("coding_data_sharing"),
        "focused re-entry must land on the requested row"
    );
    assert!(
        matches!(state.mode(), SettingsModalMode::PickingEnum { .. }),
        "focused re-entry must open the chooser, got {:?}",
        state.mode()
    );
    assert!(
        state.close_on_picker_exit,
        "focused re-entry must arm close_on_picker_exit"
    );
}
/// Chooser when editable, browse row when locked. The team-admin arm is the
/// one a `team_name.is_some()` shortcut would break.
#[test]
fn dispatch_open_settings_focus_skips_the_chooser_only_when_locked() {
    use crate::views::modal::ActiveModal;
    use crate::views::settings_modal::SettingsModalMode;
    let open_focused = |app: &mut AppView| -> SettingsModalMode {
        let _ = dispatch(
            Action::OpenSettingsFocus {
                key: "coding_data_sharing",
            },
            app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
            panic!("settings modal must be open")
        };
        assert_eq!(
            state.focused_setting().map(|(k, _)| k),
            Some("coding_data_sharing"),
            "every landing focuses the row"
        );
        state.mode()
    };
    let mut app = test_app_with_agent();
    assert!(
        matches!(
            open_focused(&mut app),
            SettingsModalMode::PickingEnum { .. }
        ),
        "an editable setting opens its chooser"
    );
    let mut app = test_app_with_agent();
    app.is_zdr = true;
    assert!(
        matches!(open_focused(&mut app), SettingsModalMode::Browse),
        "ZDR must stop at the row that says so"
    );
    let mut app = test_app_with_agent();
    app.team_name = Some("acme".to_string());
    app.team_role = Some("member".to_string());
    assert!(
        matches!(open_focused(&mut app), SettingsModalMode::Browse),
        "a team-managed lock must stop at the row that says so"
    );
    let mut app = test_app_with_agent();
    app.team_name = Some("acme".to_string());
    app.team_role = Some("admin".to_string());
    assert!(
        matches!(
            open_focused(&mut app),
            SettingsModalMode::PickingEnum { .. }
        ),
        "a team admin is not locked"
    );
}
/// Focused open that enters the chooser sets `close_on_picker_exit` so Esc
/// dismisses the modal (GB-4470). Locked landings stay in Browse with the
/// flag clear — chrome Esc already closes.
#[test]
fn dispatch_open_settings_focus_sets_close_on_picker_exit_when_chooser_opens() {
    use crate::views::modal::ActiveModal;
    use crate::views::settings_modal::SettingsModalMode;
    let mut app = test_app_with_agent();
    let _ = dispatch(
        Action::OpenSettingsFocus {
            key: "coding_data_sharing",
        },
        &mut app,
    );
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("settings modal must be open")
    };
    assert!(
        matches!(state.mode(), SettingsModalMode::PickingEnum { .. }),
        "editable focus must open the chooser"
    );
    assert!(
        state.close_on_picker_exit,
        "deep-link chooser open must set close_on_picker_exit"
    );
    let mut app = test_app_with_agent();
    app.is_zdr = true;
    let _ = dispatch(
        Action::OpenSettingsFocus {
            key: "coding_data_sharing",
        },
        &mut app,
    );
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("settings modal must be open")
    };
    assert!(matches!(state.mode(), SettingsModalMode::Browse));
    assert!(
        !state.close_on_picker_exit,
        "locked focus must not set close_on_picker_exit"
    );
}
/// Plain OpenSettings does not arm close-on-picker-Esc.
#[test]
fn dispatch_open_settings_does_not_set_close_on_picker_exit() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::OpenSettings, &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("settings modal must be open")
    };
    assert!(!state.close_on_picker_exit);
}
/// Full path: `/privacy`-style focus open → Esc dismisses the settings modal.
#[test]
fn open_settings_focus_esc_closes_settings_modal() {
    use crate::views::modal::ActiveModal;
    use crate::views::settings_modal::SettingsModalMode;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let _ = dispatch(
        Action::OpenSettingsFocus {
            key: "coding_data_sharing",
        },
        &mut app,
    );
    {
        let agent = app.agents.get(&id).unwrap();
        let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
            panic!("settings modal must be open")
        };
        assert!(matches!(
            state.mode(),
            SettingsModalMode::PickingEnum { .. }
        ));
        assert!(state.close_on_picker_exit);
    }
    let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let _ = app.handle_input(&esc);
    assert!(
        app.agents.get(&id).unwrap().active_modal.is_none(),
        "deep-link Esc must dismiss the settings modal"
    );
}
/// Full path: `/privacy`-style focus open → Enter commits and dismisses.
#[test]
fn open_settings_focus_enter_closes_settings_modal() {
    use crate::app::app_view::InputOutcome;
    use crate::views::modal::ActiveModal;
    use crate::views::settings_modal::SettingsModalMode;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let _ = dispatch(
        Action::OpenSettingsFocus {
            key: "coding_data_sharing",
        },
        &mut app,
    );
    {
        let agent = app.agents.get(&id).unwrap();
        let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
            panic!("settings modal must be open")
        };
        assert!(matches!(
            state.mode(),
            SettingsModalMode::PickingEnum { .. }
        ));
        assert!(state.close_on_picker_exit);
    }
    let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let outcome = app.handle_input(&enter);
    assert!(
        app.agents.get(&id).unwrap().active_modal.is_none(),
        "deep-link Enter must dismiss the settings modal"
    );
    assert!(
        matches!(
            outcome,
            InputOutcome::Action(Action::SetCodingDataSharing { .. })
        ),
        "deep-link Enter must commit SetCodingDataSharing, got {outcome:?}"
    );
}
/// Browse path: OpenSettings → enter picker → Esc keeps modal open in Browse.
#[test]
fn open_settings_enter_picker_esc_stays_open_in_browse() {
    use crate::views::modal::ActiveModal;
    use crate::views::settings_modal::SettingsModalMode;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let _ = dispatch(Action::OpenSettings, &mut app);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let Some(ActiveModal::Settings { state }) = &mut agent.active_modal else {
            panic!("settings modal must be open")
        };
        assert!(state.focus_key("coding_data_sharing"));
        assert!(state.try_enter_picking_enum());
        assert!(!state.close_on_picker_exit);
    }
    let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let _ = app.handle_input(&esc);
    let agent = app.agents.get(&id).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("browse-path Esc must keep the settings modal open")
    };
    assert!(
        matches!(state.mode(), SettingsModalMode::Browse),
        "browse-path Esc must return to Browse, got {:?}",
        state.mode()
    );
}
/// `ActionThenClose` closes the modal and forwards the preview-revert Action
/// through `apply_settings_outcome` (handle_input path).
#[test]
fn deep_link_preview_esc_closes_modal_and_forwards_revert_action() {
    use crate::app::app_view::InputOutcome;
    use crate::views::modal::ActiveModal;
    use crate::views::settings_modal::SettingsModalMode;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let _ = dispatch(Action::OpenSettings, &mut app);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let Some(ActiveModal::Settings { state }) = &mut agent.active_modal else {
            panic!("settings modal must be open")
        };
        assert!(state.focus_key("theme"));
        assert!(state.try_enter_picking_enum());
        state.close_on_picker_exit = true;
        assert!(matches!(
            state.mode(),
            SettingsModalMode::PickingEnum { .. }
        ));
    }
    let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let outcome = app.handle_input(&esc);
    assert!(
        app.agents.get(&id).unwrap().active_modal.is_none(),
        "ActionThenClose must clear active_modal"
    );
    match outcome {
        InputOutcome::Action(Action::PreviewTheme(name)) => {
            assert_eq!(name, "echo-agent-night");
        }
        other => panic!("expected Action(PreviewTheme), got {other:?}"),
    }
}
/// `dispatch_open_reset_confirm` moves the Settings modal state
/// into the new `ResetSettingsConfirm` variant, preserving it
/// across the confirm dialog's lifecycle. The dispatch arm is
/// only reachable from an open Settings modal (the `d` keystroke
/// in `views/settings_modal.rs::handle_browse`).
#[test]
fn dispatch_open_reset_confirm_moves_settings_state_into_confirm_variant() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::OpenSettings, &mut app);
    let agent = app.agents.get(&AgentId(0)).expect("agent must exist");
    assert!(
        matches!(agent.active_modal, Some(ActiveModal::Settings { .. })),
        "Settings modal must be open before OpenResetConfirm"
    );
    let _ = dispatch(
        Action::OpenResetConfirm {
            key: "compact_mode",
        },
        &mut app,
    );
    let agent = app.agents.get(&AgentId(0)).expect("agent must exist");
    match &agent.active_modal {
        Some(ActiveModal::ResetSettingsConfirm { key, .. }) => {
            assert_eq!(*key, "compact_mode");
        }
        _ => panic!("expected ResetSettingsConfirm modal to be active"),
    }
}
/// `ConfirmResetSetting { choice: Cancel }` restores the Settings
/// modal AND preserves the user's filter/scroll/selection state
/// byte-identically via the box-move. Pins query, selected,
/// scroll_offset, mode AND the snapshot (not just
/// ui_snapshot.compact_mode).
#[test]
fn dispatch_confirm_reset_setting_cancel_preserves_modal_state() {
    use crate::views::modal::{ActiveModal, ResetSettingsResult};
    use crate::views::settings_modal::SettingsModalMode;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetCompactMode(true), &mut app);
    assert!(app.current_ui.compact_mode);
    let _ = dispatch(Action::OpenSettings, &mut app);
    {
        let agent = app.agents.get_mut(&AgentId(0)).expect("agent must exist");
        if let Some(ActiveModal::Settings { state }) = &mut agent.active_modal {
            state.set_query("stamp");
            state.selected = 3;
            state.scroll_offset = 1;
            state.focus_filter();
        } else {
            panic!("expected Settings modal");
        }
    }
    let _ = dispatch(
        Action::OpenResetConfirm {
            key: "compact_mode",
        },
        &mut app,
    );
    let effects = dispatch(
        Action::ConfirmResetSetting {
            choice: ResetSettingsResult::Cancel,
        },
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "Cancel branch must not emit any Effects, got {effects:?}",
    );
    assert!(
        app.current_ui.compact_mode,
        "Cancel must not mutate the underlying setting",
    );
    let agent = app.agents.get(&AgentId(0)).expect("agent must exist");
    match &agent.active_modal {
        Some(ActiveModal::Settings { state }) => {
            assert!(state.ui_snapshot.compact_mode, "ui_snapshot.compact_mode");
            assert_eq!(state.query(), "stamp", "query preserved");
            assert_eq!(state.selected, 3, "selected preserved");
            assert_eq!(state.scroll_offset, 1, "scroll_offset preserved");
            assert!(
                matches!(state.mode(), SettingsModalMode::FilterFocused),
                "mode preserved (FilterFocused)"
            );
        }
        _ => panic!("expected Settings modal after Cancel"),
    }
}
/// `ConfirmResetSetting { Reset }` on a
/// PAGER-owned setting (`multiline_mode`) flips the agent's flag
/// back to default WITHOUT emitting any `Effect` (PAGER setters
/// short-circuit on no-disk-write). Pins the cross-owner
/// behavioral parity.
#[test]
fn dispatch_confirm_reset_setting_reset_dispatches_typed_setter_for_pager_bool() {
    use crate::views::modal::ResetSettingsResult;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetMultilineMode(true), &mut app);
    assert!(app.agents[&AgentId(0)].multiline_mode);
    setup_reset_confirm_open(&mut app, "multiline_mode");
    let effects = dispatch(
        Action::ConfirmResetSetting {
            choice: ResetSettingsResult::Reset,
        },
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "PAGER reset must NOT emit Effects (in-memory only), got {effects:?}",
    );
    assert!(
        !app.agents[&AgentId(0)].multiline_mode,
        "agent.multiline_mode must be reset to default",
    );
}
/// Idempotent Reset is gated. When
/// the focused row is already at its registered default, the
/// Reset branch emits a short "already at default" toast and
/// skips the recursive dispatch — unifying behavior across
/// SHARED/PAGER/SHELL owner classes.
#[test]
fn dispatch_confirm_reset_setting_reset_on_already_default_is_no_op_with_toast() {
    use crate::views::modal::ResetSettingsResult;
    let mut app = test_app_with_agent();
    assert!(!app.current_ui.compact_mode);
    setup_reset_confirm_open(&mut app, "compact_mode");
    let effects = dispatch(
        Action::ConfirmResetSetting {
            choice: ResetSettingsResult::Reset,
        },
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "idempotent reset must emit NO Effects, got {effects:?}",
    );
    assert!(!app.current_ui.compact_mode);
    let agent = app.agents.get(&AgentId(0)).expect("agent must exist");
    let toast_text = agent
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .unwrap_or_default();
    assert!(
        toast_text.contains("already at default"),
        "expected 'already at default' toast, got: {toast_text:?}",
    );
}
/// `refresh_open_settings_modals` walks into
/// `ResetSettingsConfirm.settings_state` so a `set_X` (or
/// rollback) running while the confirm dialog is open keeps the
/// boxed snapshot fresh. Without this, Cancel would restore a
/// stale snapshot.
#[test]
fn refresh_open_settings_modals_updates_reset_confirm_settings_state() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    setup_reset_confirm_open(&mut app, "compact_mode");
    {
        let agent = app.agents.get(&AgentId(0)).expect("agent must exist");
        if let Some(ActiveModal::ResetSettingsConfirm { settings_state, .. }) = &agent.active_modal
        {
            assert!(!settings_state.ui_snapshot.compact_mode);
        } else {
            panic!("expected ResetSettingsConfirm");
        }
    }
    let _ = dispatch(Action::SetCompactMode(true), &mut app);
    let agent = app.agents.get(&AgentId(0)).expect("agent must exist");
    match &agent.active_modal {
        Some(ActiveModal::ResetSettingsConfirm { settings_state, .. }) => {
            assert!(
                settings_state.ui_snapshot.compact_mode,
                "refresh_open_settings_modals must update the boxed snapshot \
                     inside ResetSettingsConfirm — without this, Cancel restores stale state",
            );
        }
        _ => panic!("expected ResetSettingsConfirm still active"),
    }
}
/// The
/// dispatch-arm bail-out path is verified directly via
/// `#[should_panic]` in debug mode rather than the previous
/// `if cfg!(debug_assertions) { return; }` placebo which gave
/// debug-mode CI no coverage of the routing-bug bail-out.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "OpenResetConfirm dispatched without an open Settings modal")]
fn dispatch_open_reset_confirm_panics_in_debug_when_no_settings_modal() {
    let mut app = test_app_with_agent();
    let agent = app.agents.get(&AgentId(0)).unwrap();
    assert!(agent.active_modal.is_none());
    let _ = dispatch(
        Action::OpenResetConfirm {
            key: "compact_mode",
        },
        &mut app,
    );
}
/// Release-mode mirror: same bail-out but verified via the
/// active_modal staying None (debug_assert! is compiled out).
#[test]
#[cfg(not(debug_assertions))]
fn dispatch_open_reset_confirm_no_op_in_release_when_no_settings_modal() {
    let mut app = test_app_with_agent();
    let agent = app.agents.get(&AgentId(0)).unwrap();
    assert!(agent.active_modal.is_none());
    let effects = dispatch(
        Action::OpenResetConfirm {
            key: "compact_mode",
        },
        &mut app,
    );
    assert!(effects.is_empty());
    let agent = app.agents.get(&AgentId(0)).unwrap();
    assert!(
        agent.active_modal.is_none(),
        "active_modal must stay None on routing-bug bail-out",
    );
}
/// The CI guard for `action_for_reset` asserts more than
/// `action.is_some()`, which would pass for a mis-mapped arm
/// like `("compact_mode", _) => Some(Action::SetTheme(...))`.
/// This version dispatches the returned action and asserts the
/// setting reads back at its registered default — catching both
/// presence AND payload correctness.
#[test]
fn every_setting_has_action_for_reset_arm() {
    use crate::settings::current_value_for;
    with_theme_test_env(|| {
        let reg = crate::settings::SettingsRegistry::defaults();
        for meta in reg.all() {
            if matches!(meta.kind, crate::settings::SettingKind::Group { .. }) {
                continue;
            }
            let default_value = crate::settings::default_value_for(meta);
            let action = action_for_reset(meta.key, &default_value);
            assert!(
                action.is_some(),
                "Setting `{}` has no `action_for_reset` arm — `d`-reset would silently \
                 no-op. Add an arm to `action_for_reset` in dispatch.rs.",
                meta.key,
            );
            if meta.key == "default_model" {
                continue;
            }
            let mut app = test_app_with_agent();
            move_setting_away_from_default(&mut app, meta.key);
            let _ = dispatch(action.unwrap(), &mut app);
            let pager = build_pager_snapshot(&app);
            let reread = current_value_for(meta.key, &app.current_ui, &pager);
            assert_eq!(
                reread,
                Some(default_value.clone()),
                "action_for_reset({}, ...) round-trip drift: dispatch did not restore the default",
                meta.key,
            );
        }
    });
}
/// Every setting whose setter emits `Effect::PersistSetting` MUST have an
/// `apply_setting_rollback` arm: a failed disk write rolls back through it,
/// and a missing arm silently diverges in-memory state from `config.toml`.
/// Drives each setter (move-away → reset) to discover which keys actually
/// persist, then asserts the rollback hits a real arm (not the catch-all).
#[test]
fn every_persisting_setting_has_rollback_arm() {
    with_theme_test_env(|| {
        let reg = crate::settings::SettingsRegistry::defaults();
        for meta in reg.all() {
            let default_value = crate::settings::default_value_for(meta);
            let Some(reset_action) = action_for_reset(meta.key, &default_value) else {
                continue;
            };
            let mut app = test_app_with_agent();
            move_setting_away_from_default(&mut app, meta.key);
            for eff in dispatch(reset_action, &mut app) {
                let Effect::PersistSetting {
                    key,
                    rollback_value,
                    ..
                } = eff
                else {
                    continue;
                };
                let mut rb_app = test_app_with_agent();
                let _ = apply_setting_rollback(&mut rb_app, key, &rollback_value);
                let toast = rb_app
                    .agents
                    .get(&AgentId(0))
                    .and_then(|a| a.toast.as_ref())
                    .map(|(s, _)| s.as_str().to_owned());
                assert_ne!(
                    toast.as_deref(),
                    Some(crate::app::dispatch::ROLLBACK_NO_ARM_TOAST),
                    "Setting `{key}` emits Effect::PersistSetting but apply_setting_rollback has \
                     no arm — a failed persist diverges in-memory state from config.toml. Add an arm.",
                );
            }
        }
    });
}
/// Pin the
/// asymmetric clear-default semantics. `Action::ClearDefaultModel`
/// persists `cfg.models.default = None` AND emits the
/// "cleared" toast, but deliberately does NOT mutate
/// `agent.session.models.current`.
#[test]
fn clear_default_model_persists_but_keeps_live_current() {
    use agent_client_protocol as acp;
    use std::sync::Arc;
    let mut app = test_app_with_agent();
    let id = acp::ModelId::new(Arc::from("echo-agent-test"));
    let info = acp::ModelInfo::new(id.clone(), "EchoAgent Test".to_string());
    let agent_id = AgentId(0);
    app.agents
        .get_mut(&agent_id)
        .unwrap()
        .session
        .models
        .available
        .insert(id.clone(), info);
    app.agents
        .get_mut(&agent_id)
        .unwrap()
        .session
        .models
        .set_current(id.clone(), None);
    let effects = dispatch(Action::ClearDefaultModel, &mut app);
    assert_eq!(
        effects.len(),
        1,
        "expected exactly one PersistSetting effect"
    );
    assert!(
        matches!(
            &effects[0],
            Effect::PersistSetting {
                key: "default_model",
                value: crate::settings::SettingValue::String(s),
                .. } if s.is_empty()
        ),
        "expected PersistSetting(default_model, ''), got {:?}",
        effects[0],
    );
    assert_eq!(
        app.agents[&agent_id].session.models.current,
        Some(id),
        "clear_default_model must NOT mutate live agent.session.models.current",
    );
}
/// `Action::SetDefaultModel(<known id>)` resolves the
/// id against the live catalog, mutates current, and emits both
/// PersistSetting + SwitchModel effects. This is the
/// dispatch-level analog of the slash-command's
/// `slash_model_valid_dispatches_set_default_model_with_switch_and_persist`
/// test.
#[test]
fn set_default_model_resolves_known_name() {
    use agent_client_protocol as acp;
    use std::sync::Arc;
    let mut app = test_app_with_agent();
    let id = acp::ModelId::new(Arc::from("echo-agent-4.5"));
    let info = acp::ModelInfo::new(id.clone(), "EchoAgent 4.5".to_string());
    let agent_id = AgentId(0);
    app.agents
        .get_mut(&agent_id)
        .unwrap()
        .session
        .models
        .available
        .insert(id.clone(), info);
    let effects = dispatch(Action::SetDefaultModel(id.clone()), &mut app);
    assert_eq!(effects.len(), 2);
    assert!(matches!(
        &effects[0],
        Effect::PersistSetting {
            key: "default_model",
            value: crate::settings::SettingValue::String(s),
            .. } if s == "echo-agent-4.5"
    ));
    assert!(matches!(
        &effects[1],
        Effect::SwitchModel { model_id: mid, .. } if mid == &id
    ));
    assert_eq!(app.agents[&agent_id].session.models.current, Some(id));
}
/// Re-dispatching the same model
/// id is idempotent — no PersistSetting, no SwitchModel, no
/// reasoning_effort reset.
#[test]
fn set_default_model_idempotent_when_already_current() {
    use agent_client_protocol as acp;
    use std::sync::Arc;
    let mut app = test_app_with_agent();
    let id = acp::ModelId::new(Arc::from("echo-agent-already"));
    let info = acp::ModelInfo::new(id.clone(), "EchoAgent Already".to_string());
    let agent_id = AgentId(0);
    app.agents
        .get_mut(&agent_id)
        .unwrap()
        .session
        .models
        .available
        .insert(id.clone(), info);
    app.agents
        .get_mut(&agent_id)
        .unwrap()
        .session
        .models
        .set_current(id.clone(), None);
    let effects = dispatch(Action::SetDefaultModel(id), &mut app);
    assert!(
        effects.is_empty(),
        "re-dispatching same model must be idempotent (no effects), got {effects:?}",
    );
}
/// `clamp_max_thoughts_width` clamps
/// out-of-range values to the registered `[40, 500]` bounds.
#[test]
fn set_max_thoughts_width_clamps_out_of_range() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetMaxThoughtsWidth(10), &mut app);
    assert_eq!(app.current_ui.max_thoughts_width, 40);
    let _ = dispatch(Action::SetMaxThoughtsWidth(9999), &mut app);
    assert_eq!(app.current_ui.max_thoughts_width, 500);
    let _ = dispatch(Action::SetMaxThoughtsWidth(200), &mut app);
    assert_eq!(app.current_ui.max_thoughts_width, 200);
}
#[test]
fn pr13_set_show_tips_emits_persist_with_rollback() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    assert_eq!(app.show_tips, None);
    let effects = dispatch(Action::SetShowTips(false), &mut app);
    assert_eq!(effects.len(), 1, "must emit exactly one effect");
    match &effects[0] {
        Effect::PersistSetting {
            key,
            value,
            rollback_value,
        } => {
            assert_eq!(*key, "show_tips");
            assert_eq!(*value, SettingValue::Bool(false));
            assert_eq!(*rollback_value, SettingValue::Bool(true));
        }
        other => panic!("expected PersistSetting effect, got {other:?}"),
    }
    assert_eq!(app.show_tips, Some(false));
    let effects = dispatch(Action::SetShowTips(true), &mut app);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistSetting {
            value,
            rollback_value,
            ..
        } => {
            assert_eq!(*value, SettingValue::Bool(true));
            assert_eq!(*rollback_value, SettingValue::Bool(false));
        }
        other => panic!("expected PersistSetting effect, got {other:?}"),
    }
    assert_eq!(app.show_tips, Some(true));
}
/// Idempotency — re-committing the same value is a no-op
/// (no Effect). Mirror of `set_auto_compact_threshold_percent_idempotent_re_commit`.
#[test]
fn pr13_set_show_tips_idempotent_re_commit() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetShowTips(false), &mut app);
    assert_eq!(app.show_tips, Some(false));
    let effects = dispatch(Action::SetShowTips(false), &mut app);
    assert!(
        effects.is_empty(),
        "re-committing the same value must be idempotent (no effects), got {effects:?}"
    );
}
/// The first-ever commit of the default value MUST persist
/// (mirror of `set_auto_compact_threshold_percent_first_commit_of_default_persists`).
/// Without this special case, a user opting in to the default
/// would see no Effect but the on-disk state would stay `None` —
/// the next session's managed-config layer could then override
/// silently.
#[test]
fn pr13_set_show_tips_first_commit_of_default_persists() {
    let mut app = test_app_with_agent();
    assert_eq!(app.show_tips, None);
    let effects = dispatch(Action::SetShowTips(true), &mut app);
    assert_eq!(
        effects.len(),
        1,
        "first commit of the default must persist (not silently no-op)"
    );
    assert_eq!(app.show_tips, Some(true));
}
/// Subsequent commits of the same default hit the
/// idempotent fast-path. Pins the symmetric half of the
/// `_first_commit_of_default_persists` contract.
#[test]
fn pr13_set_show_tips_second_commit_of_default_no_ops() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetShowTips(true), &mut app);
    assert_eq!(app.show_tips, Some(true));
    let effects = dispatch(Action::SetShowTips(true), &mut app);
    assert!(
        effects.is_empty(),
        "second commit of the default must hit the idempotent fast-path \
             (effects.is_empty()), got {effects:?}"
    );
}
/// Rollback from prev=None state restores `None` (not
/// `Some(default)`) — keeps AppView in sync with disk. Mirror of
/// `auto_compact_rollback_from_none_state_restores_none`.
#[test]
fn pr13_show_tips_rollback_from_none_state_restores_none() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    assert_eq!(app.show_tips, None);
    let effects = dispatch(Action::SetShowTips(false), &mut app);
    let rollback_value = match &effects[0] {
        Effect::PersistSetting { rollback_value, .. } => rollback_value.clone(),
        _ => panic!("expected PersistSetting"),
    };
    assert_eq!(rollback_value, SettingValue::Bool(true));
    assert_eq!(app.show_tips, Some(false));
    apply_setting_rollback(&mut app, "show_tips", &rollback_value);
    assert_eq!(
        app.show_tips, None,
        "PR 13: rollback from prev=None must restore None (not Some(true)) — \
             otherwise the AppView mirror drifts from disk, which is still None \
             after the failed persist"
    );
}
/// Toast text includes the "(restart to apply)" cue —
/// matches the modal pill so the user gets consistent restart
/// feedback through both surfaces. Mirror of
/// `set_auto_compact_threshold_percent_toast_includes_restart_marker`.
#[test]
fn pr13_set_show_tips_toast_includes_restart_marker() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetShowTips(false), &mut app);
    let toast = read_toast(&app);
    assert!(
        toast.contains("Show tips"),
        "toast must include the setting label, got {toast:?}"
    );
    assert!(
        toast.contains("off"),
        "toast must include the value, got {toast:?}"
    );
    assert!(
        toast.contains("restart to apply"),
        "toast must include the deferred-effect cue, got {toast:?}"
    );
}
/// Helper for `every_setting_has_action_for_reset_arm`. Flips the
/// setting to a non-default value so the round-trip dispatch has
/// an observable effect (otherwise the assertion would pass
/// vacuously when current == default).
/// Dispatches theme-mutating actions for the theme keys — callers must
/// hold the theme test lock (wrap the test in [`with_theme_test_env`]).
fn move_setting_away_from_default(app: &mut AppView, key: crate::settings::SettingKey) {
    match key {
        "compact_mode" => {
            let _ = dispatch(Action::SetCompactMode(true), app);
        }
        "show_timestamps" => {
            let _ = dispatch(Action::SetTimestamps(false), app);
        }
        "show_timeline" => {
            let away = !app.current_ui.show_timeline_enabled();
            let _ = dispatch(Action::SetTimeline(away), app);
        }
        "page_flip_on_send" => {
            let away = !crate::appearance::cache::load_page_flip_on_send();
            let _ = dispatch(Action::SetPageFlipOnSend(away), app);
        }
        "confirm_before_rewind" => {
            let away = !app.current_ui.confirm_before_rewind_enabled();
            let _ = dispatch(Action::SetConfirmBeforeRewind(away), app);
        }
        "combine_queued_prompts" => {
            let away = !crate::appearance::cache::load_combine_queued_prompts();
            let _ = dispatch(Action::SetCombineQueuedPrompts(away), app);
        }
        "follow_up_behavior" => {
            let away = match crate::appearance::cache::load_follow_up_behavior() {
                crate::appearance::FollowUpBehavior::Queue => {
                    crate::appearance::FollowUpBehavior::Steer
                }
                crate::appearance::FollowUpBehavior::Steer => {
                    crate::appearance::FollowUpBehavior::Queue
                }
            };
            let _ = dispatch(Action::SetFollowUpBehavior(away), app);
        }
        "simple_mode" => {
            let _ = dispatch(Action::SetSimpleMode(false), app);
        }
        "contextual_hints.undo" => {
            let _ = dispatch(Action::SetContextualHintUndo(false), app);
        }
        "contextual_hints.plan_mode" => {
            let _ = dispatch(Action::SetContextualHintPlanMode(false), app);
        }
        "contextual_hints.image_input" => {
            let _ = dispatch(Action::SetContextualHintImageInput(false), app);
        }
        "contextual_hints.send_now" => {
            let _ = dispatch(Action::SetContextualHintSendNow(false), app);
        }
        "contextual_hints.small_screen" => {
            let _ = dispatch(Action::SetContextualHintSmallScreen(false), app);
        }
        "contextual_hints.word_select" => {
            let _ = dispatch(Action::SetContextualHintWordSelect(false), app);
        }
        "contextual_hints.ssh_wrap" => {
            let _ = dispatch(Action::SetContextualHintSshWrap(false), app);
        }
        "multiline_mode" => {
            let _ = dispatch(Action::SetMultilineMode(true), app);
        }
        "render_mermaid" => {
            let _ = dispatch(
                Action::SetRenderMermaid(crate::appearance::RenderMermaid::Off),
                app,
            );
        }
        "theme" => {
            let _ = dispatch(Action::SetTheme("tokyonight".to_owned()), app);
        }
        "auto_dark_theme" => {
            let _ = dispatch(Action::SetAutoDarkTheme("tokyonight".to_owned()), app);
        }
        "auto_light_theme" => {
            let _ = dispatch(Action::SetAutoLightTheme("rosepine-dawn".to_owned()), app);
        }
        "permission_mode" => {
            let _ = dispatch(Action::SetYoloMode(true), app);
        }
        "default_model" => {
            use agent_client_protocol as acp;
            use std::sync::Arc;
            if let ActiveView::Agent(aid) = app.active_view
                && let Some(agent) = app.agents.get_mut(&aid)
            {
                let id = acp::ModelId::new(Arc::from("test-model-move"));
                let info = acp::ModelInfo::new(id.clone(), "Test Model Move".to_string());
                agent.session.models.available.insert(id.clone(), info);
                agent.session.models.set_current(id, None);
            }
        }
        "max_thoughts_width" => {
            let _ = dispatch(Action::SetMaxThoughtsWidth(200), app);
        }
        "coding_data_sharing" => {
            let _ = dispatch(Action::SetCodingDataSharing { opted_in: true }, app);
        }
        "plan_mode" => {
            let _ = dispatch(
                Action::SetPlanMode(crate::app::actions::PlanModeKind::On),
                app,
            );
        }
        "show_tips" => {
            let _ = dispatch(Action::SetShowTips(false), app);
        }
        "auto_update" => {
            let _ = dispatch(Action::SetAutoUpdate(false), app);
        }
        "vim_mode" => {
            let _ = dispatch(Action::SetVimMode(true), app);
        }
        "remember_tool_approvals" => {
            let _ = dispatch(Action::SetRememberToolApprovals(false), app);
        }
        "toolset.ask_user_question.timeout_enabled" => {
            let _ = dispatch(Action::SetAskUserQuestionTimeoutEnabled(false), app);
        }
        "keep_text_selection" => {
            let _ = dispatch(
                Action::SetKeepTextSelection(crate::appearance::TextSelection::Hold),
                app,
            );
        }
        "scroll_speed" => {
            let _ = dispatch(Action::SetScrollSpeed(75), app);
        }
        "scroll_mode" => {
            let _ = dispatch(
                Action::SetScrollMode(crate::appearance::ScrollMode::Wheel),
                app,
            );
        }
        "invert_scroll" => {
            let _ = dispatch(Action::SetInvertScroll(true), app);
        }
        "display_refresh_auto_cadence" => {
            let _ = dispatch(Action::SetDisplayRefreshAutoCadence(true), app);
        }
        "scroll_lines" => {
            let _ = dispatch(Action::SetScrollLines(5), app);
        }
        "show_thinking_blocks" => {
            let _ = dispatch(Action::SetShowThinkingBlocks(true), app);
        }
        "group_tool_verbs" => {
            let _ = dispatch(Action::SetGroupToolVerbs(false), app);
        }
        "collapsed_edit_blocks" => {
            let _ = dispatch(Action::SetCollapsedEditBlocks(true), app);
        }
        "prompt_suggestions" => {
            let _ = dispatch(Action::SetPromptSuggestions(false), app);
        }
        "respect_manual_folds" => {
            let _ = dispatch(
                Action::SetRespectManualFolds(
                    !crate::appearance::ScrollConfig::default().respect_manual_folds,
                ),
                app,
            );
        }
        "hunk_tracker_mode" => {
            let _ = dispatch(Action::SetHunkTrackerMode("all_dirty".to_string()), app);
        }
        "screen_mode" => {
            let _ = dispatch(Action::SetScreenMode("minimal".to_string()), app);
        }
        "voice_keybind_enabled" => {
            let _ = dispatch(Action::SetVoiceKeybindEnabled(false), app);
        }
        "voice_capture_mode" => {
            let _ = dispatch(Action::SetVoiceCaptureMode("toggle".to_string()), app);
        }
        "voice_stt_language" => {
            let _ = dispatch(Action::SetVoiceSttLanguage("es".to_string()), app);
        }
        "fork_secondary_model" => {
            use agent_client_protocol as acp;
            use std::sync::Arc;
            if let ActiveView::Agent(aid) = app.active_view
                && let Some(agent) = app.agents.get_mut(&aid)
            {
                let id = acp::ModelId::new(Arc::from("test-fork-move"));
                let info = acp::ModelInfo::new(id.clone(), "Test Fork Move".to_string());
                agent.session.models.available.insert(id.clone(), info);
                let _ = dispatch(Action::SetForkSecondaryModel(id), app);
            }
        }
        "default_selected_permission" => {
            let _ = dispatch(
                Action::SetDefaultSelectedPermission("allow_once".to_string()),
                app,
            );
        }
        other => {
            panic!(
                "move_setting_away_from_default: no arm for `{other}`. \
                 Add one when registering a new setting."
            )
        }
    }
}
#[test]
fn set_compact_mode_toast_format() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetCompactMode(true), &mut app);
    let toast = read_toast(&app);
    assert!(toast.contains("Compact mode"));
    assert!(toast.contains("on"));
    assert!(toast.contains('\u{2713}'));
    let _ = dispatch(Action::SetCompactMode(false), &mut app);
    let toast = read_toast(&app);
    assert!(toast.contains("Compact mode"));
    assert!(toast.contains("off"));
}
/// `set_simple_mode_inner` propagates to the active agent's
/// `input_mode` — without this, the toast says
/// "Simple mode: on" but the current session keeps Vim input.
#[test]
fn set_simple_mode_propagates_to_active_agent() {
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.input_mode = crate::views::agent::InputMode::Vim;
    let _ = dispatch(Action::SetSimpleMode(true), &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    assert_eq!(
        agent.input_mode,
        crate::views::agent::InputMode::Simple,
        "set_simple_mode(true) must switch the active agent into Simple input mode"
    );
    let _ = dispatch(Action::SetSimpleMode(false), &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    assert_eq!(
        agent.input_mode,
        crate::views::agent::InputMode::Vim,
        "set_simple_mode(false) must switch back to Vim input mode"
    );
}
/// `set_simple_mode_inner` is a no-op on the agent-propagation
/// path when there's no active agent — the persist effect still
/// fires (the setting is global, not agent-local).
#[test]
fn set_simple_mode_no_op_when_no_active_agent() {
    let mut app = test_app();
    let effects = dispatch(Action::SetSimpleMode(true), &mut app);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistSetting { key, .. } => assert_eq!(*key, "simple_mode"),
        other => panic!("expected PersistSetting, got {other:?}"),
    }
    assert_eq!(app.current_ui.simple_mode, Some(true));
}
/// `set_simple_mode_inner` propagates to **every** agent, not just
/// the active one — without iterating
/// over `app.agents.values_mut()`, agent B's input_mode stays
/// stale when the user toggles simple-mode while agent A is active.
#[test]
fn set_simple_mode_propagates_to_every_agent() {
    let mut app = test_app_with_agent();
    let id_b = AgentId(1);
    let mut agent_b = AgentView::new(
        AgentSession {
            id: id_b,
            acp_tx: app.acp_tx.clone(),
            session_id: Some("test-session-b".into()),
            models: ModelState::default(),
            state: AgentState::Idle,
            tracker: AcpUpdateTracker::new(),
            cwd: PathBuf::from("/tmp"),
            is_worktree: false,
            forked_from: None,
            pending_prompts: std::collections::VecDeque::new(),
            next_queue_id: 0,
            yolo_mode: false,
            auto_mode: false,
            prompt_history: Vec::new(),
            prompt_history_loading: false,
            loading_replay: false,
            restore_degree: None,
            rate_limited: false,
            model_incompatible: false,
            credit_limit_blocked: false,
            free_usage_blocked: false,
            available_commands: Vec::new(),
            available_commands_generation: 0,
            available_tools: None,
            model_switch_pending: false,
            user_model_preference: None,
            deferred_model_switch: None,
            bg_tasks: std::collections::BTreeMap::new(),
            bg_tool_call_to_task: std::collections::HashMap::new(),
            scheduled_tasks: std::collections::HashMap::new(),
            in_flight_prompt: None,
            compact_held_prompt: None,
            current_prompt_id: None,
            created_via_new: false,
        },
        ScrollbackState::new(),
    );
    agent_b.input_mode = crate::views::agent::InputMode::Vim;
    app.agents.insert(id_b, agent_b);
    app.next_agent_id = 2;
    let _ = dispatch(Action::SetSimpleMode(true), &mut app);
    for (id, agent) in &app.agents {
        assert_eq!(
            agent.input_mode,
            crate::views::agent::InputMode::Simple,
            "agent {id:?} input_mode did not propagate to Simple"
        );
    }
}
/// Setters must update BOTH `app.current_ui` AND
/// `crate::appearance::cache`. The cache is the render hot path's
/// source of truth; if a future refactor accidentally drops the
/// `cache::set(new)` call, `app.current_ui` would still show the
/// new value but the renderer would revert on next frame. Runs
/// in a fresh thread because the thread-locals are sticky.
#[test]
fn set_x_propagates_to_thread_local_cache() {
    std::thread::spawn(|| {
        let mut app = test_app_with_agent();
        let _ = dispatch(Action::SetCompactMode(true), &mut app);
        assert!(
            crate::appearance::cache::load(),
            "set_compact_mode must update the cache"
        );
        let _ = dispatch(Action::SetCompactMode(false), &mut app);
        assert!(
            !crate::appearance::cache::load(),
            "set_compact_mode(false) must update the cache"
        );
        let _ = dispatch(Action::SetTimestamps(false), &mut app);
        assert!(
            !crate::appearance::cache::load_timestamps(),
            "set_timestamps must update the cache"
        );
        let _ = dispatch(Action::SetSimpleMode(false), &mut app);
        assert!(
            !crate::appearance::cache::load_simple_mode(),
            "set_simple_mode must update the cache"
        );
    })
    .join()
    .unwrap();
}
/// In debug builds, dispatching `Action::OpenSettings` twice in a
/// row panics on the `debug_assert!` (input-routing bug guard).
/// Pairs with
/// `dispatch_open_settings_opens_then_close_on_reentry` (which
/// exercises the release-mode silent-close path).
#[test]
#[should_panic(expected = "OpenSettings dispatched while settings modal is already open")]
#[cfg(debug_assertions)]
fn dispatch_open_settings_double_dispatch_panics_in_debug() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::OpenSettings, &mut app);
    let _ = dispatch(Action::OpenSettings, &mut app);
}
/// `set_multiline_mode(app, true)` mutates the active agent's
/// flag, fires a toast, and emits NO effects.
#[test]
fn set_multiline_mode_mutates_agent_and_emits_no_effect() {
    let mut app = test_app_with_agent();
    assert!(!app.agents[&AgentId(0)].multiline_mode);
    let effects = dispatch(Action::SetMultilineMode(true), &mut app);
    assert!(
        effects.is_empty(),
        "PAGER-owned setter must NOT emit Effect::PersistSetting, got {effects:?}",
    );
    assert!(
        app.agents[&AgentId(0)].multiline_mode,
        "agent.multiline_mode must reflect the new value",
    );
}
/// Idempotent fast path: re-emitting the same value is a UX
/// no-op (no toast, no Effect).
///
/// **Contract divergence from SHARED setters:**
/// `set_compact_mode_inner` only skips
/// the `app.set_appearance` fan-out — the outer `set_compact_mode`
/// still toasts AND emits `Effect::PersistSetting` on every
/// dispatch, including no-ops. PAGER setters can be stricter
/// because there's no disk write to confirm: re-toasting on a
/// same-value re-dispatch is pure UI noise, so we skip the toast
/// at the outer level too.
#[test]
fn set_multiline_mode_idempotent_no_toast() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetMultilineMode(true), &mut app);
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    assert!(agent.toast.is_some(), "first set must toast");
    agent.toast = None;
    let effects = dispatch(Action::SetMultilineMode(true), &mut app);
    assert!(effects.is_empty());
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "redundant set must not re-toast",
    );
    assert!(
        app.agents[&AgentId(0)].multiline_mode,
        "idempotent re-set must preserve value",
    );
}
/// Initial-idempotence: dispatching the same value as the agent's
/// default produces no toast and no effect.
/// Pins "no UX side effects on a redundant initial dispatch" —
/// matches the user-visible "open modal, click Space twice on a
/// default-false setting" flow.
#[test]
fn set_multiline_mode_initial_same_value_is_no_op() {
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetMultilineMode(false), &mut app);
    assert!(effects.is_empty());
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "matching-default dispatch must not toast",
    );
}
/// Toast string format matches the `"\u{2713} {label}: {value}"`
/// format exactly. Exact-equality strictly catches
/// format drift (e.g., dropping the `:` separator, adding suffix)
/// that substring `contains` checks would miss.
#[test]
fn set_multiline_mode_toast_format() {
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetMultilineMode(true), &mut app);
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast must be set");
    assert_eq!(toast, "\u{2713} Multiline: on");
    let _ = dispatch(Action::SetMultilineMode(false), &mut app);
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("toast must be set");
    assert_eq!(toast, "\u{2713} Multiline: off");
}
/// No active agent → no-op (no panic, no effect, no mutation).
/// Differs from `set_simple_mode_no_op_when_no_active_agent`: SHARED
/// settings persist globally even without an agent, but PAGER
/// settings have nowhere to write without an agent's state to
/// mutate.
#[test]
fn set_multiline_mode_no_op_when_no_active_agent() {
    let mut app = test_app();
    let compact_before = app.current_ui.compact_mode;
    let timestamps_before = app.current_ui.show_timestamps;
    let simple_before = app.current_ui.simple_mode;
    let effects = dispatch(Action::SetMultilineMode(true), &mut app);
    assert!(
        effects.is_empty(),
        "no active agent → no Effect (the setting is per-agent)",
    );
    assert_eq!(app.current_ui.compact_mode, compact_before);
    assert_eq!(app.current_ui.show_timestamps, timestamps_before);
    assert_eq!(app.current_ui.simple_mode, simple_before);
    assert!(
        matches!(app.active_view, ActiveView::Welcome),
        "active_view must not flip on no-agent dispatch",
    );
}
/// Dashboard surface owns its own compose flag: `SetMultilineMode` /
/// `/multiline` flip `dashboard.multiline_mode` and leave agent flags alone.
#[test]
fn set_multiline_mode_on_dashboard_toggles_dashboard_not_agents() {
    let mut app = test_app_with_agent();
    insert_placeholder_agent(&mut app, AgentId(1));
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    app.active_view = ActiveView::AgentDashboard;
    assert!(!app.dashboard.as_ref().unwrap().multiline_mode);
    assert!(!app.agents[&AgentId(0)].multiline_mode);
    assert!(!app.agents[&AgentId(1)].multiline_mode);
    let effects = dispatch(Action::SetMultilineMode(true), &mut app);
    assert!(effects.is_empty());
    assert!(
        app.dashboard.as_ref().unwrap().multiline_mode,
        "dashboard flag must flip on"
    );
    assert!(
        !app.agents[&AgentId(0)].multiline_mode && !app.agents[&AgentId(1)].multiline_mode,
        "agent flags must stay put"
    );
    let _ = dispatch(Action::SetMultilineMode(false), &mut app);
    assert!(!app.dashboard.as_ref().unwrap().multiline_mode);
    let _ = dispatch_dashboard_dispatch_slash(&mut app, "/multiline".into());
    assert!(
        app.dashboard.as_ref().unwrap().multiline_mode,
        "/multiline must toggle dashboard on"
    );
    assert!(!app.agents[&AgentId(0)].multiline_mode);
}
/// Multi-agent fan-out. `set_multiline_mode`
/// mutates only the ACTIVE agent's `multiline_mode`, never
/// other agents in the registry. The setter currently uses
/// `app.agents.get_mut(&active)` (index by active), but a
/// future refactor that loops over `app.agents.values_mut()`
/// or that touches SHARED `app.current_ui` would silently
/// regress this contract — without a multi-agent test, the
/// regression wouldn't surface until the user opened two
/// agents and noticed cross-contamination.
#[test]
fn set_multiline_mode_mutates_only_active_agent_not_others() {
    let mut app = test_app_with_agent();
    insert_placeholder_agent(&mut app, AgentId(1));
    assert!(!app.agents[&AgentId(0)].multiline_mode);
    assert!(!app.agents[&AgentId(1)].multiline_mode);
    let _ = dispatch(Action::SetMultilineMode(true), &mut app);
    assert!(
        app.agents[&AgentId(0)].multiline_mode,
        "active agent (id=0) must flip to true",
    );
    assert!(
        !app.agents[&AgentId(1)].multiline_mode,
        "non-active agent (id=1) must NOT be touched — PAGER-owned \
             setters address the active agent only",
    );
}
/// Regression test.
///
/// Open the settings modal, dispatch `SetMultilineMode(true)`,
/// assert the modal's `pager_snapshot.multiline_mode` is refreshed
/// to `true`. Without `refresh_open_settings_modals`, the snapshot
/// stays at the open-time value and the user gets stuck — the
/// indicator shows the wrong state AND subsequent toggles are
/// no-ops via the idempotent guard.
#[test]
fn set_multiline_mode_refreshes_open_modal_pager_snapshot() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::OpenSettings, &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("expected Settings modal after OpenSettings dispatch")
    };
    assert!(
        !state.pager_snapshot.multiline_mode,
        "snapshot at open should be false (agent default)",
    );
    let _ = dispatch(Action::SetMultilineMode(true), &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("Settings modal must remain open across the dispatch")
    };
    assert!(
        state.pager_snapshot.multiline_mode,
        "snapshot must be refreshed to true after SetMultilineMode(true) — \
             stale-snapshot bug (Round-2 Issue 1) would leave it false",
    );
    let cur_value = crate::settings::current_value_for(
        "multiline_mode",
        &state.ui_snapshot,
        &state.pager_snapshot,
    )
    .expect("multiline_mode must resolve");
    assert_eq!(
        cur_value,
        crate::settings::SettingValue::Bool(true),
        "current_value_for must read the refreshed snapshot",
    );
}
/// Regression test (SHARED path).
///
/// Same bug pattern as the multiline test above, but for
/// `compact_mode` (SHARED). The bug is fixed for the entire
/// SHARED family by calling `refresh_open_settings_modals` from
/// every `set_X` outer, not just for multiline.
#[test]
fn set_compact_mode_refreshes_open_modal_ui_snapshot() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::OpenSettings, &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("expected Settings modal after OpenSettings dispatch")
    };
    assert!(!state.ui_snapshot.compact_mode);
    let _ = dispatch(Action::SetCompactMode(true), &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("Settings modal must remain open across the dispatch")
    };
    assert!(
        state.ui_snapshot.compact_mode,
        "ui_snapshot must be refreshed to true after SetCompactMode(true) — \
             stale-snapshot bug (Round-2 Issue 1) would leave it false",
    );
}
/// `Action::SetVimMode(true)` flips the cache + every agent's
/// field, emits `Effect::PersistSetting`, and writes a toast.
#[test]
fn set_vim_mode_mutates_all_agents_and_cache_no_effect() {
    crate::appearance::cache::set_vim_mode(false);
    let mut app = test_app_with_agent();
    insert_placeholder_agent(&mut app, AgentId(1));
    assert!(!app.agents[&AgentId(0)].vim_mode);
    assert!(!app.agents[&AgentId(1)].vim_mode);
    let effects = dispatch(Action::SetVimMode(true), &mut app);
    assert_eq!(
        effects.len(),
        1,
        "SHELL-owned setter must emit exactly one Effect::PersistSetting, got {effects:?}",
    );
    assert!(
        matches!(
            &effects[0],
            Effect::PersistSetting {
                key: "vim_mode",
                value: crate::settings::SettingValue::Bool(true),
                rollback_value: crate::settings::SettingValue::Bool(false),
            }
        ),
        "unexpected effect: {:?}",
        effects[0],
    );
    assert!(
        crate::appearance::cache::load_vim_mode(),
        "cache must mirror the new value so newly-created agents pick it up",
    );
    assert!(
        app.agents[&AgentId(0)].vim_mode && app.agents[&AgentId(1)].vim_mode,
        "vim_mode must fan out to EVERY agent (background subagents \
             + side panes pick up the change without restart)",
    );
}
/// Re-dispatching the same value is a no-op (no toast, no fan-out).
#[test]
fn set_vim_mode_idempotent_no_toast() {
    crate::appearance::cache::set_vim_mode(false);
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetVimMode(true), &mut app);
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    assert!(agent.toast.is_some(), "first set must toast");
    agent.toast = None;
    let effects = dispatch(Action::SetVimMode(true), &mut app);
    assert!(effects.is_empty());
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "redundant set must not re-toast",
    );
}
#[test]
fn set_keep_text_selection_emits_persist_and_updates_cache() {
    use crate::appearance::TextSelection;
    crate::appearance::cache::set_keep_text_selection(TextSelection::Flash);
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetKeepTextSelection(TextSelection::Hold), &mut app);
    assert_eq!(
        crate::appearance::cache::load_keep_text_selection(),
        TextSelection::Hold
    );
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::PersistSetting {
                key: "keep_text_selection",
                value: crate::settings::SettingValue::Enum("hold"),
                rollback_value: crate::settings::SettingValue::Enum("flash"),
            }]
        ),
        "unexpected effects: {effects:?}"
    );
}
#[test]
fn set_keep_text_selection_rollback_restores_state() {
    use crate::appearance::TextSelection;
    crate::appearance::cache::set_keep_text_selection(TextSelection::Flash);
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetKeepTextSelection(TextSelection::Hold), &mut app);
    assert_eq!(
        crate::appearance::cache::load_keep_text_selection(),
        TextSelection::Hold
    );
    let _ = apply_setting_rollback(
        &mut app,
        "keep_text_selection",
        &crate::settings::SettingValue::Enum("flash"),
    );
    assert_eq!(
        crate::appearance::cache::load_keep_text_selection(),
        TextSelection::Flash,
        "rollback must restore cache"
    );
}
/// `Action::SetVimMode` rollback restores the cache + agents.
#[test]
fn set_vim_mode_rollback_restores_state() {
    crate::appearance::cache::set_vim_mode(false);
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetVimMode(true), &mut app);
    assert!(crate::appearance::cache::load_vim_mode());
    assert!(app.agents[&AgentId(0)].vim_mode);
    let _ = apply_setting_rollback(
        &mut app,
        "vim_mode",
        &crate::settings::SettingValue::Bool(false),
    );
    assert!(
        !crate::appearance::cache::load_vim_mode(),
        "rollback must restore cache",
    );
    assert!(
        !app.agents[&AgentId(0)].vim_mode,
        "rollback must restore agent field",
    );
}
#[test]
fn set_show_thinking_blocks_applies_persists_and_rolls_back() {
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetShowThinkingBlocks(false), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::PersistSetting {
                key: "show_thinking_blocks",
                value: crate::settings::SettingValue::Bool(false),
                rollback_value: crate::settings::SettingValue::Bool(true),
            }]
        ),
        "expected exactly one PersistSetting effect, got {effects:?}",
    );
    assert!(!crate::appearance::cache::load_show_thinking_blocks());
    let effects = dispatch(Action::SetShowThinkingBlocks(false), &mut app);
    assert!(effects.is_empty(), "redundant set must be a no-op");
    let _ = apply_setting_rollback(
        &mut app,
        "show_thinking_blocks",
        &crate::settings::SettingValue::Bool(true),
    );
    assert!(
        crate::appearance::cache::load_show_thinking_blocks(),
        "rollback must restore cache",
    );
}
#[test]
fn set_group_tool_verbs_applies_persists_and_rolls_back() {
    crate::appearance::cache::set_group_tool_verbs(true);
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetGroupToolVerbs(false), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::PersistSetting {
                key: "group_tool_verbs",
                value: crate::settings::SettingValue::Bool(false),
                rollback_value: crate::settings::SettingValue::Bool(true),
            }]
        ),
        "expected exactly one PersistSetting effect, got {effects:?}",
    );
    assert!(!crate::appearance::cache::load_group_tool_verbs());
    let effects = dispatch(Action::SetGroupToolVerbs(false), &mut app);
    assert!(effects.is_empty(), "redundant set must be a no-op");
    let _ = apply_setting_rollback(
        &mut app,
        "group_tool_verbs",
        &crate::settings::SettingValue::Bool(true),
    );
    assert!(
        crate::appearance::cache::load_group_tool_verbs(),
        "rollback must restore cache",
    );
}
/// Toggling the setting re-folds the existing transcript in place
/// (no restart, no unrelated structural event needed).
#[test]
fn set_group_tool_verbs_refolds_live_transcript() {
    use crate::scrollback::block::RenderBlock;
    crate::appearance::cache::set_group_tool_verbs(true);
    let mut app = test_app_with_agent();
    {
        let sb = &mut app.agents.get_mut(&AgentId(0)).unwrap().scrollback;
        sb.push_block(RenderBlock::read("a.rs", None));
        sb.push_block(RenderBlock::read("b.rs", None));
        sb.prepare_layout(80, 40);
        assert_eq!(
            sb.get_cached_entry_height(1),
            Some(0),
            "run folds while the setting is on"
        );
    }
    let _ = dispatch(Action::SetGroupToolVerbs(false), &mut app);
    let sb = &app.agents[&AgentId(0)].scrollback;
    assert!(
        sb.get_cached_entry_height(1).unwrap_or(0) > 0,
        "toggle off must unfold the transcript immediately"
    );
    let _ = dispatch(Action::SetGroupToolVerbs(true), &mut app);
    let sb = &app.agents[&AgentId(0)].scrollback;
    assert_eq!(
        sb.get_cached_entry_height(1),
        Some(0),
        "toggle on must re-fold the transcript immediately"
    );
    crate::appearance::cache::set_group_tool_verbs(true);
}
/// Flipping the setting drops manual group expansions: the id set is shared
/// with N-more dense groups, so a stale verb-slot id must neither mark the
/// coincident dense run expanded after OFF nor reopen the verb slot expanded
/// after ON.
#[test]
fn set_group_tool_verbs_flip_resets_stale_group_expansion() {
    use crate::scrollback::block::RenderBlock;
    crate::appearance::cache::set_group_tool_verbs(true);
    let mut app = test_app_with_agent();
    {
        let sb = &mut app.agents.get_mut(&AgentId(0)).unwrap().scrollback;
        let mut appearance = crate::appearance::AppearanceConfig::default();
        appearance.scrollback.display.group_max_visible = 3;
        sb.set_appearance(appearance);
        for i in 0..6 {
            sb.push_block(RenderBlock::read(format!("f{i}.rs"), None));
        }
        sb.prepare_layout(80, 40);
        sb.set_selected(Some(0));
        assert!(sb.toggle_group_expansion());
        sb.prepare_layout(80, 40);
        let info = sb.get_cached_entry_layouts().unwrap()[0];
        assert!(info.group_collapse_header, "expanded verb slot armed");
    }
    let _ = dispatch(Action::SetGroupToolVerbs(false), &mut app);
    {
        let sb = &mut app.agents.get_mut(&AgentId(0)).unwrap().scrollback;
        sb.prepare_layout(80, 40);
        let info = sb.get_cached_entry_layouts().unwrap()[0];
        assert!(
            !info.group_collapse_header,
            "stale expansion must not mark the dense run expanded after OFF"
        );
    }
    let _ = dispatch(Action::SetGroupToolVerbs(true), &mut app);
    {
        let sb = &mut app.agents.get_mut(&AgentId(0)).unwrap().scrollback;
        sb.prepare_layout(80, 40);
        let info = sb.get_cached_entry_layouts().unwrap()[0];
        assert!(info.verb_group_header, "run refolds as a verb group");
        assert!(
            !info.group_collapse_header,
            "refold must be collapsed, not a reopened expanded slot"
        );
        assert_eq!(
            sb.get_cached_entry_height(1),
            Some(0),
            "members fold behind the collapsed header"
        );
    }
    crate::appearance::cache::set_group_tool_verbs(true);
}
#[test]
fn set_collapsed_edit_blocks_applies_persists_and_rolls_back() {
    crate::appearance::cache::set_collapsed_edit_blocks(false);
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetCollapsedEditBlocks(true), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::PersistSetting {
                key: "collapsed_edit_blocks",
                value: crate::settings::SettingValue::Bool(true),
                rollback_value: crate::settings::SettingValue::Bool(false),
            }]
        ),
        "expected exactly one PersistSetting effect, got {effects:?}",
    );
    assert!(crate::appearance::cache::load_collapsed_edit_blocks());
    let effects = dispatch(Action::SetCollapsedEditBlocks(true), &mut app);
    assert!(effects.is_empty(), "redundant set must be a no-op");
    let _ = apply_setting_rollback(
        &mut app,
        "collapsed_edit_blocks",
        &crate::settings::SettingValue::Bool(false),
    );
    assert!(
        !crate::appearance::cache::load_collapsed_edit_blocks(),
        "rollback must restore cache",
    );
}
/// Toggling the flag re-materializes on-default Edit rows in the live
/// transcript (no restart) — the dispatch-level pin of
/// `apply_collapsed_edit_blocks_flip`'s agent walk.
#[test]
fn set_collapsed_edit_blocks_refolds_live_edit_rows() {
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::blocks::tool::{EditToolCallBlock, ToolCallBlock};
    use crate::scrollback::types::DisplayMode;
    crate::appearance::cache::set_collapsed_edit_blocks(false);
    let mut app = test_app_with_agent();
    let id = {
        let sb = &mut app.agents.get_mut(&AgentId(0)).unwrap().scrollback;
        sb.push_block(RenderBlock::ToolCall(ToolCallBlock::Edit(
            EditToolCallBlock::new("f.rs", vec![]),
        )))
    };
    assert_eq!(
        app.agents[&AgentId(0)]
            .scrollback
            .get_by_id(id)
            .unwrap()
            .display_mode,
        DisplayMode::Expanded,
        "flag off materializes expanded"
    );
    let _ = dispatch(Action::SetCollapsedEditBlocks(true), &mut app);
    assert_eq!(
        app.agents[&AgentId(0)]
            .scrollback
            .get_by_id(id)
            .unwrap()
            .display_mode,
        DisplayMode::Collapsed,
        "toggle on must collapse the on-default Edit row immediately"
    );
    let _ = dispatch(Action::SetCollapsedEditBlocks(false), &mut app);
    assert_eq!(
        app.agents[&AgentId(0)]
            .scrollback
            .get_by_id(id)
            .unwrap()
            .display_mode,
        DisplayMode::Expanded,
        "toggle off must restore the expanded default"
    );
    crate::appearance::cache::set_collapsed_edit_blocks(false);
}
#[test]
fn set_show_thinking_blocks_hides_and_restores_existing_thinking_height() {
    use crate::scrollback::block::RenderBlock;
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).expect("agent 0");
    agent.scrollback.push_block(RenderBlock::thinking(
        "THINKING_HEIGHT_SENTINEL reason step",
    ));
    agent.scrollback.prepare_layout(80, 40);
    let think_idx = (0..agent.scrollback.len())
        .find(|&i| {
            agent
                .scrollback
                .entry(i)
                .is_some_and(|e| e.block.is_thinking())
        })
        .expect("thinking entry present");
    let h_on = agent
        .scrollback
        .get_cached_entry_height(think_idx)
        .expect("cached height after prepare_layout");
    assert!(h_on > 0, "thinking visible when shown, got {h_on}");
    let _ = dispatch(Action::SetShowThinkingBlocks(false), &mut app);
    assert!(!crate::appearance::cache::load_show_thinking_blocks());
    let agent = app.agents.get_mut(&AgentId(0)).expect("agent 0");
    agent.scrollback.prepare_layout(80, 40);
    let h_off = agent
        .scrollback
        .get_cached_entry_height(think_idx)
        .expect("cached height after hide");
    assert_eq!(h_off, 0, "thinking height 0 after hide");
    let _ = dispatch(Action::SetShowThinkingBlocks(true), &mut app);
    let agent = app.agents.get_mut(&AgentId(0)).expect("agent 0");
    agent.scrollback.prepare_layout(80, 40);
    let h_restored = agent
        .scrollback
        .get_cached_entry_height(think_idx)
        .expect("cached height after restore");
    assert!(h_restored > 0, "thinking restored, got {h_restored}");
    crate::appearance::cache::set_show_thinking_blocks(true);
}
/// Collapsed thinking as group_start must not steal the tool "N more" header when hidden.
#[test]
fn set_show_thinking_blocks_off_preserves_tool_group_header() {
    use crate::appearance::AppearanceConfig;
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::types::DisplayMode;
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).expect("agent 0");
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    agent.scrollback.set_appearance(appearance);
    let think_id = agent
        .scrollback
        .push_block(RenderBlock::thinking("hidden-thinking-body"));
    if let Some(e) = agent.scrollback.get_by_id_mut(think_id) {
        e.set_display_mode(DisplayMode::Collapsed);
    }
    for i in 0..6 {
        agent
            .scrollback
            .push_block(RenderBlock::tool_call(format!("Tool{i}"), "info", true));
    }
    agent.scrollback.prepare_layout(80, 40);
    let think_idx = 0usize;
    assert!(
        agent
            .scrollback
            .entry(think_idx)
            .is_some_and(|e| e.block.is_thinking())
    );
    let layouts_on = agent
        .scrollback
        .get_cached_entry_layouts()
        .expect("layout cache");
    assert_eq!(layouts_on[think_idx].height, 1);
    assert_eq!(layouts_on[think_idx].group_header_count, 3);
    let _ = dispatch(Action::SetShowThinkingBlocks(false), &mut app);
    let agent = app.agents.get_mut(&AgentId(0)).expect("agent 0");
    agent.scrollback.prepare_layout(80, 40);
    let layouts = agent
        .scrollback
        .get_cached_entry_layouts()
        .expect("layout cache after hide")
        .to_vec();
    assert_eq!(layouts[think_idx].height, 0);
    assert_eq!(layouts[think_idx].group_header_count, 0);
    assert!(!layouts[think_idx].group_collapse_header);
    let header_idx = 1usize;
    assert!(
        agent
            .scrollback
            .entry(header_idx)
            .is_some_and(|e| e.block.is_tool_call())
    );
    assert_eq!(layouts[header_idx].height, 1);
    assert_eq!(layouts[header_idx].group_header_count, 2);
    assert_eq!(layouts[2].height, 0);
    assert_eq!(layouts[3].height, 0);
    for (i, layout) in layouts.iter().enumerate().take(7).skip(4) {
        assert!(layout.height > 0, "tail tool {i} visible");
    }
    crate::appearance::cache::set_show_thinking_blocks(false);
}
/// Hidden thinking interspersed in a collapsed tool run stays transparent:
/// the run must still truncate to `group_max_visible` instead of splitting
/// in two and over-rendering the group.
#[test]
fn set_show_thinking_blocks_off_truncates_across_interspersed_thinking() {
    use crate::appearance::AppearanceConfig;
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::types::DisplayMode;
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).expect("agent 0");
    let mut appearance = AppearanceConfig::default();
    appearance.scrollback.display.group_max_visible = 3;
    agent.scrollback.set_appearance(appearance);
    for i in 0..2 {
        agent
            .scrollback
            .push_block(RenderBlock::tool_call(format!("Tool{i}"), "info", true));
    }
    let think_id = agent
        .scrollback
        .push_block(RenderBlock::thinking("hidden-thinking-body"));
    if let Some(e) = agent.scrollback.get_by_id_mut(think_id) {
        e.set_display_mode(DisplayMode::Collapsed);
    }
    for i in 2..6 {
        agent
            .scrollback
            .push_block(RenderBlock::tool_call(format!("Tool{i}"), "info", true));
    }
    agent.scrollback.prepare_layout(80, 40);
    let think_idx = 2usize;
    assert!(
        agent
            .scrollback
            .entry(think_idx)
            .is_some_and(|e| e.block.is_thinking())
    );
    let _ = dispatch(Action::SetShowThinkingBlocks(false), &mut app);
    let agent = app.agents.get_mut(&AgentId(0)).expect("agent 0");
    agent.scrollback.prepare_layout(80, 40);
    let layouts = agent
        .scrollback
        .get_cached_entry_layouts()
        .expect("layout cache after hide")
        .to_vec();
    assert_eq!(layouts[think_idx].height, 0);
    assert_eq!(layouts[think_idx].group_header_count, 0);
    assert!(!layouts[think_idx].group_collapse_header);
    assert_eq!(layouts[0].height, 1);
    assert_eq!(layouts[0].group_header_count, 2);
    assert_eq!(layouts[1].height, 0);
    assert_eq!(layouts[3].height, 0);
    for (i, layout) in layouts.iter().enumerate().take(7).skip(4) {
        assert!(layout.height > 0, "tail tool {i} visible");
    }
    crate::appearance::cache::set_show_thinking_blocks(false);
}
/// Flipping thinking visibility reshapes verb-group runs (shown finished
/// thoughts claim into folds; hidden ones stay transparent), so the flip must
/// drop manual group expansions like the `group_tool_verbs` flip does: a
/// stale id must not reopen the reshaped run expanded.
#[test]
fn set_show_thinking_blocks_flip_reshapes_verb_runs_and_resets_expansion() {
    use crate::scrollback::block::RenderBlock;
    use crate::scrollback::types::DisplayMode;
    crate::appearance::cache::set_group_tool_verbs(true);
    crate::appearance::cache::set_show_thinking_blocks(true);
    let mut app = test_app_with_agent();
    {
        let sb = &mut app.agents.get_mut(&AgentId(0)).unwrap().scrollback;
        let think_id = sb.push_block(RenderBlock::thinking("plan"));
        if let Some(e) = sb.get_by_id_mut(think_id) {
            e.set_display_mode(DisplayMode::Collapsed);
        }
        sb.push_block(RenderBlock::read("a.rs", None));
        sb.push_block(RenderBlock::read("b.rs", None));
        sb.prepare_layout(80, 40);
        let layouts = sb.get_cached_entry_layouts().unwrap();
        assert!(layouts[0].verb_group_header, "thought anchors the run");
        assert_eq!(layouts[1].height, 0);
        sb.set_selected(Some(0));
        assert!(sb.toggle_group_expansion());
        sb.prepare_layout(80, 40);
        let layouts = sb.get_cached_entry_layouts().unwrap();
        assert!(layouts[0].group_collapse_header, "expanded slot armed");
    }
    let _ = dispatch(Action::SetShowThinkingBlocks(false), &mut app);
    {
        let sb = &mut app.agents.get_mut(&AgentId(0)).unwrap().scrollback;
        sb.prepare_layout(80, 40);
        let layouts = sb.get_cached_entry_layouts().unwrap();
        assert_eq!(layouts[0].height, 0, "hidden thought takes no rows");
        assert!(layouts[1].verb_group_header, "run re-anchors on the tool");
        assert!(!layouts[1].group_collapse_header);
        assert_eq!(layouts[2].height, 0);
    }
    let _ = dispatch(Action::SetShowThinkingBlocks(true), &mut app);
    {
        let sb = &mut app.agents.get_mut(&AgentId(0)).unwrap().scrollback;
        sb.prepare_layout(80, 40);
        let layouts = sb.get_cached_entry_layouts().unwrap();
        assert!(layouts[0].verb_group_header, "thought re-anchors the run");
        assert!(
            !layouts[0].group_collapse_header,
            "flip must reset the stale expansion"
        );
        assert_eq!(layouts[1].height, 0);
    }
    crate::appearance::cache::set_group_tool_verbs(true);
    crate::appearance::cache::set_show_thinking_blocks(true);
}
/// `Action::SetRespectManualFolds` applies to `app.appearance` AND every
/// agent's scrollback, emits one `Effect::PersistSetting`, is idempotent,
/// and rolls back.
#[test]
fn set_respect_manual_folds_applies_persists_and_rolls_back() {
    let mut app = test_app_with_agent();
    insert_placeholder_agent(&mut app, AgentId(1));
    assert!(!app.appearance.scrollback.scroll.respect_manual_folds);
    let effects = dispatch(Action::SetRespectManualFolds(true), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::PersistSetting {
                key: "respect_manual_folds",
                value: crate::settings::SettingValue::Bool(true),
                rollback_value: crate::settings::SettingValue::Bool(false),
            }]
        ),
        "expected exactly one PersistSetting effect, got {effects:?}",
    );
    assert!(app.appearance.scrollback.scroll.respect_manual_folds);
    for (id, agent) in &app.agents {
        assert!(
            agent
                .scrollback
                .appearance()
                .scrollback
                .scroll
                .respect_manual_folds,
            "agent {id:?} scrollback appearance must pick up the toggle live",
        );
    }
    let effects = dispatch(Action::SetRespectManualFolds(true), &mut app);
    assert!(effects.is_empty(), "redundant set must be a no-op");
    let _ = apply_setting_rollback(
        &mut app,
        "respect_manual_folds",
        &crate::settings::SettingValue::Bool(false),
    );
    assert!(
        !app.appearance.scrollback.scroll.respect_manual_folds,
        "rollback must restore app.appearance",
    );
    assert!(
        !app.agents[&AgentId(0)]
            .scrollback
            .appearance()
            .scrollback
            .scroll
            .respect_manual_folds,
        "rollback must restore agent scrollback appearance",
    );
}
/// `Action::SetRenderMermaid` updates the process-wide cache, emits exactly
/// one `Effect::PersistSetting`, toasts on change, and is idempotent.
#[test]
fn set_render_mermaid_persists_and_is_idempotent() {
    use crate::appearance::RenderMermaid;
    crate::appearance::cache::set_render_mermaid(RenderMermaid::Auto);
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetRenderMermaid(RenderMermaid::Off), &mut app);
    assert_eq!(
        crate::appearance::cache::load_render_mermaid(),
        RenderMermaid::Off,
        "cache mirror must reflect the new value",
    );
    assert_eq!(
        effects.len(),
        1,
        "SHELL-owned setter must emit one Effect::PersistSetting, got {effects:?}",
    );
    assert!(
        matches!(
            &effects[0],
            Effect::PersistSetting {
                key: "render_mermaid",
                value: crate::settings::SettingValue::Enum("off"),
                rollback_value: crate::settings::SettingValue::Enum("auto"),
            }
        ),
        "unexpected effect: {:?}",
        effects[0],
    );
    assert!(read_toast(&app).contains("Mermaid"));
    app.agents.get_mut(&AgentId(0)).unwrap().toast = None;
    let again = dispatch(Action::SetRenderMermaid(RenderMermaid::Off), &mut app);
    assert!(
        again.is_empty(),
        "idempotent re-dispatch must emit no Effect"
    );
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "idempotent re-dispatch must not toast",
    );
    assert_eq!(
        crate::appearance::cache::load_render_mermaid(),
        RenderMermaid::Off,
    );
}
/// `apply_setting_rollback` restores the `render_mermaid` cache mirror.
#[test]
fn set_render_mermaid_rollback_restores_cache() {
    use crate::appearance::RenderMermaid;
    crate::appearance::cache::set_render_mermaid(RenderMermaid::Auto);
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetRenderMermaid(RenderMermaid::Off), &mut app);
    assert_eq!(
        crate::appearance::cache::load_render_mermaid(),
        RenderMermaid::Off,
    );
    let _ = apply_setting_rollback(
        &mut app,
        "render_mermaid",
        &crate::settings::SettingValue::Enum("auto"),
    );
    assert_eq!(
        crate::appearance::cache::load_render_mermaid(),
        RenderMermaid::Auto,
        "rollback must restore the cache mirror",
    );
}
/// `Action::SetScrollSpeed` updates the process-wide cache,
/// recomputes `app.scroll_config`, and emits `Effect::PersistSetting`.
/// Clamps to `[1, 100]`.
#[test]
fn set_scroll_speed_updates_cache_and_clamps() {
    crate::appearance::cache::set_scroll_speed(50);
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetScrollSpeed(75), &mut app);
    assert_eq!(
        effects.len(),
        1,
        "SHELL-owned setter must emit exactly one Effect::PersistSetting, got {effects:?}",
    );
    assert!(
        matches!(
            &effects[0],
            Effect::PersistSetting {
                key: "scroll_speed",
                value: crate::settings::SettingValue::Int(75),
                rollback_value: crate::settings::SettingValue::Int(50),
            }
        ),
        "unexpected effect: {:?}",
        effects[0],
    );
    assert_eq!(crate::appearance::cache::load_scroll_speed(), 75);
    let _ = dispatch(Action::SetScrollSpeed(250), &mut app);
    assert_eq!(crate::appearance::cache::load_scroll_speed(), 100);
    let _ = dispatch(Action::SetScrollSpeed(-5), &mut app);
    assert_eq!(crate::appearance::cache::load_scroll_speed(), 1);
}
/// `Action::SetScrollSpeed` with the current value is a no-op.
#[test]
fn set_scroll_speed_idempotent() {
    crate::appearance::cache::set_scroll_speed(42);
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().toast = None;
    let effects = dispatch(Action::SetScrollSpeed(42), &mut app);
    assert!(effects.is_empty());
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "redundant set must not re-toast",
    );
}
/// `Action::SetScrollSpeed` rollback restores the cache.
#[test]
fn set_scroll_speed_rollback_restores_state() {
    crate::appearance::cache::set_scroll_speed(50);
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetScrollSpeed(80), &mut app);
    assert_eq!(crate::appearance::cache::load_scroll_speed(), 80);
    let _ = apply_setting_rollback(
        &mut app,
        "scroll_speed",
        &crate::settings::SettingValue::Int(50),
    );
    assert_eq!(
        crate::appearance::cache::load_scroll_speed(),
        50,
        "rollback must restore cache",
    );
}
/// `Action::SetScrollMode` updates the process-wide cache, rebuilds
/// `app.scroll_config` with the forced mode, and emits `Effect::PersistSetting`.
#[test]
fn set_scroll_mode_updates_cache_and_scroll_config() {
    crate::appearance::cache::set_scroll_mode(crate::appearance::ScrollMode::Auto);
    let mut app = test_app_with_agent();
    let effects = dispatch(
        Action::SetScrollMode(crate::appearance::ScrollMode::Wheel),
        &mut app,
    );
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::PersistSetting {
                key: "scroll_mode",
                value: crate::settings::SettingValue::Enum("wheel"),
                rollback_value: crate::settings::SettingValue::Enum("auto"),
            }]
        ),
        "unexpected effects: {effects:?}",
    );
    assert_eq!(
        crate::appearance::cache::load_scroll_mode(),
        crate::appearance::ScrollMode::Wheel
    );
    assert!(
        format!("{:?}", app.scroll_config).contains("mode: Wheel"),
        "scroll_config must rebuild with the forced mode: {:?}",
        app.scroll_config,
    );
    let effects = dispatch(
        Action::SetScrollMode(crate::appearance::ScrollMode::Wheel),
        &mut app,
    );
    assert!(effects.is_empty());
}
/// `Action::SetInvertScroll` updates the cache and flips the rebuilt
/// config's direction bit.
#[test]
fn set_invert_scroll_updates_cache_and_scroll_config() {
    crate::appearance::cache::set_invert_scroll(false);
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetInvertScroll(true), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::PersistSetting {
                key: "invert_scroll",
                value: crate::settings::SettingValue::Bool(true),
                rollback_value: crate::settings::SettingValue::Bool(false),
            }]
        ),
        "unexpected effects: {effects:?}",
    );
    assert!(crate::appearance::cache::load_invert_scroll());
    assert!(
        format!("{:?}", app.scroll_config).contains("invert_direction: true"),
        "scroll_config must rebuild inverted: {:?}",
        app.scroll_config,
    );
    let _ = apply_setting_rollback(
        &mut app,
        "invert_scroll",
        &crate::settings::SettingValue::Bool(false),
    );
    assert!(!crate::appearance::cache::load_invert_scroll());
    assert!(format!("{:?}", app.scroll_config).contains("invert_direction: false"));
}
/// `Action::SetScrollLines` updates the cache (clamped to `[1, 10]`) and
/// rebuilds the config with BOTH per-tick values overridden.
#[test]
fn set_scroll_lines_updates_cache_and_clamps() {
    crate::appearance::cache::set_scroll_lines(3);
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SetScrollLines(5), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::PersistSetting {
                key: "scroll_lines",
                value: crate::settings::SettingValue::Int(5),
                rollback_value: crate::settings::SettingValue::Int(3),
            }]
        ),
        "unexpected effects: {effects:?}",
    );
    assert_eq!(crate::appearance::cache::load_scroll_lines(), Some(5));
    let debug = format!("{:?}", app.scroll_config);
    assert!(
        debug.contains("wheel_lines_per_tick: 5") && debug.contains("trackpad_lines_per_tick: 5"),
        "one knob must override both per-tick values: {debug}",
    );
    let _ = dispatch(Action::SetScrollLines(99), &mut app);
    assert_eq!(crate::appearance::cache::load_scroll_lines(), Some(10));
    let _ = dispatch(Action::SetScrollLines(-2), &mut app);
    assert_eq!(crate::appearance::cache::load_scroll_lines(), Some(1));
}
/// A NON-permission setting rollback must NOT touch the active agent's
/// per-session `auto_mode`. Only the `permission_mode` rollback arm syncs it;
/// otherwise the per-session flag drifts from the (possibly stale) global UI
/// mirror on multi-agent setups.
#[test]
fn non_permission_rollback_preserves_session_auto_mode() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().session.auto_mode = true;
    app.current_ui.permission_mode = Some("ask".into());
    apply_setting_rollback(&mut app, "show_tips", &SettingValue::Bool(true));
    assert!(
        app.agents[&AgentId(0)].session.is_auto(),
        "non-permission rollback must not clobber the per-session auto flag"
    );
}
/// Rollback path: a `SettingPersistFailed` for `permission_mode`
/// reverts `agent.session.yolo_mode` + `app.default_yolo` +
/// `app.current_ui.permission_mode` via `set_yolo_mode_inner`.
/// MUST NOT re-emit any effects (would loop on persistent disk
/// failure).
#[test]
fn rollback_permission_mode_reverts_state_no_effect() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::SetYoloMode(true), &mut app);
    assert!(app.agents[&AgentId(0)].session.is_yolo());
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("always-approve")
    );
    let effects = dispatch(
        Action::TaskComplete(TaskResult::SettingPersistFailed {
            key: "permission_mode",
            rollback_value: SettingValue::Enum("ask"),
            error: "permission denied".into(),
        }),
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "rollback path MUST NOT emit any Effect (would loop on persistent failure)",
    );
    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "session.yolo_mode must revert via apply_setting_rollback"
    );
    assert!(!app.default_yolo);
    assert_eq!(app.current_ui.permission_mode.as_deref(), Some("ask"));
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(s, _)| s.clone())
        .expect("failure toast must be set");
    assert_eq!(
        toast, "\u{2717} Could not save permission_mode: permission denied",
        "rollback toast must follow the exact `✗ Could not save {{key}}: {{error}}` format \
             — drift here would diverge from other rollback toasts",
    );
}
/// The reset-dispatch test asserts the typed Action lineage. The
/// reset path dispatches `Action::SetPermissionMode(Ask)` (the
/// registered default) rather than `Action::SetYoloMode(false)`.
/// Both emit `Effect::PersistPermissionMode` so the `has_persist`
/// assertion in the parent test holds; this test pins the typed
/// Action shape directly so a future refactor that drops the
/// `SetPermissionMode` mapping in `action_for_reset` is caught.
#[test]
fn action_for_reset_permission_mode_dispatches_set_permission_mode_for_each_canonical() {
    use crate::app::actions::PermissionModeKind;
    use crate::settings::SettingValue;
    match action_for_reset("permission_mode", &SettingValue::Enum("ask")) {
        Some(Action::SetPermissionMode(PermissionModeKind::Ask)) => {}
        other => {
            panic!(
                "action_for_reset(permission_mode, 'ask') must produce \
                 Action::SetPermissionMode(Ask), got {other:?}"
            )
        }
    }
    match action_for_reset("permission_mode", &SettingValue::Enum("always-approve")) {
        Some(Action::SetPermissionMode(PermissionModeKind::AlwaysApprove)) => {}
        other => {
            panic!(
                "action_for_reset(permission_mode, 'always-approve') must produce \
                 Action::SetPermissionMode(AlwaysApprove), got {other:?}"
            )
        }
    }
    match action_for_reset("permission_mode", &SettingValue::Enum("default")) {
        Some(Action::SetPermissionMode(PermissionModeKind::Default)) => {}
        other => {
            panic!(
                "action_for_reset(permission_mode, 'default') must produce \
                 Action::SetPermissionMode(Default), got {other:?}"
            )
        }
    }
    assert!(action_for_reset("permission_mode", &SettingValue::Enum("bogus")).is_none());
}
/// Slash-command-while-modal-open refresh.
/// `dispatch_cycle_mode` is the entry point for both Shift+Tab
/// and the `/plan` / `/cycle-mode` slash commands. When the
/// settings modal is open and a cycle lands, the modal's
/// `pager_snapshot.plan_mode_active` must refresh to reflect
/// the new effective state — otherwise the indicator stays
/// stale until the next setter dispatch. Mirror of
/// `set_plan_mode_refreshes_open_modal_pager_snapshot`.
#[test]
fn dispatch_cycle_mode_refreshes_open_modal_snapshot() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::OpenSettings, &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("expected Settings modal after OpenSettings dispatch")
    };
    assert!(
        !state.pager_snapshot.plan_mode_active,
        "snapshot at open should be false (agent default)",
    );
    let _ = dispatch(Action::CycleMode, &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("Settings modal must remain open across CycleMode")
    };
    assert!(
        state.pager_snapshot.plan_mode_active,
        "snapshot must be refreshed to true after CycleMode (Normal → Plan) — \
             without refresh_open_settings_modals the indicator would stay stale",
    );
    let cur_value =
        crate::settings::current_value_for("plan_mode", &state.ui_snapshot, &state.pager_snapshot)
            .expect("plan_mode must resolve");
    assert_eq!(
        cur_value,
        crate::settings::SettingValue::Enum("on"),
        "current_value_for must read the refreshed snapshot",
    );
}
/// `dispatch(Action::SetTheme("echo-agent-day"), &mut app)` emits
/// exactly one `Effect::PersistSetting`, mutates
/// `app.current_ui.theme`, fires a toast, and toggles AUTO_MODE
/// off (kind is concrete).
///
/// Note: we persist `echo-agent-day` (a non-truecolor theme) here
/// because `Effect::PersistSetting`'s payload is `&'static str`
/// from the registry's canonical table — the persisted CANONICAL
/// is what we're asserting, NOT the live theme cache (which
/// `clamp_to_terminal` might fold to EchoAgentNight in non-truecolor
/// test environments). Persist + canonical contract is the test
/// invariant; the cache contract is exercised separately by the
/// `*_applies_when_*` tests.
#[test]
fn set_theme_emits_persist_setting_with_correct_payload() {
    use crate::settings::SettingValue;
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        assert_eq!(app.current_ui.theme, None);
        crate::theme::cache::set(crate::theme::ThemeKind::EchoAgentNight);
        let effects = dispatch(Action::SetTheme("echo-agent-day".into()), &mut app);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::PersistSetting {
                key,
                value,
                rollback_value,
            } => {
                assert_eq!(*key, "theme");
                assert_eq!(*value, SettingValue::Enum("echo-agent-day"));
                assert_eq!(*rollback_value, SettingValue::Enum("echo-agent-night"));
            }
            other => panic!("expected PersistSetting, got {other:?}"),
        }
        assert_eq!(app.current_ui.theme.as_deref(), Some("echo-agent-day"));
        assert!(
            !crate::theme::cache::is_auto_mode(),
            "concrete theme commit must disable AUTO_MODE",
        );
    });
}
/// Same payload contract as `set_theme_emits_persist_setting_with_correct_payload`
/// for the auto-dark sibling. Uses `echo-agent-day` to avoid the
/// `clamp_to_terminal` ambiguity in non-truecolor test envs;
/// `apply_kind` doesn't fire here anyway (parent theme is not
/// auto by default) but using a non-truecolor canonical keeps
/// the test robust to environment differences.
#[test]
fn set_auto_dark_theme_emits_persist_setting_with_correct_payload() {
    use crate::settings::SettingValue;
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let effects = dispatch(Action::SetAutoDarkTheme("echo-agent-day".into()), &mut app);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::PersistSetting {
                key,
                value,
                rollback_value,
            } => {
                assert_eq!(*key, "auto_dark_theme");
                assert_eq!(*value, SettingValue::Enum("echo-agent-day"));
                assert_eq!(*rollback_value, SettingValue::Enum("echo-agent-night"));
            }
            other => panic!("expected PersistSetting, got {other:?}"),
        }
        assert_eq!(
            app.current_ui.auto_dark_theme.as_deref(),
            Some("echo-agent-day")
        );
    });
}
#[test]
fn set_auto_light_theme_emits_persist_setting_with_correct_payload() {
    use crate::settings::SettingValue;
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let effects = dispatch(
            Action::SetAutoLightTheme("echo-agent-night".into()),
            &mut app,
        );
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::PersistSetting {
                key,
                value,
                rollback_value,
            } => {
                assert_eq!(*key, "auto_light_theme");
                assert_eq!(*value, SettingValue::Enum("echo-agent-night"));
                assert_eq!(*rollback_value, SettingValue::Enum("echo-agent-day"));
            }
            other => panic!("expected PersistSetting, got {other:?}"),
        }
        assert_eq!(
            app.current_ui.auto_light_theme.as_deref(),
            Some("echo-agent-night"),
        );
    });
}
/// PREVIEW Actions emit ZERO `Effect::PersistSetting` and do NOT
/// mutate `app.current_ui.theme`. This is the fix for
/// per-keystroke disk write race.
#[test]
fn preview_theme_emits_no_persist_effect_and_no_current_ui_mutation() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let initial = app.current_ui.theme.clone();
        let effects = dispatch(Action::PreviewTheme("tokyonight".into()), &mut app);
        let persist_count = effects
            .iter()
            .filter(|e| matches!(e, Effect::PersistSetting { .. }))
            .count();
        assert_eq!(
            persist_count, 0,
            "preview must NOT emit PersistSetting, got {persist_count} (effects: {effects:?})",
        );
        assert_eq!(
            app.current_ui.theme, initial,
            "preview must NOT mutate app.current_ui.theme",
        );
    });
}
#[test]
fn preview_auto_dark_theme_emits_no_persist_and_no_current_ui_mutation() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let initial = app.current_ui.auto_dark_theme.clone();
        let effects = dispatch(Action::PreviewAutoDarkTheme("tokyonight".into()), &mut app);
        assert!(effects.is_empty());
        assert_eq!(app.current_ui.auto_dark_theme, initial);
    });
}
#[test]
fn preview_auto_light_theme_emits_no_persist_and_no_current_ui_mutation() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let initial = app.current_ui.auto_light_theme.clone();
        let effects = dispatch(
            Action::PreviewAutoLightTheme("rosepine-moon".into()),
            &mut app,
        );
        assert!(effects.is_empty());
        assert_eq!(app.current_ui.auto_light_theme, initial);
    });
}
/// Auto-theme commit applies the live theme **only** when
/// `theme="auto"` AND the system is in the matching mode.
///
/// Scenario: `theme="echo-agent-night"` (concrete) + system=Dark. User
/// commits `auto_dark_theme="echo-agent-day"`. The setting is dormant
/// (parent theme is concrete, not auto), so the live display
/// must stay on EchoAgentNight even though we're committing a
/// different theme. Uses `echo-agent-day` to avoid `clamp_to_terminal`
/// ambiguity in non-truecolor envs.
#[test]
fn set_auto_dark_theme_does_not_apply_when_theme_is_not_auto() {
    with_theme_test_env(|| {
        crate::theme::system_appearance::set_mock(Some(
            crate::theme::system_appearance::SystemAppearance::Dark,
        ));
        let mut app = test_app_with_agent();
        let _ = dispatch(Action::SetTheme("echo-agent-night".into()), &mut app);
        assert_eq!(
            crate::theme::cache::current_kind(),
            crate::theme::ThemeKind::EchoAgentNight,
        );
        let _ = dispatch(Action::SetAutoDarkTheme("echo-agent-day".into()), &mut app);
        assert_eq!(
            crate::theme::cache::current_kind(),
            crate::theme::ThemeKind::EchoAgentNight,
            "auto_dark_theme commit must NOT change live display when theme is not auto",
        );
        assert_eq!(
            app.current_ui.auto_dark_theme.as_deref(),
            Some("echo-agent-day")
        );
    });
}
/// Auto-theme commit DOES apply the live theme when both
/// (a) parent theme = auto AND (b) system matches.
///
/// Uses `EchoAgentDay` (non-truecolor-requiring) for the dark-mode
/// fixture: the test environment's color detection may not report
/// truecolor support, and `Theme::apply_kind` clamps
/// truecolor-only themes (TokyoNight, RosePineMoon) down to
/// EchoAgentNight. Using a non-truecolor theme avoids the clamp
/// uncertainty. The "live apply" contract is what we're testing
/// — the specific theme picked is incidental.
#[test]
fn set_auto_dark_theme_applies_when_theme_is_auto_and_system_is_dark() {
    with_theme_test_env(|| {
        crate::theme::system_appearance::set_mock(Some(
            crate::theme::system_appearance::SystemAppearance::Dark,
        ));
        let mut app = test_app_with_agent();
        let _ = dispatch(Action::SetTheme("auto".into()), &mut app);
        assert!(crate::theme::cache::is_auto_mode());
        assert_eq!(
            crate::theme::cache::current_kind(),
            crate::theme::ThemeKind::EchoAgentNight,
        );
        let _ = dispatch(Action::SetAutoDarkTheme("echo-agent-day".into()), &mut app);
        assert_eq!(
            crate::theme::cache::current_kind(),
            crate::theme::ThemeKind::EchoAgentDay,
            "auto_dark_theme commit must update live display when theme=auto + system=Dark",
        );
    });
}
/// Auto-theme commit does NOT apply when system is in the
/// non-matching mode (auto_dark_theme + system=Light).
/// Uses `echo-agent-night` for the auto_dark_theme
/// value to avoid `clamp_to_terminal` ambiguity (we want a
/// concrete kind that's clearly different from EchoAgentDay, the
/// active resolved theme).
#[test]
fn set_auto_dark_theme_does_not_apply_when_system_is_light() {
    with_theme_test_env(|| {
        crate::theme::system_appearance::set_mock(Some(
            crate::theme::system_appearance::SystemAppearance::Light,
        ));
        let mut app = test_app_with_agent();
        let _ = dispatch(Action::SetTheme("auto".into()), &mut app);
        assert_eq!(
            crate::theme::cache::current_kind(),
            crate::theme::ThemeKind::EchoAgentDay,
        );
        let _ = dispatch(
            Action::SetAutoDarkTheme("echo-agent-night".into()),
            &mut app,
        );
        assert_eq!(
            crate::theme::cache::current_kind(),
            crate::theme::ThemeKind::EchoAgentDay,
            "auto_dark_theme commit must NOT change live display when system=Light",
        );
        assert_eq!(
            app.current_ui.auto_dark_theme.as_deref(),
            Some("echo-agent-night"),
        );
    });
}
/// Symmetric to the dark test: `set_auto_light_theme` applies only
/// when theme=auto + system=Light. Uses a non-truecolor theme
/// (`echo-agent-night`) for the same clamp reason as the dark variant.
#[test]
fn set_auto_light_theme_applies_when_theme_is_auto_and_system_is_light() {
    with_theme_test_env(|| {
        crate::theme::system_appearance::set_mock(Some(
            crate::theme::system_appearance::SystemAppearance::Light,
        ));
        let mut app = test_app_with_agent();
        let _ = dispatch(Action::SetTheme("auto".into()), &mut app);
        assert_eq!(
            crate::theme::cache::current_kind(),
            crate::theme::ThemeKind::EchoAgentDay,
        );
        let _ = dispatch(
            Action::SetAutoLightTheme("echo-agent-night".into()),
            &mut app,
        );
        assert_eq!(
            crate::theme::cache::current_kind(),
            crate::theme::ThemeKind::EchoAgentNight,
            "auto_light_theme must update display when theme=auto + system=Light",
        );
    });
}
/// `Action::SetTheme` with an unknown theme name returns an empty
/// Effect vec AND does not mutate `app.current_ui.theme`.
/// Defense-in-depth at the dispatcher (the registry's choice
/// table is the primary protection).
#[test]
fn set_theme_unknown_name_is_no_op() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let initial = app.current_ui.theme.clone();
        let effects = dispatch(Action::SetTheme("nonexistent_theme".into()), &mut app);
        assert!(effects.is_empty(), "unknown name must produce no effects");
        assert_eq!(
            app.current_ui.theme, initial,
            "unknown name must not mutate app.current_ui.theme",
        );
    });
}
#[test]
fn set_auto_dark_theme_unknown_name_is_no_op() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let initial = app.current_ui.auto_dark_theme.clone();
        let effects = dispatch(Action::SetAutoDarkTheme("nonexistent".into()), &mut app);
        assert!(effects.is_empty());
        assert_eq!(app.current_ui.auto_dark_theme, initial);
    });
}
#[test]
fn set_auto_light_theme_unknown_name_is_no_op() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let initial = app.current_ui.auto_light_theme.clone();
        let effects = dispatch(Action::SetAutoLightTheme("nonexistent".into()), &mut app);
        assert!(effects.is_empty());
        assert_eq!(app.current_ui.auto_light_theme, initial);
    });
}
/// `Action::SetAutoDarkTheme("auto")` is rejected — would cause a
/// circular reference (mirrors `load_auto_theme_config`'s filter
/// at the disk-read layer).
#[test]
fn set_auto_dark_theme_rejects_auto_value() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let initial = app.current_ui.auto_dark_theme.clone();
        let effects = dispatch(Action::SetAutoDarkTheme("auto".into()), &mut app);
        assert!(
            effects.is_empty(),
            "auto value must be rejected (circular reference prevention)",
        );
        assert_eq!(app.current_ui.auto_dark_theme, initial);
    });
}
#[test]
fn set_auto_light_theme_rejects_auto_value() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let initial = app.current_ui.auto_light_theme.clone();
        let effects = dispatch(Action::SetAutoLightTheme("auto".into()), &mut app);
        assert!(effects.is_empty());
        assert_eq!(app.current_ui.auto_light_theme, initial);
    });
}
/// Toast format for theme commits uses the DISPLAY name, not the
/// snake_case canonical.
#[test]
fn set_theme_toast_format_uses_display_name() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let _ = dispatch(Action::SetTheme("echo-agent-day".into()), &mut app);
        let toast = read_toast(&app);
        assert!(
            toast.contains("Theme"),
            "toast must contain label, got: {toast:?}",
        );
        assert!(
            toast.contains("EchoAgent Day"),
            "toast must use display name `EchoAgent Day`, not canonical `echo-agent-day`, got: {toast:?}",
        );
        assert!(toast.contains('\u{2713}'), "toast must contain the ✓ glyph");
    });
}
#[test]
fn set_auto_dark_theme_toast_format_uses_display_name() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let _ = dispatch(Action::SetAutoDarkTheme("echo-agent-day".into()), &mut app);
        let toast = read_toast(&app);
        assert!(toast.contains("Auto dark theme"));
        assert!(toast.contains("EchoAgent Day"));
        assert!(toast.contains('\u{2713}'));
    });
}
#[test]
fn set_auto_light_theme_toast_format_uses_display_name() {
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let _ = dispatch(
            Action::SetAutoLightTheme("echo-agent-night".into()),
            &mut app,
        );
        let toast = read_toast(&app);
        assert!(toast.contains("Auto light theme"));
        assert!(toast.contains("EchoAgent Night"));
    });
}
/// `apply_setting_rollback` for theme keys: a failed persist
/// reverts `app.current_ui.theme` AND the live cache (mirror of
/// `rollback_known_key_reverts_cache_and_no_effect`).
///
/// Uses non-truecolor themes (`echo-agent-day` ↔ `echo-agent-night`) to avoid
/// the `clamp_to_terminal` interaction in test environments that
/// don't report truecolor support.
#[test]
fn rollback_theme_reverts_current_ui_and_cache() {
    use crate::settings::SettingValue;
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let _ = dispatch(Action::SetTheme("echo-agent-day".into()), &mut app);
        assert_eq!(app.current_ui.theme.as_deref(), Some("echo-agent-day"));
        assert_eq!(
            crate::theme::cache::current_kind(),
            crate::theme::ThemeKind::EchoAgentDay,
        );
        let _ = dispatch(
            Action::TaskComplete(TaskResult::SettingPersistFailed {
                key: "theme",
                rollback_value: SettingValue::Enum("echo-agent-night"),
                error: "disk full".into(),
            }),
            &mut app,
        );
        assert_eq!(
            app.current_ui.theme.as_deref(),
            Some("echo-agent-night"),
            "rollback must update app.current_ui.theme",
        );
        assert_eq!(
            crate::theme::cache::current_kind(),
            crate::theme::ThemeKind::EchoAgentNight,
            "rollback must update the live theme cache too",
        );
    });
}
#[test]
fn rollback_auto_dark_theme_reverts_current_ui() {
    use crate::settings::SettingValue;
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let _ = dispatch(Action::SetAutoDarkTheme("echo-agent-day".into()), &mut app);
        assert_eq!(
            app.current_ui.auto_dark_theme.as_deref(),
            Some("echo-agent-day")
        );
        let _ = dispatch(
            Action::TaskComplete(TaskResult::SettingPersistFailed {
                key: "auto_dark_theme",
                rollback_value: SettingValue::Enum("echo-agent-night"),
                error: "disk full".into(),
            }),
            &mut app,
        );
        assert_eq!(
            app.current_ui.auto_dark_theme.as_deref(),
            Some("echo-agent-night"),
        );
    });
}
#[test]
fn rollback_auto_light_theme_reverts_current_ui() {
    use crate::settings::SettingValue;
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let _ = dispatch(
            Action::SetAutoLightTheme("echo-agent-night".into()),
            &mut app,
        );
        assert_eq!(
            app.current_ui.auto_light_theme.as_deref(),
            Some("echo-agent-night"),
        );
        let _ = dispatch(
            Action::TaskComplete(TaskResult::SettingPersistFailed {
                key: "auto_light_theme",
                rollback_value: SettingValue::Enum("echo-agent-day"),
                error: "disk full".into(),
            }),
            &mut app,
        );
        assert_eq!(
            app.current_ui.auto_light_theme.as_deref(),
            Some("echo-agent-day")
        );
    });
}
/// Edge case — if the rollback value is
/// `"auto"` (corrupted hand-edit), `apply_setting_rollback` clears
/// `app.current_ui.auto_dark_theme` to `None` (matches the
/// `load_auto_theme_config` disk-read filter). Without this
/// special case, `set_auto_dark_theme_inner` would no-op and
/// leave the in-memory cache stuck at the (failed-to-persist)
/// new value.
#[test]
fn rollback_auto_dark_theme_with_auto_value_clears_to_none() {
    use crate::settings::SettingValue;
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let _ = dispatch(Action::SetAutoDarkTheme("echo-agent-day".into()), &mut app);
        assert_eq!(
            app.current_ui.auto_dark_theme.as_deref(),
            Some("echo-agent-day")
        );
        let _ = dispatch(
            Action::TaskComplete(TaskResult::SettingPersistFailed {
                key: "auto_dark_theme",
                rollback_value: SettingValue::Enum("auto"),
                error: "disk full".into(),
            }),
            &mut app,
        );
        assert_eq!(
            app.current_ui.auto_dark_theme, None,
            "rolling back to `auto` must clear the in-memory override (it's invalid; \
                 the disk-read filter would also drop it)",
        );
    });
}
#[test]
fn rollback_auto_light_theme_with_auto_value_clears_to_none() {
    use crate::settings::SettingValue;
    with_theme_test_env(|| {
        let mut app = test_app_with_agent();
        let _ = dispatch(
            Action::SetAutoLightTheme("echo-agent-night".into()),
            &mut app,
        );
        assert_eq!(
            app.current_ui.auto_light_theme.as_deref(),
            Some("echo-agent-night"),
        );
        let _ = dispatch(
            Action::TaskComplete(TaskResult::SettingPersistFailed {
                key: "auto_light_theme",
                rollback_value: SettingValue::Enum("auto"),
                error: "disk full".into(),
            }),
            &mut app,
        );
        assert_eq!(app.current_ui.auto_light_theme, None);
    });
}
/// Setter refreshes the open settings modal
/// snapshot so the indicator reflects the new value before the
/// shell's CurrentModeUpdate round-trip. Mirrors
/// `set_multiline_mode_refreshes_open_modal_pager_snapshot`.
#[test]
fn set_plan_mode_refreshes_open_modal_pager_snapshot() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    let _ = dispatch(Action::OpenSettings, &mut app);
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("expected Settings modal after OpenSettings dispatch")
    };
    assert!(
        !state.pager_snapshot.plan_mode_active,
        "snapshot at open should be false (agent default)",
    );
    let _ = dispatch(
        Action::SetPlanMode(crate::app::actions::PlanModeKind::On),
        &mut app,
    );
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let Some(ActiveModal::Settings { state }) = &agent.active_modal else {
        panic!("Settings modal must remain open across the dispatch")
    };
    assert!(
        state.pager_snapshot.plan_mode_active,
        "snapshot must be refreshed to true after SetPlanMode(On) — \
             without refresh_open_settings_modals the indicator would stay stale",
    );
    let cur_value =
        crate::settings::current_value_for("plan_mode", &state.ui_snapshot, &state.pager_snapshot)
            .expect("plan_mode must resolve");
    assert_eq!(
        cur_value,
        crate::settings::SettingValue::Enum("on"),
        "current_value_for must read the refreshed snapshot",
    );
}
#[test]
fn new_session_inherits_switched_default_model_for_welcome() {
    use crate::acp::model_state::ModelState;
    use std::sync::Arc;
    let mut app = test_app_with_agent();
    let mk = |slug: &str, name: &str| {
        let id = acp::ModelId::new(Arc::from(slug));
        (id.clone(), acp::ModelInfo::new(id, name.to_string()))
    };
    let (id_a, info_a) = mk("model-a", "Model A");
    let (id_b, info_b) = mk("model-b", "Model B");
    let mut models = ModelState::default();
    models.available.insert(id_a.clone(), info_a);
    models.available.insert(id_b.clone(), info_b);
    models.current = Some(id_a.clone());
    app.models = models.clone();
    app.agents.get_mut(&AgentId(0)).unwrap().session.models = models;
    assert!(set_default_model_inner(&mut app, &id_b));
    assert_eq!(
        app.models.current.as_ref(),
        Some(&id_b),
        "the switch must update the app-level default new sessions clone"
    );
    let _ = dispatch(Action::NewSession, &mut app);
    let new_agent = app.agents.get(&AgentId(1)).expect("new session created");
    assert_eq!(
        new_agent.session.models.current_model_name().as_deref(),
        Some("Model B"),
        "the new session — and its committed welcome card — must show the \
             switched model, not the previous default"
    );
}
/// Without config, the action is unregistered — Ctrl+R on scrollback does
/// nothing for mouse reporting (Ctrl+R is unbound on the prompt).
#[test]
fn mouse_reporting_toggle_inactive_without_config() {
    use crate::actions::{ActionId, ActionRegistry, When};
    let reg_off = ActionRegistry::defaults();
    let reg_on = ActionRegistry::defaults_with_config(true);
    let ctrl_r = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('r'),
        crossterm::event::KeyModifiers::CONTROL,
    );
    assert!(reg_off.find(ActionId::ToggleMouseCapture).is_none());
    assert_eq!(
        reg_off.lookup(&ctrl_r, When::ScrollbackFocused),
        None,
        "scrollback Ctrl+R must not toggle mouse without config"
    );
    assert_eq!(
        reg_off.lookup(&ctrl_r, When::PromptFocused),
        None,
        "Ctrl+R is deliberately unbound on the prompt"
    );
    assert!(reg_on.find(ActionId::ToggleMouseCapture).is_some());
    assert_eq!(
        reg_on.lookup(&ctrl_r, When::ScrollbackFocused),
        Some(ActionId::ToggleMouseCapture),
    );
    assert_eq!(
        reg_on.lookup(&ctrl_r, When::PromptFocused),
        None,
        "prompt Ctrl+R stays unbound even when the toggle is enabled"
    );
}
/// Config on: toggle off sets process atomic + sticky banner that survives
/// transient toasts and tick/keypress dismissal of transients.
#[serial_test::serial(MOUSE_CAPTURE_ENABLED)]
#[test]
fn mouse_reporting_toggle_off_sticky_persists_after_transient_toast() {
    reset_mouse_capture_enabled(true);
    assert!(mouse_capture_is_enabled());
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.registry = crate::actions::ActionRegistry::defaults_with_config(true);
    let _ = dispatch(Action::ToggleMouseCapture, &mut app);
    assert!(
        !mouse_capture_is_enabled(),
        "toggle off must clear process-wide capture flag"
    );
    let parent = app.agents.get(&id).unwrap();
    assert_eq!(
        parent.sticky_toast.as_deref(),
        Some(MOUSE_OFF_STICKY),
        "sticky banner set on parent"
    );
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.show_toast("Copied!");
        assert_eq!(
            agent.toast.as_ref().map(|(m, _)| m.as_str()),
            Some("Copied!"),
            "transient wins while active"
        );
        assert_eq!(agent.sticky_toast.as_deref(), Some(MOUSE_OFF_STICKY));
        if let Some((_, ref mut ticks)) = agent.toast {
            *ticks = 0;
        }
        assert!(agent.tick_toast());
        assert!(agent.toast.is_none());
        assert_eq!(
            agent.sticky_toast.as_deref(),
            Some(MOUSE_OFF_STICKY),
            "sticky remains after transient expires"
        );
        agent.show_toast("temp");
        agent.toast = None;
        assert_eq!(agent.sticky_toast.as_deref(), Some(MOUSE_OFF_STICKY));
    }
    let _ = dispatch(Action::ToggleMouseCapture, &mut app);
    assert!(mouse_capture_is_enabled());
    let parent = app.agents.get(&id).unwrap();
    assert!(parent.sticky_toast.is_none());
    assert_eq!(
        parent.toast.as_ref().map(|(m, _)| m.as_str()),
        Some("Mouse reporting on"),
    );
    reset_mouse_capture_enabled(true);
}
