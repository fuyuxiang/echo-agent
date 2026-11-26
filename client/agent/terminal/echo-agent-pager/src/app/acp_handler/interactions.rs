use super::*;

/// Resolve a superseded elicitation's reverse-request with `Cancel` so the
/// awaiting MCP server is released before a replacement takes the slot.
fn cancel_elicitation_request(
    response_tx: tokio::sync::oneshot::Sender<echo_agent_acp::AcpResult<acp::ExtResponse>>,
) {
    let cancelled = echo_agent_tools::mcp_elicitation::McpElicitExtResponse::Cancel;
    if let Ok(raw) = serde_json::value::to_raw_value(&cancelled) {
        response_tx.send(Ok(acp::ExtResponse::new(raw.into()))).ok();
    }
}

pub(crate) fn handle_mcp_elicit(
    ext: echo_agent_acp::AcpArgs<acp::ExtRequest>,
    app: &mut AppView,
) -> bool {
    use crate::views::elicitation_view::ElicitationViewState;
    use echo_agent_tools::mcp_elicitation::McpElicitExtRequest;

    let ext_req: McpElicitExtRequest = match serde_json::from_str(ext.request.params.get()) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "Failed to parse McpElicitExtRequest");
            ext.response_tx
                .send(Err(acp::Error::new(-32602, format!("Invalid params: {e}"))))
                .ok();
            return false;
        }
    };

    let Some(id) = interaction_target_agent(app, &ext_req.session_id) else {
        tracing::info!(
            session_id = %ext_req.session_id,
            "mcp elicit for a session with no local view; parked for leader replay-on-attach"
        );
        drop(ext.response_tx);
        return false;
    };
    let is_active = is_matched_agent_active(app, id);
    let Some(agent) = app.agents.get_mut(&id) else {
        drop(ext.response_tx);
        return false;
    };

    let waiting = agent
        .elicitation_view
        .as_ref()
        .is_some_and(|ev| ev.is_url_waiting());
    if waiting {
        if let Some((_, old_tx)) = agent.pending_elicitation.take() {
            cancel_elicitation_request(old_tx);
        }
        agent.pending_elicitation = Some((ext_req, ext.response_tx));
        return is_active;
    }

    if let Some((_, old_tx)) = agent.pending_elicitation.take() {
        cancel_elicitation_request(old_tx);
    }

    if let Some(mut old) = agent.elicitation_view.take() {
        if let Some(old_tx) = old.take_response_tx() {
            cancel_elicitation_request(old_tx);
        }
        agent.restore_elicitation_prompt(old.stashed_prompt);
    }

    let stashed = agent.stash_prompt_for_elicitation();
    agent.elicitation_view = Some(ElicitationViewState::from_request(
        ext_req,
        stashed,
        Some(ext.response_tx),
    ));
    agent.last_active_at = Some(std::time::Instant::now());

    tracing::info!(
        target_active = is_active,
        "Opened MCP elicitation view from ext_method"
    );
    is_active
}

/// Handle `echo.agent/ask_user_question` ext-method.
///
/// Parses the typed request, creates a `QuestionViewState` with the
/// `response_tx` stashed, and opens the question overlay. The pager does
/// NOT respond immediately — the response is sent later when the user
/// submits, cancels, or is replaced by another question.
///
/// If a question is already active, the old one is cancelled first
/// (`Cancelled` is sent on its stashed `response_tx`).
pub(crate) fn handle_ask_user_question(
    ext: echo_agent_acp::AcpArgs<acp::ExtRequest>,
    app: &mut AppView,
) -> bool {
    use crate::views::question_view::QuestionViewState;
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        AskUserQuestionExtRequest, AskUserQuestionExtResponse,
    };

    // Parse the typed request from the ext-method params.
    let ext_req: AskUserQuestionExtRequest = match serde_json::from_str(ext.request.params.get()) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "Failed to parse AskUserQuestionExtRequest");
            ext.response_tx
                .send(Err(acp::Error::new(-32602, format!("Invalid params: {e}"))))
                .ok();
            return false;
        }
    };

    // Route by the request's session id (like `session/update`), so a question
    // raised by a BACKGROUND session lands on its own view even when the user is
    // on the dashboard or another session — rather than failing because the
    // user hasn't entered the session yet.
    let Some(id) = interaction_target_agent(app, &ext_req.session_id) else {
        // No local view for this session. Do NOT send an error — that would FAIL
        // the tool (rendered red). Leave the reverse-request unanswered: the
        // agent keeps awaiting and the leader replays it when a client attaches
        // via `session/load`.
        tracing::info!(
            session_id = %ext_req.session_id,
            "ask_user_question for a session with no local view; parked for leader replay-on-attach"
        );
        drop(ext.response_tx);
        return false;
    };
    let is_active = is_matched_agent_active(app, id);
    let Some(agent) = app.agents.get_mut(&id) else {
        // `interaction_target_agent` only returns ids that exist; defensive.
        tracing::warn!("ask_user_question: agent {id:?} not found");
        drop(ext.response_tx);
        return false;
    };

    // If a question is already active, cancel it before replacing.
    if let Some(mut old_qv) = agent.question_view.take() {
        agent.record_question_pause(&old_qv);
        tracing::warn!(
            old_tool_call_id = %old_qv.tool_call_id,
            new_tool_call_id = %ext_req.tool_call_id,
            "Replacing active question - cancelling previous"
        );
        if let Some(old_tx) = old_qv.response_tx.take() {
            let cancelled = AskUserQuestionExtResponse::Cancelled;
            let raw = serde_json::value::to_raw_value(&cancelled)
                .expect("Cancelled serialization should not fail");
            old_tx.send(Ok(acp::ExtResponse::new(raw.into()))).ok();
        }
        agent.restore_card_prompt(old_qv.stashed_prompt);

        // Local question displaced by an ACP ask, so surface why it vanished.
        // Any directive it carried is dropped; the user re-issues the command after answering.
        if let Some(kind) = old_qv.local_kind.take() {
            use crate::app::actions::FeedbackTraceChoice;
            use crate::app::dispatch::notes;
            use crate::views::question_view::LocalQuestionKind;
            match kind {
                // A displaced trace-consent card still carries a committed
                // report; it must send (like Esc/skip), not silently vanish.
                LocalQuestionKind::FeedbackTrace { report, images } => {
                    if let Some(session_id) = agent.session.session_id.clone() {
                        // The shared committer closes the consent funnel,
                        // applies the emptiness rule, and picks the
                        // displaced-card copy.
                        if let Some(effect) = notes::commit_feedback(
                            agent,
                            app.coding_data_retention_opt_out,
                            id,
                            session_id,
                            report,
                            images,
                            Some(FeedbackTraceChoice::NoUpload),
                            true,
                        ) {
                            app.pending_effects.push(effect);
                        }
                    } else {
                        // No session to send through: still close the funnel.
                        // Dropping `images` cleans up its staged temp files.
                        notes::log_trace_consent_selected(
                            app.coding_data_retention_opt_out,
                            FeedbackTraceChoice::NoUpload,
                        );
                        agent.scrollback.push_block(RenderBlock::system(
                            "/feedback cancelled because another question opened.".to_owned(),
                        ));
                    }
                }
                LocalQuestionKind::DoctorFix { .. } => {
                    agent.scrollback.push_block(RenderBlock::system(
                        "/doctor fix was cancelled because another question opened.".to_owned(),
                    ));
                }
                kind => {
                    // The trace-consent and doctor-fix arms above own their
                    // variants; their labels here are graceful fallbacks.
                    let cmd = match kind {
                        LocalQuestionKind::Fork { .. } => "/fork",
                        LocalQuestionKind::NewSession => "/new",
                        LocalQuestionKind::CreditLimitUpsell { .. } => "credit-limit upsell",
                        LocalQuestionKind::FreeUsageUpsell { .. } => "EchoAgent upsell",
                        LocalQuestionKind::AgentTypeMismatch { .. } => "model switch",
                        LocalQuestionKind::DeleteCurrentSession => "/delete",
                        LocalQuestionKind::Feedback | LocalQuestionKind::FeedbackTrace { .. } => {
                            "/feedback"
                        }
                        LocalQuestionKind::DoctorFix { .. } => "/doctor fix",
                    };
                    agent.scrollback.push_block(RenderBlock::system(format!(
                        "{cmd} cancelled because another question opened."
                    )));
                }
            }
        }
    }

    // Stash the composer so it comes back when this question closes.
    agent.question_view = Some(QuestionViewState::with_response_tx(
        ext_req.tool_call_id,
        ext_req.questions,
        agent.prompt.stash(),
        Some(ext.response_tx),
        ext_req.mode,
    ));

    // Clear prompt for question interaction.
    agent.prompt.set_text("");

    // Stamp the "last activity" anchor so the
    // dashboard's NeedsInput row reflects "time since this question
    // arrived" rather than the previous turn's end time.
    agent.last_active_at = Some(std::time::Instant::now());

    tracing::info!(
        mode = ?ext_req.mode,
        question_count = agent.question_view.as_ref().map(|q| q.questions.len()).unwrap_or(0),
        target_active = is_active,
        "Opened question view from ext_method"
    );

    // Only the currently-displayed view needs an immediate redraw; a question
    // parked on a background agent surfaces via the roster `NeedsInput` delta
    // and renders when the user switches to that session.
    is_active
}

/// Handle an `echo.agent/exit_plan_mode` ext_method request.
///
/// Creates a `PlanApprovalViewState` overlay for interactive approval.
///
/// Flow: parse → guard → cancel old → capture session draft → create state →
/// prefill freeform when safe (not under an open permission) → return true.
pub(super) fn handle_exit_plan_mode(
    ext: echo_agent_acp::AcpArgs<acp::ExtRequest>,
    app: &mut AppView,
) -> bool {
    use crate::views::plan_approval_view::{ExitPlanModeExtRequest, PlanApprovalViewState};

    // 1. Parse typed request from raw JSON params.
    let params: ExitPlanModeExtRequest = match serde_json::from_str(ext.request.params.get()) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to parse ExitPlanModeExtRequest: {e}");
            ext.response_tx
                .send(Err(acp::Error::new(
                    -32602,
                    format!("Invalid exit_plan_mode params: {e}"),
                )))
                .ok();
            return false;
        }
    };

    // 2. Route by the request's session id (like `session/update`), so a
    // plan-approval raised by a BACKGROUND session lands on its own view even
    // when the user isn't currently focused on it — rather than failing.
    let Some(id) = interaction_target_agent(app, &params.session_id) else {
        // No local view for this session. Do NOT error (that fails the tool):
        // leave the reverse-request unanswered and rely on the leader's
        // replay-on-attach.
        tracing::info!(
            session_id = %params.session_id,
            "exit_plan_mode for a session with no local view; parked for leader replay-on-attach"
        );
        drop(ext.response_tx);
        return false;
    };
    let is_active = is_matched_agent_active(app, id);
    let Some(agent) = app.agents.get_mut(&id) else {
        // `interaction_target_agent` only returns ids that exist; defensive.
        tracing::warn!("exit_plan_mode: agent {id:?} not found");
        drop(ext.response_tx);
        return false;
    };

    if let Some(mut old) = agent.plan_approval_view.take() {
        tracing::warn!(
            old_tool_call_id = %old.tool_call_id,
            new_tool_call_id = %params.tool_call_id,
            "Replacing active plan approval — dismissing previous"
        );
        old.send_stale_cancel();
        agent.plan_next_comment_id = old.next_comment_id;
        agent.prompt.restore(old.stashed_prompt);
        agent.line_viewer = None;
    }

    // Dismiss competing overlays so plan approval owns the screen.
    // - active_modal: draw returns before line_viewer (plan never paints);
    //   keys still route to the invisible plan viewer.
    // - block_viewer: draw returns on line_viewer (plan visible) but
    //   handle_scroll prefers block_viewer, so wheel hits the hidden Edit pane.
    agent.active_modal = None;
    agent.block_viewer = None;

    let source = plan_review_source_for_tool(&params.tool_call_id, agent);

    // If the user was mid-casual-comment when this new plan-approval
    // request arrived, restore the pre-comment prompt first so the
    // upcoming `stash()` captures the user's original text rather
    // than the in-progress comment draft. Also clears the now-stale
    // `casual_stashed_prompt` so it doesn't dangle into the next
    // casual entry.
    if let Some(stashed) = agent.casual_stashed_prompt.take() {
        agent.prompt.restore(stashed);
    }

    // Permission open: session draft is `permission_stashed_prompt` (live is followup).
    // Otherwise: live composer is the session draft.
    let permission_still_open = !agent.permission_queue.is_empty();
    let session_draft = if let Some(perm_draft) = agent.permission_stashed_prompt.take() {
        let _permission_followup = agent.prompt.stash();
        perm_draft
    } else {
        agent.prompt.stash()
    };

    let had_session_draft = !session_draft.is_effectively_empty();
    // Never prefill freeform while permission owns the keyboard: followup would type
    // into (and could send) the private session draft. Arm deferred prefill so
    // restore_permission_stashes applies it only when the queue actually drains.
    if had_session_draft && !permission_still_open {
        agent.plan_freeform_prefill_deferred = false;
        agent.prompt.restore(session_draft.clone_for_live_prefill());
    } else {
        agent.plan_freeform_prefill_deferred = permission_still_open;
        agent.prompt.set_text("");
    }

    let state = PlanApprovalViewState::with_source(params, source, session_draft, ext.response_tx);

    agent.plan_comments.clear();
    agent.plan_next_comment_id = 0;

    if state.source == PlanReviewSource::Inline {
        agent.latest_inline_plan_content = state.plan_content.clone();
    } else {
        agent.latest_inline_plan_content = None;
    }
    agent.plan_approval_view = Some(state);

    agent.casual_commenting_range = None;
    agent.casual_editing_comment_id = None;

    agent.show_plan_preview_if_available();

    if agent.line_viewer.is_some() {
        if let Some(ref mut viewer) = agent.line_viewer {
            viewer.plan_mut().feedback_active = true;
        }
        if had_session_draft
            && !permission_still_open
            && let Some(ref mut pav) = agent.plan_approval_view
        {
            pav.focus = crate::views::plan_approval_view::PlanApprovalFocus::Prompt;
        }
    } else if !permission_still_open && let Some(ref mut pav) = agent.plan_approval_view {
        pav.focus = crate::views::plan_approval_view::PlanApprovalFocus::Prompt;
    }

    tracing::info!(
        target_active = is_active,
        "Opened plan approval view from ext_method"
    );

    // Background-parked approval renders when the user switches to the session;
    // only the active view needs an immediate redraw.
    is_active
}

pub(super) fn plan_review_source_for_tool(
    tool_call_id: &str,
    agent: &AgentView,
) -> PlanReviewSource {
    agent
        .session
        .tracker
        .tool_title(tool_call_id)
        .filter(|title| *title == "CreatePlan" || *title == "Plan: Submit for approval")
        .map_or(PlanReviewSource::FileBacked, |_| PlanReviewSource::Inline)
}
