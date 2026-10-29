//! The turn-end `Stop`/`SubagentStop` gate for `SessionActor`.

use super::*;
use echo_agent_hooks::event::{
    self, BackgroundTaskType, StopBackgroundTask, StopSessionCron, clip_stop_entry_text,
};
use echo_agent_hooks::{dispatcher, result};

pub const MAX_STOP_HOOK_CONTINUATIONS_PER_TURN: u32 = 8;

const SESSION_END_STOP_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

fn commit_stop_report(claim: TurnReportClaim<'_>, prompt_id: &str) {
    if claim.commit() == CommitOutcome::LostToAnotherReporter {
        tracing::debug!(prompt_id, "a cancel already reported this turn");
    }
}

/// `command` is a shell-only field, so a monitor's watch command is carried in
/// `description` instead.
fn stop_entry_from_task(task: &echo_agent_tools::types::TaskSnapshot) -> StopBackgroundTask {
    let command_text =
        clip_stop_entry_text(task.display_command.as_deref().unwrap_or(&task.command));
    let (kind, command, description) = match task.kind {
        echo_agent_tools::computer::types::TaskKind::Bash => {
            (BackgroundTaskType::Shell, Some(command_text), None)
        }
        echo_agent_tools::computer::types::TaskKind::Monitor => {
            (BackgroundTaskType::Monitor, None, Some(command_text))
        }
    };
    StopBackgroundTask {
        id: task.task_id.clone(),
        r#type: kind,
        status: "running".to_string(),
        description,
        command,
        agent_type: None,
    }
}

fn stop_entry_from_subagent(
    summary: &echo_agent_tools::implementations::echo_agent_build::task::types::ActiveSubagentSummary,
) -> StopBackgroundTask {
    StopBackgroundTask {
        id: summary.subagent_id.clone(),
        r#type: BackgroundTaskType::Subagent,
        status: "running".to_string(),
        description: Some(clip_stop_entry_text(&summary.description)),
        command: None,
        agent_type: Some(summary.subagent_type.clone()),
    }
}

fn stop_cron_from_scheduled(
    task: &echo_agent_tools::implementations::echo_agent_build::scheduler::types::ScheduledTask,
) -> StopSessionCron {
    StopSessionCron {
        id: task.id.clone(),
        schedule:
            echo_agent_tools::implementations::echo_agent_build::scheduler::interval::interval_to_human(
                task.interval_secs,
            ),
        recurring: task.recurring,
        prompt: clip_stop_entry_text(&task.prompt),
    }
}

const STOP_FEEDBACK_TEXT_MAX: usize = 10_000;

fn format_stop_feedback(blocks: &[dispatcher::StopBlock], additional_context: &[String]) -> String {
    use std::fmt::Write as _;
    let clip = |text: &str| event::clip_text(text, STOP_FEEDBACK_TEXT_MAX);
    let mut feedback = String::new();
    if !blocks.is_empty() {
        feedback.push_str("Stop hook feedback:\n");
        for block in blocks {
            let _ = writeln!(feedback, "- {}", clip(&block.reason));
        }
    }
    for context in additional_context {
        if !feedback.is_empty() {
            feedback.push('\n');
        }
        feedback.push_str(&clip(context));
    }
    feedback
}

/// Downgrade `Blocked` to `Success` for the observe-only session-end fire: the
/// decision is discarded, so scrollback and telemetry must not report a block.
pub(super) fn demote_ignored_blocks(
    results: Vec<result::HookRunResult>,
) -> Vec<result::HookRunResult> {
    use echo_agent_hooks::result::HookRunResult;
    results
        .into_iter()
        .map(|result| match result {
            HookRunResult::Blocked {
                hook_name,
                elapsed,
                http_info,
                ..
            } => HookRunResult::Success {
                hook_name,
                elapsed,
                http_info,
            },
            other => other,
        })
        .collect()
}

impl SessionActor {
    /// Dispatch the observe-only session-end `Stop`: runs in stop-gate mode so
    /// exit code 2 parses as a block, but the decision is discarded (no turn
    /// left to continue).
    pub(crate) async fn dispatch_session_end_stop(&self, reason: &str) {
        if self.startup_hints.is_subagent || !self.may_have_hooks_for(event::HookEventName::Stop) {
            return;
        }
        let envelope = self.fire_hook(
            event::HookEventName::Stop,
            None,
            event::HookPayload::Stop {
                reason: reason.to_string(),
                stop_hook_active: false,
                last_assistant_message: None,
                background_tasks: None,
                session_crons: None,
            },
        );
        let Some(registry) = self.allowed_hook_registry() else {
            return;
        };
        let ctx = self.hook_run_ctx();
        let dispatch =
            dispatcher::dispatch_stop(&registry, event::HookEventName::Stop, &envelope, &ctx);
        let Ok(mut result) = tokio::time::timeout(SESSION_END_STOP_BUDGET, dispatch).await else {
            tracing::warn!("session-end stop hooks exceeded the shutdown budget; skipping");
            return;
        };
        result.results = demote_ignored_blocks(result.results);
        self.send_hook_execution("stop", None, None, &result.results)
            .await;
        self.emit_hook_executed_telemetry("stop", None, &result.results)
            .await;
    }

    pub(crate) async fn list_active_subagents(
        &self,
    ) -> Vec<echo_agent_tools::implementations::echo_agent_build::task::types::ActiveSubagentSummary>
    {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentListActiveRequest,
        };
        let Some(ref event_tx) = self.tool_context.subagent_event_tx else {
            return Vec::new();
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        if event_tx
            .send(SubagentEvent::ListActive(SubagentListActiveRequest {
                parent_session_id: self.session_id_string(),
                respond_to: tx,
            }))
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Snapshot in-flight background work and scheduled wakeups for the Stop
    /// hook input (filtering out tasks owned by other sessions on the shared
    /// backend).
    async fn stop_gate_work_snapshot(&self) -> (Vec<StopBackgroundTask>, Vec<StopSessionCron>) {
        let bridge = self.tool_bridge_handle();
        let my_session = self.session_id_string();
        let mut tasks: Vec<StopBackgroundTask> = bridge
            .list_background_tasks()
            .await
            .iter()
            .filter(|t| t.is_outstanding())
            .filter(|t| {
                t.owner_session_id
                    .as_deref()
                    .is_none_or(|owner| owner == my_session)
            })
            .map(stop_entry_from_task)
            .collect();
        tasks.extend(
            self.list_active_subagents()
                .await
                .iter()
                .map(stop_entry_from_subagent),
        );

        let now = chrono::Utc::now();
        let crons = bridge
            .list_scheduled_tasks()
            .await
            .iter()
            .filter(|t| !t.is_expired(now))
            .map(stop_cron_from_scheduled)
            .collect();
        (tasks, crons)
    }

    async fn announce_force_stop(&self, prevent: &dispatcher::StopBlock) {
        self.send_hook_annotation(&format!(
            "\u{26a0} Hook `{}` stopped the agent: {}",
            prevent.hook_name, prevent.reason
        ))
        .await;
    }

    /// Answering the gate's hook request takes a client, so a test reaches the payload here.
    #[cfg(test)]
    pub(super) async fn stop_payload_for_test(&self) -> event::HookPayload {
        self.build_stop_payload(/* stop_hook_active */ false).await
    }

    async fn build_stop_payload(&self, stop_hook_active: bool) -> event::HookPayload {
        let last_assistant_message = self
            .chat_state_handle
            .get_last_assistant_text_in_turn()
            .await
            .as_deref()
            .map(event::clip_assistant_message);
        if self.startup_hints.is_subagent {
            event::HookPayload::SubagentStop {
                phase: event::SubagentStopPhase::Gate,
                subagent_id: self.session_id_string(),
                subagent_type: self.subagent_type_label().unwrap_or_default(),
                stop_hook_active: Some(stop_hook_active),
                last_assistant_message,
            }
        } else {
            let (background_tasks, session_crons) = self.stop_gate_work_snapshot().await;
            event::HookPayload::Stop {
                reason: "end_turn".to_string(),
                stop_hook_active,
                last_assistant_message,
                background_tasks: Some(background_tasks),
                session_crons: Some(session_crons),
            }
        }
    }

    async fn emit_stop_results(
        &self,
        event: event::HookEventName,
        prompt_id: &str,
        results: &[result::HookRunResult],
    ) {
        let name = event.to_string();
        self.send_hook_execution(&name, None, Some(prompt_id), results)
            .await;
        self.emit_hook_executed_telemetry(&name, None, results)
            .await;
    }

    /// Run the turn-end `Stop`/`SubagentStop` hook gate and decide whether the
    /// agent may stop or must keep working. Hook failures fail open (the agent
    /// stops normally).
    pub(super) async fn run_stop_gate(
        &self,
        prompt_id: &str,
        continuations_this_turn: u32,
    ) -> StopGateDecision {
        let event = if self.startup_hints.is_subagent {
            event::HookEventName::SubagentStop
        } else {
            event::HookEventName::Stop
        };
        if !self.has_enabled_hooks_for(event) {
            return StopGateDecision::AllowStop;
        }
        // At the cap no hook is consulted or notified for this forced stop,
        // unlike the force-stop path below which still notifies observers.
        if continuations_this_turn >= MAX_STOP_HOOK_CONTINUATIONS_PER_TURN {
            tracing::warn!(
                continuations_this_turn,
                "stop hook continuation limit reached; ending the turn"
            );
            self.send_hook_annotation(&format!(
                "\u{26a0} Stop hooks kept the agent working {MAX_STOP_HOOK_CONTINUATIONS_PER_TURN} times this turn: limit reached, ending the turn"
            ))
            .await;
            return StopGateDecision::AllowStop;
        }

        // Claim before the hooks, not after: a turn already reported is being torn down, so the
        // gate skips instead of running verification hooks for a turn the user cancelled.
        let Some(claim) = self.turn_report.claim_for_gate() else {
            tracing::debug!(
                prompt_id,
                "a cancel already reported this turn; the stop gate did not run"
            );
            return StopGateDecision::AllowStop;
        };

        let payload = self.build_stop_payload(continuations_this_turn > 0).await;
        // Gate envelope via `make_hook_envelope`, not the observe-notify
        // `fire_hook`: client hooks get the awaited `echo.agent/hooks/run` request
        // below, not a fire-and-forget event.
        let envelope = self.make_hook_envelope(event, Some(prompt_id.to_string()), payload);

        let mut result = dispatcher::StopDispatchResult::default();
        // Clone out of the RefCell before the awaits so no `Ref` is held
        // across them.
        let registry = self.allowed_hook_registry();
        if let Some(registry) = registry {
            let ctx = self.hook_run_ctx();
            result = dispatcher::dispatch_stop(&registry, event, &envelope, &ctx).await;
        }

        if let Some(prevent) = result.prevent_continuation.take() {
            // Force-stop: skip the client gate (its signals would be discarded)
            // but still send the observe notification so client callbacks see
            // the turn end.
            self.emit_stop_results(event, prompt_id, &result.results)
                .await;
            self.notify_client_hooks(&envelope);
            commit_stop_report(claim, prompt_id);
            self.announce_force_stop(&prevent).await;
            return StopGateDecision::AllowStop;
        }

        // Merge file and client results and emit once: one stop gate is one
        // scrollback entry and one telemetry batch.
        let client = self.run_stop_client_hooks(&envelope).await;
        let mut all_results = std::mem::take(&mut result.results);
        all_results.extend(client.results);
        if !all_results.is_empty() {
            self.emit_stop_results(event, prompt_id, &all_results).await;
        }

        result.blocks.extend(client.blocks);
        result.additional_context.extend(client.additional_context);
        if let Some(prevent) = client.prevent_continuation {
            commit_stop_report(claim, prompt_id);
            self.announce_force_stop(&prevent).await;
            return StopGateDecision::AllowStop;
        }

        if !result.wants_continuation() {
            // A gate whose hooks all skipped or crashed told nobody the turn ended, so it
            // releases the claim and a later cancel or failure can still report.
            let any_hook_succeeded = all_results
                .iter()
                .any(|r| matches!(r, result::HookRunResult::Success { .. }));
            if any_hook_succeeded {
                commit_stop_report(claim, prompt_id);
            }
            return StopGateDecision::AllowStop;
        }

        // The turn has not ended, so a later interrupt must still be able to report it.
        drop(claim);
        self.announce_keep_working(&result.blocks, &result.additional_context)
            .await;
        StopGateDecision::KeepWorking {
            feedback: format_stop_feedback(&result.blocks, &result.additional_context),
        }
    }

    /// Annotate the scrollback when a stop gate keeps the agent working: one line
    /// per block (with `HookBlocked` telemetry), or the context lines when only
    /// `additionalContext` was returned.
    async fn announce_keep_working(
        &self,
        blocks: &[dispatcher::StopBlock],
        additional_context: &[String],
    ) {
        for block in blocks {
            self.send_hook_annotation(&format!(
                "\u{21a9} Stop blocked by hook `{}`, continuing: {}",
                block.hook_name, block.reason
            ))
            .await;
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::HookBlocked {
                    hook_name: block.hook_name.clone(),
                },
            );
        }
        if blocks.is_empty() {
            for context in additional_context {
                self.send_hook_annotation(&format!(
                    "\u{21a9} Stop hook feedback, continuing: {context}"
                ))
                .await;
            }
        }
    }
}

#[cfg(test)]
mod stop_gate_snapshot_tests {
    use super::*;

    fn task_snapshot(
        kind: echo_agent_tools::computer::types::TaskKind,
    ) -> echo_agent_tools::types::TaskSnapshot {
        echo_agent_tools::types::TaskSnapshot {
            task_id: "task-1".into(),
            command: "sandbox-exec tail -f /var/log/syslog".into(),
            display_command: Some("tail -f /var/log/syslog".into()),
            cwd: "/tmp".into(),
            start_time: std::time::SystemTime::UNIX_EPOCH,
            end_time: None,
            output: String::new(),
            output_file: std::path::PathBuf::from("/tmp/out"),
            truncated: false,
            exit_code: None,
            signal: None,
            completed: false,
            kind,
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        }
    }

    #[test]
    fn task_snapshot_maps_to_stop_entry() {
        let shell = stop_entry_from_task(&task_snapshot(
            echo_agent_tools::computer::types::TaskKind::Bash,
        ));
        assert_eq!(shell.r#type, BackgroundTaskType::Shell);
        assert_eq!(shell.command.as_deref(), Some("tail -f /var/log/syslog"));
        assert!(shell.description.is_none());
        assert_eq!(shell.status, "running");
        assert!(shell.agent_type.is_none());

        let monitor = stop_entry_from_task(&task_snapshot(
            echo_agent_tools::computer::types::TaskKind::Monitor,
        ));
        assert_eq!(monitor.r#type, BackgroundTaskType::Monitor);
        assert!(monitor.command.is_none());
        assert_eq!(
            monitor.description.as_deref(),
            Some("tail -f /var/log/syslog")
        );
    }

    #[test]
    fn subagent_summary_maps_to_stop_entry() {
        let summary =
            echo_agent_tools::implementations::echo_agent_build::task::types::ActiveSubagentSummary {
                subagent_id: "sub-1".into(),
                subagent_type: "explore".into(),
                description: "d".repeat(2000),
                elapsed_ms: 5,
            };
        let entry = stop_entry_from_subagent(&summary);
        assert_eq!(entry.r#type, BackgroundTaskType::Subagent);
        assert_eq!(entry.agent_type.as_deref(), Some("explore"));
        let description = entry.description.unwrap();
        assert!(description.ends_with("… [+1000 chars]"));
        assert!(entry.command.is_none());
    }

    #[test]
    fn format_stop_feedback_lists_blocks_then_appends_context() {
        let block = |reason: &str| dispatcher::StopBlock {
            hook_name: "h".into(),
            reason: reason.into(),
        };
        assert_eq!(
            format_stop_feedback(&[block("first"), block("second")], &[]),
            "Stop hook feedback:\n- first\n- second\n"
        );
        assert_eq!(
            format_stop_feedback(&[block("fix tests")], &["note".to_string()]),
            "Stop hook feedback:\n- fix tests\n\nnote"
        );
        assert_eq!(
            format_stop_feedback(&[], &["only context".to_string()]),
            "only context"
        );
    }

    #[test]
    fn scheduled_task_maps_to_stop_cron() {
        let task =
            echo_agent_tools::implementations::echo_agent_build::scheduler::types::ScheduledTask::new(
                300,
                "check the build".into(),
                true,
                false,
            );
        let cron = stop_cron_from_scheduled(&task);
        assert_eq!(cron.schedule, "every 5 minutes");
        assert!(cron.recurring);
        assert_eq!(cron.prompt, "check the build");
    }

    #[test]
    fn demote_ignored_blocks_downgrades_only_blocked() {
        use echo_agent_hooks::result::HookRunResult;

        let results = demote_ignored_blocks(vec![
            HookRunResult::Blocked {
                hook_name: "gate".into(),
                detail: "blocked stop: run the tests".into(),
                elapsed: std::time::Duration::from_millis(5),
                http_info: None,
            },
            HookRunResult::Failed {
                hook_name: "broken".into(),
                error: "exit code 1".into(),
                elapsed: std::time::Duration::from_millis(3),
                http_info: None,
            },
            HookRunResult::Skipped {
                hook_name: "disabled".into(),
            },
        ]);

        assert!(
            matches!(&results[0], HookRunResult::Success { hook_name, .. } if hook_name == "gate"),
            "a discarded decision must read as success, got {:?}",
            results[0]
        );
        assert!(matches!(&results[1], HookRunResult::Failed { .. }));
        assert!(matches!(&results[2], HookRunResult::Skipped { .. }));
    }
}
