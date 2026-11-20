//! Prompt-task plumbing for `SessionActor` (`AgentTask`, `TaskSlot`,
//! `run_task`, turn guards) and the cancel paths.

use super::*;

/// Whether the post-cancel notification drain stays suppressed: rewind and non-stop cancels
/// clear it, a stop gesture arms it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WakeBarrier {
    Armed,
    Clear,
}

/// Outcome of `cancel_running_task`.
#[must_use = "the notification drain and the idle ping both depend on this"]
pub(super) struct CancelOutcome {
    pub(super) barrier: WakeBarrier,
    /// A user cancel with a classified reason tore down a turn, a rewind included. Whether that
    /// leaves the session idle is the loop's call.
    pub(super) turn_stopped: bool,
}

/// What a cancel needs to report the turn it tore down. `epoch` is captured with the task under
/// the state lock, so the report names that turn rather than whichever is current when it files.
pub(super) struct CancelledTurn<'a> {
    pub(super) prompt_id: &'a str,
    pub(super) epoch: TurnEpoch,
    pub(super) reason: echo_agent_hooks::event::StopCancelledReason,
    pub(super) trigger: Option<String>,
    pub(super) last_assistant_message: Option<String>,
}

pub(super) struct TurnSubagentScopeGuard {
    current_prompt_id: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    prompt_id: String,
}

impl TurnSubagentScopeGuard {
    pub(super) fn new(
        current_prompt_id: std::sync::Arc<std::sync::Mutex<Option<String>>>,
        prompt_id: String,
    ) -> Self {
        Self {
            current_prompt_id,
            prompt_id,
        }
    }
}

impl Drop for TurnSubagentScopeGuard {
    fn drop(&mut self) {
        let mut current_prompt_id = self
            .current_prompt_id
            .lock()
            .expect("current_prompt_id mutex poisoned");
        if current_prompt_id.as_deref() == Some(self.prompt_id.as_str()) {
            *current_prompt_id = None;
        }
    }
}

/// RAII guard that stores `false` into an `is_turn_active` flag on drop.
/// Guarantees the flag is cleared on all exit paths (early returns, errors, panics).
pub(super) struct TurnActiveGuard(Option<Arc<std::sync::atomic::AtomicBool>>);

impl TurnActiveGuard {
    pub(super) fn activate(flag: Option<&Arc<std::sync::atomic::AtomicBool>>) -> Self {
        if let Some(f) = flag {
            f.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Self(flag.cloned())
    }
}

impl Drop for TurnActiveGuard {
    fn drop(&mut self) {
        if let Some(flag) = &self.0 {
            flag.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

pub(crate) struct TurnInputRequest {
    pub(crate) prompt_id: String,
    pub(crate) input_origin: InputOrigin,
    pub(crate) prompt_blocks: Vec<ContentBlock>,
    pub(crate) prompt_mode: PromptMode,
    pub(crate) trace_gcs_config: Option<crate::session::repo_changes::TraceExportConfig>,
    pub(crate) artifact_tracker: Option<crate::upload::manifest::ArtifactTracker>,
    pub(crate) client_identifier: Option<String>,
    pub(crate) screen_mode: Option<String>,
    pub(crate) verbatim: bool,
    pub(crate) send_now: bool,
    pub(crate) json_schema: Option<serde_json::Value>,
    pub(crate) persist_ack: Option<oneshot::Sender<()>>,
    pub(crate) parsed_prompt_tx: Option<oneshot::Sender<ParsedPromptInfo>>,
}

pub(crate) struct AgentTask {
    pub(crate) prompt_id: String,
    pub(crate) handle: tokio::task::AbortHandle,
}

impl AgentTask {
    pub(super) fn new_prompt(
        session: Arc<SessionActor>,
        request: TurnInputRequest,
        completion_tx: mpsc::UnboundedSender<(String, PromptTurnResult)>,
    ) -> Self {
        let prompt_id = request.prompt_id.clone();
        Self {
            prompt_id,
            handle: tokio::task::spawn_local(run_task(session, request, completion_tx))
                .abort_handle(),
        }
    }

    fn abort(&self) {
        if !self.handle.is_finished() {
            self.handle.abort();
        }
    }
}

/// Holds at most one spawned task; arming a new one aborts the previous.
/// `Cell` interior mutability because `SessionActor` is `!Send` (single-threaded
/// LocalSet). Backs both the deferred user-message prefix (`take`s the handle to
/// await its result) and the idle-notification debounce (`cancel`s it).
pub(crate) struct TaskSlot<T> {
    handle: std::cell::Cell<Option<tokio::task::JoinHandle<T>>>,
}

impl<T> TaskSlot<T> {
    pub(crate) fn new() -> Self {
        Self {
            handle: std::cell::Cell::new(None),
        }
    }

    /// Abort any pending task and store the new one.
    pub(crate) fn arm(&self, handle: tokio::task::JoinHandle<T>) {
        if let Some(old) = self.handle.take() {
            old.abort();
        }
        self.handle.set(Some(handle));
    }

    /// Take the pending task handle (e.g. to await its result).
    pub(crate) fn take(&self) -> Option<tokio::task::JoinHandle<T>> {
        self.handle.take()
    }

    /// Abort and drop any pending task (e.g. because a new turn started).
    pub(crate) fn cancel(&self) {
        if let Some(old) = self.handle.take() {
            old.abort();
        }
    }
}

/// Client-visible user rows only; interjection fallbacks take a plain cancel.
fn front_is_rewind_poppable(front: Option<&InputItem>) -> bool {
    front.is_some_and(|f| {
        matches!(
            f.input_origin.as_prompt_origin(),
            crate::session::PromptOrigin::User
        ) && !super::interjection::is_interject_fallback(&f.prompt_id)
    })
}

async fn run_task(
    session: Arc<SessionActor>,
    request: TurnInputRequest,
    completion_tx: mpsc::UnboundedSender<(String, PromptTurnResult)>,
) {
    let prompt_id = request.prompt_id.clone();
    let result = session.handle_turn_input(request).await;
    let _ = completion_tx.send((prompt_id, result));
}

impl SessionActor {
    /// Turn-scoped: soft cancel / max-turns only (not user Stop).
    /// `parent_prompt_id` is the authoritative turn id from the turn runner.
    pub(super) fn cancel_running_turn_subagents(&self, parent_prompt_id: &str) {
        self.cancel_subagents_for_prompt_id(parent_prompt_id);
    }

    /// User Stop with cancel_subagents: all non-workflow session children.
    /// Uses the session-bound backend API so cancel never wildcards other sessions.
    pub(super) fn cancel_all_session_subagents(&self) {
        if let Some(event_tx) = self.tool_context.subagent_event_tx.clone() {
            use echo_agent_tools::implementations::echo_agent_build::task::backend::ChannelBackend;
            let backend = ChannelBackend::for_session(event_tx, self.session_id_string());
            let _ = backend.request_cancel_parent_session(tokio::sync::oneshot::channel().0);
        }
    }

    /// Re-open Task spawns for this session after a prior user Stop.
    pub(super) fn open_subagent_spawn_admission(&self) {
        if let Some(event_tx) = self.tool_context.subagent_event_tx.clone() {
            use echo_agent_tools::implementations::echo_agent_build::task::backend::ChannelBackend;
            let backend = ChannelBackend::for_session(event_tx, self.session_id_string());
            let _ = backend.open_spawn_admission();
        }
    }

    fn cancel_subagents_for_prompt_id(&self, parent_prompt_id: &str) {
        if let Some(event_tx) = self.tool_context.subagent_event_tx.clone() {
            use echo_agent_tools::implementations::echo_agent_build::task::types::{
                SubagentCancelRequest, SubagentCancelTarget, SubagentEvent,
            };
            let _ = event_tx.send(SubagentEvent::Cancel(SubagentCancelRequest {
                parent_session_id: Some(self.session_id_string()),
                target: SubagentCancelTarget::ParentPromptId(parent_prompt_id.to_string()),
                respond_to: tokio::sync::oneshot::channel().0,
            }));
        }
    }

    /// Takes the state lock with `try_lock`, so it must run before its caller's first await: a
    /// task suspended across one still holds the guard.
    fn arm_wake_barrier(&self, trigger: Option<&crate::session::CancelTrigger>) {
        if let Some(gate) = &self.tool_context.task_wake_suppressed {
            gate.set(true);
        }
        let mut state = self.state.try_lock().expect("session state is actor-owned");
        state.notifications_suppressed = true;
        echo_agent_telemetry::unified_log::info(
            "shell.task_wake.cancel_barrier",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "trigger": trigger.map(crate::session::CancelTrigger::as_str),
                "gate": self
                    .tool_context
                    .task_wake_suppressed
                    .as_ref()
                    .is_some_and(|gate| gate.get()),
                "state": state.notifications_suppressed,
            })),
        );
        drop(state);
        if let Some(is_turn_active) = &self.tool_context.is_turn_active {
            is_turn_active.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Releases the gate's claim here rather than at the aborted task's drop, which runs too late
    /// for the cancel to report. Callers read `epoch` under the lock they take the task from.
    fn abort_turn_task(&self, task: &AgentTask, epoch: super::turn_report_slot::TurnEpoch) {
        task.abort();
        self.turn_report.release_aborted(epoch);
    }

    /// The Ctrl+C teardown, except the running command moves to the background instead of being killed.
    /// Background tasks, subagents, and the queue are untouched.
    pub(super) async fn cancel_turn_for_send_now(
        &self,
        replay_buffer: &mut crate::agent::update_chunk_merge::ReplayBuffer,
    ) {
        if let Some(notification) = replay_buffer.flush() {
            self.emit_buffered(notification).await;
        }
        // Flush, don't clear: the pager already painted the text and said "Interjection sent".
        let flushed = self.flush_stranded_interjections().await;
        if flushed > 0 {
            echo_agent_telemetry::unified_log::info(
                "shell.prompt.send_now_flushed_interjections",
                Some(self.session_info.id.0.as_ref()),
                Some(serde_json::json!({ "count": flushed })),
            );
        }
        // Cancel-and-send replaces the turn rather than ending it, so it reports nothing.
        let _ = self
            .cancel_running_task(crate::session::CancelOptions {
                trigger: Some(crate::session::CancelTrigger::SendNow),
                ..Default::default()
            })
            .await;
        // Re-enable notification drains: unlike Ctrl+C, a send-now means the user is re-engaged.
        if let Some(gate) = &self.tool_context.task_wake_suppressed {
            gate.set(false);
        }
        self.state.lock().await.notifications_suppressed = false;
        echo_agent_telemetry::unified_log::info(
            "shell.task_wake.gate_cleared",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({ "reason": "send_now" })),
        );
    }

    /// Names the turn this cancel tore down, so a turn promoted in between is refused rather
    /// than reported over.
    fn report_cancelled_turn(&self, cancelled: CancelledTurn<'_>) {
        self.claim_and_queue(
            cancelled.prompt_id,
            cancelled.epoch,
            TurnEnd::Cancelled {
                reason: cancelled.reason,
                trigger: cancelled.trigger,
                reason_details: None,
                last_assistant_message: cancelled.last_assistant_message,
            },
        );
    }

    /// One line per rewind-requested cancel: `rewound | legacy_rewound | stale_prompt_id | window_closed | non_user_front`.
    fn log_rewind_decision(
        &self,
        requested_prompt_id: Option<&str>,
        front_prompt_id: Option<&str>,
        rewind_disposition: &str,
    ) {
        echo_agent_telemetry::unified_log::info(
            "shell.cancel.rewind_decision",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "requested_prompt_id": requested_prompt_id,
                "front_prompt_id": front_prompt_id,
                "rewind_disposition": rewind_disposition,
            })),
        );
    }

    /// Trim the rewound turn from in-memory history and resolve it as `Rewound`.
    /// `target_prompt_index` is the claim-time cut; `None` uses a fresh snapshot (legacy id-less).
    async fn finish_rewound_cancel(
        &self,
        input: InputItem,
        target_prompt_index: Option<usize>,
        total_tokens: u64,
        turn_stopped: bool,
    ) -> CancelOutcome {
        if let Some(mut snapshot) = self.chat_state_handle.snapshot().await {
            let target_prompt_index =
                target_prompt_index.unwrap_or_else(|| snapshot.prompt_index.saturating_sub(1));
            snapshot.prompt_index = target_prompt_index;
            snapshot.prompt_texts.truncate(target_prompt_index);
            // Marker-first cut; this path never replays, so post-compact talks rely on the marker.
            let keep_count =
                conversation_truncate_for_prompt(&snapshot.conversation, target_prompt_index);
            snapshot.conversation.truncate(keep_count);
            self.chat_state_handle.restore_snapshot(snapshot);
            self.file_state_tracker
                .truncate_from(target_prompt_index)
                .await;
        }
        let _ = input.respond_to.send(Ok(PromptTurnOk {
            stop_reason: acp::StopReason::Cancelled,
            total_tokens,
            turn_snapshot: None,
            completion_kind: PromptCompletionKind::Rewound,
            structured_output: None,
            usage: None,
            tool_overrides: self.effective_tool_overrides(),
        }));
        // The rewind cleared the barrier when it claimed/popped; wakes flow.
        CancelOutcome {
            barrier: WakeBarrier::Clear,
            turn_stopped,
        }
    }

    pub(super) async fn cancel_running_task(
        &self,
        options: crate::session::CancelOptions,
    ) -> CancelOutcome {
        let kind = options
            .trigger
            .as_ref()
            .map(crate::session::CancelTrigger::kind);
        let cancel_reason = super::turn_end_hooks::cancel_reason_for_options(&options);
        let crate::session::CancelOptions {
            cancel_subagents,
            kill_background_tasks,
            history,
            expected_prompt_id: _,
            trigger,
            user_initiated,
            acknowledged: _,
        } = options;
        let rewind_requested = matches!(
            history,
            crate::session::CancelHistoryDisposition::RewindIfNoOutput { .. }
        );
        let requested_prompt_id = match &history {
            crate::session::CancelHistoryDisposition::RewindIfNoOutput { prompt_id } => {
                prompt_id.clone()
            }
            crate::session::CancelHistoryDisposition::Keep => None,
        };
        // Claim the named front under one lock before teardown; a stale id
        // (NEW already promoted) is a no-op. Legacy (no id) uses the path below.
        let mut claimed_rewound: Option<(InputItem, TurnEpoch, usize)> = None;
        if let Some(requested) = requested_prompt_id.as_deref() {
            let mut state = self.state.lock().await;
            let front_prompt_id = state.pending_inputs.front().map(|f| f.prompt_id.clone());
            if front_prompt_id.as_deref() != Some(requested) {
                drop(state);
                self.log_rewind_decision(
                    Some(requested),
                    front_prompt_id.as_deref(),
                    "stale_prompt_id",
                );
                return CancelOutcome {
                    barrier: WakeBarrier::Clear,
                    turn_stopped: false,
                };
            }
            let poppable = front_is_rewind_poppable(state.pending_inputs.front());
            let rewind_disposition = if state.rewindable && poppable {
                // Drain here — claimed rewind never reaches the generic sweep.
                self.sweep_monitor_buffer_into_pending(&mut state, "monitor-cancel-drain");
                let turn_epoch = self.turn_report.epoch();
                if let Some(task) = state.running_task.take_if(|t| t.prompt_id == requested) {
                    self.abort_turn_task(&task, turn_epoch);
                }
                if let Some(gate) = &self.tool_context.task_wake_suppressed {
                    gate.set(false);
                }
                state.notifications_suppressed = false;
                echo_agent_telemetry::unified_log::info(
                    "shell.task_wake.gate_cleared",
                    Some(self.session_info.id.0.as_ref()),
                    Some(serde_json::json!({ "reason": "rewind" })),
                );
                state.rewindable = false;
                // Cut captured under the lock so later commits cannot move it.
                let target_prompt_index = self
                    .chat_state_handle
                    .get_prompt_index()
                    .await
                    .saturating_sub(1);
                claimed_rewound = state
                    .pending_inputs
                    .pop_front()
                    .map(|input| (input, turn_epoch, target_prompt_index));
                "rewound"
            } else if !state.rewindable {
                // Window closed: fall through to a normal cancel of this front.
                "window_closed"
            } else {
                "non_user_front"
            };
            drop(state);
            self.log_rewind_decision(
                Some(requested),
                front_prompt_id.as_deref(),
                rewind_disposition,
            );
        }
        // Claimed rewind owns only OLD. Generic teardown targets "whatever is
        // running now" and must not run — NEW may already be promoted.
        if let Some((input, epoch, target_prompt_index)) = claimed_rewound {
            self.notify_turn_abort(epoch, echo_agent_lifecycle::TurnAbortReason::Interrupted)
                .await;
            let total_tokens = self.chat_state_handle.get_total_tokens().await;
            return self
                .finish_rewound_cancel(
                    input,
                    Some(target_prompt_index),
                    total_tokens,
                    cancel_reason.is_some(),
                )
                .await;
        }
        let suppress_task_wakes = kind == Some(crate::session::CancelKind::StopGesture);
        // Abort in-flight `/compact` or auto-compact generation (stream select +
        // pre-replace guard). Safe when no compact is running.
        self.compaction.cancel.request_cancel();
        if suppress_task_wakes {
            self.arm_wake_barrier(trigger.as_ref());
        }

        // Unified-log processing marker (counterpart of `shell.cancel.received`
        // in `MvpAgent::cancel`): records which prompt the cancel lands on so
        // a stuck "Cancelling…" can be attributed to delivery vs. processing.
        // A pin snapshot only — the authoritative cancel identity is captured
        // from `running_task.prompt_id` under the state lock below, because
        // `current_prompt_id` is cleared early (turn scope guard drop /
        // `handle_completion`) while the finished front and its task slot are
        // still queued; keying the durable `TurnCompleted` on the pin alone
        // loses the terminal (and its `cancelTrigger`) in that window.
        let pinned_prompt_id = self
            .current_prompt_id
            .lock()
            .expect("current_prompt_id mutex poisoned")
            .clone();
        {
            echo_agent_telemetry::unified_log::info(
                "shell.cancel.processing",
                Some(self.session_info.id.0.as_ref()),
                Some(serde_json::json!({
                    "prompt_id": &pinned_prompt_id,
                    "cancel_subagents": cancel_subagents,
                    "kill_background_tasks": kill_background_tasks,
                    "rewind_if_no_output": rewind_requested,
                    "requested_prompt_id": &requested_prompt_id,
                    "trigger": trigger.as_ref().map(crate::session::CancelTrigger::as_str),
                })),
            );
        }

        if cancel_subagents {
            // Abort the producer first so it cannot enqueue more TaskTool work
            // after the ParentSession sweep. Keep the task slot for prompt-id
            // attribution below; cleanup will take+abort again (idempotent).
            {
                let state = self.state.lock().await;
                if let Some(task) = state.running_task.as_ref() {
                    self.abort_turn_task(task, self.turn_report.epoch());
                }
            }
            // Then cancel every non-workflow session child (incl. prior turns)
            // and close spawn admission until the next turn opens it.
            self.cancel_all_session_subagents();
        }

        // After the abort, so this chat-state round trip cannot keep the turn task alive, and
        // before the teardown edits the text.
        let last_assistant_message = if cancel_reason.is_some() {
            self.last_assistant_message_for_cancel().await
        } else {
            None
        };

        // A rewind is not a cancel either: the turn is being replaced, not stopped.
        if user_initiated && !rewind_requested {
            self.signals_handle().record_cancellation();
        }

        // A send-now redirect is the user continuing, not stopping, so it never kills an in-flight command.
        let send_now = matches!(trigger, Some(crate::session::CancelTrigger::SendNow));

        // Kill all running foreground terminal processes before aborting the task. Send-now skips this and backgrounds them after the abort.
        // Each TerminalBackend implementation knows how to kill its own processes.
        // Background tasks are left alive for interactive sessions but killed
        // during subagent teardown (kill_background_tasks = true).
        //
        // Note: a narrow TOCTOU window exists — the running task could spawn a
        // new terminal between this call and the abort() below. In practice this
        // is negligible; abort() drops the future and any child handle it owns.
        if !send_now {
            self.kill_foreground_commands_for_cancel().await;
        }

        if kill_background_tasks {
            if self.startup_hints.is_subagent {
                // Subagent teardown: only kill tasks owned by this session,
                // not the parent's or sibling's tasks on the shared backend.
                self.agent
                    .borrow()
                    .tool_bridge()
                    .kill_all_background_tasks_by_owner(&self.session_info.id.0)
                    .await;
            } else {
                self.agent
                    .borrow()
                    .tool_bridge()
                    .kill_all_background_tasks()
                    .await;
            }
        }

        let total_tokens = self.chat_state_handle.get_total_tokens().await;
        let (running_task, pending_inputs, rewound_input, had_queued_user_prompt, turn_epoch) = {
            let mut state = self.state.lock().await;
            debug_assert!(
                pinned_prompt_id.is_none()
                    || state.running_prompt_id().is_none()
                    || state.running_prompt_id() == pinned_prompt_id.as_deref(),
                "current_prompt_id pin disagrees with running_task identity"
            );

            // Drain MonitorEventBuffer into pending_notifications.
            // Closes the race between abort() and TurnActiveGuard drop:
            // is_turn_active may still be true, causing InjectNotification
            // to route Next-priority events to the buffer instead of
            // pending_notifications. Moving them here ensures they survive in
            // the queue; an interactive stop defers their drain, while other
            // cancels do not.
            self.sweep_monitor_buffer_into_pending(&mut state, "monitor-cancel-drain");

            // When killing all background tasks, also clear their pending
            // notifications — the monitors that produced them are now dead.
            if kill_background_tasks {
                state.clear_pending_notifications();
            }

            let front_is_user_row = front_is_rewind_poppable(state.pending_inputs.front());
            let front_prompt_id = state.pending_inputs.front().map(|f| f.prompt_id.clone());
            // Window-closed / non-user-front: the front can change across the
            // teardown awaits; a now-stale id must not cancel the promoted turn.
            let identity_matches = requested_prompt_id
                .as_deref()
                .is_none_or(|id| front_prompt_id.as_deref() == Some(id));
            let was_rewindable = state.rewindable;
            // Names the turn being torn down, for the claim release, the report, and the abort
            // announcement below. All three run after awaits a newer turn could start across.
            let turn_epoch = self.turn_report.epoch();
            let rewound_input = if rewind_requested
                && requested_prompt_id.is_none()
                && state.rewindable
                && front_is_user_row
            {
                if let Some(task) = state.running_task.take() {
                    self.abort_turn_task(&task, turn_epoch);
                }
                if let Some(gate) = &self.tool_context.task_wake_suppressed {
                    gate.set(false);
                }
                state.notifications_suppressed = false;
                echo_agent_telemetry::unified_log::info(
                    "shell.task_wake.gate_cleared",
                    Some(self.session_info.id.0.as_ref()),
                    Some(serde_json::json!({ "reason": "rewind" })),
                );
                state.rewindable = false;
                state.pending_inputs.pop_front()
            } else {
                None
            };
            if rewind_requested && requested_prompt_id.is_none() {
                self.log_rewind_decision(
                    None,
                    front_prompt_id.as_deref(),
                    if rewound_input.is_some() {
                        "legacy_rewound"
                    } else if !was_rewindable {
                        "window_closed"
                    } else {
                        "non_user_front"
                    },
                );
            }
            let running_task = if rewound_input.is_some() || !identity_matches {
                None
            } else {
                state.running_task.take()
            };
            if let Some(task) = &running_task {
                self.abort_turn_task(task, turn_epoch);
            }
            // Front changed across our awaits — leave the promoted turn alone.
            if !identity_matches {
                self.log_rewind_decision(
                    requested_prompt_id.as_deref(),
                    front_prompt_id.as_deref(),
                    "stale_prompt_id",
                );
                if suppress_task_wakes {
                    if let Some(gate) = &self.tool_context.task_wake_suppressed {
                        gate.set(false);
                    }
                    state.notifications_suppressed = false;
                }
                return CancelOutcome {
                    barrier: WakeBarrier::Clear,
                    turn_stopped: false,
                };
            }

            // Decide which queued inputs get resolved with `Cancelled` now vs.
            // preserved for the post-cancel drain:
            //
            // * rewind: the front was already popped above; respond to nothing.
            // * hard teardown (`kill_background_tasks`, the subagent-shutdown
            //   path that sends `Shutdown` next): drain the WHOLE queue — there
            //   is no point starting the next prompt and draining resolves every
            //   queued input's `respond_to` cleanly.
            // * normal cancel: remove the running turn; only an interactive stop
            //   (Ctrl+C / Esc / [stop]) also removes queued task/workflow
            //   completion wakes. Preserve real user prompts
            //   and unrelated synthetic entries so `maybe_start_running_task` can
            //   promote the next genuine user turn.
            //   The cancelling client does not pull any prompt back into its
            //   input — the server queue is the single source of truth for what
            //   runs next. Previously every cancel did `std::mem::take`,
            //   discarding the whole queue server-side; because no broadcast
            //   followed, clients kept a stale mirror and the queue only visibly
            //   vanished on the next prompt's (now-empty) broadcast.
            //
            // The in-flight turn is always `pending_inputs.front()`
            // (`maybe_start_running_task` promotes the front WITHOUT popping it;
            // `handle_completion` pops it at turn end) — so index 0 is the slot
            // whose `respond_to` the client is awaiting and whose spinner is
            // showing. We ALWAYS resolve it with `Cancelled`, regardless of
            // whether `running_task` is currently `Some`: in the narrow windows
            // where the front has no live task (e.g. a completion was just
            // dequeued and the next prompt not yet promoted, or a cancel races
            // ahead of `maybe_start_running_task`), gating on `running_task`
            // would drop the front's `respond_to`, hanging the client's
            // `session/prompt` forever (the TUI spinner never returns to idle).
            let pending_inputs = if rewound_input.is_some() {
                VecDeque::new()
            } else if kill_background_tasks {
                std::mem::take(&mut state.pending_inputs)
            } else {
                let mut kept = VecDeque::with_capacity(state.pending_inputs.len());
                let mut cancelled = VecDeque::new();
                for (idx, item) in std::mem::take(&mut state.pending_inputs)
                    .into_iter()
                    .enumerate()
                {
                    let is_running_turn = idx == 0;
                    if is_running_turn {
                        cancelled.push_back(item);
                    } else if suppress_task_wakes
                        && matches!(
                            item.input_origin.as_prompt_origin(),
                            super::PromptOrigin::TaskCompleted { .. }
                                | super::PromptOrigin::WorkflowCompleted { .. }
                        )
                    {
                        if let Some(fallback) = item.task_wake_fallback {
                            Self::push_task_wake_fallback(&mut state, fallback);
                        }
                        Self::respond_removed_prompt(item.respond_to);
                    } else {
                        kept.push_back(item);
                    }
                }
                state.pending_inputs = kept;
                cancelled
            };
            // Whether a user prompt remains queued behind the just-cancelled
            // turn. It distinguishes the next turn's redirect kind for
            // telemetry: `queued_after_cancel` (a queued prompt is promoted)
            // vs `cancel_then_send` (the user types a fresh prompt). Synthetic
            // inputs (auto-wake / nudges) are not user redirects.
            let had_queued_user_prompt = state
                .pending_inputs
                .iter()
                .any(|i| !i.input_origin.is_synthetic());
            // NOTE: `current_prompt_id` is deliberately NOT cleared here —
            // cancel usage attribution must snapshot the ledger against the
            // live pin first; it is cleared below, right after the
            // `finalize_usage_from_outcome` / `snapshot_prompt_usage` call.
            (
                running_task,
                pending_inputs,
                rewound_input,
                had_queued_user_prompt,
                turn_epoch,
            )
        };
        // Authoritative cancel identity: the task actually torn down. The pin
        // is only a fallback for windows where a cancel raced ahead of the
        // promote (no task yet) — see the capture comment above.
        let cancelled_prompt_id = running_task
            .as_ref()
            .map(|t| t.prompt_id.clone())
            .or(pinned_prompt_id);

        if rewound_input.is_none()
            && let Some(prompt_id) = cancelled_prompt_id.as_deref()
            && let Some(reason) = cancel_reason
        {
            self.report_cancelled_turn(CancelledTurn {
                prompt_id,
                epoch: turn_epoch,
                reason,
                trigger: trigger.as_ref().map(|t| t.as_str().to_string()),
                last_assistant_message,
            });
        }

        self.agent
            .borrow()
            .tool_bridge()
            .update_resource(
                echo_agent_tools::implementations::echo_agent_build::task::types::CurrentPromptIdResource(
                    String::new(),
                ),
            )
            .await;
        // Cancellation aborts the in-turn goal loop before its post-loop
        // cleanup runs, so clear the goal-loop flag here too.
        self.set_goal_loop_active_resource(false).await;

        self.events.cancel_active_tool();
        // No prompt id means no turn ran, and closing a fresh session would
        // otherwise write a `TurnEnded` with no `turn_started`.
        if rewound_input.is_none() && cancelled_prompt_id.is_some() {
            // The trigger (esc / ctrl_c / …) rides in the events.jsonl
            // `cancellation_context`; the category stays `MidTurnAbort` so the
            // existing dashboards/dataset keep working.
            let cancellation_context = trigger
                .as_ref()
                .map(|t| serde_json::json!({ "trigger": t.as_str() }));
            self.emit_turn_ended(
                crate::session::events::TurnOutcomeLabel::Cancelled,
                Some(crate::session::events::CancellationCategory::MidTurnAbort),
                cancellation_context,
            );
            // Mark the next real user prompt as following a mid-turn abort so
            // replay/analytics/the model can see the user stopped this turn.
            // Send-now is a silent cancel-and-send: the user is continuing,
            // not aborting, so it must not arm the interrupt category or the
            // interrupt envelope for its own continuation turn
            // (mirrors the cancel-rate skip above).
            if !send_now {
                self.events.set_prior_interrupt_category(
                    crate::session::events::CancellationCategory::MidTurnAbort,
                );
            }
            // Interjection frame only if this abort cut assistant text and left
            // no other signal (a dangling tool already emits a cancelled result).
            if !send_now
                && !self.chat_state_handle.has_dangling_tool_calls().await
                && self
                    .chat_state_handle
                    .get_last_assistant_text_in_turn()
                    .await
                    .is_some()
            {
                self.events.set_pending_interrupt_reminder();
            }
            // Shared `redirect_kind` for the data pipeline: the next user turn's
            // `turn_started` records HOW the user redirected after this abort.
            self.events
                .set_prior_redirect_kind(if had_queued_user_prompt {
                    crate::session::events::RedirectKind::QueuedAfterCancel
                } else {
                    crate::session::events::RedirectKind::CancelThenSend
                });
        }

        // After the abort, so the turn no longer owns the commands it started.
        if send_now {
            self.background_foreground_commands_for_send_now().await;
        }

        if let Some(is_turn_active) = &self.tool_context.is_turn_active {
            is_turn_active.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        // The aborted turn's `BlockingWaitGuard`s drop asynchronously (they
        // live in tool futures owned by the drainer task / subagent spawn
        // task). Until they do, `queue_input` would read a stale depth > 0 and
        self.tool_context.blocking_wait_depth.reset();
        self.flush_pending_skill_reminders().await;

        // No multi-second drain here (actor loop would block RecordSubagentUsage).
        // Same UsageDrainOutcome policy as freeze via finalize_usage_from_outcome.
        let cancelled_usage = if rewound_input.is_none() {
            if let Some(ref prompt_id) = cancelled_prompt_id {
                let reply = self.outstanding_reply_for_prompt(prompt_id).await;
                let outcome =
                    super::turn::UsageDrainOutcome::from_outstanding_reply(reply.as_ref());
                self.finalize_usage_from_outcome(prompt_id, outcome).await
            } else if !pending_inputs.is_empty() {
                self.snapshot_prompt_usage().await
            } else {
                None
            }
        } else {
            None
        };
        {
            let mut current_prompt_id = self
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned");
            *current_prompt_id = None;
        }
        let turn_stopped = cancelled_prompt_id.is_some() && cancel_reason.is_some();
        // Announced for every cancel that stopped a turn, including a rewind and a
        // cancel-and-send. A no-op if the turn task announced first.
        if cancelled_prompt_id.is_some() {
            self.notify_turn_abort(
                turn_epoch,
                echo_agent_lifecycle::TurnAbortReason::Interrupted,
            )
            .await;
        }
        if rewound_input.is_none()
            && let Some(prompt_id) = cancelled_prompt_id
        {
            // `cancelTrigger` tells clients a send-now cancel from Ctrl+C/Esc;
            // `MidTurnAbort` matches what the prompt's RPC resolves with below
            // so the two rails agree.
            self.emit_turn_completed(
                prompt_id,
                &Ok(acp::StopReason::Cancelled),
                cancelled_usage.clone(),
                trigger.as_ref().map(crate::session::CancelTrigger::as_str),
                Some(crate::session::commands::meta_category_str(
                    crate::session::events::CancellationCategory::MidTurnAbort,
                )),
            )
            .await;
        }

        if let Some(input) = rewound_input {
            return self
                .finish_rewound_cancel(input, None, total_tokens, turn_stopped)
                .await;
        }

        for (idx, input) in pending_inputs.into_iter().enumerate() {
            // Running turn is idx 0; queued prompts never spent tokens.
            let is_running_turn = idx == 0;
            if let Some(task_id) = input.input_origin.completion_id()
                && let Some(reservations) = &self.tool_context.task_completion_reservations
            {
                reservations.release(task_id);
            }
            let _ = input
                .respond_to
                .send(Ok(PromptTurnOk {
                    stop_reason: acp::StopReason::Cancelled,
                    total_tokens,
                    turn_snapshot: None,
                    completion_kind: PromptCompletionKind::Cancelled {
                        // Running turn only, matching events.jsonl's category;
                        // queued prompts were removed, not mid-turn aborted.
                        category: is_running_turn
                            .then_some(crate::session::events::CancellationCategory::MidTurnAbort),
                        // Thread the trigger on the running turn only (idx 0);
                        // MvpAgent stamps it on the `PromptResponse` `_meta`.
                        context: if is_running_turn {
                            trigger.as_ref().map(|t| {
                                crate::session::commands::CancellationContext {
                                    trigger: Some(t.as_str().to_string()),
                                    ..Default::default()
                                }
                            })
                        } else {
                            None
                        },
                    },
                    structured_output: None,
                    usage: if is_running_turn {
                        cancelled_usage.clone()
                    } else {
                        None
                    },
                    // Only the running turn (idx 0) ran, so only it echoes a bound; a queued prompt
                    // that never promoted attests nothing (like respond_removed_prompt).
                    tool_overrides: if is_running_turn {
                        self.effective_tool_overrides()
                    } else {
                        None
                    },
                }))
                .ok();
        }
        CancelOutcome {
            barrier: if suppress_task_wakes {
                WakeBarrier::Armed
            } else {
                WakeBarrier::Clear
            },
            turn_stopped,
        }
    }

    /// The user sent a message mid-turn: move any running command to the background instead of killing it.
    /// Each one's tool call gets an honest answer saying the command is still running.
    async fn background_foreground_commands_for_send_now(&self) {
        // This session and its subagents share one terminal, so move only this session's own commands.
        // Replying to another session's command would put an answer in this session's history for a question it never asked.
        let owner = Some(self.session_info.id.0.as_ref());
        let backgrounded = {
            self.agent
                .borrow()
                .tool_bridge()
                .background_foreground_commands(owner)
                .await
        };

        // Empty can mean nothing was running, or that this backend cannot background at all. Kill, so the command does not outlive its turn.
        // A kill does not report which commands it stopped, so `repair_dangling_tool_calls` answers the tool call before the next request.
        if backgrounded.is_empty() {
            self.kill_foreground_commands_for_cancel().await;
        }

        for bg in backgrounded {
            // No command text: the model already has it in the tool call this answers.
            let message = format!(
                "Command was moved to the background because the user sent a new message. \
                 The process is still running, not cancelled. Retrieve its output later \
                 with {} (task_id: {}).",
                self.tool_context.task_output_tool_name, bg.tool_call_id
            );
            self.chat_state_handle
                .push_tool_result(ConversationItem::tool_result(bg.tool_call_id, message));
        }
    }

    /// Kill the foreground commands this cancel owns. A subagent kills only its own, never the parent's or a sibling's on the shared backend.
    async fn kill_foreground_commands_for_cancel(&self) {
        if self.startup_hints.is_subagent {
            self.agent
                .borrow()
                .tool_bridge()
                .kill_foreground_commands_by_owner(&self.session_info.id.0)
                .await;
        } else {
            self.agent
                .borrow()
                .tool_bridge()
                .kill_foreground_commands()
                .await;
        }
    }
}

#[cfg(test)]
mod task_slot_tests {
    // Exercises the shared `TaskSlot<T>` primitive that backs both the deferred
    // prefix and the idle-notification debounce: arm / take / cancel / re-arm.
    use super::TaskSlot;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Spawn a 60s task that bumps `fired` by `by`, armed into `slot`.
    fn arm_counter(slot: &TaskSlot<()>, fired: &Arc<AtomicUsize>, by: usize) {
        let f = Arc::clone(fired);
        slot.arm(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            f.fetch_add(by, Ordering::SeqCst);
        }));
    }

    /// A task left armed runs to completion once its delay elapses.
    #[tokio::test(start_paused = true)]
    async fn armed_task_fires_after_delay() {
        let fired = Arc::new(AtomicUsize::new(0));
        let slot: TaskSlot<()> = TaskSlot::new();
        arm_counter(&slot, &fired, 1);

        let task = slot.take().expect("task armed");
        tokio::time::advance(Duration::from_secs(61)).await;
        let _ = task.await;

        assert_eq!(fired.load(Ordering::SeqCst), 1, "armed task must fire");
    }

    /// `cancel()` aborts a still-pending task (the new-user-prompt reset path).
    #[tokio::test(start_paused = true)]
    async fn cancel_aborts_pending_task() {
        let fired = Arc::new(AtomicUsize::new(0));
        let slot: TaskSlot<()> = TaskSlot::new();
        arm_counter(&slot, &fired, 1);

        tokio::time::advance(Duration::from_secs(30)).await;
        slot.cancel();
        assert!(slot.take().is_none(), "cancel must clear the slot");
        tokio::time::advance(Duration::from_secs(120)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "cancelled task must not fire"
        );
    }

    /// Arming a new task aborts the previous one so only the latest fires.
    #[tokio::test(start_paused = true)]
    async fn rearm_aborts_previous_task() {
        let fired = Arc::new(AtomicUsize::new(0));
        let slot: TaskSlot<()> = TaskSlot::new();
        arm_counter(&slot, &fired, 1);
        arm_counter(&slot, &fired, 10);

        let task = slot.take().expect("task armed");
        tokio::time::advance(Duration::from_secs(61)).await;
        let _ = task.await;
        tokio::task::yield_now().await;

        assert_eq!(
            fired.load(Ordering::SeqCst),
            10,
            "only the re-armed task fires"
        );
    }
}
