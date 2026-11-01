use super::support::*;
use super::*;
use echo_agent_tool_types::{
    KillTaskOutput, KillTaskResult, MultiTaskOutputResult, SubagentCompletedOutput,
    TaskOutputOutput, TaskOutputResult,
};
use echo_agent_tools::reminders::task_completion::consumed_completion_ids;
use echo_agent_tools::types::output::{BashOutput, TextOutput, ToolOutput};
fn input_with_origin(prompt_id: &str, origin: crate::session::PromptOrigin) -> InputItem {
    input_with_origin_rx(prompt_id, origin).0
}
fn task_completed_input(task_id: &str) -> InputItem {
    input_with_origin(
        &format!("task-completed-{task_id}"),
        crate::session::PromptOrigin::TaskCompleted {
            task_id: task_id.to_string(),
        },
    )
}
fn subagent_completed_input(subagent_id: &str) -> InputItem {
    input_with_origin(
        &format!("subagent-completed-{subagent_id}"),
        crate::session::PromptOrigin::SubagentCompleted {
            subagent_id: subagent_id.to_string(),
        },
    )
}
fn notification_drain_input(prompt_id: &str) -> InputItem {
    input_with_origin(prompt_id, crate::session::PromptOrigin::NotificationDrain)
}
fn goal_summary_input(prompt_id: &str) -> InputItem {
    input_with_origin(prompt_id, crate::session::PromptOrigin::GoalSummary)
}
fn user_input(prompt_id: &str) -> InputItem {
    input_with_origin(prompt_id, crate::session::PromptOrigin::User)
}
fn task_output_result(task_id: &str, status: &str) -> TaskOutputResult {
    TaskOutputResult {
        task_id: task_id.to_string(),
        command: "echo test".into(),
        status: status.to_string(),
        exit_code: Some(0),
        started: "2026-01-01T00:00:00Z".into(),
        ended: Some("2026-01-01T00:00:01Z".into()),
        duration_secs: 1.0,
        output: "test".into(),
        output_file: "/tmp/out.log".into(),
        truncated: false,
        truncation_hint: String::new(),
        raw_output_bytes: 4,
    }
}
fn bash_completed_notification(task_id: &str) -> PendingNotification {
    PendingNotification {
        prompt_id: format!("bash-completed-{task_id}"),
        prompt_blocks: vec![],
        priority: NotificationPriority::Later,
        source: NotificationSource::BashTaskCompleted {
            task_id: task_id.to_string(),
        },
    }
}
fn monitor_completed_notification(task_id: &str) -> PendingNotification {
    PendingNotification {
        prompt_id: format!("monitor-completed-{task_id}"),
        prompt_blocks: vec![],
        priority: NotificationPriority::Later,
        source: NotificationSource::MonitorCompleted {
            task_id: task_id.to_string(),
        },
    }
}
fn task_wake_admission(
    task_id: &str,
    source: NotificationSource,
) -> (
    crate::session::commands::TaskWakeAdmission,
    oneshot::Receiver<bool>,
) {
    let (respond_to, response_rx) = oneshot::channel();
    (
        crate::session::commands::TaskWakeAdmission {
            respond_to,
            fallback: crate::session::commands::TaskWakeFallback {
                prompt_id: format!("deferred-{task_id}"),
                prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(format!(
                    "completion {task_id}"
                )))],
                source,
            },
        },
        response_rx,
    )
}
fn monitor_event_notification(task_id: &str) -> PendingNotification {
    PendingNotification {
        prompt_id: format!("monitor-{task_id}"),
        prompt_blocks: vec![],
        priority: NotificationPriority::Next,
        source: NotificationSource::MonitorEvent {
            task_id: task_id.to_string(),
        },
    }
}
/// Monitor notifications in the idle drain collapse into ONE
/// `format_monitor_events` block (same shape as the mid-turn injection);
/// non-monitor notifications keep their raw blocks, `---`-separated.
#[test]
fn pending_notification_cap_keeps_newest_entries() {
    let mut state = State {
        running_task: None,
        pending_inputs: std::collections::VecDeque::new(),
        edit_holds: HashMap::new(),
        pending_notifications: Vec::new(),
        notifications_suppressed: true,
        rewindable: false,
        front_message_committed: false,
        nudges_used_this_session: 0,
    };
    for index in 0..(MAX_PENDING_NOTIFICATIONS + 3) {
        SessionActor::push_pending_notification(
            &mut state,
            bash_completed_notification(&format!("task-{index}")),
        );
    }
    assert_eq!(state.pending_notifications.len(), MAX_PENDING_NOTIFICATIONS);
    assert_eq!(state.pending_notifications[0].source.task_id(), "task-3");
    let newest = format!("task-{}", MAX_PENDING_NOTIFICATIONS + 2);
    assert_eq!(
        state.pending_notifications.last().unwrap().source.task_id(),
        newest
    );
}
#[tokio::test(flavor = "current_thread")]
async fn drain_batches_monitor_notifications_into_formatted_block() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = tokio::sync::mpsc::unbounded_channel::<
                echo_agent_acp::AcpClientMessage,
            >();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<
                PersistenceMsg,
            >();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx)
                .await;
            let monitor_notif = |task: &str, line: &str| PendingNotification {
                prompt_id: format!("monitor-{task}"),
                prompt_blocks: vec![agent_client_protocol::ContentBlock::Text(
                    agent_client_protocol::TextContent::new(format!(
                            "<monitor-event description=\"watch\" task_id=\"{task}\">\n{line}\n</monitor-event>"
                        )),
                )],
                priority: NotificationPriority::Next,
                source: NotificationSource::MonitorEvent {
                    task_id: task.to_string(),
                },
            };
            let mut bash = bash_completed_notification("bg-1");
            bash.prompt_blocks = vec![agent_client_protocol::ContentBlock::Text(
                agent_client_protocol::TextContent::new("Background task \"bg-1\" completed."),
            )];
            let mut state = actor.state.lock().await;
            let drained = SessionActor::drain_notifications_into_turn(
                &mut state,
                vec![
                    monitor_notif("mon-1", "tick 1"),
                    bash,
                    monitor_notif("mon-1", "tick 2"),
                ],
                "get_task_output",
            );
            assert!(drained);
            let item = state.pending_inputs.back().expect("drained turn queued");
            let text = item
                .prompt_blocks
                .iter()
                .filter_map(|b| match b {
                    agent_client_protocol::ContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                text.contains("2 monitor events from 1 monitor"),
                "monitor entries must collapse into one formatted batch: {text}"
            );
            assert!(
                text.contains(
                    "<monitor description=\"watch\" task_id=\"mon-1\">\n[1] tick 1\n[2] tick 2"
                ),
                "batch must group + label the ticks: {text}"
            );
            assert_eq!(
                text.matches("<monitor-event").count(),
                0,
                "raw per-event wrappers must not survive the drain: {text}"
            );
            assert!(
                text.contains("Background task \"bg-1\" completed."),
                "non-monitor notification keeps its raw block: {text}"
            );
            assert_eq!(
                text.matches("---").count(),
                1,
                "one separator between the batch and the bash block: {text}"
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn cancel_barrier_rejects_task_completion_wake_without_reporting_it() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            let reservations = actor
                .tool_context
                .task_completion_reservations
                .clone()
                .expect("completion reservations");
            reservations.reserve("bg-suppressed".to_string());
            actor.state.lock().await.notifications_suppressed = true;
            let gate = actor
                .tool_context
                .task_wake_suppressed
                .clone()
                .expect("task-wake gate");
            gate.set(true);
            let resources = actor
                .agent
                .borrow()
                .tool_bridge()
                .clone()
                .shared_resources()
                .await;
            {
                let mut resources = resources.lock().await;
                resources.insert(reservations.clone());
                resources.insert(gate.clone());
            }
            let origin = crate::session::PromptOrigin::TaskCompleted {
                task_id: "bg-suppressed".to_string(),
            };
            let (admission, response_rx) = task_wake_admission(
                "bg-suppressed",
                NotificationSource::BashTaskCompleted {
                    task_id: "bg-suppressed".to_string(),
                },
            );
            assert!(
                actor
                    .admit_task_completion_wake(&origin, admission)
                    .await
                    .is_none()
            );
            assert_eq!(response_rx.await, Ok(false));
            assert!(gate.get());
            let state = actor.state.lock().await;
            assert!(state.running_task.is_none());
            assert!(state.pending_inputs.is_empty());
            assert!(matches!(
                state.pending_notifications.as_slice(),
                [PendingNotification {
                    source: NotificationSource::BashTaskCompleted { task_id },
                    ..
                }] if task_id == "bg-suppressed"
            ));
            drop(state);
            assert!(reservations.contains("bg-suppressed"));
            let res = resources.lock().await;
            assert!(
                res.get::<echo_agent_tools::types::resources::State<
                    echo_agent_tools::reminders::task_completion::ReportedTaskCompletions,
                >>()
                .is_none(),
                "declined admission must not report before user re-engagement"
            );
            drop(res);
            let reminder = echo_agent_tools::reminders::TaskCompletionReminder;
            let reminders = echo_agent_tools::types::tool::Reminder::collect_reminders(
                &reminder,
                resources,
                &ToolOutput::Dynamic(serde_json::Value::Null.into()),
            )
            .await;
            assert!(reminders.is_empty());
            assert!(reservations.contains("bg-suppressed"));
            reservations.release("bg-suppressed");
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn closed_admission_ack_stores_fallback_before_prompt_rejection() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            let origin = crate::session::PromptOrigin::TaskCompleted {
                task_id: "mon-timeout".to_string(),
            };
            let (admission, response_rx) = task_wake_admission(
                "mon-timeout",
                NotificationSource::MonitorCompleted {
                    task_id: "mon-timeout".to_string(),
                },
            );
            drop(response_rx);
            assert!(
                actor
                    .admit_task_completion_wake(&origin, admission)
                    .await
                    .is_none()
            );
            let state = actor.state.lock().await;
            assert!(matches!(
                state.pending_notifications.as_slice(),
                [PendingNotification {
                    source: NotificationSource::MonitorCompleted { task_id },
                    ..
                }] if task_id == "mon-timeout"
            ));
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn non_task_prompt_is_not_subject_to_task_wake_barrier() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.state.lock().await.notifications_suppressed = true;
            let (admission, response_rx) = task_wake_admission(
                "sub-1",
                NotificationSource::BashTaskCompleted {
                    task_id: "sub-1".to_string(),
                },
            );
            assert!(
                actor
                    .admit_task_completion_wake(
                        &crate::session::PromptOrigin::SubagentCompleted {
                            subagent_id: "sub-1".to_string(),
                        },
                        admission,
                    )
                    .await
                    .is_some(),
                "subagent completion is outside terminal task-wake suppression scope"
            );
            assert_eq!(response_rx.await, Ok(true));
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn task_completion_wake_is_admitted_without_cancel_barrier() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = std::sync::Arc::new(
                create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await,
            );
            let origin = crate::session::PromptOrigin::TaskCompleted {
                task_id: "bg-normal".to_string(),
            };
            actor
                .tool_context
                .task_completion_reservations
                .as_ref()
                .expect("completion reservations")
                .reserve("bg-normal".to_string());
            let (admission, response_rx) = task_wake_admission(
                "bg-normal",
                NotificationSource::BashTaskCompleted {
                    task_id: "bg-normal".to_string(),
                },
            );
            let fallback = actor
                .admit_task_completion_wake(&origin, admission)
                .await
                .expect("normal task wake should be admitted");
            assert_eq!(response_rx.await, Ok(true));
            let (respond_to, _rx) = oneshot::channel();
            let _ = actor
                .queue_input(QueueInputRequest {
                    verbatim: true,
                    task_wake_fallback: Some(fallback),
                    ..queue_input_request(vec![], "task-completed-bg-normal", respond_to)
                })
                .await;
            let state = actor.state.lock().await;
            assert_eq!(state.pending_inputs.len(), 1);
            assert!(matches!(
                state
                    .pending_inputs
                    .front()
                    .map(|item| item.input_origin.as_prompt_origin()),
                Some(crate::session::PromptOrigin::TaskCompleted { task_id })
                    if task_id == "bg-normal"
            ));
            drop(state);
            let resources = actor
                .agent
                .borrow()
                .tool_bridge()
                .clone()
                .shared_resources()
                .await;
            assert!(
                resources
                    .lock()
                    .await
                    .get::<echo_agent_tools::types::resources::State<
                        echo_agent_tools::reminders::task_completion::ReportedTaskCompletions,
                    >>()
                    .is_none(),
                "queue acceptance alone must not mark the completion reported"
            );
            let actor_for_turn = actor.clone();
            let turn = tokio::task::spawn_local(async move {
                actor_for_turn
                    .handle_prompt(
                        "task-completed-bg-normal",
                        vec![acp::ContentBlock::Text(acp::TextContent::new("done"))],
                        PromptMode::Agent,
                        None,
                        None,
                        None,
                        None,
                        true,
                        false,
                        None,
                        None,
                        None,
                    )
                    .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if already_reported(&actor, "bg-normal").await {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("synthetic turn marked completion reported");
            turn.abort();
            assert!(
                already_reported(&actor, "bg-normal").await,
                "actual synthetic turn start must mark the completion reported"
            );
            assert!(
                actor
                    .tool_context
                    .task_completion_reservations
                    .as_ref()
                    .is_none_or(|ids| !ids.contains("bg-normal"))
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn disk_full_refusal_still_clears_task_completion_reservation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            tokio::task::spawn_local(async move {
                while let Some(msg) = persistence_rx.recv().await {
                    if let PersistenceMsg::ProbeWritable { respond_to } = msg {
                        let _ = respond_to
                            .send(Err(std::io::Error::from(std::io::ErrorKind::StorageFull)));
                    }
                }
            });
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.notifications.disk_full = tokio::sync::watch::channel(true).1;
            actor
                .tool_context
                .task_completion_reservations
                .as_ref()
                .expect("completion reservations")
                .reserve("bg-disk".to_string());
            let actor = std::sync::Arc::new(actor);
            let error = actor
                .handle_prompt(
                    "task-completed-bg-disk",
                    vec![acp::ContentBlock::Text(acp::TextContent::new("done"))],
                    PromptMode::Agent,
                    None,
                    None,
                    None,
                    None,
                    true,
                    false,
                    None,
                    None,
                    None,
                )
                .await
                .expect_err("latched disk-full must refuse the wake");
            assert_eq!(error.message, "No space left on device");
            assert!(
                already_reported(&actor, "bg-disk").await,
                "disk-full refusal must mark the completion reported"
            );
            assert!(
                actor
                    .tool_context
                    .task_completion_reservations
                    .as_ref()
                    .is_none_or(|ids| !ids.contains("bg-disk")),
                "disk-full refusal must release the completion reservation"
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn genuine_user_start_consumes_deferred_completions_without_notification_turn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = std::sync::Arc::new(
                create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await,
            );
            let body = echo_agent_tools::reminders::task_completion::format_monitor_completion(
                &echo_agent_tools::types::TaskSnapshot {
                    task_id: "mon-quiet".to_string(),
                    command: "tail -f quiet.log".to_string(),
                    display_command: Some("[monitor] quiet logs".to_string()),
                    cwd: String::new(),
                    start_time: std::time::SystemTime::now(),
                    end_time: Some(std::time::SystemTime::now()),
                    output: String::new(),
                    output_file: std::path::PathBuf::new(),
                    truncated: false,
                    exit_code: Some(0),
                    signal: None,
                    completed: true,
                    kind: echo_agent_tools::computer::types::TaskKind::Monitor,
                    block_waited: false,
                    explicitly_killed: false,
                    kill_result_delivered: false,
                    owner_session_id: None,
                    description: None,
                    is_backgrounded: false,
                    output_total_bytes: 0,
                },
                Some("get_command_or_subagent_output"),
            );
            {
                let mut state = actor.state.lock().await;
                state.notifications_suppressed = true;
                state
                    .pending_notifications
                    .push(monitor_event_notification("mon-quiet"));
                let mut monitor_completion = monitor_completed_notification("mon-quiet");
                monitor_completion.prompt_blocks =
                    vec![acp::ContentBlock::Text(acp::TextContent::new(body))];
                state.pending_notifications.push(monitor_completion);
                let mut bash_completion = bash_completed_notification("bash-deferred");
                bash_completion.prompt_blocks = vec![acp::ContentBlock::Text(
                    acp::TextContent::new("Background task bash-deferred completed."),
                )];
                state.pending_notifications.push(bash_completion);
            }
            let reservations = actor
                .tool_context
                .task_completion_reservations
                .as_ref()
                .expect("completion reservations");
            reservations.reserve("mon-quiet".to_string());
            reservations.reserve("bash-deferred".to_string());
            actor
                .tool_context
                .task_wake_suppressed
                .as_ref()
                .expect("task-wake gate")
                .set(true);
            let actor_for_turn = actor.clone();
            let turn = tokio::task::spawn_local(async move {
                actor_for_turn
                    .handle_prompt(
                        "user-deferred-completions",
                        vec![acp::ContentBlock::Text(acp::TextContent::new("continue"))],
                        PromptMode::Agent,
                        None,
                        None,
                        None,
                        None,
                        false,
                        false,
                        None,
                        None,
                        None,
                    )
                    .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if actor.state.lock().await.pending_notifications.is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("user turn consumed deferred completions");
            turn.abort();
            tokio::task::yield_now().await;
            let state = actor.state.lock().await;
            assert!(state.notifications_suppressed);
            assert!(state.pending_notifications.is_empty());
            assert!(state.pending_inputs.iter().all(|input| !matches!(
                input.input_origin.as_prompt_origin(),
                crate::session::PromptOrigin::NotificationDrain
            )));
            drop(state);
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            SessionActor::maybe_drain_notifications(actor.clone(), completion_tx).await;
            let state = actor.state.lock().await;
            assert!(state.pending_inputs.iter().all(|input| !matches!(
                input.input_origin.as_prompt_origin(),
                crate::session::PromptOrigin::NotificationDrain
            )));
            drop(state);
            assert!(!reservations.contains("mon-quiet"));
            assert!(!reservations.contains("bash-deferred"));
            assert!(
                !actor
                    .tool_context
                    .task_wake_suppressed
                    .as_ref()
                    .expect("task-wake gate")
                    .get()
            );
            let conversation = actor.chat_state_handle.get_conversation().await;
            let text = conversation
                .iter()
                .map(|item| item.text_content())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("Monitor \"mon-quiet\" ended"));
            assert!(text.contains("Background task bash-deferred completed."));
            assert!(!text.contains("<monitor-event"));
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn accepted_reservation_survives_user_start() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = std::sync::Arc::new(
                create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await,
            );
            let reservations = actor
                .tool_context
                .task_completion_reservations
                .as_ref()
                .expect("completion reservations");
            reservations.reserve("accepted-race".to_string());
            actor
                .tool_context
                .task_wake_suppressed
                .as_ref()
                .expect("task-wake gate")
                .set(true);
            let actor_for_turn = actor.clone();
            let turn = tokio::task::spawn_local(async move {
                actor_for_turn
                    .handle_prompt(
                        "user-accepted-race",
                        vec![acp::ContentBlock::Text(acp::TextContent::new("continue"))],
                        PromptMode::Agent,
                        None,
                        None,
                        None,
                        None,
                        false,
                        false,
                        None,
                        None,
                        None,
                    )
                    .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if actor
                        .tool_context
                        .task_wake_suppressed
                        .as_ref()
                        .is_none_or(|gate| !gate.get())
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("user turn started");
            assert!(reservations.contains("accepted-race"));
            turn.abort();
            reservations.release("accepted-race");
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn same_id_bash_completion_does_not_suppress_monitor_event() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) = tokio::sync::mpsc::unbounded_channel::<
                echo_agent_acp::AcpClientMessage,
            >();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<
                PersistenceMsg,
            >();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx)
                .await;
            let monitor = PendingNotification {
                prompt_id: "monitor-shared".to_string(),
                prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
                    "<monitor-event description=\"watch\" task_id=\"shared\">\nstdout\n</monitor-event>",
                ))],
                priority: NotificationPriority::Next,
                source: NotificationSource::MonitorEvent {
                    task_id: "shared".to_string(),
                },
            };
            let mut bash = bash_completed_notification("shared");
            bash.prompt_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(
                "Background task shared completed.",
            ))];
            let mut state = actor.state.lock().await;
            SessionActor::drain_notifications_into_turn(
                &mut state,
                vec![monitor, bash],
                "get_command_or_subagent_output",
            );
            let text = state
                .pending_inputs
                .back()
                .expect("drained turn")
                .prompt_blocks
                .iter()
                .filter_map(|block| match block {
                    acp::ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("<monitor-event task_id=\"shared\">"));
            assert!(text.contains("Background task shared completed."));
        })
        .await;
}
/// Fix 1, TaskOutput(completed) — the matching pending `task-completed-{id}`
/// input must be dropped; any non-matching synthetic prompt must survive.
#[tokio::test(flavor = "current_thread")]
async fn task_output_completed_drops_matching_pending_input() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-target"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-other"));
                state.pending_inputs.push_back(user_input("user-real"));
            }
            let output = ToolOutput::TaskOutput(TaskOutputOutput::Result(task_output_result(
                "bg-target",
                "completed",
            )));
            let consumed = consumed_completion_ids(&output);
            assert_eq!(consumed, vec!["bg-target"]);
            actor
                .drop_pending_items_for_consumed_completions(&consumed)
                .await;
            let state = actor.state.lock().await;
            let remaining_ids: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                remaining_ids,
                vec!["task-completed-bg-other", "user-real"],
                "only the matching synthetic input should be dropped"
            );
        })
        .await;
}
/// The sweep must never drop the running turn's own slot. An auto-wake turn
/// polls its own task's output, so the consumed id matches the front
/// `task-completed-{id}` entry — which IS the in-flight turn
/// (`maybe_start_running_task` promotes the front without popping it).
/// Deleting it shifts whatever is queued behind (a real user prompt) to
/// index 0, which the next interactive cancel resolves as Cancelled —
/// destroying the user's message. Queued NON-running synthetics must still
/// be dropped.
#[tokio::test(flavor = "current_thread")]
async fn sweep_never_drops_running_turns_own_slot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let mut state = actor.state.lock().await;
                state.running_task = Some(running_task_stub("task-completed-bg-target"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-target"));
                state.pending_inputs.push_back(user_input("user-real"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-other"));
            }
            actor
                .drop_pending_items_for_consumed_completions(&["bg-target", "bg-other"])
                .await;
            let state = actor.state.lock().await;
            let remaining_ids: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                remaining_ids,
                vec!["task-completed-bg-target", "user-real"],
                "the running turn's own front slot must survive the sweep; \
                 only the queued non-running synthetic is dropped"
            );
        })
        .await;
}
/// `queue_input`'s user-priority preempt is the second sweep over
/// `pending_inputs` and needs the same guard: a user prompt arriving WHILE a
/// synthetic auto-wake turn is running must not delete the running turn's own
/// front slot (or the user prompt lands at index 0 and the next interactive
/// cancel destroys it). Queued non-running synthetics are still preempted.
#[tokio::test(flavor = "current_thread")]
async fn user_prompt_preempt_keeps_running_synthetic_slot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            let reservations = actor
                .tool_context
                .task_completion_reservations
                .as_ref()
                .expect("completion reservations");
            reservations.reserve("bg-other".to_string());
            {
                let mut state = actor.state.lock().await;
                state.running_task = Some(running_task_stub("task-completed-bg-target"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-target"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-other"));
            }
            let (respond_to, _rx) = oneshot::channel();
            let _ = actor
                .queue_input(queue_input_request(vec![], "user-clarify", respond_to))
                .await;
            let state = actor.state.lock().await;
            let remaining_ids: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                remaining_ids,
                vec!["task-completed-bg-target", "user-clarify"],
                "the running synthetic turn's slot must survive the user-priority \
                 preempt; only the queued non-running synthetic is dropped"
            );
            assert!(
                !reservations.contains("bg-other"),
                "ordinary user-priority preemption releases ownership immediately"
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn await_text_completed_drops_matching_pending_input() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-target"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-other"));
                state.pending_inputs.push_back(user_input("user-real"));
            }
            let output = ToolOutput::Text(TextOutput {
                text: "Task completed in 100ms with exit code: 0.".into(),
                consumed_completion_task_id: Some("bg-target".into()),
            });
            let consumed = consumed_completion_ids(&output);
            assert_eq!(consumed, vec!["bg-target"]);
            actor
                .drop_pending_items_for_consumed_completions(&consumed)
                .await;
            let state = actor.state.lock().await;
            let remaining_ids: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                remaining_ids,
                vec!["task-completed-bg-other", "user-real"],
                "only the matching synthetic input should be dropped"
            );
        })
        .await;
}
/// Fix 1, KillTask — same shape as get_task_output(completed): the
/// matching synthetic prompt must be dropped.
#[tokio::test(flavor = "current_thread")]
async fn kill_task_drops_matching_pending_input() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-killed"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-alive"));
            }
            let output = ToolOutput::KillTask(KillTaskOutput::Result(KillTaskResult {
                task_id: "bg-killed".into(),
                outcome: "killed".into(),
                message: "Task was terminated successfully".into(),
            }));
            let consumed = consumed_completion_ids(&output);
            assert_eq!(consumed, vec!["bg-killed"]);
            actor
                .drop_pending_items_for_consumed_completions(&consumed)
                .await;
            let state = actor.state.lock().await;
            let remaining_ids: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(remaining_ids, vec!["task-completed-bg-alive"]);
        })
        .await;
}
/// Fix 1, SubagentCompleted — the matching `subagent-completed-{id}`
/// input must be dropped. ALSO seeds a matching `bash_completed`
/// pending notification and asserts it is filtered out so the
/// notification-sweep half of the helper is covered for the subagent
/// shape too (not only in `sweep_clears_matching_pending_notifications`).
#[tokio::test(flavor = "current_thread")]
async fn subagent_completed_drops_matching_pending_input() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(subagent_completed_input("sub-target"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-survives"));
                state
                    .pending_notifications
                    .push(bash_completed_notification("sub-target"));
                state
                    .pending_notifications
                    .push(bash_completed_notification("bg-survives"));
            }
            let output = ToolOutput::SubagentCompleted(SubagentCompletedOutput {
                output: "done".into(),
                subagent_id: "sub-target".into(),
                subagent_type: "general-purpose".into(),
                tool_calls: 1,
                turns: 1,
                duration_ms: 500,
                worktree_path: None,
                persona: None,
                resume_from_hint: "sub-target".into(),
                persona_hint: None,
            });
            let consumed = consumed_completion_ids(&output);
            assert_eq!(consumed, vec!["sub-target"]);
            actor
                .drop_pending_items_for_consumed_completions(&consumed)
                .await;
            let state = actor.state.lock().await;
            let remaining_ids: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(remaining_ids, vec!["task-completed-bg-survives"]);
            let remaining_notif: Vec<&str> = state
                .pending_notifications
                .iter()
                .map(|n| n.source.task_id())
                .collect();
            assert_eq!(
                remaining_notif,
                vec!["bg-survives"],
                "pending notification for the consumed subagent id must be dropped"
            );
        })
        .await;
}
/// Fix 1, MultiResult — only the `status == "completed"` task_ids
/// should be dropped from `pending_inputs`. Running ones are skipped.
#[tokio::test(flavor = "current_thread")]
async fn multi_task_output_drops_each_completed_id() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-done-1"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-done-2"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-running"));
            }
            let output =
                ToolOutput::TaskOutput(TaskOutputOutput::MultiResult(MultiTaskOutputResult {
                    mode: "all".into(),
                    results: vec![
                        task_output_result("bg-done-1", "completed"),
                        task_output_result("bg-done-2", "completed"),
                        task_output_result("bg-running", "running"),
                    ],
                    summary: String::new(),
                }));
            let consumed = consumed_completion_ids(&output);
            assert_eq!(consumed, vec!["bg-done-1", "bg-done-2"]);
            actor
                .drop_pending_items_for_consumed_completions(&consumed)
                .await;
            let state = actor.state.lock().await;
            let remaining_ids: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(remaining_ids, vec!["task-completed-bg-running"]);
        })
        .await;
}
/// Fix 1, status != "completed" — must NOT drop any inputs. A "running"
/// result is just a poll snapshot, not a consumption of the completion.
#[tokio::test(flavor = "current_thread")]
async fn task_output_running_does_not_drop_pending_input() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-running"));
            }
            let output = ToolOutput::TaskOutput(TaskOutputOutput::Result(task_output_result(
                "bg-running",
                "running",
            )));
            let consumed = consumed_completion_ids(&output);
            assert!(
                consumed.is_empty(),
                "running status must not yield a consumed completion id"
            );
            actor
                .drop_pending_items_for_consumed_completions(&consumed)
                .await;
            let state = actor.state.lock().await;
            assert_eq!(
                state.pending_inputs.len(),
                1,
                "running status must NOT drop the queued synthetic prompt"
            );
        })
        .await;
}
/// Negative-case coverage for the exhaustive match in
/// `consumed_completion_ids`. Each of these tool outputs must
/// yield an empty list (no consumption surface). The compiler
/// already enforces exhaustiveness via the match; these tests
/// pin the *semantics* of the no-op arms (so a future contributor
/// who adds a real id to one of these arms breaks the test).
#[tokio::test(flavor = "current_thread")]
async fn task_not_found_does_not_consume() {
    let out = ToolOutput::TaskOutput(TaskOutputOutput::TaskNotFound("missing".into()));
    assert!(consumed_completion_ids(&out).is_empty());
}
#[tokio::test(flavor = "current_thread")]
async fn kill_task_not_found_does_not_consume() {
    let out = ToolOutput::KillTask(KillTaskOutput::TaskNotFound("missing".into()));
    assert!(consumed_completion_ids(&out).is_empty());
}
#[tokio::test(flavor = "current_thread")]
async fn unrelated_tool_output_does_not_consume() {
    let bash = BashOutput {
        output: b"hi".to_vec(),
        output_for_prompt: "hi".into(),
        exit_code: 0,
        command: "echo hi".into(),
        truncated: false,
        signal: None,
        timed_out: false,
        description: None,
        current_dir: "/tmp".into(),
        output_file: String::new(),
        total_bytes: 2,
        output_delta: None,
        was_bare_echo: false,
    };
    let out = ToolOutput::Bash(bash);
    assert!(consumed_completion_ids(&out).is_empty());
}
/// Fix 1, pending notifications — same task_id in `pending_notifications`
/// must also be cleared by the sweep (both BashTaskCompleted and
/// MonitorEvent shapes).
#[tokio::test(flavor = "current_thread")]
async fn sweep_clears_matching_pending_notifications() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_notifications
                    .push(bash_completed_notification("bg-A"));
                state
                    .pending_notifications
                    .push(monitor_event_notification("bg-A"));
                state
                    .pending_notifications
                    .push(bash_completed_notification("bg-B"));
            }
            actor
                .drop_pending_items_for_consumed_completions(&["bg-A"])
                .await;
            let state = actor.state.lock().await;
            let remaining: Vec<&str> = state
                .pending_notifications
                .iter()
                .map(|n| n.source.task_id())
                .collect();
            assert_eq!(
                remaining,
                vec!["bg-B"],
                "all notifications with the consumed task_id must be cleared \
                     (including MonitorEvent shapes — by design, since the model just \
                     learned the task is done so any pending monitor stdout is stale)"
            );
        })
        .await;
}
/// Fix 4 — shutdown drain must keep real user inputs and drop every
/// variant for which `PromptOrigin::is_synthetic()` returns true. ALL
/// `pending_notifications` are cleared unconditionally.
#[tokio::test(flavor = "current_thread")]
async fn shutdown_drops_pending_synthetic_inputs() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_input("user-1"));
                state.pending_inputs.push_back(task_completed_input("bg-1"));
                state.pending_inputs.push_back(user_input("user-2"));
                state
                    .pending_inputs
                    .push_back(subagent_completed_input("sub-1"));
                state
                    .pending_inputs
                    .push_back(notification_drain_input("notifications-019e0000"));
                state
                    .pending_inputs
                    .push_back(goal_summary_input("goal-summary-019e2d3e"));
                state
                    .pending_notifications
                    .push(bash_completed_notification("bg-2"));
                state
                    .pending_notifications
                    .push(monitor_event_notification("mon-1"));
            }
            actor.drop_pending_synthetic_items().await;
            let state = actor.state.lock().await;
            let remaining_ids: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                remaining_ids,
                vec!["user-1", "user-2"],
                "real user inputs must be preserved; every synthetic origin \
                     (TaskCompleted, SubagentCompleted, NotificationDrain, GoalSummary) \
                     must be dropped"
            );
            assert!(
                state.pending_notifications.is_empty(),
                "all pending notifications must be cleared on shutdown"
            );
        })
        .await;
}
/// Integration test for the Fix 1 wiring — drives the actual
/// `handle_bridge_tool_success` call site rather than calling the
/// helper directly. A regression that removes or moves the call
/// from `handle_bridge_tool_success` will be caught here even
/// though the helper unit-tests still pass. Mirrors the call shape
/// used by `execute_tool_calls`.
#[tokio::test(flavor = "current_thread")]
async fn handle_bridge_tool_success_runs_consumed_completion_sweep() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-foo"));
                state
                    .pending_inputs
                    .push_back(task_completed_input("bg-other"));
            }
            let output = ToolOutput::TaskOutput(TaskOutputOutput::Result(task_output_result(
                "bg-foo",
                "completed",
            )));
            let result = ToolRunResult {
                output,
                prompt_text: "ok".into(),
                effective_tool_name: None,
            };
            let parsed_args = serde_json::json!({});
            let _ = actor
                .handle_bridge_tool_success(
                    &acp::ToolCallId::new("tc-1"),
                    "tc-1",
                    "get_task_output",
                    "get_task_output",
                    DrainedToolSuccess::new(result),
                    0,
                    "test-model",
                    &parsed_args,
                )
                .await;
            let state = actor.state.lock().await;
            let remaining_ids: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                remaining_ids,
                vec!["task-completed-bg-other"],
                "handle_bridge_tool_success must run the Fix 1 sweep — the matching \
                     synthetic prompt for bg-foo should be gone"
            );
        })
        .await;
}
/// Helper: returns `true` iff `task_id` was already marked reported in the
/// tool layer's `ReportedTaskCompletions` (so the per-tool-call
/// `TaskCompletionReminder` won't resurface it). Mirrors the resource access
/// in `SessionActor::mark_completions_reported`.
async fn already_reported(actor: &SessionActor, task_id: &str) -> bool {
    use echo_agent_tools::reminders::task_completion::ReportedTaskCompletions;
    use echo_agent_tools::types::resources::State;
    let bridge = actor.agent.borrow().tool_bridge().clone();
    let resources = bridge.shared_resources().await;
    let mut res = resources.lock().await;
    let reported = res.get_or_default::<State<ReportedTaskCompletions>>();
    !reported.mark_reported(task_id)
}
/// Pure decision: a goal-turn-origin task is dropped even when the blanket
/// goal Active/Complete gate is OFF (status Blocked / paused / None) — the
/// exact bug. Non-origin notifications survive.
#[tokio::test(flavor = "current_thread")]
async fn split_drops_goal_turn_origin_when_blanket_gate_off() {
    let mut goal_turn = std::collections::HashSet::new();
    goal_turn.insert("bg-goal".to_string());
    let notifications = vec![
        bash_completed_notification("bg-goal"),
        bash_completed_notification("bg-user"),
        monitor_event_notification("bg-goal"),
    ];
    let (surface, dropped) = SessionActor::split_goal_suppressed(false, &goal_turn, notifications);
    assert_eq!(
        dropped, 2,
        "both goal-origin entries (bash + monitor) dropped"
    );
    let surfaced: Vec<&str> = surface.iter().map(|n| n.source.task_id()).collect();
    assert_eq!(
        surfaced,
        vec!["bg-user"],
        "only the non-goal-origin completion survives"
    );
}
/// No goal involved (empty origin set, blanket gate off) — nothing is
/// dropped, so normal background-task completions still surface. Guards
/// against an over-suppression regression.
#[tokio::test(flavor = "current_thread")]
async fn split_surfaces_normal_completions_with_no_goal() {
    let goal_turn = std::collections::HashSet::new();
    let notifications = vec![
        bash_completed_notification("bg-1"),
        bash_completed_notification("bg-2"),
    ];
    let (surface, dropped) = SessionActor::split_goal_suppressed(false, &goal_turn, notifications);
    assert_eq!(dropped, 0, "no goal => no suppression");
    let surfaced: Vec<&str> = surface.iter().map(|n| n.source.task_id()).collect();
    assert_eq!(surfaced, vec!["bg-1", "bg-2"]);
}
/// Blanket gate ON (goal Active/Complete) drops everything — the existing
/// behavior is preserved.
#[tokio::test(flavor = "current_thread")]
async fn split_blanket_gate_drops_all() {
    let goal_turn = std::collections::HashSet::new();
    let notifications = vec![
        bash_completed_notification("bg-1"),
        monitor_event_notification("mon-1"),
    ];
    let (surface, dropped) = SessionActor::split_goal_suppressed(true, &goal_turn, notifications);
    assert!(surface.is_empty(), "blanket gate surfaces nothing");
    assert_eq!(dropped, 2);
}
/// End-to-end drain: a goal-turn-origin completion is DROPPED at idle drain
/// even though the goal status is `None` (goal cleared / never Active), and
/// it is still marked reported so it can't resurface via the per-tool-call
/// reminder path.
#[tokio::test(flavor = "current_thread")]
async fn drain_drops_goal_turn_origin_when_status_none_and_marks_reported() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = std::sync::Arc::new(
                create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await,
            );
            set_goal_harness_for_tests(&actor);
            actor
                .goal_turn_task_ids
                .lock()
                .insert("bg-goal".to_string());
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_notifications
                    .push(bash_completed_notification("bg-goal"));
            }
            let (completion_tx, _completion_rx) =
                tokio::sync::mpsc::unbounded_channel::<(String, PromptTurnResult)>();
            std::sync::Arc::clone(&actor)
                .maybe_drain_notifications(completion_tx)
                .await;
            {
                let state = actor.state.lock().await;
                assert!(
                    state.pending_notifications.is_empty(),
                    "the notification must be taken from the queue"
                );
                assert!(
                    state.running_task.is_none(),
                    "a goal-turn-origin completion must be DROPPED, not surfaced as a turn"
                );
            }
            assert!(
                already_reported(&actor, "bg-goal").await,
                "the dropped completion must be marked reported so it can't resurface"
            );
        })
        .await;
}
/// S-1 regression: a harness verifier subagent's reparented server, recorded
/// via the `RecordGoalTurnTaskIds` path (`record_reparented_goal_turn_task_ids`),
/// is suppressed even when the goal has already flipped to Blocked/None by the
/// time the reparent command lands — i.e. the case-(b) gate is the stable
/// harness flag, not the racy `Active` status. Without this, a final-round
/// skeptic's leftover server would still storm the idle parent.
#[tokio::test(flavor = "current_thread")]
async fn reparented_harness_subagent_task_suppressed_when_status_not_active() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = std::sync::Arc::new(
                create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await,
            );
            set_goal_harness_for_tests(&actor);
            actor.record_goal_turn_task_ids(["bg-skeptic".to_string()]);
            assert!(
                !actor.goal_turn_task_ids.lock().contains("bg-skeptic"),
                "the active-goal record path is correctly a no-op when not Active"
            );
            actor.record_reparented_goal_turn_task_ids(["bg-skeptic".to_string()]);
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_notifications
                    .push(bash_completed_notification("bg-skeptic"));
            }
            let (completion_tx, _completion_rx) =
                tokio::sync::mpsc::unbounded_channel::<(String, PromptTurnResult)>();
            std::sync::Arc::clone(&actor)
                .maybe_drain_notifications(completion_tx)
                .await;
            let state = actor.state.lock().await;
            assert!(
                state.pending_notifications.is_empty(),
                "the reparented harness server's completion must be taken"
            );
            assert!(
                state.running_task.is_none(),
                "a final-round skeptic's leftover server must be DROPPED even at status None"
            );
        })
        .await;
}
/// The reparent record path is gated on the (stable) goal harness flag, so it
/// is a no-op in a non-goal session — guards against over-suppression of a
/// normal subagent's reparented tasks outside any goal.
#[tokio::test(flavor = "current_thread")]
async fn reparented_record_is_noop_without_goal_harness() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.record_reparented_goal_turn_task_ids(["bg-user".to_string()]);
            assert!(
                actor.goal_turn_task_ids.lock().is_empty(),
                "no goal harness => reparented ids are not recorded (no over-suppression)"
            );
        })
        .await;
}
/// Regression: the between-turn completion drain must suppress subagent
/// completions already delivered to the model via auto-wake synthetic
/// prompts. Without completion reservations feeding `suppress_ids`, the same
/// completion is reported twice — once as the auto-wake "Background subagent
/// … completed" prompt and again as the "While you were idle, N background
/// subagent(s) completed" reminder.
#[tokio::test(flavor = "current_thread")]
async fn between_turn_drain_suppresses_reserved_subagents() {
    use echo_agent_tools::implementations::echo_agent_build::task::types::{
        SubagentCompletionSummary, SubagentEvent,
    };
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor
                .tool_context
                .task_completion_reservations
                .as_ref()
                .expect("completion reservations")
                .reserve("sa-autowake".to_string());
            let captured: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
            actor.tool_context.subagent_event_tx = Some(tx);
            let captured_task = std::sync::Arc::clone(&captured);
            tokio::task::spawn_local(async move {
                while let Some(ev) = rx.recv().await {
                    if let SubagentEvent::Completions(req) = ev {
                        *captured_task.lock().unwrap() = req.suppress_ids.clone();
                        let mk = |id: &str| SubagentCompletionSummary {
                            subagent_id: id.into(),
                            subagent_type: "general-purpose".into(),
                            description: format!("desc {id}"),
                            success: true,
                            duration_ms: 1000,
                            tool_calls: 3,
                            turns: 1,
                            output: std::sync::Arc::from("done"),
                        };
                        let mut completions = vec![mk("sa-autowake"), mk("sa-fresh")];
                        completions.retain(|c| !req.suppress_ids.contains(&c.subagent_id));
                        let _ = req.respond_to.send(completions);
                    }
                }
            });
            actor.drain_between_turn_completions().await;
            let suppress = captured.lock().unwrap().clone();
            assert!(
                suppress.contains(&"sa-autowake".to_string()),
                "between-turn drain must pass reserved ids as suppress_ids: \
                 {suppress:?}",
            );
            let conversation = actor.chat_state_handle.get_conversation().await;
            let texts: String = conversation
                .iter()
                .map(|i| i.text_content())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                texts.contains("While you were idle") && texts.contains("sa-fresh"),
                "between-turn drain should surface the fresh completion: {texts}",
            );
            assert!(
                !texts.contains("sa-autowake"),
                "reserved completion must NOT be re-surfaced: {texts}",
            );
            assert!(
                actor
                    .tool_context
                    .task_completion_reservations
                    .as_ref()
                    .is_some_and(|ids| ids.contains("sa-autowake"))
            );
        })
        .await;
}
/// `set_goal_loop_active_resource` — the single chokepoint — must
/// mirror the active flag into `tool_context.goal_loop_active_gate`, the shared
/// `Arc` the notification bridge (bash auto-wake) and subagent spawn contexts
/// read. Both the `true` set and the `false` reset funnel through this method,
/// so this also covers the reset paths. Deleting the `store` line (or cloning
/// the wrong Arc into the bridge) would break production suppression silently.
#[tokio::test(flavor = "current_thread")]
async fn set_goal_loop_active_resource_mirrors_into_gate() {
    use std::sync::atomic::Ordering::Relaxed;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            assert!(!actor.tool_context.goal_loop_active_gate.load(Relaxed));
            actor.set_goal_loop_active_resource(true).await;
            assert!(
                actor.tool_context.goal_loop_active_gate.load(Relaxed),
                "set(true) must mirror into the shared gate"
            );
            actor.set_goal_loop_active_resource(false).await;
            assert!(
                !actor.tool_context.goal_loop_active_gate.load(Relaxed),
                "set(false) must clear the shared gate"
            );
        })
        .await;
}
/// Minimal terminal backend that reports a fixed task list, so the bash arm of
/// the between-turn drain (`drain_between_turn_bash_completions` → `list_tasks`)
/// can be exercised without running a real background command.
#[derive(Debug)]
pub(super) struct OneTaskTerminal {
    pub(super) tasks: Vec<echo_agent_tools::computer::types::TaskSnapshot>,
}
#[async_trait::async_trait]
impl echo_agent_tools::computer::types::TerminalBackend for OneTaskTerminal {
    async fn run(
        &self,
        _: echo_agent_tools::computer::types::TerminalRunRequest,
    ) -> Result<
        echo_agent_tools::computer::types::TerminalRunResult,
        echo_agent_tools::computer::types::ComputerError,
    > {
        unimplemented!()
    }
    async fn run_background(
        &self,
        _: echo_agent_tools::computer::types::TerminalRunRequest,
    ) -> Result<
        echo_agent_tools::computer::types::BackgroundHandle,
        echo_agent_tools::computer::types::ComputerError,
    > {
        unimplemented!()
    }
    async fn get_task(&self, _: &str) -> Option<echo_agent_tools::computer::types::TaskSnapshot> {
        None
    }
    async fn kill_task(&self, _: &str) -> echo_agent_tools::computer::types::KillOutcome {
        echo_agent_tools::computer::types::KillOutcome::NotFound
    }
    async fn wait_for_completion(
        &self,
        _: &str,
        _: Option<std::time::Duration>,
    ) -> Option<echo_agent_tools::computer::types::TaskSnapshot> {
        None
    }
    async fn list_tasks(&self) -> Vec<echo_agent_tools::computer::types::TaskSnapshot> {
        self.tasks.clone()
    }
}
pub(super) fn completed_bash_task(id: &str) -> echo_agent_tools::computer::types::TaskSnapshot {
    echo_agent_tools::computer::types::TaskSnapshot {
        task_id: id.into(),
        command: "echo done".into(),
        display_command: None,
        cwd: String::new(),
        start_time: std::time::SystemTime::now(),
        end_time: Some(std::time::SystemTime::now()),
        output: String::new(),
        output_file: std::path::PathBuf::new(),
        truncated: false,
        exit_code: Some(0),
        signal: None,
        completed: true,
        kind: Default::default(),
        block_waited: false,
        explicitly_killed: false,
        kill_result_delivered: false,
        owner_session_id: None,
        description: None,
        is_backgrounded: false,
        output_total_bytes: 0,
    }
}
/// Real-actor coverage for the `SessionCommand::IsBusy` predicate
/// (`state_is_busy`) — exercises the production computation the leader's
/// idle-unload decision depends on, rather than the test fake actor.
#[tokio::test(flavor = "current_thread")]
async fn state_is_busy_reflects_queued_inputs() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            {
                let state = actor.state.lock().await;
                assert!(
                    !state_is_busy(&state),
                    "an idle actor (no turn, empty queue) must report not busy"
                );
            }
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_input("queued-1"));
                assert!(
                    state_is_busy(&state),
                    "a non-empty pending_inputs queue must report busy"
                );
            }
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.clear();
                assert!(
                    !state_is_busy(&state),
                    "clearing the queue must return to not busy"
                );
            }
        })
        .await;
}
