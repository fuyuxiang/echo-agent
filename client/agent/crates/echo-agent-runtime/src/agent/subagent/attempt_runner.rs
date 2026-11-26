//! Executes the existing single-prompt child attempt while its session actor is live.
use super::*;
use crate::session::commands::{
    PromptCompletionKind, PromptTurnResult as SubagentPromptTurnResult,
};
pub(super) struct OneTurnAttemptInput<'a> {
    pub child_handle: &'a SessionHandle,
    pub request: &'a SubagentRequest,
    pub worktree_path: Option<&'a Path>,
    pub task_prompt_text: &'a str,
    pub inherited_tool_overrides: Option<echo_agent_sampling_types::ToolOverrides>,
    pub gcs_bucket_url: Option<&'a str>,
    pub gcs_upload_method: Option<&'a crate::session::repo_changes::UploadMethod>,
    pub cancel_token: CancellationToken,
    pub child_run_started_at: std::time::Instant,
}
pub(super) struct OneTurnTraceCapture {
    pub before_copy_rx:
        oneshot::Receiver<anyhow::Result<crate::session::persistence::SessionStateCopy>>,
    pub child_prompt_id: String,
    pub turn_started_at: String,
    pub turn_token_totals: Option<(u64, u64, u64)>,
}
pub(super) struct OneTurnAttemptOutcome {
    pub result: SubagentResult,
    pub trace: OneTurnTraceCapture,
    pub cancellation_may_hide_usage: bool,
}
pub(super) struct OneTurnUsageInput<'a> {
    pub child_handle: &'a SessionHandle,
    pub task_budget_usage: Option<(u64, bool)>,
    pub cancellation_may_hide_usage: bool,
    pub parent_cmd_tx: Option<&'a mpsc::UnboundedSender<SessionCommand>>,
    pub parent_prompt_id: Option<&'a str>,
}
pub(super) async fn run_one_turn_attempt(
    mut input: OneTurnAttemptInput<'_>,
) -> OneTurnAttemptOutcome {
    let (before_copy_tx, before_copy_rx) = oneshot::channel();
    let _ = input.child_handle.cmd_tx.send(SessionCommand::CopyFile {
        respond_to: before_copy_tx,
    });
    if let Some(overrides) = input.inherited_tool_overrides.take() {
        let _ = input
            .child_handle
            .cmd_tx
            .send(SessionCommand::SetToolOverrides { overrides });
    }
    let (prompt_tx, prompt_rx) = oneshot::channel::<SubagentPromptTurnResult>();
    let child_prompt_id = uuid::Uuid::now_v7().to_string();
    let turn_started_at = chrono::Utc::now().to_rfc3339();
    let _ = input.child_handle.cmd_tx.send(SessionCommand::Prompt {
        prompt_id: child_prompt_id.clone(),
        prompt_blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
            input.task_prompt_text.to_owned(),
        ))],
        prompt_mode: crate::session::plan_mode::PromptMode::Agent,
        artifact_upload_ctx: input.gcs_bucket_url.and_then(|_| {
            input
                .gcs_upload_method
                .map(|method| crate::upload::manifest::ArtifactUploadContext {
                    gcs_config: crate::session::repo_changes::TraceExportConfig {
                        bucket_url: input.gcs_bucket_url.map(str::to_owned),
                        service_account_key: None,
                        prefix_dir: None,
                        gcs_prefix: Some(format!("{}/turn_0", &input.request.id)),
                        absolute_paths: false,
                        archive_name_override: None,
                        upload_method: method.clone(),
                    },
                    artifact_tracker: crate::upload::manifest::new_artifact_tracker(),
                })
        }),
        client_identifier: None,
        screen_mode: None,
        verbatim: true,
        traceparent: echo_agent_file_utils::trace_context::current_traceparent(),
        json_schema: input.request.runtime_overrides.output_schema.clone(),
        send_now: false,
        admission: None,
        tool_overrides_update: None,
        respond_to: prompt_tx,
        persist_ack: None,
        parsed_prompt_tx: None,
    });
    let mut turn_token_totals = None;
    let mut cancellation_may_hide_usage = false;
    let wait_outcome =
        await_subagent_turn_or_cancellation(prompt_rx, input.cancel_token.clone()).await;
    let duration_ms = input.child_run_started_at.elapsed().as_millis() as u64;
    let result = match wait_outcome {
        SubagentWaitOutcome::Cancelled => {
            let (tool_calls, turns) = signals_snapshot_counts(input.child_handle).await;
            cancellation_may_hide_usage = turns > 0 || tool_calls > 0;
            SubagentResult {
                success: false,
                cancelled: true,
                error: Some("Subagent was cancelled".to_string()),
                ..base_result(&input, tool_calls, turns, duration_ms)
            }
        }
        SubagentWaitOutcome::TurnResult(turn_result) => {
            let was_cancelled = input.cancel_token.is_cancelled();
            let (tool_calls, turns) = match &*turn_result {
                Ok(Ok(crate::session::commands::PromptTurnOk {
                    turn_snapshot: Some(snapshot),
                    ..
                })) => {
                    turn_token_totals = Some((
                        snapshot.turn_input_tokens,
                        snapshot.turn_cached_input_tokens,
                        snapshot.turn_output_tokens,
                    ));
                    (
                        snapshot.current.tool_call_count,
                        snapshot.current.turn_count,
                    )
                }
                _ => signals_snapshot_counts(input.child_handle).await,
            };
            let final_text = input
                .child_handle
                .chat_state_handle
                .get_last_assistant_text()
                .await
                .unwrap_or_default();
            let result_tokens = input
                .child_handle
                .chat_state_handle
                .get_total_tokens()
                .await;
            match *turn_result {
                Ok(Ok(crate::session::commands::PromptTurnOk {
                    completion_kind: PromptCompletionKind::Cancelled { category, context },
                    ..
                })) => {
                    cancellation_may_hide_usage = true;
                    SubagentResult {
                        success: false,
                        cancelled: true,
                        error: Some(super::cancellation_error_message(
                            category,
                            context.as_ref(),
                        )),
                        output: text_or_summary(final_text, || {
                            format!(
                                "Subagent '{}' ({}) was cancelled. {tool_calls} tool calls, \
                                 {turns} turns.",
                                input.request.description.as_str(),
                                input.request.subagent_type.as_str()
                            )
                        }),
                        tokens_used: result_tokens,
                        output_usage_incomplete: true,
                        ..base_result(&input, tool_calls, turns, duration_ms)
                    }
                }
                Ok(Ok(crate::session::commands::PromptTurnOk {
                    completion_kind: PromptCompletionKind::MaxTurnsReached { limit },
                    ..
                })) => SubagentResult {
                    success: false,
                    cancelled: true,
                    error: Some(format!("max turns reached (limit: {limit})")),
                    output: text_or_summary(final_text, || {
                        format!(
                            "Subagent '{}' ({}) hit max-turns limit ({limit}). {tool_calls} \
                             tool calls, {turns} turns.",
                            input.request.description.as_str(),
                            input.request.subagent_type.as_str()
                        )
                    }),
                    tokens_used: result_tokens,
                    output_usage_incomplete: true,
                    ..base_result(&input, tool_calls, turns, duration_ms)
                },
                Ok(Ok(crate::session::commands::PromptTurnOk {
                    structured_output, ..
                })) => {
                    let wanted_schema = input.request.runtime_overrides.output_schema.is_some();
                    let (success, error, output) = match (wanted_schema, structured_output) {
                        (true, Some(Ok(value))) => (true, None, Arc::from(value.to_string())),
                        (true, Some(Err(error))) => (
                            false,
                            Some(format!("structured output validation failed: {error}")),
                            Arc::from(final_text),
                        ),
                        (true, None) => (
                            false,
                            Some("structured output requested but none produced".to_string()),
                            Arc::from(final_text),
                        ),
                        (false, _) => (
                            true,
                            None,
                            text_or_summary(final_text, || {
                                format!(
                                    "Subagent '{}' ({}) completed successfully. {tool_calls} \
                                     tool calls, {turns} turns.",
                                    input.request.description.as_str(),
                                    input.request.subagent_type.as_str()
                                )
                            }),
                        ),
                    };
                    SubagentResult {
                        success,
                        error,
                        output,
                        tokens_used: result_tokens,
                        output_usage_incomplete: true,
                        ..base_result(&input, tool_calls, turns, duration_ms)
                    }
                }
                Ok(Err(error)) => {
                    cancellation_may_hide_usage = was_cancelled;
                    SubagentResult {
                        success: false,
                        cancelled: was_cancelled,
                        error: Some(if was_cancelled {
                            "Subagent was cancelled".to_string()
                        } else {
                            format!("Session error: {error}")
                        }),
                        ..base_result(&input, tool_calls, turns, duration_ms)
                    }
                }
                Err(_) => {
                    cancellation_may_hide_usage = was_cancelled;
                    SubagentResult {
                        success: false,
                        cancelled: was_cancelled,
                        error: Some(if was_cancelled {
                            "Subagent was cancelled".to_string()
                        } else {
                            "Child session dropped unexpectedly".to_string()
                        }),
                        ..base_result(&input, tool_calls, turns, duration_ms)
                    }
                }
            }
        }
    };
    OneTurnAttemptOutcome {
        result,
        trace: OneTurnTraceCapture {
            before_copy_rx,
            child_prompt_id,
            turn_started_at,
            turn_token_totals,
        },
        cancellation_may_hide_usage,
    }
}
pub(super) fn canonical_total_tokens(totals: &echo_agent_chat_state::UsageTotals) -> u64 {
    totals.total_tokens()
}
pub(super) fn usage_is_incomplete(
    ledger_incomplete: bool,
    cancellation_may_hide_usage: bool,
) -> bool {
    ledger_incomplete || cancellation_may_hide_usage
}
pub(super) async fn record_subagent_usage(
    parent_cmd_tx: Option<&mpsc::UnboundedSender<SessionCommand>>,
    by_model: Option<Vec<(String, echo_agent_chat_state::UsageTotals)>>,
    parent_prompt_id: Option<String>,
    incomplete: bool,
) -> bool {
    match by_model {
        None => false,
        Some(by_model) if by_model.is_empty() && !incomplete => true,
        Some(by_model) => {
            let Some(cmd_tx) = parent_cmd_tx else {
                return false;
            };
            let (respond_to, ack) = oneshot::channel();
            if cmd_tx
                .send(SessionCommand::RecordSubagentUsage {
                    by_model,
                    parent_prompt_id,
                    incomplete,
                    respond_to,
                })
                .is_err()
            {
                return false;
            }
            ack.await.is_ok()
        }
    }
}
pub(super) async fn capture_and_fold_one_turn_usage(
    result: &mut SubagentResult,
    input: OneTurnUsageInput<'_>,
) -> bool {
    let (by_model, incomplete, output_tokens, total_tokens) = match input
        .child_handle
        .chat_state_handle
        .try_get_session_usage()
        .await
    {
        Ok(usage) => {
            let output_tokens = usage.totals.output_tokens;
            let total_tokens = canonical_total_tokens(&usage.totals);
            let incomplete =
                usage_is_incomplete(usage.incomplete, input.cancellation_may_hide_usage);
            (
                Some(usage.by_model.into_iter().collect::<Vec<_>>()),
                incomplete,
                (!incomplete).then_some(output_tokens),
                Some(total_tokens),
            )
        }
        Err(()) => (None, true, None, None),
    };
    result.total_tokens_used = total_tokens.unwrap_or(0);
    if let Some((task_spent, task_incomplete)) = input.task_budget_usage {
        result.output_tokens_used = output_tokens.unwrap_or(task_spent);
        result.output_usage_incomplete = task_incomplete || incomplete || output_tokens.is_none();
    } else {
        result.output_tokens_used = output_tokens.unwrap_or(0);
        result.output_usage_incomplete = incomplete || output_tokens.is_none();
    }
    record_subagent_usage(
        input.parent_cmd_tx,
        by_model,
        input.parent_prompt_id.map(str::to_owned),
        incomplete,
    )
    .await
}
fn base_result(
    input: &OneTurnAttemptInput<'_>,
    tool_calls: u32,
    turns: u32,
    duration_ms: u64,
) -> SubagentResult {
    SubagentResult {
        subagent_id: input.request.id.clone(),
        child_session_id: input.request.id.clone(),
        tool_calls,
        turns,
        duration_ms,
        worktree_path: input
            .worktree_path
            .map(|path| path.to_string_lossy().into_owned()),
        ..Default::default()
    }
}
fn text_or_summary(final_text: String, summary: impl FnOnce() -> String) -> Arc<str> {
    if final_text.is_empty() {
        Arc::from(summary())
    } else {
        Arc::from(final_text)
    }
}
