//! Tests for turn cancellation, subagent kills, and cancel preferences.

use super::*;

#[test]
fn demote_dispatch_keeps_turn_session_and_execute_guards() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    assert!(dispatch(Action::DemoteToBackground, &mut app).is_empty());

    crate::app::agent_view::test_fixtures::add_running_execute(app.agents.get_mut(&id).unwrap());
    let effects = dispatch(Action::DemoteToBackground, &mut app);
    assert!(matches!(
        effects.as_slice(),
        [Effect::DemoteToBackground {
            session_id,
            tool_call_id,
        }] if session_id.0.as_ref() == "test-session" && tool_call_id == "exec-1"
    ));

    app.agents.get_mut(&id).unwrap().session.state = AgentState::Idle;
    assert!(dispatch(Action::DemoteToBackground, &mut app).is_empty());
}

/// Regression (leader mode): a queued prompt's parked `session/prompt` RPC
/// can resolve as an *error* — e.g. its `respond_to` is dropped on the
/// leader when the prompt is removed from the shared queue, surfacing as
/// `Internal error: "session failed to respond"`. An `acp::Error` carries
/// no `promptId`, so before the Err-arm gate this error was misattributed
/// to the running turn and rendered as a spurious "Turn failed", detonating
/// an unrelated in-flight turn. The handler now gates the Err arm on the
/// `prompt_id` the pager minted for that RPC: an error whose id is NOT the
/// running turn is discarded; the running turn is left untouched.
#[test]
fn queued_prompt_rpc_error_does_not_kill_running_turn() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    // First prompt drains immediately → Running. Capture its prompt_id.
    let effects = dispatch(Action::SendPrompt("running".into()), &mut app);
    let running_pid = match &effects[0] {
        Effect::SendPrompt { prompt_id, .. } => prompt_id.clone(),
        other => panic!("expected SendPrompt, got {other:?}"),
    };
    assert!(app.agents[&id].session.state.is_turn_running());
    assert_eq!(
        app.agents[&id].session.current_prompt_id.as_deref(),
        Some(running_pid.as_str())
    );

    // Second prompt typed while running → immediate server-authoritative
    // send (queued at the leader). Capture its prompt_id.
    let effects = dispatch(Action::SendPrompt("queued".into()), &mut app);
    let queued_pid = match &effects[0] {
        Effect::SendPrompt { prompt_id, .. } => prompt_id.clone(),
        other => panic!("expected immediate SendPrompt, got {other:?}"),
    };
    assert_ne!(running_pid, queued_pid);

    let scrollback_before = app.agents[&id].scrollback.len();

    // The queued prompt is removed; its parked RPC resolves Err.
    let effects = dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Err("Internal error: session failed to respond".to_string()),
            http_status: None,
            prompt_id: Some(queued_pid.clone()),
        }),
        &mut app,
    );

    // Discarded: no effects, running turn untouched, no "Turn failed" block.
    assert!(
        effects.is_empty(),
        "a queued prompt's RPC error must be discarded, got {effects:?}"
    );
    assert!(
        app.agents[&id].session.state.is_turn_running(),
        "the running turn must survive a queued prompt's RPC error"
    );
    assert_eq!(
        app.agents[&id].session.current_prompt_id.as_deref(),
        Some(running_pid.as_str()),
        "current_prompt_id must still point at the running turn"
    );
    assert_eq!(
        app.agents[&id].scrollback.len(),
        scrollback_before,
        "no TurnFailed block may be pushed for a non-running prompt's error"
    );

    // Sanity: an error for the ACTUAL running prompt is NOT discarded — it
    // ends the turn and renders the failure.
    let _ = dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Err("upstream boom".to_string()),
            http_status: None,
            prompt_id: Some(running_pid.clone()),
        }),
        &mut app,
    );
    assert!(
        !app.agents[&id].session.state.is_turn_running(),
        "the running turn's own error must end the turn"
    );
    assert!(
        app.agents[&id].scrollback.len() > scrollback_before,
        "the running turn's own error must render a failure block"
    );
}

#[test]
fn cta_install_done_skills_only_settles_installed_without_fetch() {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::Installing {
            plugin_relative_path: "plugins/figma".into(),
            name: "figma".into(),
        };
        cta.expects_mcp = false;
    }
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginInstallDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(cta_outcome(OutcomeStatus::Success, "installed")),
        }),
        &mut app,
    );
    // No MCP fetch, no "Setting up…" flash: straight to Installed.
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Installed {
            name: "figma".into()
        }
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::FetchPluginCtaMcps { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::DismissCtaInstalled { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchPluginCtaCatalog { .. }))
    );
}

#[test]
fn cta_reload_done_skills_only_settles_installed_without_fetch() {
    use crate::app::agent_view::CtaPhase;
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let cta = &mut app.agents.get_mut(&id).unwrap().plugin_cta;
        cta.phase = CtaPhase::AwaitingReload {
            name: "figma".into(),
        };
        cta.expects_mcp = false;
    }
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CtaPluginReloadDone {
            agent_id: id,
            plugin_name: "figma".into(),
            result: Ok(cta_outcome(OutcomeStatus::Success, "reloaded")),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].plugin_cta.phase,
        CtaPhase::Installed {
            name: "figma".into()
        }
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::FetchPluginCtaMcps { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::DismissCtaInstalled { .. }))
    );
}

#[test]
fn cancel_turn_without_subagents_cancels_immediately() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::CancelTurn {
            cancel_subagents: true,
            ..
        }
    ));
    assert!(app.agents[&id].session.state.is_cancelling());
}

/// Cancel inside a subagent drill-in view kills the focused running subagent
/// instead of resolving the root turn. The root is idle here, so only the kill
/// path reaches the coordinator-run child.
#[test]
fn cancel_turn_in_subagent_view_kills_focused_subagent() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::Idle;
        agent
            .subagent_sessions
            .insert("child-1".to_string(), make_test_subagent("child-1", "sa-1"));
        agent.active_subagent = Some("child-1".into());
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(
        matches!(
            effects.as_slice(),
            [Effect::KillSubagent { subagent_id, .. }] if subagent_id == "sa-1"
        ),
        "stop in a subagent view must kill the focused subagent, got {effects:?}"
    );
    assert!(app.agents[&id].subagent_sessions["child-1"].pending_kill);
}

/// The kill routing keys off the focused running subagent, not root idleness:
/// with the root turn running, cancel still kills the child and leaves the root
/// turn running (never cancelling).
#[test]
fn cancel_turn_in_subagent_view_kills_child_even_with_running_root() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent
            .subagent_sessions
            .insert("child-1".to_string(), make_test_subagent("child-1", "sa-1"));
        agent.active_subagent = Some("child-1".into());
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(
        matches!(
            effects.as_slice(),
            [Effect::KillSubagent { subagent_id, .. }] if subagent_id == "sa-1"
        ),
        "a running focused subagent must be killed even while the root turn runs, got {effects:?}"
    );
    assert!(
        app.agents[&id].session.state.is_turn_running(),
        "the root turn must keep running"
    );
    assert!(!app.agents[&id].session.state.is_cancelling());
}

/// A finished focused subagent must NOT swallow the cancel into a kill: the
/// stop falls through to normal root-turn cancellation.
#[test]
fn cancel_turn_in_finished_subagent_view_falls_through_to_root() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        let mut info = make_test_subagent("child-1", "sa-1");
        info.finished = true;
        agent.subagent_sessions.insert("child-1".to_string(), info);
        agent.active_subagent = Some("child-1".into());
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(
        matches!(effects.as_slice(), [Effect::CancelTurn { .. }]),
        "a finished subagent must not intercept cancel, got {effects:?}"
    );
}

#[test]
fn cancel_turn_forwards_trigger_hint_to_effect() {
    // The key/mouse producer sets `cancel_trigger_hint` (here ESC) before
    // dispatching CancelTurn; `do_cancel_turn` must forward it onto
    // `Effect::CancelTurn.trigger` (→ `_meta.cancelTrigger`) and consume it.
    // This is the same plumbing the Ctrl+C end-to-end test exercises; only
    // the `CancelTrigger` value differs across producers (esc/ctrl_c/mouse).
    use crate::app::actions::CancelTrigger;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.cancel_trigger_hint = Some(CancelTrigger::Esc);
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(matches!(
        &effects[0],
        Effect::CancelTurn {
            trigger: Some(CancelTrigger::Esc),
            ..
        }
    ));
    // One-shot: consumed when the cancel is built.
    assert_eq!(app.agents[&id].cancel_trigger_hint, None);
}

#[test]
fn cancel_turn_without_trigger_hint_sends_none() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(matches!(
        &effects[0],
        Effect::CancelTurn { trigger: None, .. }
    ));
}

#[test]
fn lost_cancel_is_resent_while_still_cancelling() {
    use crate::app::actions::CancelTrigger;
    use crate::app::dispatch::CANCEL_RESEND_GRACE;
    use crate::app::dispatch::reconcile_overdue_cancels;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.cancel_trigger_hint = Some(CancelTrigger::Mouse);
    }
    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(matches!(effects.as_slice(), [Effect::CancelTurn { .. }]));
    assert!(app.agents[&id].session.state.is_cancelling());

    // Inside the grace: nothing fires.
    assert!(reconcile_overdue_cancels(&mut app).is_none());

    // The cancel is lost in transit (no response ever arrives); age it out.
    app.agents
        .get_mut(&id)
        .unwrap()
        .pending_cancel_resend
        .as_mut()
        .unwrap()
        .sent_at = std::time::Instant::now() - CANCEL_RESEND_GRACE;
    let resent = reconcile_overdue_cancels(&mut app).expect("overdue cancel must re-send");
    assert!(
        matches!(
            resent.as_slice(),
            [Effect::CancelTurn {
                trigger: Some(CancelTrigger::Mouse),
                rewind_prompt_id: None,
                ..
            }]
        ),
        "the resend replays the gesture trigger, got {resent:?}"
    );
    assert_eq!(
        app.agents[&id]
            .pending_cancel_resend
            .as_ref()
            .unwrap()
            .attempts,
        2
    );

    // A received `prompt_complete` broadcast proves the cancel landed: the
    // resend stops even though the pane is still cancelling, so it can
    // never race the turn-end reconcile and cancel a promoted queued prompt.
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.pending_cancel_resend.as_mut().unwrap().sent_at =
            std::time::Instant::now() - CANCEL_RESEND_GRACE;
        agent.pending_turn_end_reconcile = Some(crate::app::agent_view::PendingTurnEnd {
            prompt_id: "p1".into(),
            stop_reason: Some("cancelled".into()),
            agent_result: None,
            cancel_trigger: None,
            cancellation_category: None,
            received_at: std::time::Instant::now(),
        });
    }
    assert!(reconcile_overdue_cancels(&mut app).is_none());
    // The record survives, confirmed: the auto-resend is dead, but a manual
    // retry can still read the recorded subagent choice.
    assert!(
        app.agents[&id]
            .pending_cancel_resend
            .as_ref()
            .unwrap()
            .confirmed
    );
    app.agents.get_mut(&id).unwrap().pending_turn_end_reconcile = None;
    assert!(
        reconcile_overdue_cancels(&mut app).is_none(),
        "a confirmed record keeps the auto-resend off after the window closes"
    );

    // Turn resolved: the marker clears and nothing more fires.
    app.agents.get_mut(&id).unwrap().session.state = AgentState::Idle;
    assert!(reconcile_overdue_cancels(&mut app).is_none());
    assert!(app.agents[&id].pending_cancel_resend.is_none());
}

#[test]
fn cancel_retry_reuses_recorded_subagent_choice() {
    use crate::app::actions::CancelTrigger;
    use crate::app::dispatch::reconcile_overdue_cancels;
    use crate::views::modal::CancelTurnChoice;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.cancel_trigger_hint = Some(CancelTrigger::CtrlC);
    }

    let effects = dispatch(
        Action::CancelTurnChoice(CancelTurnChoice::ContinueToRun),
        &mut app,
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::CancelTurn {
            cancel_subagents: false,
            ..
        }]
    ));

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                cancel_subagents: false,
                ..
            }]
        ),
        "the retry must not escalate past the one-shot choice, got {effects:?}"
    );

    // The turn-end broadcast stands the auto-resend down; a retry after it
    // must still reuse the recorded choice instead of escalating.
    app.agents.get_mut(&id).unwrap().pending_turn_end_reconcile =
        Some(crate::app::agent_view::PendingTurnEnd {
            prompt_id: "p1".into(),
            stop_reason: Some("cancelled".into()),
            agent_result: None,
            cancel_trigger: None,
            cancellation_category: None,
            received_at: std::time::Instant::now(),
        });
    assert!(reconcile_overdue_cancels(&mut app).is_none());
    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                cancel_subagents: false,
                ..
            }]
        ),
        "a confirmed cancel must not discard the recorded choice, got {effects:?}"
    );
}

#[test]
fn confirmed_stop_retry_does_not_rearm_auto_resend() {
    use crate::app::actions::CancelTrigger;
    use crate::app::dispatch::CANCEL_RESEND_GRACE;
    use crate::app::dispatch::reconcile_overdue_cancels;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.cancel_trigger_hint = Some(CancelTrigger::Mouse);
    }
    assert!(matches!(
        dispatch(Action::CancelTurn, &mut app).as_slice(),
        [Effect::CancelTurn {
            trigger: Some(CancelTrigger::Mouse),
            ..
        }]
    ));

    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.pending_turn_end_reconcile = Some(crate::app::agent_view::PendingTurnEnd {
            prompt_id: "p1".into(),
            stop_reason: Some("cancelled".into()),
            agent_result: None,
            cancel_trigger: None,
            cancellation_category: None,
            received_at: std::time::Instant::now(),
        });
    }
    assert!(reconcile_overdue_cancels(&mut app).is_none());
    assert!(
        app.agents[&id]
            .pending_cancel_resend
            .as_ref()
            .is_some_and(|p| p.confirmed)
    );

    // Gesture retry (hint set, as `[stop]` / Esc do).
    app.agents.get_mut(&id).unwrap().cancel_trigger_hint = Some(CancelTrigger::Mouse);
    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                trigger: Some(CancelTrigger::Mouse),
                ..
            }]
        ),
        "a manual retry still re-sends, got {effects:?}"
    );
    let pending = app.agents[&id]
        .pending_cancel_resend
        .as_ref()
        .expect("resend record must survive");
    assert!(
        pending.confirmed,
        "a confirmed record must stay confirmed across a gesture retry"
    );

    app.agents
        .get_mut(&id)
        .unwrap()
        .pending_cancel_resend
        .as_mut()
        .unwrap()
        .sent_at = std::time::Instant::now() - CANCEL_RESEND_GRACE;
    assert!(
        reconcile_overdue_cancels(&mut app).is_none(),
        "auto-resend must stay off after a confirmed gesture retry"
    );
}

#[test]
fn hintless_retry_replays_recorded_trigger() {
    use crate::app::actions::CancelTrigger;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.cancel_trigger_hint = Some(CancelTrigger::Esc);
    }
    assert!(matches!(
        dispatch(Action::CancelTurn, &mut app).as_slice(),
        [Effect::CancelTurn {
            trigger: Some(CancelTrigger::Esc),
            ..
        }]
    ));

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                trigger: Some(CancelTrigger::Esc),
                ..
            }]
        ),
        "a hint-less retry must replay the recorded trigger, got {effects:?}"
    );
}

#[test]
fn cancel_turn_stops_compact_even_with_stale_wake_marker() {
    use crate::app::actions::CancelTrigger;
    use crate::app::agent::AgentCommand;
    use crate::app::agent_view::RunningWakeTurn;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.start_command(AgentCommand::Compact);
        agent.running_wake_turn = Some(RunningWakeTurn {
            prompt_id: "task-completed-bg1".into(),
            cancel_sent: false,
        });
        agent.cancel_trigger_hint = Some(CancelTrigger::Esc);
    }

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(effects.as_slice(), [Effect::CancelTurn { .. }]),
        "compact cancel must emit, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(
        matches!(
            agent.session.state,
            AgentState::CommandCancelling {
                command: AgentCommand::Compact,
            }
        ),
        "Esc during /compact must cancel compact, not only the stale wake, got {:?}",
        agent.session.state
    );
}

#[test]
fn cancel_after_local_send_during_wake_does_not_arm_resend() {
    use crate::app::actions::CancelTrigger;
    use crate::app::agent_view::RunningWakeTurn;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.running_wake_turn = Some(RunningWakeTurn {
            prompt_id: "task-completed-bg1".into(),
            cancel_sent: false,
        });
        agent.start_turn_boundary(Some("user-1"));
        agent.session.current_prompt_id = Some("user-1".into());
        agent.cancel_trigger_hint = Some(CancelTrigger::Esc);
    }

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                trigger: Some(CancelTrigger::Esc),
                rewind_prompt_id: None,
                ..
            }]
        ),
        "must still cancel the shell-front wake, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(
        agent.session.state.is_turn_running(),
        "the local user turn is queued on the shell, not cancelled"
    );
    assert!(
        agent.pending_cancel_resend.is_none(),
        "auto-resend would cancel the promoted user turn"
    );
    assert!(
        agent
            .running_wake_turn
            .as_ref()
            .is_some_and(|w| w.cancel_sent),
        "the wake marker must record the cancel"
    );
}

#[test]
fn stale_cancel_resend_clears_once_pane_is_idle() {
    use crate::app::actions::CancelTrigger;
    use crate::app::dispatch::reconcile_overdue_cancels;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::Idle;
        agent.pending_cancel_resend = Some(crate::app::agent_view::PendingCancelResend {
            prompt_id: None,
            sent_at: std::time::Instant::now(),
            attempts: 3,
            confirmed: true,
            cancel_subagents: false,
            trigger: CancelTrigger::Esc,
        });
    }
    assert!(reconcile_overdue_cancels(&mut app).is_none());
    assert!(
        app.agents[&id].pending_cancel_resend.is_none(),
        "reconcile must drop a stale record once nothing is cancelling"
    );
}

#[test]
fn do_cancel_turn_cancels_running_wake_turn() {
    use crate::app::agent_view::RunningWakeTurn;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.running_wake_turn = Some(RunningWakeTurn {
            prompt_id: "task-completed-bg1".into(),
            cancel_sent: false,
        });
    }

    let effects = super::super::turn::do_cancel_turn(
        &mut app,
        true,
        crate::app::cancel_latency::CancelOrigin::UserGesture,
    );
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                cancel_subagents: true,
                rewind_prompt_id: None,
                trigger: None,
                ..
            }]
        ),
        "programmatic cancel must stop a wake turn, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(agent.session.state.is_idle());
    assert!(agent.wake_turn_cancelling());
}

#[test]
fn stop_click_cancels_running_wake_turn() {
    use crate::app::actions::CancelTrigger;
    use crate::app::agent_view::RunningWakeTurn;
    use crate::app::dispatch::CANCEL_RESEND_GRACE;
    use crate::app::dispatch::reconcile_overdue_cancels;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.running_wake_turn = Some(RunningWakeTurn {
            prompt_id: "task-completed-bg1".into(),
            cancel_sent: false,
        });
        assert!(
            matches!(agent.wake_display_state(), Some(AgentState::TurnRunning)),
            "a streaming wake turn must offer the running chrome (and [stop])"
        );
        // The mouse handler sets the hint before dispatching CancelTurn.
        agent.cancel_trigger_hint = Some(CancelTrigger::Mouse);
    }

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                trigger: Some(CancelTrigger::Mouse),
                rewind_prompt_id: None,
                ..
            }]
        ),
        "the wake cancel must ride the normal cancel wire, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(
        agent.session.state.is_idle(),
        "a wake cancel must not fabricate a local turn"
    );
    assert!(matches!(
        agent.wake_display_state(),
        Some(AgentState::TurnCancelling)
    ));

    // The fire-and-forget cancel is loss-prone: the resend reconcile must
    // stay armed even though the pane never left Idle.
    app.agents
        .get_mut(&id)
        .unwrap()
        .pending_cancel_resend
        .as_mut()
        .unwrap()
        .sent_at = std::time::Instant::now() - CANCEL_RESEND_GRACE;
    assert!(reconcile_overdue_cancels(&mut app).is_some());
}

#[test]
fn cancel_turn_leaves_shared_queue_for_agent_to_drain() {
    use crate::app::prompt_queue::QueueEntryWire;
    // Prompts typed while a turn runs live on the server-authoritative
    // shared queue (broadcast to all attached clients). The agent owns the
    // drain: on cancel the FRONT queued prompt runs next (promoted
    // server-side), so the pager must NOT pull it back into the input or
    // mutate the queue locally — the `echo.agent/queue/changed` rebroadcast is the
    // source of truth.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.shared_queue = vec![
            QueueEntryWire {
                id: "q1".into(),
                version: 3,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "first queued".into(),
                position: 0,
                combined_texts: None,
            },
            QueueEntryWire {
                id: "q2".into(),
                version: 4,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "second queued".into(),
                position: 1,
                combined_texts: None,
            },
        ];
        assert!(agent.prompt.text().is_empty());
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    // The input box is left untouched — the front queued prompt is NOT
    // pulled back into it (it runs next on the agent instead).
    assert!(
        app.agents[&id].prompt.text().is_empty(),
        "cancel must not restore a queued prompt into the input"
    );
    // The local mirror is left intact; the agent's rebroadcast drives the
    // queue, so the pager must not predict the post-cancel order.
    let q = &app.agents[&id].shared_queue;
    assert_eq!(
        q.len(),
        2,
        "cancel must not mutate the shared queue locally"
    );
    assert_eq!(q[0].id, "q1");
    assert_eq!(q[1].id, "q2");
    // A plain CancelTurn is emitted (no queued-prompt id threaded, no
    // separate QueueRemove) — the agent tears down the running turn and
    // promotes q1 as the next turn.
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CancelTurn { .. })),
        "must emit CancelTurn, got {effects:?}"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::QueueRemove { .. })),
        "must NOT emit a separate QueueRemove on cancel"
    );
}

#[test]
fn cancel_turn_with_running_subagents_shows_panel() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    app.agents
        .get_mut(&id)
        .unwrap()
        .subagent_sessions
        .insert("child-1".into(), make_test_subagent("child-1", "sa-1"));

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(effects.is_empty());
    assert!(app.agents[&id].cancel_turn_view.is_some());
    assert_eq!(
        app.agents[&id]
            .cancel_turn_view
            .as_ref()
            .unwrap()
            .running_count,
        1
    );
    assert!(app.agents[&id].session.state.is_turn_running());
}

#[test]
fn cancel_turn_choice_stop_running_sends_cancel_true() {
    use crate::views::modal::CancelTurnChoice;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let effects = dispatch(
        Action::CancelTurnChoice(CancelTurnChoice::StopRunning),
        &mut app,
    );

    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::CancelTurn {
            cancel_subagents: true,
            ..
        }
    ));
    assert!(app.agents[&id].session.state.is_cancelling());
}

#[test]
fn cancel_turn_choice_continue_to_run_sends_cancel_false() {
    use crate::views::modal::CancelTurnChoice;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let effects = dispatch(
        Action::CancelTurnChoice(CancelTurnChoice::ContinueToRun),
        &mut app,
    );

    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::CancelTurn {
            cancel_subagents: false,
            ..
        }
    ));
    assert!(app.agents[&id].session.state.is_cancelling());
}

#[test]
fn cancel_turn_choice_after_turn_finished_is_noop() {
    use crate::views::modal::CancelTurnChoice;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    assert!(app.agents[&id].session.state.is_idle());

    let effects = dispatch(
        Action::CancelTurnChoice(CancelTurnChoice::StopRunning),
        &mut app,
    );

    assert!(effects.is_empty());
    assert!(app.agents[&id].session.state.is_idle());
}

#[test]
fn cancel_turn_choice_after_subagents_finished_still_cancels() {
    use crate::views::modal::CancelTurnChoice;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let mut info = make_test_subagent("child-1", "sa-1");
    info.finished = true;
    app.agents
        .get_mut(&id)
        .unwrap()
        .subagent_sessions
        .insert("child-1".into(), info);

    let effects = dispatch(
        Action::CancelTurnChoice(CancelTurnChoice::StopRunning),
        &mut app,
    );

    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::CancelTurn {
            cancel_subagents: true,
            ..
        }
    ));
    assert!(app.agents[&id].session.state.is_cancelling());
}

#[test]
fn cancel_turn_double_dispatch_falls_through_when_panel_open() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    app.agents
        .get_mut(&id)
        .unwrap()
        .subagent_sessions
        .insert("child-1".into(), make_test_subagent("child-1", "sa-1"));

    // First CancelTurn shows the panel.
    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(effects.is_empty());
    assert!(app.agents[&id].cancel_turn_view.is_some());

    // Second CancelTurn falls through (panel already open) and cancels.
    let effects = dispatch(Action::CancelTurn, &mut app);
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::CancelTurn {
            cancel_subagents: true,
            ..
        }
    ));
    assert!(app.agents[&id].session.state.is_cancelling());
}

#[test]
fn cancel_turn_when_idle_does_nothing() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    assert!(app.agents[&id].session.state.is_idle());

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(effects.is_empty());
    assert!(app.agents[&id].session.state.is_idle());
}

/// An Idle parent with a TurnRunning overlay child must cancel the child session.
#[test]
fn cancel_turn_in_subagent_overlay_cancels_child_while_parent_idle() {
    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    let child_sid = "child-overlay-idle-parent";
    let mut child_session = make_test_agent_session(&app, AgentId(1), child_sid);
    child_session.state = AgentState::TurnRunning;
    let child = AgentView::new(child_session, ScrollbackState::new());
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent
            .subagent_views
            .insert(child_sid.to_string(), Box::new(child));
        parent.active_subagent = Some(child_sid.to_string());
        assert!(parent.session.state.is_idle());
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                session_id,
                cancel_subagents: true,
                rewind_prompt_id: None,
                ..
            }] if session_id.0.as_ref() == child_sid
        ),
        "overlay stop must emit CancelTurn for the child session, got {effects:?}"
    );
    let parent = app.agents.get(&parent_id).unwrap();
    assert!(
        parent.session.state.is_idle(),
        "parent stays Idle; overlay stop is not a parent cancel"
    );
    assert!(parent.cancel_turn_view.is_none());
    let child = parent.subagent_views.get(child_sid).unwrap();
    assert!(
        child.session.state.is_cancelling(),
        "child overlay must show Cancelling"
    );
}

/// Overlay stop cancels the running child and must not open the parent ask panel.
#[test]
fn cancel_turn_in_subagent_overlay_does_not_open_parent_ask_panel() {
    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    let child_sid = "child-overlay-running-parent";
    let mut child_session = make_test_agent_session(&app, AgentId(1), child_sid);
    child_session.state = AgentState::TurnRunning;
    let child = AgentView::new(child_session, ScrollbackState::new());
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent.session.state = AgentState::TurnRunning;
        parent
            .subagent_sessions
            .insert(child_sid.into(), make_test_subagent(child_sid, "sa-1"));
        parent
            .subagent_views
            .insert(child_sid.to_string(), Box::new(child));
        parent.active_subagent = Some(child_sid.to_string());
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                session_id,
                cancel_subagents: true,
                ..
            }] if session_id.0.as_ref() == child_sid
        ),
        "overlay stop must target the child session, got {effects:?}"
    );
    let parent = app.agents.get(&parent_id).unwrap();
    assert!(
        parent.cancel_turn_view.is_none(),
        "ask panel on the parent is unreachable under the overlay"
    );
    assert!(
        parent.session.state.is_turn_running(),
        "parent turn is not the cancel target"
    );
    assert!(
        parent
            .subagent_views
            .get(child_sid)
            .unwrap()
            .session
            .state
            .is_cancelling()
    );
}

/// No overlay: a running subagent still opens the parent ask panel.
#[test]
fn cancel_turn_without_overlay_still_shows_subagent_ask_panel() {
    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    let child_sid = "child-not-focused";
    let mut child_session = make_test_agent_session(&app, AgentId(1), child_sid);
    child_session.state = AgentState::TurnRunning;
    let child = AgentView::new(child_session, ScrollbackState::new());
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent.session.state = AgentState::TurnRunning;
        parent
            .subagent_sessions
            .insert(child_sid.into(), make_test_subagent(child_sid, "sa-1"));
        parent
            .subagent_views
            .insert(child_sid.to_string(), Box::new(child));
        parent.active_subagent = None;
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(effects.is_empty());
    let parent = app.agents.get(&parent_id).unwrap();
    assert!(parent.cancel_turn_view.is_some());
    assert!(parent.session.state.is_turn_running());
    assert!(
        parent
            .subagent_views
            .get(child_sid)
            .unwrap()
            .session
            .state
            .is_turn_running(),
        "unfocused child must not be cancelled"
    );
}

/// Second `[stop]` while the overlay child is already Cancelling must re-send.
#[test]
fn cancel_turn_in_subagent_overlay_retries_when_child_already_cancelling() {
    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    let child_sid = "child-overlay-retry";
    let mut child_session = make_test_agent_session(&app, AgentId(1), child_sid);
    child_session.state = AgentState::TurnRunning;
    let child = AgentView::new(child_session, ScrollbackState::new());
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent
            .subagent_views
            .insert(child_sid.to_string(), Box::new(child));
        parent.active_subagent = Some(child_sid.to_string());
    }

    let first = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            first.as_slice(),
            [Effect::CancelTurn { session_id, .. }] if session_id.0.as_ref() == child_sid
        ),
        "first overlay stop must cancel the child, got {first:?}"
    );

    let retry = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            retry.as_slice(),
            [Effect::CancelTurn {
                session_id,
                cancel_subagents: true,
                rewind_prompt_id: None,
                ..
            }] if session_id.0.as_ref() == child_sid
        ),
        "second overlay stop must re-send child CancelTurn, got {retry:?}"
    );
    let parent = app.agents.get(&parent_id).unwrap();
    assert!(parent.session.state.is_idle());
    assert!(
        parent
            .subagent_views
            .get(child_sid)
            .unwrap()
            .session
            .state
            .is_cancelling()
    );
}

/// An Idle parent with a cancelling overlay child must keep Fast ticks for resend.
#[test]
fn tick_demand_fast_for_idle_parent_with_cancelling_overlay_child() {
    use crate::app::app_view::TickDemand;

    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    let child_sid = "child-overlay-tick-demand";
    let mut child_session = make_test_agent_session(&app, AgentId(1), child_sid);
    child_session.state = AgentState::TurnRunning;
    let mut child = AgentView::new(child_session, ScrollbackState::new());
    child.cancel_trigger_hint = Some(crate::app::actions::CancelTrigger::Mouse);
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent
            .subagent_views
            .insert(child_sid.to_string(), Box::new(child));
        parent.active_subagent = Some(child_sid.to_string());
        assert!(parent.session.state.is_idle());
    }
    assert_eq!(app.tick_demand(), TickDemand::None, "idle overlay parks");

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(effects.as_slice(), [Effect::CancelTurn { .. }]),
        "overlay stop must cancel the child, got {effects:?}"
    );
    assert_eq!(
        app.tick_demand(),
        TickDemand::Fast,
        "idle parent with a cancelling child must not park before resend grace"
    );
}

/// Overlay stop sends cancel_subagents true even when always_continue is set.
#[test]
fn cancel_turn_in_subagent_overlay_ignores_always_continue_pref() {
    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    let child_sid = "child-overlay-always-continue";
    let mut child_session = make_test_agent_session(&app, AgentId(1), child_sid);
    child_session.state = AgentState::TurnRunning;
    let mut child = AgentView::new(child_session, ScrollbackState::new());
    child.cancel_subagents_preference = Some(false);
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent.cancel_subagents_preference = Some(false);
        parent
            .subagent_views
            .insert(child_sid.to_string(), Box::new(child));
        parent.active_subagent = Some(child_sid.to_string());
    }
    app.current_ui.cancel_subagents_on_turn_cancel = Some("always_continue".into());

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                session_id,
                cancel_subagents: true,
                ..
            }] if session_id.0.as_ref() == child_sid
        ),
        "overlay stop must ignore always_continue, got {effects:?}"
    );
}

/// Dangling active_subagent is not an overlay; parent ask-panel still opens.
#[test]
fn cancel_turn_with_stale_active_subagent_still_shows_ask_panel() {
    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent.session.state = AgentState::TurnRunning;
        parent
            .subagent_sessions
            .insert("child-1".into(), make_test_subagent("child-1", "sa-1"));
        parent.active_subagent = Some("stale-sid".into());
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(effects.is_empty());
    let parent = app.agents.get(&parent_id).unwrap();
    assert!(parent.cancel_turn_view.is_some());
    assert!(parent.session.state.is_turn_running());
}

/// Overlay child with no session_id: no wire cancel and no local Cancelling.
#[test]
fn cancel_turn_in_subagent_overlay_without_session_id_is_noop() {
    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    let child_sid = "child-overlay-no-sid";
    let mut child_session = make_test_agent_session(&app, AgentId(1), child_sid);
    child_session.state = AgentState::TurnRunning;
    child_session.session_id = None;
    let child = AgentView::new(child_session, ScrollbackState::new());
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent
            .subagent_views
            .insert(child_sid.to_string(), Box::new(child));
        parent.active_subagent = Some(child_sid.to_string());
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(effects.is_empty());
    let parent = app.agents.get(&parent_id).unwrap();
    assert!(
        parent
            .subagent_views
            .get(child_sid)
            .unwrap()
            .session
            .state
            .is_turn_running(),
        "must not flip to Cancelling when there is no session to cancel"
    );
}

#[test]
fn reconcile_overdue_cancels_resends_for_overlay_child() {
    use crate::app::actions::CancelTrigger;
    use crate::app::dispatch::CANCEL_RESEND_GRACE;
    use crate::app::dispatch::reconcile_overdue_cancels;

    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    let child_sid = "child-overlay-resend";
    let mut child_session = make_test_agent_session(&app, AgentId(1), child_sid);
    child_session.state = AgentState::TurnRunning;
    let mut child = AgentView::new(child_session, ScrollbackState::new());
    child.cancel_trigger_hint = Some(CancelTrigger::Mouse);
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent
            .subagent_views
            .insert(child_sid.to_string(), Box::new(child));
        parent.active_subagent = Some(child_sid.to_string());
    }

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(matches!(effects.as_slice(), [Effect::CancelTurn { .. }]));
    assert!(reconcile_overdue_cancels(&mut app).is_none());

    app.agents
        .get_mut(&parent_id)
        .unwrap()
        .subagent_views
        .get_mut(child_sid)
        .unwrap()
        .pending_cancel_resend
        .as_mut()
        .unwrap()
        .sent_at = std::time::Instant::now() - CANCEL_RESEND_GRACE;

    let resent = reconcile_overdue_cancels(&mut app).expect("child overdue cancel must re-send");
    assert!(
        matches!(
            resent.as_slice(),
            [Effect::CancelTurn {
                session_id,
                trigger: Some(CancelTrigger::Mouse),
                rewind_prompt_id: None,
                ..
            }] if session_id.0.as_ref() == child_sid
        ),
        "auto-resend must target the child session, got {resent:?}"
    );
}

/// Idle parent with no overlay must not cancel a background running child view.
#[test]
fn cancel_turn_without_overlay_while_idle_is_noop_even_with_running_child() {
    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    let child_sid = "child-background-running";
    let mut child_session = make_test_agent_session(&app, AgentId(1), child_sid);
    child_session.state = AgentState::TurnRunning;
    let child = AgentView::new(child_session, ScrollbackState::new());
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent
            .subagent_views
            .insert(child_sid.to_string(), Box::new(child));
        parent.active_subagent = None;
        assert!(parent.session.state.is_idle());
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(effects.is_empty());
    let parent = app.agents.get(&parent_id).unwrap();
    assert!(parent.session.state.is_idle());
    assert!(
        parent
            .subagent_views
            .get(child_sid)
            .unwrap()
            .session
            .state
            .is_turn_running()
    );
}

#[test]
fn cancel_turn_when_already_cancelling_resends_cancel() {
    // A cancel that was sent but never resolved (lost notification or
    // lost turn-end response) used to make every further
    // Esc a silent no-op, permanently stranding the pane on
    // "Cancelling…". Cancelling again must RE-SEND the (idempotent)
    // cancel instead.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnCancelling;

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                cancel_subagents: true,
                ..
            }]
        ),
        "cancel while cancelling must re-send the cancel, got {effects:?}"
    );
    assert!(app.agents[&id].session.state.is_cancelling());
}

#[test]
fn cancel_turn_retry_honors_subagent_preference() {
    // The retry skips the subagent panel (the choice was already made on
    // the first cancel) but must reuse the remembered preference.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnCancelling;
        agent.cancel_subagents_preference = Some(false);
    }

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert!(matches!(
        effects.as_slice(),
        [Effect::CancelTurn {
            cancel_subagents: false,
            ..
        }]
    ));
}

/// The latched-cancel deadlock: cancel sent → state
/// `TurnCancelling` → the turn's PromptResponse RPC is lost → nothing can
/// ever exit the state. The armed broadcast marker must finish the turn
/// after the grace window.
#[test]
fn reconcile_finishes_cancelling_turn_after_grace() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnCancelling;
        agent.session.current_prompt_id = Some("pid-stuck".into());
    }
    arm_reconcile(
        &mut app,
        id,
        "pid-stuck",
        "cancelled",
        TURN_END_RECONCILE_GRACE + std::time::Duration::from_secs(1),
    );

    let fired = reconcile_overdue_turn_ends(&mut app);

    assert!(fired.is_some(), "an overdue marker must fire the reconcile");
    let agent = &app.agents[&id];
    assert!(
        agent.session.state.is_idle(),
        "reconcile must exit TurnCancelling"
    );
    assert!(agent.session.current_prompt_id.is_none());
    assert!(agent.pending_turn_end_reconcile.is_none());
    let has_cancelled_marker = (0..agent.scrollback.len()).any(|i| {
        matches!(
            agent.scrollback.entry(i).map(|e| &e.block),
            Some(RenderBlock::SessionEvent(ev))
                if matches!(ev.event, SessionEvent::TurnCancelled { .. })
        )
    });
    assert!(
        has_cancelled_marker,
        "reconcile must surface the 'Turn cancelled' marker"
    );
}

/// A lost-RPC reconcile for a send-now cancel (`_meta.cancelTrigger: "send_now"`) pushes no marker.
#[test]
fn reconcile_suppresses_send_now_cancel_marker() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.session.current_prompt_id = Some("pid-stuck".into());
    }
    arm_reconcile_with_trigger(
        &mut app,
        id,
        "pid-stuck",
        "cancelled",
        Some("send_now"),
        TURN_END_RECONCILE_GRACE + std::time::Duration::from_secs(1),
    );

    let fired = reconcile_overdue_turn_ends(&mut app);

    assert!(fired.is_some(), "the overdue reconcile must still fire");
    let agent = &app.agents[&id];
    assert!(agent.session.state.is_idle(), "the turn still finishes");
    let has_marker = (0..agent.scrollback.len()).any(|i| {
        matches!(
            agent.scrollback.entry(i).map(|e| &e.block),
            Some(RenderBlock::SessionEvent(ev))
                if matches!(
                    ev.event,
                    SessionEvent::TurnCancelled { .. } | SessionEvent::TurnCompleted { .. }
                )
        )
    });
    assert!(
        !has_marker,
        "a send-now cancel reconcile must not push a cancelled (or substitute \
         completed) marker"
    );
}

/// A lost-RPC reconcile for a hook-denied cancel consumes the parked
/// `cancellationCategory` and renders the blocked-by-a-hook marker, not
/// "cancelled by user".
#[test]
fn reconcile_renders_hook_denied_marker_from_parked_category() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.session.current_prompt_id = Some("pid-stuck".into());
    }
    arm_reconcile_with_meta(
        &mut app,
        id,
        "pid-stuck",
        "cancelled",
        None,
        Some(crate::app::turn_completion::HOOK_DENIED_CATEGORY),
        TURN_END_RECONCILE_GRACE + std::time::Duration::from_secs(1),
    );

    let fired = reconcile_overdue_turn_ends(&mut app);

    assert!(fired.is_some(), "the overdue reconcile must fire");
    let agent = &app.agents[&id];
    assert!(agent.session.state.is_idle());
    let has_blocked_marker = (0..agent.scrollback.len()).any(|i| {
        matches!(
            agent.scrollback.entry(i).map(|e| &e.block),
            Some(RenderBlock::SessionEvent(ev))
                if matches!(ev.event, SessionEvent::TurnBlockedByHook { .. })
        )
    });
    assert!(
        has_blocked_marker,
        "the reconcile must surface the blocked-by-a-hook marker"
    );
}

/// Older-shell fallback on the reconcile rail: no wire trigger, armed expectation.
#[test]
fn reconcile_suppresses_expected_send_now_cancel_without_wire_trigger() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.session.current_prompt_id = Some("pid-stuck".into());
        agent.expect_send_now_cancel = Some("p-next".into());
    }
    arm_reconcile(
        &mut app,
        id,
        "pid-stuck",
        "cancelled",
        TURN_END_RECONCILE_GRACE + std::time::Duration::from_secs(1),
    );

    let fired = reconcile_overdue_turn_ends(&mut app);

    assert!(fired.is_some());
    let agent = &app.agents[&id];
    let has_cancelled = (0..agent.scrollback.len()).any(|i| {
        matches!(
            agent.scrollback.entry(i).map(|e| &e.block),
            Some(RenderBlock::SessionEvent(ev))
                if matches!(ev.event, SessionEvent::TurnCancelled { .. })
        )
    });
    assert!(!has_cancelled, "expected send-now cancel renders no marker");
    assert!(
        agent.expect_send_now_cancel.is_none(),
        "the expectation is consumed by the reconcile"
    );
}

#[test]
fn reconcile_waits_for_grace_window() {
    // A freshly-armed marker means the RPC response may still be in
    // flight (healthy path: it lands milliseconds after the broadcast) —
    // do not touch the turn yet.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnCancelling;
        agent.session.current_prompt_id = Some("pid-stuck".into());
    }
    arm_reconcile(
        &mut app,
        id,
        "pid-stuck",
        "cancelled",
        std::time::Duration::ZERO,
    );

    let fired = reconcile_overdue_turn_ends(&mut app);

    assert!(fired.is_none());
    let agent = &app.agents[&id];
    assert!(agent.session.state.is_cancelling());
    assert!(
        agent.pending_turn_end_reconcile.is_some(),
        "marker must stay armed until grace expires"
    );
}

#[test]
fn reconcile_drops_stale_marker_when_turn_already_resolved() {
    // The normal path won the race (PromptResponse finished the turn, or
    // a new turn was adopted): the marker is stale and must be dropped
    // without touching state or pushing a marker.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let scrollback_before = app.agents[&id].scrollback.len();
    arm_reconcile(
        &mut app,
        id,
        "pid-old",
        "end_turn",
        TURN_END_RECONCILE_GRACE + std::time::Duration::from_secs(1),
    );

    let fired = reconcile_overdue_turn_ends(&mut app);

    assert!(fired.is_none(), "stale marker must not fire");
    let agent = &app.agents[&id];
    assert!(agent.session.state.is_idle());
    assert!(agent.pending_turn_end_reconcile.is_none());
    assert_eq!(agent.scrollback.len(), scrollback_before);
}

#[test]
fn reconcile_applies_stashed_running_adoption() {
    // The failing sequence: queued prompt promoted server-side while the
    // cancelled turn's response was lost. The reconcile must hand the pane
    // to the promoted prompt (turn-start shim), not strand it Idle.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnCancelling;
        agent.session.current_prompt_id = Some("pid-stuck".into());
    }
    // The leader's running_prompt_id broadcast arrived mid-teardown and
    // was stashed (same as the PromptResponse path).
    app.pending_running_adoptions.insert(
        id,
        crate::app::acp_handler::PendingRunningAdoption {
            prompt_id: "pid-next".into(),
            text: Some("queued prompt".into()),
            combined_texts: None,
            kind: "prompt".into(),
            turn_ended: false,
        },
    );
    arm_reconcile(
        &mut app,
        id,
        "pid-stuck",
        "cancelled",
        TURN_END_RECONCILE_GRACE + std::time::Duration::from_secs(1),
    );

    let fired = reconcile_overdue_turn_ends(&mut app);

    assert!(fired.is_some());
    let agent = &app.agents[&id];
    assert_eq!(
        agent.session.current_prompt_id.as_deref(),
        Some("pid-next"),
        "the stashed adoption must be applied after the reconcile"
    );
    assert!(
        matches!(agent.session.state, AgentState::TurnRunning),
        "the promoted prompt is the new running turn"
    );
    assert!(!app.pending_running_adoptions.contains_key(&id));
}

/// The reconcile rail's `stop_reason == "error"` arm: formats the raw
/// agent_result and skips the marker when a dedicated banner already
/// explains the failure.
#[test]
fn reconcile_error_formats_marker_and_defers_to_banner() {
    fn run(with_banner: bool) -> Option<String> {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        {
            let agent = app.agents.get_mut(&id).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.session.current_prompt_id = Some("pid-stuck".into());
            if with_banner {
                agent.scrollback.push_block(RenderBlock::session_event(
                    SessionEvent::RequestFailed {
                        status: Some(500),
                        headline: "Server error (500)".into(),
                        detail: String::new(),
                    },
                ));
            }
            agent.pending_turn_end_reconcile = Some(crate::app::agent_view::PendingTurnEnd {
                prompt_id: "pid-stuck".into(),
                stop_reason: Some("error".into()),
                agent_result: Some("boom".into()),
                cancel_trigger: None,
                cancellation_category: None,
                received_at: std::time::Instant::now()
                    - (TURN_END_RECONCILE_GRACE + std::time::Duration::from_secs(1)),
            });
        }
        let fired = reconcile_overdue_turn_ends(&mut app);
        assert!(
            fired.is_some(),
            "the overdue reconcile must finish the turn"
        );
        let agent = &app.agents[&id];
        (0..agent.scrollback.len()).find_map(|i| {
            match agent.scrollback.entry(i).map(|e| &e.block) {
                Some(RenderBlock::SessionEvent(ev)) => match &ev.event {
                    SessionEvent::TurnFailed { error, .. } => Some(error.clone()),
                    _ => None,
                },
                _ => None,
            }
        })
    }

    assert_eq!(
        run(false).as_deref(),
        Some("Request failed: boom. Try sending again."),
        "the raw agent_result must render as a formatted marker"
    );
    assert_eq!(
        run(true),
        None,
        "a dedicated banner must suppress the reconcile's TurnFailed marker"
    );
}

#[test]
fn always_stop_preference_skips_panel() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    app.agents.get_mut(&id).unwrap().cancel_subagents_preference = Some(true);
    app.agents
        .get_mut(&id)
        .unwrap()
        .subagent_sessions
        .insert("child-1".into(), make_test_subagent("child-1", "sa-1"));

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::CancelTurn {
            cancel_subagents: true,
            ..
        }
    ));
    assert!(app.agents[&id].cancel_turn_view.is_none());
}

#[test]
fn always_continue_preference_skips_panel() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    app.agents.get_mut(&id).unwrap().cancel_subagents_preference = Some(false);
    app.agents
        .get_mut(&id)
        .unwrap()
        .subagent_sessions
        .insert("child-1".into(), make_test_subagent("child-1", "sa-1"));

    let effects = dispatch(Action::CancelTurn, &mut app);

    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::CancelTurn {
            cancel_subagents: false,
            ..
        }
    ));
    assert!(app.agents[&id].cancel_turn_view.is_none());
}

#[test]
fn always_stop_choice_sets_preference() {
    use crate::views::modal::CancelTurnChoice;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let effects = dispatch(
        Action::CancelTurnChoice(CancelTurnChoice::AlwaysStop),
        &mut app,
    );

    assert_eq!(app.agents[&id].cancel_subagents_preference, Some(true));
    assert_eq!(
        app.current_ui.cancel_subagents_on_turn_cancel.as_deref(),
        Some("always_stop")
    );
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::PersistSetting {
            key: "cancel_subagents_on_turn_cancel",
            value: crate::settings::SettingValue::Enum("always_stop"),
            ..
        }
    )));
}

#[test]
fn always_continue_choice_sets_preference() {
    use crate::views::modal::CancelTurnChoice;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let effects = dispatch(
        Action::CancelTurnChoice(CancelTurnChoice::AlwaysContinue),
        &mut app,
    );

    assert_eq!(app.agents[&id].cancel_subagents_preference, Some(false));
    assert_eq!(
        app.current_ui.cancel_subagents_on_turn_cancel.as_deref(),
        Some("always_continue")
    );
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::PersistSetting {
            key: "cancel_subagents_on_turn_cancel",
            value: crate::settings::SettingValue::Enum("always_continue"),
            ..
        }
    )));
}

#[test]
fn prompt_response_clears_cancel_turn_panel() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    app.agents.get_mut(&id).unwrap().turn_started_at = Some(std::time::Instant::now());
    app.agents.get_mut(&id).unwrap().cancel_turn_view =
        Some(crate::views::modal::CancelTurnViewState {
            active_idx: 0,
            running_count: 2,
        });

    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Ok(acp::PromptResponse::new(acp::StopReason::EndTurn)),
            http_status: None,
            prompt_id: None,
        }),
        &mut app,
    );

    assert!(app.agents[&id].cancel_turn_view.is_none());
    assert!(app.agents[&id].session.state.is_idle());
}

#[test]
fn cancel_after_first_activity_does_not_restore() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("keep me".into()), &mut app);
    assert!(app.agents[&id].session.in_flight_prompt.is_some());
    // Simulate that the server emitted activity (the acp_handler
    // clear-on-first-activity hook would have cleared this).
    app.agents.get_mut(&id).unwrap().session.in_flight_prompt = None;

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::CancelTurn { .. }));

    // Prompt was NOT restored; user-prompt block stays; state is
    // the normal TurnCancelling (not the rewind-Idle).
    assert!(app.agents[&id].prompt.text().is_empty());
    assert_eq!(app.agents[&id].scrollback.len(), 1);
    assert!(app.agents[&id].session.state.is_cancelling());

    // PromptResponse arrives — TurnCancelled banner is pushed.
    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Ok(acp::PromptResponse::new(acp::StopReason::Cancelled)),
            http_status: None,
            prompt_id: None,
        }),
        &mut app,
    );
    // user_prompt + TurnCancelled banner.
    assert_eq!(app.agents[&id].scrollback.len(), 2);
}

/// Ctrl+C rewind of a locally-drained combined turn must remove *every*
/// per-segment user bubble (not just the last) and restore the joined text.
#[test]
fn cancel_rewind_removes_all_combined_segment_blocks() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let (first_id, last_id) = {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.session.current_prompt_id = Some("p-combo".into());
        // One bubble per original follow-up, as a combined drain paints them.
        let first_id = agent
            .scrollback
            .push_block(RenderBlock::user_prompt("first"));
        let last_id = agent
            .scrollback
            .push_block(RenderBlock::user_prompt("second"));
        agent.session.in_flight_prompt = Some(crate::app::agent::InFlightPrompt {
            text: "first\n\nsecond".into(),
            images: Vec::new(),
            scrollback_entry: last_id,
            combined_scrollback_entries: vec![first_id],
            chip_elements: Vec::new(),
        });
        (first_id, last_id)
    };

    let _ = dispatch(Action::CancelTurn, &mut app);

    let agent = &app.agents[&id];
    assert!(
        agent.scrollback.index_of_id(first_id).is_none(),
        "the earlier segment bubble must also be removed on rewind"
    );
    assert!(
        agent.scrollback.index_of_id(last_id).is_none(),
        "the primary segment bubble must be removed on rewind"
    );
    assert_eq!(
        agent.prompt.text(),
        "first\n\nsecond",
        "the joined combined text is restored into the composer"
    );
}

/// A cancel landing before first server activity must NOT rewind the stashed
/// in-flight prompt over a NEWER composer draft. Esc (and the mouse stop /
/// palette cancel) fire with the draft intact — unlike keyboard Ctrl+C,
/// which only cancels on an empty prompt — so the no-output rewind falls back
/// to the standard cancel and the draft survives.
#[test]
fn cancel_with_newer_draft_skips_no_output_rewind_and_keeps_draft() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let sent_id = {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent.session.current_prompt_id = Some("p-sent".into());
        let sent_id = agent
            .scrollback
            .push_block(RenderBlock::user_prompt("sent prompt"));
        agent.session.in_flight_prompt = Some(crate::app::agent::InFlightPrompt {
            text: "sent prompt".into(),
            images: Vec::new(),
            scrollback_entry: sent_id,
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        // Typed WHILE the turn was starting — newer than the stash.
        agent.prompt.set_text("newer draft");
        sent_id
    };

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(effects.as_slice(), [Effect::CancelTurn { .. }]),
        "cancel still flies to the server, got {effects:?}"
    );

    let agent = &app.agents[&id];
    assert_eq!(
        agent.prompt.text(),
        "newer draft",
        "the composer draft must survive the cancel (no rewind clobber)"
    );
    assert!(
        agent.scrollback.index_of_id(sent_id).is_some(),
        "standard cancel keeps the sent prompt's block (no rewind removal)"
    );
    assert!(
        agent.session.state.is_cancelling(),
        "standard cancel path (TurnCancelling), not the rewind-Idle"
    );
}

#[test]
fn entry_title_strips_skill_xml_from_generated_title() {
    use crate::views::session_title::entry_title;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.generated_session_title = Some(
        "<command-name>implement</command-name>\n\
             <command-message>/implement</command-message>\n\
             <command-args>fix the rendering bug</command-args>"
            .into(),
    );
    let title = entry_title(&app.agents[&AgentId(0)]);
    assert_eq!(title, "/implement fix the rendering bug");
}

#[test]
fn entry_title_strips_skill_xml_from_first_prompt() {
    use crate::scrollback::block::RenderBlock;
    use crate::views::session_title::entry_title;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.scrollback.push_block(RenderBlock::user_prompt(
        "<command-name>deploy</command-name>\n\
             <command-message>/deploy</command-message>",
    ));
    let title = entry_title(&app.agents[&AgentId(0)]);
    assert_eq!(title, "/deploy");
}

#[test]
fn prompt_history_loaded_sanitizes_skill_xml() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    let prompts = vec![
        "<command-name>review</command-name>\n\
             <command-message>/review</command-message>\n\
             <command-args>198653</command-args>"
            .into(),
        "plain prompt".into(),
        "<command-name>deploy</command-name>".into(),
    ];

    dispatch(
        Action::TaskComplete(TaskResult::PromptHistoryLoaded {
            agent_id: id,
            prompts,
        }),
        &mut app,
    );

    let history = &app.agents[&id].session.prompt_history;
    assert_eq!(history[0], "/review 198653");
    assert_eq!(history[1], "plain prompt");
    assert_eq!(history[2], "/deploy");
}

#[test]
fn bg_task_killed_already_exited_clears_pending_kill_on_inactive_agent() {
    let mut app = two_agent_app_with_bg_task();
    // Agent 1 has task-B-1 with pending_kill=true, active view is agent 0

    dispatch(
        Action::TaskComplete(TaskResult::BgTaskKilled {
            session_id: "sess-B".into(),
            task_id: "task-B-1".into(),
            outcome: Some(echo_agent_tools::types::KillOutcome::AlreadyExited),
        }),
        &mut app,
    );

    let task = &app.agents[&AgentId(1)].session.bg_tasks["task-B-1"];
    assert!(!task.pending_kill);
    assert!(task.kill_requested_at.is_none());
}

#[test]
fn bg_task_killed_not_found_removes_task_from_inactive_agent() {
    let mut app = two_agent_app_with_bg_task();

    dispatch(
        Action::TaskComplete(TaskResult::BgTaskKilled {
            session_id: "sess-B".into(),
            task_id: "task-B-1".into(),
            outcome: Some(echo_agent_tools::types::KillOutcome::NotFound),
        }),
        &mut app,
    );

    assert!(
        !app.agents[&AgentId(1)]
            .session
            .bg_tasks
            .contains_key("task-B-1")
    );
}

/// Resume regression: a stale row restored by replay keeps a
/// running "Task started" scrollback entry. When the ✗ kill resolves
/// `not_found`, the entry must be finished alongside the row removal so
/// the started block doesn't keep its running accent forever.
#[test]
fn bg_task_killed_not_found_finishes_scrollback_entry() {
    let mut app = two_agent_app_with_bg_task();
    {
        let agent1 = app.agents.get_mut(&AgentId(1)).unwrap();
        let eid = agent1.scrollback.push_block(RenderBlock::BgTask(
            crate::scrollback::blocks::BgTaskBlock::started("sleep 99", "task-B-1"),
        ));
        agent1.scrollback.set_last_running(true);
        agent1
            .session
            .bg_tasks
            .get_mut("task-B-1")
            .unwrap()
            .scrollback_entry_id = Some(eid);
        assert!(agent1.scrollback.needs_animation());
    }

    dispatch(
        Action::TaskComplete(TaskResult::BgTaskKilled {
            session_id: "sess-B".into(),
            task_id: "task-B-1".into(),
            outcome: Some(echo_agent_tools::types::KillOutcome::NotFound),
        }),
        &mut app,
    );

    let agent1 = &app.agents[&AgentId(1)];
    assert!(!agent1.session.bg_tasks.contains_key("task-B-1"));
    assert!(
        !agent1.scrollback.needs_animation(),
        "started entry must be finished when the stale row is removed"
    );
}

/// `outcome: None` (error envelope / unparseable payload) clears the
/// pending state so the user can retry, and keeps the row.
#[test]
fn bg_task_killed_missing_outcome_clears_pending_kill() {
    let mut app = two_agent_app_with_bg_task();

    dispatch(
        Action::TaskComplete(TaskResult::BgTaskKilled {
            session_id: "sess-B".into(),
            task_id: "task-B-1".into(),
            outcome: None,
        }),
        &mut app,
    );

    let task = &app.agents[&AgentId(1)].session.bg_tasks["task-B-1"];
    assert!(!task.pending_kill);
    assert!(task.kill_requested_at.is_none());
}

#[test]
fn bg_task_killed_keeps_pending_kill_on_killed_outcome() {
    let mut app = two_agent_app_with_bg_task();

    dispatch(
        Action::TaskComplete(TaskResult::BgTaskKilled {
            session_id: "sess-B".into(),
            task_id: "task-B-1".into(),
            outcome: Some(echo_agent_tools::types::KillOutcome::Killed),
        }),
        &mut app,
    );

    // "killed" means signal sent, wait for task_completed — pending_kill stays
    let task = &app.agents[&AgentId(1)].session.bg_tasks["task-B-1"];
    assert!(task.pending_kill);
}

#[test]
fn kill_bg_task_action_emits_client_ui_source() {
    use echo_agent_runtime::extensions::task::TaskKillSource;

    let mut app = test_app_with_agent();
    {
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent
            .session
            .bg_tasks
            .insert("pane-x".into(), super::make_bg_task("pane-x"));
    }

    let effects = dispatch(Action::KillBgTask("pane-x".into()), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::KillBgTask {
                task_id,
                source: TaskKillSource::ClientUi,
                ..
            }] if task_id == "pane-x"
        ),
        "single-task [×] must stay ClientUi, got {effects:?}"
    );
}

#[test]
fn bg_task_kill_failed_clears_pending_kill_on_inactive_agent() {
    let mut app = two_agent_app_with_bg_task();

    dispatch(
        Action::TaskComplete(TaskResult::BgTaskKillFailed {
            session_id: "sess-B".into(),
            task_id: "task-B-1".into(),
            error: "connection lost".into(),
        }),
        &mut app,
    );

    let task = &app.agents[&AgentId(1)].session.bg_tasks["task-B-1"];
    assert!(!task.pending_kill);
    assert!(task.kill_requested_at.is_none());
}

/// build_rows handles many subagents (placeholder).
#[test]
fn build_rows_collapses_many_subagents() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    // Insert 9 subagents.
    for i in 0..9 {
        let info = make_test_subagent(&format!("c{i}"), &format!("sa{i}"));
        agent
            .subagent_sessions
            .insert(info.child_session_id.to_string(), info);
    }
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    // 1 parent + 8 subagents + 1 placeholder = 10.
    assert_eq!(rows.len(), 10);
    assert!(rows.last().unwrap().is_more_placeholder);
    assert_eq!(rows.last().unwrap().more_count, 1);
}

/// Pin the threshold neighbour just BELOW the
/// `MAX_VISIBLE_SUBAGENTS = 8` cap. 7 subagents fit without a
/// placeholder.
#[test]
fn build_rows_seven_subagents_no_placeholder() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    for i in 0..7 {
        let info = make_test_subagent(&format!("c{i}"), &format!("sa{i}"));
        agent
            .subagent_sessions
            .insert(info.child_session_id.to_string(), info);
    }
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    // 1 parent + 7 subagents + 0 placeholder = 8.
    assert_eq!(rows.len(), 8);
    assert!(!rows.last().unwrap().is_more_placeholder);
}

/// At the threshold (exactly 8), no placeholder.
#[test]
fn build_rows_eight_subagents_no_placeholder() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    for i in 0..8 {
        let info = make_test_subagent(&format!("c{i}"), &format!("sa{i}"));
        agent
            .subagent_sessions
            .insert(info.child_session_id.to_string(), info);
    }
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    // 1 parent + 8 subagents + 0 placeholder = 9.
    assert_eq!(rows.len(), 9);
    assert!(!rows.last().unwrap().is_more_placeholder);
}

/// Well over the threshold (16), placeholder counts
/// the trailing 8 hidden rows.
#[test]
fn build_rows_sixteen_subagents_placeholder_counts_remainder() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    for i in 0..16 {
        let info = make_test_subagent(&format!("c{i}"), &format!("sa{i}"));
        agent
            .subagent_sessions
            .insert(info.child_session_id.to_string(), info);
    }
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    // 1 parent + 8 subagents + 1 placeholder = 10.
    assert_eq!(rows.len(), 10);
    assert!(rows.last().unwrap().is_more_placeholder);
    // 16 total - 8 shown = 8 hidden.
    assert_eq!(rows.last().unwrap().more_count, 8);
}

/// The live dashboard builder (`build_rows_with_roster`, used by both
/// rendering and keyboard navigation) hides subagents: only the parent
/// row is listed. The full-tree `build_rows` still emits them.
#[test]
fn build_rows_with_roster_hides_subagent_rows() {
    use crate::views::dashboard::{build_rows, build_rows_with_roster};
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    for i in 0..3 {
        let info = make_test_subagent(&format!("c{i}"), &format!("sa{i}"));
        agent
            .subagent_sessions
            .insert(info.child_session_id.to_string(), info);
    }
    let live = build_rows_with_roster(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
        &[],
    );
    assert_eq!(live.len(), 1, "only the parent row shows in the dashboard");
    assert!(
        live.iter().all(|r| r.indent == 0),
        "no nested subagent rows in the live dashboard"
    );
    // `build_rows` keeps the full tree (1 parent + 3 subagents).
    let full = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    assert_eq!(full.len(), 4, "build_rows still emits subagent rows");
    assert!(full.iter().any(|r| r.indent == 1));
}

/// Subagent labels also sanitise ANSI escapes
/// out of the persona.
#[test]
fn subagent_label_strips_control_characters() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let mut info = make_test_subagent("child-evil", "sa-evil");
    // Inject an ANSI escape into the persona — this is what flows
    // through `format_subagent_label` → row builder sanitisation.
    info.persona = Some(Arc::from("a\x1b[31mevil\x1b[0m"));
    agent
        .subagent_sessions
        .insert(info.child_session_id.to_string(), info);
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    let sub = rows
        .iter()
        .find(|r| r.indent > 0)
        .expect("subagent row expected");
    assert!(
        !sub.label.contains('\x1b'),
        "subagent label must not retain \\x1b: {:?}",
        sub.label
    );
    // Visible characters survive.
    assert!(
        sub.label.contains("evil"),
        "sanitised label should preserve printable characters, got {:?}",
        sub.label
    );
}

/// Sticky must land on parent + subagent and remain on parent after leaving
/// the subagent view (Esc clears `active_subagent` only).
#[serial_test::serial(MOUSE_CAPTURE_ENABLED)]
#[test]
fn mouse_reporting_toggle_sticky_survives_subagent_esc_to_parent() {
    reset_mouse_capture_enabled(true);
    assert!(mouse_capture_is_enabled());
    let mut app = test_app_with_agent();
    let parent_id = AgentId(0);
    let child_sid = "child-mouse-toggle".to_string();

    let child_session = make_test_agent_session(&app, AgentId(1), &child_sid);
    let child = AgentView::new(child_session, ScrollbackState::new());
    {
        let parent = app.agents.get_mut(&parent_id).unwrap();
        parent
            .subagent_views
            .insert(child_sid.clone(), Box::new(child));
        parent.active_subagent = Some(child_sid.clone());
    }
    app.registry = crate::actions::ActionRegistry::defaults_with_config(true);

    // Toggle while subagent is "focused" (active_subagent set).
    let _ = dispatch(Action::ToggleMouseCapture, &mut app);

    let parent = app.agents.get(&parent_id).unwrap();
    assert_eq!(parent.sticky_toast.as_deref(), Some(MOUSE_OFF_STICKY));
    let child = parent.subagent_views.get(&child_sid).unwrap();
    assert_eq!(
        child.sticky_toast.as_deref(),
        Some(MOUSE_OFF_STICKY),
        "child gets sticky recursively even if toast path targeted active view only"
    );

    // Simulate Esc: leave subagent, return to parent agent view.
    app.agents.get_mut(&parent_id).unwrap().active_subagent = None;

    let parent = app.agents.get(&parent_id).unwrap();
    assert_eq!(
        parent.sticky_toast.as_deref(),
        Some(MOUSE_OFF_STICKY),
        "parent keeps sticky after leaving subagent fullscreen"
    );
    assert!(parent.toast.is_none() || parent.sticky_toast.is_some());

    reset_mouse_capture_enabled(true);
}

#[test]
fn fork_failure_force_idle_drops_a_live_cancel_anchor() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let _ = super::super::turn::do_cancel_turn(
        &mut app,
        true,
        crate::app::cancel_latency::CancelOrigin::UserGesture,
    );
    assert!(
        app.agents[&id].cancel_latency.is_some(),
        "the cancel armed the anchor"
    );

    let _ = super::super::session::fork::handle_fork_session_failed(&mut app, id, "boom".into());

    assert!(
        app.agents[&id].cancel_latency.is_none(),
        "the fork teardown drops the anchor (no leak into a later settle)"
    );
}

#[test]
fn settled_cancel_emits_latency_from_arm_anchor_once() {
    use crate::app::cancel_latency::{CancelLatency, CancelOrigin, TurnEnd};
    use echo_agent_telemetry::events::CancellationScope;
    use std::time::{Duration, Instant};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let _ = super::super::turn::do_cancel_turn(&mut app, true, CancelOrigin::UserGesture);
    assert_eq!(
        app.agents[&id].cancel_latency.map(|c| c.scope),
        Some(CancellationScope::Turn),
        "the real arm path armed a Turn-scoped anchor"
    );

    let t0 = Instant::now();
    let agent = app.agents.get_mut(&id).unwrap();
    agent.cancel_latency = Some(CancelLatency::new(t0, CancellationScope::Turn));

    let event = agent
        .settle_cancel(TurnEnd::Completed, t0 + Duration::from_millis(50))
        .expect("a settled turn emits the pending anchor");
    assert_eq!(event.latency_ms, 50);
    assert_eq!(event.scope, CancellationScope::Turn);

    assert!(
        agent
            .settle_cancel(TurnEnd::Completed, t0 + Duration::from_millis(999))
            .is_none(),
        "the anchor is consumed, so a second settle emits nothing"
    );

    agent.cancel_latency = Some(CancelLatency::new(t0, CancellationScope::Turn));
    assert!(
        agent
            .settle_cancel(TurnEnd::Aborted, t0 + Duration::from_millis(50))
            .is_none(),
        "a torn-down turn discards the anchor unmeasured"
    );
}

#[test]
fn cancel_and_arm_anchors_before_the_cancel_teardown() {
    use crate::app::cancel_latency::CancelOrigin;
    use echo_agent_telemetry::events::CancellationScope;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    crate::app::agent_view::test_fixtures::add_running_execute(app.agents.get_mut(&id).unwrap());

    let agent = app.agents.get_mut(&id).unwrap();
    assert!(
        agent.scrollback.last().is_some_and(|e| e.is_running),
        "the running tool entry is live before the cancel"
    );

    agent.cancel_and_arm(CancellationScope::Turn, CancelOrigin::UserGesture);

    let requested_at = agent
        .cancel_latency
        .expect("the user gesture armed the anchor")
        .requested_at;
    let teardown_at = agent
        .scrollback
        .last()
        .and_then(|e| e.finished_at)
        .expect("the cancel teardown finished the running entry");
    assert!(
        requested_at <= teardown_at,
        "the latency anchor must be sampled before the cancel teardown finishes the turn"
    );
}
