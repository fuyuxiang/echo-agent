//! Top-level action router: maps actions and action results to handlers.
use super::auth::{
    dispatch_cancel_login, dispatch_login, dispatch_logout, dispatch_submit_auth_code,
    dispatch_switch_account,
};
use super::billing::dispatch_open_echoagent_url;
use super::ctx::{
    active_agent_session_id, get_active_agent_mut, navigate_clearing_selection, open_url_or_show,
    sync_sleep_inhibitor, with_active_agent, with_scrollback,
};
use super::dashboard::{
    dispatch_dashboard_attach, dispatch_dashboard_begin_rename, dispatch_dashboard_change_location,
    dispatch_dashboard_commit_rename, dispatch_dashboard_confirm_worktree,
    dispatch_dashboard_create_new_agent_with_detail, dispatch_dashboard_delete,
    dispatch_dashboard_dispatch, dispatch_dashboard_dispatch_slash,
    dispatch_dashboard_open_location_picker, dispatch_dashboard_open_shortcuts_help,
    dispatch_dashboard_overlay_cycle, dispatch_dashboard_overlay_exit,
    dispatch_dashboard_overlay_stop, dispatch_dashboard_peek_cycle_mode,
    dispatch_dashboard_peek_reply, dispatch_dashboard_permission_followup,
    dispatch_dashboard_permission_select, dispatch_dashboard_question_answer,
    dispatch_dashboard_reorder, dispatch_dashboard_select, dispatch_dashboard_stop,
    dispatch_dashboard_toggle_auto_approve, dispatch_dashboard_toggle_grouping,
    dispatch_dashboard_toggle_pin, dispatch_dashboard_toggle_worktree, dispatch_exit_dashboard,
    dispatch_open_dashboard,
};
use super::import_claude::{
    dispatch_dismiss_claude_import, dispatch_import_claude, dispatch_import_claude_cancel,
    dispatch_import_claude_confirm,
};
use super::interject::dispatch_interject;
use super::jump::{dispatch_jump_dismiss, dispatch_jump_picker_select, dispatch_jump_show_picker};
use super::modes::{
    dispatch_cycle_mode, dispatch_enter_plan_mode, dispatch_show_plan, dispatch_toggle_yolo,
    set_permission_mode, set_plan_mode, set_yolo_mode,
};
use super::notes::{
    dispatch_enter_remember_mode, dispatch_open_feedback_pane,
    dispatch_save_remember_note_from_modal, dispatch_send_btw, dispatch_send_feedback,
    dispatch_send_recap, dispatch_send_remember_note,
};
use super::permissions::{
    dispatch_permission_cancel, dispatch_permission_followup, dispatch_permission_select,
};
use super::prompt::{
    dispatch_accept_word_select_tip, dispatch_clear_prompt, dispatch_open_history_search,
    dispatch_send_bash_command, dispatch_send_prompt, dispatch_send_prompt_inner,
    dispatch_show_plan_nudge, dispatch_show_undo_tip, dispatch_show_word_select_tip,
};
use super::queue;
use super::queue::dispatch_drain_queue;
use super::rewind::{
    dispatch_inline_edit_submit, dispatch_rewind, dispatch_rewind_cancel_offer,
    dispatch_rewind_confirm, dispatch_rewind_confirm_never_ask, dispatch_rewind_dismiss,
    dispatch_rewind_dismiss_error, dispatch_rewind_picker_select, dispatch_rewind_show_picker,
};
use super::session::foreign::dispatch_fetch_session_list;
use super::session::fork::{
    apply_persist_worktree_mode, dispatch_fork, dispatch_fork_resolved,
    dispatch_startup_fork_session,
};
use super::session::lifecycle::{
    clear_startup_actions, dispatch_accept_consent, dispatch_agent_type_mismatch_answered,
    dispatch_delete_current_session_answered, dispatch_exit_session, dispatch_new_session,
    dispatch_new_session_inner, dispatch_new_session_with_id, dispatch_new_worktree_session,
    dispatch_trust_folder, open_delete_current_session_question, open_new_session_question,
};
use super::session::load::{
    dispatch_cycle_session_source_filter, dispatch_load_session, dispatch_pick_content_session,
    dispatch_pick_content_session_in_worktree, dispatch_pick_session,
    dispatch_pick_session_in_worktree, dispatch_session_picker_closed,
    dispatch_show_session_picker, dispatch_trigger_deep_search, session_picker_entry_matches,
    session_picker_external_filter_active,
};
use super::session::modal::{dispatch_rename_session, dispatch_reset_session_title};
use super::settings::setters::{
    clear_default_model, clear_fork_secondary_model, preview_auto_dark_theme,
    preview_auto_light_theme, preview_theme, set_ask_user_question_timeout_enabled,
    set_auto_dark_theme, set_auto_light_theme, set_auto_update, set_collapsed_edit_blocks,
    set_combine_queued_prompts, set_compact_mode, set_confirm_before_rewind,
    set_contextual_hint_image_input, set_contextual_hint_plan_mode, set_contextual_hint_send_now,
    set_contextual_hint_small_screen, set_contextual_hint_ssh_wrap, set_contextual_hint_undo,
    set_contextual_hint_word_select, set_default_model, set_default_selected_permission,
    set_display_refresh_auto_cadence, set_follow_up_behavior, set_fork_secondary_model,
    set_group_tool_verbs, set_hunk_tracker_mode, set_invert_scroll, set_keep_text_selection,
    set_max_thoughts_width, set_multiline_mode, set_page_flip_on_send, set_prompt_suggestions,
    set_remember_tool_approvals, set_render_mermaid, set_respect_manual_folds, set_screen_mode,
    set_scroll_lines, set_scroll_mode, set_scroll_speed, set_show_thinking_blocks, set_show_tips,
    set_simple_mode, set_theme, set_timeline, set_timestamps, set_vim_mode, set_voice_capture_mode,
    set_voice_keybind_enabled, set_voice_stt_language,
};
use super::settings::ui::{
    dispatch_confirm_reset_setting, dispatch_open_command_palette, dispatch_open_howto_guides,
    dispatch_open_reset_confirm, dispatch_open_settings, dispatch_toggle_compact_mode,
    dispatch_toggle_mouse_capture, dispatch_toggle_multiline, dispatch_toggle_timestamps,
    dispatch_toggle_vim_mode,
};
use super::status::{
    dispatch_copy_session_id, dispatch_manage_billing, dispatch_open_gboom, dispatch_open_tutorial,
    dispatch_privacy_banner_opt_in, dispatch_privacy_banner_opt_out, dispatch_share_session,
    dispatch_show_context_info, dispatch_show_queue, dispatch_show_release_notes,
    dispatch_show_session_info, dispatch_show_tasks, dispatch_show_usage, set_coding_data_sharing,
};
use super::task_result::{dispatch_task_result, unregister_all_active_sessions};
use super::transcript::{
    dispatch_copy_assistant_message, dispatch_copy_block_content, dispatch_copy_block_meta,
    dispatch_dump_input_log, dispatch_export_conversation, dispatch_open_block_viewer,
    dispatch_open_config_agents_modal, dispatch_open_extensions_modal,
    dispatch_open_transcript_pager,
};
use super::turn::{
    dispatch_cancel_scheduled_task, dispatch_cancel_turn, dispatch_cancel_turn_choice,
    dispatch_demote_to_background, dispatch_kill_bg_task, dispatch_kill_subagent,
};
use super::voice::{dispatch_enable_voice_mode, dispatch_voice_stop, dispatch_voice_toggle};
use crate::app::actions::{Action, Effect};
use crate::app::agent_view::ActivePane;
use crate::app::app_view::{ActiveView, AppView, AuthState};
use crate::app::consent::ConsentState;
use crate::scrollback::types::DisplayMode;
use crate::views::session_picker::CONTENT_EXPAND_OFFSET;
use echo_agent_telemetry::session_ctx::log_event;
pub(super) fn dispatch_copy_auth_url(
    app: &mut AppView,
    copy: impl FnOnce(&str) -> crate::clipboard::ClipboardDelivery,
) -> Vec<Effect> {
    let AuthState::Authenticating {
        auth_url: Some(url),
        ..
    } = &app.auth_state
    else {
        return vec![];
    };
    app.auth_clipboard_delivery = Some(copy(url));
    app.auth_clipboard_feedback_generation = app.auth_clipboard_feedback_generation.wrapping_add(1);
    vec![Effect::ScheduleClearAuthCopyFeedback {
        generation: app.auth_clipboard_feedback_generation,
    }]
}
/// Dispatch an action: mutate state, return effects to execute.
///
/// The returned `Vec<Effect>` may be empty (pure state mutation) or contain
/// async work that the event loop should spawn.
///
/// The match feeds the `sync_sleep_inhibitor(app)` tail below it; arms that
/// `return` early bypass that tail deliberately. Do not extract a returning
/// arm into a handler: as a delegation its `return`s become plain arm values
/// and start flowing through the tail. The fat inline arms stayed inline for
/// this reason; audit an arm's `return`s before moving it.
pub(crate) fn dispatch(action: Action, app: &mut AppView) -> Vec<Effect> {
    app.reconcile_foreign_resume_launch();
    let effects = match action {
        Action::Quit | Action::QuitConfirmed => {
            if let Some(tx) = &app.voice_cmd_tx {
                let _ = tx.try_send(echo_agent_voice::VoiceCommand::Shutdown);
            }
            let mut effects = unregister_all_active_sessions(app);
            effects.push(Effect::Quit);
            effects
        }
        Action::QuitForUpdate => {
            let mut effects = unregister_all_active_sessions(app);
            app.quit_for_update = true;
            effects.push(Effect::Quit);
            effects
        }
        Action::ResumeForeignSession => {
            let Some(hint) = app.take_foreign_resume_hint() else {
                return vec![];
            };
            clear_startup_actions(app);
            let source = crate::app::foreign_sessions::ForeignPickerSource::from_tool(hint.tool);
            tracing::info!(
                tool = source.picker_source(),
                age_secs = hint.age.as_secs(),
                "foreign_resume accepted"
            );
            let prompt = source.resume_prompt(&hint.native_id);
            if !app.session_startup_allowed() {
                app.deferred_startup.prompt = Some(prompt);
                return vec![];
            }
            super::dispatch_initial_prompt(app, prompt)
        }
        Action::RelaunchInScreenMode { minimal } => {
            if !crate::app::screen_mode_relaunch::exec_switch_forced() {
                app.pending_screen_mode_switch = Some(if minimal {
                    crate::app::ScreenMode::Minimal
                } else {
                    crate::app::ScreenMode::Fullscreen
                });
                return vec![];
            }
            if let Some(session_id) = app.active_session_id().map(str::to_owned) {
                app.relaunch = Some(crate::app::app_view::ScreenModeRelaunch {
                    minimal,
                    session_id,
                });
            }
            let mut effects = unregister_all_active_sessions(app);
            effects.push(Effect::Quit);
            effects
        }
        Action::NewSession => dispatch_new_session(app),
        #[cfg(feature = "local-workspace")]
        Action::ConfirmWelcomeLocalWorkspaceAck => {
            match crate::views::welcome::workspace_mode::confirm_welcome_local_workspace_ack(
                &app.cwd, false,
            ) {
                Ok(cfg) => {
                    app.welcome_workspace_mode =
                        crate::views::welcome::WelcomeWorkspaceMode::LocalWorkspace;
                    app.welcome_session_local_workspace = Some(Some(cfg));
                    app.welcome_local_workspace_ack_pending = false;
                    let effects = if app.deferred_startup.worktree {
                        app.deferred_startup.worktree = false;
                        let label = app.deferred_startup.worktree_label.take();
                        let git_ref = app.deferred_startup.worktree_ref.take();
                        let load_session_id = match app.deferred_startup.session.take() {
                            Some(crate::app::session_startup::DeferredSessionStartup::Load {
                                session_id,
                                ..
                            }) => Some(session_id),
                            other => {
                                app.deferred_startup.session = other;
                                None
                            }
                        };
                        let preferred = app.deferred_startup.preferred_session_id.take();
                        dispatch_new_worktree_session(
                            app,
                            load_session_id,
                            label,
                            None,
                            None,
                            git_ref,
                            preferred,
                        )
                    } else {
                        dispatch_new_session(app)
                    };
                    if !crate::app::event_loop::welcome_oneshot_applies_to_effects(&effects) {
                        app.welcome_session_local_workspace = None;
                    }
                    effects
                }
                Err(err) => {
                    tracing::warn!("welcome local-workspace ack: {err}");
                    app.show_toast(&format!("Local workspace: {err}"));
                    vec![]
                }
            }
        }
        Action::ChooseNewSessionMode => open_new_session_question(app),
        Action::ExitSession | Action::ExitSessionConfirmed => dispatch_exit_session(app),
        Action::DeleteCurrentSession => open_delete_current_session_question(app),
        Action::DeleteCurrentSessionAnswered { confirmed } => {
            dispatch_delete_current_session_answered(app, confirmed)
        }
        Action::NewWorktreeSession {
            load_session_id,
            label,
            git_ref,
        } => dispatch_new_worktree_session(app, load_session_id, label, None, None, git_ref, None),
        Action::OpenNewWorktreeDialog => {
            app.new_worktree_dialog = Some(crate::app::app_view::NewWorktreeDialogState::new());
            vec![]
        }
        Action::ImportClaudeSettings => dispatch_import_claude(app),
        Action::ImportClaudeConfirm => dispatch_import_claude_confirm(app),
        Action::ImportClaudeCancel => dispatch_import_claude_cancel(app),
        Action::DismissClaudeImport => dispatch_dismiss_claude_import(app),
        Action::LoadSession(session_id, session_cwd, chat_kind) => {
            dispatch_load_session(app, session_id, session_cwd, chat_kind)
        }
        Action::NewSessionWithId(session_id) => dispatch_new_session_with_id(app, session_id),
        Action::StartupForkSession {
            parent_session_id,
            parent_cwd,
            new_session_id,
        } => dispatch_startup_fork_session(app, parent_session_id, parent_cwd, new_session_id),
        Action::FetchSessionList => dispatch_fetch_session_list(app),
        Action::CycleSessionSourceFilter => dispatch_cycle_session_source_filter(app),
        Action::ShowSessionPicker => dispatch_show_session_picker(app),
        Action::SessionPickerClosed => dispatch_session_picker_closed(app),
        Action::PickSession(index) => dispatch_pick_session(app, index),
        Action::PickSessionInWorktree(index) => dispatch_pick_session_in_worktree(app, index),
        Action::CopySessionId(index) => dispatch_copy_session_id(app, index),
        Action::ExpandSessionCard { source, session_id } => {
            let native_source = matches!(source.as_str(), "local" | "remote" | "both");
            let conversation_source = source == "conversation";
            if session_picker_external_filter_active(app)
                || crate::app::foreign_sessions::is_foreign_picker_source(&source)
                || (!native_source && !conversation_source)
            {
                return vec![];
            }
            use crate::views::modal::ActiveModal;
            let detail_generation = app.session_picker_detail_generation;
            let from_modal = if let Some(agent) = get_active_agent_mut(app) {
                if let Some(ActiveModal::SessionPicker {
                    entries: Some(ref entries),
                    ref mut state,
                    ref content_results,
                    ..
                }) = agent.active_modal
                {
                    let expanded_idx = entries
                        .iter()
                        .position(|entry| entry.source == source && entry.id == session_id);
                    if let Some(idx) = expanded_idx {
                        if state.expanded.contains(&idx) {
                            state.expanded.remove(&idx);
                            return vec![];
                        }
                        state.expanded.insert(idx);
                        let entry = &entries[idx];
                        if native_source && entry.card_detail.is_none() {
                            return vec![Effect::LoadCardDetail {
                                source: entry.source.clone(),
                                session_id: entry.id.clone(),
                                cwd: entry.cwd.clone(),
                                generation: detail_generation,
                            }];
                        }
                        return vec![];
                    } else if native_source
                        && let Some(hits) = content_results.as_ref()
                        && let Some(hit_idx) = hits.iter().position(|h| h.session_id == session_id)
                    {
                        let key = CONTENT_EXPAND_OFFSET + hit_idx;
                        if state.expanded.contains(&key) {
                            state.expanded.remove(&key);
                        } else {
                            state.expanded.insert(key);
                        }
                        return vec![];
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if from_modal {
                return vec![];
            }
            let expanded_idx = app.session_picker_entries.as_ref().and_then(|entries| {
                entries
                    .iter()
                    .position(|entry| entry.source == source && entry.id == session_id)
            });
            if let Some(idx) = expanded_idx {
                if app.session_picker_state.expanded.contains(&idx) {
                    app.session_picker_state.expanded.remove(&idx);
                    return vec![];
                }
                app.session_picker_state.expanded.insert(idx);
                if native_source
                    && let Some(entry) = app
                        .session_picker_entries
                        .as_ref()
                        .and_then(|entries| entries.get(idx))
                    && entry.card_detail.is_none()
                {
                    return vec![Effect::LoadCardDetail {
                        source: entry.source.clone(),
                        session_id: entry.id.clone(),
                        cwd: entry.cwd.clone(),
                        generation: detail_generation,
                    }];
                }
            } else if native_source
                && let Some(hits) = app.session_picker_content_results.as_ref()
                && let Some(hit_idx) = hits.iter().position(|h| h.session_id == session_id)
            {
                let key = CONTENT_EXPAND_OFFSET + hit_idx;
                if app.session_picker_state.expanded.contains(&key) {
                    app.session_picker_state.expanded.remove(&key);
                } else {
                    app.session_picker_state.expanded.insert(key);
                }
                return vec![];
            }
            vec![]
        }
        Action::SendPrompt(text) => dispatch_send_prompt(app, text),
        Action::SubmitFollowUp(text) => dispatch_send_prompt_inner(app, text, false, true, true),
        Action::SendSlashCommandPreservingDraft(text) => {
            dispatch_send_prompt_inner(app, text, false, false, false)
        }
        Action::Interject { text, images } => dispatch_interject(app, text, images),
        Action::SendPromptNow { text, images } => {
            super::interject::dispatch_send_prompt_now(app, text, images)
        }
        Action::EnableVoiceMode => dispatch_enable_voice_mode(app, true),
        Action::VoiceToggle => dispatch_voice_toggle(app),
        Action::VoiceStop => dispatch_voice_stop(app),
        Action::SendBashCommand(cmd) => dispatch_send_bash_command(app, cmd),
        Action::ShowUndoTip => dispatch_show_undo_tip(app),
        Action::ShowPlanNudge => dispatch_show_plan_nudge(app),
        Action::ShowWordSelectTip => dispatch_show_word_select_tip(app),
        Action::AcceptWordSelectTip => dispatch_accept_word_select_tip(app),
        Action::DrainQueue => dispatch_drain_queue(app),
        Action::QueueRemoveShared {
            id,
            expected_version,
        } => match active_agent_session_id(app) {
            Some(session_id) => {
                vec![Effect::QueueRemove {
                    session_id,
                    id,
                    expected_version,
                }]
            }
            None => vec![],
        },
        Action::QueueReorderShared { ordered_ids } => match active_agent_session_id(app) {
            Some(session_id) => {
                vec![Effect::QueueReorder {
                    session_id,
                    ordered_ids,
                }]
            }
            None => vec![],
        },
        Action::QueueClearShared => match active_agent_session_id(app) {
            Some(session_id) => vec![Effect::QueueClear { session_id }],
            None => vec![],
        },
        Action::QueueEditShared { id, new_text } => match active_agent_session_id(app) {
            Some(session_id) => vec![Effect::QueueEdit {
                session_id,
                id,
                new_text,
            }],
            None => vec![],
        },
        Action::QueueHoldEditShared { id } => match active_agent_session_id(app) {
            Some(session_id) => vec![Effect::QueueHoldEdit { session_id, id }],
            None => vec![],
        },
        Action::QueueReleaseEditShared { id } => match active_agent_session_id(app) {
            Some(session_id) => vec![Effect::QueueReleaseEdit { session_id, id }],
            None => vec![],
        },
        Action::QueueInterjectShared {
            id,
            expected_version,
            new_text,
        } => queue::dispatch_queue_interject_shared(app, id, expected_version, new_text),
        Action::RunEditedQueuedCommand {
            local_id,
            server,
            text,
        } => queue::dispatch_run_edited_queued_command(app, local_id, server, text),
        Action::FocusPrompt => {
            with_active_agent(app, |agent| {
                agent.set_active_pane(ActivePane::Prompt, false);
            });
            vec![]
        }
        Action::FocusScrollback => {
            with_active_agent(app, |agent| {
                agent.set_active_pane(ActivePane::Scrollback, false);
            });
            vec![]
        }
        Action::ClearPrompt => dispatch_clear_prompt(app),
        Action::OpenHistorySearch => dispatch_open_history_search(app),
        Action::OpenScrollbackSearch(query) => {
            with_active_agent(app, |agent| {
                agent.open_scrollback_search(query.as_deref());
            });
            vec![]
        }
        Action::SelectNext => {
            navigate_clearing_selection(app, |s| s.select_next());
            vec![]
        }
        Action::SelectPrev => {
            navigate_clearing_selection(app, |s| s.select_prev());
            vec![]
        }
        Action::NextTurn => {
            with_scrollback(app, |s| {
                s.next_turn();
            });
            vec![]
        }
        Action::PrevTurn => {
            with_scrollback(app, |s| {
                s.prev_turn();
            });
            vec![]
        }
        Action::NextResponse => {
            with_scrollback(app, |s| {
                s.next_response();
            });
            vec![]
        }
        Action::PrevResponse => {
            with_scrollback(app, |s| {
                s.prev_response();
            });
            vec![]
        }
        Action::GotoTop => {
            navigate_clearing_selection(app, |s| s.goto_top());
            vec![]
        }
        Action::GotoBottom => {
            navigate_clearing_selection(app, |s| s.goto_bottom());
            vec![]
        }
        Action::ScrollUp(n) => {
            with_scrollback(app, |s| s.scroll_up(n));
            vec![]
        }
        Action::ScrollDown(n) => {
            with_scrollback(app, |s| s.scroll_down(n));
            vec![]
        }
        Action::HalfPageUp => {
            navigate_clearing_selection(app, |s| s.half_page_up());
            vec![]
        }
        Action::HalfPageDown => {
            navigate_clearing_selection(app, |s| s.half_page_down());
            vec![]
        }
        Action::PageUp => {
            navigate_clearing_selection(app, |s| s.page_up());
            vec![]
        }
        Action::PageDown => {
            navigate_clearing_selection(app, |s| s.page_down());
            vec![]
        }
        Action::Collapse => {
            with_scrollback(app, |s| {
                let at_minimum = s
                    .selected()
                    .and_then(|i| s.entry(i))
                    .is_some_and(|e| e.display_mode == DisplayMode::Collapsed);
                if !at_minimum || !s.collapse_group_if_expanded() {
                    s.collapse_selected();
                }
            });
            vec![]
        }
        Action::Expand => {
            with_scrollback(app, |s| {
                if !s.toggle_group_expansion() {
                    s.expand_selected();
                }
            });
            vec![]
        }
        Action::ToggleFold => {
            with_scrollback(app, |s| {
                if !s.toggle_group_expansion() {
                    s.toggle_fold_selected();
                }
            });
            vec![]
        }
        Action::ToggleExpandAll => {
            with_scrollback(app, |s| s.toggle_expand_all());
            vec![]
        }
        Action::ExpandAllThinking => {
            with_scrollback(app, |s| s.expand_all_thinking());
            vec![]
        }
        Action::ToggleRaw => {
            with_scrollback(app, |s| s.toggle_raw_selected());
            vec![]
        }
        Action::ToggleMouseCapture => {
            crate::unified_log::info(
                "mouse_reporting_toggle.dispatch",
                None,
                Some(serde_json::json!({
                    "phase": "entered_dispatch_arm",
                })),
            );
            dispatch_toggle_mouse_capture(app);
            vec![]
        }
        Action::ToggleScrollDebugHud => {
            app.scroll_debug_hud.toggle();
            vec![]
        }
        Action::ToggleFpsHud => {
            app.fps_hud.toggle();
            vec![]
        }
        Action::ToggleScrollLog => {
            let msg = match app.scroll_state.toggle_scroll_log() {
                Some(path) => format!("scroll log: recording to {}", path.display()),
                None => "scroll log: off".to_string(),
            };
            if let Some(agent) = get_active_agent_mut(app) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(msg));
            }
            vec![]
        }
        Action::ShowDebugStatus => {
            let on = |b: bool| if b { "on" } else { "off" };
            let msg = format!(
                "debug toggles: scroll {} \u{00b7} fps {} \u{00b7} log {}. Toggle with /debug <scroll|fps|log>",
                on(app.scroll_debug_hud.enabled()),
                on(app.fps_hud.enabled()),
                on(app.scroll_state.scroll_log_active()),
            );
            if let Some(agent) = get_active_agent_mut(app) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(msg));
            }
            vec![]
        }
        Action::CopyBlockContent => {
            dispatch_copy_block_content(app);
            vec![]
        }
        Action::CopyAssistantMessage { n, file_path } => {
            dispatch_copy_assistant_message(app, n, file_path);
            vec![]
        }
        Action::ExportConversation { file_path } => {
            dispatch_export_conversation(app, file_path);
            vec![]
        }
        Action::OpenTranscriptPager => {
            dispatch_open_transcript_pager(app);
            vec![]
        }
        Action::MinimalExpandLast => {
            app.minimal_expand_last();
            vec![]
        }
        Action::CopyBlockMeta => {
            dispatch_copy_block_meta(app);
            vec![]
        }
        Action::OpenBlockViewer => {
            let mut group_toggled = false;
            with_scrollback(app, |s| group_toggled = s.toggle_group_expansion());
            if group_toggled {
                return vec![];
            }
            let mut credit_card: Option<(String, echo_agent_telemetry::events::CreditLimitChoice)> =
                None;
            with_scrollback(app, |s| {
                if let Some(idx) = s.selected()
                    && let Some(entry) = s.entry(idx)
                    && let crate::scrollback::block::RenderBlock::CreditLimit(ref blk) = entry.block
                {
                    use crate::scrollback::blocks::CreditLimitCardAction;
                    let choice = match blk.action {
                        CreditLimitCardAction::PurchaseCredits => {
                            echo_agent_telemetry::events::CreditLimitChoice::PurchaseCredits
                        }
                        CreditLimitCardAction::EnablePayg
                        | CreditLimitCardAction::IncreasePaygLimit => {
                            echo_agent_telemetry::events::CreditLimitChoice::PayAsYouGo
                        }
                    };
                    credit_card = Some((blk.url.clone(), choice));
                }
            });
            if let Some((url, choice)) = credit_card {
                log_event(echo_agent_telemetry::events::CreditLimitUpsellClicked {
                    surface: echo_agent_telemetry::events::CreditLimitUpsellSurface::InlineCard,
                    choice,
                });
                open_url_or_show(app, &url);
            } else {
                dispatch_open_block_viewer(app);
            }
            vec![]
        }
        Action::OpenExtensionsModal { tab, trigger } => {
            if app.appearance.disable_plugins {
                return vec![];
            }
            dispatch_open_extensions_modal(app, tab, trigger)
        }
        Action::OpenConfigAgentsModal(tab) => dispatch_open_config_agents_modal(app, tab),
        Action::McpAuthTrigger { server_name } => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get_mut(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::McpAuthTrigger {
                agent_id: id,
                session_id,
                server_name,
            }]
        }
        Action::McpSetupSubmit {
            server_name,
            values,
        } => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get_mut(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::McpSetupSubmit {
                agent_id: id,
                session_id,
                server_name,
                values,
            }]
        }
        Action::ReloadSkills => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get_mut(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            if let Some(ref mut modal) = agent.extensions_modal {
                modal.skills_data = crate::views::extensions_modal::TabDataState::Loading;
                modal.workflows_data = crate::views::extensions_modal::TabDataState::Loading;
            }
            vec![
                Effect::FetchSkillsList {
                    agent_id: id,
                    session_id: session_id.clone(),
                },
                Effect::FetchWorkflowsList {
                    agent_id: id,
                    session_id,
                },
            ]
        }
        Action::RefreshMcpList => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get_mut(&id) else {
                return vec![];
            };
            if let Some(ref mut modal) = agent.extensions_modal {
                modal.mcps_data = crate::views::extensions_modal::TabDataState::Loading;
            }
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::FetchMcpsList {
                agent_id: id,
                session_id,
                cache: false,
            }]
        }
        Action::ExecuteHooksAction(action) => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::HooksAction {
                agent_id: id,
                session_id,
                action,
            }]
        }
        Action::ExecutePluginsAction(action) => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::PluginsAction {
                agent_id: id,
                session_id,
                action,
            }]
        }
        Action::ExecuteMarketplaceAction(action) => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::MarketplaceAction {
                agent_id: id,
                session_id,
                action,
            }]
        }
        Action::UpsertMcpServer { name, config } => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::UpsertMcpServer {
                agent_id: id,
                session_id,
                name,
                config,
            }]
        }
        Action::DeleteMcpServer { server_name } => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::DeleteMcpServer {
                agent_id: id,
                session_id,
                server_name,
            }]
        }
        Action::ToggleSkill {
            skill_name,
            enabled,
        } => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::ToggleSkill {
                agent_id: id,
                session_id,
                skill_name,
                enabled,
            }]
        }
        Action::ToggleMcpServer {
            server_name,
            enabled,
        } => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::ToggleMcpServer {
                agent_id: id,
                session_id,
                server_name,
                enabled,
            }]
        }
        Action::ToggleMcpTool {
            server_name,
            tool_name,
            enabled,
        } => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                return vec![];
            };
            vec![Effect::ToggleMcpTool {
                agent_id: id,
                session_id,
                server_name,
                tool_name,
                enabled,
            }]
        }
        Action::NextModel => vec![],
        Action::SwitchModel { model_id, effort } => {
            let ActiveView::Agent(id) = app.active_view else {
                return vec![];
            };
            let Some(agent) = app.agents.get_mut(&id) else {
                return vec![];
            };
            let Some(session_id) = agent.session.session_id.clone() else {
                let prev_model = agent.session.models.current.clone();
                let prev_effort = agent.session.models.reasoning_effort;
                agent.session.models.set_current(model_id.clone(), effort);
                let resolved_effort = agent.session.models.reasoning_effort;
                let unchanged =
                    prev_model.as_ref() == Some(&model_id) && prev_effort == resolved_effort;
                let rollback_prev = agent
                    .session
                    .deferred_model_switch
                    .take()
                    .and_then(|prior| prior.prev_model_id)
                    .or(prev_model);
                agent.session.deferred_model_switch =
                    Some(crate::app::agent::DeferredModelSwitch {
                        model_id: model_id.clone(),
                        effort,
                        prev_model_id: rollback_prev,
                    });
                return if unchanged {
                    vec![]
                } else {
                    vec![Effect::PersistPreferredModel {
                        model_id,
                        reasoning_effort: resolved_effort,
                    }]
                };
            };
            agent.session.model_switch_pending = true;
            vec![Effect::SwitchModel {
                agent_id: id,
                session_id,
                model_id,
                effort,
                prev_model_id: None,
            }]
        }
        Action::AnnouncementsHide => {
            let shown_key = crate::views::announcements::first_session_announcement(
                &app.active_announcements,
                &app.hidden_announcement_ids,
            )
            .filter(|a| crate::views::announcements::is_dismissible(a))
            .map(echo_agent_announcements::announcement_hide_key);
            if let Some(key) = shown_key
                && app.hidden_announcement_ids.insert(key)
            {
                vec![Effect::PersistAnnouncementsHidden {
                    hidden_ids: app.hidden_announcement_ids.clone(),
                }]
            } else {
                vec![]
            }
        }
        Action::AnnouncementsShow => {
            let mut changed = false;
            for key in crate::views::announcements::session_announcement_hide_keys(
                &app.active_announcements,
            ) {
                changed |= app.hidden_announcement_ids.remove(&key);
            }
            if changed {
                vec![Effect::PersistAnnouncementsHidden {
                    hidden_ids: app.hidden_announcement_ids.clone(),
                }]
            } else {
                vec![]
            }
        }
        Action::AnnouncementsOpenCta(surface) => {
            if let Some((promo, url)) = crate::views::announcements::promo_cta_target(
                &app.active_announcements,
                &app.hidden_announcement_ids,
            ) {
                let url = url.to_owned();
                let promo_id = promo.id.clone();
                log_event(echo_agent_telemetry::events::AnnouncementCtaClicked {
                    id: promo_id,
                    source: surface,
                });
                open_url_or_show(app, &url);
            }
            vec![]
        }
        Action::CancelTurn => dispatch_cancel_turn(app),
        Action::CancelTurnChoice(choice) => dispatch_cancel_turn_choice(app, choice),
        Action::KillBgTask(task_id) => dispatch_kill_bg_task(app, task_id),
        Action::KillSubagent(subagent_id) => dispatch_kill_subagent(app, subagent_id),
        Action::CancelScheduledTask(task_id) => dispatch_cancel_scheduled_task(app, task_id),
        Action::DemoteToBackground => dispatch_demote_to_background(app),
        Action::RequestBundleStatus => vec![Effect::FetchBundleStatus],
        Action::ViewCatalogEntry { kind, name } => {
            vec![Effect::FetchCatalogEntry { kind, name }]
        }
        Action::CycleMode => dispatch_cycle_mode(app),
        Action::ShareSession => dispatch_share_session(app),
        Action::ShowSessionInfo => dispatch_show_session_info(app),
        Action::ShowReleaseNotes { title, content } => {
            dispatch_show_release_notes(app, title, content)
        }
        Action::OpenTutorial => dispatch_open_tutorial(app),
        Action::RenameSession { title } => dispatch_rename_session(app, title),
        Action::ResetSessionTitleToAuto => dispatch_reset_session_title(app),
        Action::ShowContextInfo => dispatch_show_context_info(app),
        Action::ShowUsage => dispatch_show_usage(app),
        Action::ManageBilling => dispatch_manage_billing(app),
        Action::ShowQueue => dispatch_show_queue(app),
        Action::ShowTasks => dispatch_show_tasks(app),
        Action::ShowPlan => dispatch_show_plan(app),
        Action::EnterPlanMode { description } => dispatch_enter_plan_mode(app, description),
        Action::SetPlanMode(kind) => set_plan_mode(app, kind),
        Action::OpenFeedbackPane { prefill, images } => {
            dispatch_open_feedback_pane(app, prefill, images)
        }
        Action::SendFeedback {
            text,
            images,
            trace,
        } => dispatch_send_feedback(app, text, images, trace),
        Action::EnterRememberMode => dispatch_enter_remember_mode(app),
        Action::SendRememberNote(text) => dispatch_send_remember_note(app, text),
        Action::SaveRememberNoteFromModal => dispatch_save_remember_note_from_modal(app),
        Action::SendBtw(question) => dispatch_send_btw(app, question),
        Action::SendRecap { auto } => dispatch_send_recap(app, auto),
        Action::SetCodingDataSharing { opted_in } => set_coding_data_sharing(
            app,
            opted_in,
            echo_agent_telemetry::events::CodingDataConsentSource::Settings,
        ),
        Action::ToggleYolo => dispatch_toggle_yolo(app),
        Action::ToggleMultiline => dispatch_toggle_multiline(app),
        Action::ToggleCompactMode => dispatch_toggle_compact_mode(app),
        Action::ToggleVimMode => dispatch_toggle_vim_mode(app),
        Action::SetVimMode(v) => set_vim_mode(app, v),
        Action::SetRememberToolApprovals(v) => set_remember_tool_approvals(app, v),
        Action::SetAskUserQuestionTimeoutEnabled(v) => {
            set_ask_user_question_timeout_enabled(app, v)
        }
        Action::SetKeepTextSelection(v) => set_keep_text_selection(app, v),
        Action::SetScrollSpeed(v) => set_scroll_speed(app, v),
        Action::SetScrollMode(v) => set_scroll_mode(app, v),
        Action::SetInvertScroll(v) => set_invert_scroll(app, v),
        Action::SetScrollLines(v) => set_scroll_lines(app, v),
        Action::SetShowThinkingBlocks(v) => set_show_thinking_blocks(app, v),
        Action::SetGroupToolVerbs(v) => set_group_tool_verbs(app, v),
        Action::SetCollapsedEditBlocks(v) => set_collapsed_edit_blocks(app, v),
        Action::SetPromptSuggestions(v) => set_prompt_suggestions(app, v),
        Action::SetRespectManualFolds(v) => set_respect_manual_folds(app, v),
        Action::SetDefaultSelectedPermission(s) => set_default_selected_permission(app, s),
        Action::SetHunkTrackerMode(s) => set_hunk_tracker_mode(app, s),
        Action::SetScreenMode(s) => set_screen_mode(app, s),
        Action::SetVoiceKeybindEnabled(v) => set_voice_keybind_enabled(app, v),
        Action::SetVoiceCaptureMode(s) => set_voice_capture_mode(app, s),
        Action::SetVoiceSttLanguage(s) => set_voice_stt_language(app, s),
        Action::ToggleTimestamps => dispatch_toggle_timestamps(app),
        Action::SetYoloMode(v) => set_yolo_mode(app, v),
        Action::SetPermissionMode(kind) => set_permission_mode(app, kind),
        Action::SetMultilineMode(v) => set_multiline_mode(app, v),
        Action::SetRenderMermaid(kind) => set_render_mermaid(app, kind),
        Action::SetCompactMode(v) => set_compact_mode(app, v),
        Action::SetTimestamps(v) => set_timestamps(app, v),
        Action::SetTimeline(v) => set_timeline(app, v),
        Action::SetPageFlipOnSend(v) => set_page_flip_on_send(app, v),
        Action::SetConfirmBeforeRewind(v) => set_confirm_before_rewind(app, v),
        Action::SetCombineQueuedPrompts(v) => set_combine_queued_prompts(app, v),
        Action::SetFollowUpBehavior(v) => set_follow_up_behavior(app, v),
        Action::SetSimpleMode(v) => set_simple_mode(app, v),
        Action::SetContextualHintUndo(v) => set_contextual_hint_undo(app, v),
        Action::SetContextualHintPlanMode(v) => set_contextual_hint_plan_mode(app, v),
        Action::SetContextualHintImageInput(v) => set_contextual_hint_image_input(app, v),
        Action::SetContextualHintSendNow(v) => set_contextual_hint_send_now(app, v),
        Action::SetContextualHintSmallScreen(v) => set_contextual_hint_small_screen(app, v),
        Action::SetContextualHintWordSelect(v) => set_contextual_hint_word_select(app, v),
        Action::SetContextualHintSshWrap(v) => set_contextual_hint_ssh_wrap(app, v),
        Action::SetTheme(v) => set_theme(app, v),
        Action::SetAutoDarkTheme(v) => set_auto_dark_theme(app, v),
        Action::SetAutoLightTheme(v) => set_auto_light_theme(app, v),
        Action::SetDefaultModel(v) => set_default_model(app, v),
        Action::ClearDefaultModel => clear_default_model(app),
        Action::SetForkSecondaryModel(v) => set_fork_secondary_model(app, v),
        Action::ClearForkSecondaryModel => clear_fork_secondary_model(app),
        Action::SetMaxThoughtsWidth(v) => set_max_thoughts_width(app, v),
        Action::SetShowTips(v) => set_show_tips(app, v),
        Action::SetAutoUpdate(v) => set_auto_update(app, v),
        Action::SetDisplayRefreshAutoCadence(v) => set_display_refresh_auto_cadence(app, v),
        Action::PreviewTheme(v) => preview_theme(app, v),
        Action::PreviewAutoDarkTheme(v) => preview_auto_dark_theme(app, v),
        Action::PreviewAutoLightTheme(v) => preview_auto_light_theme(app, v),
        Action::OpenSettings => dispatch_open_settings(app, None),
        Action::OpenSettingsFocus { key } => dispatch_open_settings(app, Some(key)),
        Action::PrivacyBannerOptIn => dispatch_privacy_banner_opt_in(app),
        Action::PrivacyBannerOptOut => dispatch_privacy_banner_opt_out(app),
        Action::OpenCommandPalette => dispatch_open_command_palette(app),
        Action::OpenHowtoGuides => dispatch_open_howto_guides(app),
        Action::OpenResetConfirm { key } => dispatch_open_reset_confirm(app, key),
        Action::ConfirmResetSetting { choice } => dispatch_confirm_reset_setting(app, choice),
        Action::DumpInputLog => dispatch_dump_input_log(app),
        Action::PermissionSelect(option_id) => dispatch_permission_select(app, option_id),
        Action::PermissionFollowup(text) => dispatch_permission_followup(app, text),
        Action::PermissionCancel => dispatch_permission_cancel(app),
        Action::Logout => dispatch_logout(app),
        Action::SwitchAccount => dispatch_switch_account(app),
        Action::CheckSubscription => vec![Effect::CheckSubscription { verify: None }],
        Action::OpenSuperechoagentUrl => dispatch_open_echoagent_url(app),
        Action::OpenUrl(url) => {
            if url.starts_with("file://") {
                let opened = url::Url::parse(&url)
                    .ok()
                    .and_then(|u| u.to_file_path().ok())
                    .is_some_and(|path| crate::app::link_opener::open_path(&path));
                app.show_toast(if opened {
                    "Opening in default app\u{2026}"
                } else {
                    "Could not open file"
                });
            } else {
                open_url_or_show(app, &url);
            }
            vec![]
        }
        Action::OpenLink(target) => {
            use crate::render::osc8::LinkTarget;
            match crate::render::osc8::resolve_link_open_target(&target) {
                Some(LinkTarget::File(path)) => {
                    let opened = crate::app::link_opener::open_path(&path);
                    app.show_toast(if opened {
                        "Opening in default app\u{2026}"
                    } else {
                        "Could not open file"
                    });
                }
                Some(LinkTarget::Url(url)) => {
                    crate::app::link_opener::open_url(&url);
                }
                None => {}
            }
            vec![]
        }
        Action::OpenManagedConnectors => {
            let url = crate::views::mcps_modal::managed_connectors_url(app.team_id.as_deref());
            open_url_or_show(app, &url);
            vec![]
        }
        Action::OpenNextLink => {
            with_active_agent(app, |agent| agent.cycle_highlighted_link(true));
            vec![]
        }
        Action::OpenPrevLink => {
            with_active_agent(app, |agent| agent.cycle_highlighted_link(false));
            vec![]
        }
        Action::Login => dispatch_login(app),
        Action::CancelLogin => dispatch_cancel_login(app),
        Action::SubmitAuthCode(code) => dispatch_submit_auth_code(app, code),
        Action::CopyAuthUrl => {
            dispatch_copy_auth_url(app, crate::clipboard::SystemClipboard::try_set)
        }
        Action::ShowRawAuthUrl => {
            app.auth_show_raw_url = true;
            vec![]
        }
        Action::HideRawAuthUrl => {
            app.auth_show_raw_url = false;
            vec![]
        }
        Action::TrustFolder => dispatch_trust_folder(app),
        Action::AcceptConsent => dispatch_accept_consent(app),
        Action::OpenConsentLink(index) => {
            let url = match &app.consent_state {
                ConsentState::Pending { notice, .. } => notice.links.get(index).cloned(),
                ConsentState::Done => None,
            };
            if let Some(url) = url {
                open_url_or_show(app, &url);
            }
            vec![]
        }
        Action::TriggerDeepSearch => dispatch_trigger_deep_search(app, false),
        Action::ForceDeepSearch => dispatch_trigger_deep_search(app, true),
        Action::PickContentSession { session_id, cwd } => {
            dispatch_pick_content_session(app, session_id, cwd)
        }
        Action::PickContentSessionInWorktree { session_id, cwd } => {
            dispatch_pick_content_session_in_worktree(app, session_id, cwd)
        }
        Action::DeleteSession {
            source,
            session_id,
            cwd,
        } => {
            if session_picker_external_filter_active(app) {
                return vec![];
            }
            if crate::app::foreign_sessions::is_foreign_picker_source(&source) {
                app.show_toast("External sessions can't be deleted");
                return vec![];
            }
            if source == "conversation" {
                app.show_toast("Deleting chat conversations isn't supported yet");
                return vec![];
            }
            if !matches!(source.as_str(), "local" | "remote" | "both")
                || !session_picker_entry_matches(app, &source, &session_id)
            {
                return vec![];
            }
            app.show_toast("Deleting session\u{2026}");
            vec![Effect::DeleteSession {
                source,
                session_id,
                cwd,
                after: crate::app::actions::AfterSessionDelete::Stay,
            }]
        }
        Action::Fork(args) => dispatch_fork(app, args),
        Action::ForkAnswered {
            worktree,
            directive,
            persist_mode,
        } => {
            let mut effects = dispatch_fork_resolved(app, worktree, directive);
            apply_persist_worktree_mode(
                &mut app.fork_worktree_mode,
                &mut effects,
                persist_mode,
                "fork_worktree_mode",
            );
            effects
        }
        Action::NewSessionAnswered {
            worktree,
            persist_mode,
        } => {
            let mut effects = if worktree {
                dispatch_new_worktree_session(app, None, None, None, None, None, None)
            } else {
                dispatch_new_session_inner(app, None)
            };
            apply_persist_worktree_mode(
                &mut app.new_session_worktree_mode,
                &mut effects,
                persist_mode,
                "new_session_worktree_mode",
            );
            effects
        }
        Action::DoctorFixConfirmed { target, plan } => {
            let Some(target) = super::task_result::current_doctor_target(app, &target) else {
                super::task_result::deliver_doctor_message(
                    app,
                    target.agent_id,
                    "This fix was cancelled because the session changed. Run `/doctor fix` again."
                        .to_owned(),
                );
                return vec![];
            };
            if let Some(agent) = app.agents.get_mut(&target.agent_id) {
                agent
                    .scrollback
                    .push_block(crate::scrollback::block::RenderBlock::system(format!(
                        "Applying {}…",
                        plan.id()
                    )));
            }
            vec![Effect::ApplyDoctorFix { target, plan }]
        }
        Action::DoctorFixCancelled(target) => {
            super::task_result::deliver_doctor_message(
                app,
                target.agent_id,
                "Fix cancelled.".to_owned(),
            );
            vec![]
        }
        Action::AgentTypeMismatchAnswered {
            start_new,
            model_id,
            effort,
        } => dispatch_agent_type_mismatch_answered(app, start_new, model_id, effort),
        Action::PersistMemoryFullscreen(fs) => {
            vec![Effect::PersistMemoryFullscreen { fullscreen: fs }]
        }
        Action::OpenMemoryModal => {
            if let ActiveView::Agent(id) = app.active_view
                && let Some(agent) = app.agents.get(&id)
                && let Some(session_id) = agent.session.session_id.clone()
            {
                return vec![Effect::SendPrompt {
                    agent_id: id,
                    session_id,
                    text: "/memory".to_string(),
                    prompt_id: uuid::Uuid::new_v4().to_string(),
                    skill_token_ranges: Vec::new(),
                }];
            }
            vec![]
        }
        Action::OpenGboom => dispatch_open_gboom(app),
        Action::SuspendForEditor {
            path,
            refresh_agents_modal,
        } => {
            if app.pending_editor.is_none() {
                if let ActiveView::Agent(id) = app.active_view
                    && let Some(agent) = app.agents.get_mut(&id)
                {
                    agent.active_modal = None;
                }
                app.pending_editor = Some(
                    crate::app::external_editor::PendingEditorRequest::ConfigFile {
                        path,
                        refresh_agents_modal,
                    },
                );
            }
            vec![]
        }
        Action::EditPromptExternal => super::external_editor::dispatch_edit_prompt_external(app),
        Action::OpenDashboard => dispatch_open_dashboard(app),
        Action::ExitDashboard => dispatch_exit_dashboard(app),
        Action::DashboardAttach(id) => dispatch_dashboard_attach(app, id),
        Action::DashboardDispatch { text, attach } => {
            dispatch_dashboard_dispatch(app, text, attach)
        }
        Action::DashboardDispatchSlash { text } => dispatch_dashboard_dispatch_slash(app, text),
        Action::DashboardTogglePin => dispatch_dashboard_toggle_pin(app),
        Action::DashboardBeginRename => {
            dispatch_dashboard_begin_rename(app);
            vec![]
        }
        Action::DashboardCommitRename => dispatch_dashboard_commit_rename(app),
        Action::DashboardCancelRename => {
            if let Some(d) = app.dashboard.as_mut() {
                d.rename = None;
            }
            vec![]
        }
        Action::DashboardStop => dispatch_dashboard_stop(app),
        Action::DashboardDelete => dispatch_dashboard_delete(app),
        Action::DashboardCycleMode => {
            let policy_block = app.yolo_policy_block;
            let auto_mode_gate = app.auto_mode_gate;
            if let Some(d) = app.dashboard.as_mut() {
                d.pending_mode = d.pending_mode.cycle(auto_mode_gate);
                if d.pending_mode == crate::views::dashboard::DashboardDispatchMode::AlwaysApprove
                    && let Some(warning) = policy_block
                {
                    d.pending_mode = d.pending_mode.cycle(auto_mode_gate);
                    d.set_error_toast(warning);
                }
            }
            vec![]
        }
        Action::DashboardPeekCycleMode => dispatch_dashboard_peek_cycle_mode(app),
        Action::DashboardToggleGrouping => dispatch_dashboard_toggle_grouping(app),
        Action::DashboardSetFilter(value) => {
            if let Some(d) = app.dashboard.as_mut() {
                d.filter = crate::views::dashboard::Filter::from_value(value);
                d.clear_manual_scroll();
            }
            vec![]
        }
        Action::DashboardSelectNext => {
            dispatch_dashboard_select(app, true);
            vec![]
        }
        Action::DashboardSelectPrev => {
            dispatch_dashboard_select(app, false);
            vec![]
        }
        Action::DashboardReorderUp => dispatch_dashboard_reorder(app, true),
        Action::DashboardReorderDown => dispatch_dashboard_reorder(app, false),
        Action::DashboardOverlayExit => dispatch_dashboard_overlay_exit(app),
        Action::DashboardOverlayPrev => dispatch_dashboard_overlay_cycle(app, -1),
        Action::DashboardOverlayNext => dispatch_dashboard_overlay_cycle(app, 1),
        Action::DashboardOverlayStop => dispatch_dashboard_overlay_stop(app),
        Action::DashboardToggleAutoApprove => dispatch_dashboard_toggle_auto_approve(app),
        Action::DashboardToggleWorktree => dispatch_dashboard_toggle_worktree(app),
        Action::DashboardOpenShortcutsHelp => {
            dispatch_dashboard_open_shortcuts_help(app);
            vec![]
        }
        Action::DashboardCloseShortcutsHelp => {
            if let Some(d) = app.dashboard.as_mut() {
                d.shortcuts_modal = None;
            }
            vec![]
        }
        Action::DashboardFocusNewAgentButton => {
            if let Some(d) = app.dashboard.as_mut() {
                d.focus_new_agent_button();
                d.clear_manual_scroll();
            }
            vec![]
        }
        Action::DashboardCreateNewAgentWithDetail => {
            dispatch_dashboard_create_new_agent_with_detail(app)
        }
        Action::DashboardOpenLocationPicker => dispatch_dashboard_open_location_picker(app),
        Action::DashboardCloseLocationPicker => {
            if let Some(d) = app.dashboard.as_mut() {
                if let Some(wt) = d.location_picker.as_ref().map(|lp| lp.worktree_mode) {
                    d.dispatch_worktree = wt && d.cwd_has_git_ancestor;
                }
                d.location_picker = None;
            }
            vec![]
        }
        Action::DashboardChangeLocation { input } => dispatch_dashboard_change_location(app, input),
        Action::DashboardConfirmWorktree { label } => {
            dispatch_dashboard_confirm_worktree(app, label)
        }
        Action::DashboardPermissionSelect {
            row,
            request_id,
            option_id,
        } => dispatch_dashboard_permission_select(app, row, request_id, option_id),
        Action::DashboardPermissionFollowup {
            row,
            request_id,
            text,
        } => dispatch_dashboard_permission_followup(app, row, request_id, text),
        Action::DashboardQuestionAnswer {
            row,
            option_idx,
            freeform,
        } => dispatch_dashboard_question_answer(app, row, option_idx, freeform),
        Action::DashboardPeekReply { row, text, attach } => {
            dispatch_dashboard_peek_reply(app, row, text, attach)
        }
        Action::TaskComplete(result) => dispatch_task_result(result, app),
        Action::ToggleGoalDetail => {
            with_active_agent(app, |agent| {
                if agent.goal_state.is_some() {
                    agent.show_goal_detail = !agent.show_goal_detail;
                }
            });
            vec![]
        }
        Action::ToggleWorkflows => {
            let opening = matches!(app.active_view, ActiveView::Agent(id) if app.agents.get(&id).is_some_and(|agent| !agent.show_workflows));
            if opening {
                app.scroll_state.cancel_stream();
                app.last_scroll_pos = None;
            }
            with_active_agent(app, |agent| {
                agent.show_workflows = !agent.show_workflows;
                if agent.show_workflows {
                    agent.workflows_view.reset();
                    agent.show_goal_detail = false;
                }
            });
            vec![]
        }
        Action::Rewind => dispatch_rewind(app),
        Action::RewindShowPicker => dispatch_rewind_show_picker(app),
        Action::RewindPickerSelect(prompt_index) => {
            dispatch_rewind_picker_select(app, prompt_index)
        }
        Action::RewindConfirm(target) => dispatch_rewind_confirm(app, target),
        Action::RewindConfirmNeverAsk(target) => dispatch_rewind_confirm_never_ask(app, target),
        Action::RewindCancelOffer => dispatch_rewind_cancel_offer(app),
        Action::RewindDismiss => dispatch_rewind_dismiss(app),
        Action::RewindDismissError => dispatch_rewind_dismiss_error(app),
        Action::InlineEditSubmit => dispatch_inline_edit_submit(app),
        Action::JumpShowPicker => dispatch_jump_show_picker(app),
        Action::JumpPickerSelect(turn_idx) => dispatch_jump_picker_select(app, turn_idx),
        Action::JumpDismiss => dispatch_jump_dismiss(app),
    };
    restore_stash_where_the_draft_was_consumed(app);
    app.reconcile_foreign_resume_launch();
    sync_sleep_inhibitor(app);
    effects
}
/// Drains the agent and its focused subagent: the paste drain reports on the parent while `with_active_agent` would pick the child,
/// and a stranded flag restores on a later dispatch.
fn restore_stash_where_the_draft_was_consumed(app: &mut AppView) {
    let ActiveView::Agent(id) = app.active_view else {
        return;
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return;
    };
    if let Some(child_sid) = agent.active_subagent.clone()
        && let Some(child) = agent.subagent_views.get_mut(&child_sid)
        && child.take_draft_consumed()
    {
        child.auto_restore_stash_after_send();
    }
    if agent.take_draft_consumed() {
        agent.auto_restore_stash_after_send();
    }
}
pub(super) fn dispatch_action_result(
    app: &mut AppView,
    agent_id: crate::app::agent::AgentId,
    result: Result<echo_agent_hooks_plugins_types::ActionOutcome, String>,
) -> Vec<Effect> {
    use echo_agent_hooks_plugins_types::OutcomeStatus;
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    match result {
        Err(e) => {
            if let Some(ref mut modal) = agent.extensions_modal {
                modal.pending_action = None;
                modal.pending_entry_index = None;
                modal.modal_message = Some(crate::views::extensions_modal::ModalMessage::Error(e));
            }
            vec![]
        }
        Ok(outcome) => match outcome.status {
            OutcomeStatus::Success => {
                let mut effects = Vec::new();
                if let Some(ref mut modal) = agent.extensions_modal {
                    if !outcome.message.trim().is_empty() && modal.result_notice.is_none() {
                        let entry_index = match modal.last_plugins_action {
                            Some(echo_agent_hooks_plugins_types::PluginsAction::Uninstall {
                                ..
                            }) => None,
                            _ => modal.pending_entry_index,
                        };
                        modal.result_notice =
                            Some(crate::views::extensions_modal::ActionResultNotice {
                                message: outcome.message.clone(),
                                entry_index,
                                ticks_remaining:
                                    crate::views::extensions_modal::RESULT_NOTICE_TICKS,
                            });
                    }
                    modal.pending_action = None;
                    modal.pending_entry_index = None;
                }
                if let Some(session_id) = agent.session.session_id.clone() {
                    if outcome.requires_reload {
                        effects.push(Effect::PluginsAction {
                            agent_id,
                            session_id,
                            action: echo_agent_hooks_plugins_types::PluginsAction::Reload,
                        });
                    } else if let Some(modal) = agent.extensions_modal.as_mut() {
                        effects.push(Effect::FetchHooksList {
                            agent_id,
                            session_id: session_id.clone(),
                        });
                        effects.push(Effect::FetchPluginsList {
                            agent_id,
                            session_id: session_id.clone(),
                        });
                        crate::app::dispatch::transcript::push_marketplace_fetch(
                            modal,
                            &mut effects,
                            agent_id,
                            session_id.clone(),
                        );
                        effects.push(Effect::FetchMcpsList {
                            agent_id,
                            session_id,
                            cache: false,
                        });
                    }
                }
                effects
            }
            OutcomeStatus::ConfirmationRequired => {
                if let Some(ref mut modal) = agent.extensions_modal {
                    let confirmed_action = modal.last_plugins_action.as_ref().map(|a| {
                        let mut action = a.clone();
                        if let echo_agent_hooks_plugins_types::PluginsAction::Uninstall {
                            ref mut confirmed,
                            ..
                        } = action
                        {
                            *confirmed = true;
                        }
                        action
                    });
                    if let Some(action) = confirmed_action {
                        let pending_entry_index = modal
                            .pending_entry_index
                            .or(Some(modal.picker_state.selected));
                        modal.modal_message =
                            Some(crate::views::extensions_modal::ModalMessage::Confirmation {
                                message: outcome.message,
                                action: crate::views::extensions_modal::ConfirmationAction::Plugins(
                                    action,
                                ),
                                pending_entry_index,
                            });
                        modal.picker_state.link_band = None;
                    } else {
                        modal.modal_message = Some(
                            crate::views::extensions_modal::ModalMessage::Error(outcome.message),
                        );
                    }
                }
                vec![]
            }
            OutcomeStatus::ValidationError
            | OutcomeStatus::NotFound
            | OutcomeStatus::InternalError
            | OutcomeStatus::Unsupported => {
                if let Some(ref mut modal) = agent.extensions_modal {
                    modal.pending_action = None;
                    modal.pending_entry_index = None;
                    modal.modal_message = Some(
                        crate::views::extensions_modal::ModalMessage::Error(outcome.message),
                    );
                }
                vec![]
            }
        },
    }
}
