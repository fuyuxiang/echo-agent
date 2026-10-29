//! Hook dispatch concern for `SessionActor`: run contexts, hook execution
//! notifications and telemetry, and turn/tool outcome mapping.

use super::*;

/// Map a turn result to the hub protocol's `TurnHookOutcome`.
pub(super) fn turn_result_to_hook_outcome(
    result: &Result<TurnOutcome, acp::Error>,
) -> echo_agent_tool_protocol::turn_hook::TurnHookOutcome {
    use echo_agent_tool_protocol::turn_hook::TurnHookOutcome;
    match result {
        Ok(TurnOutcome::Completed { .. }) | Ok(TurnOutcome::StationarityEnded { .. }) => {
            TurnHookOutcome::Completed
        }
        Ok(TurnOutcome::Cancelled { .. }) | Ok(TurnOutcome::MaxTurnsReached { .. }) => {
            TurnHookOutcome::Cancelled
        }
        Err(_) => TurnHookOutcome::Error,
    }
}

/// Encode a [`CancellationCategory`](crate::session::events::CancellationCategory)
/// as its bare snake_case wire string for the `after_turn` hook payload.
/// Deliberately `serde_json::to_value` + `as_str`, NOT `to_string` — the
/// latter yields the quoted form and fails the workspace decode.
pub(super) fn cancellation_category_to_wire_string(
    category: Option<crate::session::events::CancellationCategory>,
) -> Option<String> {
    let category = category?;
    serde_json::to_value(category)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
}

/// Map shell-internal `ToolOutcome` to the hub protocol's `ToolCallOutcome`.
///
/// The shell tracks 9 granular outcome variants; the hub protocol has 3:
/// - `Success` → tool executed and returned a result
/// - `Error` → tool failed to execute or was invalid
/// - `Cancelled` → tool never ran (permission, doom-loop, hook, followup)
pub(super) fn map_tool_outcome(
    outcome: crate::session::events::ToolOutcome,
) -> echo_agent_tool_protocol::session_event::ToolCallOutcome {
    use crate::session::events::ToolOutcome;
    use echo_agent_tool_protocol::session_event::ToolCallOutcome;
    match outcome {
        ToolOutcome::Success => ToolCallOutcome::Success,
        ToolOutcome::Error | ToolOutcome::InvalidTool => ToolCallOutcome::Error,
        ToolOutcome::PermissionRejected
        | ToolOutcome::PermissionCancelled
        | ToolOutcome::Followup
        | ToolOutcome::HookDenied
        | ToolOutcome::Cancelled => ToolCallOutcome::Cancelled,
    }
}

/// Returns `(notification_type, message, title, level)` when this update should
/// trigger a vendor-compatible `Notification` hook.
///
/// Internal/high-frequency updates (hook scrollback, retry progress, config
/// changes) are excluded so migrated hooks only fire on user-attention
/// events — not on every tool call or session tick. `DiffReview` always waits
/// on the user, so it is safe to fire `permission_prompt` here.
#[allow(clippy::type_complexity)]
pub(super) fn notification_hook_for_update(
    update: &EchoAgentSessionUpdate,
) -> Option<(String, Option<String>, Option<String>, Option<String>)> {
    match update {
        EchoAgentSessionUpdate::DiffReview { .. } => Some((
            "permission_prompt".into(),
            Some("Diff review requested".into()),
            None,
            Some("info".into()),
        )),
        EchoAgentSessionUpdate::AutoRecoveryExhausted { error, .. } => Some((
            "agent_error".into(),
            Some(error.clone()),
            None,
            Some("error".into()),
        )),
        EchoAgentSessionUpdate::RetryState(RetryState::Exhausted { reason, .. }) => Some((
            "agent_error".into(),
            Some(reason.clone()),
            None,
            Some("error".into()),
        )),
        EchoAgentSessionUpdate::RetryState(RetryState::Failed { message, .. }) => Some((
            "agent_error".into(),
            Some(message.clone()),
            None,
            Some("error".into()),
        )),
        _ => None,
    }
}

impl SessionActor {
    pub(super) fn hook_run_ctx(&self) -> echo_agent_hooks::runner::RunContext<'_> {
        echo_agent_hooks::runner::RunContext {
            session_id: &self.session_info.id.0,
            workspace_root: &self.hook_resolved_workspace_root,
            process_scope: self.tool_context.process_scope.clone(),
        }
    }

    /// Send a hook annotation to the TUI scrollback.
    ///
    /// Rendered inline with the preceding tool call block rather than as
    /// a separate agent message.
    pub(super) async fn send_hook_annotation(&self, message: &str) {
        self.send_echo_agent_notification(EchoAgentSessionUpdate::HookAnnotation {
            message: message.to_string(),
        })
        .await;
    }

    /// Send structured hook execution data for rich scrollback rendering.
    ///
    /// `prompt_id` is `None` for session-level dispatches (session_start /
    /// session-end stop).
    pub(super) async fn send_hook_execution(
        &self,
        event_name: &str,
        tool_name: Option<&str>,
        prompt_id: Option<&str>,
        results: &[echo_agent_hooks::result::HookRunResult],
    ) {
        if results.is_empty()
            || results
                .iter()
                .all(|r| matches!(r, echo_agent_hooks::result::HookRunResult::Skipped { .. }))
        {
            return;
        }
        use crate::extensions::notification::{HookRunEntryDto, HookRunStatusDto};
        use echo_agent_hooks::result::HookRunResult;

        let runs: Vec<HookRunEntryDto> = results
            .iter()
            .map(|r| {
                let (name, status) = match r {
                    HookRunResult::Success {
                        hook_name, elapsed, ..
                    } => (
                        hook_name.clone(),
                        HookRunStatusDto::Success {
                            elapsed_ms: elapsed.as_millis() as u64,
                        },
                    ),
                    HookRunResult::Skipped { hook_name } => {
                        (hook_name.clone(), HookRunStatusDto::Skipped)
                    }
                    HookRunResult::Blocked {
                        hook_name,
                        detail,
                        elapsed,
                        ..
                    } => (
                        hook_name.clone(),
                        HookRunStatusDto::Failed {
                            error: detail.clone(),
                            elapsed_ms: elapsed.as_millis() as u64,
                            blocked: true,
                        },
                    ),
                    HookRunResult::Failed {
                        hook_name,
                        error,
                        elapsed,
                        ..
                    } => (
                        hook_name.clone(),
                        HookRunStatusDto::Failed {
                            error: error.clone(),
                            elapsed_ms: elapsed.as_millis() as u64,
                            blocked: false,
                        },
                    ),
                };
                HookRunEntryDto {
                    name,
                    status,
                    output: None,
                }
            })
            .collect();

        self.send_echo_agent_notification(EchoAgentSessionUpdate::HookExecution {
            event_name: event_name.to_string(),
            tool_name: tool_name.map(|s| s.to_string()),
            prompt_id: prompt_id.map(|s| s.to_string()),
            runs,
        })
        .await;
    }

    /// Returns the resolved workspace root for hook envelopes.
    pub(super) fn hook_workspace_root(&self) -> String {
        self.hook_resolved_workspace_root.clone()
    }

    /// Subagent type for tool-hook attribution, or `None` for the top-level session. Prefers
    /// the task `subagent_type`, falling back to the agent definition name for older spawns.
    pub(super) fn subagent_type_label(&self) -> Option<String> {
        if !self.startup_hints.is_subagent {
            return None;
        }
        Some(
            self.startup_hints
                .subagent_type
                .clone()
                .unwrap_or_else(|| self.agent.borrow().definition().name.clone()),
        )
    }

    /// The session's current permission mode (`plan` / `bypassPermissions` / `auto` / `default`).
    pub(super) fn permission_mode_label(&self) -> &'static str {
        if self.plan_mode.lock().is_active() {
            "plan"
        } else if self.permissions.is_yolo_mode() {
            "bypassPermissions"
        } else if self.permissions.is_auto_mode() {
            "auto"
        } else {
            "default"
        }
    }

    /// Dispatch a non-blocking hook event: build the envelope, fire observe-only
    /// client hooks, then run the on-disk registry. No-op (no payload built) when no
    /// hook listens for `event`, so it stays inert when unused.
    pub(super) async fn dispatch_hook(
        &self,
        event: echo_agent_hooks::event::HookEventName,
        payload: echo_agent_hooks::event::HookPayload,
        prompt_id: Option<&str>,
        tool_name: Option<&str>,
    ) {
        // Repository hooks can run arbitrary commands. A read-only model turn
        // must not gain an execution channel indirectly through a read tool.
        if self.is_readonly_turn() || !self.may_have_hooks_for(event) {
            return;
        }
        // Fires observe-only client hooks before (and independent of) the on-disk registry guard below.
        let envelope = self.fire_hook(event, prompt_id.map(|s| s.to_string()), payload);
        let Some(registry) = self.allowed_hook_registry() else {
            return;
        };
        let ctx = self.hook_run_ctx();
        let results =
            echo_agent_hooks::dispatcher::dispatch_non_blocking(&registry, event, &envelope, &ctx)
                .await;
        self.send_hook_execution(&event.to_string(), tool_name, prompt_id, &results)
            .await;
        self.emit_hook_executed_telemetry(&event.to_string(), tool_name, &results)
            .await;
    }

    pub(super) async fn emit_hook_executed_telemetry(
        &self,
        event_name: &str,
        tool_name: Option<&str>,
        results: &[echo_agent_hooks::result::HookRunResult],
    ) {
        let tool = tool_name.map(|s| s.to_string());
        for r in results {
            let (hook_name, elapsed, outcome) = match r {
                echo_agent_hooks::result::HookRunResult::Success {
                    hook_name, elapsed, ..
                } => (
                    hook_name,
                    elapsed,
                    echo_agent_telemetry::events::HookOutcome::Success,
                ),
                echo_agent_hooks::result::HookRunResult::Blocked {
                    hook_name, elapsed, ..
                } => (
                    hook_name,
                    elapsed,
                    echo_agent_telemetry::events::HookOutcome::Blocked,
                ),
                echo_agent_hooks::result::HookRunResult::Failed {
                    hook_name, elapsed, ..
                } => (
                    hook_name,
                    elapsed,
                    echo_agent_telemetry::events::HookOutcome::Error,
                ),
                echo_agent_hooks::result::HookRunResult::Skipped { .. } => continue,
            };
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::HookExecuted {
                    hook_name: hook_name.clone(),
                    event: event_name.to_string(),
                    tool_name: tool.clone(),
                    duration_ms: elapsed.as_millis() as u64,
                    outcome,
                },
            );
        }
    }
}

#[cfg(test)]
mod notification_hook_filter_tests {
    use super::*;
    use crate::extensions::notification::{
        FeedbackRequestNotification, HookRunEntryDto, HookRunStatusDto, RetryState,
    };

    #[test]
    fn hook_updates_do_not_fire_notification_hook() {
        let execution = EchoAgentSessionUpdate::HookExecution {
            event_name: "pre_tool_use".into(),
            tool_name: Some("read_file".into()),
            prompt_id: None,
            runs: vec![HookRunEntryDto {
                name: "test".into(),
                status: HookRunStatusDto::Success { elapsed_ms: 1 },
                output: None,
            }],
        };
        assert!(notification_hook_for_update(&execution).is_none());

        let annotation = EchoAgentSessionUpdate::HookAnnotation {
            message: "running hooks".into(),
        };
        assert!(notification_hook_for_update(&annotation).is_none());
    }

    #[test]
    fn retry_in_progress_does_not_fire_notification_hook() {
        let update = EchoAgentSessionUpdate::RetryState(RetryState::Retrying {
            attempt: 1,
            max_retries: 3,
            reason: "timeout".into(),
        });
        assert!(notification_hook_for_update(&update).is_none());
    }

    #[test]
    fn feedback_request_does_not_fire_notification_hook() {
        let update = EchoAgentSessionUpdate::FeedbackRequest(FeedbackRequestNotification {
            request_id: "req-1".into(),
            tier: "tier1".into(),
            prompt: "How was this session?".into(),
            dismissible: true,
            trigger_type: "tier1_engagement".into(),
            trigger_condition: "turns >= 10".into(),
            trigger_reason: "long session".into(),
            stars: true,
            thumbs: false,
            text: false,
        });
        assert!(notification_hook_for_update(&update).is_none());
    }

    #[test]
    fn diff_review_fires_permission_prompt() {
        let update = EchoAgentSessionUpdate::DiffReview { content: vec![] };
        let (ty, message, _, level) = notification_hook_for_update(&update).expect("should fire");
        assert_eq!(ty, "permission_prompt");
        assert_eq!(message.as_deref(), Some("Diff review requested"));
        assert_eq!(level.as_deref(), Some("info"));
    }

    #[test]
    fn task_completed_does_not_fire_via_filter() {
        let update = EchoAgentSessionUpdate::TaskCompleted {
            task_snapshot: echo_agent_tools::types::TaskSnapshot {
                task_id: "task-1".into(),
                command: "echo hi".into(),
                display_command: None,
                cwd: "/tmp".into(),
                start_time: std::time::SystemTime::UNIX_EPOCH,
                end_time: None,
                output: String::new(),
                output_file: std::path::PathBuf::from("/tmp/out"),
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
            },
            will_wake: false,
        };
        assert!(notification_hook_for_update(&update).is_none());
    }
}
