use super::support::*;
use super::*;
use crate::session::plan_mode::PlanModeState;
/// An actor plus the `SessionEvent` rail its mode updates ride. Plan-mode
/// changes deliberately queue behind the turn's streaming deltas rather than
/// emitting straight to the client, so the assertions have to read that rail
/// and not the gateway.
async fn actor_with_events() -> (
    SessionActor,
    tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
) {
    let (gateway_tx, _) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel();
    create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await
}
/// Every mode id the actor has queued for the client so far, in order.
fn mode_updates(rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEvent>) -> Vec<String> {
    let mut seen = Vec::new();
    while let Ok(SessionEvent::Notification(notification)) = rx.try_recv() {
        let SessionNotification::Acp(notification) = notification else {
            continue;
        };
        if let acp::SessionUpdate::CurrentModeUpdate(update) = &notification.update {
            seen.push(update.current_mode_id.0.to_string());
        }
    }
    seen
}
#[tokio::test(flavor = "current_thread")]
async fn desktop_current_prompt_modes_record_every_admitted_ask_agent_turn() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut actor, mut events) = actor_with_events().await;
            actor.client_identifier = Some("echo-agent-desktop".into());
            actor.reconcile_plan_mode_with_prompt(PromptMode::Ask);
            actor.reconcile_plan_mode_with_prompt(PromptMode::Agent);
            actor.reconcile_plan_mode_with_prompt(PromptMode::Ask);
            assert_eq!(mode_updates(&mut events), vec!["ask", "default", "ask"]);
            assert_eq!(*actor.current_prompt_mode.lock(), PromptMode::Ask);
        })
        .await;
}
#[test]
fn prompt_mode_from_session_mode_id_uses_acp_session_mode() {
    assert_eq!(
        PromptMode::Ask,
        prompt_mode_from_session_mode_id(&acp::SessionModeId::new("ask"))
    );
    assert_eq!(
        PromptMode::Plan,
        prompt_mode_from_session_mode_id(&acp::SessionModeId::new("plan"))
    );
    assert_eq!(
        PromptMode::Agent,
        prompt_mode_from_session_mode_id(&acp::SessionModeId::new("default"))
    );
    assert_eq!(
        PromptMode::Agent,
        prompt_mode_from_session_mode_id(&acp::SessionModeId::new("browser_use"))
    );
    assert_eq!(
        PromptMode::Agent,
        prompt_mode_from_session_mode_id(&acp::SessionModeId::new("computer_use"))
    );
}
fn fn_def(name: &str) -> ToolDefinition {
    ToolDefinition::function(name, None::<&str>, serde_json::json!({"type": "object"}))
}
fn names(defs: &[ToolDefinition]) -> Vec<&str> {
    defs.iter().map(|d| d.function.name.as_str()).collect()
}
#[test]
fn cursor_filter_in_plan_mode_keeps_writes_and_shows_create_plan() {
    let defs = vec![
        fn_def("Read"),
        fn_def("Grep"),
        fn_def("Write"),
        fn_def("StrReplace"),
        fn_def("CreatePlan"),
        fn_def("SwitchMode"),
        fn_def("AskQuestion"),
    ];
    let filtered = filter_cursor_tools_by_plan_mode(defs, true);
    let kept = names(&filtered);
    assert!(kept.contains(&"Read"));
    assert!(kept.contains(&"Grep"));
    assert!(kept.contains(&"CreatePlan"));
    assert!(kept.contains(&"SwitchMode"));
    assert!(kept.contains(&"AskQuestion"));
    assert!(kept.contains(&"Write"));
    assert!(kept.contains(&"StrReplace"));
}
#[test]
fn cursor_filter_is_noop_for_non_cursor_tools() {
    let defs = vec![
        fn_def("read_file"),
        fn_def("search_replace"),
        fn_def("write"),
        fn_def("ask_user_question"),
        fn_def("enter_plan_mode"),
        fn_def("exit_plan_mode"),
    ];
    let in_plan = filter_cursor_tools_by_plan_mode(defs.clone(), true);
    let out_of_plan = filter_cursor_tools_by_plan_mode(defs.clone(), false);
    assert_eq!(names(&in_plan).len(), defs.len());
    assert_eq!(names(&out_of_plan).len(), defs.len());
}
/// Pins the `reconcile_plan_mode_with_prompt` transitions:
/// Plan → Pending, idempotent, non-plan modes exit cleanly.
#[test]
fn prompt_mode_plan_drives_tracker_into_pending_when_inactive() {
    use crate::session::plan_mode::PlanModeTracker;
    use std::path::PathBuf;
    fn reconcile(tracker: &mut PlanModeTracker, mode: PromptMode) {
        match mode {
            PromptMode::Plan => {
                tracker.enter_pending();
            }
            PromptMode::Agent | PromptMode::Ask => {
                if tracker.state() != PlanModeState::Inactive {
                    tracker.user_exit(false);
                }
            }
        }
    }
    let mut tracker = PlanModeTracker::new(PathBuf::from("/tmp/test"));
    assert_eq!(tracker.state(), PlanModeState::Inactive);
    reconcile(&mut tracker, PromptMode::Plan);
    assert_eq!(tracker.state(), PlanModeState::Pending);
    reconcile(&mut tracker, PromptMode::Plan);
    assert_eq!(tracker.state(), PlanModeState::Pending);
    reconcile(&mut tracker, PromptMode::Agent);
    assert_eq!(tracker.state(), PlanModeState::Inactive);
    reconcile(&mut tracker, PromptMode::Plan);
    assert_eq!(tracker.state(), PlanModeState::Pending);
    reconcile(&mut tracker, PromptMode::Ask);
    assert_eq!(tracker.state(), PlanModeState::Inactive);
}
#[test]
fn session_mode_id_from_prompt_mode_inverts_the_parse() {
    for id in ["plan", "ask", "default"] {
        let mode_id = acp::SessionModeId::new(id);
        let round_tripped =
            session_mode_id_from_prompt_mode(prompt_mode_from_session_mode_id(&mode_id));
        assert_eq!(round_tripped.0.as_ref(), id);
    }
}
/// A prompt that declares `_meta.mode` is the client changing mode, and the
/// client has to be told it took effect. Both arms used to persist the
/// transition and inject the model's reminder but emit nothing — so a client
/// that carries its mode on the prompt could enter or leave plan mode with no
/// signal at all, and `updates.jsonl` carried no mode line for replay either.
#[tokio::test]
async fn a_declared_mode_change_is_published_to_the_client() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut event_rx) = actor_with_events().await;
            actor.reconcile_plan_mode_with_prompt(PromptMode::Plan);
            actor.reconcile_plan_mode_with_prompt(PromptMode::Agent);
            assert_eq!(actor.plan_mode.lock().state(), PlanModeState::Inactive);
            assert_eq!(
                mode_updates(&mut event_rx),
                vec!["plan".to_string(), "default".to_string()],
            );
        })
        .await;
}
/// `ask` is its own client-facing mode, so leaving plan for it must not report
/// `default`.
#[tokio::test]
async fn leaving_plan_for_ask_reports_ask() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut event_rx) = actor_with_events().await;
            actor.reconcile_plan_mode_with_prompt(PromptMode::Plan);
            actor.reconcile_plan_mode_with_prompt(PromptMode::Ask);
            assert_eq!(
                mode_updates(&mut event_rx),
                vec!["plan".to_string(), "ask".to_string()],
            );
        })
        .await;
}
/// Re-declaring the mode already in effect is not a mode change. A client that
/// mirrors the session's mode back onto every prompt would otherwise emit one
/// `CurrentModeUpdate` per turn.
#[tokio::test]
async fn redeclaring_the_mode_already_in_effect_publishes_nothing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut event_rx) = actor_with_events().await;
            actor.reconcile_plan_mode_with_prompt(PromptMode::Plan);
            assert_eq!(mode_updates(&mut event_rx), vec!["plan".to_string()]);
            actor.reconcile_plan_mode_with_prompt(PromptMode::Plan);
            actor.reconcile_plan_mode_with_prompt(PromptMode::Plan);
            assert!(mode_updates(&mut event_rx).is_empty());
            actor.reconcile_plan_mode_with_prompt(PromptMode::Agent);
            assert_eq!(mode_updates(&mut event_rx), vec!["default".to_string()]);
            actor.reconcile_plan_mode_with_prompt(PromptMode::Agent);
            assert!(mode_updates(&mut event_rx).is_empty());
        })
        .await;
}
/// A synthetic turn — a background task wake, a goal summary, a notification
/// drain — declares no mode; it is constructed with a placeholder `Agent`.
/// Treating that placeholder as a declaration ended plan mode just by waking
/// the session, and silently: nothing was emitted, so the indicator stayed lit
/// for the rest of the session while the agent was back in agent mode.
#[tokio::test]
async fn a_synthetic_turn_inherits_plan_mode_instead_of_ending_it() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut event_rx) = actor_with_events().await;
            actor.reconcile_plan_mode_with_prompt(PromptMode::Plan);
            actor.plan_mode.lock().activate();
            let _ = mode_updates(&mut event_rx);
            for prompt_id in [
                "task-completed-abc",
                "subagent-completed-abc",
                "workflow-completed-abc",
                "notifications-1",
                "goal-summary-1",
                "goal-classifier-nudge-1",
                "scheduler-fired-1",
                "plan-resume-1",
            ] {
                let origin = crate::session::PromptOrigin::from_prompt_id(prompt_id);
                let resolved = actor.resolve_turn_prompt_mode(&origin, PromptMode::Agent);
                assert_eq!(
                    actor.plan_mode.lock().state(),
                    PlanModeState::Active,
                    "{prompt_id} must not end plan mode"
                );
                assert_eq!(
                    resolved,
                    PromptMode::Plan,
                    "{prompt_id} runs under the session's mode, so it is recorded under it too"
                );
                assert!(
                    mode_updates(&mut event_rx).is_empty(),
                    "{prompt_id} changed no mode, so it must announce none"
                );
            }
        })
        .await;
}
/// The other half of the same rule: a real user turn still applies what it
/// declared, and the resolved mode is what it asked for.
#[tokio::test]
async fn a_user_turn_still_applies_its_declared_mode() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut event_rx) = actor_with_events().await;
            let origin = crate::session::PromptOrigin::from_prompt_id("prompt-1");
            assert!(!origin.is_synthetic(), "precondition");
            let resolved = actor.resolve_turn_prompt_mode(&origin, PromptMode::Plan);
            assert_eq!(resolved, PromptMode::Plan);
            assert_eq!(actor.plan_mode.lock().state(), PlanModeState::Pending);
            assert_eq!(mode_updates(&mut event_rx), vec!["plan".to_string()]);
        })
        .await;
}

#[tokio::test]
async fn automation_mode_rebuild_removes_default_shell_tools_and_restores_them_on_exit() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (actor, _events) = actor_with_events().await;
            actor
                .apply_session_mode(acp::SessionModeId::new("computer_use"))
                .await
                .unwrap();
            assert_eq!(
                actor.active_agent_type.lock().as_deref(),
                Some("computer-use")
            );
            let tools = actor.agent.borrow().tool_bridge().tool_definitions().await;
            assert!(
                !tools
                    .iter()
                    .any(|t| t.function.name == "run_terminal_command")
            );
            assert!(
                actor
                    .agent
                    .borrow()
                    .system_prompt()
                    .contains("Computer Use mode")
            );
            actor
                .apply_session_mode(acp::SessionModeId::new("default"))
                .await
                .unwrap();
            let tools = actor.agent.borrow().tool_bridge().tool_definitions().await;
            assert!(
                tools
                    .iter()
                    .any(|t| t.function.name == "run_terminal_command")
            );
        })
        .await;
}

#[tokio::test]
async fn automation_mode_rebuild_rejects_inflight_turn_without_advertising_success() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (actor, mut events) = actor_with_events().await;
            let before = actor.active_agent_type.lock().clone();
            actor.state.lock().await.running_task = Some(AgentTask {
                prompt_id: "pending-turn".into(),
                handle: tokio::task::spawn_local(std::future::pending::<()>()).abort_handle(),
            });
            assert!(
                actor
                    .apply_session_mode(acp::SessionModeId::new("computer_use"))
                    .await
                    .is_err()
            );
            assert_eq!(*actor.active_agent_type.lock(), before);
            assert!(mode_updates(&mut events).is_empty());
        })
        .await;
}

#[tokio::test]
async fn every_regular_mode_restores_the_toolset_after_browser_or_computer_use() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for automation in ["computer_use", "browser_use"] {
                for target in ["plan", "ask", "default"] {
                    let (actor, mut events) = actor_with_events().await;
                    // The general actor fixture intentionally has an empty
                    // mock bridge. Build a real, customized return profile for
                    // this toolset-boundary test before measuring its inventory.
                    let mut profile = actor.agent.borrow().definition().clone();
                    profile.prompt_body =
                        Some("Preserve this session role across automation.".into());
                    let regular = actor.rebuild_spec.build_agent(profile).await.unwrap();
                    *actor.agent.borrow_mut() = regular;
                    let original = actor.agent.borrow().tool_bridge().tool_definitions().await;
                    let mut original_names: Vec<_> =
                        original.iter().map(|t| t.function.name.clone()).collect();
                    original_names.sort();
                    actor
                        .apply_session_mode(acp::SessionModeId::new(automation))
                        .await
                        .unwrap();
                    mode_updates(&mut events);
                    actor
                        .apply_session_mode(acp::SessionModeId::new(target))
                        .await
                        .unwrap();
                    let tools = actor.agent.borrow().tool_bridge().tool_definitions().await;
                    assert!(
                        tools
                            .iter()
                            .any(|t| t.function.name == "run_terminal_command"),
                        "{automation} -> {target} must restore the regular tools"
                    );
                    let mut restored_names: Vec<_> =
                        tools.iter().map(|t| t.function.name.clone()).collect();
                    restored_names.sort();
                    assert_eq!(
                        restored_names, original_names,
                        "restore the session's resolved tool profile"
                    );
                    assert!(!matches!(
                        actor.active_agent_type.lock().as_deref(),
                        Some("computer-use" | "browser-use")
                    ));
                    assert!(
                        !actor
                            .agent
                            .borrow()
                            .system_prompt()
                            .contains("Computer Use mode")
                    );
                    assert!(
                        !actor
                            .agent
                            .borrow()
                            .system_prompt()
                            .contains("Browser Use mode")
                    );
                    assert!(
                        actor
                            .agent
                            .borrow()
                            .system_prompt()
                            .contains("Preserve this session role across automation.")
                    );
                    assert_eq!(mode_updates(&mut events), vec![target.to_string()]);
                    assert_eq!(
                        *actor.current_prompt_mode.lock(),
                        prompt_mode_from_session_mode_id(&acp::SessionModeId::new(target))
                    );
                }
            }
        })
        .await;
}
