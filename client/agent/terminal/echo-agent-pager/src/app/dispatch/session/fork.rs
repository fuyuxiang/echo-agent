//! Fork dispatchers and fork placeholder builders.
use super::lifecycle::{dispatch_new_session_inner_with_id, refuse_chat_mode_build_agent};
use super::load::session_opens_as_chat;
use crate::acp::tracker::AcpUpdateTracker;
use crate::app::actions::Effect;
use crate::app::agent::{AgentCommand, AgentId, AgentSession, AgentState};
use crate::app::agent_view::AgentView;
use crate::app::app_view::{ActiveView, AppView};
use crate::app::cancel_latency::TurnEnd;
use crate::app::dispatch::ctx::{SwitchCause, switch_to_agent};
use crate::app::dispatch::modes::inherit_auto_mode;
use crate::app::dispatch::prompt::supersede_open_reload_window;
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::SessionEvent;
use crate::scrollback::state::ScrollbackState;
use agent_client_protocol as acp;
use std::time::Instant;
/// Top-level `/fork` dispatcher. Resolves the worktree decision: an
/// explicit `--worktree` / `--no-worktree` flag short-circuits to
/// [`dispatch_fork_resolved`]. When no flag is given and a persisted
/// `fork_worktree_mode` preference is set (`Always` / `Never`), the
/// popup is skipped and the corresponding path is taken directly. The
/// `Ask` default opens the [`open_fork_question`] modal so the user is
/// asked.
///
/// When the parent session's working directory is **not** inside a git
/// repository (indicated by the absence of a `git_head_changed`
/// notification — `current_branch` is `None`):
/// - `--worktree` is rejected with a toast (nothing to create a worktree from).
/// - No flag (regardless of `fork_worktree_mode`): the worktree question
///   is skipped and the fork proceeds with `worktree = false`.
///
/// Note: if the notification has not arrived yet (rare — user forks
/// before the shell sends `git_head_changed`), the fallback to
/// `worktree = false` is safe and the worktree can be created manually
/// afterwards.
///
/// Two failure surfaces:
/// - Active view is not an agent: toast and return.
/// - Active agent has no `session_id` (still being created): toast and
///   return. Both rejections are deliberate -- queueing the fork until
///   `SessionLoaded` would require persisting `ForkArgs` across the
///   `TaskResult` and is deferred to v2.
pub(in crate::app::dispatch) fn dispatch_fork(
    app: &mut AppView,
    args: crate::slash::commands::fork::ForkArgs,
) -> Vec<Effect> {
    let ActiveView::Agent(parent_id) = app.active_view else {
        app.show_toast("/fork only works inside a session");
        return vec![];
    };
    let (has_session, in_git_repo) = app
        .agents
        .get(&parent_id)
        .map(|a| (a.session.session_id.is_some(), a.current_branch.is_some()))
        .unwrap_or((false, false));
    if !has_session {
        app.show_toast("Cannot fork: session is still being created");
        return vec![];
    }
    match args.worktree_override {
        Some(true) if !in_git_repo => {
            app.show_toast("Cannot create worktree: not in a git repository");
            vec![]
        }
        Some(worktree) => dispatch_fork_resolved(app, worktree, args.directive),
        None => {
            if in_git_repo {
                use crate::app::app_view::WorktreeMode;
                match app.fork_worktree_mode {
                    WorktreeMode::Always => dispatch_fork_resolved(app, true, args.directive),
                    WorktreeMode::Never => dispatch_fork_resolved(app, false, args.directive),
                    WorktreeMode::Ask => open_fork_question(app, args.directive),
                }
            } else {
                dispatch_fork_resolved(app, false, args.directive)
            }
        }
    }
}
/// If `persist_mode` is `Some`, write `mode` into `*field` and append
/// a [`Effect::PersistWorktreeMode`] to `effects` with the given
/// `config_key`.
pub(in crate::app::dispatch) fn apply_persist_worktree_mode(
    field: &mut crate::app::app_view::WorktreeMode,
    effects: &mut Vec<Effect>,
    persist_mode: Option<crate::app::app_view::WorktreeMode>,
    config_key: &'static str,
) {
    if let Some(mode) = persist_mode {
        *field = mode;
        effects.push(Effect::PersistWorktreeMode { mode, config_key });
    }
}
/// Build the two persistence options shared by the fork and new-session
/// worktree question modals ("Always worktree" / "Never worktree").
pub(super) fn worktree_persist_options()
-> [echo_agent_tools::implementations::echo_agent_build::ask_user_question::QuestionOption; 2] {
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::QuestionOption;
    [
        QuestionOption {
            label: "Always worktree".into(),
            description: "Use worktree and stop asking (reset in config.toml)".into(),
            preview: None,
            id: None,
        },
        QuestionOption {
            label: "Never worktree".into(),
            description: "Skip worktree and stop asking (reset in config.toml)".into(),
            preview: None,
            id: None,
        },
    ]
}
/// Open the local worktree question modal on the active agent. Refuses
/// if a question (ACP or local) is already on screen, surfacing a toast
/// instead -- the modal-collision protocol.
fn open_fork_question(app: &mut AppView, directive: Option<String>) -> Vec<Effect> {
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
            description: "Fork in a new isolated git worktree".into(),
            preview: None,
            id: None,
        },
        QuestionOption {
            label: "No".into(),
            description: "Fork in the current cwd".into(),
            preview: None,
            id: None,
        },
    ];
    options.extend(worktree_persist_options());
    let question = Question {
        question: "Run this fork in an isolated git worktree?".into(),
        id: None,
        options,
        multi_select: Some(false),
    };
    let agent = app.agents.get_mut(&id).expect("agent present (re-borrow)");
    let stashed = agent.prompt.stash();
    let state = QuestionViewState::new(
        format!("fork-{}", uuid::Uuid::new_v4()),
        vec![question],
        stashed,
    )
    .with_local_kind(LocalQuestionKind::Fork { directive });
    agent.question_view = Some(state);
    agent.prompt.set_text("");
    vec![]
}
/// Construct the placeholder agent, push discoverability markers, flip
/// the discovery gate, switch to the new agent, and emit the appropriate
/// fork effect (worktree or no-worktree path).
///
/// `worktree == true` reuses the existing
/// [`Effect::CreateWorktreeSession`] pipeline (with `load_session_id`
/// set to the parent session id). `worktree == false` emits the new
/// [`Effect::ForkSession`] which calls `echo.agent/session/fork` directly.
pub(in crate::app::dispatch) fn dispatch_fork_resolved(
    app: &mut AppView,
    worktree: bool,
    directive: Option<String>,
) -> Vec<Effect> {
    let ActiveView::Agent(parent_id) = app.active_view else {
        return vec![];
    };
    let Some(parent) = app.agents.get(&parent_id) else {
        return vec![];
    };
    let Some(parent_session_id) = parent.session.session_id.clone() else {
        app.show_toast("Cannot fork: session not yet created");
        return vec![];
    };
    let parent_cwd = parent.session.cwd.clone();
    let parent_is_worktree = parent.session.is_worktree;
    let new_id = AgentId(app.next_agent_id);
    app.next_agent_id += 1;
    let new_agent = build_fork_placeholder(app, new_id, parent_id, &parent_cwd, worktree);
    let parent_marker = match directive.as_deref() {
        Some(d) => format!("Forked: {d}"),
        None => "Forked".to_string(),
    };
    let parent_chat_kind = parent.chat_kind || app.chat_mode;
    let parent_conversation_entry = parent.conversation_entry;
    app.agents.insert(new_id, new_agent);
    {
        let agent = app
            .agents
            .get_mut(&new_id)
            .expect("just-inserted agent missing");
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
        agent.chat_kind = parent_chat_kind;
        agent.conversation_entry = parent_conversation_entry;
        agent.apply_credit_balance(app.credit_balance.clone(), app.auto_topup.clone());
        agent
            .prompt
            .slash_controller
            .registry_mut()
            .set_plugins_visible(!app.appearance.disable_plugins);
        agent.pending_fork_banner = Some(crate::app::agent_view::PendingForkBanner {
            parent_sid: parent_session_id.0.to_string(),
            worktree,
        });
        if worktree {
            agent
                .scrollback
                .push_block(RenderBlock::system("Creating worktree\u{2026}".to_string()));
        }
        agent.pending_first_prompt = directive;
    }
    if let Some(parent_mut) = app.agents.get_mut(&parent_id) {
        parent_mut
            .scrollback
            .push_block(RenderBlock::system(parent_marker));
    }
    switch_to_agent(app, new_id, SwitchCause::Fork);
    if worktree {
        vec![Effect::CreateWorktreeSession {
            agent_id: new_id,
            load_session_id: Some(parent_session_id.0.to_string()),
            label: None,
            git_ref: None,
            // Fork resumes the parent session, which carries its own model.
            model_id: None,
            permission_mode_override: None,
            preferred_session_id: None,
            chat_kind: parent_chat_kind,
        }]
    } else {
        vec![Effect::ForkSession {
            agent_id: new_id,
            parent_session_id,
            parent_cwd,
            parent_is_worktree,
            new_session_id: None,
        }]
    }
}
/// Build the placeholder [`AgentView`] for a fork. Centralises the
/// `AgentSession`/spinner construction shared by both worktree and
/// no-worktree branches so the parallel struct literal does not drift.
fn build_fork_placeholder(
    app: &AppView,
    new_id: AgentId,
    parent_id: AgentId,
    parent_cwd: &std::path::Path,
    worktree: bool,
) -> AgentView {
    let mut scrollback = ScrollbackState::new();
    scrollback.set_appearance(app.appearance.clone());
    let mut agent = AgentView::new(
        AgentSession {
            id: new_id,
            acp_tx: app.acp_tx.clone(),
            session_id: None,
            models: app.models.clone(),
            state: AgentState::Idle,
            tracker: AcpUpdateTracker::new(),
            cwd: parent_cwd.to_path_buf(),
            is_worktree: false,
            forked_from: Some(parent_id),
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
    let cmd = if worktree {
        AgentCommand::CreateWorktree
    } else {
        AgentCommand::ForkSession
    };
    agent.session.start_command(cmd);
    agent.turn_started_at = Some(Instant::now());
    agent
}
/// Build the discoverability banner for the child agent. Includes the
/// child's session id, the full parent session id, and — when
/// `switch_hint` names a command (the caller's
/// [`crate::views::dashboard::session_switch_hint_command`]: `/dashboard`
/// normally, `/resume` in minimal mode where the dashboard is refused) —
/// a session-switch tip so the user knows how to switch back. No-worktree
/// case appends the dim continuation `(both agents share cwd)`.
///
/// Called in `TaskResult::SessionLoaded` (not at dispatch time) because
/// the child's session id is not known until the backend responds.
pub(in crate::app::dispatch) fn build_child_fork_marker(
    session_id: &str,
    parent_sid: &str,
    worktree: bool,
    switch_hint: Option<&str>,
) -> String {
    let header = if let Some(cmd) = switch_hint {
        format!(
            "Session {session_id} (forked from {parent_sid}), use {cmd} to switch between sessions",
        )
    } else {
        format!("Session {session_id} (forked from {parent_sid})")
    };
    if worktree {
        header
    } else {
        format!("{header}\n  (both agents share cwd)")
    }
}
pub(in crate::app::dispatch) fn dispatch_startup_fork_session(
    app: &mut AppView,
    parent_session_id: String,
    parent_cwd: Option<std::path::PathBuf>,
    new_session_id: Option<String>,
) -> Vec<Effect> {
    if !app.session_startup_allowed() {
        app.deferred_startup.session =
            Some(crate::app::session_startup::DeferredSessionStartup::Fork {
                parent_session_id,
                parent_cwd,
                new_session_id,
            });
        return vec![];
    }
    let (_agent_id, mut effects) = dispatch_new_session_inner_with_id(app, None);
    let agent_id = app
        .agents
        .keys()
        .next_back()
        .copied()
        .expect("fork placeholder agent");
    effects.retain(|e| !matches!(e, Effect::CreateSession { .. }));
    let cwd = parent_cwd.unwrap_or_else(|| app.cwd.clone());
    let parent_is_worktree =
        crate::app::session_startup::parent_session_is_worktree(&parent_session_id, &cwd);
    effects.push(Effect::ForkSession {
        agent_id,
        parent_session_id: acp::SessionId::new(parent_session_id),
        parent_cwd: cwd,
        parent_is_worktree,
        new_session_id,
    });
    effects
}
#[allow(clippy::too_many_arguments)]
pub(in crate::app::dispatch) fn handle_worktree_forked(
    app: &mut AppView,
    agent_id: AgentId,
    session_id: acp::SessionId,
    worktree_path: std::path::PathBuf,
    session_cwd: std::path::PathBuf,
    code_restored: bool,
    restore_summary: Option<String>,
    restore_degree: Option<echo_agent_workspace::session::git::RestoreDegree>,
    resume_session_id: Option<String>,
) -> Vec<Effect> {
    let session_id_str = session_id.0.to_string();
    let pending_entry = std::mem::take(&mut app.deferred_startup.pending_chat);
    let agent_entry = app
        .agents
        .get(&agent_id)
        .is_some_and(|a| a.conversation_entry);
    let conversation_entry = pending_entry || agent_entry;
    if crate::app::session_startup::chat_mode_refuses_local_build_load(
        app.chat_mode,
        conversation_entry,
        &session_id_str,
        &app.cwd,
    ) {
        refuse_chat_mode_build_agent(app, agent_id);
        return vec![];
    }
    let rename_entry = session_opens_as_chat(app, conversation_entry);
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        supersede_open_reload_window(agent, agent_id, "WorktreeForked");
        agent.session.finish_command();
        agent.mark_turn_finished(TurnEnd::Aborted);
        agent.bind_session_id(session_id);
        agent.scrollback.begin_batch();
        agent.begin_replay_window();
        agent.session.restore_degree = restore_degree;
        agent.session.cwd = session_cwd.clone();
        agent.session.is_worktree = true;
        agent.current_branch = None;
        agent.main_repo = None;
        agent.is_worktree = true;
        crate::git_info::populate_from_cwd_async(session_cwd.clone());
        app.restore_code = None;
        agent.prompt.file_search.retarget(&session_cwd);
        agent.scrollback.push_block(RenderBlock::system(format!(
            "Worktree ready: {}",
            worktree_path.display()
        )));
        match (code_restored, restore_summary.as_deref()) {
            (true, Some(s)) => {
                agent
                    .scrollback
                    .push_block(RenderBlock::system(format!("\u{2713} Code restored: {s}")));
            }
            (false, Some(s)) => {
                agent.scrollback.push_block(RenderBlock::system(format!(
                    "\u{26A0} Code restore failed: {s}"
                )));
            }
            _ => {}
        }
        let effective_chat = conversation_entry || app.chat_mode;
        agent.chat_kind = effective_chat;
        agent.conversation_entry = rename_entry;
        agent.apply_credit_balance(app.credit_balance.clone(), app.auto_topup.clone());
    } else {
        return vec![];
    }
    if let Some(resume_id) = resume_session_id.as_deref() {
        crate::app::event_loop::retarget_suppress_code_restore(
            app,
            resume_id,
            session_id_str.clone(),
        );
    }
    vec![Effect::LoadSession {
        agent_id,
        session_id: session_id_str,
        session_cwd: Some(session_cwd),
        chat_kind: conversation_entry,
    }]
}
pub(in crate::app::dispatch) fn handle_fork_session_ready(
    app: &mut AppView,
    agent_id: AgentId,
    new_session_id: acp::SessionId,
    cwd: std::path::PathBuf,
    parent_session_id: acp::SessionId,
) -> Vec<Effect> {
    let session_id_str = new_session_id.0.to_string();
    let pending_entry = std::mem::take(&mut app.deferred_startup.pending_chat);
    let agent_entry = app
        .agents
        .get(&agent_id)
        .is_some_and(|a| a.conversation_entry);
    let conversation_entry = pending_entry || agent_entry;
    if crate::app::session_startup::chat_mode_refuses_local_build_load(
        app.chat_mode,
        conversation_entry,
        &session_id_str,
        &app.cwd,
    ) {
        refuse_chat_mode_build_agent(app, agent_id);
        return vec![];
    }
    let rename_entry = session_opens_as_chat(app, conversation_entry);
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        supersede_open_reload_window(agent, agent_id, "ForkSessionReady");
        agent.session.finish_command();
        agent.mark_turn_finished(TurnEnd::Aborted);
        agent.bind_session_id(new_session_id);
        agent.scrollback.begin_batch();
        agent.begin_replay_window();
        agent.session.cwd = cwd.clone();
        let effective_chat = conversation_entry || app.chat_mode;
        agent.chat_kind = effective_chat;
        agent.conversation_entry = rename_entry;
    } else {
        return vec![];
    }
    crate::app::event_loop::retarget_suppress_code_restore(
        app,
        parent_session_id.0.as_ref(),
        session_id_str.clone(),
    );
    vec![Effect::LoadSession {
        agent_id,
        session_id: session_id_str,
        session_cwd: Some(cwd),
        chat_kind: conversation_entry,
    }]
}
pub(in crate::app::dispatch) fn handle_fork_session_failed(
    app: &mut AppView,
    agent_id: AgentId,
    error: String,
) -> Vec<Effect> {
    tracing::error!(agent = ?agent_id, error = %error, "Fork session failed");
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        agent.pending_extensions_fetch = false;
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
