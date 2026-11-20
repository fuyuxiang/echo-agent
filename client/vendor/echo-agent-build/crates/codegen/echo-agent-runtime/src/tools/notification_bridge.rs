//! Notification bridge: translates `echo-agent-tools` `ToolNotification` events
//! into `echo-agent-runtime`'s native systems (ACP gateway, hunk tracker, file state tracker).
use crate::session::commands::SessionCommand;
use crate::session::commands::{NotificationPriority, NotificationSource};
use crate::session::persistence::{DurableAppendError, PersistenceHandle, PersistenceMsg};
use crate::tools::task_completed_frame;
use agent_client_protocol::{self as acp, Client as _};
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
use echo_agent_hunk_tracker::HunkTrackerHandle;
use echo_agent_tools::notification::types::{ToolNotification, ToolNotificationHandle};
use echo_agent_tools::types::output::{BashOutput, ToolOutput};
use echo_agent_workspace::session::file_state::FileStateTracker;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex as TokioMutex, mpsc};
const TASK_WAKE_ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
/// Configuration for the notification bridge.
pub(crate) struct NotificationBridgeConfig {
    /// ACP gateway for sending streaming updates to TUI
    pub gateway: GatewaySender,
    /// ACP session ID
    pub session_id: acp::SessionId,
    /// Hunk tracker for recording agent writes
    pub hunk_tracker_handle: HunkTrackerHandle,
    /// File state tracker for rewind functionality
    pub file_state_tracker: Arc<FileStateTracker>,
    /// Current prompt index (shared with session state)
    pub prompt_index: Arc<TokioMutex<usize>>,
    /// Working directory for path relativization
    pub cwd: PathBuf,
    /// Shared gate: when false, suppress gateway forwarding.
    /// Events are still processed for hunk tracking and file state.
    pub gateway_enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Persistence handle for FIFO ordinary writes and durable tombstone barriers.
    pub persistence: PersistenceHandle,
    /// When true, send incremental `output_delta` instead of full `output`
    /// in bash streaming updates. The client must opt in via the
    /// `echo.agent/incrementalBashOutput` capability.
    pub incremental_bash_output: bool,
    /// Plan mode tracker shared with the session actor.
    /// Used to transition state on `PlanModeEntered` / `PlanModeExited`
    /// tool notifications.
    pub plan_mode: Arc<parking_lot::Mutex<crate::session::plan_mode::PlanModeTracker>>,
    /// Session-level prompt mode shared with the session actor.
    /// Updated on `PlanModeEntered` / `PlanModeExited` and `session/set_mode`
    /// so the next turn starts in the correct mode.
    pub current_prompt_mode: Arc<parking_lot::Mutex<crate::session::plan_mode::PromptMode>>,
    /// Turn-level prompt mode. Set at turn start, then updated only by
    /// agent tool calls (`EnterPlanMode` / `ExitPlanMode`). NOT affected
    /// by `session/set_mode`. Read at turn end for `end_prompt_mode`.
    pub turn_prompt_mode: Arc<parking_lot::Mutex<crate::session::plan_mode::PromptMode>>,
    /// Session command channel for monitor events and task-completed injections.
    pub session_cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    pub task_completion_reservations:
        echo_agent_tools::reminders::task_completion::TaskCompletionReservations,
    pub task_wake_suppressed: echo_agent_tools::reminders::task_completion::TaskWakeSuppressed,
    /// Channel for requesting trace uploads for synthetic auto-wake turns.
    /// Wrapped in `Arc<Mutex<..>>` because the coordinator creates the channel
    /// after the notification bridge is spawned — the bridge reads the latest
    /// value on each notification.
    pub(crate) synthetic_trace_tx: Arc<
        std::sync::Mutex<
            Option<
                tokio::sync::mpsc::UnboundedSender<crate::upload::turn::SyntheticTurnTraceRequest>,
            >,
        >,
    >,
    /// Resolved name of the `BackgroundTaskAction` tool. Written exactly
    /// once after the agent's toolset is finalized; read many times
    /// thereafter from the notification bridge and the session actor's
    /// between-turn drain. `None` means no such tool is registered in this
    /// toolset, which is a valid resolved state.
    pub task_output_tool_name: Arc<std::sync::OnceLock<Option<String>>>,
    /// Resolved name of the `Read` tool, used by `format_bash_completion`'s
    /// disk-pointer footer so the model can recover full bash output from
    /// `task.output_file` even when no polling tool is available. Same
    /// write-once-read-many lifecycle as `task_output_tool_name`.
    pub read_tool_name: Arc<std::sync::OnceLock<Option<String>>>,
    /// When `false`, bash task completions fall back to the idle-gated
    /// `InjectNotification` path instead of immediate synthetic prompts.
    pub auto_wake_enabled: bool,
    /// When `true`, an approved `PlanModeExited` also arms the tracker's
    /// next-turn exit reminder. EchoAgent-build leaves this `false` — its
    /// exit-plan tool result already informs the model, and a deferred
    /// reminder would arrive stale. Shared with the session actor (the
    /// `gateway_enabled` pattern) and refreshed on zero-turn rebuilds so the
    /// bridge always agrees with the live session gate.
    pub queue_exit_reminder_on_approved_exit: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When `true`, suppress the bash auto-wake synthetic prompt. Shared `Arc`
    /// written at one chokepoint — see
    /// `SessionActor::set_goal_loop_active_resource` for the rationale.
    pub goal_loop_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
/// Snapshot a shared `OnceLock` tool-name slot as a borrowed `&str`.
/// Returns `None` if the slot is still unset (toolset not yet finalized)
/// or if the resolved value is `None` (no such tool registered in this
/// toolset.
pub(crate) fn resolved_tool_name(slot: &std::sync::OnceLock<Option<String>>) -> Option<&str> {
    slot.get().and_then(|v| v.as_deref())
}
/// Stamp a bridge-emitted notification's meta before it forks into
/// persistence + broadcast — see `util::event_id::ensure_event_id_meta`.
fn stamp_event_id(config: &NotificationBridgeConfig, meta: &mut Option<acp::Meta>) {
    crate::util::event_id::ensure_event_id_meta(&config.session_id.0, meta);
}
fn stamp_scheduler_meta(
    config: &NotificationBridgeConfig,
    meta: &mut Option<acp::Meta>,
    generation: &str,
    revision: u64,
) {
    stamp_event_id(config, meta);
    let meta = meta.get_or_insert_with(acp::Meta::new);
    meta.insert(
        "echo.agent/schedulerGeneration".to_owned(),
        generation.into(),
    );
    meta.insert("echo.agent/schedulerRevision".to_owned(), revision.into());
}
fn durable_append_landed(result: Result<(), DurableAppendError>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        Err(DurableAppendError::Committed(error)) => {
            tracing::warn!(%error, "Scheduler tombstone committed with bookkeeping failure");
            Ok(())
        }
        Err(DurableAppendError::NotCommitted(error)) => {
            Err(format!("scheduler tombstone was not committed: {error}"))
        }
        Err(DurableAppendError::AcknowledgementLost(error)) => Err(format!(
            "scheduler tombstone commit status is unknown: {error}"
        )),
    }
}
async fn handle_scheduled_task_removed(
    config: &NotificationBridgeConfig,
    removed: echo_agent_tools::notification::ScheduledTaskRemoved,
    acknowledgement: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
) -> Result<(), String> {
    tracing::info!(task_id = %removed.task_id, "Scheduled task removed");
    let result: Result<Box<serde_json::value::RawValue>, String> = async {
        let mut meta = None;
        stamp_scheduler_meta(config, &mut meta, &removed.generation, removed.revision);
        let notification = crate::extensions::notification::SessionNotification {
            session_id: config.session_id.clone(),
            update: crate::extensions::notification::SessionUpdate::ScheduledTaskDeleted {
                task_id: removed.task_id,
                reason: removed.reason,
            },
            meta: meta.map(serde_json::Value::Object),
        };
        let params = serde_json::to_value(&notification)
            .and_then(|value| serde_json::value::to_raw_value(&value))
            .map_err(|error| format!("failed to serialize scheduled task deletion: {error}"))?;
        let update = crate::session::storage::SessionUpdate::EchoAgent(Box::new(notification));
        if acknowledgement.is_some() {
            durable_append_landed(config.persistence.append_update_durably(update).await)?;
        } else {
            config
                .persistence
                .tx
                .send(PersistenceMsg::Update(update))
                .map_err(|_| "session persistence stopped".to_owned())?;
        }
        Ok(params)
    }
    .await;
    match result {
        Ok(params) => {
            if let Some(acknowledgement) = acknowledgement {
                let _ = acknowledgement.send(Ok(()));
            }
            config
                .gateway
                .forward_fire_and_forget(acp::ExtNotification::new(
                    "echo.agent/scheduled_task_deleted",
                    params.into(),
                ));
            Ok(())
        }
        Err(error) => {
            if let Some(acknowledgement) = acknowledgement {
                let _ = acknowledgement.send(Err(error.clone()));
            }
            Err(error)
        }
    }
}
/// Create a `ToolNotificationHandle` and spawn a bridge task that
/// translates notifications into shell-native systems.
pub(crate) fn spawn_notification_bridge(
    config: NotificationBridgeConfig,
) -> ToolNotificationHandle {
    let (handle, mut rx) = ToolNotificationHandle::acknowledged_channel();
    tokio::task::spawn_local(async move {
        let mut offsets: HashMap<String, usize> = HashMap::new();
        while let Some(delivery) = rx.recv().await {
            let acknowledgement = delivery.acknowledgement;
            match delivery.notification {
                ToolNotification::ScheduledTaskRemoved(removed) => {
                    if let Err(error) =
                        handle_scheduled_task_removed(&config, removed, acknowledgement).await
                    {
                        tracing::warn!(%error, "Failed to handle scheduled task removal");
                    }
                }
                notification => {
                    handle_notification(&config, notification, &mut offsets).await;
                    if let Some(acknowledgement) = acknowledgement {
                        let _ = acknowledgement.send(Ok(()));
                    }
                }
            }
        }
        tracing::debug!("Notification bridge task exiting (sender dropped)");
    });
    handle
}
/// Emit a `CurrentModeUpdate` for the given [`SessionMode`] — persisted to
/// `updates.jsonl` so session replay re-applies the mode, and forwarded to
/// the gateway so the pager updates live.
async fn emit_current_mode_update(
    config: &NotificationBridgeConfig,
    mode: echo_agent_tools::types::SessionMode,
) {
    let mut notification = acp::SessionNotification::new(
        config.session_id.clone(),
        acp::SessionUpdate::CurrentModeUpdate(acp::CurrentModeUpdate::new(
            acp::SessionModeId::new(mode.as_id()),
        )),
    );
    stamp_event_id(config, &mut notification.meta);
    let _ = config.persistence.tx.send(PersistenceMsg::Update(
        crate::session::storage::SessionUpdate::Acp(Box::new(notification.clone())),
    ));
    config.gateway.forward_fire_and_forget(notification);
}
/// Handle a single notification by forwarding it to the appropriate shell system.
async fn handle_notification(
    config: &NotificationBridgeConfig,
    notification: ToolNotification,
    offsets: &mut HashMap<String, usize>,
) {
    match notification {
        ToolNotification::BashOutputChunk(chunk) => {
            let (output, output_delta) = if config.incremental_bash_output {
                let prev_offset = offsets.get(&chunk.base.tool_call_id).copied().unwrap_or(0);
                let full = &chunk.base.output;
                let delta = if prev_offset <= full.len() {
                    full[prev_offset..].to_vec()
                } else {
                    full.clone()
                };
                offsets.insert(chunk.base.tool_call_id.clone(), full.len());
                (Vec::new(), Some(delta))
            } else {
                (chunk.base.output.clone(), None)
            };
            let bash_output = ToolOutput::Bash(BashOutput {
                output_for_prompt: BashOutput::make_output_for_prompt(&String::from_utf8_lossy(
                    &chunk.base.output,
                )),
                output,
                exit_code: 0,
                command: chunk.base.command.clone(),
                truncated: chunk.base.truncated,
                signal: None,
                timed_out: false,
                description: None,
                current_dir: chunk.base.cwd.to_string_lossy().to_string(),
                output_file: String::new(),
                total_bytes: chunk.base.total_bytes,
                output_delta,
                was_bare_echo: false,
            });
            let update = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(chunk.base.tool_call_id.clone()),
                acp::ToolCallUpdateFields::new()
                    .status(Some(acp::ToolCallStatus::InProgress))
                    .content(Some(vec![acp::ToolCallContent::from(
                        acp::ContentBlock::Text(acp::TextContent::new(
                            String::from_utf8_lossy(&chunk.base.output).into_owned(),
                        )),
                    )]))
                    .raw_output(serde_json::to_value(&bash_output).ok()),
            ));
            let notification = acp::SessionNotification::new(config.session_id.clone(), update);
            if config
                .gateway_enabled
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                let _ = config.gateway.session_notification(notification).await;
            }
        }
        ToolNotification::BashExecutionComplete(complete) => {
            offsets.remove(&complete.base.tool_call_id);
            tracing::debug!(
                tool_call_id = %complete.base.tool_call_id,
                exit_code = ?complete.exit_code,
                "Bash execution complete notification received"
            );
        }
        ToolNotification::BashExecutionTimeout(timeout) => {
            tracing::debug!(
                tool_call_id = %timeout.base.tool_call_id,
                elapsed = ?timeout.elapsed,
                "Bash execution timeout notification received"
            );
        }
        ToolNotification::BashExecutionFailed(failed) => {
            tracing::warn!(
                tool_call_id = %failed.tool_call_id,
                error = %failed.error,
                "Bash execution failed notification received"
            );
        }
        ToolNotification::BashExecutionBackgrounded(bg) => {
            tracing::debug!(
                tool_call_id = %bg.base.tool_call_id,
                task_id = %bg.task_id,
                command = %bg.base.command,
                output_file = %bg.output_file.display(),
                "Bash execution backgrounded notification received — forwarding to TUI"
            );
            let mut notification = crate::extensions::notification::SessionNotification {
                session_id: config.session_id.clone(),
                update: crate::extensions::notification::SessionUpdate::TaskBackgrounded {
                    tool_call_id: bg.base.tool_call_id.clone(),
                    task_id: bg.task_id.clone(),
                    command: bg.base.command.clone(),
                    cwd: bg.base.cwd.to_string_lossy().to_string(),
                    output_file: bg.output_file.to_string_lossy().to_string(),
                    monitor_description: bg.monitor_description.clone(),
                    description: bg.description.clone(),
                },
                meta: None,
            };
            {
                let mut meta_map = None;
                stamp_event_id(config, &mut meta_map);
                notification.meta = meta_map.map(serde_json::Value::Object);
            }
            let _ = config.persistence.tx.send(PersistenceMsg::Update(
                crate::session::storage::SessionUpdate::EchoAgent(Box::new(notification.clone())),
            ));
            let params = serde_json::to_value(&notification)
                .and_then(|v| serde_json::value::to_raw_value(&v))
                .ok();
            if let Some(params) = params {
                let ext_notification =
                    acp::ExtNotification::new("echo.agent/task_backgrounded", params.into());
                config.gateway.forward_fire_and_forget(ext_notification);
            }
        }
        ToolNotification::FileWritten(written) => {
            let prompt_index = *config.prompt_index.lock().await;
            config.hunk_tracker_handle.record_agent_write(
                written.absolute_path.clone(),
                written.content.clone(),
                prompt_index,
                written.previous_content.clone(),
            );
            if written.previous_content.is_some() || written.is_new_file {
                config
                    .file_state_tracker
                    .record_tool_write(
                        prompt_index,
                        &written.absolute_path,
                        &config.cwd,
                        written.previous_content,
                        (!written.is_deleted).then_some(written.content),
                    )
                    .await;
            }
            tracing::debug!(
                path = %written.absolute_path.display(),
                is_new_file = written.is_new_file,
                "FileWritten notification forwarded to hunk tracker"
            );
        }
        ToolNotification::SubagentCompleted(_) => {}
        ToolNotification::TaskCompleted(task_snapshot) => {
            let is_monitor =
                task_snapshot.kind == echo_agent_tools::computer::types::TaskKind::Monitor;
            let task_id = task_snapshot.task_id.clone();
            let goal_loop_active = config
                .goal_loop_active
                .load(std::sync::atomic::Ordering::Relaxed);
            let mut will_wake = false;
            if task_snapshot.is_auto_wake_suppressed() {
                echo_agent_telemetry::unified_log::info(
                    "shell.task_wake.suppressed",
                    Some(config.session_id.0.as_ref()),
                    Some(serde_json::json!({
                        "task_id": &task_id,
                        "block_waited": task_snapshot.block_waited,
                        "explicitly_killed": task_snapshot.explicitly_killed,
                        "kill_result_delivered": task_snapshot.kill_result_delivered,
                    })),
                );
            } else if goal_loop_active {
                tracing::info!(
                    task_id = %task_id,
                    is_monitor,
                    "auto-wake: suppressed completion (goal loop active)"
                );
            } else if config.auto_wake_enabled {
                config.task_completion_reservations.reserve(task_id.clone());
                let tool_name = resolved_tool_name(&config.task_output_tool_name);
                let read_name = resolved_tool_name(&config.read_tool_name);
                let body = if is_monitor {
                    echo_agent_tools::reminders::task_completion::format_monitor_completion(
                        &task_snapshot,
                        tool_name,
                    )
                } else {
                    echo_agent_tools::reminders::task_completion::format_bash_completion(
                        &task_snapshot,
                        tool_name,
                        read_name,
                    )
                };
                let message = echo_agent_tools::reminders::wrap_reminder(&body);
                let prompt_id = format!("task-completed-{task_id}");
                let prompt_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(message))];
                let synthetic_trace_tx = config
                    .synthetic_trace_tx
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let (respond_to, completion_rx) = tokio::sync::oneshot::channel();
                let (admission_tx, admission_rx) = tokio::sync::oneshot::channel();
                tracing::info!(
                    task_id = %task_id,
                    prompt_id = %prompt_id,
                    is_monitor,
                    "auto-wake: requesting synthetic prompt admission for completed background task"
                );
                let enqueued = config
                    .session_cmd_tx
                    .send(SessionCommand::Prompt {
                        prompt_id: prompt_id.clone(),
                        prompt_blocks,
                        prompt_mode: crate::session::plan_mode::PromptMode::Agent,
                        artifact_upload_ctx: None,
                        client_identifier: None,
                        screen_mode: None,
                        verbatim: true,
                        traceparent: echo_agent_file_utils::trace_context::current_traceparent(),
                        json_schema: None,
                        send_now: false,
                        tool_overrides_update: None,
                        admission: Some(crate::session::commands::TaskWakeAdmission {
                            respond_to: admission_tx,
                            fallback: crate::session::commands::TaskWakeFallback {
                                prompt_id: if is_monitor {
                                    format!("monitor-completed-{task_id}")
                                } else {
                                    format!("bash-completed-{task_id}")
                                },
                                prompt_blocks: vec![acp::ContentBlock::Text(
                                    acp::TextContent::new(body.clone()),
                                )],
                                source: if is_monitor {
                                    NotificationSource::MonitorCompleted {
                                        task_id: task_id.clone(),
                                    }
                                } else {
                                    NotificationSource::BashTaskCompleted {
                                        task_id: task_id.clone(),
                                    }
                                },
                            },
                        }),
                        respond_to,
                        persist_ack: None,
                        parsed_prompt_tx: None,
                    })
                    .is_ok();
                if !enqueued {
                    config.task_completion_reservations.release(&task_id);
                }
                let admitted = if enqueued {
                    tokio::time::timeout(TASK_WAKE_ADMISSION_TIMEOUT, admission_rx)
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or(false)
                } else {
                    false
                };
                will_wake = admitted;
                echo_agent_telemetry::unified_log::info(
                    "shell.task_wake.bridge_admission",
                    Some(config.session_id.0.as_ref()),
                    Some(serde_json::json!({
                        "task_id": &task_id,
                        "monitor": is_monitor,
                        "enqueued": enqueued,
                        "admitted": admitted,
                        "gate": config.task_wake_suppressed.get(),
                    })),
                );
                if will_wake {
                    if is_monitor {
                        let _ =
                            config
                                .session_cmd_tx
                                .send(SessionCommand::DropMonitorNotifications {
                                    task_id: task_id.clone(),
                                });
                    }
                    if let Some(trace_tx) = synthetic_trace_tx {
                        let (before_copy_tx, before_session_copy_rx) =
                            tokio::sync::oneshot::channel();
                        let copy_requested = config
                            .session_cmd_tx
                            .send(SessionCommand::CopyFile {
                                respond_to: before_copy_tx,
                            })
                            .is_ok();
                        if copy_requested {
                            tracing::info!(
                                task_id = %task_id,
                                "auto-wake: sending synthetic turn trace request"
                            );
                            let _ = trace_tx.send(crate::upload::turn::SyntheticTurnTraceRequest {
                                session_id: config.session_id.clone(),
                                prompt_id,
                                completion_rx,
                                before_session_copy_rx,
                            });
                        } else {
                            tracing::debug!(
                                task_id = %task_id,
                                "auto-wake: session snapshot request failed, skipping trace request"
                            );
                        }
                    } else {
                        tracing::debug!(
                            task_id = %task_id,
                            "auto-wake: no synthetic trace consumer, skipping trace request"
                        );
                    }
                }
            } else {
                let tool_name = resolved_tool_name(&config.task_output_tool_name);
                let read_name = resolved_tool_name(&config.read_tool_name);
                let message = if is_monitor {
                    echo_agent_tools::reminders::task_completion::format_monitor_completion(
                        &task_snapshot,
                        tool_name,
                    )
                } else {
                    echo_agent_tools::reminders::task_completion::format_bash_completion(
                        &task_snapshot,
                        tool_name,
                        read_name,
                    )
                };
                let source = if is_monitor {
                    NotificationSource::MonitorCompleted {
                        task_id: task_id.clone(),
                    }
                } else {
                    NotificationSource::BashTaskCompleted {
                        task_id: task_id.clone(),
                    }
                };
                let _ = config
                    .session_cmd_tx
                    .send(SessionCommand::InjectNotification {
                        prompt_id: if is_monitor {
                            format!("monitor-completed-{task_id}")
                        } else {
                            format!("bash-completed-{task_id}")
                        },
                        prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
                            message,
                        ))],
                        priority: NotificationPriority::Later,
                        source,
                    });
            }
            let mut notification = crate::extensions::notification::SessionNotification {
                session_id: config.session_id.clone(),
                update: crate::extensions::notification::SessionUpdate::TaskCompleted {
                    task_snapshot,
                    will_wake,
                },
                meta: None,
            };
            {
                let mut meta_map = None;
                stamp_event_id(config, &mut meta_map);
                notification.meta = meta_map.map(serde_json::Value::Object);
            }
            if let Some(params) = task_completed_frame::encode(&mut notification) {
                let _ = config.persistence.tx.send(PersistenceMsg::Update(
                    crate::session::storage::SessionUpdate::EchoAgent(Box::new(
                        notification.clone(),
                    )),
                ));
                let notification: acp::ExtNotification = acp::ExtNotification::new(
                    task_completed_frame::METHOD,
                    params.into_inner().into(),
                );
                config.gateway.forward_fire_and_forget(notification);
            }
            let _ = config
                .session_cmd_tx
                .send(SessionCommand::DispatchNotificationHook {
                    notification_type: "task_complete".into(),
                    message: Some(format!("Background task completed: {task_id}")),
                    title: None,
                    level: Some("info".into()),
                });
        }
        ToolNotification::PlanModeEntered(entered) => {
            let activated = config.plan_mode.lock().activate_from_tool();
            if activated {
                *config.current_prompt_mode.lock() = crate::session::plan_mode::PromptMode::Plan;
                *config.turn_prompt_mode.lock() = crate::session::plan_mode::PromptMode::Plan;
                let snapshot = config.plan_mode.lock().snapshot();
                let _ = config
                    .persistence
                    .tx
                    .send(PersistenceMsg::PlanModeState(snapshot));
                emit_current_mode_update(config, echo_agent_tools::types::SessionMode::Plan).await;
            }
            tracing::info!(
                tool_call_id = %entered.tool_call_id,
                activated,
                "Plan mode entered via EnterPlanMode tool"
            );
        }
        ToolNotification::PlanModeExited(exited) => {
            let deactivated = {
                let mut tracker = config.plan_mode.lock();
                let deactivated = tracker.deactivate_approved();
                if deactivated
                    && config
                        .queue_exit_reminder_on_approved_exit
                        .load(std::sync::atomic::Ordering::Relaxed)
                {
                    tracker.queue_exit_reminder();
                }
                deactivated
            };
            if deactivated {
                *config.current_prompt_mode.lock() = crate::session::plan_mode::PromptMode::Agent;
                *config.turn_prompt_mode.lock() = crate::session::plan_mode::PromptMode::Agent;
                let snapshot = config.plan_mode.lock().snapshot();
                let _ = config
                    .persistence
                    .tx
                    .send(PersistenceMsg::PlanModeState(snapshot));
                emit_current_mode_update(config, echo_agent_tools::types::SessionMode::Default)
                    .await;
            }
            tracing::info!(
                tool_call_id = %exited.tool_call_id,
                deactivated,
                has_plan = exited.plan_content.is_some(),
                "Plan mode exited via ExitPlanMode tool"
            );
        }
        ToolNotification::UserQuestionAsked(asked) => {
            tracing::info!(
                tool_call_id = %asked.tool_call_id,
                "User question asked"
            );
        }
        ToolNotification::LspServerStarting(s) => {
            tracing::debug!(server = %s.server_name, command = %s.command, "LSP server starting");
        }
        ToolNotification::LspServerReady(s) => {
            tracing::info!(server = %s.server_name, "LSP server ready");
        }
        ToolNotification::LspServerCrashed(s) => {
            tracing::warn!(server = %s.server_name, "LSP server crashed");
        }
        ToolNotification::LspServerRetrying(s) => {
            tracing::warn!(
                server = %s.server_name,
                attempt = s.attempt,
                max_restarts = s.max_restarts,
                backoff_ms = s.backoff_ms,
                "LSP server retrying"
            );
        }
        ToolNotification::LspServerFailed(s) => {
            tracing::error!(server = %s.server_name, error = %s.error, "LSP server failed");
        }
        ToolNotification::ScheduledTaskFired(fired) => {
            tracing::info!(
                task_id = %fired.task_id,
                schedule = %fired.human_schedule,
                subagent_id = fired.subagent_id.as_deref().unwrap_or(""),
                "Scheduled task fired"
            );
            if fired.subagent_id.is_none() {
                let inject_payload = serde_json::json!({
                    "sessionId": config.session_id,
                    "taskId": &fired.task_id,
                    "prompt": &fired.prompt,
                    "humanSchedule": &fired.human_schedule,
                    "nextFireAt": &fired.next_fire_at,
                });
                if let Ok(params) = serde_json::value::to_raw_value(&inject_payload) {
                    config
                        .gateway
                        .forward_fire_and_forget(acp::ExtNotification::new(
                            "echo.agent/scheduled_task_inject_prompt",
                            params.into(),
                        ));
                }
            }
            let mut meta = None;
            stamp_scheduler_meta(config, &mut meta, &fired.generation, fired.revision);
            let fired_notif = crate::extensions::notification::SessionNotification {
                session_id: config.session_id.clone(),
                update: crate::extensions::notification::SessionUpdate::ScheduledTaskFired {
                    task_id: fired.task_id,
                    prompt: fired.prompt,
                    human_schedule: fired.human_schedule,
                    next_fire_at: fired.next_fire_at,
                    subagent_id: fired.subagent_id,
                },
                meta: meta.map(serde_json::Value::Object),
            };
            if let Ok(params) =
                serde_json::to_value(&fired_notif).and_then(|v| serde_json::value::to_raw_value(&v))
            {
                config
                    .gateway
                    .forward_fire_and_forget(acp::ExtNotification::new(
                        "echo.agent/scheduled_task_fired",
                        params.into(),
                    ));
            }
        }
        ToolNotification::MonitorEvent(event) => {
            let my_session = config.session_id.0.as_ref();
            if let Some(owner) = event.owner_session_id.as_deref()
                && owner != my_session
            {
                tracing::warn!(
                    task_id = %event.task_id,
                    description = %event.description,
                    monitor_owner = %owner,
                    bridge_session = %my_session,
                    "Dropped cross-session monitor event: owner does not match this bridge's session"
                );
                return;
            }
            tracing::debug!(
                task_id = %event.task_id,
                description = %event.description,
                "Monitor event received, injecting into session"
            );
            let notification = crate::extensions::notification::SessionNotification {
                session_id: config.session_id.clone(),
                update: crate::extensions::notification::SessionUpdate::MonitorEvent {
                    task_id: event.task_id.clone(),
                    description: event.description.clone(),
                    event_text: event.raw_text.clone(),
                },
                meta: None,
            };
            let params = serde_json::to_value(&notification)
                .and_then(|v| serde_json::value::to_raw_value(&v))
                .ok();
            if let Some(params) = params {
                config
                    .gateway
                    .forward_fire_and_forget(acp::ExtNotification::new(
                        "echo.agent/monitor_event",
                        params.into(),
                    ));
            }
            if config.task_completion_reservations.contains(&event.task_id) {
                tracing::debug!(
                    task_id = %event.task_id,
                    "skipping model inject for monitor event: task already auto-woke via TaskCompleted"
                );
                return;
            }
            let prompt_id = format!("monitor-{}-{}", event.task_id, uuid::Uuid::now_v7());
            let prompt_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(
                event.event_text,
            ))];
            let _ = config
                .session_cmd_tx
                .send(SessionCommand::InjectNotification {
                    prompt_id,
                    prompt_blocks,
                    priority: NotificationPriority::Next,
                    source: NotificationSource::MonitorEvent {
                        task_id: event.task_id.clone(),
                    },
                });
        }
        ToolNotification::ScheduledTaskRemoved(removed) => {
            if let Err(error) = handle_scheduled_task_removed(config, removed, None).await {
                tracing::warn!(%error, "Failed to handle scheduled task removal");
            }
        }
        ToolNotification::ScheduledTaskCreated(created) => {
            tracing::info!(task_id = %created.task_id, "Scheduled task created");
            let mut meta = None;
            stamp_scheduler_meta(config, &mut meta, &created.generation, created.revision);
            let notification = crate::extensions::notification::SessionNotification {
                session_id: config.session_id.clone(),
                update: crate::extensions::notification::SessionUpdate::ScheduledTaskCreated {
                    task_id: created.task_id,
                    prompt: created.prompt,
                    human_schedule: created.human_schedule,
                    next_fire_at: created.next_fire_at,
                },
                meta: meta.map(serde_json::Value::Object),
            };
            let _ = config.persistence.tx.send(PersistenceMsg::Update(
                crate::session::storage::SessionUpdate::EchoAgent(Box::new(notification.clone())),
            ));
            if let Ok(params) = serde_json::to_value(&notification)
                .and_then(|v| serde_json::value::to_raw_value(&v))
            {
                config
                    .gateway
                    .forward_fire_and_forget(acp::ExtNotification::new(
                        "echo.agent/scheduled_task_created",
                        params.into(),
                    ));
            }
        }
    }
}
#[cfg(test)]
#[path = "notification_bridge_tests.rs"]
mod tests;
