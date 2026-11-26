//! Turn-completion concern for `SessionActor`: completion handling
//! and turn-error classification.

use super::turn_end_hooks::{cancel_details, cancel_reason_for_completion};
use super::*;

fn completion_cancel_trigger(result: &PromptTurnResult) -> Option<&str> {
    match result.as_ref().ok()?.completion_kind {
        PromptCompletionKind::Cancelled {
            context: Some(ref ctx),
            ..
        } => ctx.trigger.as_deref(),
        _ => None,
    }
}

impl SessionActor {
    /// Emit a cosmetic `Plan` update at turn end to clear stale spinners.
    ///
    /// When the model produces its final text response without a cleanup
    /// `todo_write` call, any remaining `in_progress` items leave stale
    /// spinners in the UI.
    ///
    /// This method **does not mutate** the underlying `TodoState` — the
    /// persisted resource state and the model's view of the todo list
    /// are unchanged.  It only emits a transient (non-persisted) `Plan`
    /// notification where `in_progress` entries are mapped to `completed`
    /// for display.
    ///
    /// Uses the canonical `plan_entry_from_todo_item` helper to preserve
    /// cancelled metadata, priorities, and other semantics.
    ///
    /// No-op if no `in_progress` items exist.
    pub(super) async fn emit_turn_end_plan_cleanup(&self) {
        use crate::tools::todo::{TodoState, TodoStatus, plan_entry_from_todo_item};
        use echo_agent_tools::types::resources::State;

        // Read the current TodoState (no mutation).
        let (entries, stale_count) = {
            let res = self
                .agent
                .borrow()
                .tool_bridge()
                .read_resource::<State<TodoState>>()
                .await;
            let Some(state) = res else {
                return; // No todo state at all.
            };

            let stale_count = state
                .0
                .todo_items()
                .filter(|t| t.status == TodoStatus::InProgress)
                .count();
            if stale_count == 0 {
                return;
            }

            // Build plan entries with in_progress → completed for display.
            // Uses the canonical `plan_entry_from_todo_item` helper to
            // preserve cancelled metadata, priority, and other semantics.
            let entries: Vec<_> = state
                .0
                .todo_items()
                .map(|item| {
                    let mut entry = plan_entry_from_todo_item(item.clone());
                    if item.status == TodoStatus::InProgress {
                        entry.status = acp::PlanEntryStatus::Completed;
                    }
                    entry
                })
                .collect();

            (entries, stale_count)
        };

        tracing::info!(
            stale_count,
            "emitting transient turn-end Plan cleanup — in_progress shown as completed"
        );

        // Use transient notification — this is a cosmetic UI update that
        // must NOT be persisted or replayed on session reload.  The real
        // TodoState in Resources is the source of truth.
        let notification = acp::SessionNotification::new(
            self.session_info.id.clone(),
            acp::SessionUpdate::Plan(acp::Plan::new(entries)),
        );
        self.emit_transient_notification(notification);
    }

    /// Emit `echo.agent/git_head_changed` after an edit/shell command that may have
    /// moved HEAD (e.g. `git checkout`, `git commit`), so clients update their
    /// status bar and changes panel immediately rather than waiting for the
    /// debounced fs-watch refresh.
    pub(super) async fn maybe_notify_git_branch(&self) {
        if !self.git_head_enabled {
            return;
        }
        let cwd = self.tool_context.cwd.as_path();

        // `get_worktree_info` doubles as the "in a git repo?" probe (None when not).
        let (worktree_info, branch, commit) = tokio::join!(
            echo_agent_workspace::session::git::get_worktree_info(cwd),
            echo_agent_workspace::session::git::get_branch(cwd),
            echo_agent_workspace::session::git::get_current_commit(cwd),
        );
        let Some((is_worktree, main_repo)) = worktree_info else {
            return;
        };

        let dedup_key = git_head_dedup_key(
            branch.as_deref(),
            is_worktree,
            main_repo.as_deref(),
            commit.as_deref(),
        );
        {
            let mut last = self.last_reported_branch.lock();
            if last.as_deref() == Some(&dedup_key) {
                return;
            }
            *last = Some(dedup_key);
        }

        let params = echo_agent_workspace::session::git::GitHeadChanged {
            session_id: self.session_info.id.0.to_string(),
            branch,
            is_worktree,
            main_repo,
        };
        if let Ok(raw) = serde_json::value::to_raw_value(&params) {
            let notification = acp::ExtNotification::new("echo.agent/git_head_changed", raw.into());
            self.notifications
                .gateway
                .forward_fire_and_forget(notification);
        }

        // The row carries `workspace.branch`, and HEAD has just moved. Inside
        // the `git_head_enabled` gate above, so a client that did not ask for
        // HEAD notifications refreshes its row at turn end instead.
        self.emit_status_snapshot_detached();
    }

    /// Live subagents and sticky usage-not-applied. `None` if the query failed.
    pub(super) async fn outstanding_reply_for_prompt(
        &self,
        prompt_id: &str,
    ) -> Option<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentOutstandingReply,
    > {
        let Some(tx) = &self.tool_context.subagent_event_tx else {
            return Some(Default::default());
        };
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentOutstandingRequest,
        };
        let (respond_to, rx) = tokio::sync::oneshot::channel();
        if tx
            .send(SubagentEvent::Outstanding(SubagentOutstandingRequest {
                parent_session_id: self.session_id_string(),
                prompt_id: prompt_id.to_string(),
                respond_to,
            }))
            .is_err()
        {
            return None;
        }
        rx.await.ok()
    }

    /// Report-level incomplete (error-path attach, tests). Same OR as
    /// [`super::turn::UsageDrainOutcome::report_incomplete`].
    pub(super) fn usage_incomplete_from_reply(
        reply: Option<
            &echo_agent_tools::implementations::echo_agent_build::task::types::SubagentOutstandingReply,
        >,
    ) -> bool {
        super::turn::UsageDrainOutcome::from_outstanding_reply(reply).report_incomplete()
    }

    pub(super) fn clear_subagent_usage_not_applied(&self, prompt_id: &str) {
        let Some(tx) = &self.tool_context.subagent_event_tx else {
            return;
        };
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentClearUsageNotAppliedRequest, SubagentEvent,
        };
        let _ = tx.send(SubagentEvent::ClearUsageNotApplied(
            SubagentClearUsageNotAppliedRequest {
                parent_session_id: self.session_id_string(),
                prompt_id: prompt_id.to_string(),
            },
        ));
    }

    /// Returns whether this handler OWNED the completion (it matched and
    /// dequeued the front prompt). `false` means a stale completion for a
    /// turn another path (e.g. Cancel) already finalized — callers must not
    /// treat it as a turn ending now.
    pub(super) async fn handle_completion(
        &self,
        prompt_id: String,
        result: PromptTurnResult,
    ) -> bool {
        let result = result.map(|mut ok| {
            ok.tool_overrides = self.effective_tool_overrides();
            ok
        });
        let became_idle = {
            let mut current_prompt_id = self
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned");
            if current_prompt_id.as_deref() == Some(prompt_id.as_str()) {
                *current_prompt_id = None;
            }
            current_prompt_id.is_none()
        };
        if became_idle {
            self.flush_pending_skill_reminders().await;
            // Idle-gated: a stale completion must not clobber the promoted turn's resources.
            self.agent
                .borrow()
                .tool_bridge()
                .update_resource(
                    echo_agent_tools::implementations::echo_agent_build::task::types::CurrentPromptIdResource(
                        String::new(),
                    ),
                )
                .await;
            // Goal turn is over — re-enable per-tool-call completion reminders.
            self.set_goal_loop_active_resource(false).await;
        }

        // Read before the lock: the report below runs under it and cannot await.
        let last_assistant_message = match result.as_ref().ok() {
            Some(ok) if cancel_reason_for_completion(&ok.completion_kind).is_some() => {
                self.last_assistant_message_for_cancel().await
            }
            _ => None,
        };
        let mut state = self.state.lock().await;
        // True only when this completion matched the front prompt and dequeued
        // it. The unknown-prompt branch below must NOT emit a terminal: a turn
        // the Cancel path already finalized can leave a stale completion here.
        let mut owned_completion = false;
        let mut broadcast_queue = false;
        if state
            .pending_inputs
            .front()
            .is_some_and(|input| input.prompt_id == prompt_id)
        {
            let Some(input) = state.pending_inputs.pop_front() else {
                return false;
            };
            owned_completion = true;
            let _ = input.respond_to.send(result.clone()).ok();
            // The completed prompt left the queue; re-broadcast (below, after
            // `running_task` clears so the wire's `running_prompt_id` doesn't
            // advertise the finished turn) the authoritative queue.
            broadcast_queue = input.queue_meta.is_some();
        } else {
            tracing::warn!("Received completion for unknown prompt: {prompt_id}");
            echo_agent_telemetry::unified_log::warn(
                "shell.turn.stale_completion_dropped",
                Some(self.session_info.id.0.as_ref()),
                Some(serde_json::json!({
                    "prompt_id": prompt_id,
                    "running_prompt_id": state.running_prompt_id(),
                })),
            );
        }
        // Owned (dequeued at the front) only: the unknown-prompt branch above is a stale
        // completion the Cancel path already finalized, and `RemovedFromQueue` never ran.
        let finalizes_turn = owned_completion
            && !matches!(
                result,
                Ok(PromptTurnOk {
                    completion_kind: PromptCompletionKind::RemovedFromQueue,
                    ..
                })
            );
        // Under the lock that promotion needs and before the task is cleared, so this
        // completion's turn is still the current one.
        if finalizes_turn
            && let Ok(ok) = &result
            && let Some(reason) = cancel_reason_for_completion(&ok.completion_kind)
        {
            self.report_turn_end(
                &prompt_id,
                TurnEnd::Cancelled {
                    reason,
                    trigger: completion_cancel_trigger(&result).map(str::to_string),
                    reason_details: cancel_details(&ok.completion_kind),
                    last_assistant_message,
                },
            );
        }
        // Ownership-gated: a stale completion must not null the promoted turn's
        // task, or `maybe_start_running_task` would double-spawn the prompt.
        if state.running_prompt_id() == Some(prompt_id.as_str()) || owned_completion {
            state.running_task = None;
        }
        if broadcast_queue {
            self.broadcast_queue_changed(&state);
        }
        // Note: Auto-compact is now handled inline during process_conversation_turn,
        // so we no longer need to queue it here after turn completion.

        // If the user toggled plan mode off while this turn was in-flight
        // (state == ExitPending), complete the deferred exit now that the
        // turn is finished. The next handle_prompt() will inject the exit
        // reminder via has_pending_exit_reminder().
        {
            let mut tracker = self.plan_mode.lock();
            let transitioned =
                tracker.state() == crate::session::plan_mode::PlanModeState::ExitPending;
            tracker.complete_deferred_exit();
            drop(tracker);
            if transitioned {
                self.persist_plan_mode_state();
            }
        }
        // Drop the state guard before the async emit so the persist/broadcast
        // fork doesn't run under the state lock.
        drop(state);

        // Durable twin of the fire-and-forget `prompt_complete` (emitted from
        // `MvpAgent::prompt`): publish the turn's terminal on the persisted +
        // replayed `_echo.agent/session/update` rail so a viewer that re-attaches
        // mid-turn finalizes from replay instead of stranding on "Waiting…".
        // The caller flushed the replay buffer first, so this lands strictly
        // after the turn's last `session/update` delta.
        if finalizes_turn {
            let mapped = result
                .as_ref()
                .map(|ok| ok.stop_reason)
                .map_err(Clone::clone);
            let usage = match &result {
                Ok(ok) => ok.usage.clone(),
                Err(err) => {
                    if let Some(u) = crate::sampling::error::prompt_usage_from_error(err) {
                        Some(u)
                    } else {
                        self.error_path_usage_fallback(&prompt_id).await
                    }
                }
            };
            // `cancellationCategory` on the terminal `_meta` lets re-attaching
            // viewers finalize with the same copy as the driver (a hook-denied
            // turn must not render as "cancelled by user").
            let cancellation_category = result
                .as_ref()
                .ok()
                .and_then(|ok| ok.completion_kind.cancellation_category_meta());
            self.emit_turn_completed(
                prompt_id,
                &mapped,
                usage,
                completion_cancel_trigger(&result),
                cancellation_category.as_deref(),
            )
            .await;
        }
        owned_completion
    }

    /// Emit the durable, replayable `TurnCompleted` terminal — the single
    /// chokepoint shared by the completion (`handle_completion`) and cancel
    /// (`cancel_running_task`) sites. Derives `(stop_reason, agent_result)`
    /// from the SAME source as the fire-and-forget `prompt_complete`
    /// (`prompt_complete_fields`), so the two signals never disagree, then
    /// persists + forwards via `send_echo_agent_notification`. Retiring
    /// `prompt_complete` later is then a one-line change at the call sites.
    ///
    /// `cancel_trigger` (when `Some`) rides the `_meta` as `cancelTrigger`;
    /// `"send_now"` marks a cancel-and-send end (marker suppressed).
    /// `cancellation_category` (when `Some`) rides as `cancellationCategory`
    /// (`meta_category_str`, e.g. `"HookDenied"`) so viewer rails pick
    /// category-aware terminal copy.
    ///
    /// Both callers queue the turn-end report before this, but the worker can dispatch first,
    /// so the terminal and the report race. The pager handles either order.
    pub(super) async fn emit_turn_completed(
        &self,
        prompt_id: String,
        mapped: &std::result::Result<acp::StopReason, acp::Error>,
        usage: Option<crate::extensions::notification::PromptUsage>,
        cancel_trigger: Option<&str>,
        cancellation_category: Option<&str>,
    ) {
        let (stop_reason, agent_result) = crate::sampling::error::prompt_complete_fields(mapped);
        let mut extra = serde_json::Map::new();
        if let Some(t) = cancel_trigger {
            extra.insert("cancelTrigger".to_string(), serde_json::json!(t));
        }
        if let Some(c) = cancellation_category {
            extra.insert("cancellationCategory".to_string(), serde_json::json!(c));
        }
        let extra_meta = (!extra.is_empty()).then_some(extra);
        self.send_echo_agent_notification_with_extra_meta(
            crate::session::turn_completion::build_turn_completed(
                prompt_id,
                stop_reason,
                agent_result,
                usage,
            ),
            extra_meta,
        )
        .await;

        // Cost, context occupancy and the turn timer all moved during the turn.
        self.emit_status_snapshot_detached();
    }

    /// Telemetry error category; delegates to `stop_failure_error_type` so the
    /// two classifications cannot drift.
    pub(super) fn classify_turn_error(err: &acp::Error) -> String {
        use echo_agent_hooks::event::StopFailureKind as K;
        match Self::stop_failure_error_type(err) {
            K::RateLimit => "rate_limit",
            K::AuthenticationFailed => "auth",
            K::InvalidRequest => "invalid_request",
            K::ServerError => "internal",
            K::MaxOutputTokens => "max_tokens",
            K::Unknown => "unknown",
        }
        .to_string()
    }

    /// The `StopFailure` hook input's classified `error`. Structured markers win
    /// over the JSON-RPC code because they are more specific; anything the
    /// runtime cannot distinguish stays `Unknown`.
    pub(super) fn stop_failure_error_type(
        err: &acp::Error,
    ) -> echo_agent_hooks::event::StopFailureKind {
        use echo_agent_hooks::event::StopFailureKind as K;
        if crate::sampling::error::stop_reason_for_turn_error(err) == "MaxTokens" {
            return K::MaxOutputTokens;
        }
        // The data-carried HTTP status discriminates over the JSON-RPC code. 403
        // is content-safety, not auth: it folds into `invalid_request` on the turn
        // path (carries `http_status: 403`) and `server_error` on the setup path
        // (no status, so `-32603` below).
        match crate::sampling::error::http_status_from_error(err) {
            Some(401) => return K::AuthenticationFailed,
            Some(429) | Some(503) | Some(529) => return K::RateLimit,
            Some(s) if (400..500).contains(&s) => return K::InvalidRequest,
            Some(s) if s >= 500 => return K::ServerError,
            _ => {}
        }
        match i32::from(err.code) {
            crate::sampling::error::RATE_LIMITED_ERROR_CODE => K::RateLimit,
            -32000 => K::AuthenticationFailed,
            -32002 | -32600 | -32602 => K::InvalidRequest,
            -32603 => K::ServerError,
            _ => K::Unknown,
        }
    }

    /// Whether a turn error is transient infra worth a goal retry. Keys on the
    /// JSON-RPC code only (unlike `stop_failure_error_type`), so `-32603` counts
    /// as infra.
    pub(super) fn is_infra_turn_error(err: &acp::Error) -> bool {
        matches!(
            i32::from(err.code),
            crate::sampling::error::RATE_LIMITED_ERROR_CODE | -32000 | -32603
        )
    }

    /// `(turn_succeeded, suppress_goal_continuation, infra_pause_message)`.
    /// StationarityEnded is success for the streak but skips GoalSummary re-queue.
    /// `infra_pause_message` is extracted before `handle_completion` consumes `result`.
    pub(super) fn post_turn_goal_degradation_plan(
        result: &PromptTurnResult,
    ) -> (bool, bool, Option<String>) {
        let suppress_goal_continuation = result.as_ref().ok().is_some_and(|ok| {
            matches!(
                ok.completion_kind,
                crate::session::commands::PromptCompletionKind::StationarityEnded
            )
        });
        let turn_cancelled = result.as_ref().ok().is_some_and(|ok| {
            matches!(
                ok.completion_kind,
                crate::session::commands::PromptCompletionKind::Cancelled { .. }
                    | crate::session::commands::PromptCompletionKind::MaxTurnsReached { .. }
            )
        });
        let turn_succeeded = result
            .as_ref()
            .ok()
            .is_some_and(|ok| !turn_cancelled && ok.stop_reason != acp::StopReason::Refusal);
        let infra_pause_message = result
            .as_ref()
            .err()
            .filter(|err| Self::is_infra_turn_error(err))
            .map(Self::format_turn_error_message);
        (
            turn_succeeded,
            suppress_goal_continuation,
            infra_pause_message,
        )
    }

    pub(super) async fn apply_infra_pause_after_turn_err(&self, message: String) -> bool {
        let slash_detail = match message.strip_prefix("Turn failed: ") {
            Some(rest) => rest.to_owned(),
            None => message.clone(),
        };
        let paused = self
            .auto_pause_goal_if_active_with_message(
                crate::session::goal_tracker::GoalPauseReason::Infra,
                message,
            )
            .await;
        if paused {
            self.send_slash_command_output(&format!(
                "Goal paused due to turn error: {slash_detail}. Use /goal resume to retry."
            ))
            .await;
        }
        paused
    }

    /// Extract the best human-readable detail from an infra turn error.
    pub(super) fn turn_error_detail(err: &acp::Error) -> Option<String> {
        err.data
            .as_ref()
            .and_then(crate::sampling::error::error_detail_from_data)
            .or_else(|| {
                if !err.message.is_empty() {
                    Some(err.message.clone())
                } else {
                    None
                }
            })
    }

    pub(super) fn format_turn_error_message(err: &acp::Error) -> String {
        if let Some(detail) = Self::turn_error_detail(err) {
            format!("Turn failed: {detail}")
        } else {
            format!("Turn failed: {}", Self::classify_turn_error(err))
        }
    }

    pub(super) fn classify_install_error(
        err: &echo_agent_core::plugins::install_registry::InstallError,
    ) -> String {
        crate::plugin::classify_install_error(err)
    }
}
