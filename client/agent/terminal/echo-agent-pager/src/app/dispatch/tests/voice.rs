//! Tests for voice mode enable, toggle, and stop dispatchers.

use super::*;

/// Plan mode must not gate voice: typing `/voice` + Enter through the real
/// input path (prompt keys → slash registry → dispatch) starts recording
/// with `plan_mode_active` set, exactly like normal mode.
#[test]
fn voice_slash_submit_starts_recording_in_plan_mode() {
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    let mut app = test_app_with_agent();
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    app.voice_cmd_tx = Some(tx);
    // Production reveal path: flips the flag AND the per-surface `/voice`
    // registry visibility together.
    app.apply_voice_mode_enabled(true);
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.plan_mode_active = true;
    agent.set_active_pane(crate::views::agent::ActivePane::Prompt, false);

    for ch in "/voice".chars() {
        app.handle_input(&Event::Key(KeyEvent::new(
            KeyCode::Char(ch),
            KeyModifiers::NONE,
        )));
    }
    let out = app.handle_input(&Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    let InputOutcome::Action(action) = out else {
        panic!("Enter on /voice must produce a submit action, got {out:?}");
    };
    dispatch(action, &mut app);
    assert!(
        app.voice_listening(),
        "typed /voice + Enter must start recording in plan mode"
    );
}

#[test]
fn voice_on_welcome_noop_when_startup_gated() {
    // Auth/folder-trust unresolved: voice must not create a session (that would
    // bypass the startup gate) — it stays a silent no-op on welcome.
    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    app.voice_mode_enabled = true;
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    app.voice_cmd_tx = Some(tx);

    dispatch(Action::EnableVoiceMode, &mut app);

    assert!(app.agents.is_empty(), "no session created while gated");
    assert!(matches!(app.active_view, ActiveView::Welcome));
    assert!(!app.voice_listening());
}

#[test]
fn voice_final_appends_to_prompt_with_single_space() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.voice_state = VoiceState::Stopping {
        target: VoiceTarget::Agent(id),
        interim: None,
    };
    let p = &mut app.agents.get_mut(&id).unwrap().prompt;
    p.set_text("hello");
    p.set_cursor(5);
    let redraw = crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::UtteranceFinal {
            text: "world".into(),
        },
    );
    assert!(redraw);
    let p = &app.agents.get(&id).unwrap().prompt;
    assert_eq!(p.text(), "hello world");
    assert_eq!(p.cursor(), "hello world".len());
}

#[test]
fn voice_final_preserves_mid_text_cursor() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(id),
        interim: Some("partial".into()),
    };
    let p = &mut app.agents.get_mut(&id).unwrap().prompt;
    p.set_text("hello world");
    p.set_cursor(5);

    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::UtteranceFinal {
            text: "again".into(),
        },
    );

    let p = &app.agents.get(&id).unwrap().prompt;
    assert_eq!(p.text(), "hello world again");
    assert_eq!(p.cursor(), 5);
    assert!(app.voice_listening());
    assert!(app.voice_interim().is_none());
}

#[test]
fn voice_final_into_empty_prompt_has_no_leading_space() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.voice_state = VoiceState::Stopping {
        target: VoiceTarget::Agent(id),
        interim: None,
    };
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::UtteranceFinal {
            text: "hi there".into(),
        },
    );
    let p = &app.agents.get(&id).unwrap().prompt;
    assert_eq!(p.text(), "hi there");
    assert_eq!(p.cursor(), "hi there".len());
}

#[test]
fn voice_final_replaces_whitespace_only_draft() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.voice_state = VoiceState::Stopping {
        target: VoiceTarget::Agent(id),
        interim: None,
    };
    let p = &mut app.agents.get_mut(&id).unwrap().prompt;
    p.set_text("  \n");
    p.set_cursor(0);
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::UtteranceFinal { text: "hi".into() },
    );
    let p = &app.agents.get(&id).unwrap().prompt;
    assert_eq!(p.text(), "hi");
    assert_eq!(p.cursor(), 2);
}

#[test]
fn voice_final_preserves_trailing_newline() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.voice_state = VoiceState::Stopping {
        target: VoiceTarget::Agent(id),
        interim: None,
    };
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .set_text("line one\n");
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::UtteranceFinal {
            text: "line two".into(),
        },
    );
    // Newline preserved: dictation lands on the new line.
    assert_eq!(
        app.agents.get(&id).unwrap().prompt.text(),
        "line one\nline two"
    );
}

/// A Ctrl+Space release ends only a session an Ctrl+Space hold started. A recording from
/// `/voice` / Ctrl+Space (toggle) is left running — its release isn't ours.
#[test]
fn voice_ctrl_space_release_leaves_toggle_recording_running() {
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    let mut app = test_app_with_agent();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_mode_enabled = true;
    app.voice_cmd_tx = Some(tx);

    // Ctrl+Space starts a toggle recording (no hold-ownership).
    dispatch(Action::VoiceToggle, &mut app);
    assert!(app.voice_listening());
    assert!(!app.voice_state.hold());
    let _ = rx.try_recv(); // drain the PttPress

    // A stray Ctrl+Space release must not stop it.
    dispatch(Action::VoiceStop, &mut app);
    assert!(
        app.voice_listening(),
        "Ctrl+Space release must not stop a toggle session"
    );
    assert!(
        rx.try_recv().is_err(),
        "no PttRelease for a non-hold session"
    );
}

/// A free-tier user hitting the voice keybinding gets the EchoAgent upsell
/// instead of a doomed voice session — the keybinding bypasses the slash
/// registry, so this dispatcher is the enforcement point.
#[test]
fn voice_keybinding_on_restricted_tier_opens_upsell() {
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return; // The tier check runs after the AUDIO_SUPPORTED gate.
    }
    let mut app = test_app_with_agent();
    app.voice_mode_enabled = true;
    // Personal login without a subscription tier ⇒ free tier ⇒ voice restricted.
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta::default());
    assert!(app.is_voice_tier_restricted());

    dispatch(Action::EnableVoiceMode, &mut app);

    assert!(
        app.agents.get(&AgentId(0)).unwrap().question_view.is_some(),
        "restricted-tier voice keybinding must open the EchoAgent upsell"
    );
    assert!(
        !app.voice_listening(),
        "voice must not start on a restricted tier"
    );
}

/// A paid-tier user's voice keybinding is not intercepted by the tier gate.
#[test]
fn voice_keybinding_on_paid_tier_not_gated() {
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    let mut app = test_app_with_agent();
    app.voice_mode_enabled = true;
    let meta = echo_agent_runtime::auth::AuthMeta {
        subscription_tier: Some("EchoAgent".into()),
        ..Default::default()
    };
    app.apply_auth_meta(&meta);
    assert!(!app.is_voice_tier_restricted());

    dispatch(Action::EnableVoiceMode, &mut app);

    // No upsell modal — the paid user proceeds down the normal voice path.
    assert!(
        app.agents.get(&AgentId(0)).unwrap().question_view.is_none(),
        "paid-tier voice must not be intercepted by the tier gate"
    );
}

#[test]
fn voice_interim_sets_then_error_clears_state() {
    let mut app = test_app_with_agent();
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(AgentId(0)),
        interim: None,
    };
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::InterimTranscript {
            text: "partial".into(),
        },
    );
    assert_eq!(app.voice_interim(), Some("partial"));

    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::Error {
            message: "boom".into(),
            hint: None,
        },
    );
    assert!(!app.voice_listening());
    assert!(app.voice_interim().is_none());
}

#[test]
fn voice_error_hint_lands_in_bound_agent_scrollback() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let before = app.agents.get(&id).unwrap().scrollback.len();

    // Hint follows the bound target (like finals), not the active view.
    app.active_view = ActiveView::AgentDashboard;
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(id),
        interim: None,
    };
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::Error {
            message: "no speech detected".into(),
            hint: Some("allow terminal mic access in system settings".into()),
        },
    );
    let agent = app.agents.get(&id).unwrap();
    assert_eq!(agent.scrollback.len(), before + 1);
    let text = match agent
        .scrollback
        .get(agent.scrollback.len() - 1)
        .map(|e| &e.block)
    {
        Some(crate::scrollback::block::RenderBlock::System(b)) => b.text.as_str(),
        other => panic!("expected system hint block, got {other:?}"),
    };
    assert!(
        text.contains("no speech detected")
            && text.contains("allow terminal mic access in system settings"),
        "scrollback should carry short message + long hint, got {text:?}"
    );

    // No hint while still bound → toast only, no scrollback growth.
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(id),
        interim: None,
    };
    let before = app.agents.get(&id).unwrap().scrollback.len();
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::Error {
            message: "boom".into(),
            hint: None,
        },
    );
    assert_eq!(app.agents.get(&id).unwrap().scrollback.len(), before);
}

#[test]
fn voice_error_hint_dropped_for_dashboard_dispatch() {
    // The dispatch box has no scrollback; only the dashboard toast survives.
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::AgentDashboard;
    ensure_dashboard_state(&mut app);
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::DashboardDispatch,
        interim: None,
    };
    let before = app.agents.get(&AgentId(0)).unwrap().scrollback.len();
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::Error {
            message: "no speech detected".into(),
            hint: Some("allow terminal mic access in system settings".into()),
        },
    );
    assert_eq!(
        app.agents.get(&AgentId(0)).unwrap().scrollback.len(),
        before
    );
    assert!(
        app.dashboard
            .as_ref()
            .is_some_and(|d| d.error_toast.is_some())
    );
}

#[test]
fn voice_interim_ignored_after_stop() {
    // Late interim events that arrive after recording stopped must not
    // repopulate the overlay ("Interim shown after stop").
    let mut app = test_app_with_agent();
    app.voice_state = VoiceState::Idle; // not recording → interim is None
    let redraw = crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::InterimTranscript {
            text: "late".into(),
        },
    );
    assert!(!redraw, "stale interim event must not request a redraw");
    assert!(
        app.voice_interim().is_none(),
        "stale interim event must not set voice_interim"
    );
}

#[test]
fn voice_interim_kept_on_stop_then_cleared_by_final() {
    // An explicit stop keeps the last interim on screen (no flicker) until
    // the trailing final commits it; the final then clears the interim.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(id),
        interim: Some("partial".into()),
    };

    app.voice_stop_keeping_final();
    assert!(!app.voice_listening());
    assert_eq!(
        app.voice_interim(),
        Some("partial"),
        "stop keeps the interim"
    );

    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::UtteranceFinal {
            text: "partial".into(),
        },
    );
    assert_eq!(app.voice_interim(), None, "final clears the interim");
    assert_eq!(app.agents.get(&id).unwrap().prompt.text(), "partial");
}

#[test]
fn voice_toggle_starts_and_stops() {
    // Starting routes through the `/voice` gate, which requires compiled-in
    // audio capture; skip on builds without a `cpal` backend.
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    let mut app = test_app_with_agent();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_mode_enabled = true;
    app.voice_ui_active = true;
    app.voice_cmd_tx = Some(tx);

    dispatch(Action::VoiceToggle, &mut app);
    assert!(app.voice_listening());
    assert!(matches!(
        rx.try_recv(),
        Ok(echo_agent_voice::VoiceCommand::PttPress)
    ));

    dispatch(Action::VoiceToggle, &mut app);
    assert!(!app.voice_listening());
    assert!(matches!(
        rx.try_recv(),
        Ok(echo_agent_voice::VoiceCommand::PttRelease)
    ));
}

#[test]
fn voice_toggle_silent_no_op_when_flag_disabled() {
    // With the voice gate off (kill switch / env force-off), the voice key is
    // a silent no-op — no recording, and no toast.
    let mut app = test_app_with_agent();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_mode_enabled = false;
    app.voice_cmd_tx = Some(tx);
    dispatch(Action::VoiceToggle, &mut app);
    assert!(
        !app.voice_listening(),
        "must not start recording when the flag is off"
    );
    assert!(
        !app.voice_ui_active,
        "voice mode must not arm with flag off"
    );
    assert!(rx.try_recv().is_err(), "no PttPress with flag off");
}

#[test]
fn voice_toggle_starts_without_voice_mode_prereq() {
    // Ctrl+Space is a direct start — it no longer requires `/voice` first.
    // Skip when audio capture isn't compiled in (see sibling test).
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    let mut app = test_app_with_agent();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_mode_enabled = true;
    app.voice_ui_active = false;
    app.voice_cmd_tx = Some(tx);
    dispatch(Action::VoiceToggle, &mut app);
    assert!(app.voice_ui_active, "Ctrl+Space enables voice mode");
    assert!(
        app.voice_listening(),
        "Ctrl+Space starts recording without a /voice prerequisite"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(echo_agent_voice::VoiceCommand::PttPress)
    ));
}

#[test]
fn voice_mode_enable_starts_recording_and_stays_on() {
    // `/voice` gates on compiled-in audio capture; skip when the build has
    // no `cpal` backend (e.g. Bazel/headless), where enabling is a no-op.
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    let mut app = test_app_with_agent();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_mode_enabled = true;
    app.voice_cmd_tx = Some(tx);

    // `EnableVoiceMode` (the Ctrl+Space hold-press start) begins recording when the
    // pipeline is already up.
    dispatch(Action::EnableVoiceMode, &mut app);
    assert!(app.voice_ui_active);
    assert!(app.voice_listening(), "start begins recording");
    assert!(
        !app.voice_state.pending_cold_start(),
        "pipeline already up — no re-request"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(echo_agent_voice::VoiceCommand::PttPress)
    ));

    // `EnableVoiceMode` is start-only (not a toggle): running it again while
    // already recording is idempotent — no stop, no second PttPress.
    dispatch(Action::EnableVoiceMode, &mut app);
    assert!(app.voice_ui_active, "start never turns voice mode off");
    assert!(app.voice_listening());
    assert!(
        rx.try_recv().is_err(),
        "no second PttPress while already recording"
    );
}

#[test]
fn voice_mode_on_requests_lazy_pipeline_when_missing() {
    // Skip when audio capture isn't compiled in (see sibling test).
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    let mut app = test_app_with_agent();
    app.voice_mode_enabled = true;
    // No voice_cmd_tx — first /voice should ask the event loop to spawn.
    dispatch(Action::EnableVoiceMode, &mut app);
    assert!(app.voice_ui_active);
    assert!(
        app.voice_state.pending_cold_start(),
        "event loop should spawn the pipeline and auto-start capture"
    );
}

#[test]
fn voice_toggle_while_spawn_pending_keeps_start_armed() {
    // A second Ctrl+Space while the pipeline is still spawning re-affirms
    // the queued start rather than cancelling it (there's no visible recording
    // yet to toggle off).
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    let mut app = test_app_with_agent();
    app.voice_mode_enabled = true;
    app.voice_state = VoiceState::ColdStart {
        hold: false,
        target: VoiceTarget::Agent(AgentId(0)),
    };

    dispatch(Action::VoiceToggle, &mut app);
    assert!(
        app.voice_state.pending_cold_start(),
        "Ctrl+Space re-affirms the queued auto-start (does not cancel it)"
    );
    assert!(!app.voice_listening());
}

#[test]
fn voice_toggle_preserves_pending_ctrl_space_hold_cancel() {
    // A Ctrl+Space quick-tap queues a hold-owned cold-start; a Ctrl+Space toggle
    // arriving before the pipeline spawns must re-affirm it without clearing
    // hold-ownership, so the matching Ctrl+Space release still cancels the tap.
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    let mut app = test_app_with_agent();
    app.voice_mode_enabled = true;
    app.voice_state = VoiceState::ColdStart {
        hold: true,
        target: VoiceTarget::Agent(AgentId(0)),
    };

    dispatch(Action::VoiceToggle, &mut app);
    assert!(
        app.voice_state.hold(),
        "toggle must not clear the Ctrl+Space hold-ownership"
    );

    dispatch(Action::VoiceStop, &mut app);
    assert!(
        !app.voice_state.pending_cold_start(),
        "the matching Ctrl+Space release still cancels the queued tap"
    );
}

#[test]
fn voice_toggle_can_always_stop_even_with_flag_disabled() {
    let mut app = test_app_with_agent();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    // Recording was started while the flag was on, then it flipped off.
    app.voice_cmd_tx = Some(tx);
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(AgentId(0)),
        interim: None,
    };
    app.voice_mode_enabled = false;
    app.voice_ui_active = false;

    dispatch(Action::VoiceToggle, &mut app);
    assert!(
        !app.voice_listening(),
        "must stop an active recording even with the flag off"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(echo_agent_voice::VoiceCommand::PttRelease)
    ));
}

#[test]
fn voice_stop_stops_and_drops_pending_cold_start() {
    // The Ctrl+Space hold release stops capture and cancels a queued cold-start, so
    // a release that arrives while the pipeline is still spawning can't leave
    // a hot mic running after the key is up.
    let mut app = test_app_with_agent();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_cmd_tx = Some(tx);
    // A live recording started by an Ctrl+Space hold-press.
    app.voice_state = VoiceState::Recording {
        hold: true,
        target: VoiceTarget::Agent(AgentId(0)),
        interim: None,
    };

    dispatch(Action::VoiceStop, &mut app);
    assert!(!app.voice_listening());
    assert!(!app.voice_state.pending_cold_start());
    assert!(!app.voice_state.hold());
    assert!(matches!(
        rx.try_recv(),
        Ok(echo_agent_voice::VoiceCommand::PttRelease)
    ));
}

/// A stray Ctrl+Space release must NOT cancel a cold-start queued by `/voice` /
/// Ctrl+Space (`hold` is false for those).
#[test]
fn voice_stop_leaves_non_hold_cold_start_armed() {
    let mut app = test_app_with_agent();
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    app.voice_cmd_tx = Some(tx);
    // Queued by /voice, not Ctrl+Space (hold inactive).
    app.voice_state = VoiceState::ColdStart {
        hold: false,
        target: VoiceTarget::Agent(AgentId(0)),
    };

    dispatch(Action::VoiceStop, &mut app);
    assert!(
        app.voice_state.pending_cold_start(),
        "a /voice cold-start must survive an unrelated Ctrl+Space release"
    );
}

/// Changing the STT language shuts down a running pipeline (it holds the
/// VoiceConfig it was spawned with) so the next capture cold-starts one
/// with the new language, and persists the preference. A mid-recording change
/// also ends the in-flight session so the mic indicator clears at once rather
/// than lingering until the dead pipeline's channel-close is misreported.
#[test]
fn voice_stt_language_change_recycles_pipeline() {
    let mut app = test_app_with_agent();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_cmd_tx = Some(tx);
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(AgentId(0)),
        interim: Some("hola".to_string()),
    };

    let effects = dispatch(Action::SetVoiceSttLanguage("es".to_string()), &mut app);

    assert_eq!(app.voice_config.language, "es");
    assert_eq!(app.current_ui.voice_stt_language.as_deref(), Some("es"));
    assert!(
        app.voice_cmd_tx.is_none(),
        "handle must drop so the event loop respawns with the new config"
    );
    assert!(
        !app.voice_listening(),
        "recycling the pipeline must end the in-flight session (no lingering hot mic)"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(echo_agent_voice::VoiceCommand::Shutdown)
    ));
    assert!(matches!(
        effects.as_slice(),
        [Effect::PersistSetting {
            key: "voice_stt_language",
            ..
        }]
    ));
}

/// With the UI key unset and a `[voice].language` in effect, selecting the
/// UI default (English) must still commit — and roll back to the language
/// actually in effect, not the unset-UI default.
#[test]
fn voice_stt_language_english_commits_over_voice_config_language() {
    let mut app = test_app_with_agent();
    app.voice_config.language = "es".into(); // [voice].language; UI key unset

    let effects = dispatch(Action::SetVoiceSttLanguage("en".to_string()), &mut app);

    assert_eq!(app.voice_config.language, "en");
    assert_eq!(app.current_ui.voice_stt_language.as_deref(), Some("en"));
    assert!(matches!(
        effects.as_slice(),
        [Effect::PersistSetting {
            key: "voice_stt_language",
            value: crate::settings::SettingValue::Enum("en"),
            rollback_value: crate::settings::SettingValue::Enum("es"),
        }]
    ));
}

/// Re-selecting the persisted language is a no-op: no persist, and the
/// running pipeline is left alone. An unset UI key never no-ops — the first
/// explicit selection always commits (pins the choice to `[ui]`).
#[test]
fn voice_stt_language_noop_keeps_pipeline() {
    let mut app = test_app_with_agent();
    app.current_ui.voice_stt_language = Some("es".to_string());
    app.voice_config.language = "es".into();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_cmd_tx = Some(tx);

    let effects = dispatch(Action::SetVoiceSttLanguage("es".to_string()), &mut app);

    assert!(effects.is_empty());
    assert!(app.voice_cmd_tx.is_some(), "pipeline must survive a no-op");
    assert!(rx.try_recv().is_err());

    // Unset UI key + matching live value still commits (pins the choice) — but
    // the language is unchanged, so the pipeline must NOT be recycled.
    app.current_ui.voice_stt_language = None;
    let effects = dispatch(Action::SetVoiceSttLanguage("es".to_string()), &mut app);
    assert!(!effects.is_empty(), "explicit pick must persist when unset");
    assert_eq!(app.current_ui.voice_stt_language.as_deref(), Some("es"));
    assert!(
        app.voice_cmd_tx.is_some(),
        "re-pinning the same language must not cut off dictation"
    );
    assert!(
        rx.try_recv().is_err(),
        "no Shutdown when language is unchanged"
    );

    // A non-canonical stored value (hand-edited/invalid on disk) re-commits
    // so the clean canonical is rewritten — still no language change, so the
    // pipeline survives.
    app.current_ui.voice_stt_language = Some("ES!".to_string());
    let effects = dispatch(Action::SetVoiceSttLanguage("es".to_string()), &mut app);
    assert!(
        !effects.is_empty(),
        "invalid stored value must be rewritten"
    );
    assert_eq!(app.current_ui.voice_stt_language.as_deref(), Some("es"));
    assert!(
        app.voice_cmd_tx.is_some(),
        "rewriting a non-canonical mirror must not recycle the pipeline"
    );
    assert!(
        rx.try_recv().is_err(),
        "no Shutdown when language is unchanged"
    );
}

/// `auto` is stored as the preference (not a resolved locale code) so the
/// voice crate re-resolves it from the locale on each STT connect.
#[test]
fn voice_stt_language_auto_stored_unresolved() {
    let mut app = test_app_with_agent();

    let _ = dispatch(Action::SetVoiceSttLanguage("auto".to_string()), &mut app);

    assert_eq!(app.voice_config.language, "auto");
    assert_eq!(app.current_ui.voice_stt_language.as_deref(), Some("auto"));
}

#[test]
fn voice_submit_includes_interim() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_cmd_tx = Some(tx);
    app.agents.get_mut(&id).unwrap().prompt.set_text("hello");
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(id),
        interim: Some("world".into()),
    };

    let effects = dispatch(Action::SendPrompt("hello".into()), &mut app);
    let Effect::SendPrompt { text, .. } = &effects[0] else {
        panic!("expected SendPrompt, got {effects:?}");
    };
    assert_eq!(text, "hello world");
    assert!(!app.voice_listening());
    assert!(matches!(
        rx.try_recv(),
        Ok(echo_agent_voice::VoiceCommand::PttRelease)
    ));
}

#[test]
fn voice_submit_interim_only() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(id),
        interim: Some("ghost only".into()),
    };

    let effects = dispatch(Action::SendPrompt(String::new()), &mut app);
    let Effect::SendPrompt { text, .. } = &effects[0] else {
        panic!("expected SendPrompt, got {effects:?}");
    };
    assert_eq!(text, "ghost only");
    assert!(!app.voice_listening());
}

#[test]
fn voice_submit_follow_up_keeps_chip_literal() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().prompt.set_text("draft");
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::Agent(id),
        interim: Some("dictated".into()),
    };

    let effects = dispatch(Action::SubmitFollowUp("chip text".into()), &mut app);
    let Effect::SendPrompt { text, .. } = &effects[0] else {
        panic!("expected SendPrompt, got {effects:?}");
    };
    assert_eq!(text, "chip text");
    assert_eq!(app.agents.get(&id).unwrap().prompt.text(), "draft dictated");
    assert!(!app.voice_listening());
}
