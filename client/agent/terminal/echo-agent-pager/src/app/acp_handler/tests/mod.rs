#![cfg_attr(rustfmt, rustfmt::skip)]
use super::*;
use crate::acp::model_state::ModelState;
use crate::acp::tracker::AcpUpdateTracker;
use crate::app::agent::{AgentId, AgentSession, AgentState, InFlightPrompt};
use crate::app::agent_view::AgentView;
use crate::scrollback::entry::EntryId;
use crate::scrollback::state::ScrollbackState;
use crate::views::permission_view::SubagentInfo;
use std::path::PathBuf;
use std::time::Instant;
use echo_agent_runtime::extensions::notification::RetryState;
use echo_agent_runtime::extensions::notification::SessionUpdate as EchoAgentSessionUpdate;
pub(super) fn make_session(session_id: Option<&str>) -> AgentSession {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    AgentSession {
        id: AgentId(0),
        acp_tx: tx,
        session_id: session_id.map(acp::SessionId::new),
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
    }
}
pub(super) fn make_agent(session_id: Option<&str>) -> AgentView {
    AgentView::new(make_session(session_id), ScrollbackState::new())
}
pub(super) fn permission_req_with_raw_input(
    raw_input: Option<serde_json::Value>,
) -> acp::RequestPermissionRequest {
    let fields = acp::ToolCallUpdateFields::new().raw_input(raw_input);
    acp::RequestPermissionRequest::new(
        acp::SessionId::new(std::sync::Arc::from("s1")),
        acp::ToolCallUpdate::new(
            acp::ToolCallId::new(std::sync::Arc::from("call-1")),
            fields,
        ),
        vec![],
    )
}
pub(super) fn recap_block(text: &str) -> RenderBlock {
    RenderBlock::session_event(SessionEvent::Recap {
        summary: text.to_string(),
        auto: false,
    })
}
pub(super) fn make_subagent_info(child_sid: &str) -> SubagentInfo {
    SubagentInfo {
        subagent_id: Arc::from(format!("sa-{child_sid}")),
        child_session_id: Arc::from(child_sid),
        description: Arc::from("test"),
        subagent_type: Arc::from("general-purpose"),
        persona: None,
        role: None,
        model: None,
        context_source: None,
        resumed_from: None,
        capability_mode: None,
        workflow_run_id: None,
        context_normalized: false,
        parent_prompt_id: None,
        started_at: Instant::now(),
        last_progress_at: Instant::now(),
        finished: false,
        status: None,
        error: None,
        duration_ms: None,
        tool_calls: None,
        turns: None,
        turn_count: None,
        tool_call_count: None,
        tokens_used: None,
        context_window_tokens: Some(131072),
        context_usage_pct: Some(85),
        tools_used: Vec::new(),
        error_count: None,
        activity_label: None,
        is_background: false,
        pending_kill: false,
        kill_requested_at: None,
        scrollback_entry_id: None,
        prompt: None,
        child_cwd: None,
        worktree_path: None,
        transcript: Default::default(),
    }
}
#[test]
fn workflow_catalog_projection_and_open_modal_refresh_are_coalesced() {
    let workflow = acp::AvailableCommand::new("review", "review")
        .meta(serde_json::json!({"workflowSource": "project"}).as_object().cloned());
    assert_eq!(
            workflow_commands(&[workflow]),
            vec![("review", "review", Some("project"), None)]
        );
    let mut app = make_app_with_agent("session-workflows");
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().extensions_modal = Some(
        crate::views::extensions_modal::ExtensionsModalState::new(
            crate::views::extensions_modal::ExtensionsTab::Skills,
        ),
    );
    queue_open_workflows_modal_refresh(&mut app, id);
    queue_open_workflows_modal_refresh(&mut app, id);
    assert_eq!(app.pending_effects.len(), 1);
    assert!(matches!(
            app.pending_effects.first(),
            Some(Effect::FetchWorkflowsList { agent_id, session_id })
                if *agent_id == id && session_id.0.as_ref() == "session-workflows"
        ));
}
#[test]
fn workflow_catalog_projection_detects_same_name_metadata_changes() {
    let command = |description: &str, path: &str| {
        acp::AvailableCommand::new("review", description)
            .meta(
                serde_json::json!({
                    "workflowSource": "project",
                    "workflowPath": path,
                })
                    .as_object()
                    .cloned(),
            )
    };
    assert_ne!(
            workflow_commands(&[command("Workflow: old", "/old/review.rhai")]),
            workflow_commands(&[command("Workflow: new", "/new/review.rhai")]),
        );
}
pub(super) fn compressed_entry(
    index: usize,
) -> echo_agent_runtime::extensions::notification::ImageCompressedEntry {
    echo_agent_runtime::extensions::notification::ImageCompressedEntry {
        index,
        original_bytes: 4_200_000,
        compressed_bytes: 780_000,
        original_width: 3024,
        original_height: 1964,
        compressed_width: 1568,
        compressed_height: 1018,
    }
}
/// Most recent `SessionEvent` pushed to the scrollback, if any.
pub(super) fn last_session_event(sb: &ScrollbackState) -> Option<SessionEvent> {
    (0..sb.len())
        .rev()
        .find_map(|i| match sb.get(i).map(|e| &e.block) {
            Some(RenderBlock::SessionEvent(b)) => Some(b.event.clone()),
            _ => None,
        })
}
pub(super) fn make_app_with_agent(session_id: &str) -> AppView {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = AppView::new(tx.clone(), ModelState::default(), Vec::new());
    app.leader_mode = true;
    let id = AgentId(0);
    let agent = make_agent(Some(session_id));
    app.agents.insert(id, agent);
    crate::app::dispatch::switch_to_agent(
        &mut app,
        id,
        crate::app::dispatch::SwitchCause::New,
    );
    app
}
/// A server-shape interjection broadcast (no `interjectionId`, like the
/// shared-queue interject path — every pane renders it).
pub(super) fn interjection_broadcast(
    session_id: &str,
    text: &str,
) -> acp::ExtNotification {
    acp::ExtNotification::new(
        "echo.agent/session/interjection",
        std::sync::Arc::from(
            serde_json::value::to_raw_value(
                    &serde_json::json!({
                    "sessionId": session_id,
                    "text": text,
                }),
                )
                .unwrap(),
        ),
    )
}
/// A Running background task registered on the agent's root session.
pub(super) fn insert_running_task(agent: &mut AgentView, task_id: &str, command: &str) {
    agent
        .session
        .bg_tasks
        .insert(
            task_id.into(),
            crate::app::agent::BgTaskState {
                task_id: task_id.into(),
                tool_call_id: format!("call-{task_id}"),
                command: command.into(),
                description: None,
                cwd: "/tmp".into(),
                output_file: "/tmp/out".into(),
                status: BgTaskStatus::Running,
                start_time: std::time::SystemTime::now(),
                end_time: None,
                exit_code: None,
                signal: None,
                stdout: String::new(),
                stdout_line_count: 0,
                truncated: false,
                pending_kill: false,
                kill_requested_at: None,
                scrollback_entry_id: None,
                is_monitor: false,
                restored_from_replay: false,
            },
        );
}
pub(super) fn park_on_subagents(agent: &mut AgentView, child_ids: &[&str]) {
    use crate::app::agent_view::test_fixtures::simulate_wait_all;
    agent.session.state = AgentState::TurnRunning;
    agent.session.current_prompt_id = Some("p1".into());
    for &child_id in child_ids {
        agent.subagent_sessions.insert(child_id.into(), make_subagent_info(child_id));
    }
    simulate_wait_all(agent);
    assert!(agent.renders_parked());
}
pub(super) fn follow_ups_ext(
    response_id: &str,
    labels: &[&str],
) -> acp::ExtNotification {
    let suggestions: Vec<serde_json::Value> = labels
        .iter()
        .map(|l| serde_json::json!({ "label": l }))
        .collect();
    let params = serde_json::json!({
            "response_id": response_id,
            "suggestions": suggestions,
        });
    acp::ExtNotification::new(
        "echo.agent/follow_ups",
        std::sync::Arc::from(serde_json::value::to_raw_value(&params).unwrap()),
    )
}
pub(super) fn follow_ups_ext_with_prompt(
    response_id: &str,
    prompt_id: &str,
    labels: &[&str],
) -> acp::ExtNotification {
    let suggestions: Vec<serde_json::Value> = labels
        .iter()
        .map(|l| serde_json::json!({ "label": l }))
        .collect();
    let params = serde_json::json!({
            "response_id": response_id,
            "promptId": prompt_id,
            "suggestions": suggestions,
        });
    acp::ExtNotification::new(
        "echo.agent/follow_ups",
        std::sync::Arc::from(serde_json::value::to_raw_value(&params).unwrap()),
    )
}
pub(super) fn voice_settings_update(enabled: bool) -> acp::ExtNotification {
    acp::ExtNotification::new(
        "echo.agent/settings/update",
        std::sync::Arc::from(
            serde_json::value::to_raw_value(
                    &serde_json::json!({ "voice_mode_enabled": enabled }),
                )
                .unwrap(),
        ),
    )
}
pub(super) fn tier_settings_update(tier: &str) -> acp::ExtNotification {
    acp::ExtNotification::new(
        "echo.agent/settings/update",
        std::sync::Arc::from(
            serde_json::value::to_raw_value(
                    &serde_json::json!({
                    "subscription_tier_display": tier
                }),
                )
                .unwrap(),
        ),
    )
}
pub(super) fn group_tool_verbs_settings_update(
    value: Option<bool>,
) -> acp::ExtNotification {
    let params = match value {
        Some(v) => serde_json::json!({ "group_tool_verbs": v }),
        None => serde_json::json!({}),
    };
    acp::ExtNotification::new(
        "echo.agent/settings/update",
        std::sync::Arc::from(serde_json::value::to_raw_value(&params).unwrap()),
    )
}
pub(super) fn collapsed_edit_blocks_settings_update(
    value: Option<bool>,
) -> acp::ExtNotification {
    let params = match value {
        Some(v) => serde_json::json!({ "collapsed_edit_blocks": v }),
        None => serde_json::json!({}),
    };
    acp::ExtNotification::new(
        "echo.agent/settings/update",
        std::sync::Arc::from(serde_json::value::to_raw_value(&params).unwrap()),
    )
}
pub(super) fn subagent_ext_replay(
    session_id: &str,
    update: serde_json::Value,
    event_id: &str,
) -> acp::ExtNotification {
    let params = serde_json::json!({
            "sessionId": session_id,
            "update": update,
            "_meta": { "isReplay": true, "eventId": event_id },
        });
    acp::ExtNotification::new(
        "echo.agent/session/update",
        std::sync::Arc::from(serde_json::value::to_raw_value(&params).unwrap()),
    )
}
pub(super) fn make_exit_plan_ext(
    plan_content: Option<&str>,
) -> (
    echo_agent_acp::AcpArgs<acp::ExtRequest>,
    tokio::sync::oneshot::Receiver<echo_agent_acp::AcpResult<acp::ExtResponse>>,
) {
    make_exit_plan_ext_with_tool_call_id("call-plan", plan_content)
}
pub(super) fn make_exit_plan_ext_with_tool_call_id(
    tool_call_id: &str,
    plan_content: Option<&str>,
) -> (
    echo_agent_acp::AcpArgs<acp::ExtRequest>,
    tokio::sync::oneshot::Receiver<echo_agent_acp::AcpResult<acp::ExtResponse>>,
) {
    let raw = serde_json::value::to_raw_value(
            &serde_json::json!({
            "sessionId": "sess-1",
            "toolCallId": tool_call_id,
            "planContent": plan_content,
        }),
        )
        .unwrap();
    let request = acp::ExtRequest::new("echo.agent/exit_plan_mode", raw.into());
    let (tx, rx) = tokio::sync::oneshot::channel();
    (
        echo_agent_acp::AcpArgs {
            request,
            response_tx: tx,
        },
        rx,
    )
}
pub(super) fn seed_pending_tool(agent: &mut AgentView, tool_call_id: &str, title: &str) {
    agent
        .session
        .tracker
        .handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(
                        acp::ToolCallId::new(
                            std::sync::Arc::from(tool_call_id.to_owned()),
                        ),
                        title.to_string(),
                    )
                    .kind(acp::ToolKind::Other)
                    .status(acp::ToolCallStatus::Pending)
                    .content(vec![])
                    .locations(vec![]),
            ),
            &NotificationMeta::default(),
            &mut agent.scrollback,
        );
}
pub(super) fn queue_changed_ext(session_id: &str, ids: &[&str]) -> acp::ExtNotification {
    let entries: Vec<serde_json::Value> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            serde_json::json!({
                    "id": id,
                    "version": 0,
                    "owner": "A",
                    "kind": "prompt",
                    "text": format!("text {id}"),
                    "position": i,
                })
        })
        .collect();
    let params = serde_json::json!({ "sessionId": session_id, "entries": entries });
    acp::ExtNotification::new(
        "echo.agent/queue/changed",
        std::sync::Arc::from(serde_json::value::to_raw_value(&params).unwrap()),
    )
}
/// Build a `echo.agent/queue/changed` notification carrying `runningPromptId`.
pub(super) fn queue_changed_running(
    session_id: &str,
    ids: &[&str],
    running: Option<&str>,
) -> acp::ExtNotification {
    queue_changed_running_ex(session_id, ids, running, None, None, None)
}
/// Like [`queue_changed_running`], with optional running-turn display
/// fields (`runningText` / `runningKind` / `runningCombinedTexts`).
pub(super) fn queue_changed_running_ex(
    session_id: &str,
    ids: &[&str],
    running: Option<&str>,
    running_text: Option<&str>,
    running_kind: Option<&str>,
    running_combined_texts: Option<&[&str]>,
) -> acp::ExtNotification {
    let entries: Vec<serde_json::Value> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            serde_json::json!({
                    "id": id,
                    "version": 0,
                    "kind": "prompt",
                    "text": format!("text {id}"),
                    "position": i,
                })
        })
        .collect();
    let mut params = serde_json::json!({ "sessionId": session_id, "entries": entries });
    if let Some(r) = running {
        params["runningPromptId"] = serde_json::Value::String(r.to_string());
    }
    if let Some(t) = running_text {
        params["runningText"] = serde_json::Value::String(t.to_string());
    }
    if let Some(k) = running_kind {
        params["runningKind"] = serde_json::Value::String(k.to_string());
    }
    if let Some(segs) = running_combined_texts {
        params["runningCombinedTexts"] = serde_json::json!(segs);
    }
    acp::ExtNotification::new(
        "echo.agent/queue/changed",
        std::sync::Arc::from(serde_json::value::to_raw_value(&params).unwrap()),
    )
}
/// Fixture: p1 runs locally; promoted queued bash b1's adoption is stashed.
pub(super) fn app_with_running_p1_and_stashed_b1() -> AppView {
    let mut app = make_app_with_agent("sess-1");
    {
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.session.current_prompt_id = Some("p1".to_string());
        agent.session.state = AgentState::TurnRunning;
        agent.note_self_originated_prompt("b1");
    }
    app.push_optimistic_prompt_echo("sess-1", "b1", "printf hi", "bash");
    assert!(handle_queue_changed(
            &queue_changed_running("sess-1", &[], Some("b1")),
            &mut app
        ));
    assert!(app.pending_running_adoptions.contains_key(&AgentId(0)));
    app
}
/// Drive a live Execute tool_call `session/update` through the full handler.
pub(super) fn send_tool_call_update(
    app: &mut AppView,
    prompt_id: &str,
    tool_id: &str,
    event_id: Option<&str>,
) {
    let mut meta = serde_json::json!({ "promptId": prompt_id });
    if let Some(eid) = event_id {
        meta["eventId"] = serde_json::Value::String(eid.to_string());
    }
    let (tx, _rx) = tokio::sync::oneshot::channel();
    handle(
        AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
            request: acp::SessionNotification::new(
                    acp::SessionId::new("sess-1"),
                    acp::SessionUpdate::ToolCall(
                        acp::ToolCall::new(
                                acp::ToolCallId::new(tool_id.to_owned()),
                                format!("tool {tool_id}"),
                            )
                            .kind(acp::ToolKind::Execute)
                            .status(acp::ToolCallStatus::Completed),
                    ),
                )
                .meta(meta.as_object().cloned()),
            response_tx: tx,
        }),
        app,
    );
}
/// Dispatch an `Ok(EndTurn)` PromptResponse for `prompt_id`.
pub(super) fn prompt_response(app: &mut AppView, prompt_id: &str) {
    use crate::app::actions::{Action, TaskResult};
    crate::app::dispatch::dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: AgentId(0),
            result: Ok(
                acp::PromptResponse::new(acp::StopReason::EndTurn)
                    .meta(
                        serde_json::json!({ "promptId": prompt_id })
                            .as_object()
                            .cloned(),
                    ),
            ),
            http_status: None,
            prompt_id: Some(prompt_id.to_string()),
        }),
        app,
    );
}
pub(super) fn tool_call_block_count(agent: &AgentView) -> usize {
    agent
        .scrollback
        .entries_in_range(0..agent.scrollback.len())
        .iter()
        .filter(|e| matches!(&e.block, RenderBlock::ToolCall(_)))
        .count()
}
pub(super) fn make_inject_notif(payload: &serde_json::Value) -> acp::ExtNotification {
    let raw = serde_json::value::to_raw_value(payload).unwrap();
    acp::ExtNotification::new(
        "echo.agent/scheduled_task_inject_prompt",
        std::sync::Arc::from(raw),
    )
}
pub(super) fn make_fired_notif(
    session_id: &str,
    task_id: &str,
    prompt: &str,
    human_schedule: &str,
    next_fire_at: Option<&str>,
) -> acp::ExtNotification {
    let notif = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::ScheduledTaskFired {
            task_id: task_id.into(),
            prompt: prompt.into(),
            human_schedule: human_schedule.into(),
            next_fire_at: next_fire_at.map(str::to_string),
            subagent_id: None,
        },
        meta: None,
    };
    let raw = serde_json::value::to_raw_value(&notif).unwrap();
    acp::ExtNotification::new("echo.agent/scheduled_task_fired", std::sync::Arc::from(raw))
}
pub(super) fn make_fired_notif_with_subagent(
    session_id: &str,
    task_id: &str,
    subagent_id: &str,
) -> acp::ExtNotification {
    let notif = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::ScheduledTaskFired {
            task_id: task_id.into(),
            prompt: "p".into(),
            human_schedule: "every 1 minute".into(),
            next_fire_at: Some("2026-02-02T02:02:02Z".into()),
            subagent_id: Some(subagent_id.into()),
        },
        meta: None,
    };
    let raw = serde_json::value::to_raw_value(&notif).unwrap();
    acp::ExtNotification::new("echo.agent/scheduled_task_fired", std::sync::Arc::from(raw))
}
/// Set up an app with two agents; the active view points to agent 1, but
/// agent 0 owns the scheduled task. Handlers that gate on `active_view`
/// will mutate the wrong agent (or silently no-op).
pub(super) fn make_app_two_agents() -> AppView {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = AppView::new(tx.clone(), ModelState::default(), Vec::new());
    let id0 = AgentId(0);
    let agent0 = make_agent(Some("sess-owner"));
    app.agents.insert(id0, agent0);
    let id1 = AgentId(1);
    let agent1 = make_agent(Some("sess-active"));
    app.agents.insert(id1, agent1);
    crate::app::dispatch::switch_to_agent(
        &mut app,
        id1,
        crate::app::dispatch::SwitchCause::New,
    );
    assert_eq!(app.active_view, ActiveView::Agent(AgentId(1)));
    app
}
pub(super) fn critical_announcement(
    id: &str,
) -> echo_agent_announcements::RemoteAnnouncement {
    echo_agent_announcements::RemoteAnnouncement {
        id: Some(id.into()),
        title: Some(format!("{id} title")),
        message: Some(format!("{id} message")),
        severity: Some("critical".into()),
        ..Default::default()
    }
}
pub(super) fn announcements_update_notif(
    r#gen: u64,
    announcements: &[echo_agent_announcements::RemoteAnnouncement],
) -> acp::ExtNotification {
    acp::ExtNotification::new(
        "echo.agent/announcements/update",
        std::sync::Arc::from(
            serde_json::value::to_raw_value(
                    &serde_json::json!({ "gen": r#gen, "announcements": announcements }),
                )
                .unwrap(),
        ),
    )
}
/// Id of the item the banner slot currently selects (None = banner closed).
pub(super) fn shown_banner_id(app: &AppView) -> Option<String> {
    crate::views::announcements::first_session_announcement(
            &app.active_announcements,
            &app.hidden_announcement_ids,
        )
        .and_then(|a| a.id.clone())
}
pub(super) fn make_created_ext_notif(
    session_id: &str,
    task_id: &str,
    prompt: &str,
    human_schedule: &str,
    next_fire_at: Option<&str>,
) -> acp::ExtNotification {
    let notif = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::ScheduledTaskCreated {
            task_id: task_id.into(),
            prompt: prompt.into(),
            human_schedule: human_schedule.into(),
            next_fire_at: next_fire_at.map(str::to_string),
        },
        meta: None,
    };
    let raw = serde_json::value::to_raw_value(&notif).unwrap();
    acp::ExtNotification::new("echo.agent/scheduled_task_created", std::sync::Arc::from(raw))
}
pub(super) fn make_deleted_ext_notif(
    session_id: &str,
    task_id: &str,
) -> acp::ExtNotification {
    make_deleted_ext_notif_with_reason(
        session_id,
        task_id,
        echo_agent_tools::notification::ScheduledTaskRemovedReason::Unknown,
        false,
    )
}
pub(super) fn make_deleted_ext_notif_with_reason(
    session_id: &str,
    task_id: &str,
    reason: echo_agent_tools::notification::ScheduledTaskRemovedReason,
    is_replay: bool,
) -> acp::ExtNotification {
    let notif = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::ScheduledTaskDeleted {
            task_id: task_id.into(),
            reason,
        },
        meta: is_replay.then(crate::acp::meta::ReplayMetaStamp::replayed),
    };
    let raw = serde_json::value::to_raw_value(&notif).unwrap();
    acp::ExtNotification::new("echo.agent/scheduled_task_deleted", std::sync::Arc::from(raw))
}
pub(super) fn make_token_notification_message(
    session_id: &str,
    total_tokens: u64,
) -> AcpClientMessage {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let request = acp::SessionNotification::new(
            acp::SessionId::new(session_id),
            acp::SessionUpdate::AgentMessageChunk(
                acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new("hi")),
                ),
            ),
        )
        .meta(serde_json::json!({
                "totalTokens": total_tokens,
            }).as_object().cloned());
    AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
        request,
        response_tx: tx,
    })
}
use crate::scrollback::block::RenderBlock;
/// Build an `AgentMessageChunk` notification carrying `text` for `session_id`.
pub(super) fn make_agent_chunk_message(
    session_id: &str,
    text: &str,
) -> AcpClientMessage {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let request = acp::SessionNotification::new(
        acp::SessionId::new(session_id),
        acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(text))),
        ),
    );
    AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
        request,
        response_tx: tx,
    })
}
/// `AgentMessageChunk` with `promptId`/`isReplay` + optional `eventId`.
pub(super) fn make_agent_chunk_meta(
    session_id: &str,
    text: &str,
    prompt_id: &str,
    event_id: Option<&str>,
    is_replay: bool,
) -> AcpClientMessage {
    let mut meta = serde_json::Map::new();
    meta.insert("promptId".to_string(), serde_json::json!(prompt_id));
    meta.insert("isReplay".to_string(), serde_json::json!(is_replay));
    if let Some(eid) = event_id {
        meta.insert("eventId".to_string(), serde_json::json!(eid));
    }
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let request = acp::SessionNotification::new(
            acp::SessionId::new(session_id),
            acp::SessionUpdate::AgentMessageChunk(
                acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new(text)),
                ),
            ),
        )
        .meta(serde_json::Value::Object(meta).as_object().cloned());
    AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
        request,
        response_tx: tx,
    })
}
/// `promptId`-tagged chunk (no `eventId`) — drives the viewer live-delta path.
pub(super) fn make_agent_chunk_message_with_prompt(
    session_id: &str,
    text: &str,
    prompt_id: &str,
    is_replay: bool,
) -> AcpClientMessage {
    make_agent_chunk_meta(session_id, text, prompt_id, None, is_replay)
}
/// Live (`isReplay=false`) chunk with an optional `eventId`, for dedup tests.
pub(super) fn make_agent_chunk_with_event(
    session_id: &str,
    text: &str,
    prompt_id: &str,
    event_id: Option<&str>,
) -> AcpClientMessage {
    make_agent_chunk_meta(session_id, text, prompt_id, event_id, false)
}
/// Replay-marked chunk with an eventId, as `session/load` emits.
pub(super) fn replay_chunk(
    session_id: &str,
    text: &str,
    event_id: &str,
) -> AcpClientMessage {
    make_agent_chunk_meta(session_id, text, "p-history", Some(event_id), true)
}
pub(super) fn scrollback_has_system_text(agent: &mut AgentView, needle: &str) -> bool {
    agent
        .scrollback
        .entries_mut()
        .any(|e| {
            matches!(&e.block, crate::scrollback::block::RenderBlock::System(b) if b.text.contains(needle))
        })
}
/// `Plan` update message with the given entry contents.
pub(super) fn plan_update_msg(
    session_id: &str,
    entries: &[&str],
    event_id: Option<&str>,
    is_replay: bool,
) -> AcpClientMessage {
    let entries = entries
        .iter()
        .map(|content| acp::PlanEntry::new(
            *content,
            acp::PlanEntryPriority::Medium,
            acp::PlanEntryStatus::Pending,
        ))
        .collect();
    let mut meta = serde_json::Map::new();
    meta.insert("isReplay".to_string(), serde_json::json!(is_replay));
    if let Some(eid) = event_id {
        meta.insert("eventId".to_string(), serde_json::json!(eid));
    }
    let (tx, _rx) = tokio::sync::oneshot::channel();
    AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
        request: acp::SessionNotification::new(
                acp::SessionId::new(session_id),
                acp::SessionUpdate::Plan(acp::Plan::new(entries)),
            )
            .meta(serde_json::Value::Object(meta).as_object().cloned()),
        response_tx: tx,
    })
}
pub(super) fn todo_contents(app: &AppView, id: AgentId) -> Vec<String> {
    app.agents[&id].todo.todos().iter().map(|t| t.content.clone()).collect()
}
pub(super) fn echo_agent_model_switch_notif(
    session_id: &str,
    event_id: &str,
) -> acp::ExtNotification {
    let payload = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::ModelAutoSwitched {
            previous_model_id: "m-old".into(),
            new_model_id: "m-new".into(),
            reason: "gone".into(),
        },
        meta: Some(serde_json::json!({ "eventId": event_id })),
    };
    acp::ExtNotification::new(
        "echo.agent/session/update",
        std::sync::Arc::from(serde_json::value::to_raw_value(&payload).unwrap()),
    )
}
pub(super) fn echo_agent_unhandled_notif(
    session_id: &str,
    event_id: &str,
) -> acp::ExtNotification {
    let payload = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::MemoryFlushStarted,
        meta: Some(serde_json::json!({ "eventId": event_id })),
    };
    acp::ExtNotification::new(
        "echo.agent/session/update",
        std::sync::Arc::from(serde_json::value::to_raw_value(&payload).unwrap()),
    )
}
/// Build an `agent_message_chunk` notification carrying both `totalTokens`
/// and an explicit `eventId`, for context/dedup interaction tests.
pub(super) fn make_token_notification_with_event(
    session_id: &str,
    total_tokens: u64,
    event_id: &str,
) -> AcpClientMessage {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let request = acp::SessionNotification::new(
            acp::SessionId::new(session_id),
            acp::SessionUpdate::AgentMessageChunk(
                acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new("hi")),
                ),
            ),
        )
        .meta(
            serde_json::json!({
                "totalTokens": total_tokens,
                "eventId": event_id,
            })
                .as_object()
                .cloned(),
        );
    AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
        request,
        response_tx: tx,
    })
}
/// Build an `echo.agent/session/prompt_complete` ext-notification for `session_id`.
pub(super) fn prompt_complete_ext(session_id: &str) -> acp::ExtNotification {
    let raw = serde_json::value::to_raw_value(
            &serde_json::json!({
            "sessionId": session_id,
            "stopReason": "end_turn",
        }),
        )
        .unwrap();
    acp::ExtNotification::new("echo.agent/session/prompt_complete", std::sync::Arc::from(raw))
}
/// Insert a fresh agent at `id` with an optional pre-assigned session id.
pub(super) fn insert_agent(app: &mut AppView, id: AgentId, session_id: Option<&str>) {
    app.agents.insert(id, make_agent(session_id));
}
/// Build an `echo.agent/session/prompt_complete` ext-notification with an explicit
/// `stopReason` and optional `agentResult`.
pub(super) fn prompt_complete_ext_with_reason(
    session_id: &str,
    stop_reason: &str,
    agent_result: Option<&str>,
) -> acp::ExtNotification {
    let mut payload = serde_json::json!({
            "sessionId": session_id,
            "stopReason": stop_reason,
        });
    if let Some(r) = agent_result {
        payload["agentResult"] = serde_json::json!(r);
    }
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    acp::ExtNotification::new("echo.agent/session/prompt_complete", std::sync::Arc::from(raw))
}
/// Build an `echo.agent/session/prompt_complete` ext-notification carrying a
/// `promptId` (shells with the lost-response fix). Built through the
/// typed [`PromptCompletePayload`] so the test wire shape can never
/// drift from what `handle_prompt_complete` parses.
pub(super) fn prompt_complete_ext_with_prompt_id(
    session_id: &str,
    prompt_id: &str,
    stop_reason: &str,
) -> acp::ExtNotification {
    let raw = serde_json::value::to_raw_value(
            &PromptCompletePayload {
                session_id: session_id.to_string(),
                stop_reason: Some(stop_reason.to_string()),
                prompt_id: Some(prompt_id.to_string()),
                agent_result: None,
                cancel_trigger: None,
                cancellation_category: None,
                meta: None,
            },
        )
        .unwrap();
    acp::ExtNotification::new("echo.agent/session/prompt_complete", std::sync::Arc::from(raw))
}
/// Build a live `AgentMessageChunk` whose meta carries `promptId` plus a
/// `turnStartMs` `start_ms_ago` milliseconds in the past — drives the viewer
/// adoption path with a known authoritative turn start.
pub(super) fn make_viewer_chunk_with_turn_start(
    session_id: &str,
    prompt_id: &str,
    start_ms_ago: i64,
) -> AcpClientMessage {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let turn_start_ms = chrono::Utc::now().timestamp_millis() - start_ms_ago;
    let request = acp::SessionNotification::new(
            acp::SessionId::new(session_id),
            acp::SessionUpdate::AgentMessageChunk(
                acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new("driver chunk")),
                ),
            ),
        )
        .meta(
            serde_json::json!({
                "promptId": prompt_id,
                "isReplay": false,
                "turnStartMs": turn_start_ms,
            })
                .as_object()
                .cloned(),
        );
    AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
        request,
        response_tx: tx,
    })
}
/// Build a durable `TurnCompleted` update on the `echo.agent/session/update` rail,
/// optionally stamped `isReplay`. Built through the typed `SessionNotification`
/// so the wire shape can't drift from what the dispatch parses.
pub(super) fn echo_agent_turn_completed_notif(
    session_id: &str,
    prompt_id: &str,
    stop_reason: &str,
    is_replay: bool,
) -> acp::ExtNotification {
    let payload = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::TurnCompleted {
            prompt_id: prompt_id.into(),
            stop_reason: stop_reason.into(),
            agent_result: None,
            usage: None,
        },
        meta: Some(serde_json::json!({ "isReplay": is_replay })),
    };
    acp::ExtNotification::new(
        "echo.agent/session/update",
        std::sync::Arc::from(serde_json::value::to_raw_value(&payload).unwrap()),
    )
}
/// Live `TurnCompleted` stamped with `_meta.cancelTrigger` (send-now / ctrl_c).
pub(super) fn echo_agent_turn_completed_notif_with_cancel_trigger(
    session_id: &str,
    prompt_id: &str,
    stop_reason: &str,
    cancel_trigger: &str,
) -> acp::ExtNotification {
    let payload = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::TurnCompleted {
            prompt_id: prompt_id.into(),
            stop_reason: stop_reason.into(),
            agent_result: None,
            usage: None,
        },
        meta: Some(
            serde_json::json!({
                "isReplay": false,
                "cancelTrigger": cancel_trigger,
            }),
        ),
    };
    acp::ExtNotification::new(
        "echo.agent/session/update",
        std::sync::Arc::from(serde_json::value::to_raw_value(&payload).unwrap()),
    )
}
/// A live durable `TurnCompleted`, optionally stamped with the shell
/// completion clock (`agentTimestampMs`) the wake marker's elapsed reads.
pub(super) fn echo_agent_wake_turn_completed_notif(
    session_id: &str,
    prompt_id: &str,
    agent_timestamp_ms: Option<i64>,
) -> acp::ExtNotification {
    let mut meta = serde_json::json!({ "isReplay": false });
    if let Some(ms) = agent_timestamp_ms {
        meta["agentTimestampMs"] = ms.into();
    }
    let payload = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::TurnCompleted {
            prompt_id: prompt_id.into(),
            stop_reason: "end_turn".into(),
            agent_result: None,
            usage: None,
        },
        meta: Some(meta),
    };
    acp::ExtNotification::new(
        "echo.agent/session/update",
        std::sync::Arc::from(serde_json::value::to_raw_value(&payload).unwrap()),
    )
}
/// Build a `HookExecution` update (one successful run) on the
/// `echo.agent/session/update` rail, optionally stamped `isReplay`.
/// `prompt_id == None` models pre-attribution shells.
pub(super) fn echo_agent_hook_execution_notif_for_prompt(
    session_id: &str,
    event_name: &str,
    prompt_id: Option<&str>,
    is_replay: bool,
) -> acp::ExtNotification {
    use echo_agent_runtime::extensions::notification::{HookRunEntryDto, HookRunStatusDto};
    echo_agent_hook_execution_notif_with_runs(
        session_id,
        event_name,
        prompt_id,
        is_replay,
        vec![HookRunEntryDto {
                name: "global/notify".into(),
                status: HookRunStatusDto::Success { elapsed_ms: 12 },
                output: None,
            }],
    )
}
pub(super) fn echo_agent_hook_execution_notif_with_runs(
    session_id: &str,
    event_name: &str,
    prompt_id: Option<&str>,
    is_replay: bool,
    runs: Vec<echo_agent_runtime::extensions::notification::HookRunEntryDto>,
) -> acp::ExtNotification {
    let payload = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::HookExecution {
            event_name: event_name.into(),
            tool_name: None,
            prompt_id: prompt_id.map(str::to_string),
            runs,
        },
        meta: Some(serde_json::json!({ "isReplay": is_replay })),
    };
    acp::ExtNotification::new(
        "echo.agent/session/update",
        serde_json::value::to_raw_value(&payload).unwrap().into(),
    )
}
pub(super) fn echo_agent_hook_execution_notif(
    session_id: &str,
    event_name: &str,
    is_replay: bool,
) -> acp::ExtNotification {
    echo_agent_hook_execution_notif_for_prompt(session_id, event_name, None, is_replay)
}
pub(super) fn count_lifecycle_blocks(
    sb: &crate::scrollback::state::ScrollbackState,
) -> usize {
    use crate::scrollback::blocks::tool::ToolCallBlock;
    (0..sb.len())
        .filter(|i| {
            matches!(
                    sb.get(*i).map(|e| &e.block),
                    Some(RenderBlock::ToolCall(ToolCallBlock::Lifecycle(_)))
                )
        })
        .count()
}
/// Stop-hook groups on the last turn-terminal session-event marker, if any.
pub(super) fn last_marker_stop_hook_groups(
    sb: &crate::scrollback::state::ScrollbackState,
) -> Option<usize> {
    (0..sb.len())
        .rev()
        .find_map(|i| match sb.get(i).map(|e| &e.block) {
            Some(RenderBlock::SessionEvent(b)) if b.event.is_turn_terminal() => {
                Some(b.stop_hooks.len())
            }
            _ => None,
        })
}
/// Work-only status lines ("N … still running") pushed as system rows.
/// Never pushed in production; tests assert emptiness.
pub(super) fn work_status_lines(sb: &ScrollbackState) -> Vec<String> {
    (0..sb.len())
        .filter_map(|i| match sb.get(i).map(|e| &e.block) {
            Some(RenderBlock::System(b)) if b.text.contains("still running") => {
                Some(b.text.clone())
            }
            _ => None,
        })
        .collect()
}
/// Register two running background commands on the (idle) agent through
/// the wire.
pub(super) fn seed_two_bg_tasks(app: &mut AppView, session_id: &str) {
    let _ = handle_ext_notification(
        &make_task_backgrounded_notif(session_id, "tc-1", "task-1", "sleep 98"),
        app,
    );
    let _ = handle_ext_notification(
        &make_task_backgrounded_notif(session_id, "tc-2", "task-2", "sleep 99"),
        app,
    );
}
/// Build an `echo.agent/session/interjection` ext-notification (no id).
pub(super) fn interjection_ext(session_id: &str, text: &str) -> acp::ExtNotification {
    interjection_ext_with_id(session_id, text, None)
}
/// Build an `echo.agent/session/interjection` ext-notification with an optional
/// `interjectionId` (the originator-dedup key).
pub(super) fn interjection_ext_with_id(
    session_id: &str,
    text: &str,
    interjection_id: Option<&str>,
) -> acp::ExtNotification {
    let mut payload = serde_json::json!({ "sessionId": session_id, "text": text });
    if let Some(id) = interjection_id {
        payload["interjectionId"] = serde_json::json!(id);
    }
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    acp::ExtNotification::new("echo.agent/session/interjection", std::sync::Arc::from(raw))
}
/// Text of the most recent user prompt block in scrollback, if any.
/// Interjections render as standard user prompt blocks.
pub(super) fn last_interjection_text(sb: &ScrollbackState) -> Option<String> {
    (0..sb.len())
        .rev()
        .find_map(|i| match sb.get(i).map(|e| &e.block) {
            Some(RenderBlock::UserPrompt(b)) => Some(b.text.clone()),
            _ => None,
        })
}
/// Switch the active view to `id` via the canonical helper. Wrapping
/// here keeps the source-scan invariant test
/// (`no_direct_active_view_assignment_outside_switch_to_agent`) happy.
pub(super) fn switch_active_to(app: &mut AppView, id: AgentId) {
    crate::app::dispatch::switch_to_agent(
        app,
        id,
        crate::app::dispatch::SwitchCause::Picker,
    );
}
/// Concatenate the text of every `AgentMessage` block in this view's scrollback.
pub(super) fn agent_message_text(view: &AgentView) -> String {
    let mut out = String::new();
    for i in 0..view.scrollback.len() {
        if let Some(entry) = view.scrollback.get(i)
            && let RenderBlock::AgentMessage(msg) = &entry.block
        {
            out.push_str(&msg.text());
        }
    }
    out
}
/// Build a `Plan` notification with one entry per `entries` string.
pub(super) fn make_plan_message(session_id: &str, entries: &[&str]) -> AcpClientMessage {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let plan_entries = entries
        .iter()
        .map(|content| acp::PlanEntry::new(
            *content,
            acp::PlanEntryPriority::Medium,
            acp::PlanEntryStatus::Pending,
        ))
        .collect();
    let request = acp::SessionNotification::new(
        acp::SessionId::new(session_id),
        acp::SessionUpdate::Plan(acp::Plan::new(plan_entries)),
    );
    AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
        request,
        response_tx: tx,
    })
}
/// Build an `AvailableCommandsUpdate` notification with the given command names.
pub(super) fn make_commands_update_message(
    session_id: &str,
    names: &[&str],
) -> AcpClientMessage {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let commands = names
        .iter()
        .map(|name| acp::AvailableCommand::new(*name, String::new()))
        .collect();
    let request = acp::SessionNotification::new(
        acp::SessionId::new(session_id),
        acp::SessionUpdate::AvailableCommandsUpdate(
            acp::AvailableCommandsUpdate::new(commands),
        ),
    );
    AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
        request,
        response_tx: tx,
    })
}
/// Build a `ToolCallUpdate` notification carrying a Bash `raw_output`
/// chunk for `tool_call_id`. Used to drive the bg-task stdout route.
pub(super) fn make_bash_stdout_message(
    session_id: &str,
    tool_call_id: &str,
    stdout: &str,
) -> AcpClientMessage {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let request = acp::SessionNotification::new(
        acp::SessionId::new(session_id),
        acp::SessionUpdate::ToolCallUpdate(
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new(tool_call_id),
                acp::ToolCallUpdateFields::new()
                    .raw_output(
                        Some(
                            serde_json::json!({
                    "type": "Bash",
                    "output_for_prompt": stdout,
                }),
                        ),
                    ),
            ),
        ),
    );
    AcpClientMessage::SessionNotification(echo_agent_acp::AcpArgs {
        request,
        response_tx: tx,
    })
}
/// Build an `ExtNotification` envelope for `echo.agent/session_notification`.
pub(super) fn make_ext_session_notification(
    session_id: &str,
    update: EchoAgentSessionUpdate,
) -> AcpClientMessage {
    make_ext_session_notification_with_method(
        session_id,
        "echo.agent/session_notification",
        update,
    )
}
/// Build an `ExtNotification` envelope with an explicit EchoAgent session method.
pub(super) fn make_ext_session_notification_with_method(
    session_id: &str,
    method: &str,
    update: EchoAgentSessionUpdate,
) -> AcpClientMessage {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let payload = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update,
        meta: None,
    };
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    let request = acp::ExtNotification::new(method, raw.into());
    AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
        request,
        response_tx: tx,
    })
}
use crate::scrollback::blocks::SubagentBlockKind;
pub(super) fn test_subagent_spawned(
    parent_sid: &str,
    child_sid: &str,
) -> EchoAgentSessionUpdate {
    test_subagent_spawned_for_workflow(parent_sid, child_sid, None)
}
pub(super) fn test_subagent_spawned_for_workflow(
    parent_sid: &str,
    child_sid: &str,
    workflow_run_id: Option<String>,
) -> EchoAgentSessionUpdate {
    EchoAgentSessionUpdate::SubagentSpawned {
        subagent_id: child_sid.into(),
        parent_session_id: parent_sid.into(),
        parent_prompt_id: None,
        child_session_id: child_sid.into(),
        subagent_type: "explore".into(),
        description: "scan src/".into(),
        effective_context_source: None,
        context_normalized: false,
        capability_mode: None,
        workflow_run_id,
        persona: None,
        role: None,
        model: None,
        resumed_from: None,
    }
}
pub(super) fn test_subagent_finished(child_sid: &str) -> EchoAgentSessionUpdate {
    EchoAgentSessionUpdate::SubagentFinished {
        subagent_id: child_sid.into(),
        child_session_id: child_sid.into(),
        status: "completed".into(),
        error: None,
        tool_calls: 2,
        turns: 1,
        duration_ms: 500,
        tokens_used: 0,
        output: None,
        will_wake: false,
    }
}
pub(super) fn test_subagent_progress(
    parent_sid: &str,
    child_sid: &str,
) -> EchoAgentSessionUpdate {
    EchoAgentSessionUpdate::SubagentProgress {
        subagent_id: child_sid.into(),
        parent_session_id: parent_sid.into(),
        child_session_id: child_sid.into(),
        duration_ms: 100,
        turn_count: 1,
        tool_call_count: 0,
        tokens_used: 0,
        context_window_tokens: 0,
        context_usage_pct: 0,
        tools_used: vec![],
        error_count: 0,
    }
}
/// Snapshot of subagent state after SubagentSpawned for method-parity tests.
pub(super) struct SubagentSpawnSnapshot {
    description: String,
    subagent_type: String,
    has_child_view: bool,
    scrollback_len: usize,
    child_session_id: String,
    block_kind: SubagentBlockKind,
    scrollback_entry_id: Option<EntryId>,
}
pub(super) fn snapshot_after_subagent_spawn(
    app: &AppView,
    child_sid: &str,
) -> SubagentSpawnSnapshot {
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let info = agent.subagent_sessions.get(child_sid).unwrap();
    let entry_id = info.scrollback_entry_id.expect("scrollback_entry_id after spawn");
    let entry = agent.scrollback.get_by_id(entry_id).unwrap();
    let RenderBlock::Subagent(sb) = &entry.block else {
        panic!("expected Subagent block after spawn");
    };
    SubagentSpawnSnapshot {
        description: info.description.to_string(),
        subagent_type: info.subagent_type.to_string(),
        has_child_view: agent.subagent_views.contains_key(child_sid),
        scrollback_len: agent.scrollback.len(),
        child_session_id: sb.child_session_id.clone(),
        block_kind: sb.kind.clone(),
        scrollback_entry_id: info.scrollback_entry_id,
    }
}
/// Snapshot after SubagentFinished for method-parity tests.
pub(super) struct SubagentFinishSnapshot {
    finished: bool,
    status: Option<String>,
    tool_calls: Option<u32>,
    turns: Option<u32>,
    duration_ms: Option<u64>,
    block_kind: SubagentBlockKind,
}
pub(super) fn snapshot_after_subagent_finish(
    app: &AppView,
    child_sid: &str,
) -> SubagentFinishSnapshot {
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let info = agent.subagent_sessions.get(child_sid).unwrap();
    let entry_id = info.scrollback_entry_id.expect("scrollback_entry_id after finish");
    let entry = agent.scrollback.get_by_id(entry_id).unwrap();
    let RenderBlock::Subagent(sb) = &entry.block else {
        panic!("expected Subagent block after finish");
    };
    SubagentFinishSnapshot {
        finished: info.finished,
        status: info.status.as_ref().map(|s| s.to_string()),
        tool_calls: info.tool_calls,
        turns: info.turns,
        duration_ms: info.duration_ms,
        block_kind: sb.kind.clone(),
    }
}
pub(super) fn run_subagent_lifecycle_via_method(
    method: &str,
    child_sid: &str,
) -> (SubagentSpawnSnapshot, SubagentFinishSnapshot) {
    let mut app = make_app_with_agent("sess-parent");
    let _ = handle(
        make_ext_session_notification_with_method(
            "sess-parent",
            method,
            test_subagent_spawned("sess-parent", child_sid),
        ),
        &mut app,
    );
    let spawn = snapshot_after_subagent_spawn(&app, child_sid);
    let _ = handle(
        make_ext_session_notification_with_method(
            "sess-parent",
            method,
            test_subagent_finished(child_sid),
        ),
        &mut app,
    );
    let finish = snapshot_after_subagent_finish(&app, child_sid);
    (spawn, finish)
}
/// Shared temp `ECHO_AGENT_HOME` for disk-replay tests. `echo_agent_home()` uses a
/// process-wide `OnceLock`, so parallel tests must not each set `ECHO_AGENT_HOME`
/// to a different tempdir.
pub(super) fn replay_disk_test_home() -> &'static std::path::Path {
    use std::sync::OnceLock;
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
            let tmp = tempfile::tempdir().expect("tempdir creation");
            unsafe {
                std::env::set_var("ECHO_AGENT_HOME", tmp.path());
            }
            tmp
        })
        .path()
}
/// Runs `f` with a thread-local echoagent home override so disk replay tests do not
/// depend on process-wide `echo_agent_home()` cache order when the full suite runs.
pub(super) fn with_replay_disk_home<R>(f: impl FnOnce(&std::path::Path) -> R) -> R {
    let home = replay_disk_test_home();
    crate::app::subagent::set_replay_echo_agent_home_for_tests(Some(home.to_path_buf()));
    let out = f(home);
    crate::app::subagent::set_replay_echo_agent_home_for_tests(None);
    out
}
pub(super) fn write_child_updates_jsonl(
    echo_agent_home: &std::path::Path,
    child_sid: &str,
    content: &str,
) {
    write_child_updates_jsonl_under_cwd(echo_agent_home, "/tmp", child_sid, content);
}
pub(super) fn write_child_updates_jsonl_under_cwd(
    echo_agent_home: &std::path::Path,
    cwd: &str,
    child_sid: &str,
    content: &str,
) {
    let sessions_dir = echo_agent_home
        .join("sessions")
        .join(echo_agent_config::encode_cwd_dirname(cwd))
        .join(child_sid);
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::write(sessions_dir.join("summary.json"), "{}").unwrap();
    std::fs::write(sessions_dir.join("updates.jsonl"), content).unwrap();
}
pub(super) fn child_scrollback_tool_call_count(
    agent: &AgentView,
    child_sid: &str,
) -> usize {
    let child = agent.subagent_views.get(child_sid).expect("child subagent view");
    (0..child.scrollback.len())
        .filter(|i| {
            child
                .scrollback
                .entry(*i)
                .is_some_and(|e| matches!(e.block, RenderBlock::ToolCall(_)))
        })
        .count()
}
/// `SessionEvent` blocks (the `TurnCompleted` footer) in a child scrollback.
pub(super) fn child_scrollback_session_event_count(
    agent: &AgentView,
    child_sid: &str,
) -> usize {
    let child = agent.subagent_views.get(child_sid).expect("child subagent view");
    (0..child.scrollback.len())
        .filter(|i| {
            child
                .scrollback
                .entry(*i)
                .is_some_and(|e| matches!(e.block, RenderBlock::SessionEvent(_)))
        })
        .count()
}
pub(super) fn child_tool_line(child_sid: &str) -> String {
    format!(
            r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"tool_call","toolCallId":"tc1","title":"Read foo","kind":"read","locations":[{{"path":"/tmp/foo"}}]}}}}}}"#
        )
}
pub(super) fn child_user_message_line(child_sid: &str, text: &str) -> String {
    let escaped = serde_json::to_string(text).unwrap();
    format!(
            r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"user_message_chunk","content":{{"type":"text","text":{escaped}}}}}}}}}"#
        )
}
pub(super) fn write_subagent_meta_json(
    echo_agent_home: &std::path::Path,
    parent_sid: &str,
    subagent_id: &str,
    prompt: &str,
) {
    let sessions_dir = echo_agent_home
        .join("sessions")
        .join(urlencoding::encode("/tmp").as_ref())
        .join(parent_sid)
        .join("subagents")
        .join(subagent_id);
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let json = format!(r#"{{"prompt":{}}}"#, serde_json::to_string(prompt).unwrap());
    std::fs::write(sessions_dir.join("meta.json"), json).unwrap();
}
/// The persisted echo of a task prompt wraps differently from the
/// injected copy, so compare with internal whitespace collapsed.
fn subagent_prompt_text_eq(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}
pub(super) fn child_scrollback_matching_prompt_count(
    agent: &AgentView,
    child_sid: &str,
    prompt: &str,
) -> usize {
    let child = agent.subagent_views.get(child_sid).expect("child subagent view");
    if prompt.trim().is_empty() {
        return 0;
    }
    (0..child.scrollback.len())
        .filter(|i| {
            child
                .scrollback
                .entry(*i)
                .is_some_and(|e| {
                    let t = match &e.block {
                        RenderBlock::UserPrompt(b) => Some(b.text.as_str()),
                        _ => None,
                    };
                    t.is_some_and(|s| subagent_prompt_text_eq(s, prompt))
                })
        })
        .count()
}
pub(super) fn child_tracker_expects_user_echo(
    agent: &AgentView,
    child_sid: &str,
) -> bool {
    agent
        .subagent_views
        .get(child_sid)
        .expect("child subagent view")
        .session
        .tracker
        .expects_user_echo()
}
pub(super) fn spawn_subagent_with_optional_updates(
    app: &mut AppView,
    child_sid: &str,
    updates: Option<&str>,
) {
    if let Some(content) = updates {
        write_child_updates_jsonl(replay_disk_test_home(), child_sid, content);
    }
    let _ = handle(
        make_ext_session_notification_with_method(
            "sess-parent",
            "echo.agent/session/update",
            test_subagent_spawned("sess-parent", child_sid),
        ),
        app,
    );
}
/// A minimal `GoalUpdated` `update` object (the required wire fields) for
/// `sess-A`; callers add optional fields before dispatching.
pub(super) fn goal_update_value(
    goal_id: &str,
    status: &str,
    elapsed_ms: u64,
) -> serde_json::Value {
    serde_json::json!({
            "sessionUpdate": "goal_updated",
            "goal_id": goal_id,
            "objective": "obj",
            "status": status,
            "phase": "executing",
            "tokens_used": 0,
            "elapsed_ms": elapsed_ms,
            "total_deliverables": 0,
            "completed_deliverables": 0,
            "total_worker_rounds": 0,
            "total_verify_rounds": 0,
            "token_baseline": 0,
            "finished_subagent_tokens": 0,
        })
}
/// Wrap an `update` object in the session envelope and run it through the
/// real handler; returns whether the notification requested a redraw.
pub(super) fn dispatch_goal_update(
    app: &mut AppView,
    update: serde_json::Value,
) -> bool {
    let raw_payload = serde_json::json!({ "sessionId": "sess-A", "update": update });
    let raw = serde_json::value::to_raw_value(&raw_payload).unwrap();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    handle(
        AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
            request: acp::ExtNotification::new("echo.agent/session_notification", raw.into()),
            response_tx: tx,
        }),
        app,
    )
}
/// Build + dispatch a `GoalUpdated` for `sess-A` with the given id /
/// status / elapsed; returns whether the notification requested a redraw.
pub(super) fn send_goal_update(
    app: &mut AppView,
    goal_id: &str,
    status: &str,
    elapsed_ms: u64,
) -> bool {
    dispatch_goal_update(app, goal_update_value(goal_id, status, elapsed_ms))
}
/// Build a minimal `RequestPermission` message that carries `session_id`
/// and one `AllowOnce` option.
pub(super) fn make_permission_message(
    session_id: &str,
) -> (
    AcpClientMessage,
    tokio::sync::oneshot::Receiver<Result<acp::RequestPermissionResponse, acp::Error>>,
) {
    use std::sync::Arc;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let request = acp::RequestPermissionRequest::new(
        acp::SessionId::new(session_id),
        acp::ToolCallUpdate::new(
            acp::ToolCallId::new(Arc::from("call-perm-1")),
            acp::ToolCallUpdateFields::default(),
        ),
        vec![acp::PermissionOption::new(
                acp::PermissionOptionId::new(Arc::from("allow-once")),
                "Allow once",
                acp::PermissionOptionKind::AllowOnce,
            )],
    );
    let msg = AcpClientMessage::RequestPermission(echo_agent_acp::AcpArgs {
        request,
        response_tx: tx,
    });
    (msg, rx)
}
/// Build an `echo.agent/session_notification` carrying
/// `InteractionResolved{tool_call_id}` (the first-answer-wins broadcast that
/// tells every other pane to retract its shared interaction modal).
pub(super) fn interaction_resolved_ext(
    session_id: &str,
    tool_call_id: &str,
) -> acp::ExtNotification {
    let notif = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::InteractionResolved {
            tool_call_id: tool_call_id.into(),
        },
        meta: None,
    };
    let raw = serde_json::value::to_raw_value(&notif).unwrap();
    acp::ExtNotification::new("echo.agent/session_notification", std::sync::Arc::from(raw))
}
pub(super) fn make_git_head_changed_notif(
    session_id: &str,
    branch: Option<&str>,
    is_worktree: bool,
    main_repo: Option<&str>,
) -> acp::ExtNotification {
    let payload = echo_agent_workspace::session::git::GitHeadChanged {
        session_id: session_id.into(),
        branch: branch.map(str::to_string),
        is_worktree,
        main_repo: main_repo.map(str::to_string),
    };
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    acp::ExtNotification::new("echo.agent/git_head_changed", std::sync::Arc::from(raw))
}
pub(super) fn make_task_backgrounded_notif(
    session_id: &str,
    tool_call_id: &str,
    task_id: &str,
    command: &str,
) -> acp::ExtNotification {
    let notif = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::TaskBackgrounded {
            tool_call_id: tool_call_id.into(),
            task_id: task_id.into(),
            command: command.into(),
            cwd: "/tmp".into(),
            output_file: "/tmp/output.log".into(),
            monitor_description: None,
            description: None,
        },
        meta: None,
    };
    let raw = serde_json::value::to_raw_value(&notif).unwrap();
    acp::ExtNotification::new("echo.agent/task_backgrounded", std::sync::Arc::from(raw))
}
/// Like [`make_task_backgrounded_notif`] but stamped `_meta.isReplay:
/// true` via the typed [`ReplayMetaStamp`](crate::acp::meta::ReplayMetaStamp),
/// mirroring the `session/load` replay envelope.
pub(super) fn make_replayed_task_backgrounded_notif(
    session_id: &str,
    tool_call_id: &str,
    task_id: &str,
    command: &str,
) -> acp::ExtNotification {
    let notif = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::TaskBackgrounded {
            tool_call_id: tool_call_id.into(),
            task_id: task_id.into(),
            command: command.into(),
            cwd: "/tmp".into(),
            output_file: "/tmp/output.log".into(),
            monitor_description: None,
            description: None,
        },
        meta: Some(crate::acp::meta::ReplayMetaStamp::replayed()),
    };
    let raw = serde_json::value::to_raw_value(&notif).unwrap();
    acp::ExtNotification::new("echo.agent/session/update", std::sync::Arc::from(raw))
}
/// Register a pending Execute tool call in the tracker and send an InProgress
/// update to create the scrollback entry. Returns the agent for further use.
pub(super) fn setup_pending_execute_tool(app: &mut AppView, tc_id: &str) {
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let meta = crate::acp::meta::NotificationMeta::default();
    let tc = agent_client_protocol::SessionUpdate::ToolCall(
        agent_client_protocol::ToolCall::new(
                agent_client_protocol::ToolCallId::new(std::sync::Arc::from(tc_id)),
                "Execute `sleep 9999`".to_string(),
            )
            .kind(agent_client_protocol::ToolKind::Execute)
            .status(agent_client_protocol::ToolCallStatus::Pending)
            .content(vec![])
            .locations(vec![]),
    );
    agent.session.tracker.handle_update(tc, &meta, &mut agent.scrollback);
    let update = agent_client_protocol::SessionUpdate::ToolCallUpdate(
        agent_client_protocol::ToolCallUpdate::new(
            agent_client_protocol::ToolCallId::new(std::sync::Arc::from(tc_id)),
            agent_client_protocol::ToolCallUpdateFields::new()
                .status(Some(agent_client_protocol::ToolCallStatus::InProgress)),
        ),
    );
    agent.session.tracker.handle_update(update, &meta, &mut agent.scrollback);
}
/// Send a late InProgress update with is_background=true to trigger late bg detection.
pub(super) fn send_late_bg_detection(app: &mut AppView, tc_id: &str) {
    use serde_json::json;
    use echo_agent_tools::types::output::{BashOutput, ToolOutput};
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let meta = crate::acp::meta::NotificationMeta::default();
    let bash = BashOutput {
        output: b"more output".to_vec(),
        output_for_prompt: String::new(),
        exit_code: 0,
        command: "sleep 9999".to_string(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: Some("long running".to_string()),
        current_dir: "/tmp".to_string(),
        output_file: String::new(),
        total_bytes: 11,
        output_delta: None,
        was_bare_echo: false,
    };
    let update = agent_client_protocol::SessionUpdate::ToolCallUpdate(
        agent_client_protocol::ToolCallUpdate::new(
            agent_client_protocol::ToolCallId::new(std::sync::Arc::from(tc_id)),
            agent_client_protocol::ToolCallUpdateFields::new()
                .status(Some(agent_client_protocol::ToolCallStatus::InProgress))
                .raw_output(serde_json::to_value(ToolOutput::Bash(bash)).ok())
                .raw_input(
                    Some(
                        json!({
                        "command": "sleep 9999",
                        "is_background": true,
                        "description": "long running"
                    }),
                    ),
                ),
        ),
    );
    agent.session.tracker.handle_update(update, &meta, &mut agent.scrollback);
}
pub(super) fn make_app_with_parent_and_child(
    parent_sid: &str,
    child_sid: &str,
) -> AppView {
    let mut app = make_app_with_agent(parent_sid);
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.subagent_sessions.insert(child_sid.into(), make_subagent_info(child_sid));
    let child_view = make_agent(Some(child_sid));
    agent.subagent_views.insert(child_sid.into(), Box::new(child_view));
    app
}
pub(super) fn make_task_completed_notif(
    session_id: &str,
    task_id: &str,
    command: &str,
    exit_code: Option<i32>,
) -> acp::ExtNotification {
    make_task_completed_notif_with_signal(session_id, task_id, command, exit_code, None)
}
pub(super) fn make_task_completed_notif_with_signal(
    session_id: &str,
    task_id: &str,
    command: &str,
    exit_code: Option<i32>,
    signal: Option<&str>,
) -> acp::ExtNotification {
    task_completed_notif(session_id, task_id, command, exit_code, signal, false)
}
pub(super) fn task_completed_notif(
    session_id: &str,
    task_id: &str,
    command: &str,
    exit_code: Option<i32>,
    signal: Option<&str>,
    will_wake: bool,
) -> acp::ExtNotification {
    use echo_agent_tools::types::TaskSnapshot;
    let notif = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::TaskCompleted {
            task_snapshot: TaskSnapshot {
                task_id: task_id.into(),
                command: command.into(),
                display_command: None,
                cwd: "/tmp".into(),
                start_time: std::time::SystemTime::now(),
                end_time: Some(std::time::SystemTime::now()),
                output: String::new(),
                output_file: "/tmp/out.log".into(),
                truncated: false,
                exit_code,
                signal: signal.map(|s| s.to_string()),
                completed: true,
                kind: Default::default(),
                block_waited: false,
                explicitly_killed: false,
                kill_result_delivered: false,
                owner_session_id: None,
                description: None,
                is_backgrounded: false,
                output_total_bytes: 0,
            },
            will_wake,
        },
        meta: None,
    };
    let raw = serde_json::value::to_raw_value(&notif).unwrap();
    acp::ExtNotification::new("echo.agent/task_completed", std::sync::Arc::from(raw))
}
pub(super) fn make_monitor_event_notif(
    session_id: &str,
    task_id: &str,
    event_text: &str,
) -> acp::ExtNotification {
    let notif = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::MonitorEvent {
            task_id: task_id.into(),
            description: "test monitor".into(),
            event_text: event_text.into(),
        },
        meta: None,
    };
    let raw = serde_json::value::to_raw_value(&notif).unwrap();
    acp::ExtNotification::new("echo.agent/monitor_event", std::sync::Arc::from(raw))
}
pub(super) fn make_model_info(id: &str) -> acp::ModelInfo {
    acp::ModelInfo::new(acp::ModelId::new(std::sync::Arc::from(id)), id.to_string())
}
pub(super) fn make_models_update_notif(
    current_model_id: &str,
    model_ids: &[&str],
) -> acp::ExtNotification {
    let models: Vec<acp::ModelInfo> = model_ids
        .iter()
        .map(|id| make_model_info(id))
        .collect();
    let state = acp::SessionModelState::new(
        acp::ModelId::new(std::sync::Arc::from(current_model_id)),
        models,
    );
    let raw = serde_json::value::to_raw_value(&state).unwrap();
    acp::ExtNotification::new("echo.agent/models/update", std::sync::Arc::from(raw))
}
/// `echo.agent/models/update` carrying a single reasoning-capable model whose
/// catalog-default effort is `default_effort` (what the broadcast reports
/// for every client — never the per-session selection).
pub(super) fn make_reasoning_models_update_notif(
    current_model_id: &str,
    default_effort: &str,
) -> acp::ExtNotification {
    let mut info = make_model_info(current_model_id);
    info.meta = serde_json::json!({
            "supportsReasoningEffort": true,
            "reasoningEffort": default_effort,
        })
        .as_object()
        .cloned();
    let state = acp::SessionModelState::new(
        acp::ModelId::new(std::sync::Arc::from(current_model_id)),
        vec![info],
    );
    let raw = serde_json::value::to_raw_value(&state).unwrap();
    acp::ExtNotification::new("echo.agent/models/update", std::sync::Arc::from(raw))
}
/// Seed a session's model catalog with the given ids and mark
/// `current_model_id` as the active one (must be in the list). Used by
/// the `ModelChanged` broadcast tests to set up a starting state that
/// the simulated remote/local switch then transitions away from.
pub(super) fn seed_models(agent: &mut AgentView, current: &str, available: &[&str]) {
    for id in available {
        let model_id = acp::ModelId::new(std::sync::Arc::from(*id));
        agent.session.models.available.insert(model_id.clone(), make_model_info(id));
    }
    agent.session.models.current = Some(
        acp::ModelId::new(std::sync::Arc::from(current)),
    );
}
pub(super) fn model_changed_ext(
    session_id: &str,
    model_id: &str,
    reasoning_effort: Option<&str>,
) -> acp::ExtNotification {
    let payload = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::ModelChanged {
            model_id: model_id.to_string(),
            reasoning_effort: reasoning_effort.map(String::from),
        },
        meta: None,
    };
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    acp::ExtNotification::new("echo.agent/session_notification", std::sync::Arc::from(raw))
}
pub(super) fn model_changed_ext_with_event(
    session_id: &str,
    model_id: &str,
    event_id: &str,
) -> acp::ExtNotification {
    let payload = SessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdate::ModelChanged {
            model_id: model_id.to_string(),
            reasoning_effort: None,
        },
        meta: Some(serde_json::json!({ "eventId": event_id })),
    };
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    acp::ExtNotification::new("echo.agent/session_notification", std::sync::Arc::from(raw))
}
pub(super) fn make_tool_call_update(title: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCallUpdate(
        acp::ToolCallUpdate::new(
            acp::ToolCallId::new("tc-1"),
            acp::ToolCallUpdateFields::new()
                .title(Some(title.to_string()))
                .status(Some(acp::ToolCallStatus::Completed)),
        ),
    )
}
pub(super) fn make_tool_call(title: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(acp::ToolCallId::new("tc-2"), title.to_string())
            .kind(acp::ToolKind::Other)
            .status(acp::ToolCallStatus::Pending)
            .content(vec![])
            .locations(vec![]),
    )
}
pub(super) fn make_current_mode_update(mode_id: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::CurrentModeUpdate(
        acp::CurrentModeUpdate::new(acp::SessionModeId::new(mode_id)),
    )
}
/// Helper: build an `echo.agent/mcp/init_progress` notification.
pub(super) fn make_mcp_init_progress_notif(
    total: u32,
    connected: u32,
) -> acp::ExtNotification {
    let raw = serde_json::value::to_raw_value(
            &serde_json::json!({
            "total": total,
            "connected": connected,
        }),
        )
        .unwrap();
    acp::ExtNotification::new("echo.agent/mcp/init_progress", std::sync::Arc::from(raw))
}
pub(super) fn make_mcps_modal_with_servers(
    servers: Vec<crate::views::mcps_modal::McpServerInfo>,
) -> crate::views::extensions_modal::ExtensionsModalState {
    use crate::views::extensions_modal::{
        ExtensionsModalState, ExtensionsTab, TabDataState,
    };
    let mut state = ExtensionsModalState::new(ExtensionsTab::McpServers);
    state.mcps_data = TabDataState::Loaded(servers);
    state
}
pub(super) fn seed_owner_agent_with_open_modal(app: &mut AppView) {
    use crate::views::mcps_modal::{McpServerDisplayStatus, McpServerInfo, McpWireSource};
    let owner = app.agents.get_mut(&AgentId(0)).expect("owner present");
    owner.extensions_modal = Some(
        make_mcps_modal_with_servers(
            vec![McpServerInfo {
            name: "alpha".into(),
            display_name: None,
            status: McpServerDisplayStatus::Initializing,
            tool_count: 0,
            auth_required: false,
            setup_required: false,
            setup: None,
            setup_values: std::collections::HashMap::new(),
            tools: Vec::new(),
            enabled: true,
            source: "local".into(),
            wire_source: McpWireSource::Local,
            plugin_name: None,
            is_managed_gateway: false,
        }],
        ),
    );
}
/// Build a `server_status` notification using the SHELL's canonical
/// `McpServerStatusPayload` so the test exercises the actual wire
/// type — not a synthesized json object that could drift from
/// the shell.
pub(super) fn make_server_status_notif(
    session_id: &str,
    name: &str,
    status: echo_agent_runtime::extensions::mcp::McpServerStatus,
    tools: Option<serde_json::Value>,
) -> acp::ExtNotification {
    use echo_agent_runtime::extensions::mcp::{
        McpServerSource, McpServerStatusPayload, McpServerStatusReason,
    };
    let payload = McpServerStatusPayload {
        session_id: session_id.to_string(),
        name: name.to_string(),
        source: McpServerSource::Local,
        status,
        reason: McpServerStatusReason::Initialized,
        detail: None,
        tools,
    };
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    acp::ExtNotification::new("echo.agent/mcp/server_status", std::sync::Arc::from(raw))
}
/// `mcp/servers_updated` real wire shape — `{ mcpServers: [...] }`
/// with NO `sessionId`. Regression guard: anything that tries to
/// extract a session id here must fail and fall through to the
/// broadcast path.
pub(super) fn make_servers_updated_notif() -> acp::ExtNotification {
    let payload = serde_json::json!({ "mcpServers": [] });
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    acp::ExtNotification::new("echo.agent/mcp/servers_updated", std::sync::Arc::from(raw))
}
/// Real post-handshake / auth-recovery wire shape:
/// `McpToolsChanged { sessionId, serverName, tools }`.
pub(super) fn make_tools_changed_notif_post_h2(
    session_id: &str,
) -> acp::ExtNotification {
    let payload = echo_agent_runtime::extensions::mcp::McpToolsChanged {
        session_id: session_id.to_string(),
        server_name: "echo_agent_com_linear".to_string(),
        tools: Vec::new(),
    };
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    acp::ExtNotification::new("echo.agent/mcp/tools_changed", std::sync::Arc::from(raw))
}
/// Legacy / forward-compat wire shape: older shells emit
/// `{ serverName, tools }` with NO sessionId. The pager must fall
/// back to active_view for this shape.
pub(super) fn make_tools_changed_notif_pre_h2() -> acp::ExtNotification {
    let payload = serde_json::json!({ "serverName": "echo_agent_com_linear", "tools": [] });
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    acp::ExtNotification::new("echo.agent/mcp/tools_changed", std::sync::Arc::from(raw))
}
/// Real `mcp_initialized` wire shape:
/// `{ sessionId, mcpToolCount, elapsedMs }`.
pub(super) fn make_mcp_initialized_notif(session_id: &str) -> acp::ExtNotification {
    let payload = serde_json::json!({
            "sessionId": session_id,
            "mcpToolCount": 12_u64,
            "elapsedMs": 250_u64,
        });
    let raw = serde_json::value::to_raw_value(&payload).unwrap();
    acp::ExtNotification::new("echo.agent/mcp_initialized", std::sync::Arc::from(raw))
}
/// Helper: `init_progress` notification carrying an explicit sessionId.
pub(super) fn make_mcp_init_progress_notif_for(
    total: u32,
    connected: u32,
    session_id: &str,
) -> acp::ExtNotification {
    let raw = serde_json::value::to_raw_value(
            &serde_json::json!({
            "total": total,
            "connected": connected,
            "sessionId": session_id,
        }),
        )
        .unwrap();
    acp::ExtNotification::new("echo.agent/mcp/init_progress", std::sync::Arc::from(raw))
}
/// Helper: `mcp_initialized` notification for a specific sessionId.
pub(super) fn make_mcp_initialized_notif_for(session_id: &str) -> acp::ExtNotification {
    let raw = serde_json::value::to_raw_value(
            &serde_json::json!({
            "sessionId": session_id,
            "mcpToolCount": 0,
            "elapsedMs": 0,
        }),
        )
        .unwrap();
    acp::ExtNotification::new("echo.agent/mcp_initialized", std::sync::Arc::from(raw))
}
mod permissions;
mod session_events;
mod follow_ups;
mod settings;
mod announcements;
mod scheduled_tasks;
mod queue_and_adoption;
mod plan_mode;
mod reconnect;
mod turn_completion;
mod interjection;
mod session_routing;
mod plugins;
mod subagents;
mod goals;
mod interactions;
mod background_tasks;
mod models;
mod mcp;
mod git_head;
mod version_mismatch;
