//! Async task-result application: routes task results into state.
use super::auth::{
    ensure_login_method, handle_auth_complete, handle_auth_url_ready, handle_mcp_auth_trigger_done,
    handle_mcp_setup_submit_done,
};
use super::billing::{
    PAYWALL_AUTO_CHECK_TIMEOUT, apply_auto_topup, handle_billing_fetched,
    handle_check_subscription_complete, handle_credit_limit_recheck_complete,
    handle_gate_refreshed, handle_gate_verify_timeout,
};
use super::cta::{
    handle_cta_plugin_install_done, handle_cta_plugin_reload_done,
    handle_plugin_cta_catalog_loaded, handle_plugin_cta_debounce_expired,
    handle_plugin_cta_mcps_loaded,
};
use super::ctx::{find_agent_by_session_id, get_active_agent_mut};
use super::notes::{handle_btw_response, handle_memory_note_saved};
use super::prompt::{
    defer_to_open_reload_window, handle_compact_complete, handle_prompt_response,
    handle_suggestion_debounce_expired,
};
use super::queue::push_and_page_flip;
use super::rewind::{
    dispatch_rewind_success, handle_rewind_execute_failed, handle_rewind_points_loaded,
};
use super::router::{dispatch, dispatch_action_result};
use super::session::foreign::{
    handle_foreign_sessions_scanned, handle_session_list_failed, handle_session_list_loaded,
};
use super::session::fork::{
    handle_fork_session_failed, handle_fork_session_ready, handle_worktree_forked,
};
use super::session::lifecycle::{
    dispatch_exit_session, handle_session_created, handle_session_failed,
    handle_switch_model_complete, handle_worktree_session_created, handle_worktree_session_failed,
};
use super::session::load::{
    handle_card_detail_loaded, handle_deep_search_results, handle_session_load_failed,
    handle_session_loaded, handle_session_restore_failed, handle_session_restored,
    handle_session_search_debounce_expired, remove_session_from_pickers,
};
use super::session::modal::remove_agent_and_cleanup;
use super::settings::ui::apply_setting_rollback;
use super::status::{
    handle_coding_data_sharing_failed, handle_coding_data_sharing_updated,
    handle_context_info_complete, handle_session_usage_result, scrub_error_for_toast,
    usage_modal_state_mut,
};
use super::transcript::{
    handle_hooks_list_loaded, handle_marketplace_list_loaded, handle_marketplace_updates_available,
    handle_mcp_toggle_done, handle_plugins_list_loaded, handle_skills_toggle_done,
};
use super::turn::handle_bg_task_killed;
use crate::app::actions::{
    ClipboardPasteCompletion, ClipboardPasteContext, ClipboardPasteFailure, ClipboardPasteTarget,
    DoctorFixTarget, DoctorPlanningOutcome, Effect, ProbedAttachment, SubagentKillOutcome,
    TaskResult,
};
use crate::app::agent::AgentId;
use crate::app::agent_view::AgentDeferredSend;
use crate::app::app_view::{ActiveView, AppView, AuthState};
use crate::scrollback::block::RenderBlock;
use agent_client_protocol as acp;
pub(super) fn unregister_session_effect(session_id: Option<acp::SessionId>) -> Vec<Effect> {
    session_id
        .map(|sid| Effect::UnregisterActiveSession { session_id: sid })
        .into_iter()
        .collect()
}
pub(super) fn unregister_all_active_sessions(app: &AppView) -> Vec<Effect> {
    app.agents
        .values()
        .filter_map(|a| {
            a.session
                .session_id
                .as_ref()
                .map(|sid| Effect::UnregisterActiveSession {
                    session_id: sid.clone(),
                })
        })
        .collect()
}
pub(super) const X11_PRIMARY_PASTE_HINT: &str = "Try Shift+Insert to paste selected text";
fn show_clipboard_toast(target: &ClipboardPasteTarget, message: &str, app: &mut AppView) {
    match target {
        ClipboardPasteTarget::AgentPrompt { agent_id, .. } => {
            if let Some(agent) = app.agents.get_mut(agent_id) {
                agent.show_toast(message);
            }
        }
        ClipboardPasteTarget::DashboardDispatch | ClipboardPasteTarget::DashboardPeek { .. } => {
            if let Some(dashboard) = app.dashboard.as_mut() {
                dashboard.error_toast = Some(message.to_owned());
            }
        }
    }
}
pub(super) fn maybe_show_x11_primary_paste_hint(
    eligible: bool,
    completion: ClipboardPasteCompletion,
    target: &ClipboardPasteTarget,
    app: &mut AppView,
) {
    if !eligible || completion != ClipboardPasteCompletion::FullMiss {
        return;
    }
    show_clipboard_toast(target, X11_PRIMARY_PASTE_HINT, app);
}
/// Whether a completed clipboard probe should fall through to the `echoagent wrap`
/// host-image request. A clean `FullMiss` always qualifies; a remote read
/// *error* (`AttachmentRead`) also qualifies because inside `echoagent wrap` the
/// authoritative pasteboard is the local host's, not the (absent) remote one, so
/// the error is recoverable over the wrap OSC path. Every other failure
/// (`TextRead`, `TargetInsertion`, `AlreadyReported`) is a real dead end and
/// must keep toasting. The request itself still self-gates on
/// `osc52_sink_active()`, so this is inert outside `echoagent wrap`.
pub(super) fn wrap_host_image_request_eligible(completion: ClipboardPasteCompletion) -> bool {
    matches!(
        completion,
        ClipboardPasteCompletion::FullMiss
            | ClipboardPasteCompletion::Failed(ClipboardPasteFailure::AttachmentRead)
    )
}
pub(super) fn show_clipboard_failure(
    target: &ClipboardPasteTarget,
    failure: ClipboardPasteFailure,
    app: &mut AppView,
) {
    let message = match failure {
        ClipboardPasteFailure::AlreadyReported => return,
        ClipboardPasteFailure::TextRead => "Couldn't read clipboard text",
        ClipboardPasteFailure::AttachmentRead => "Couldn't read clipboard contents",
        ClipboardPasteFailure::TargetInsertion => "Couldn't paste clipboard contents",
    };
    show_clipboard_toast(target, message, app);
}
fn apply_clipboard_paste_result(
    ctx: ClipboardPasteContext,
    image: ProbedAttachment,
    file_urls: Option<String>,
    app: &mut AppView,
) -> ClipboardPasteCompletion {
    match ctx.target.clone() {
        ClipboardPasteTarget::AgentPrompt { agent_id, .. } => app
            .agents
            .get_mut(&agent_id)
            .map_or(ClipboardPasteCompletion::Dropped, |agent| {
                agent.complete_clipboard_attachment_paste(ctx, image, file_urls)
            }),
        ClipboardPasteTarget::DashboardDispatch | ClipboardPasteTarget::DashboardPeek { .. } => app
            .dashboard
            .as_mut()
            .map_or(ClipboardPasteCompletion::Dropped, |dashboard| {
                dashboard.complete_clipboard_attachment_paste(ctx, image, file_urls)
            }),
    }
}
fn drain_clipboard_target(target: &ClipboardPasteTarget, app: &mut AppView) -> Vec<Effect> {
    match target {
        ClipboardPasteTarget::AgentPrompt { agent_id, .. } => {
            let is_active = app.active_view == ActiveView::Agent(*agent_id);
            let Some(agent) = app.agents.get_mut(agent_id) else {
                return vec![];
            };
            let resend = agent.take_deferred_send_after_paste();
            let action = resend
                .filter(|kind| is_active || matches!(kind, AgentDeferredSend::Stash))
                .and_then(|kind| agent.resume_deferred_send(kind));
            let mut effects = std::mem::take(&mut agent.pending_effects);
            if let Some(action) = action {
                effects.extend(dispatch(action, app));
            }
            effects
        }
        ClipboardPasteTarget::DashboardDispatch | ClipboardPasteTarget::DashboardPeek { .. } => {
            let Some(dashboard) = app.dashboard.as_mut() else {
                return vec![];
            };
            let resends = dashboard.take_deferred_sends_after_paste();
            let mut effects = std::mem::take(&mut dashboard.pending_effects);
            if matches!(app.active_view, ActiveView::AgentDashboard) {
                for action in resends {
                    effects.extend(dispatch(action, app));
                }
            }
            effects
        }
    }
}
pub(crate) fn current_doctor_target(
    app: &AppView,
    target: &DoctorFixTarget,
) -> Option<DoctorFixTarget> {
    let agent = app.agents.get(&target.agent_id)?;
    if agent.session.cwd != target.cwd {
        return None;
    }
    match (&target.session_id, &agent.session.session_id) {
        (Some(expected), Some(current))
            if expected == current
                && target.session_binding_epoch == agent.session_binding_epoch =>
        {
            Some(target.clone())
        }
        (None, Some(current))
            if agent.session_binding_epoch == target.session_binding_epoch.wrapping_add(1) =>
        {
            Some(DoctorFixTarget {
                session_id: Some(current.clone()),
                session_binding_epoch: agent.session_binding_epoch,
                ..target.clone()
            })
        }
        (None, None) if target.session_binding_epoch == agent.session_binding_epoch => {
            Some(target.clone())
        }
        _ => None,
    }
}
pub(crate) fn deliver_doctor_message(app: &mut AppView, preferred: AgentId, message: String) {
    let destination = app
        .agents
        .contains_key(&preferred)
        .then_some(preferred)
        .or_else(|| match app.active_view {
            ActiveView::Agent(id) if app.agents.contains_key(&id) => Some(id),
            _ => app.agents.keys().next().copied(),
        });
    if let Some(destination) = destination
        && let Some(agent) = app.agents.get_mut(&destination)
    {
        agent.scrollback.push_block(RenderBlock::system(message));
        return;
    }
    app.startup_warnings.push(crate::startup::StartupWarning {
        severity: crate::startup::WarningSeverity::Info,
        message,
        action: None,
    });
}
/// Handle a completed async task result.
pub(super) fn dispatch_task_result(result: TaskResult, app: &mut AppView) -> Vec<Effect> {
    if result.ends_startup() {
        app.finish_startup(echo_agent_telemetry::startup::StartupOutcome::Ok);
    }
    match result {
        TaskResult::SessionCreated {
            agent_id,
            session_id,
            models: new_models,
            scheduler_background_loops,
        } => handle_session_created(
            app,
            agent_id,
            session_id,
            new_models,
            scheduler_background_loops,
        ),
        TaskResult::SessionFailed { agent_id, error } => {
            handle_session_failed(app, agent_id, error)
        }
        TaskResult::WorktreeSessionCreated {
            agent_id,
            session_id,
            worktree_path,
            session_cwd,
            models: new_models,
            scheduler_background_loops,
        } => handle_worktree_session_created(
            app,
            agent_id,
            session_id,
            worktree_path,
            session_cwd,
            new_models,
            scheduler_background_loops,
        ),
        TaskResult::WorktreeForked {
            agent_id,
            session_id,
            worktree_path,
            session_cwd,
            code_restored,
            restore_summary,
            restore_degree,
            resume_session_id,
        } => handle_worktree_forked(
            app,
            agent_id,
            session_id,
            worktree_path,
            session_cwd,
            code_restored,
            restore_summary,
            restore_degree,
            resume_session_id,
        ),
        TaskResult::WorktreeSessionFailed { agent_id, error } => {
            handle_worktree_session_failed(app, agent_id, error)
        }
        TaskResult::ForkSessionReady {
            agent_id,
            new_session_id,
            cwd,
            parent_session_id,
        } => handle_fork_session_ready(app, agent_id, new_session_id, cwd, parent_session_id),
        TaskResult::ForkSessionFailed { agent_id, error } => {
            handle_fork_session_failed(app, agent_id, error)
        }
        TaskResult::BillingFetched {
            agent_id,
            balance,
            silent,
            subscription_tier,
            autotopup,
            nonce,
        } => handle_billing_fetched(
            app,
            agent_id,
            balance,
            silent,
            subscription_tier,
            autotopup,
            nonce,
        ),
        TaskResult::BillingError {
            agent_id,
            error,
            silent,
            nonce,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                if let Some(state) = usage_modal_state_mut(agent)
                    && state.fetch_nonce == nonce
                {
                    state.billing_loading = false;
                    state.billing_error = Some(error.clone());
                }
                if !silent {
                    agent.scrollback.push_block(RenderBlock::System(
                        crate::scrollback::blocks::SystemMessageBlock::new(format!(
                            "Billing error: {error}"
                        )),
                    ));
                }
            }
            vec![]
        }
        TaskResult::AppBillingFetched { balance, autotopup } => {
            app.credit_balance = balance;
            apply_auto_topup(&mut app.auto_topup, &autotopup);
            vec![]
        }
        TaskResult::GateRefreshed { settings } => handle_gate_refreshed(app, settings),
        TaskResult::SessionLoaded {
            agent_id,
            session_id,
            models: new_models,
            code_restored,
            restore_summary,
            restore_degree,
            running_prompt_id,
            scheduler_background_loops,
        } => handle_session_loaded(
            app,
            agent_id,
            session_id,
            new_models,
            code_restored,
            restore_summary,
            restore_degree,
            running_prompt_id,
            scheduler_background_loops,
        ),
        TaskResult::SessionMetaFromDisk {
            agent_id,
            title,
            last_turn_summary,
            last_turn_summary_gen,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                if let Some((raw, is_manual)) = title
                    && let Some(t) =
                        echo_agent_runtime::session::persistence::sanitize_and_cap_title(&raw)
                {
                    if is_manual && agent.display_name.is_none() {
                        agent.display_name = Some(t.clone());
                    }
                    if agent.generated_session_title.is_none() {
                        agent.generated_session_title = Some(t);
                    }
                }
                if agent.last_turn_summary_gen == last_turn_summary_gen
                    && agent.last_turn_summary.is_none()
                {
                    agent.last_turn_summary = last_turn_summary;
                }
            }
            vec![]
        }
        TaskResult::SessionLoadFailed {
            agent_id,
            session_id,
            error,
        } => handle_session_load_failed(app, agent_id, session_id, error),
        TaskResult::SessionListLoaded {
            sessions,
            partial,
            scope,
            seq,
            query,
        } => handle_session_list_loaded(app, sessions, partial, scope, seq, query),
        TaskResult::ForeignSessionsScanned { entries, seq } => {
            handle_foreign_sessions_scanned(app, entries, seq)
        }
        TaskResult::ForeignResumeCwdCanonicalized {
            requested_cwd,
            canonical_cwd,
            launch_token,
        } => {
            let accepted_cwd = canonical_cwd.clone();
            if app.accept_foreign_resume_canonical_cwd(launch_token, &requested_cwd, canonical_cwd)
                && let Some(canonical_cwd) = accepted_cwd
            {
                vec![Effect::DetectForeignResumeHint {
                    canonical_cwd,
                    compat: app.foreign_session_compat,
                    echo_agent_home: echo_agent_tools::util::echo_agent_home::echo_agent_home(),
                    launch_token,
                }]
            } else {
                vec![]
            }
        }
        TaskResult::ForeignResumeHintDetected {
            canonical_cwd,
            launch_token,
            hint,
        } => {
            app.apply_foreign_resume_detection(launch_token, &canonical_cwd, hint);
            vec![]
        }
        TaskResult::SessionListFailed { error, seq, query } => {
            handle_session_list_failed(app, error, seq, query)
        }
        TaskResult::SessionSearchDebounceExpired { query, seq } => {
            handle_session_search_debounce_expired(app, query, seq)
        }
        TaskResult::RosterLoaded { sessions } => {
            app.leader_roster = sessions;
            app.dashboard_sessions_loading = false;
            vec![]
        }
        TaskResult::RosterFailed { error } => {
            tracing::debug!(error = %error, "leader roster fetch failed");
            app.dashboard_sessions_loading = false;
            vec![]
        }
        TaskResult::DashboardSessionsLoaded { sessions } => {
            app.dashboard_local_sessions = sessions;
            app.dashboard_sessions_loading = false;
            vec![]
        }
        TaskResult::CardDetailLoaded {
            source,
            session_id,
            generation,
            detail,
        } => handle_card_detail_loaded(app, source, session_id, generation, detail),
        TaskResult::SessionRestored {
            agent_id,
            local_session_id,
        } => handle_session_restored(app, agent_id, local_session_id),
        TaskResult::SessionRestoreFailed { agent_id, error } => {
            handle_session_restore_failed(app, agent_id, error)
        }
        TaskResult::SessionRestoreProgress { agent_id, message } => {
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && !defer_to_open_reload_window(agent, agent_id, "SessionRestoreProgress")
            {
                agent.scrollback.push_block(RenderBlock::system(message));
            }
            vec![]
        }
        TaskResult::PromptResponse {
            agent_id,
            result,
            http_status,
            prompt_id,
        } => {
            let effects = handle_prompt_response(app, agent_id, result, http_status, prompt_id);
            app.refresh_status_line_for(agent_id);
            effects
        }
        TaskResult::SendPromptNowFailed {
            agent_id,
            session_id,
            prompt_id,
            error,
            blocks,
        } => {
            let sid = session_id.0.to_string();
            super::queue::retire_optimistic_echo(
                &mut app.optimistic_prompt_echoes,
                &mut app.shared_prompt_queues,
                &sid,
                &prompt_id,
            );
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.shared_queue.retain(|e| e.id != prompt_id);
                agent.note_queue_echo_retired(&prompt_id);
                if agent.expect_send_now_cancel.as_deref() == Some(prompt_id.as_str())
                    || agent.follow_without_jump_prompt_id.as_deref() == Some(prompt_id.as_str())
                {
                    agent.clear_send_now_expectation();
                }
                agent.retire_send_now_painted_block(&prompt_id);
                let text = blocks
                    .iter()
                    .find_map(|b| match b {
                        acp::ContentBlock::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let id = agent.session.next_queue_id;
                agent.session.next_queue_id += 1;
                agent
                    .session
                    .pending_prompts
                    .push_front(crate::app::agent::QueuedPrompt {
                        wire_blocks: Some(blocks),
                        ..crate::app::agent::QueuedPrompt::plain(
                            id,
                            &text,
                            crate::app::agent::QueueEntryKind::Prompt,
                        )
                    });
                agent.show_toast(&format!("Send now failed. Requeued: {error}"));
            }
            vec![]
        }
        TaskResult::PreferredModelPersisted { result } => {
            if let Err(err) = result
                && let Some(agent) = get_active_agent_mut(app)
            {
                agent.scrollback.push_block(RenderBlock::system(format!(
                    "Couldn't save preferred model: {err} (still active for this session)"
                )));
            }
            vec![]
        }
        TaskResult::CancelComplete => {
            tracing::trace!("Cancel notification sent successfully");
            vec![]
        }
        TaskResult::ConsentPersistFailed { error } => {
            tracing::warn!(%error, "consent answer not persisted; the notice re-arms next launch");
            app.show_toast(
                "\u{2717} Could not save your answer, so this notice returns next launch",
            );
            vec![]
        }
        TaskResult::ConsentRecorded { notice_id, version } => match app.account_email.clone() {
            Some(account) => {
                vec![Effect::PersistConsentAnswer {
                    account: Some(account),
                    notice_id,
                    version,
                    acked: true,
                }]
            }
            None => vec![],
        },
        TaskResult::KillSubagentComplete {
            session_id,
            subagent_id,
            outcome,
        } => {
            if let SubagentKillOutcome::NothingLive { status } = outcome {
                let status = status.as_deref().unwrap_or("cancelled");
                crate::app::acp_handler::finalize_killed_subagent(
                    app,
                    &session_id,
                    &subagent_id,
                    status,
                );
            }
            vec![]
        }
        TaskResult::CompactComplete { agent_id, result } => {
            handle_compact_complete(app, agent_id, result)
        }
        TaskResult::SwitchModelComplete {
            agent_id,
            model_id,
            effort,
            result,
            prev_model_id,
        } => handle_switch_model_complete(app, agent_id, model_id, effort, result, prev_model_id),
        TaskResult::BgTaskKilled {
            session_id,
            task_id,
            outcome,
        } => handle_bg_task_killed(app, session_id, task_id, outcome),
        TaskResult::BgTaskKillFailed {
            session_id,
            task_id,
            error,
        } => {
            tracing::warn!(task_id = %task_id, error = %error, "Failed to kill bg task");
            if let Some(agent) = find_agent_by_session_id(&mut app.agents, &session_id)
                && let Some(task) = agent.session.bg_tasks.get_mut(&task_id)
            {
                task.pending_kill = false;
                task.kill_requested_at = None;
            }
            vec![]
        }
        TaskResult::ChangelogFetched { markdown, entries } => {
            app.changelog_markdown = markdown;
            app.changelog_bullets =
                echo_agent_runtime::util::changelog::bullets_from_entries(&entries, 3);
            vec![]
        }
        TaskResult::ClipboardAttachmentProbed {
            ctx,
            image,
            file_urls,
        } => {
            let is_clipboard_key = ctx.source.is_clipboard_key();
            let primary_hint_eligible = is_clipboard_key
                && !app.screen_mode.is_minimal()
                && crate::clipboard::x11_primary_guidance_available();
            let target = ctx.target.clone();
            let wrap_text = if is_clipboard_key {
                ctx.source.text().map(str::to_owned)
            } else {
                None
            };
            let completion = apply_clipboard_paste_result(ctx, image, file_urls, app);
            let wrap_request_emitted = wrap_host_image_request_eligible(completion)
                && is_clipboard_key
                && crate::wrap_clipboard_image::maybe_request_wrap_host_image(
                    None,
                    wrap_text.as_deref(),
                    None,
                );
            let effects = drain_clipboard_target(&target, app);
            maybe_show_x11_primary_paste_hint(
                primary_hint_eligible && !wrap_request_emitted,
                completion,
                &target,
                app,
            );
            if let ClipboardPasteCompletion::Failed(failure) = completion
                && !wrap_request_emitted
            {
                show_clipboard_failure(&target, failure, app);
            }
            effects
        }
        TaskResult::PromptImagePreviewPrepared => vec![],
        TaskResult::DoctorFixPlanned { target, result } => {
            let Some(target) = current_doctor_target(app, &target) else {
                deliver_doctor_message(
                    app,
                    target.agent_id,
                    "This fix was cancelled because the session changed. Run `/doctor fix` again."
                        .to_owned(),
                );
                return vec![];
            };
            match result {
                Ok(DoctorPlanningOutcome::Listing(listing)) => {
                    deliver_doctor_message(app, target.agent_id, listing);
                }
                Ok(DoctorPlanningOutcome::Plan(plan)) => {
                    super::prompt::open_doctor_fix_question(app, target, plan);
                }
                Ok(DoctorPlanningOutcome::RunLocally(command)) => {
                    deliver_doctor_message(
                        app,
                        target.agent_id,
                        format!(
                            "This fix configures your local computer, not this SSH session.\nOn your local computer, run: {command}"
                        ),
                    );
                }
                Err(error) => deliver_doctor_message(
                    app,
                    target.agent_id,
                    if error.starts_with("Could not prepare the fix:") {
                        error
                    } else {
                        format!("Could not prepare the fix: {error}")
                    },
                ),
            }
            vec![]
        }
        TaskResult::DoctorFixApplied { target, result } => {
            let message = match result {
                Ok(outcome) => crate::diagnostics::format_fix_success(&outcome),
                Err(error) if error.starts_with("Could not apply the fix:") => error,
                Err(error) => format!("Could not apply the fix: {error}"),
            };
            deliver_doctor_message(app, target.agent_id, message);
            vec![]
        }
        TaskResult::AnnouncementsHiddenPersisted { result } => {
            if let Err(e) = result {
                tracing::warn!("Failed to persist announcements hidden state: {}", e);
            }
            vec![]
        }
        TaskResult::PromptHistoryLoaded { agent_id, prompts } => {
            use echo_agent_tools::implementations::skills::skill::extract_skill_display_text;
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.session.prompt_history_loading = false;
                let fetched: Vec<String> = prompts
                    .into_iter()
                    .map(|p| extract_skill_display_text(&p).unwrap_or(p))
                    .collect();
                let local: std::collections::HashSet<String> = agent
                    .session
                    .prompt_history
                    .iter()
                    .flat_map(|p| {
                        let t = p.trim();
                        [t.to_owned(), t.strip_prefix("! ").unwrap_or(t).to_owned()]
                    })
                    .collect();
                let local_entries = agent.session.prompt_history.len();
                let fetched_entries = fetched.len();
                agent
                    .session
                    .prompt_history
                    .extend(fetched.into_iter().filter(|p| !local.contains(p.trim())));
                agent
                    .session
                    .prompt_history
                    .truncate(crate::app::agent::PROMPT_HISTORY_CAP);
                tracing::info!(
                    history.local_entries = local_entries,
                    history.fetched_entries = fetched_entries,
                    history.merged_entries = agent.session.prompt_history.len(),
                    "history.fetch_merged"
                );
                if agent.prompt.history_search.is_active() {
                    let history = agent.combined_prompt_history();
                    agent.prompt.history_search.refresh_items(&history);
                    if !agent.prompt.history_search.is_browse() {
                        let query = agent.prompt.text().to_owned();
                        agent.prompt.history_search.update_query(&query);
                    }
                }
            }
            vec![]
        }
        TaskResult::AuthComplete { request_seq, meta } => {
            handle_auth_complete(app, request_seq, meta)
        }
        TaskResult::AuthFailed { request_seq, error } => {
            if let AuthState::Authenticating {
                request_seq: current_seq,
                ..
            } = &app.auth_state
                && *current_seq == request_seq
            {
                app.auth_state = AuthState::Pending { error: Some(error) };
                app.auth_code_input.reset();
            }
            vec![]
        }
        TaskResult::AuthUrlReady {
            request_seq,
            auth_url,
            external,
            mode,
        } => handle_auth_url_ready(app, request_seq, auth_url, external, mode),
        TaskResult::AuthCodeSubmitted { .. } => vec![],
        TaskResult::AuthCancelComplete => vec![],
        TaskResult::McpsListLoaded { agent_id, result } => {
            use crate::views::extensions_modal::TabDataState;
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some(ref mut modal) = agent.extensions_modal
            {
                modal.pending_action = None;
                modal.pending_entry_index = None;
                modal.mcps_data = match result {
                    Ok(response) => TabDataState::Loaded(response),
                    Err(e) => TabDataState::Error(e),
                };
            }
            vec![]
        }
        TaskResult::McpAuthTriggerDone {
            agent_id,
            server_name,
            result,
        } => handle_mcp_auth_trigger_done(app, agent_id, server_name, result),
        TaskResult::McpSetupSubmitDone {
            agent_id,
            server_name,
            result,
        } => handle_mcp_setup_submit_done(app, agent_id, server_name, result),
        TaskResult::HooksListLoaded { agent_id, result } => {
            handle_hooks_list_loaded(app, agent_id, result)
        }
        TaskResult::PluginsListLoaded { agent_id, result } => {
            handle_plugins_list_loaded(app, agent_id, result)
        }
        TaskResult::HooksActionResult { agent_id, result }
        | TaskResult::PluginsActionResult { agent_id, result }
        | TaskResult::MarketplaceActionResult { agent_id, result } => {
            dispatch_action_result(app, agent_id, result)
        }
        TaskResult::CtaPluginInstallDone {
            agent_id,
            plugin_name,
            result,
        } => handle_cta_plugin_install_done(app, agent_id, plugin_name, result),
        TaskResult::CtaPluginReloadDone {
            agent_id,
            plugin_name,
            result,
        } => handle_cta_plugin_reload_done(app, agent_id, plugin_name, result),
        TaskResult::PluginCtaMcpsLoaded {
            agent_id,
            plugin_name,
            result,
        } => handle_plugin_cta_mcps_loaded(app, agent_id, plugin_name, result),
        TaskResult::CtaInstalledDismissTimeout {
            agent_id,
            plugin_name,
        } => {
            use crate::app::agent_view::CtaPhase;
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && let CtaPhase::Installed { name } = &agent.plugin_cta.phase
                && *name == plugin_name
            {
                agent.plugin_cta.phase = CtaPhase::Hidden;
            }
            vec![]
        }
        TaskResult::McpToggleDone { agent_id, result } => {
            handle_mcp_toggle_done(app, agent_id, result)
        }
        TaskResult::MarketplaceUpdatesAvailable { agent_id, updates } => {
            handle_marketplace_updates_available(app, agent_id, updates)
        }
        TaskResult::MarketplaceListLoaded { agent_id, result } => {
            handle_marketplace_list_loaded(app, agent_id, result)
        }
        TaskResult::PluginCtaCatalogLoaded { agent_id, result } => {
            handle_plugin_cta_catalog_loaded(app, agent_id, result)
        }
        TaskResult::SkillsListLoaded { agent_id, result } => {
            use crate::views::extensions_modal::TabDataState;
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && let Some(ref mut modal) = agent.extensions_modal
            {
                modal.skills_data = match result {
                    Ok(skills) => {
                        modal.seed_skills_groups_once(&skills);
                        TabDataState::Loaded(skills)
                    }
                    Err(e) => TabDataState::Error(e),
                };
                modal.pending_action = None;
                modal.pending_entry_index = None;
            }
            vec![]
        }
        TaskResult::WorkflowsListLoaded {
            agent_id,
            session_id,
            result,
        } => {
            use crate::views::extensions_modal::TabDataState;
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && agent.session.session_id.as_ref() == Some(&session_id)
                && let Some(ref mut modal) = agent.extensions_modal
            {
                modal.workflows_data = match result {
                    Ok(workflows) => TabDataState::Loaded(workflows),
                    Err(e) => TabDataState::Error(e),
                };
            }
            vec![]
        }
        TaskResult::SkillsToggleDone { agent_id, result } => {
            handle_skills_toggle_done(app, agent_id, result)
        }
        TaskResult::ShareSessionComplete {
            agent_id,
            share_url,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Session shared: {share_url}"
                    )));
            }
            vec![]
        }
        TaskResult::ShareSessionFailed { agent_id, error } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't share session: {error}"
                    )));
            }
            vec![]
        }
        TaskResult::SessionAgentNameResolved {
            agent_id,
            agent_name,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.session_agent_name = agent_name.clone();
                if let Some(modal) = agent.agents_modal.as_mut() {
                    modal.active_agent = agent_name;
                }
            }
            vec![]
        }
        TaskResult::SessionInfoComplete {
            agent_id,
            session_id,
            info,
            text,
            fields,
            nonce,
        } => {
            let minimal = app.screen_mode.is_minimal();
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                if agent.session.session_id.as_ref() != Some(&session_id) {
                    return vec![];
                }
                if let Some(state) = usage_modal_state_mut(agent)
                    && state.fetch_nonce != nonce
                {
                    return vec![];
                }
                agent.session_agent_name = info.data.agent_name.clone();
                if let Some(modal) = agent.agents_modal.as_mut() {
                    modal.active_agent = info.data.agent_name.clone();
                }
                agent.apply_full_context_info(info.data.context);
                if let Some(state) = usage_modal_state_mut(agent) {
                    state.session_fields = Some(fields);
                    state.session_error = None;
                } else if minimal {
                    push_and_page_flip(
                        &mut agent.scrollback,
                        crate::scrollback::block::RenderBlock::system(text),
                    );
                }
            }
            vec![]
        }
        TaskResult::SessionInfoFailed {
            agent_id,
            session_id,
            error,
            nonce,
        } => {
            let minimal = app.screen_mode.is_minimal();
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                if agent.session.session_id.as_ref() != Some(&session_id) {
                    return vec![];
                }
                if let Some(state) = usage_modal_state_mut(agent) {
                    if state.fetch_nonce == nonce {
                        state.session_error = Some(error);
                    }
                } else if minimal {
                    push_and_page_flip(
                        &mut agent.scrollback,
                        crate::scrollback::block::RenderBlock::system(format!(
                            "Couldn't load session info: {error}"
                        )),
                    );
                }
            }
            vec![]
        }
        TaskResult::CodingDataSharingUpdated {
            agent_id,
            opted_in,
            seq,
        } => handle_coding_data_sharing_updated(app, agent_id, opted_in, seq),
        TaskResult::CodingDataSharingFailed {
            agent_id,
            error,
            rollback_to_opted_in,
            seq,
        } => handle_coding_data_sharing_failed(app, agent_id, error, rollback_to_opted_in, seq),
        TaskResult::RenameSessionComplete { agent_id, title } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                let safe = crate::views::session_title::sanitize_display_text(&title);
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Session renamed to \"{safe}\""
                    )));
            }
            vec![]
        }
        TaskResult::RenameSessionFailed { agent_id, error } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't rename session: {error}"
                    )));
            }
            vec![]
        }
        TaskResult::ResetSessionTitleComplete { agent_id } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.title_unpin_committed = false;
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(
                        "Session title reset to auto",
                    ));
            }
            vec![]
        }
        TaskResult::ResetSessionTitleFailed {
            agent_id,
            error,
            previous_display_name,
            previous_generated_title,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                if agent.title_unpin_committed {
                    agent.title_unpin_committed = false;
                    agent
                        .scrollback
                        .push_block(crate::scrollback::block::RenderBlock::system(
                            "Session title reset to auto",
                        ));
                } else {
                    agent.display_name = previous_display_name;
                    agent.generated_session_title = previous_generated_title;
                    agent
                        .scrollback
                        .push_block(crate::scrollback::block::RenderBlock::system(format!(
                            "Couldn't reset session title: {error}"
                        )));
                }
            }
            vec![]
        }
        TaskResult::DeleteSessionComplete {
            source,
            session_id,
            after,
        } => {
            use crate::app::actions::AfterSessionDelete;
            remove_session_from_pickers(
                app,
                &source,
                &session_id,
                after != AfterSessionDelete::Stay,
            );
            if after == AfterSessionDelete::Stay {
                app.dashboard_local_sessions
                    .retain(|entry| entry.session_id != session_id);
                app.leader_roster
                    .retain(|entry| entry.session_id != session_id);
                app.show_toast("Session deleted");
                return vec![];
            }
            let sid = acp::SessionId::new(session_id.clone());
            let to_remove: Vec<_> = app
                .agents
                .iter()
                .filter(|(_, agent)| agent.session.session_id.as_ref() == Some(&sid))
                .map(|(id, _)| *id)
                .collect();
            let foreground =
                matches!(app.active_view, ActiveView::Agent(id) if to_remove.contains(&id));
            let roster_row = crate::views::dashboard::DashboardRowId::Roster {
                session_id: session_id.clone(),
            };
            let closed_rows: Vec<_> = to_remove
                .iter()
                .copied()
                .map(crate::views::dashboard::DashboardRowId::TopLevel)
                .chain(std::iter::once(roster_row))
                .collect();
            let selected = app.dashboard.as_ref().and_then(|d| d.selected.clone());
            let neighbor = if after == AfterSessionDelete::Dashboard
                && let Some(sel) = selected.as_ref().filter(|sel| closed_rows.contains(sel))
            {
                super::dashboard::dashboard_neighbor_row(app, sel)
            } else {
                None
            };
            app.dashboard_local_sessions
                .retain(|entry| entry.session_id != session_id);
            app.leader_roster
                .retain(|entry| entry.session_id != session_id);
            let attached_was_removed = app
                .dashboard
                .as_ref()
                .and_then(|d| d.attached_agent)
                .is_some_and(|id| to_remove.contains(&id));
            for id in to_remove {
                remove_agent_and_cleanup(app, id);
            }
            let mut effects = unregister_session_effect(Some(sid));
            if after == AfterSessionDelete::Dashboard {
                if let Some(d) = app.dashboard.as_mut() {
                    d.delete_confirm = None;
                    if attached_was_removed {
                        d.close_popup();
                    }
                    let selected_closed = d
                        .selected
                        .as_ref()
                        .is_some_and(|sel| closed_rows.contains(sel));
                    match (selected_closed, neighbor) {
                        (true, Some(n)) => d.focus_row(n),
                        (true, None) => d.focus_new_agent_button(),
                        _ => {}
                    }
                }
                if foreground {
                    super::dashboard::ensure_dashboard_state(app);
                    app.active_view = ActiveView::AgentDashboard;
                }
            } else if foreground && after == AfterSessionDelete::Welcome {
                effects.extend(dispatch_exit_session(app));
            }
            app.show_toast("Session deleted");
            effects
        }
        TaskResult::DeleteSessionFailed {
            source,
            session_id,
            error,
        } => {
            tracing::warn!(source, session_id = %session_id, error = %error, "session delete failed");
            app.show_toast(&format!("Couldn't delete session: {error}"));
            vec![]
        }
        TaskResult::ContextInfoComplete {
            agent_id,
            session_id,
            info,
            nonce,
        } => handle_context_info_complete(app, agent_id, &session_id, info, nonce),
        TaskResult::ContextInfoFailed {
            agent_id,
            session_id,
            error,
            nonce,
        } => {
            let minimal = app.screen_mode.is_minimal();
            let Some(agent) = app.agents.get_mut(&agent_id) else {
                return vec![];
            };
            if agent.session.session_id.as_ref() != Some(&session_id) {
                return vec![];
            }
            if let Some(state) = usage_modal_state_mut(agent) {
                if state.fetch_nonce == nonce {
                    state.context_error = Some(error);
                }
            } else if minimal {
                push_and_page_flip(
                    &mut agent.scrollback,
                    crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't load context info: {error}"
                    )),
                );
            }
            vec![]
        }
        TaskResult::SessionUsageComplete {
            agent_id,
            session_id,
            usage,
            nonce,
        } => handle_session_usage_result(
            app,
            agent_id,
            &session_id,
            crate::app::status_blocks::session_usage_block_text(&usage),
            nonce,
        ),
        TaskResult::SessionUsageFailed {
            agent_id,
            session_id,
            error,
            nonce,
        } => handle_session_usage_result(
            app,
            agent_id,
            &session_id,
            format!("Couldn't load session usage: {error}"),
            nonce,
        ),
        TaskResult::FeedbackComplete { .. } => vec![],
        TaskResult::FeedbackFailed { agent_id, error } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't send feedback: {error}"
                    )));
            }
            vec![]
        }
        TaskResult::FeedbackTraceUploaded { agent_id, error } => {
            if let Some(error) = error
                && let Some(agent) = app.agents.get_mut(&agent_id)
            {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Couldn't upload a session trace; your feedback was still sent. {error}"
                    )));
            }
            vec![]
        }
        TaskResult::MemoryNoteSaved { agent_id, result } => {
            handle_memory_note_saved(app, agent_id, result)
        }
        TaskResult::MemoryNoteRewritten {
            agent_id,
            result,
            nonce,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id)
                && let Ok(markdown) = result
                && let Some(crate::views::modal::ActiveModal::RememberNoteReview {
                    ref mut enhanced_content,
                    ref mut cached_lines,
                    rewrite_nonce,
                    ..
                }) = agent.active_modal
                && rewrite_nonce == nonce
            {
                *enhanced_content = Some(markdown);
                *cached_lines = None;
            }
            vec![]
        }
        TaskResult::BundleStatusReady {
            has_cache,
            version,
            personas,
            roles,
            agents,
            skills,
            persona_details,
            role_details,
        } => {
            app.bundle_state.has_cache = has_cache;
            app.bundle_state.version = version.unwrap_or_default();
            app.bundle_state.personas = personas;
            app.bundle_state.roles = roles;
            app.bundle_state.agents = agents;
            app.bundle_state.skills = skills;
            app.bundle_state.persona_details = persona_details;
            app.bundle_state.role_details = role_details;
            vec![]
        }
        TaskResult::BundleStatusFailed { error } => {
            tracing::warn!(error = %error, "bundle status fetch failed");
            vec![]
        }
        TaskResult::CatalogEntryReady {
            kind,
            name,
            content,
        } => {
            if let ActiveView::Agent(id) = app.active_view
                && let Some(agent) = app.agents.get_mut(&id)
            {
                let title = format!("{kind}: {name}");
                agent.block_viewer = Some(
                    crate::views::block_viewer::BlockViewerPane::for_plain_text(&title, &content),
                );
            }
            vec![]
        }
        TaskResult::CatalogEntryFailed { error } => {
            tracing::warn!(error = %error, "catalog entry fetch failed");
            if let ActiveView::Agent(id) = app.active_view
                && let Some(agent) = app.agents.get_mut(&id)
            {
                agent
                    .scrollback
                    .push_block(RenderBlock::system(format!("Couldn't load entry: {error}")));
            }
            vec![]
        }
        TaskResult::BtwResponse {
            agent_id,
            result,
            minimal_request_id,
        } => handle_btw_response(app, agent_id, result, minimal_request_id),
        TaskResult::InterjectQueued { .. } => vec![],
        TaskResult::RecapRequested {
            session_id,
            auto,
            error,
        } => {
            if let Some(error) = error {
                tracing::debug!(%error, "recap request failed");
                if !auto
                    && let Some(agent) = find_agent_by_session_id(&mut app.agents, &session_id.0)
                    && let Some(pending_id) = agent.pending_recap_entry.take()
                {
                    agent.scrollback.remove_entry(pending_id);
                    agent.show_toast(super::recap_unavailable_toast(
                        super::scrollback_has_user_messages(&agent.scrollback),
                    ));
                }
            }
            vec![]
        }
        TaskResult::InterjectFailed {
            agent_id,
            error,
            text,
            blocks,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                let id = agent.session.next_queue_id;
                agent.session.next_queue_id += 1;
                agent
                    .session
                    .pending_prompts
                    .push_front(crate::app::agent::QueuedPrompt {
                        id,
                        text,
                        kind: crate::app::agent::QueueEntryKind::Prompt,
                        wire_blocks: blocks,
                        images: Vec::new(),
                        display_as_skill: false,
                        task_id: None,
                        human_schedule: None,
                        chip_elements: Vec::new(),
                        skill_token_ranges: Vec::new(),
                        combined_texts: Vec::new(),
                    });
                agent.show_toast(&format!("Interjection failed. Requeued: {error}"));
            }
            vec![]
        }
        TaskResult::AvailableCommandsRefreshed { agent_id, commands } => {
            if !commands.is_empty()
                && let Some(agent) = app.agents.get_mut(&agent_id)
            {
                agent.session.available_commands = commands;
                agent.session.available_commands_generation += 1;
                super::super::acp_handler::refresh_workflow_run_capabilities(agent);
            }
            vec![]
        }
        TaskResult::StatusLineCommandFinished { id, outcome } => {
            app.on_status_line_command_finished(id, outcome);
            vec![]
        }
        TaskResult::AuthCopyFeedbackTimeout { generation } => {
            if generation == app.auth_clipboard_feedback_generation {
                app.auth_clipboard_delivery = None;
            }
            vec![]
        }
        TaskResult::PaywallCheckTick => {
            let timed_out = app
                .paywall_check_started
                .is_some_and(|t| t.elapsed() >= PAYWALL_AUTO_CHECK_TIMEOUT);
            if !app.has_access() && !timed_out {
                vec![
                    Effect::CheckSubscription { verify: None },
                    Effect::SchedulePaywallCheck,
                ]
            } else {
                vec![]
            }
        }
        TaskResult::CheckSubscriptionComplete { verify, meta } => {
            handle_check_subscription_complete(app, verify, meta)
        }
        TaskResult::GateVerifyTimeout { generation } => handle_gate_verify_timeout(app, generation),
        TaskResult::CreditLimitRecheckComplete { agent_id, meta } => {
            handle_credit_limit_recheck_complete(app, agent_id, meta)
        }
        TaskResult::LogoutComplete => {
            app.auth_state = AuthState::Pending { error: None };
            app.access_gate_shown_logged = false;
            app.announcement_cta_impressions_logged.clear();
            app.gate = None;
            app.pending_gate_verification = None;
            app.last_subscription_check_at = None;
            app.login_method_id = None;
            ensure_login_method(app);
            app.auth_clipboard_delivery = None;
            let effects = dispatch_exit_session(app);
            app.welcome_prompt_focused = false;
            effects
        }
        TaskResult::DeepSearchResults { results, seq } => {
            handle_deep_search_results(app, results, seq)
        }
        TaskResult::RewindPointsLoaded { agent_id, points } => {
            handle_rewind_points_loaded(app, agent_id, points)
        }
        TaskResult::RewindPointsFailed { agent_id, error } => {
            let Some(agent) = app.agents.get_mut(&agent_id) else {
                return vec![];
            };
            agent.rewind_state = None;
            app.show_toast(&format!("Undo failed: {error}"));
            vec![]
        }
        TaskResult::RewindExecuteComplete { agent_id, response } => {
            dispatch_rewind_success(app, agent_id, response)
        }
        TaskResult::RewindExecuteFailed { agent_id, error } => {
            handle_rewind_execute_failed(app, agent_id, error)
        }
        TaskResult::SuggestionDebounceExpired {
            agent_id,
            generation,
        } => handle_suggestion_debounce_expired(app, agent_id, generation),
        TaskResult::PluginCtaDebounceExpired {
            agent_id,
            generation,
        } => handle_plugin_cta_debounce_expired(app, agent_id, generation),
        TaskResult::ShellSuggestionsLoaded {
            agent_id,
            response,
            request_text,
            request_cursor,
        } => {
            let Some(agent) = app.agents.get_mut(&agent_id) else {
                return vec![];
            };
            if agent.prompt_input_mode != crate::app::agent_view::PromptInputMode::Bash {
                return vec![];
            }
            let generation = response.generation;
            agent
                .prompt
                .suggestions
                .on_suggestions_loaded(response, &request_text, request_cursor);
            let text = agent.prompt.text().to_owned();
            agent.prompt.suggestions.set_last_request_text(&text);
            let mark = agent.pending_effects.len();
            if agent.prompt.suggestions.take_pending_tab(generation) {
                agent.shell_completion_tab();
            }
            agent.pending_effects.split_off(mark)
        }
        TaskResult::PromptSuggestionLoaded {
            agent_id,
            suggestion,
            generation,
        } => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent
                    .prompt
                    .prompt_suggestion
                    .on_loaded(suggestion, generation);
                agent.refresh_prompt_suggestion_gate();
                agent.log_prompt_suggestion_shown_if_visible();
            }
            vec![]
        }
        TaskResult::SettingPersisted { key, value } => {
            tracing::trace!(target: "settings", ?key, ?value, "setting persisted");
            vec![]
        }
        TaskResult::SettingPersistFailed {
            key,
            rollback_value,
            error,
        } => {
            let rollback_effects = apply_setting_rollback(app, key, &rollback_value);
            tracing::warn!(target: "settings", ?key, ?rollback_value, %error, "setting persist failed; rolled back");
            let scrubbed = scrub_error_for_toast(&error);
            app.show_toast(&format!("\u{2717} Could not save {key}: {scrubbed}"));
            rollback_effects
        }
        TaskResult::SettingPersistFailedBestEffort { key, error } => {
            tracing::warn!(
                target: "settings",
                ?key, %error,
                "setting persist failed (best-effort); in-memory state stays at optimistic value",
            );
            let scrubbed = scrub_error_for_toast(&error);
            app.show_toast(&format!("\u{2717} Could not save {key}: {scrubbed}"));
            vec![]
        }
    }
}
