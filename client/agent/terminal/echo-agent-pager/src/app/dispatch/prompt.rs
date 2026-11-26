//! Prompt and bash-command submission dispatchers and reload-window helpers.

use super::auth::{
    scrollback_has_recent_context_too_large, scrollback_has_recent_disk_full,
    scrollback_has_recent_reauth_prompt, scrollback_has_recent_request_failed,
};
use super::billing::is_credit_limit_error;
use super::ctx::with_active_agent;
use super::interject;
use super::permissions::drain_permission_queue;
use super::queue::{
    apply_turn_start_shim, drain_prompt_state_to_last_queued, immediate_server_send_eligible,
    maybe_drain_queue, note_peek_page_flip, push_and_page_flip, push_server_queue_echo,
    retire_optimistic_echo,
};
use super::router::dispatch;
use super::voice::{merge_prompt_with_voice_interim, voice_stop_on_submit};
use crate::app::actions::{Action, DoctorFixTarget, Effect};
use crate::app::agent::{AgentCommand, AgentId, AgentState};
use crate::app::agent_view::AgentView;
use crate::app::app_view::{ActiveView, AppView};
use crate::app::cancel_latency::TurnEnd;
use crate::notifications::{NotificationEvent, NotificationEventKind};
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::SessionEvent;
use crate::slash::command::DoctorRequest;
use agent_client_protocol as acp;
use echo_agent_telemetry::session_ctx::log_event;

/// Shared by every submit guard that refuses while the session reconnects.
pub(super) const RECONNECTING_NOTICE: &str = "Reconnecting, please wait...";

/// Chat kind for the next create: CLI `--chat` (`app.chat_mode`) or one-shot
/// `/chat` (`deferred_startup.pending_chat`, consumed here).
pub(super) fn consume_chat_kind(app: &mut AppView) -> bool {
    let pending = std::mem::take(&mut app.deferred_startup.pending_chat);
    app.chat_mode || pending
}

/// Enqueue a prompt and try to drain immediately.
///
/// The prompt is always pushed to the queue first. If the agent is idle
/// (and has a session), `maybe_drain_queue` pops the front prompt and
/// sends it in the same dispatch call — no deferred ticks.
/// Start (if needed) and submit the initial prompt from `echoagent "<prompt>"`.
///
/// Shared by the TUI startup path (already authenticated) and the post-login
/// `AuthComplete` path (deferred via `deferred_startup.prompt`). It does nothing
/// special for auth or session lifecycle: it reuses the exact `NewSession` /
/// `SendPrompt` actions the welcome screen dispatches, so the normal
/// session-creation + enqueue/drain machinery carries the prompt (the
/// prompt waits in the queue until `SessionCreated`/`SessionLoaded` drains
/// it). `NewSession` is only dispatched when no session is active yet — a
/// `--resume`/`-c`/`-w` session started earlier in startup is reused.
pub(crate) fn dispatch_initial_prompt(app: &mut AppView, prompt: String) -> Vec<Effect> {
    let mut effects = Vec::new();
    if !matches!(app.active_view, ActiveView::Agent(_)) {
        effects.extend(dispatch(Action::NewSession, app));
    }
    effects.extend(dispatch(Action::SendPrompt(prompt), app));
    effects
}

pub(super) fn collect_live_doctor_report_for_terminal(
    app: &AppView,
    agent_id: AgentId,
    terminal: &crate::terminal::TerminalContext,
) -> Option<crate::diagnostics::DiagnosticReport> {
    let agent = app.agents.get(&agent_id)?;
    let mut report = crate::slash::commands::doctor::DoctorCommand::report_for_terminal(
        terminal,
        app.screen_mode,
        crate::diagnostics::TuiRuntimeRequest {
            workspace: &agent.session.cwd,
            notification_method: app.notification_service.config().method,
            notification_protocol: app.notification_service.protocol(),
            notification_condition: app.notification_service.config().condition,
        },
    );
    if crate::app::voice_mode_enabled() {
        crate::diagnostics::apply_voice_probe(&mut report, true);
    }
    Some(report)
}

fn doctor_fix_target(agent: &AgentView) -> DoctorFixTarget {
    DoctorFixTarget {
        agent_id: agent.session.id,
        session_id: agent.session.session_id.clone(),
        session_binding_epoch: agent.session_binding_epoch,
        cwd: agent.session.cwd.clone(),
    }
}

pub(super) fn dispatch_doctor(request: DoctorRequest, app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(agent_id) = app.active_view else {
        return vec![];
    };
    let terminal = crate::terminal::terminal_context().clone();
    let Some(report) = collect_live_doctor_report_for_terminal(app, agent_id, &terminal) else {
        return vec![];
    };

    match request {
        DoctorRequest::Report => {
            if let Some(agent) = app.agents.get_mut(&agent_id) {
                agent.scrollback.push_block(RenderBlock::system(
                    crate::diagnostics::format_doctor(&report),
                ));
            }
        }
        DoctorRequest::ListFixes | DoctorRequest::Fix(_) => {
            let Some(agent) = app.agents.get(&agent_id) else {
                return vec![];
            };
            let target = doctor_fix_target(agent);
            return vec![Effect::PlanDoctorFix {
                target,
                report: Box::new(report),
                terminal,
                request,
            }];
        }
    }
    vec![]
}

pub(super) fn open_doctor_fix_question(
    app: &mut AppView,
    target: DoctorFixTarget,
    plan: Box<crate::diagnostics::FixPlan>,
) {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };

    let Some(agent) = app.agents.get_mut(&target.agent_id) else {
        return;
    };
    if agent.question_view.is_some() {
        agent.scrollback.push_block(RenderBlock::system(
            "Close the current question before applying this fix.",
        ));
        return;
    }
    let preview = crate::diagnostics::format_fix_preview(&plan);
    let question = Question {
        question: "Apply this fix?".to_owned(),
        options: vec![
            QuestionOption {
                label: "Apply".to_owned(),
                description: "Make the changes shown above.".to_owned(),
                preview: Some(preview),
                id: None,
            },
            QuestionOption {
                label: "Cancel".to_owned(),
                description: "Do not change the configuration.".to_owned(),
                preview: None,
                id: None,
            },
        ],
        multi_select: Some(false),
        id: None,
    };
    let stashed = agent.prompt.stash();
    agent.question_view = Some(
        QuestionViewState::new("doctor-fix".to_owned(), vec![question], stashed)
            .with_local_kind(LocalQuestionKind::DoctorFix { target, plan })
            .with_no_freeform(),
    );
    agent.prompt.set_text("");
}

pub(super) fn dispatch_send_prompt(app: &mut AppView, text: String) -> Vec<Effect> {
    crate::unified_log::info(
        "prompt.enqueue",
        None,
        Some(serde_json::json!({"len": text.len()})),
    );
    dispatch_send_prompt_inner(
        app, text, /* consume_input */ true, /* literal */ false,
        /* is_follow_up */ false,
    )
}

/// Clear the active prompt into the stash (Esc Esc).
///
/// The draft goes to the stash, not the recall list. `Ctrl+S` is how it comes back.
pub(super) fn dispatch_clear_prompt(app: &mut AppView) -> Vec<Effect> {
    with_active_agent(app, |agent| {
        // Recoverable with the stash chord, but Esc-Esc is a discard: it never comes back on its own.
        agent.stash_prompt_draft(crate::app::agent_view::StashCause::ClearedDraft);
    });
    vec![]
}

/// Open the prompt-history search panel on the active agent (composer as
/// filter query). Dispatched by `/history`; the slash pipeline has already
/// cleared the composer, so the panel opens with an empty query.
pub(super) fn dispatch_open_history_search(app: &mut AppView) -> Vec<Effect> {
    with_active_agent(app, |agent| {
        let history = agent.combined_prompt_history();
        let current_text = agent.prompt.text().to_string();
        let opened = agent
            .prompt
            .history_search
            .activate(&history, &current_text);
        if !opened {
            // Matcher thread didn't start: the overlay stays closed on
            // purpose (nothing could ever populate it).
            tracing::debug!("history search unavailable: matcher spawn failed");
        }
    });
    vec![]
}

/// Show the "ctrl+z to undo" hint after the user wiped a substantial draft.
/// Gated by the per-tip `contextual_hints.undo` gate (default ON). The
/// ephemeral-tip seen gate caps it at `UNDO_TIP_SEEN_CAP` shows per session
/// (in-memory `app.tip_seen_counts`); nothing is persisted to disk.
pub(super) fn dispatch_show_undo_tip(app: &mut AppView) -> Vec<Effect> {
    if !app.contextual_hints.undo {
        return vec![];
    }
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    // Shows and increments the per-session count in place (no disk write).
    // Emit the impression only when the tip actually took the slot (mirrors
    // the `tip.shown` gate), so gated no-ops and TTL refreshes don't count.
    if agent.show_ephemeral_tip(
        crate::tips::clear_detector::undo_tip(),
        &mut app.tip_seen_counts,
    ) {
        log_event(echo_agent_telemetry::events::ContextualTip {
            tip: echo_agent_telemetry::events::ContextualTipKind::Undo,
            action: echo_agent_telemetry::events::ContextualTipAction::Shown,
        });
    }
    vec![]
}

/// Show the one-shot "Tight on space? Try /compact-mode" hint after the first
/// stable agent-view draw landed in the small-screen band (the trigger gates
/// on band + user compact OFF; see `AppView::maybe_trigger_small_screen_tip`).
/// Gated by the per-tip `contextual_hints.small_screen` gate (default ON).
/// Seen-gated in-memory via `app.tip_seen_counts`; nothing persists to disk.
///
/// Called directly from the draw-path trigger — not routed as an `Action`,
/// so it returns `()` and "no effects from draw" holds structurally.
pub(in crate::app) fn show_small_screen_tip(app: &mut AppView) {
    if !app.contextual_hints.small_screen {
        return;
    }
    let ActiveView::Agent(id) = app.active_view else {
        return;
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return;
    };
    // Impression only when the tip actually takes the slot (mirrors undo/plan).
    if agent.show_ephemeral_tip(
        crate::tips::small_screen::small_screen_tip(),
        &mut app.tip_seen_counts,
    ) {
        log_event(echo_agent_telemetry::events::ContextualTip {
            tip: echo_agent_telemetry::events::ContextualTipKind::SmallScreen,
            action: echo_agent_telemetry::events::ContextualTipAction::Shown,
        });
    }
}

/// Show the existing one-shot SSH discovery tip, redirected to `/doctor`.
pub(in crate::app) fn show_ssh_wrap_tip(app: &mut AppView) {
    if !app.contextual_hints.ssh_wrap {
        return;
    }
    let ActiveView::Agent(id) = app.active_view else {
        return;
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return;
    };
    if agent.show_ephemeral_tip(
        crate::tips::ssh_wrap::ssh_wrap_tip(),
        &mut app.tip_seen_counts,
    ) {
        log_event(echo_agent_telemetry::events::ContextualTip {
            tip: echo_agent_telemetry::events::ContextualTipKind::SshWrap,
            action: echo_agent_telemetry::events::ContextualTipAction::Shown,
        });
    }
}

pub(super) fn dispatch_show_plan_nudge(app: &mut AppView) -> Vec<Effect> {
    if !app.contextual_hints.plan_mode {
        return vec![];
    }
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    // Shows and increments the per-session count in place (no disk write).
    // Impression counts only on a real show (see `dispatch_show_undo_tip`).
    if agent.show_ephemeral_tip(
        crate::tips::plan_nudge::plan_nudge_tip(),
        &mut app.tip_seen_counts,
    ) {
        log_event(echo_agent_telemetry::events::ContextualTip {
            tip: echo_agent_telemetry::events::ContextualTipKind::PlanMode,
            action: echo_agent_telemetry::events::ContextualTipAction::Shown,
        });
    }
    vec![]
}

/// After a fold/nav double-click on scrollback, tip that Word select lives in
/// `/settings`. Gated by `contextual_hints.word_select` (default ON).
pub(super) fn dispatch_show_word_select_tip(app: &mut AppView) -> Vec<Effect> {
    if !app.contextual_hints.word_select {
        return vec![];
    }
    // Already on word_select — tip would be wrong / redundant.
    if crate::appearance::cache::load_keep_text_selection().selects_word() {
        return vec![];
    }
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    if agent.show_ephemeral_tip(
        crate::tips::word_select::word_select_tip(),
        &mut app.tip_seen_counts,
    ) {
        log_event(echo_agent_telemetry::events::ContextualTip {
            tip: echo_agent_telemetry::events::ContextualTipKind::WordSelect,
            action: echo_agent_telemetry::events::ContextualTipAction::Shown,
        });
    }
    // Snapshot the prompt as of this double-click (also on a same-key TTL
    // refresh — a new double-click is a new moment). Any later divergence
    // (typed, pasted, dropped) refuses the chord and retires the tip; a
    // seen-cap-gated no-show leaves the slot to another tip and skips this.
    if agent.ephemeral_tip.current_key() == Some(crate::tips::word_select::WORD_SELECT_TIP_KEY) {
        agent.word_select_tip_prompt_snapshot = Some(agent.prompt.text().to_string());
    }
    vec![]
}

/// Accept the word-select tip via its advertised chord: flip
/// `keep_text_selection` to `word_select` (cache + persist + toast, the same
/// path as the settings modal) and retire the tip so one impression maps to
/// at most one acceptance. No-op unless the tip is on screen — the chord is
/// tip-scoped and must not become a global setting toggle.
pub(super) fn dispatch_accept_word_select_tip(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    if agent.ephemeral_tip.current_key() != Some(crate::tips::word_select::WORD_SELECT_TIP_KEY) {
        return vec![];
    }
    agent
        .ephemeral_tip
        .clear(crate::tips::word_select::WORD_SELECT_TIP_KEY);
    agent.word_select_tip_prompt_snapshot = None;
    log_event(echo_agent_telemetry::events::ContextualTip {
        tip: echo_agent_telemetry::events::ContextualTipKind::WordSelect,
        action: echo_agent_telemetry::events::ContextualTipAction::Accepted,
    });
    super::settings::setters::set_keep_text_selection(
        app,
        crate::appearance::TextSelection::WordSelect,
    )
}

/// After queuing a follow-up mid-turn, tip that empty Enter force-sends the top
/// queued item. Gated by the per-tip `contextual_hints.send_now` gate (default
/// ON). Seen-gated in-memory via `app.tip_seen_counts`.
fn maybe_show_send_now_tip(app: &mut AppView) {
    if !app.contextual_hints.send_now {
        return;
    }
    let ActiveView::Agent(id) = app.active_view else {
        return;
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return;
    };
    // Impression only when the tip actually takes the slot (mirrors undo/plan).
    if agent.show_ephemeral_tip(
        crate::tips::send_now::send_now_tip(),
        &mut app.tip_seen_counts,
    ) {
        log_event(echo_agent_telemetry::events::ContextualTip {
            tip: echo_agent_telemetry::events::ContextualTipKind::SendNow,
            action: echo_agent_telemetry::events::ContextualTipAction::Shown,
        });
    }
}

/// Body of [`dispatch_send_prompt`], parameterized over whether to consume
/// the prompt textarea after the command is processed.
///
/// `consume_input = true` (Enter from the prompt) wipes the textarea, drains
/// pending prompt images into the queue, and inserts the text into the local
/// up-arrow history. `consume_input = false` (modal-driven dispatch from the
/// command palette or ArgPicker) preserves the user's draft, leaves prompt
/// images attached, and skips the history insert. The slash-registry
/// resolution and the downstream `Effect`s are identical in both cases.
///
/// `literal = true` (follow-up chip click) submits `text` straight to the
/// model: the slash-command and exit-alias branches are skipped so server/model-controlled chip text can never execute a command.
pub(super) fn dispatch_send_prompt_inner(
    app: &mut AppView,
    text: String,
    consume_input: bool,
    literal: bool,
    is_follow_up: bool,
) -> Vec<Effect> {
    // Submitting is a fresh intent that retires any armed double-press. The
    // AppView pending-action check only resets on KEY events, so a submit with
    // no intervening key (mouse send, follow-up chip click `SubmitFollowUp`,
    // `SendSlashCommandPreservingDraft`) would otherwise leave a stale arm
    // (e.g. an idle-Esc `ClearPrompt`) that shadows the next Esc — firing stale
    // ClearPrompt|Rewind instead of the mid-turn Esc policy until TTL. Cleared in
    // the common funnel so every submit path is covered, before any early-return
    // guard below.
    app.pending_action = None;
    // Promote interim + hard-reset; merge only when consuming the composer.
    let interim = voice_stop_on_submit(app);
    let text = if consume_input {
        merge_prompt_with_voice_interim(text, interim)
    } else {
        text
    };

    if app.reconnect_pending {
        app.show_toast(RECONNECTING_NOTICE);
        return vec![];
    }

    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    // Capture app-level fields before the mut-borrow on `agent`.
    let coding_data_sharing_opt_out_from_app = app.coding_data_retention_opt_out;
    let coding_data_sharing_lock_from_app = app.coding_data_sharing_lock();
    let show_tips_from_app = app.show_tips;
    let auto_update_from_app = app.auto_update;
    let respect_manual_folds_from_app = app.appearance.scrollback.scroll.respect_manual_folds;
    let auto_mode_gate_from_app = app.auto_mode_gate;
    let ask_user_question_timeout_enabled_from_app = app.ask_user_question_timeout_enabled;
    // Set when a plain prompt is queued while a turn is running (local path);
    // shown after the agent borrow ends so we can re-enter via the tip helper.
    let mut tip_send_now_after_queue = false;
    let voice_stt_language_from_app = app.voice_config.language.clone();
    let scheduler_background_loops_seed = app.scheduler_background_loops_seed;
    let login_method_id_from_app = app.login_method_id.as_ref().map(|id| id.0.to_string());
    let leader_mode = app.leader_mode;
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    // Paste-then-immediate-send: an image probe from a just-pasted Cmd+V is
    // still off-thread. Stash this send and re-issue it once the probe completes
    // so the image is never dropped from the built content blocks. Scoped to
    // `consume_input` sends: only those clear the draft, so only they can drop a
    // not-yet-attached image — draft-preserving sends (follow-up chip,
    // slash-preserving) keep it in the draft for the next real send.
    if consume_input && agent.paste_probe_in_flight > 0 {
        agent.deferred_send = Some(crate::app::agent_view::AgentDeferredSend::SendPrompt);
        return vec![];
    }

    // Submitting the prompt retires any edit-contextual ephemeral tip
    // (ambient tips live out their TTL across the submit).
    agent.ephemeral_tip.clear_on_submit();

    let trimmed = text.trim();

    // Recorded before the registry runs, because most command outcomes return on their own path.
    let recorded_as_command = !literal && consume_input && trimmed.starts_with('/');
    if recorded_as_command {
        agent.record_prompt_in_history(trimmed);
    }

    let mut effects = Vec::new();

    // ── Tier-restricted command upsell ─────────────────────────────
    // Restricted commands (`/usage`, `/imagine`, …) are hidden from the
    // registry's `get()`, so a typed invocation would otherwise fall
    // through the unknown-command path below and leak to the model as a
    // raw prompt. Upsell instead; genuinely unknown commands still pass
    // through (shell/ACP commands depend on that).
    if !literal
        && trimmed.starts_with('/')
        && let Some(invocation) = crate::slash::parse_invocation(trimmed)
        && agent
            .prompt
            .slash_controller
            .registry()
            .is_restricted(invocation.token)
    {
        // Only consume the composer when the upsell can actually open: with
        // another question modal already up, `open_echoagent_upsell` would
        // no-op and wiping the composer here would silently drop the typed
        // text. Keep it instead so the user can resubmit after closing the
        // modal — and never fall through to passthrough for restricted
        // commands.
        if agent.question_view.is_none() {
            if consume_input {
                agent.prompt.set_text("");
            }
            let opened =
                super::billing::open_restricted_command_upsell(agent, login_method_id_from_app);
            debug_assert!(opened, "no modal was open, so the upsell must open");
        }
        return vec![];
    }

    // ── Registry-based slash command execution ─────────────────────
    // If the text starts with `/`, run it through the slash registry.
    // The registry resolves builtins, ACP-advertised commands, and
    // unknown commands uniformly. Dispatch is the SOLE execution owner.
    // `literal` (chip click) skips this so chip text is never a command.
    if !literal && trimmed.starts_with('/') {
        use crate::slash::command::{CommandExecCtx, CommandResult};
        use crate::slash::parse_invocation;

        // Build execution context.
        let exec_result = {
            let mut ctx = CommandExecCtx {
                models: &agent.session.models,
                session_id: agent.session.session_id.as_ref(),
                bundle_state: &app.bundle_state,
                screen_mode: app.screen_mode,
                billing_surface_visible: app.usage_visible,
                usage_command_visible: !app.has_external_auth_provider,
                // PAGER-owned snapshot for slash commands.
                pager_state: crate::settings::PagerLocalSnapshot {
                    multiline_mode: agent.multiline_mode,
                    yolo_mode: agent.session.is_yolo(),
                    auto_mode: agent.session.is_auto(),
                    current_model_name: agent.session.models.current_model_name(),
                    available_models: agent
                        .session
                        .models
                        .available
                        .iter()
                        .map(|(id, info)| (info.name.clone(), id.clone()))
                        .collect(),
                    coding_data_sharing_opt_out: coding_data_sharing_opt_out_from_app,
                    coding_data_sharing_lock: coding_data_sharing_lock_from_app,
                    // Prefer optimistic pending over confirmed active.
                    plan_mode_active: agent.plan_mode_pending.unwrap_or(agent.plan_mode_active),
                    show_tips: show_tips_from_app,
                    auto_update: auto_update_from_app,
                    vim_mode: crate::appearance::cache::load_vim_mode(),
                    scroll_speed: crate::appearance::cache::load_scroll_speed(),
                    respect_manual_folds: respect_manual_folds_from_app,
                    auto_mode_gate: auto_mode_gate_from_app,
                    ask_user_question_timeout_enabled: ask_user_question_timeout_enabled_from_app,
                    voice_stt_language: voice_stt_language_from_app,
                    // This session's own value (what its fires will actually
                    // do), seed only until the session response lands.
                    scheduler_background_loops: agent
                        .scheduler_background_loops
                        .unwrap_or(scheduler_background_loops_seed),
                },
            };

            if let Some(invocation) = parse_invocation(trimmed) {
                let (is_builtin, command) = {
                    let reg = agent.prompt.slash_controller.registry();
                    let is_builtin = reg.is_builtin(invocation.token);
                    // Bypasses only the menu-only hide (hard gates still
                    // return `None`); see `CommandRegistry::get_for_dispatch`.
                    let command = reg.get_for_dispatch(invocation.token).cloned();
                    (is_builtin, command)
                };
                {
                    use echo_agent_telemetry::events::{PagerCommandSource, PagerSlashCommand};
                    use echo_agent_telemetry::session_ctx::log_event;
                    let source = if is_builtin {
                        PagerCommandSource::Builtin
                    } else {
                        PagerCommandSource::NonBuiltin
                    };
                    log_event(PagerSlashCommand {
                        command_name: invocation.token.to_string(),
                        source,
                    });
                }
                if let Some(command) = command {
                    // Central screen-mode gate. Such a command is already
                    // filtered out of every completion surface, but it stays
                    // resolvable so a fully-typed invocation earns a hint that
                    // names the way out instead of leaking to the model.
                    // A refusal added here must also extend the pre-check in `EditedCommandGate`
                    // (`dispatch::queue`): that caller has to know the command will be refused
                    // before it drops the queued row the text came from.
                    if let Some(refusal) = command
                        .mode_support()
                        .refusal(invocation.token, ctx.screen_mode)
                    {
                        CommandResult::Message(refusal)
                    } else {
                        agent
                            .prompt
                            .slash_controller
                            .record_command_use(invocation.token, invocation.token);
                        command.run(&mut ctx, invocation.args)
                    }
                } else {
                    // Unknown command -- pass through to shell.
                    CommandResult::PassThrough(text.clone())
                }
            } else {
                // Bare `/` or malformed -- pass through.
                CommandResult::PassThrough(text.clone())
            }
        };

        // Map CommandResult to pager behavior. (MRU persistence is queued
        // off-thread inside `record_command_use` above.)
        match exec_result {
            CommandResult::Handled | CommandResult::HandledNoOp => {
                if consume_input {
                    agent.prompt.set_text("");
                }
                return vec![];
            }
            CommandResult::Error(msg) => {
                if consume_input {
                    agent.prompt.set_text("");
                }
                push_and_page_flip(&mut agent.scrollback, RenderBlock::system(msg));
                return vec![];
            }
            CommandResult::Message(msg) => {
                if consume_input {
                    agent.prompt.set_text("");
                }
                push_and_page_flip(&mut agent.scrollback, RenderBlock::system(msg));
                return vec![];
            }
            CommandResult::Doctor(request) => {
                if consume_input {
                    agent.prompt.set_text("");
                }
                return dispatch_doctor(request, app);
            }
            CommandResult::Action(Action::ExitSession) => {
                if consume_input {
                    agent.prompt.set_text("");
                }
                return dispatch(Action::ExitSession, app);
            }
            CommandResult::Action(Action::EditPromptExternal) => {
                // Typed slash input occupies the composer; the palette route preserves an existing draft.
                if consume_input {
                    agent.prompt.set_text("");
                }
                return dispatch(Action::EditPromptExternal, app);
            }
            CommandResult::Action(Action::SendRememberNote(note)) => {
                if consume_input {
                    agent.prompt.set_text("");
                }
                // The typed `/remember <text>` is the row already recorded above.
                return super::notes::dispatch_send_remember_note_from_command(app, note);
            }
            CommandResult::Action(mut action) => {
                if consume_input {
                    // Inline `/feedback` composed alongside pasted images:
                    // the chips belong to the report, so drain them into the
                    // action before the composer wipe destroys them.
                    if let Action::SendFeedback { images, .. }
                    | Action::OpenFeedbackPane { images, .. } = &mut action
                    {
                        *images = agent.prompt.drain_images().into();
                    }
                    agent.prompt.set_text("");
                }
                return dispatch(action, app);
            }
            CommandResult::QueueCommand(cmd_text) => {
                agent.session.enqueue_command(cmd_text);
            }
            CommandResult::InjectSkill {
                display_text,
                prompt_blocks,
                display_as_skill,
                scheduled_task_preview,
            } => {
                // Enqueue with display text for scrollback but wire_blocks
                // for the actual prompt sent to the model. Leading skill
                // invocation: display_as_skill owns styling (no ranges).
                let id = agent.session.next_queue_id;
                agent.session.next_queue_id += 1;
                agent
                    .session
                    .pending_prompts
                    .push_back(crate::app::agent::QueuedPrompt {
                        wire_blocks: Some(prompt_blocks),
                        display_as_skill,
                        ..crate::app::agent::QueuedPrompt::plain(
                            id,
                            display_text,
                            crate::app::agent::QueueEntryKind::Prompt,
                        )
                    });

                // Insert a provisional scheduled task so the tasks pane shows
                // it immediately, before the LLM round-trips through
                // scheduler_create. Keyed by a provisional ID; replaced when
                // the real ScheduledTaskCreated notification arrives.
                if let Some(preview) = scheduled_task_preview {
                    use crate::app::agent::ScheduledTaskInfo;
                    let provisional_id = format!("provisional-{}", id);
                    agent.session.scheduled_tasks.insert(
                        provisional_id.clone(),
                        ScheduledTaskInfo {
                            task_id: provisional_id,
                            prompt: preview.prompt,
                            human_schedule: preview.human_schedule,
                            created_at: std::time::Instant::now(),
                            next_fire_at: preview.next_fire_at,
                            tag: preview.tag,
                            last_subagent_id: None,
                        },
                    );
                }
            }
            CommandResult::PassThrough(pass_text) => {
                // A recognized token later in the passthrough text still styles the echo.
                let skill_token_ranges = agent
                    .prompt
                    .slash_controller
                    .recognized_token_ranges(&pass_text, &agent.session.models);
                agent
                    .session
                    .enqueue_prompt_with_skill_tokens(pass_text, skill_token_ranges);
            }
        }
        if consume_input {
            // Drain prompt images before clearing prompt state.
            drain_prompt_state_to_last_queued(agent);
            agent.prompt.set_text("");
            agent.note_draft_consumed();
        }
    } else if !literal && crate::slash::commands::exit::is_exit_alias(trimmed) {
        if consume_input {
            agent.prompt.set_text("");
        }
        return dispatch(Action::Quit, app);
    } else {
        // ── Server-authoritative immediate send (plain prompt only) ──
        // A plain prompt typed while a turn is RUNNING is sent to the agent
        // immediately instead of being held in the local drip-feed queue. The
        // agent appends it to its authoritative `pending_inputs` (no concurrent
        // turn starts — validated keystone) and drives the drain via
        // `echo.agent/queue/changed`. We render an optimistic echo into the shared
        // queue keyed by `prompt_id`; the broadcast reconciles it by id.
        //
        // The IDLE case is unchanged (falls through to the local path below,
        // which drains instantly and renders the user block) — preserving the
        // byte-for-byte idle experience. Image/skill/editing/non-running cases
        // also stay local; they're out of immediate-send scope.
        // Plain prompts also require "no images" (image prompts stay local).
        //
        // A follow-up chip submission supersedes the current response's
        // suggestions: clear the visible chips here — INSIDE the send/enqueue
        // path, after the `reconnect_pending` and active-agent early-return
        // guards — so the chips are cleared ONLY when the suggestion actually
        // sends/enqueues. Placing it before those guards (the prior fix) cleared
        // the chips even when `reconnect_pending` aborted with a toast and no
        // send, losing both the chips and the submit. This single clear covers
        // BOTH the immediate-send and enqueue subpaths below; `clear_follow_ups`
        // is idempotent (so the immediate-send branch's own clear is a no-op)
        // and keeps `follow_up_seen` (a stale re-delivery stays rejected).
        //
        // Gate on a BOUND session: with no `session_id`, the enqueue subpath
        // below queues the text but `maybe_drain_queue` returns WITHOUT emitting
        // `SendPrompt` (nothing can drain to an unbound session), so clearing the
        // chips here would lose the click with nothing submitted. Leaving them
        // shown preserves the suggestion for a retry once the session binds.
        if is_follow_up && agent.session.session_id.is_some() {
            agent.clear_follow_ups();
        }

        // If the user queues a follow-up while a turn is already running, surface
        // a short tip advertising send-now — plain Enter queues; Enter again on
        // the emptied composer sends the queued message now (cancel-and-send).
        let queued_while_running = agent.session.state.is_turn_running();

        // Composer-recognized slash tokens at submit time: styles the
        // scrollback echo and rides the wire meta so replay restyles it.
        let skill_token_ranges = agent
            .prompt
            .slash_controller
            .recognized_token_ranges(&text, &agent.session.models);

        let immediate_server_send =
            immediate_server_send_eligible(agent, leader_mode) && agent.prompt.images.is_empty();
        tracing::debug!(
            target: "qtrace",
            pid = std::process::id(),
            event = "send_route_plain",
            immediate = immediate_server_send,
            is_turn_running = agent.session.state.is_turn_running(),
            shared_queue_len = agent.shared_queue.len(),
            pending_len = agent.session.pending_prompts.len(),
            current_prompt_id = agent.session.current_prompt_id.as_deref().unwrap_or(""),
            session = agent.session.session_id.as_ref().map(|s| s.0.as_ref()).unwrap_or(""),
            images = agent.prompt.images.len(),
            text = %text.chars().take(48).collect::<String>(),
            "plain prompt send routing decision",
        );

        // Occupancy/park flags: only the image branch below immediately send-nows on an empty held wait.
        let parked_sendable_wait = agent.is_parked_on_sendable_wait();
        let hold_behind_existing_queue = parked_sendable_wait && agent.has_held_user_queue();

        // Images can't ride immediate server-send; empty-held park still send-nows.
        if !immediate_server_send
            && immediate_server_send_eligible(agent, leader_mode)
            && !agent.prompt.images.is_empty()
            && parked_sendable_wait
            && !hold_behind_existing_queue
        {
            let images = agent.prompt.drain_images();
            if consume_input {
                agent.prompt.set_text("");
                agent.note_draft_consumed();
            }
            // A new prompt is taking the wheel (same contract as the
            // immediate-send branch below).
            agent.clear_follow_ups();
            return interject::dispatch_send_prompt_now(app, text, images);
        }

        if immediate_server_send {
            let session_id = agent
                .session
                .session_id
                .clone()
                .expect("session_id is_some checked");
            let agent_id = agent.session.id;
            let prompt_id = uuid::Uuid::new_v4().to_string();
            // Self-originated: when this prompt becomes the running turn (via the
            // `running_prompt_id` adoption + turn-start shim), the ACP gate must
            // treat its deltas as ours, not adopt them as another client's turn.
            agent.note_self_originated_prompt(&prompt_id);
            // Plain image-free sends stay unarmed: shell queue state and cancelTrigger decide disposition.

            if consume_input {
                // Plain prompt: no images to drain. Clear textarea + record
                // up-arrow history (same as the local path's history insert).
                agent.prompt.set_text("");
                agent.note_draft_consumed();
                agent.record_prompt_in_history(&text);
            }

            // A new prompt is taking the wheel: the previous response's
            // follow-up chips must not linger into it. The local drain
            // (`maybe_drain_queue`) and the turn-start shim clear them on
            // their paths; this immediate-send path returns early, so it must
            // clear them here too (notably a chip click, which submits while
            // a turn is running). `clear_follow_ups` keeps `follow_up_seen`
            // (turn-boundary semantics) so a stale re-delivery stays rejected.
            agent.clear_follow_ups();

            // `agent` borrow ends here; push the optimistic echo via `app`.
            let sid_str = session_id.0.to_string();
            push_server_queue_echo(app, agent_id, &sid_str, &prompt_id, &text, "prompt");
            crate::unified_log::info(
                "prompt.send_server_authoritative",
                Some(&sid_str),
                Some(serde_json::json!({ "kind": "prompt", "len": text.len() })),
            );
            if queued_while_running
                && !parked_sendable_wait
                && !crate::appearance::cache::load_follow_up_steer()
            {
                maybe_show_send_now_tip(app);
            }

            return vec![Effect::SendPrompt {
                agent_id,
                session_id,
                text,
                prompt_id,
                skill_token_ranges,
            }];
        }

        agent
            .session
            .enqueue_prompt_with_skill_tokens(text.clone(), skill_token_ranges);
        if consume_input {
            // Drain prompt images before clearing prompt state.
            drain_prompt_state_to_last_queued(agent);
            agent.prompt.set_text("");
            agent.note_draft_consumed();
        }
        // Local queue while a turn is running (e.g. images attached): tip after
        // this branch so the agent mut-borrow is released first.
        tip_send_now_after_queue = queued_while_running;
    }

    // Mid-turn local queue: advertise send-now via the ephemeral tip (skip during
    // a sendable wait — the inline hint already says it).
    if tip_send_now_after_queue {
        let inline_hint_shown = app
            .agents
            .get(&id)
            .is_some_and(|agent| agent.held_queue_count() > 0);
        if !inline_hint_shown {
            maybe_show_send_now_tip(app);
        }
    }

    let drain = {
        let Some(agent) = app.agents.get_mut(&id) else {
            return effects;
        };

        // Skipped for modal-driven dispatch: the user didn't type these commands and shouldn't see them in up-arrow history.
        // `PassThrough`, `QueueCommand` and `InjectSkill` reach here, so a command recorded above would land twice.
        if consume_input && !recorded_as_command {
            agent.record_prompt_in_history(&text);
        }
        maybe_drain_queue(agent)
    };
    effects.extend(drain.effects);
    note_peek_page_flip(app, id, drain.page_flip_entry);
    // A prompt queued while the turn is already busy (wait / live watcher /
    // running tool) would otherwise sit locally until the next ACP batch.
    // An open /btw overlay does not produce that batch, so a send after
    // `/btw` would stay queued for the rest of the wait. Release here —
    // same helper the ACP re-check uses; a no-op when the turn is not busy.
    effects.extend(super::queue::maybe_release_queued_prompt_into_turn(
        app, None,
    ));
    effects
}

/// Enqueue a bash command and try to drain immediately.
///
/// Bash commands go through the same enqueue/drain pipeline as normal prompts,
/// just with `QueueEntryKind::BashCommand`. No scrollback block is pushed here;
/// the execute block from the shell IS the visual entry.
pub(super) fn dispatch_send_bash_command(app: &mut AppView, command: String) -> Vec<Effect> {
    if app.reconnect_pending {
        app.show_toast(RECONNECTING_NOTICE);
        return vec![];
    }

    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let leader_mode = app.leader_mode;
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    // Submitting a bash command retires any edit-contextual ephemeral tip.
    agent.ephemeral_tip.clear_on_submit();

    agent.record_prompt_in_history(&crate::app::agent_view::prompt_history_text(
        &command,
        crate::app::agent_view::PromptInputMode::Bash,
    ));

    // ── Server-authoritative immediate send for bash while running ──
    // A bash command typed while a turn is RUNNING is sent to the agent
    // immediately (it's already a `session/prompt` with bash meta) and echoed
    // into the shared queue with `kind="bash"`. On `running_prompt_id`
    // adoption the turn-start shim sets `bash_turn` (no user block). The IDLE
    // case is unchanged: enqueue locally + drain instantly.
    let bash_immediate = immediate_server_send_eligible(agent, leader_mode);
    tracing::debug!(
        target: "qtrace",
        pid = std::process::id(),
        event = "send_route_bash",
        immediate = bash_immediate,
        is_turn_running = agent.session.state.is_turn_running(),
        shared_queue_len = agent.shared_queue.len(),
        pending_len = agent.session.pending_prompts.len(),
        current_prompt_id = agent.session.current_prompt_id.as_deref().unwrap_or(""),
        session = agent.session.session_id.as_ref().map(|s| s.0.as_ref()).unwrap_or(""),
        text = %command.chars().take(48).collect::<String>(),
        "bash command send routing decision",
    );
    if bash_immediate {
        let session_id = agent
            .session
            .session_id
            .clone()
            .expect("session_id is_some checked");
        let agent_id = agent.session.id;
        let prompt_id = uuid::Uuid::new_v4().to_string();
        // Self-originated (see the plain immediate-send path): keep this turn's
        // deltas ours in the ACP gate once it becomes the running turn.
        agent.note_self_originated_prompt(&prompt_id);
        agent.prompt.set_text("");
        agent.note_draft_consumed();

        let sid_str = session_id.0.to_string();
        push_server_queue_echo(app, agent_id, &sid_str, &prompt_id, &command, "bash");
        crate::unified_log::info(
            "prompt.send_server_authoritative",
            Some(&sid_str),
            Some(serde_json::json!({ "kind": "bash", "len": command.len() })),
        );
        return vec![Effect::SendBashCommand {
            agent_id,
            session_id,
            command,
            prompt_id,
        }];
    }

    agent.session.enqueue_bash_command(command.clone());
    agent.prompt.set_text("");
    agent.note_draft_consumed();

    let drain = maybe_drain_queue(agent);
    note_peek_page_flip(app, id, drain.page_flip_entry);
    drain.effects
}

/// Whether a load-result handler must stand down because a reconnect reload
/// window is open on the agent.
///
/// The window owns the agent's batch / `loading_replay` / turn state; a load
/// result resolving mid-window (a stale fresh-view load, or `/resume` racing a
/// reconnect) must not close it — flipping `loading_replay` would make the
/// replay gate drop the rest of the reconnect replay, and a failure block
/// would be pushed into staging state. The window finalize supersedes the
/// result.
pub(super) fn defer_to_open_reload_window(
    agent: &AgentView,
    agent_id: AgentId,
    result: &str,
) -> bool {
    if agent.session_reload.is_none() {
        return false;
    }
    tracing::warn!(
        agent = ?agent_id,
        result,
        "load result during an open reload window — deferring to the window finalize"
    );
    true
}

/// The initiation-side counterpart of [`defer_to_open_reload_window`]: a load
/// INITIATION that takes over the agent (fork/worktree-fork/remote-restore
/// binding a session) finalizes any open reload window as failed first, so
/// the new load owns the agent's batch/replay state and its results are not
/// deferred. Unreachable through today's flows (these arms target freshly
/// created `session_id: None` agents, which can never host a window) —
/// defense in depth against future initiation paths on live agents.
pub(super) fn supersede_open_reload_window(
    agent: &mut AgentView,
    agent_id: AgentId,
    initiation: &str,
) {
    if agent.session_reload.is_none() {
        return;
    }
    tracing::warn!(
        agent = ?agent_id,
        initiation,
        "load initiation supersedes an open reload window (finalizing as failed)"
    );
    agent.abort_session_reload();
}

// TaskResult handlers.

pub(super) fn handle_prompt_response(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<acp::PromptResponse, String>,
    http_status: Option<u16>,
    prompt_id: Option<String>,
) -> Vec<Effect> {
    // A server-authoritative queued prompt may have drained into
    // the running slot while this turn was still finishing (the leader's
    // `running_prompt_id` broadcast can arrive before this
    // `PromptResponse`). Take any stashed adoption now; it is applied
    // after `finish_turn` clears `current_prompt_id` below.
    let pending_adoption = app.pending_running_adoptions.remove(&agent_id);
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        // Discard PromptResponses that don't belong to the currently
        // active prompt -- they belong to a turn the user rewound, or to
        // a queued prompt that never became the running turn.
        //
        // The prompt id comes from two places depending on the arm:
        //   - `Ok`:  the agent echoes `promptId` in PR meta.
        //   - `Err`: an `acp::Error` carries NO meta, so we fall back to
        //            the `prompt_id` the pager minted when it sent this
        //            RPC (threaded through `TaskResult::PromptResponse`).
        //
        // Without the `Err` fallback, a queued prompt's RPC error has no
        // id to gate on and is misattributed to the running turn — e.g.
        // when a queued prompt is removed in leader mode and its
        // `respond_to` is dropped on the leader, the resulting
        // "session failed to respond" error would detonate an unrelated
        // in-flight turn with a spurious "Turn failed" (on the
        // submitter's screen, even when another client did the edit).
        let response_pid = match &result {
            Ok(pr) => pr
                .meta
                .as_ref()
                .and_then(|m| m.get("promptId"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            Err(_) => prompt_id.clone(),
        };
        // The turn-end RPC for this prompt arrived — disarm the
        // lost-response reconcile that `handle_prompt_complete` armed
        // for it (the broadcast is emitted before the RPC response, so
        // in the healthy path the marker lives only a few ms).
        if let Some(pending) = agent.pending_turn_end_reconcile.as_ref()
            && response_pid.as_deref() == Some(pending.prompt_id.as_str())
        {
            agent.pending_turn_end_reconcile = None;
        }
        if let Some(response_pid) = response_pid.as_deref()
            && agent.session.current_prompt_id.as_deref() != Some(response_pid)
        {
            if (agent.session.current_prompt_id.is_none()
                || agent
                    .session
                    .current_prompt_id
                    .as_deref()
                    .is_some_and(crate::app::acp_handler::is_server_initiated_prompt))
                && crate::app::acp_handler::is_server_initiated_prompt(response_pid)
            {
                // Server-initiated turn (auto-wake) — adopt.
                agent.session.current_prompt_id = Some(response_pid.to_string());
            } else {
                // Not the running turn: this response (Ok rewound/stale,
                // or Err from a queued/removed prompt) must not touch the
                // active turn. Restore the adoption we popped above so a
                // genuinely-draining next prompt can still be adopted by
                // the real running turn's PromptResponse — unless it is the
                // stashed turn's own response: that spent the turn's only
                // exit, so consume (discard), never restore.
                if let Some(p) = pending_adoption {
                    if p.prompt_id == response_pid {
                        agent.discard_pending_adoption_updates(&p.prompt_id);
                    } else {
                        app.pending_running_adoptions.insert(agent_id, p);
                    }
                }
                // Server-authoritative queue lifecycle: this prompt's RPC
                // resolved without becoming the running turn (removed,
                // cancelled, rewound). Retire its optimistic echo so a
                // later `echo.agent/queue/changed` broadcast can't re-pin a
                // stale placeholder and reorder the queue.
                if let Some(sid) = agent.session.session_id.as_ref().map(|s| s.0.to_string()) {
                    retire_optimistic_echo(
                        &mut app.optimistic_prompt_echoes,
                        &mut app.shared_prompt_queues,
                        &sid,
                        response_pid,
                    );
                    agent.shared_queue.retain(|e| e.id != response_pid);
                    agent.note_queue_echo_retired(response_pid);
                }
                // Resolved-without-running never adopts; explicit for the
                // session-less arm (no note_queue_echo_retired above).
                // Exception: an active-goal Send Now painted block still awaiting
                // its interjection claim stays put — the `RemovedFromQueue`
                // response is the expected outcome of routing the Send Now as an
                // interjection, and `handle_interjection` converts the block in
                // place. Retiring it here (before that claim wins the race) would
                // drop and re-push the message at the scrollback end.
                if !agent.is_send_now_awaiting_interjection_claim(response_pid) {
                    agent.retire_send_now_painted_block(response_pid);
                }
                return vec![];
            }
        }
        let was_cancelling = agent.session.state.is_cancelling()
            || matches!(
                &result,
                Ok(pr) if pr.stop_reason == acp::StopReason::Cancelled
            );
        // Send-now cancel: suppress the "Turn cancelled by user" marker (the new
        // prompt follows right under the partial). Wire `cancelTrigger` wins, else
        // the client-side expectation; consumed at every turn end (no stale flag).
        let expected_send_now = agent.expect_send_now_cancel.take();
        let wire_cancel_trigger = result.as_ref().ok().and_then(|pr| {
            pr.meta
                .as_ref()?
                .get(crate::app::turn_completion::CANCEL_TRIGGER_KEY)?
                .as_str()
                .map(str::to_string)
        });
        let send_now_cancel = was_cancelling
            && match wire_cancel_trigger.as_deref() {
                Some(trigger) => trigger == "send_now",
                None => expected_send_now.is_some(),
            };
        // A hook-denied end rides the cancelled stop reason but is a policy
        // block, not a user cancel — `cancelled_turn_event` picks the marker.
        let wire_cancellation_category = result.as_ref().ok().and_then(|pr| {
            pr.meta
                .as_ref()?
                .get(crate::app::turn_completion::CANCELLATION_CATEGORY_KEY)?
                .as_str()
                .map(str::to_string)
        });
        let rate_limited = agent.session.rate_limited;
        // Fallback mirroring the credit-limit race guard below: if the retry
        // notification lost the race with (or never reached) this
        // PromptResponse, detect the free-usage code from the prompt error
        // itself — the flattened 429 body embeds it.
        let free_usage_blocked = agent.session.free_usage_blocked
            || result.as_ref().err().is_some_and(|e| {
                echo_agent_runtime::sampling::error::is_free_usage_exhausted_error(e)
            });
        let model_incompatible = agent.session.model_incompatible;
        // Context overflow: the RetryState handler already pushed the actionable
        // block, so the generic TurnFailed + error toast are redundant. Derived
        // from the scrollback (mirrors reauth), not a session flag.
        let context_overflow = scrollback_has_recent_context_too_large(&agent.scrollback);
        let disk_full_from_error = result
            .as_ref()
            .err()
            .is_some_and(|e| crate::app::effects::is_disk_full_error(e));
        if disk_full_from_error && !scrollback_has_recent_disk_full(&agent.scrollback) {
            agent
                .scrollback
                .push_block(RenderBlock::session_event(SessionEvent::DiskFull));
        }
        let disk_full = disk_full_from_error || scrollback_has_recent_disk_full(&agent.scrollback);
        // Fallback: if the retry notification didn't set the flag,
        // detect credit-limit denials (legacy 403 or pool 402) from
        // the PromptResponse error + HTTP status. Covers races where
        // the retry notification arrives after the PromptResponse.
        // The error text is already banner-formatted ("Request failed (402):
        // …"), so recover the status from it when the field is absent.
        let credit_limit_blocked = agent.session.credit_limit_blocked
            || result.as_ref().err().is_some_and(|e| {
                let status =
                    http_status.or_else(|| crate::app::error_display::parse_http_status(e));
                is_credit_limit_error(status, e)
            });
        // A 401/auth failure already surfaced an actionable
        // `ReAuthRequired` prompt via the RetryState handler (which
        // runs before this PromptResponse). Suppress the redundant
        // "Turn failed" block + error toast so only the prompt shows.
        // "(401)" matches both the raw "Unauthorized (401)" dump and the
        // banner-formatted "Request failed (401): …" text.
        let reauth_prompted = scrollback_has_recent_reauth_prompt(&agent.scrollback)
            || (http_status == Some(401)
                && result.as_ref().err().is_some_and(|e| e.contains("(401)")));
        let request_failed_shown = scrollback_has_recent_request_failed(&agent.scrollback);
        // A dedicated prompt/modal/banner replaces the generic TurnFailed
        // marker and error toast (rate limit, free-usage paywall, model
        // incompatibility, credit 402/403, 401 re-auth, context overflow,
        // disk-full, or a formatted RequestFailed banner from RetryState).
        let dedicated_ux_shown = rate_limited
            || free_usage_blocked
            || model_incompatible
            || credit_limit_blocked
            || reauth_prompted
            || context_overflow
            || disk_full
            || request_failed_shown;
        let elapsed = agent.turn_elapsed();

        {
            let sid = agent.session.session_id.as_ref().map(|s| s.0.as_ref());
            let elapsed_ms = elapsed.map(|d| d.as_millis() as u64).unwrap_or(0);
            let ok = result.is_ok();
            crate::unified_log::info(
                "turn.complete",
                sid,
                Some(serde_json::json!({
                    "elapsed_ms": elapsed_ms,
                    "ok": ok,
                    "was_cancelling": was_cancelling,
                    "send_now_cancel": send_now_cancel,
                })),
            );
        }

        // Stash the complete in-flight prompt before finish_turn clears it.
        // Used by CreditLimitRecheckComplete to retry after a tier upgrade.
        if credit_limit_blocked {
            agent.credit_limit_stashed_prompt = agent.session.in_flight_prompt.clone();
        }
        // Stash for AuthComplete after 401. Prefer in_flight; fall back to
        // compact_held (cleared for cancel-rewind during auto-compact). Skip if both None.
        if reauth_prompted {
            let held = agent
                .session
                .in_flight_prompt
                .clone()
                .or_else(|| agent.session.compact_held_prompt.clone());
            if let Some(prompt) = held {
                agent.reauth_stashed_prompt = Some(prompt);
            }
        }

        // qtrace: turn end on this client. This clears current_prompt_id
        // and (briefly) returns the client to Idle — the start of the
        // leader-mode turn-end window where a freshly-sent prompt can be
        // wrongly local-drained before the next running-prompt broadcast
        // is adopted.
        tracing::debug!(
            target: "qtrace",
            pid = std::process::id(),
            event = "turn_end",
            prompt_id = prompt_id.as_deref().unwrap_or(""),
            was_cancelling,
            shared_queue_len = agent.shared_queue.len(),
            pending_len = agent.session.pending_prompts.len(),
            has_pending_adoption = pending_adoption.is_some(),
            session = agent.session.session_id.as_ref().map(|s| s.0.as_ref()).unwrap_or(""),
            "turn ended; client returning to idle",
        );

        // Read before `finish_turn()` clears it; keys the pending stop-hook stash.
        let ending_prompt_id = agent
            .session
            .current_prompt_id
            .clone()
            .or_else(|| response_pid.clone());

        agent.session.finish_turn(&mut agent.scrollback);

        // Insert session event message (skip TurnCompleted for bash-mode — no agent turn).
        let event = match (&result, was_cancelling) {
            // Send-now cancel: no marker (the new prompt is the next turn); the
            // `None` still flushes any held stop hooks standalone.
            (Ok(_), true) if send_now_cancel => None,
            (Ok(_), true) => Some(crate::app::turn_completion::cancelled_turn_event(
                wire_cancellation_category.as_deref(),
                elapsed.unwrap_or_default(),
            )),
            (Ok(_), false) if agent.bash_turn => None,
            (Ok(_), false) => Some(SessionEvent::TurnCompleted {
                // Legacy copy on purpose: unknown elapsed keeps the "in 0.0s"
                // form here — only wake markers use the honest `None` form.
                elapsed: Some(elapsed.unwrap_or_default()),
            }),
            (Err(_), _) if dedicated_ux_shown => None,
            // `err` is already banner-formatted by `format_acp_error` at the
            // producer — the single formatting owner. Don't re-format here.
            (Err(err), _) => Some(SessionEvent::TurnFailed {
                error: err.clone(),
                elapsed,
            }),
        };
        crate::app::turn_completion::push_turn_terminal_marker(
            agent,
            event,
            ending_prompt_id.as_deref(),
        );

        let notification = match (&result, was_cancelling) {
            (Ok(_), false) if !agent.bash_turn => {
                let body = match elapsed {
                    Some(d) => {
                        format!("Turn complete in {}.", crate::util::format_duration(d))
                    }
                    None => String::from("Turn complete."),
                };
                Some((NotificationEventKind::TurnComplete, body))
            }
            (Err(err), _) if !dedicated_ux_shown => {
                Some((NotificationEventKind::AgentError, format!("Error: {err}")))
            }
            _ => None,
        };

        agent.mark_turn_finished(TurnEnd::Completed);
        agent.activity_started_at = None;
        agent.last_activity = None;

        // Drain all queued permission requests — the turn is over,
        // so any pending permissions are stale. Send Cancelled to each.
        drain_permission_queue(agent);

        // Dismiss any active plan approval or review — the turn
        // that produced it has completed, so the state is stale.
        if let Some(mut pav) = agent.plan_approval_view.take() {
            pav.send_stale_cancel();
            agent.plan_next_comment_id = pav.next_comment_id;
            agent.prompt.restore(pav.stashed_prompt);
            agent.line_viewer = None;
        }

        agent.cancel_turn_view = None;
        agent.cancel_turn_buttons.clear();

        // After a bash-mode turn, scroll to bottom so the user sees
        // the command output, but keep focus on the prompt for
        // consistency with normal prompt behavior.
        let was_bash_turn = agent.bash_turn;
        if agent.bash_turn {
            agent.bash_turn = false;
            agent.scrollback.goto_bottom();
        }
        agent.cron_task_id = None;

        // TurnComplete suppressed when queue is non-empty (badge
        // fires only after the final queued turn); AgentError always fires.
        if let Some((kind, body)) = notification {
            // A stashed server-authoritative adoption means the next
            // turn is about to start, so treat the queue as non-empty
            // (suppress the TurnComplete notification / idle escapes),
            // mirroring the local non-empty-queue behavior.
            let queue_empty =
                agent.session.pending_prompts.is_empty() && pending_adoption.is_none();
            let session_name = agent
                .display_name
                .as_deref()
                .or(agent.generated_session_title.as_deref());

            // Skip idle escapes when queue is non-empty — the next
            // turn starts immediately and would overwrite them (title flicker).
            if queue_empty {
                let cwd_str = app.cwd.to_string_lossy();
                let model = agent.session.models.current_model_name();
                let idle_title = crate::notifications::TitleState {
                    session_name,
                    model: model.as_deref(),
                    activity: None,
                    has_pending_permissions: false,
                    cwd: Some(&cwd_str),
                    turn_elapsed: None,
                    is_busy: false,
                    focused: true,
                };
                app.pending_notification_escapes =
                    app.notification_service.build_idle_escapes(&idle_title);
            }

            if kind != NotificationEventKind::TurnComplete || queue_empty {
                // Defer the notification so the terminal has time
                // to apply the idle title.  Ghostty debounces
                // setTitle() by 75 ms (SurfaceView_AppKit.swift:576),
                // so we need >75 ms before the notification reads
                // self.title for the subtitle.  3 ticks × 33 ms ≈ 99 ms.
                let session_id = agent.session.session_id.as_ref().map(|s| s.0.to_string());

                // Use the session name as the notification title so
                // terminals that show it (Ghostty/OSC 777) display
                // which session completed.  For body-only protocols
                // (Warp, iTerm2/OSC 9), emit_notification folds the
                // title into the body automatically.
                let notif_title = session_name
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "EchoAgent".into());

                app.deferred_notification = Some((
                    NotificationEvent {
                        kind,
                        title: notif_title,
                        body,
                        session_id,
                    },
                    3,
                ));
            }
        }

        if let Err(ref err) = result {
            tracing::error!(agent = ?agent_id, error = %err, "Prompt failed");
        }

        // Predicted-next-prompt (tab autocomplete): wipe any stale suggestion
        // at every turn boundary. This must run before the reconnect /
        // credit-limit early returns below, which skip the fetch gate
        // entirely — a prior ghost would otherwise survive those paths.
        agent.prompt.prompt_suggestion.clear();

        // Cancelled turns resume queue processing one item at a time
        // through the same drain path as normal completions.
        // `maybe_drain_queue` keeps the idle-only and editing-front
        // guards so we do not send from under the user.
        if app.reconnect_pending {
            if let Some(p) = pending_adoption {
                agent.discard_pending_adoption_updates(&p.prompt_id);
            }
            return vec![];
        }

        // Credit-limit (403 legacy / 402 pool): strip stale error
        // blocks, then do a one-shot subscription re-check. If the
        // tier changed (user upgraded mid-session), the stashed
        // prompt is retried automatically; otherwise the upsell
        // is shown.
        if credit_limit_blocked {
            // Strip stale error blocks that were pushed before the
            // credit-limit was detected.
            let to_remove: Vec<usize> = super::auth::trailing_session_events(&agent.scrollback)
                .filter(|(_, ev)| {
                    matches!(
                        ev,
                        SessionEvent::RequestFailed { .. }
                            | SessionEvent::RetryFailed { .. }
                            | SessionEvent::TurnFailed { .. }
                    )
                })
                .map(|(idx, _)| idx)
                .collect();
            for idx in to_remove {
                agent.scrollback.remove_from(idx);
            }

            // Defer the upsell until the subscription re-check
            // completes. Queue drain + billing fetch happen in the
            // CreditLimitRecheckComplete handler.
            if let Some(p) = pending_adoption {
                agent.discard_pending_adoption_updates(&p.prompt_id);
            }
            return vec![Effect::CreditLimitRecheck { agent_id }];
        }

        // Free-usage paywall (429 + subscription:free-usage-exhausted): the
        // RetryState handler set the flag and suppressed the generic
        // rate-limit block; show the upsell modal. Driver-only by
        // construction — viewers never receive a PromptResponse. No queue
        // drain: queued prompts would fail on the same exhausted quota.
        if free_usage_blocked {
            let auth_method = app.login_method_id.as_ref().map(|id| id.0.to_string());
            super::billing::open_free_usage_upsell(agent, auth_method);
            if let Some(p) = pending_adoption {
                agent.discard_pending_adoption_updates(&p.prompt_id);
            }
            return vec![];
        }

        // FIFO handoff: if a server-authoritative prompt drained
        // into the running slot during this turn's teardown, adopt it
        // now (finish_turn cleared current_prompt_id) and run the
        // turn-start shim. This sets `TurnRunning`, so the
        // `maybe_drain_queue` below no-ops rather than draining a local
        // prompt — the leader owns the drain order.
        let adopted_page_flip = if let Some(p) = pending_adoption
            && agent.session.current_prompt_id.is_none()
        {
            if response_pid.as_deref() != Some(p.prompt_id.as_str())
                && agent.should_adopt_running_prompt(&p.prompt_id)
            {
                apply_turn_start_shim(agent, p.prompt_id, p.text, &p.kind, p.combined_texts)
            } else {
                agent.discard_pending_adoption_updates(&p.prompt_id);
                None
            }
        } else {
            None
        };

        let drain = maybe_drain_queue(agent);
        let page_flip_entry = adopted_page_flip.or(drain.page_flip_entry);
        let mut effects = drain.effects;

        // Predicted-next-prompt (tab autocomplete): fetch a fresh suggestion
        // (the stale one was wiped above) — but only after a clean, non-bash
        // agent turn that leaves the session idle with an empty prompt and no
        // queued work, local or server-side (a draft in progress or a draining
        // queue means the user is already mid-thought). Placed after
        // `maybe_drain_queue` so `is_idle` reflects a locally-drained next
        // turn.
        if crate::views::prompt_suggestion::resolve_enabled()
            && result.is_ok()
            && !was_cancelling
            && !was_bash_turn
            && agent.prompt.text().is_empty()
            && agent.session.pending_prompts.is_empty()
            && agent.shared_queue.is_empty()
            && agent.session.state.is_idle()
            && let Some(session_id) = agent.session.session_id.as_ref().map(|s| s.0.to_string())
        {
            let generation = agent.prompt.prompt_suggestion.begin_fetch();
            let model = crate::views::prompt_suggestion::resolve_model(&agent.session.models);
            effects.push(Effect::FetchPromptSuggestion {
                agent_id,
                generation,
                model,
                session_id: Some(session_id),
            });
        }

        effects.push(Effect::FetchBilling {
            agent_id,
            silent: true,
            nonce: Default::default(),
        });
        note_peek_page_flip(app, agent_id, page_flip_entry);
        return effects;
    }
    vec![]
}

pub(super) fn handle_compact_complete(
    app: &mut AppView,
    agent_id: AgentId,
    result: Result<(), String>,
) -> Vec<Effect> {
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        // Defensive: only process if we're still in a compact command state.
        let was_cancelling = matches!(
            agent.session.state,
            AgentState::CommandCancelling {
                command: AgentCommand::Compact,
            }
        );
        if !matches!(
            agent.session.state,
            AgentState::CommandRunning {
                command: AgentCommand::Compact,
                ..
            } | AgentState::CommandCancelling {
                command: AgentCommand::Compact,
            }
        ) {
            tracing::debug!("Ignoring CompactComplete (not in compact command state)");
            return vec![];
        }

        let elapsed = agent.turn_elapsed();
        agent.session.finish_command();

        match &result {
            Ok(()) => {
                agent.scrollback.push_block(RenderBlock::session_event(
                    SessionEvent::CompactCompleted {
                        elapsed: elapsed.unwrap_or_default(),
                    },
                ));
            }
            Err(err) if was_cancelling || err.contains("compact cancelled") => {
                agent.scrollback.push_block(RenderBlock::session_event(
                    SessionEvent::CompactionCancelled,
                ));
            }
            Err(err) => {
                tracing::error!(agent = ?agent_id, error = %err, "Compaction failed");
                agent.scrollback.push_block(RenderBlock::session_event(
                    SessionEvent::CompactionFailed {
                        error: String::new(),
                    },
                ));
            }
        }

        agent.mark_turn_finished(TurnEnd::Completed);
        agent.activity_started_at = None;
        agent.last_activity = None;

        if app.reconnect_pending {
            return vec![];
        }
        let drain = maybe_drain_queue(agent);
        note_peek_page_flip(app, agent_id, drain.page_flip_entry);
        return drain.effects;
    }
    vec![]
}

pub(super) fn handle_suggestion_debounce_expired(
    app: &mut AppView,
    agent_id: AgentId,
    generation: u64,
) -> Vec<Effect> {
    // Route by the arming agent (the timer carries it), not the active
    // view: a view switch inside the debounce window must neither fire a
    // spurious fetch on another agent nor drop this one's.
    let Some(agent) = app.agents.get(&agent_id) else {
        return vec![];
    };
    // Bash-mode feature: a debounce that outlives the mode fetches nothing.
    if agent.prompt_input_mode != crate::app::agent_view::PromptInputMode::Bash {
        return vec![];
    }
    if !agent.prompt.suggestions.on_debounce_expired(generation) {
        return vec![];
    }
    let text = agent.prompt.text().to_owned();
    let cursor = agent.prompt.cursor();
    let cwd = agent.session.cwd.to_string_lossy().into_owned();
    let include_ai = agent.prompt.suggestions.ai_enabled;
    let ai_model = agent.prompt.suggestions.ai_model.clone();
    let session_id = agent.session.session_id.as_ref().map(|s| s.0.to_string());
    vec![Effect::FetchShellSuggestions {
        agent_id,
        text,
        cursor,
        cwd,
        generation,
        limit: crate::views::suggestion_controller::SHELL_SUGGEST_WIRE_LIMIT,
        include_ai,
        ai_model,
        session_id,
        // The as-you-type (ghost) surface keeps history/AI providers.
        token_only: false,
    }]
}
