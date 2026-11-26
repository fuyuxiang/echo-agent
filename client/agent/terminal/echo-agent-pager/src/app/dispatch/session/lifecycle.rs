//! New, exit, cloud, and worktree session dispatchers plus trust and startup actions.
use super::fork::{dispatch_startup_fork_session, worktree_persist_options};
use super::load::dispatch_load_session;
use super::modal::remove_agent_and_cleanup;
use crate::acp::model_state::{EffortTokenError, ModelState};
use crate::acp::tracker::AcpUpdateTracker;
use crate::app::actions::{Action, Effect, SwitchModelError};
use crate::app::agent::{AgentCommand, AgentId, AgentSession, AgentState, DeferredModelSwitch};
use crate::app::agent_view::{ActivePane, AgentView, McpInitProgress};
use crate::app::app_view::{ActiveView, AppView, TrustState};
use crate::app::cancel_latency::TurnEnd;
use crate::app::consent::ConsentState;
use crate::app::dispatch::ctx::{
    SwitchCause, get_active_agent, reseed_tip_for_new_session, show_welcome, switch_to_agent,
};
use crate::app::dispatch::modes::inherit_auto_mode;
use crate::app::dispatch::prompt::{consume_chat_kind, dispatch_initial_prompt};
use crate::app::dispatch::queue::{QueueDrain, maybe_drain_queue, note_peek_page_flip};
use crate::app::dispatch::router::dispatch;
use crate::app::dispatch::status::notify_session_ready;
use crate::app::dispatch::task_result::unregister_session_effect;
use crate::app::dispatch::transcript::extensions_modal_tab_fetches;
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::SessionEvent;
use crate::scrollback::state::ScrollbackState;
use agent_client_protocol as acp;
use echo_agent_runtime::sampling::types::ReasoningEffort;
use std::time::Instant;
/// A deferred model switch to apply once the session exists, plus any effort
/// error to surface. `switch` is still populated when a `-m` model was stashed
/// even if the effort token failed, so an invalid effort never drops the CLI
/// model override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeferredSwitchOutcome {
    pub switch: Option<DeferredModelSwitch>,
    pub effort_error: Option<EffortTokenError>,
}
/// Resolve the stashed `-m` switch and/or `cli_effort_token` against the session
/// catalog via [`ModelState::resolve_effort_for_model`] (same gate-first policy
/// as `/effort` and headless).
pub(crate) fn take_deferred_model_switch(
    stashed: Option<DeferredModelSwitch>,
    models: &ModelState,
    cli_effort_token: Option<&str>,
) -> DeferredSwitchOutcome {
    if let Some(DeferredModelSwitch {
        model_id,
        mut effort,
        prev_model_id,
    }) = stashed
    {
        let effort_error = match cli_effort_token {
            Some(token) if effort.is_none() => {
                match models.resolve_effort_for_model(&model_id, token) {
                    Ok(resolved) => {
                        effort = Some(resolved);
                        None
                    }
                    Err(err) => Some(err),
                }
            }
            _ => None,
        };
        return DeferredSwitchOutcome {
            switch: Some(DeferredModelSwitch {
                model_id,
                effort,
                prev_model_id,
            }),
            effort_error,
        };
    }
    let Some(token) = cli_effort_token else {
        return DeferredSwitchOutcome {
            switch: None,
            effort_error: None,
        };
    };
    let Some(current) = models.current.clone() else {
        return DeferredSwitchOutcome {
            switch: None,
            effort_error: Some(EffortTokenError::NoActiveModel),
        };
    };
    match models.resolve_effort_for_model(&current, token) {
        Ok(effort) if models.reasoning_effort == Some(effort) => DeferredSwitchOutcome {
            switch: None,
            effort_error: None,
        },
        Ok(effort) => DeferredSwitchOutcome {
            switch: Some(DeferredModelSwitch {
                model_id: current,
                effort: Some(effort),
                prev_model_id: None,
            }),
            effort_error: None,
        },
        Err(err) => DeferredSwitchOutcome {
            switch: None,
            effort_error: Some(err),
        },
    }
}
/// Take the stashed deferred switch, resolve any CLI effort token against the
/// session catalog, surface effort errors, and return the switch to apply.
/// `prev_model_id` becomes the session's authoritative model when the
/// create/load response replaced the catalog, else the stash-time display.
pub(crate) fn apply_deferred_model_switch(
    agent: &mut AgentView,
    cli_effort_token: Option<&str>,
) -> Option<DeferredModelSwitch> {
    let stashed = agent.session.deferred_model_switch.take();
    let outcome = take_deferred_model_switch(stashed, &agent.session.models, cli_effort_token);
    apply_deferred_switch_outcome(agent, outcome).map(|mut switch| {
        if agent.session.models.current.as_ref() != Some(&switch.model_id) {
            switch.prev_model_id = agent.session.models.current.clone();
        }
        switch
    })
}
/// Surface effort-token errors and return the model/effort switch (if any).
pub(crate) fn apply_deferred_switch_outcome(
    agent: &mut AgentView,
    outcome: DeferredSwitchOutcome,
) -> Option<DeferredModelSwitch> {
    if let Some(err) = outcome.effort_error {
        let msg = format!("--effort/--reasoning-effort: {}", err.message());
        tracing::warn!("{msg}");
        agent.show_toast(&msg);
        agent.scrollback.push_block(RenderBlock::system(msg));
    }
    outcome.switch
}
/// Top-level `/new` dispatcher. If the working directory is inside a
/// git repository, consults the persisted `new_session_worktree_mode`
/// preference: `Always` skips the popup and creates a worktree, `Never`
/// skips the popup and stays in-cwd, `Ask` opens the worktree question
/// modal (mirroring `/fork`'s UX). Otherwise proceeds directly via
/// [`dispatch_new_session_inner`].
///
/// When no active agent exists (e.g. from the welcome screen), the
/// git-repo check falls back to `app.cwd_has_git_ancestor` so the
/// persisted `Always` / `Never` modes still take effect. The `Ask`
/// path falls through to `dispatch_new_session_inner` in that case
/// because `open_new_session_question` requires an active agent.
pub(in crate::app::dispatch) fn dispatch_new_session(app: &mut AppView) -> Vec<Effect> {
    use crate::app::app_view::WorktreeMode;
    if !app.session_startup_allowed() {
        app.deferred_startup.new_session = true;
        return vec![];
    }
    #[cfg(feature = "local-workspace")]
    if matches!(app.active_view, ActiveView::Welcome) {
        let skip_apply = app.welcome_session_local_workspace.is_some();
        if !skip_apply && let Err(effects) = apply_welcome_workspace_on_new_session(app) {
            return effects;
        }
    }
    let in_git_repo = get_active_agent(app)
        .map(|a| a.current_branch.is_some())
        .unwrap_or(app.cwd_has_git_ancestor);
    if in_git_repo {
        match app.new_session_worktree_mode {
            WorktreeMode::Always => {
                dispatch_new_worktree_session(app, None, None, None, None, None, None)
            }
            WorktreeMode::Never => dispatch_new_session_inner(app, None),
            WorktreeMode::Ask => {
                if matches!(app.active_view, ActiveView::Agent(_)) {
                    open_new_session_question(app)
                } else {
                    dispatch_new_session_inner(app, None)
                }
            }
        }
    } else {
        dispatch_new_session_inner(app, None)
    }
}
/// Open the local worktree question modal for `/new`. Mirrors
/// [`open_fork_question`] but uses [`LocalQuestionKind::NewSession`] so
/// the answer routes to [`dispatch_new_session_inner`] or
/// [`dispatch_new_worktree_session`].
pub(in crate::app::dispatch) fn open_new_session_question(app: &mut AppView) -> Vec<Effect> {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    if agent.question_view.is_some() {
        app.show_toast("Finish answering the current question first");
        return vec![];
    }
    let mut options = vec![
        QuestionOption {
            label: "Yes".into(),
            description: "New session in a new isolated git worktree".into(),
            preview: None,
            id: None,
        },
        QuestionOption {
            label: "No".into(),
            description: "New session in the current cwd".into(),
            preview: None,
            id: None,
        },
    ];
    options.extend(worktree_persist_options());
    let question = Question {
        question: "Start the new session in an isolated git worktree?".into(),
        id: None,
        options,
        multi_select: Some(false),
    };
    let agent = app.agents.get_mut(&id).expect("agent present (re-borrow)");
    let stashed = agent.prompt.stash();
    let state = QuestionViewState::new(
        format!("new-session-{}", uuid::Uuid::new_v4()),
        vec![question],
        stashed,
    )
    .with_local_kind(LocalQuestionKind::NewSession)
    .with_no_freeform();
    agent.question_view = Some(state);
    agent.prompt.set_text("");
    vec![]
}
/// Open a local QuestionView asking whether to start a new session for a
/// model that requires a different agent harness. Mirrors
/// [`open_new_session_question`] but uses
/// [`LocalQuestionKind::AgentTypeMismatch`] so the answer routes to
/// [`dispatch_new_session_inner`] with a deferred model switch.
pub(in crate::app::dispatch) fn open_agent_type_mismatch_question(
    app: &mut AppView,
    model_id: acp::ModelId,
    effort: Option<echo_agent_runtime::sampling::types::ReasoningEffort>,
    model_name: &str,
) -> Vec<Effect> {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    if agent.question_view.is_some() {
        app.show_toast("Finish answering the current question first");
        return vec![];
    }
    let question = Question {
        question: format!("Switching to {model_name} requires starting a new session. Continue?"),
        id: None,
        options: vec![
            QuestionOption {
                label: "Yes".into(),
                description: format!("Start a new session with {model_name}"),
                preview: None,
                id: None,
            },
            QuestionOption {
                label: "No".into(),
                description: "Continue the current session".into(),
                preview: None,
                id: None,
            },
        ],
        multi_select: Some(false),
    };
    let agent = app.agents.get_mut(&id).expect("agent present (re-borrow)");
    let stashed = agent.prompt.stash();
    let state = QuestionViewState::new(
        format!("agent-type-mismatch-{}", uuid::Uuid::new_v4()),
        vec![question],
        stashed,
    )
    .with_local_kind(LocalQuestionKind::AgentTypeMismatch { model_id, effort })
    .with_no_freeform();
    agent.question_view = Some(state);
    agent.prompt.set_text("");
    vec![]
}
/// Core new-session logic: create a placeholder agent, push the
/// `/dashboard` tip, and return the `CreateSession` effect.
///
/// Apply welcome workspace selection before creating a session from Welcome.
///
/// Returns `Err(effects)` when session creation should abort (ACK pending or
/// empty effects). `Ok(())` means continue into the normal new-session path.
#[cfg(feature = "local-workspace")]
fn apply_welcome_workspace_on_new_session(app: &mut AppView) -> Result<(), Vec<Effect>> {
    use crate::views::welcome::workspace_mode::{
        WelcomeWorkspaceMode, WelcomeWorkspacePrepare, prepare_welcome_workspace_for_new_session,
    };
    match prepare_welcome_workspace_for_new_session(
        app.welcome_workspace_mode,
        app.local_workspace_startup_locked,
        app.chat_mode,
        &app.cwd,
        false,
    ) {
        Ok(WelcomeWorkspacePrepare::Continue {
            session_override,
            warning,
        }) => {
            if let Some(msg) = warning {
                tracing::warn!("{msg}");
                app.show_toast(&msg);
            }
            if let Some(override_cfg) = session_override {
                app.welcome_session_local_workspace = Some(override_cfg);
            }
            Ok(())
        }
        Ok(WelcomeWorkspacePrepare::AwaitAck) => {
            app.welcome_local_workspace_ack_pending = true;
            app.session_picker_entries = None;
            app.session_picker_loading = false;
            app.session_picker_list_seq = app.session_picker_list_seq.saturating_add(1);
            Err(vec![])
        }
        Err(err) => {
            tracing::warn!("welcome workspace mode: {err}");
            app.show_toast(&format!(
                "Local workspace unavailable ({err}); using sandbox"
            ));
            app.welcome_session_local_workspace = Some(None);
            app.welcome_workspace_mode = WelcomeWorkspaceMode::Sandbox;
            Ok(())
        }
    }
}
/// Factored out of [`dispatch_new_session`] so the worktree-question
/// "No" path can call it directly without re-opening the modal.
pub(in crate::app::dispatch) fn dispatch_new_session_inner(
    app: &mut AppView,
    model_id: Option<acp::ModelId>,
) -> Vec<Effect> {
    let (_id, effects) = dispatch_new_session_inner_with_id(app, model_id);
    effects
}
/// Sibling that returns the new `AgentId` alongside
/// the effects. Used by `dispatch_dashboard_dispatch` so it doesn't
/// rely on the (correct-but-brittle) `app.agents.last()` lookup.
pub(in crate::app::dispatch) fn dispatch_new_session_inner_with_id(
    app: &mut AppView,
    model_id: Option<acp::ModelId>,
) -> (AgentId, Vec<Effect>) {
    let (previous_session_id, effective_cwd, inherit_worktree) = match get_active_agent(app) {
        Some(a) => (
            a.session.session_id.clone(),
            a.session.cwd.clone(),
            a.session.is_worktree,
        ),
        None => (None, app.cwd.clone(), false),
    };
    let mut effects = unregister_session_effect(previous_session_id);
    reseed_tip_for_new_session(app);
    let agent_id = AgentId(app.next_agent_id);
    app.next_agent_id += 1;
    let mut scrollback = ScrollbackState::new();
    scrollback.set_appearance(app.appearance.clone());
    let agent = AgentView::new(
        AgentSession {
            id: agent_id,
            acp_tx: app.acp_tx.clone(),
            session_id: None,
            models: app.models.clone(),
            state: AgentState::Idle,
            tracker: AcpUpdateTracker::new(),
            cwd: effective_cwd.clone(),
            is_worktree: inherit_worktree,
            forked_from: None,
            pending_prompts: std::collections::VecDeque::new(),
            next_queue_id: 0,
            yolo_mode: app.default_yolo,
            auto_mode: inherit_auto_mode(app),
            prompt_history: Vec::new(),
            prompt_history_loading: false,
            loading_replay: false,
            restore_degree: None,
            rate_limited: false,
            model_incompatible: false,
            credit_limit_blocked: false,
            free_usage_blocked: false,
            available_commands: app.bootstrap_acp_commands.clone(),
            available_commands_generation: 1,
            available_tools: None,
            model_switch_pending: false,
            user_model_preference: None,
            deferred_model_switch: app.deferred_model_switch_from_cli(),
            bg_tasks: std::collections::BTreeMap::new(),
            bg_tool_call_to_task: std::collections::HashMap::new(),
            scheduled_tasks: std::collections::HashMap::new(),
            in_flight_prompt: None,
            compact_held_prompt: None,
            current_prompt_id: None,
            created_via_new: true,
        },
        scrollback,
    );
    app.agents.insert(agent_id, agent);
    {
        let agent = app.agents.get_mut(&agent_id).unwrap();
        agent.prompt.set_compact(app.appearance.prompt.compact);
        agent.prompt.adopt_slash_mru(app.slash_mru.clone());
        agent.prompt.adopt_command_tags(app.command_tags.clone());
        agent
            .prompt
            .set_contextual_hints(app.contextual_hints.undo, app.contextual_hints.plan_mode);
        agent.set_session_recap_available(app.session_recap_available);
        agent.set_voice_mode_available(app.voice_mode_enabled);
        agent.apply_app_scoped_gates(
            app.sharing_enabled,
            app.usage_visible,
            !app.has_external_auth_provider,
            app.chat_mode,
            app.screen_mode,
            &app.active_announcements,
            &app.tier_restricted_commands,
        );
        agent.apply_credit_balance(app.credit_balance.clone(), app.auto_topup.clone());
        agent
            .prompt
            .slash_controller
            .registry_mut()
            .set_plugins_visible(!app.appearance.disable_plugins);
        agent.active_pane = ActivePane::Prompt;
    }
    switch_to_agent(app, agent_id, SwitchCause::New);
    if app.screen_mode.is_minimal() {
        app.minimal_state.welcome_pending = true;
    }
    let chat_kind = consume_chat_kind(app);
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        agent.chat_kind = chat_kind;
        agent.conversation_entry = chat_kind;
        #[cfg(feature = "local-workspace")]
        {
            let local_intent = match &app.welcome_session_local_workspace {
                Some(Some(_)) => true,
                Some(None) => false,
                None => crate::app::session_startup::active_local_workspace()
                    .ok()
                    .flatten()
                    .is_some(),
            };
            let (mode, locked) =
                crate::views::welcome::workspace_mode::indicator_for_opening_session(
                    agent.chat_kind,
                    false,
                    app.local_workspace_startup_locked,
                    local_intent,
                );
            agent.workspace_mode = mode;
            agent.workspace_mode_cli_locked = locked;
        }
        agent.apply_credit_balance(app.credit_balance.clone(), app.auto_topup.clone());
        agent.mcp_init_progress = Some(McpInitProgress {
            total: 0,
            connected: 0,
            started_at: Instant::now(),
        });
        agent.session.prompt_history_loading = true;
    }
    let preferred_session_id = app.deferred_startup.preferred_session_id.take();
    effects.push(Effect::CreateSession {
        agent_id,
        cwd: effective_cwd,
        model_id,
        permission_mode_override: None,
        preferred_session_id,
        chat_kind,
    });
    (agent_id, effects)
}
/// Exit the current session and return to the welcome screen.
pub(in crate::app::dispatch) fn dispatch_exit_session(app: &mut AppView) -> Vec<Effect> {
    let effects =
        unregister_session_effect(get_active_agent(app).and_then(|a| a.session.session_id.clone()));
    show_welcome(app);
    app.welcome_prompt_focused = true;
    app.session_picker_entries = None;
    app.session_picker_loading = false;
    app.session_picker_state.selected = 0;
    app.session_picker_content_results = None;
    app.session_picker_content_loading = false;
    app.exit_session_pending = None;
    effects
}
/// Aftermath for `/delete` on the active agent: dashboard overlay returns
/// there; standalone agent sessions go home.
fn after_delete_current_session(
    app: &AppView,
    id: AgentId,
) -> crate::app::actions::AfterSessionDelete {
    use crate::app::actions::AfterSessionDelete;
    if app
        .dashboard
        .as_ref()
        .is_some_and(|d| d.attached_agent == Some(id))
    {
        AfterSessionDelete::Dashboard
    } else {
        AfterSessionDelete::Welcome
    }
}
/// Confirm deleting the parent session (not a subagent view).
pub(in crate::app::dispatch) fn open_delete_current_session_question(
    app: &mut AppView,
) -> Vec<Effect> {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let delete_description = if after_delete_current_session(app, id)
        == crate::app::actions::AfterSessionDelete::Dashboard
    {
        "Remove history and return to the dashboard"
    } else {
        "Remove history and return home"
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    if agent.session.session_id.is_none() {
        app.show_toast("No active session to delete");
        return vec![];
    }
    if agent.question_view.is_some() {
        app.show_toast("Finish answering the current question first");
        return vec![];
    }
    let question = Question {
        question: "Delete this session permanently?".into(),
        id: None,
        options: vec![
            QuestionOption {
                label: "Delete".into(),
                description: delete_description.into(),
                preview: None,
                id: None,
            },
            QuestionOption {
                label: "Cancel".into(),
                description: "Keep the session".into(),
                preview: None,
                id: None,
            },
        ],
        multi_select: Some(false),
    };
    let stashed = agent.prompt.stash();
    agent.question_view = Some(
        QuestionViewState::new(
            format!("delete-session-{}", uuid::Uuid::new_v4()),
            vec![question],
            stashed,
        )
        .with_local_kind(LocalQuestionKind::DeleteCurrentSession)
        .with_no_freeform(),
    );
    agent.prompt.set_text("");
    vec![]
}
pub(in crate::app::dispatch) fn dispatch_delete_current_session_answered(
    app: &mut AppView,
    confirmed: bool,
) -> Vec<Effect> {
    if !confirmed {
        return vec![];
    }
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some((session_id, cwd, running_bg_tasks)) = app.agents.get(&id).and_then(|agent| {
        let session_id = agent.session.session_id.clone()?;
        let cwd = agent.session.cwd.display().to_string();
        let running_bg_tasks: Vec<String> = agent
            .session
            .bg_tasks
            .values()
            .filter(|t| t.status == crate::app::agent::BgTaskStatus::Running)
            .map(|t| t.task_id.clone())
            .collect();
        Some((session_id, cwd, running_bg_tasks))
    }) else {
        app.show_toast("No active session to delete");
        return vec![];
    };
    let after = after_delete_current_session(app, id);
    let mut effects = vec![Effect::CancelTurn {
        session_id: session_id.clone(),
        cancel_subagents: true,
        trigger: None,
        rewind_prompt_id: None,
    }];
    effects.extend(
        running_bg_tasks
            .into_iter()
            .map(|task_id| Effect::KillBgTask {
                session_id: session_id.clone(),
                task_id,
                source: echo_agent_runtime::extensions::task::TaskKillSource::Teardown,
            }),
    );
    app.show_toast("Deleting session\u{2026}");
    effects.push(Effect::DeleteSession {
        source: "current".into(),
        session_id: session_id.to_string(),
        cwd,
        after,
    });
    effects
}
/// Handle the user accepting the folder-trust question: persist the grant for
/// the workspace (writes `~/.echo-agent/trusted_folders.toml`), mark trust resolved,
/// then replay any deferred session startup (only if auth is also done).
pub(in crate::app::dispatch) fn dispatch_trust_folder(app: &mut AppView) -> Vec<Effect> {
    if let TrustState::Pending { workspace } = &app.trust_state {
        echo_agent_workspace::folder_trust::grant_folder_trust(workspace);
    }
    finish_trust(app)
}
/// Tail of accepting the folder-trust question (via [`dispatch_trust_folder`];
/// declining quits instead): resolve `trust_state` to `Done`, focus the welcome
/// prompt, and replay the deferred session startup once auth is also resolved.
/// Drains iff [`AppView::session_startup_allowed`] -- the same predicate
/// `AuthComplete` uses, so whichever gate resolves last drains exactly once.
pub(in crate::app::dispatch) fn finish_trust(app: &mut AppView) -> Vec<Effect> {
    app.trust_state = TrustState::Done;
    app.welcome_prompt_focused = !app.is_access_blocked();
    if app.session_startup_allowed() {
        drain_startup_actions(app)
    } else {
        vec![]
    }
}
/// Resolves `consent_state` before the marker write, so a failed write cannot trap the user.
pub(in crate::app::dispatch) fn dispatch_accept_consent(app: &mut AppView) -> Vec<Effect> {
    let ConsentState::Pending {
        notice, legibility, ..
    } = &app.consent_state
    else {
        return vec![];
    };
    if !legibility.can_accept() {
        return vec![];
    }
    let notice_id = notice.id.clone();
    let version = notice.version;
    app.consent_answered = Some((notice_id.clone(), version));
    let mut effects = Vec::new();
    if let Some(account) = app.account_email.clone() {
        effects.push(Effect::PersistConsentAnswer {
            account: Some(account),
            notice_id: notice_id.clone(),
            version,
            acked: false,
        });
    }
    effects.push(Effect::RecordConsentUpstream { notice_id, version });
    effects.extend(finish_consent(app));
    effects
}
fn finish_consent(app: &mut AppView) -> Vec<Effect> {
    app.consent_state = ConsentState::Done;
    app.welcome_prompt_focused = !app.is_access_blocked();
    if app.session_startup_allowed() {
        drain_startup_actions(app)
    } else {
        vec![]
    }
}
/// Clear EVERY deferred `startup_*` action without replaying any. Used on the
/// paths that must not run startup at all — ZDR-blocked login, and mid-session
/// re-auth (`auth_return_view`) where a session already exists — so a stash
/// (including an incidental mid-session `Ctrl+N` that the chokepoint deferred)
/// can never linger and fire later. Atomic counterpart to `drain_startup_actions`.
pub(in crate::app::dispatch) fn clear_startup_actions(app: &mut AppView) {
    let _ = app.deferred_startup.take();
}
/// Replay the session-startup actions deferred until auth + trust both resolved
/// (`--resume` / `--worktree` / initial-prompt / `echoagent dashboard`). Extracted
/// from the `AuthComplete` handler so the folder-trust answer can run the SAME
/// machinery; whichever gate resolves last drains it (each call site guards on
/// the other gate being `Done`, so it runs exactly once).
pub(in crate::app::dispatch) fn drain_startup_actions(app: &mut AppView) -> Vec<Effect> {
    use crate::app::session_startup::{DeferredSessionStartup, DeferredStartupActions};
    debug_assert!(
        app.session_startup_allowed(),
        "drain_startup_actions must run only with the startup gate open"
    );
    let DeferredStartupActions {
        session: deferred,
        preferred_session_id: preferred_id,
        worktree,
        worktree_label,
        worktree_ref,
        new_session,
        prompt,
        open_dashboard,
        pending_chat,
        #[cfg(feature = "local-workspace")]
        history_load_as_build,
    } = app.deferred_startup.take();
    let mut effects = Vec::new();
    match deferred {
        Some(DeferredSessionStartup::Fork {
            parent_session_id,
            parent_cwd,
            new_session_id,
        }) => {
            effects.extend(dispatch_startup_fork_session(
                app,
                parent_session_id,
                parent_cwd,
                new_session_id,
            ));
        }
        Some(DeferredSessionStartup::Load {
            session_id,
            session_cwd,
            chat_kind,
        }) => {
            #[cfg(feature = "local-workspace")]
            {
                app.welcome_history_load_as_build = history_load_as_build;
            }
            if worktree {
                if chat_kind || pending_chat {
                    app.deferred_startup.pending_chat = true;
                }
                effects.extend(dispatch_new_worktree_session(
                    app,
                    Some(session_id),
                    worktree_label,
                    None,
                    None,
                    worktree_ref,
                    None,
                ));
            } else {
                effects.extend(dispatch_load_session(
                    app,
                    session_id,
                    session_cwd,
                    chat_kind,
                ));
            }
        }
        Some(DeferredSessionStartup::NewWithId { session_id }) => {
            if pending_chat {
                app.deferred_startup.pending_chat = true;
            }
            if worktree {
                effects.extend(dispatch_new_worktree_session(
                    app,
                    None,
                    worktree_label,
                    None,
                    None,
                    worktree_ref,
                    Some(session_id),
                ));
            } else {
                effects.extend(dispatch_new_session_with_id(app, session_id));
            }
        }
        Some(DeferredSessionStartup::ForeignResume { tool, native_id }) => {
            effects.extend(dispatch_new_session_inner(app, None));
            effects.extend(dispatch(
                Action::SendPrompt(
                    crate::app::foreign_sessions::ForeignPickerSource::from_tool(tool)
                        .resume_prompt(&native_id),
                ),
                app,
            ));
        }
        None => {
            if pending_chat {
                app.deferred_startup.pending_chat = true;
            }
            if let Some(sid) = preferred_id {
                if worktree {
                    effects.extend(dispatch_new_worktree_session(
                        app,
                        None,
                        worktree_label,
                        None,
                        None,
                        worktree_ref,
                        Some(sid),
                    ));
                } else {
                    effects.extend(dispatch_new_session_with_id(app, sid));
                }
            } else if worktree {
                effects.extend(dispatch_new_worktree_session(
                    app,
                    None,
                    worktree_label,
                    None,
                    None,
                    worktree_ref,
                    None,
                ));
            } else if new_session {
                effects.extend(dispatch_new_session(app));
            } else {
                app.deferred_startup.pending_chat = false;
            }
        }
    }
    if let Some(prompt) = prompt {
        effects.extend(dispatch_initial_prompt(app, prompt));
    }
    if open_dashboard {
        effects.extend(dispatch(Action::OpenDashboard, app));
    }
    effects
}
/// Create a placeholder agent in a new git worktree, then create a session.
pub(in crate::app::dispatch) fn dispatch_new_worktree_session(
    app: &mut AppView,
    load_session_id: Option<String>,
    label: Option<String>,
    prompt: Option<String>,
    model_id: Option<acp::ModelId>,
    git_ref: Option<String>,
    preferred_session_id: Option<String>,
) -> Vec<Effect> {
    #[cfg(feature = "local-workspace")]
    if load_session_id.is_none()
        && matches!(app.active_view, crate::app::app_view::ActiveView::Welcome)
    {
        let skip_apply = app.welcome_session_local_workspace.is_some();
        if !skip_apply && let Err(effects) = apply_welcome_workspace_on_new_session(app) {
            app.deferred_startup.worktree = true;
            if let Some(ref label) = label {
                app.deferred_startup.worktree_label = Some(label.clone());
            }
            if let Some(ref git_ref) = git_ref {
                app.deferred_startup.worktree_ref = Some(git_ref.clone());
            }
            if let Some(sid) = load_session_id.clone() {
                app.deferred_startup.session =
                    Some(crate::app::session_startup::DeferredSessionStartup::Load {
                        session_id: sid,
                        session_cwd: None,
                        chat_kind: app.deferred_startup.pending_chat,
                    });
            }
            if let Some(id) = preferred_session_id.clone() {
                app.deferred_startup.preferred_session_id = Some(id);
            }
            return effects;
        }
    }
    let preferred_session_id =
        preferred_session_id.or_else(|| app.deferred_startup.preferred_session_id.take());
    if !app.session_startup_allowed() {
        debug_assert!(
            prompt.is_none() && model_id.is_none(),
            "worktree prompt/model_id only flow from an active session (gate open)"
        );
        if let Some(sid) = load_session_id {
            app.deferred_startup.session =
                Some(crate::app::session_startup::DeferredSessionStartup::Load {
                    session_id: sid,
                    session_cwd: None,
                    chat_kind: app.deferred_startup.pending_chat,
                });
        }
        app.deferred_startup.worktree = true;
        if label.is_some() {
            app.deferred_startup.worktree_label = label;
        }
        if git_ref.is_some() {
            app.deferred_startup.worktree_ref = git_ref;
        }
        if preferred_session_id.is_some() {
            app.deferred_startup.preferred_session_id = preferred_session_id;
        }
        return vec![];
    }
    if !app.cwd_has_git_ancestor {
        let msg: String = "Not inside a git repository. Navigate to a git repo \
                      or run 'git init' first."
            .into();
        if !app.startup_warnings.iter().any(|w| w.message == msg) {
            app.startup_warnings.push(crate::startup::StartupWarning {
                severity: crate::startup::WarningSeverity::Warning,
                message: msg,
                action: None,
            });
        }
        #[cfg(feature = "local-workspace")]
        {
            app.welcome_session_local_workspace = None;
            app.welcome_history_load_as_build = false;
        }
        return vec![];
    }
    if load_session_id.is_none() {
        reseed_tip_for_new_session(app);
    }
    let agent_id = AgentId(app.next_agent_id);
    app.next_agent_id += 1;
    let mut scrollback = ScrollbackState::new();
    scrollback.set_appearance(app.appearance.clone());
    let mut agent = AgentView::new(
        AgentSession {
            id: agent_id,
            acp_tx: app.acp_tx.clone(),
            session_id: None,
            models: app.models.clone(),
            state: AgentState::Idle,
            tracker: AcpUpdateTracker::new(),
            cwd: app.cwd.clone(),
            is_worktree: false,
            forked_from: None,
            pending_prompts: std::collections::VecDeque::new(),
            next_queue_id: 0,
            yolo_mode: app.default_yolo,
            auto_mode: inherit_auto_mode(app),
            prompt_history: Vec::new(),
            prompt_history_loading: false,
            loading_replay: false,
            restore_degree: None,
            rate_limited: false,
            model_incompatible: false,
            credit_limit_blocked: false,
            free_usage_blocked: false,
            available_commands: app.bootstrap_acp_commands.clone(),
            available_commands_generation: 1,
            available_tools: None,
            model_switch_pending: false,
            user_model_preference: None,
            deferred_model_switch: app.deferred_model_switch_from_cli(),
            bg_tasks: std::collections::BTreeMap::new(),
            bg_tool_call_to_task: std::collections::HashMap::new(),
            scheduled_tasks: std::collections::HashMap::new(),
            in_flight_prompt: None,
            compact_held_prompt: None,
            current_prompt_id: None,
            created_via_new: false,
        },
        scrollback,
    );
    let cmd = if load_session_id.is_some() {
        AgentCommand::RestoreWorktree
    } else {
        AgentCommand::CreateWorktree
    };
    agent.session.start_command(cmd);
    agent.turn_started_at = Some(Instant::now());
    app.agents.insert(agent_id, agent);
    let chat_kind = if load_session_id.is_none() {
        consume_chat_kind(app)
    } else {
        app.deferred_startup.pending_chat
    };
    {
        let agent = app.agents.get_mut(&agent_id).unwrap();
        agent.prompt.set_compact(app.appearance.prompt.compact);
        agent.prompt.adopt_slash_mru(app.slash_mru.clone());
        agent.prompt.adopt_command_tags(app.command_tags.clone());
        agent
            .prompt
            .set_contextual_hints(app.contextual_hints.undo, app.contextual_hints.plan_mode);
        agent.set_session_recap_available(app.session_recap_available);
        agent.set_voice_mode_available(app.voice_mode_enabled);
        agent.apply_app_scoped_gates(
            app.sharing_enabled,
            app.usage_visible,
            !app.has_external_auth_provider,
            app.chat_mode,
            app.screen_mode,
            &app.active_announcements,
            &app.tier_restricted_commands,
        );
        agent.chat_kind = chat_kind;
        agent.conversation_entry = chat_kind;
        #[cfg(feature = "local-workspace")]
        {
            let local_intent = match &app.welcome_session_local_workspace {
                Some(Some(_)) => true,
                Some(None) => false,
                None => crate::app::session_startup::active_local_workspace()
                    .ok()
                    .flatten()
                    .is_some(),
            };
            let (mode, locked) =
                crate::views::welcome::workspace_mode::indicator_for_opening_session(
                    agent.chat_kind,
                    app.welcome_history_load_as_build,
                    app.local_workspace_startup_locked,
                    local_intent,
                );
            agent.workspace_mode = mode;
            agent.workspace_mode_cli_locked = locked;
        }
        agent.apply_credit_balance(app.credit_balance.clone(), app.auto_topup.clone());
        agent
            .prompt
            .slash_controller
            .registry_mut()
            .set_plugins_visible(!app.appearance.disable_plugins);
    }
    if let Some(prompt) = prompt
        && let Some(agent) = app.agents.get_mut(&agent_id)
    {
        agent.session.enqueue_prompt(prompt);
    }
    switch_to_agent(app, agent_id, SwitchCause::New);
    let effects = vec![Effect::CreateWorktreeSession {
        agent_id,
        load_session_id,
        label,
        git_ref,
        model_id,
        permission_mode_override: None,
        preferred_session_id,
        chat_kind,
    }];
    effects
}
pub(in crate::app::dispatch) fn dispatch_new_session_with_id(
    app: &mut AppView,
    session_id: String,
) -> Vec<Effect> {
    app.deferred_startup.preferred_session_id = Some(session_id.clone());
    if !app.session_startup_allowed() {
        app.deferred_startup.session =
            Some(crate::app::session_startup::DeferredSessionStartup::NewWithId { session_id });
        return vec![];
    }
    let (_agent_id, effects) = dispatch_new_session_inner_with_id(app, None);
    effects
}
/// Tear down a placeholder agent that must not proceed under sticky `--chat`
/// (local Build refuse). Never leave a half-loaded slot with a bound session id.
pub(in crate::app::dispatch) fn refuse_chat_mode_build_agent(app: &mut AppView, agent_id: AgentId) {
    app.show_toast(crate::app::session_startup::CHAT_MODE_LOCAL_BUILD_REFUSAL);
    let fallback = app.agents.keys().copied().find(|id| *id != agent_id);
    remove_agent_and_cleanup(app, agent_id);
    if let Some(target) = fallback {
        switch_to_agent(app, target, SwitchCause::Picker);
    } else {
        show_welcome(app);
        app.welcome_prompt_focused = true;
        app.session_picker_entries = None;
        app.session_picker_loading = false;
        app.session_picker_state.selected = 0;
        app.session_picker_content_results = None;
        app.session_picker_content_loading = false;
        let msg = crate::app::session_startup::CHAT_MODE_LOCAL_BUILD_REFUSAL.to_string();
        if !app.startup_warnings.iter().any(|w| w.message == msg) {
            app.startup_warnings.push(crate::startup::StartupWarning {
                severity: crate::startup::WarningSeverity::Warning,
                message: msg,
                action: None,
            });
        }
    }
}
pub(in crate::app::dispatch) fn handle_session_created(
    app: &mut AppView,
    agent_id: AgentId,
    session_id: acp::SessionId,
    new_models: Option<acp::SessionModelState>,
    scheduler_background_loops: Option<bool>,
) -> Vec<Effect> {
    let agent_count = app.agents.len();
    let switch_hint =
        crate::views::dashboard::session_switch_hint_command(app.screen_mode.is_minimal());
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        let session_id_clone = session_id.clone();
        if agent.session.created_via_new
            && agent_count > 1
            && let Some(cmd) = switch_hint
        {
            agent.scrollback.push_block(RenderBlock::system(format!(
                "Session {}, use {cmd} to switch between sessions",
                session_id_clone.0,
            )));
        } else if agent_count > 1 {
            agent.scrollback.push_block(RenderBlock::system(format!(
                "Session: {}",
                session_id_clone.0,
            )));
        }
        agent.bind_session_id(session_id);
        agent.scheduler_background_loops = scheduler_background_loops;
        if let Some(m) = new_models {
            app.models = Some(m).into();
            agent.session.models = app.models.clone();
        }
        let deferred = apply_deferred_model_switch(agent, app.cli_effort_token.as_deref());
        let deferred_mode = agent.deferred_session_mode.take();
        let cwd = agent.session.cwd.clone();
        if deferred.is_some() {
            agent.session.model_switch_pending = true;
        }
        let mut drain = if app.reconnect_pending {
            QueueDrain {
                effects: vec![],
                page_flip_entry: None,
            }
        } else {
            maybe_drain_queue(agent)
        };
        let mut effects = std::mem::take(&mut drain.effects);
        agent.session.prompt_history_loading = true;
        effects.push(Effect::FetchPromptHistory {
            agent_id,
            cwd: cwd.clone(),
            session_id: session_id_clone.to_string(),
        });
        effects.push(Effect::FetchSessionAgentName {
            agent_id,
            session_id: session_id_clone.clone(),
        });
        effects.push(Effect::RefreshAvailableCommands {
            agent_id,
            session_id: session_id_clone.clone(),
        });
        effects.push(Effect::CheckMarketplaceUpdates {
            agent_id,
            session_id: session_id_clone.clone(),
        });
        if app.plugin_cta_enabled {
            effects.push(Effect::FetchPluginCtaCatalog {
                agent_id,
                session_id: session_id_clone.clone(),
            });
        }
        effects.push(Effect::FetchBilling {
            agent_id,
            silent: true,
            nonce: Default::default(),
        });
        if let Some(switch) = deferred {
            effects.push(Effect::SwitchModel {
                agent_id,
                session_id: session_id_clone.clone(),
                model_id: switch.model_id,
                effort: switch.effort,
                prev_model_id: switch.prev_model_id,
            });
        }
        if let Some(mode) = deferred_mode {
            effects.push(Effect::SetSessionMode {
                session_id: session_id_clone.clone(),
                mode_id: acp::SessionModeId::new(mode.as_id()),
            });
        }
        if std::mem::take(&mut agent.pending_extensions_fetch)
            && let Some(modal) = agent.extensions_modal.as_mut()
        {
            effects.extend(extensions_modal_tab_fetches(
                modal,
                agent_id,
                session_id_clone.clone(),
            ));
        }
        effects.push(Effect::RegisterActiveSession {
            session_id: session_id_clone,
            cwd: agent.session.cwd.display().to_string(),
        });
        notify_session_ready(&app.notification_service, agent);
        note_peek_page_flip(app, agent_id, drain.page_flip_entry);
        return effects;
    }
    vec![]
}
pub(in crate::app::dispatch) fn handle_worktree_session_created(
    app: &mut AppView,
    agent_id: AgentId,
    session_id: acp::SessionId,
    worktree_path: std::path::PathBuf,
    session_cwd: std::path::PathBuf,
    new_models: Option<acp::SessionModelState>,
    scheduler_background_loops: Option<bool>,
) -> Vec<Effect> {
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        agent.session.finish_command();
        agent.mark_turn_finished(TurnEnd::Aborted);
        let session_id_clone = session_id.clone();
        agent.bind_session_id(session_id);
        agent.scheduler_background_loops = scheduler_background_loops;
        agent.session.cwd = session_cwd.clone();
        agent.session.is_worktree = true;
        agent.current_branch = None;
        agent.main_repo = None;
        agent.is_worktree = true;
        crate::git_info::populate_from_cwd_async(session_cwd.clone());
        if let Some(m) = new_models {
            app.models = Some(m).into();
            agent.session.models = app.models.clone();
        }
        agent.prompt.file_search.retarget(&session_cwd);
        agent.scrollback.push_block(RenderBlock::system(format!(
            "Worktree ready: {}",
            worktree_path.display()
        )));
        let deferred = apply_deferred_model_switch(agent, app.cli_effort_token.as_deref());
        let deferred_mode = agent.deferred_session_mode.take();
        let cwd = agent.session.cwd.clone();
        if deferred.is_some() {
            agent.session.model_switch_pending = true;
        }
        let mut drain = if app.reconnect_pending {
            QueueDrain {
                effects: vec![],
                page_flip_entry: None,
            }
        } else {
            maybe_drain_queue(agent)
        };
        let mut effects = std::mem::take(&mut drain.effects);
        agent.session.prompt_history_loading = true;
        effects.push(Effect::FetchPromptHistory {
            agent_id,
            cwd: cwd.clone(),
            session_id: session_id_clone.to_string(),
        });
        effects.push(Effect::FetchSessionAgentName {
            agent_id,
            session_id: session_id_clone.clone(),
        });
        effects.push(Effect::RefreshAvailableCommands {
            agent_id,
            session_id: session_id_clone.clone(),
        });
        effects.push(Effect::CheckMarketplaceUpdates {
            agent_id,
            session_id: session_id_clone.clone(),
        });
        if app.plugin_cta_enabled {
            effects.push(Effect::FetchPluginCtaCatalog {
                agent_id,
                session_id: session_id_clone.clone(),
            });
        }
        effects.push(Effect::FetchBilling {
            agent_id,
            silent: true,
            nonce: Default::default(),
        });
        if let Some(switch) = deferred {
            effects.push(Effect::SwitchModel {
                agent_id,
                session_id: session_id_clone.clone(),
                model_id: switch.model_id,
                effort: switch.effort,
                prev_model_id: switch.prev_model_id,
            });
        }
        if let Some(mode) = deferred_mode {
            effects.push(Effect::SetSessionMode {
                session_id: session_id_clone.clone(),
                mode_id: acp::SessionModeId::new(mode.as_id()),
            });
        }
        if std::mem::take(&mut agent.pending_extensions_fetch)
            && let Some(modal) = agent.extensions_modal.as_mut()
        {
            effects.extend(extensions_modal_tab_fetches(
                modal,
                agent_id,
                session_id_clone.clone(),
            ));
        }
        effects.push(Effect::RegisterActiveSession {
            session_id: session_id_clone,
            cwd: agent.session.cwd.display().to_string(),
        });
        notify_session_ready(&app.notification_service, agent);
        note_peek_page_flip(app, agent_id, drain.page_flip_entry);
        return effects;
    }
    vec![]
}
/// Surface a session-creation failure on the welcome screen (no toast sink).
fn push_session_create_failure_warning(app: &mut AppView, msg: &str) {
    if !app.startup_warnings.iter().any(|w| w.message == msg) {
        app.startup_warnings.push(crate::startup::StartupWarning {
            severity: crate::startup::WarningSeverity::Warning,
            message: msg.to_string(),
            action: None,
        });
    }
}
/// After an orphan create fails, New/Fork may already have moved overlay
/// attach onto the removed placeholder. Re-point to the survivor so
/// Left/Esc still exit to the dashboard; clear when recovery is Welcome.
fn restore_dashboard_attach_after_orphan_remove(
    app: &mut AppView,
    removed: AgentId,
    survivor: Option<AgentId>,
) {
    let Some(d) = app.dashboard.as_mut() else {
        return;
    };
    if d.attached_agent != Some(removed) {
        return;
    }
    match survivor {
        Some(target) => d.repoint_attach_if_on(removed, target),
        None => d.close_popup(),
    }
}
/// Failed plain `CreateSession`: drop orphan placeholders, clear the
/// starting-session spinner, and surface the error (toast when an agent
/// remains; startup warning on the welcome screen, which has no toast).
pub(in crate::app::dispatch) fn handle_session_failed(
    app: &mut AppView,
    agent_id: AgentId,
    error: String,
) -> Vec<Effect> {
    tracing::error!(agent = ?agent_id, error = %error, "Session creation failed");
    let msg = format!("Session creation failed: {error}");
    let is_orphan = app
        .agents
        .get(&agent_id)
        .is_some_and(|a| a.session.session_id.is_none() && a.session.forked_from.is_none());
    if is_orphan {
        let failed_was_active = matches!(app.active_view, ActiveView::Agent(id) if id == agent_id);
        let fallback = app.agents.keys().copied().find(|id| *id != agent_id);
        remove_agent_and_cleanup(app, agent_id);
        if let Some(target) = fallback {
            if failed_was_active {
                switch_to_agent(app, target, SwitchCause::Picker);
            }
            restore_dashboard_attach_after_orphan_remove(app, agent_id, Some(target));
            if matches!(app.active_view, ActiveView::Welcome) {
                push_session_create_failure_warning(app, &msg);
            } else {
                app.show_toast(&msg);
            }
        } else {
            show_welcome(app);
            app.welcome_prompt_focused = true;
            app.session_picker_entries = None;
            app.session_picker_loading = false;
            app.session_picker_state.selected = 0;
            app.session_picker_content_results = None;
            app.session_picker_content_loading = false;
            restore_dashboard_attach_after_orphan_remove(app, agent_id, None);
            push_session_create_failure_warning(app, &msg);
        }
    } else if let Some(agent) = app.agents.get_mut(&agent_id) {
        agent.pending_extensions_fetch = false;
        agent.session.prompt_history_loading = false;
        agent.mcp_init_progress = None;
        agent.session.finish_command();
        let elapsed = agent.turn_elapsed();
        agent.mark_turn_finished(TurnEnd::Aborted);
        agent.pending_first_prompt = None;
        agent.pending_fork_banner = None;
        agent.show_toast(&msg);
        agent
            .scrollback
            .push_block(RenderBlock::session_event(SessionEvent::TurnFailed {
                error,
                elapsed,
            }));
    }
    vec![]
}
pub(in crate::app::dispatch) fn handle_worktree_session_failed(
    app: &mut AppView,
    agent_id: AgentId,
    error: String,
) -> Vec<Effect> {
    tracing::error!(agent = ?agent_id, error = %error, "Worktree session creation failed");
    let is_orphan = app
        .agents
        .get(&agent_id)
        .is_some_and(|a| a.session.session_id.is_none() && a.session.forked_from.is_none());
    if is_orphan {
        let fallback = app.agents.keys().copied().find(|id| *id != agent_id);
        remove_agent_and_cleanup(app, agent_id);
        if let Some(target) = fallback {
            switch_to_agent(app, target, SwitchCause::Picker);
            restore_dashboard_attach_after_orphan_remove(app, agent_id, Some(target));
        } else {
            show_welcome(app);
            app.welcome_prompt_focused = true;
            app.session_picker_entries = None;
            app.session_picker_loading = false;
            app.session_picker_state.selected = 0;
            app.session_picker_content_results = None;
            app.session_picker_content_loading = false;
            restore_dashboard_attach_after_orphan_remove(app, agent_id, None);
        }
        let msg = format!("Cannot create worktree: {error}");
        if !app.startup_warnings.iter().any(|w| w.message == msg) {
            app.startup_warnings.push(crate::startup::StartupWarning {
                severity: crate::startup::WarningSeverity::Warning,
                message: msg,
                action: None,
            });
        }
    } else if let Some(agent) = app.agents.get_mut(&agent_id) {
        agent.pending_extensions_fetch = false;
        agent.session.prompt_history_loading = false;
        agent.mcp_init_progress = None;
        agent.session.finish_command();
        let elapsed = agent.turn_elapsed();
        agent.mark_turn_finished(TurnEnd::Aborted);
        agent.pending_first_prompt = None;
        agent.pending_fork_banner = None;
        agent
            .scrollback
            .push_block(RenderBlock::session_event(SessionEvent::TurnFailed {
                error,
                elapsed,
            }));
    }
    vec![]
}
pub(in crate::app::dispatch) fn handle_switch_model_complete(
    app: &mut AppView,
    agent_id: AgentId,
    model_id: acp::ModelId,
    effort: Option<ReasoningEffort>,
    result: Result<(), SwitchModelError>,
    prev_model_id: Option<acp::ModelId>,
) -> Vec<Effect> {
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        agent.session.model_switch_pending = false;
        let mut effects = match result {
            Ok(()) => {
                agent.session.user_model_preference = Some(model_id.clone());
                let display_name = agent
                    .session
                    .models
                    .available
                    .get(&model_id)
                    .map(|info| info.name.clone())
                    .unwrap_or_else(|| model_id.0.to_string());
                let prev_model = agent.session.models.current.clone();
                let prev_effort = agent.session.models.reasoning_effort;
                agent.session.models.set_current(model_id.clone(), effort);
                let resolved_effort = agent.session.models.reasoning_effort;
                let unchanged =
                    prev_model.as_ref() == Some(&model_id) && prev_effort == resolved_effort;
                if !unchanged {
                    let msg = if let Some(eff) = resolved_effort {
                        format!("Switched to {display_name} ({eff} effort)")
                    } else {
                        format!("Switched to {display_name}")
                    };
                    agent.scrollback.push_block(RenderBlock::system(msg));
                }
                if unchanged {
                    vec![]
                } else {
                    vec![Effect::PersistPreferredModel {
                        model_id: model_id.clone(),
                        reasoning_effort: resolved_effort,
                    }]
                }
            }
            Err(SwitchModelError::IncompatibleAgent { .. }) => {
                if let Some(ref prev) = prev_model_id {
                    agent.session.models.set_current(prev.clone(), None);
                }
                agent.active_modal = None;
                let display_name = agent.session.models.display_name_for(&model_id);
                return open_agent_type_mismatch_question(app, model_id, effort, &display_name);
            }
            Err(SwitchModelError::Other(msg)) => {
                agent
                    .scrollback
                    .push_block(RenderBlock::system(format!("Couldn't switch model: {msg}")));
                vec![]
            }
        };
        let drain = maybe_drain_queue(agent);
        effects.extend(drain.effects);
        note_peek_page_flip(app, agent_id, drain.page_flip_entry);
        effects
    } else {
        vec![]
    }
}
pub(in crate::app::dispatch) fn dispatch_agent_type_mismatch_answered(
    app: &mut AppView,
    start_new: bool,
    model_id: acp::ModelId,
    effort: Option<ReasoningEffort>,
) -> Vec<Effect> {
    if start_new {
        let effects = dispatch_new_session_inner(app, Some(model_id.clone()));
        if let ActiveView::Agent(new_aid) = app.active_view
            && let Some(agent) = app.agents.get_mut(&new_aid)
        {
            agent.session.models.set_current(model_id.clone(), effort);
            if effort.is_some() {
                agent.session.deferred_model_switch = Some(DeferredModelSwitch {
                    model_id,
                    effort,
                    prev_model_id: None,
                });
            }
        }
        effects
    } else {
        vec![]
    }
}
