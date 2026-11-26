//! Tests for dashboard dispatchers: attach, overlays, rows, and permissions.
use super::*;
use crate::app::app_view::InputOutcome;
use crate::app::dispatch::queue::maybe_drain_queue;
#[test]
fn voice_final_appends_to_dashboard_dispatch() {
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::AgentDashboard;
    ensure_dashboard_state(&mut app);
    app.dashboard.as_mut().unwrap().dispatch.set_text("fix");
    app.voice_state = VoiceState::Stopping {
        target: VoiceTarget::DashboardDispatch,
        interim: None,
    };
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::UtteranceFinal {
            text: "the build".into(),
        },
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().dispatch.text(),
        "fix the build"
    );
}
#[test]
fn voice_final_appends_to_peek_reply_when_peek_open() {
    use crate::views::dashboard::DashboardRowId;
    use crate::views::dashboard::peek::{PeekFields, PeekPanelState};
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::AgentDashboard;
    ensure_dashboard_state(&mut app);
    let id = AgentId(0);
    let dash = app.dashboard.as_mut().unwrap();
    dash.peek = Some(PeekPanelState::new(
        DashboardRowId::TopLevel(id),
        PeekFields {
            label: "l".into(),
            time_ago: "1m".into(),
            response_type: "Response".into(),
            last_user_message: None,
            question: None,
            options: vec![],
            request_id: None,
            reject_option: None,
        },
    ));
    dash.peek_reply.set_text("reply");
    app.voice_state = VoiceState::Stopping {
        target: VoiceTarget::DashboardPeekReply(id),
        interim: None,
    };
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::UtteranceFinal {
            text: "with voice".into(),
        },
    );
    let dash = app.dashboard.as_ref().unwrap();
    assert_eq!(
        dash.peek_reply.text(),
        "reply with voice",
        "dictation must append to the live peek reply"
    );
    assert_eq!(
        dash.dispatch.text(),
        "",
        "the hidden dispatch box must be untouched while the peek is open"
    );
}
#[test]
fn voice_final_discarded_when_peek_row_changed_after_stop() {
    use crate::views::dashboard::DashboardRowId;
    use crate::views::dashboard::peek::{PeekFields, PeekPanelState};
    let peek_for = |row: DashboardRowId| {
        PeekPanelState::new(
            row,
            PeekFields {
                label: "l".into(),
                time_ago: "1m".into(),
                response_type: "Response".into(),
                last_user_message: None,
                question: None,
                options: vec![],
                request_id: None,
                reject_option: None,
            },
        )
    };
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::AgentDashboard;
    ensure_dashboard_state(&mut app);
    app.voice_state = VoiceState::Stopping {
        target: VoiceTarget::DashboardPeekReply(AgentId(0)),
        interim: None,
    };
    app.dashboard.as_mut().unwrap().peek = Some(peek_for(DashboardRowId::TopLevel(AgentId(1))));
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::UtteranceFinal {
            text: "late words".into(),
        },
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().peek_reply.text(),
        "",
        "a final for a no-longer-peeked row must be discarded"
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn voice_dashboard_dispatch_submit_tears_down_voice() {
    let mut app = test_app();
    open_dashboard(&mut app);
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_mode_enabled = true;
    app.voice_cmd_tx = Some(tx);
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::DashboardDispatch,
        interim: Some("the build".into()),
    };
    let _ = dispatch_dashboard_dispatch(&mut app, "fix the build".into(), false);
    assert!(!app.voice_listening(), "submit stops capture");
    assert!(
        app.voice_recording_target().is_none(),
        "submit drops the target"
    );
    assert!(app.voice_interim().is_none());
    assert!(matches!(
        rx.try_recv(),
        Ok(echo_agent_voice::VoiceCommand::PttRelease)
    ));
    crate::voice::handle_voice_event(
        &mut app,
        echo_agent_voice::VoiceEvent::UtteranceFinal {
            text: "late words".into(),
        },
    );
    assert!(
        !app.dashboard
            .as_ref()
            .unwrap()
            .dispatch
            .text()
            .contains("late words"),
        "a late final must not refill the submitted dispatch box"
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn submit_cancels_pending_voice_cold_start() {
    let mut app = test_app();
    open_dashboard(&mut app);
    app.voice_mode_enabled = true;
    app.voice_ui_active = true;
    app.voice_state = VoiceState::ColdStart {
        hold: false,
        target: VoiceTarget::DashboardDispatch,
    };
    let _ = dispatch_dashboard_dispatch(&mut app, "ship it".into(), false);
    assert!(
        !app.voice_state.pending_cold_start(),
        "submit cancels the queued spawn"
    );
    assert!(app.voice_recording_target().is_none());
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn voice_dashboard_peek_reply_submit_tears_down_voice() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_mode_enabled = true;
    app.voice_cmd_tx = Some(tx);
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::DashboardPeekReply(AgentId(0)),
        interim: None,
    };
    let _ = dispatch_dashboard_peek_reply(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        "ship it".into(),
        false,
    );
    assert!(!app.voice_listening(), "peek reply submit stops capture");
    assert!(
        app.voice_recording_target().is_none(),
        "submit drops the target"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(echo_agent_voice::VoiceCommand::PttRelease)
    ));
}
#[test]
fn voice_target_bound_at_start_dispatch_vs_peek() {
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    use crate::views::dashboard::DashboardRowId;
    use crate::views::dashboard::peek::{PeekFields, PeekPanelState};
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::AgentDashboard;
    ensure_dashboard_state(&mut app);
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    app.voice_mode_enabled = true;
    app.voice_cmd_tx = Some(tx);
    dispatch(Action::EnableVoiceMode, &mut app);
    assert_eq!(
        app.voice_recording_target(),
        Some(VoiceTarget::DashboardDispatch)
    );
    app.voice_state = VoiceState::Idle;
    app.dashboard.as_mut().unwrap().peek = Some(PeekPanelState::new(
        DashboardRowId::TopLevel(AgentId(0)),
        PeekFields {
            label: "l".into(),
            time_ago: "1m".into(),
            response_type: "Response".into(),
            last_user_message: None,
            question: None,
            options: vec![],
            request_id: None,
            reject_option: None,
        },
    ));
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    app.voice_cmd_tx = Some(tx);
    dispatch(Action::EnableVoiceMode, &mut app);
    assert_eq!(
        app.voice_recording_target(),
        Some(VoiceTarget::DashboardPeekReply(AgentId(0)))
    );
}
/// Changing the peeked dashboard row while dictating into a peek reply stops
/// capture, so a final can't land on the newly-selected agent's reply.
#[test]
fn voice_auto_stops_when_peek_row_changes() {
    use crate::views::dashboard::DashboardRowId;
    use crate::views::dashboard::peek::{PeekFields, PeekPanelState};
    let peek_for = |row: DashboardRowId| {
        PeekPanelState::new(
            row,
            PeekFields {
                label: "l".into(),
                time_ago: "1m".into(),
                response_type: "Response".into(),
                last_user_message: None,
                question: None,
                options: vec![],
                request_id: None,
                reject_option: None,
            },
        )
    };
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::AgentDashboard;
    ensure_dashboard_state(&mut app);
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    app.voice_cmd_tx = Some(tx);
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::DashboardPeekReply(AgentId(0)),
        interim: None,
    };
    app.dashboard.as_mut().unwrap().peek = Some(peek_for(DashboardRowId::TopLevel(AgentId(0))));
    app.enforce_voice_session_bound();
    assert!(app.voice_listening());
    app.dashboard.as_mut().unwrap().peek = Some(peek_for(DashboardRowId::TopLevel(AgentId(1))));
    app.enforce_voice_session_bound();
    assert!(!app.voice_listening(), "row change must stop capture");
    assert!(app.voice_recording_target().is_none());
}
/// Opening the attached-agent popup hides the dashboard inputs, so dictation
/// must not bind there: starting is a no-op and an active capture auto-stops.
#[test]
fn voice_suppressed_while_dashboard_popup_open() {
    if !echo_agent_voice::AUDIO_SUPPORTED {
        return;
    }
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::AgentDashboard;
    ensure_dashboard_state(&mut app);
    app.dashboard.as_mut().unwrap().attached_agent = Some(AgentId(0));
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    app.voice_mode_enabled = true;
    app.voice_cmd_tx = Some(tx);
    dispatch(Action::EnableVoiceMode, &mut app);
    assert!(!app.voice_listening());
    assert!(app.voice_recording_target().is_none());
    app.voice_state = VoiceState::Recording {
        hold: false,
        target: VoiceTarget::DashboardDispatch,
        interim: None,
    };
    app.enforce_voice_session_bound();
    assert!(
        !app.voice_listening(),
        "popup must stop dashboard dictation"
    );
    assert!(app.voice_recording_target().is_none());
}
#[test]
fn voice_off_target_surface_does_not_enable_or_record() {
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::AgentDashboard;
    ensure_dashboard_state(&mut app);
    app.dashboard.as_mut().unwrap().attached_agent = Some(AgentId(0));
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    app.voice_mode_enabled = true;
    app.voice_cmd_tx = Some(tx);
    let agents_before = app.agents.len();
    dispatch(Action::EnableVoiceMode, &mut app);
    assert_eq!(
        app.agents.len(),
        agents_before,
        "no session spawned off-target"
    );
    assert!(!app.voice_ui_active, "voice mode must not arm off-target");
    assert!(!app.voice_listening());
    assert!(!app.voice_state.pending_cold_start());
    assert!(rx.try_recv().is_err(), "no PttPress without a target");
}
/// `echoagent dashboard` before login: the startup hook consumes the
/// `ECHO_AGENT_OPEN_DASHBOARD_AT_STARTUP` env var and stashes
/// `deferred_startup.open_dashboard`; `AuthComplete` must then open the
/// dashboard view. Regression test for the silent drop where the
/// user landed on the welcome screen / agent view instead.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn auth_complete_opens_deferred_dashboard() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };
    app.deferred_startup.open_dashboard = true;
    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: None,
        }),
        &mut app,
    );
    assert!(matches!(app.auth_state, AuthState::Done));
    assert!(
        matches!(app.active_view, ActiveView::AgentDashboard),
        "deferred `echoagent dashboard` must open the dashboard after login",
    );
    assert!(
        !app.deferred_startup.open_dashboard,
        "the deferred-dashboard flag must be consumed",
    );
}
/// Mid-session re-auth completed from the dashboard still consumes the
/// stash on the agent that 401'd (auth is global, not per-view).
#[test]
fn auth_complete_retries_stashed_prompt_from_dashboard() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let mut image = crate::prompt_images::from_clipboard_data(&crate::clipboard::ImageData {
        data: vec![1, 2, 3],
        mime_type: "image/png".into(),
    });
    image.display_number = 1;
    app.agents.get_mut(&id).unwrap().reauth_stashed_prompt =
        Some(crate::app::agent::InFlightPrompt {
            text: "retry me [Image #1]".into(),
            images: vec![image],
            scrollback_entry: crate::scrollback::EntryId::new(0),
            combined_scrollback_entries: Vec::new(),
            chip_elements: vec![crate::app::agent::ChipElement {
                range: 9..19,
                kind: crate::views::prompt_widget::KIND_IMAGE,
                display: None,
            }],
        });
    app.active_view = ActiveView::AgentDashboard;
    dispatch(Action::Login, &mut app);
    let seq = authenticating_seq(&app);
    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: seq,
            meta: None,
        }),
        &mut app,
    );
    assert_eq!(app.active_view, ActiveView::AgentDashboard);
    assert!(app.agents[&id].reauth_stashed_prompt.is_none());
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::SendPromptBlocks { .. })),
        "stash must be retried even when login returns to the dashboard, got: {effects:?}"
    );
}
/// Cancelling a mid-session re-auth from the dashboard also drops the
/// stash (auth is global, not per-view).
#[test]
fn cancel_login_from_dashboard_drops_reauth_stashed_prompt() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().reauth_stashed_prompt =
        Some(crate::app::agent::InFlightPrompt {
            text: "stale".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::EntryId::new(0),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
    app.active_view = ActiveView::AgentDashboard;
    dispatch(Action::Login, &mut app);
    dispatch(Action::CancelLogin, &mut app);
    assert!(app.agents[&id].reauth_stashed_prompt.is_none());
}
#[test]
fn resolve_location_input_expands_and_joins() {
    let cwd = PathBuf::from("/work/dir");
    assert_eq!(
        resolve_location_input("/abs/path", &cwd),
        Some(PathBuf::from("/abs/path"))
    );
    assert_eq!(
        resolve_location_input("sub/dir", &cwd),
        Some(PathBuf::from("/work/dir/sub/dir"))
    );
    let home = dirs::home_dir().expect("home dir");
    assert_eq!(resolve_location_input("~", &cwd), Some(home.clone()));
    assert_eq!(resolve_location_input("~/x", &cwd), Some(home.join("x")));
    assert_eq!(resolve_location_input("   ", &cwd), None);
}
#[test]
fn open_location_picker_without_dashboard_is_noop() {
    let mut app = test_app();
    let effects = dispatch(Action::DashboardOpenLocationPicker, &mut app);
    assert!(effects.is_empty());
    assert!(app.dashboard.is_none());
}
/// Regression: `/cd` is gated on the dashboard being the FOREGROUND view,
/// not merely existing. `app.dashboard` stays `Some` for the rest of the
/// session once opened, so a stale `is_some()` check would let `/cd <path>`
/// silently change the process cwd from an agent session. With the
/// dashboard open but an agent in front, `DashboardChangeLocation` must be
/// a no-op (cwd unchanged, no `SetWorkingDir`) even for a valid directory.
#[test]
fn dashboard_change_location_blocked_when_not_foreground() {
    let mut app = test_app();
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    app.active_view = ActiveView::Agent(AgentId(0));
    let original = PathBuf::from("/tmp");
    app.cwd = original.clone();
    let effects = dispatch(
        Action::DashboardChangeLocation {
            input: std::env::temp_dir().to_string_lossy().into_owned(),
        },
        &mut app,
    );
    assert_eq!(app.cwd, original, "cwd must be unchanged off the dashboard");
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::SetWorkingDir { .. })),
        "no SetWorkingDir when the dashboard isn't the foreground view",
    );
}
#[tokio::test]
async fn dashboard_change_location_valid_updates_cwd_and_closes_modal() {
    let mut app = test_app();
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.location_picker = Some(crate::views::dashboard::LocationPickerState::new(
            vec![],
            std::path::PathBuf::from("/base"),
            std::collections::HashMap::new(),
        ));
    }
    app.cwd = PathBuf::from("/definitely/not/a/real/dir");
    let dir = std::env::temp_dir();
    let target = dunce::canonicalize(&dir).unwrap_or(dir);
    let effects = dispatch(
        Action::DashboardChangeLocation {
            input: target.to_string_lossy().into_owned(),
        },
        &mut app,
    );
    assert_eq!(app.cwd, target);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SetWorkingDir { path } if path == &target))
    );
    assert!(
        app.dashboard.as_ref().unwrap().location_picker.is_none(),
        "successful change must close the picker"
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().dispatch.file_search.root(),
        target,
        "the dispatch box's @ file-context picker must retarget to the new cwd",
    );
}
#[tokio::test]
async fn dashboard_change_location_invalid_keeps_cwd_and_sets_error() {
    let mut app = test_app();
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.location_picker = Some(crate::views::dashboard::LocationPickerState::new(
            vec![],
            std::path::PathBuf::from("/base"),
            std::collections::HashMap::new(),
        ));
    }
    let original = PathBuf::from("/tmp");
    app.cwd = original.clone();
    let effects = dispatch(
        Action::DashboardChangeLocation {
            input: "/no/such/dir/xyz-123-not-real".into(),
        },
        &mut app,
    );
    assert_eq!(app.cwd, original, "cwd must be unchanged on a bad path");
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::SetWorkingDir { .. })),
    );
    let lp = app
        .dashboard
        .as_ref()
        .unwrap()
        .location_picker
        .as_ref()
        .expect("picker stays open on error");
    assert!(lp.error.is_some(), "an inline error must be set");
}
/// Applying a git-repo location with the picker's worktree toggle on arms
/// the dashboard's worktree dispatch so it survives the modal closing.
#[tokio::test]
async fn dashboard_change_location_persists_worktree_toggle() {
    let mut app = test_app();
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        let mut lp = crate::views::dashboard::LocationPickerState::new(
            vec![],
            std::path::PathBuf::from("/base"),
            std::collections::HashMap::new(),
        );
        lp.worktree_mode = true;
        d.location_picker = Some(lp);
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    let target = dunce::canonicalize(tmp.path()).unwrap_or_else(|_| tmp.path().to_path_buf());
    dispatch(
        Action::DashboardChangeLocation {
            input: target.to_string_lossy().into_owned(),
        },
        &mut app,
    );
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.cwd_has_git_ancestor,
        "the dashboard's git snapshot must reflect the new git cwd",
    );
    assert!(
        d.dispatch_worktree,
        "the worktree toggle must persist onto the dashboard in a git repo",
    );
}
/// Applying a NON-git location with the picker's worktree toggle on must
/// NOT arm worktree mode — worktrees require a git repo, so the dashboard
/// is never in worktree mode outside one.
#[tokio::test]
async fn dashboard_change_location_to_non_git_clears_worktree_toggle() {
    let mut app = test_app();
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        let mut lp = crate::views::dashboard::LocationPickerState::new(
            vec![],
            std::path::PathBuf::from("/base"),
            std::collections::HashMap::new(),
        );
        lp.worktree_mode = true;
        d.location_picker = Some(lp);
    }
    let tmp = tempfile::tempdir().unwrap();
    let target = dunce::canonicalize(tmp.path()).unwrap_or_else(|_| tmp.path().to_path_buf());
    dispatch(
        Action::DashboardChangeLocation {
            input: target.to_string_lossy().into_owned(),
        },
        &mut app,
    );
    let d = app.dashboard.as_ref().unwrap();
    assert!(!d.cwd_has_git_ancestor, "non-git cwd snapshot");
    assert!(
        !d.dispatch_worktree,
        "worktree mode must be forced off outside a git repo",
    );
}
/// `Ctrl+W` (DashboardToggleWorktree) flips worktree-dispatch mode on and
/// off when the cwd is a git repo.
#[test]
fn dashboard_toggle_worktree_in_git_repo_flips_flag() {
    let mut app = test_app();
    app.cwd_has_git_ancestor = true;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    assert!(!app.dashboard.as_ref().unwrap().dispatch_worktree);
    dispatch(Action::DashboardToggleWorktree, &mut app);
    assert!(
        app.dashboard.as_ref().unwrap().dispatch_worktree,
        "toggle arms worktree mode in a git repo",
    );
    dispatch(Action::DashboardToggleWorktree, &mut app);
    assert!(
        !app.dashboard.as_ref().unwrap().dispatch_worktree,
        "toggle disarms worktree mode",
    );
}
/// Outside a git repo the toggle is inert (worktrees require a repo) and
/// surfaces an explanatory toast instead of arming worktree mode.
#[test]
fn dashboard_toggle_worktree_outside_git_repo_is_noop_with_toast() {
    let mut app = test_app();
    app.cwd_has_git_ancestor = false;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    dispatch(Action::DashboardToggleWorktree, &mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        !d.dispatch_worktree,
        "worktree mode must stay off outside a git repo",
    );
    assert!(
        d.error_toast.is_some(),
        "a toast must explain why the toggle is unavailable",
    );
}
/// `[+ New Agent]` with worktree mode armed opens the worktree-label
/// dialog instead of creating a plain session (no prompt stashed).
#[test]
fn dashboard_create_new_agent_with_worktree_mode_opens_dialog() {
    let mut app = test_app();
    app.cwd_has_git_ancestor = true;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    app.dashboard.as_mut().unwrap().dispatch_worktree = true;
    let before = app.agents.len();
    let effects = dispatch_dashboard_create_new_agent_with_detail(&mut app);
    assert!(effects.is_empty(), "no session yet; the dialog opens first");
    assert_eq!(
        app.agents.len(),
        before,
        "no agent until the dialog is confirmed"
    );
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.worktree_dialog.is_some(), "worktree dialog must open");
    assert!(d.pending_worktree_prompt.is_none(), "button has no prompt");
}
/// Worktree mode armed but the cwd isn't a git repo: creating an agent
/// falls through to a normal session (no worktree dialog, no error).
#[test]
fn dashboard_create_new_agent_worktree_mode_non_repo_creates_normally() {
    let mut app = test_app();
    app.cwd_has_git_ancestor = false;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    app.dashboard.as_mut().unwrap().dispatch_worktree = true;
    let before = app.agents.len();
    let _ = dispatch_dashboard_create_new_agent_with_detail(&mut app);
    assert!(
        app.dashboard.as_ref().unwrap().worktree_dialog.is_none(),
        "no worktree dialog outside a git repo",
    );
    assert_eq!(
        app.agents.len(),
        before + 1,
        "a normal session is created instead",
    );
}
/// A prompt-send with worktree mode armed stashes the prompt and opens
/// the worktree-label dialog (no session yet).
#[test]
fn dashboard_dispatch_with_worktree_mode_stashes_prompt_and_opens_dialog() {
    let mut app = test_app();
    app.cwd_has_git_ancestor = true;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    app.dashboard.as_mut().unwrap().dispatch_worktree = true;
    let before = app.agents.len();
    let effects = dispatch_dashboard_dispatch(&mut app, "fix the bug".into(), false);
    assert!(effects.is_empty(), "no session yet; the dialog opens first");
    assert_eq!(
        app.agents.len(),
        before,
        "no agent until the dialog is confirmed"
    );
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.worktree_dialog.is_some(), "worktree dialog must open");
    assert_eq!(
        d.pending_worktree_prompt
            .as_ref()
            .map(|prompt| prompt.text.as_str()),
        Some("fix the bug")
    );
    assert!(
        !d.pending_worktree_attach,
        "a plain Enter prompt-send stashes attach=false (stay on dashboard)",
    );
}
/// Confirming the worktree dialog with `attach` set (Ctrl+S / the
/// `[+ New Agent]` button) creates a worktree session, replays the stashed
/// prompt, and opens the new agent as the dashboard's detail view.
#[test]
fn dashboard_confirm_worktree_creates_session_with_prompt() {
    let mut app = test_app_with_agent();
    app.cwd_has_git_ancestor = true;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.dispatch.set_text("hello wt");
        d.pending_worktree_prompt = Some(d.dispatch.stash());
        d.pending_worktree_attach = true;
    }
    let effects = dispatch_dashboard_confirm_worktree(&mut app, Some("my-wt".into()));
    let wt_id = effects
        .iter()
        .find_map(|e| match e {
            Effect::CreateWorktreeSession {
                agent_id, label, ..
            } => {
                assert_eq!(label.as_deref(), Some("my-wt"));
                Some(*agent_id)
            }
            _ => None,
        })
        .expect("expected a CreateWorktreeSession effect");
    assert_eq!(
        app.agents[&wt_id].session.queue_len(),
        1,
        "the stashed prompt must be enqueued on the worktree agent",
    );
    assert!(
        app.dashboard
            .as_ref()
            .unwrap()
            .pending_worktree_prompt
            .is_none(),
        "the stash must be consumed",
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(wt_id),
        "the worktree agent must attach to the dashboard overlay",
    );
    assert!(
        matches!(app.active_view, ActiveView::Agent(id) if id == wt_id),
        "the active view must be the new worktree agent",
    );
}
/// Confirming the worktree dialog from a plain `Enter` prompt-send
/// (`attach == false`) creates the worktree session and replays the prompt
/// but STAYS on the dashboard — no detail view, no overlay attach.
#[test]
fn dashboard_confirm_worktree_without_attach_stays_on_dashboard() {
    let mut app = test_app_with_agent();
    app.cwd_has_git_ancestor = true;
    app.active_view = ActiveView::AgentDashboard;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.dispatch.set_text("hello wt");
        d.pending_worktree_prompt = Some(d.dispatch.stash());
        d.pending_worktree_attach = false;
    }
    let effects = dispatch_dashboard_confirm_worktree(&mut app, Some("my-wt".into()));
    let wt_id = effects
        .iter()
        .find_map(|e| match e {
            Effect::CreateWorktreeSession { agent_id, .. } => Some(*agent_id),
            _ => None,
        })
        .expect("expected a CreateWorktreeSession effect");
    assert_eq!(
        app.agents[&wt_id].session.queue_len(),
        1,
        "the stashed prompt must still be enqueued on the worktree agent",
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        None,
        "a plain prompt-send must not attach the worktree agent to the overlay",
    );
    assert!(
        matches!(app.active_view, ActiveView::AgentDashboard),
        "a plain prompt-send must stay on the dashboard, got {:?}",
        app.active_view,
    );
}
/// Confirming the worktree dialog outside a git repo creates nothing and
/// surfaces a dashboard error instead.
#[test]
fn dashboard_confirm_worktree_without_git_repo_creates_nothing() {
    let mut app = test_app_with_agent();
    app.cwd_has_git_ancestor = false;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    {
        let dashboard = app.dashboard.as_mut().unwrap();
        dashboard.dispatch.set_text("fix the bug");
        dashboard.pending_worktree_prompt = Some(dashboard.dispatch.stash());
    }
    let before = app.agents.len();
    let effects = dispatch_dashboard_confirm_worktree(&mut app, None);
    assert_eq!(app.agents.len(), before, "no agent without a git repo");
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::CreateWorktreeSession { .. })),
        "no worktree session without a git repo",
    );
    let d = app.dashboard.as_ref().unwrap();
    assert_eq!(
        d.dispatch.text(),
        "fix the bug",
        "the stashed prompt must be restored to the dispatch input",
    );
    assert!(
        d.pending_worktree_prompt.is_none(),
        "the stash must be consumed",
    );
}
/// The worktree dispatch path threads the dashboard's staged `/model` +
/// `/plan` through the same way the normal path does: the model id rides on
/// the `CreateWorktreeSession` effect, the effort is stashed as a deferred
/// switch, and plan mode is deferred + optimistic. Regression — the worktree
/// branch used to drop the staged config entirely.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_confirm_worktree_applies_pending_model_and_plan() {
    let mut app = test_app();
    seed_model(&mut app, "echo-agent-4.5", "EchoAgent 4.5");
    open_dashboard(&mut app);
    app.cwd_has_git_ancestor = true;
    let model_id = acp::ModelId::new(std::sync::Arc::from("echo-agent-4.5"));
    if let Some(d) = app.dashboard.as_mut() {
        d.pending_model = Some(crate::views::dashboard::PendingDispatchModel {
            id: model_id.clone(),
            effort: Some(echo_agent_runtime::sampling::types::ReasoningEffort::High),
            display: "EchoAgent 4.5".to_string(),
        });
        d.pending_mode = crate::views::dashboard::DashboardDispatchMode::Plan;
        d.dispatch.set_text("do the thing");
        d.pending_worktree_prompt = Some(d.dispatch.stash());
    }
    let effects = dispatch_dashboard_confirm_worktree(&mut app, Some("my-wt".into()));
    let (wt_id, effect_model) = effects
        .iter()
        .find_map(|e| match e {
            Effect::CreateWorktreeSession {
                agent_id, model_id, ..
            } => Some((*agent_id, model_id.clone())),
            _ => None,
        })
        .expect("expected a CreateWorktreeSession effect");
    assert_eq!(
        effect_model,
        Some(model_id.clone()),
        "the staged model id must ride on the worktree effect",
    );
    let agent = &app.agents[&wt_id];
    assert_eq!(
        agent.session.deferred_model_switch,
        Some(crate::app::agent::DeferredModelSwitch {
            model_id,
            effort: Some(echo_agent_runtime::sampling::types::ReasoningEffort::High),
            prev_model_id: None,
        }),
        "effort must be stashed for the shell",
    );
    assert_eq!(
        agent.deferred_session_mode,
        Some(echo_agent_tools::types::SessionMode::Plan),
    );
    assert_eq!(agent.plan_mode_pending, Some(true));
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_confirm_worktree_carries_auto_permission_override() {
    use crate::app::actions::PermissionModeKind;
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    open_dashboard(&mut app);
    app.cwd_has_git_ancestor = true;
    if let Some(dashboard) = app.dashboard.as_mut() {
        dashboard.pending_mode = DashboardDispatchMode::Auto;
        dashboard.dispatch.set_text("do the thing");
        dashboard.pending_worktree_prompt = Some(dashboard.dispatch.stash());
        dashboard.pending_worktree_attach = true;
    }
    let effects = dispatch_dashboard_confirm_worktree(&mut app, Some("auto-wt".into()));
    let agent_id = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::CreateWorktreeSession {
                agent_id,
                permission_mode_override: Some(PermissionModeKind::Auto),
                ..
            } => Some(*agent_id),
            _ => None,
        })
        .expect("Auto must ride on the worktree create effect");
    assert!(app.agents[&agent_id].session.is_auto());
    assert_eq!(app.current_ui.permission_mode.as_deref(), Some("auto"));
    assert!(!app.default_yolo);
}
/// Images pasted into the dispatch input survive a worktree dispatch:
/// stashed when the dialog opens, replayed onto the worktree agent's queued
/// prompt on confirm. Regression — the worktree branch dropped them while
/// the normal dispatch path carried them.
#[test]
fn dashboard_confirm_worktree_replays_pasted_images() {
    let mut app = test_app_with_agent();
    app.cwd_has_git_ancestor = true;
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    let img = crate::prompt_images::from_clipboard_data(&crate::clipboard::ImageData {
        data: vec![1, 2, 3],
        mime_type: "image/png".into(),
    });
    if let Some(d) = app.dashboard.as_mut() {
        d.dispatch.set_text("look at this ");
        d.dispatch.insert_image(img).unwrap();
        d.pending_worktree_prompt = Some(d.dispatch.stash());
    }
    let effects = dispatch_dashboard_confirm_worktree(&mut app, Some("my-wt".into()));
    let wt_id = effects
        .iter()
        .find_map(|e| match e {
            Effect::CreateWorktreeSession { agent_id, .. } => Some(*agent_id),
            _ => None,
        })
        .expect("expected a CreateWorktreeSession effect");
    let entry = app.agents[&wt_id]
        .session
        .pending_prompts
        .back()
        .expect("the replayed prompt must be queued");
    assert_eq!(
        entry.images.len(),
        1,
        "pasted images must travel to the worktree agent",
    );
    assert!(entry.text.contains("[Image #1]"));
    assert_eq!(
        entry
            .chip_elements
            .iter()
            .filter(|element| element.kind == crate::views::prompt_widget::KIND_IMAGE)
            .count(),
        1,
        "worktree replay must preserve the image chip element"
    );
    assert!(
        app.dashboard
            .as_ref()
            .unwrap()
            .pending_worktree_prompt
            .is_none(),
        "the image-bearing prompt stash must be consumed",
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_image_dispatch_cancel_rewind_resends_attachment() {
    let mut app = test_app_with_agent();
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    let text = {
        let dashboard = app.dashboard.as_mut().unwrap();
        dashboard.dispatch.set_text("inspect ");
        dashboard
            .dispatch
            .insert_image(crate::prompt_images::from_clipboard_data(
                &crate::clipboard::ImageData {
                    data: vec![1, 2, 3],
                    mime_type: "image/png".into(),
                },
            ))
            .unwrap();
        dashboard.dispatch.text().to_owned()
    };
    let create = dispatch_dashboard_dispatch(&mut app, text, true);
    let new_id = create
        .iter()
        .find_map(|effect| match effect {
            Effect::CreateSession { agent_id, .. } => Some(*agent_id),
            _ => None,
        })
        .unwrap();
    {
        let agent = app.agents.get_mut(&new_id).unwrap();
        let queued = agent.session.pending_prompts.front().unwrap();
        assert_eq!(queued.images.len(), 1);
        assert_eq!(queued.chip_elements.len(), 1);
        agent.session.session_id = Some(acp::SessionId::new("dashboard-image"));
        agent.session.state = AgentState::Idle;
        assert!(matches!(
            maybe_drain_queue(agent).effects.as_slice(),
            [Effect::SendPromptBlocks { .. }]
        ));
    }
    app.active_view = ActiveView::Agent(new_id);
    let _ = dispatch(Action::CancelTurn, &mut app);
    let agent = app.agents.get(&new_id).unwrap();
    assert_eq!(agent.prompt.images.len(), 1);
    assert_eq!(
        agent
            .prompt
            .textarea()
            .elements()
            .iter()
            .filter(|element| element.kind == crate::views::prompt_widget::KIND_IMAGE)
            .count(),
        1
    );
    let restored = agent.prompt.text().to_owned();
    let resend = dispatch(Action::SendPrompt(restored), &mut app);
    assert!(matches!(
        resend.as_slice(),
        [Effect::SendPromptBlocks { .. }]
    ));
    assert_eq!(
        app.agents[&new_id]
            .session
            .in_flight_prompt
            .as_ref()
            .unwrap()
            .images
            .len(),
        1
    );
}
/// Paste-then-immediate-send race (dashboard dispatch): the same guarantee
/// for the dashboard's session-spawning input — the new session must carry
/// the pasted image even when Enter beats the deferred probe.
#[test]
fn dashboard_dispatch_send_before_paste_probe_keeps_image() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    {
        let reg = crate::actions::ActionRegistry::defaults();
        let d = app.dashboard.as_mut().unwrap();
        d.dispatch.set_text("look at this");
        let _ = d.handle_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            &reg,
        );
    }
    let ctx = app
        .dashboard
        .as_ref()
        .unwrap()
        .pending_effects
        .iter()
        .find_map(|e| match e {
            Effect::ProbeClipboardAttachment { ctx, .. } => Some(ctx.clone()),
            _ => None,
        })
        .expect("Cmd+V of an image must defer a probe");
    crate::clipboard::clear_clipboard_probe_hook();
    assert_eq!(app.dashboard.as_ref().unwrap().paste_probe_in_flight, 1);
    let before = app.agents.len();
    let effects = dispatch(
        Action::DashboardDispatch {
            text: "look at this".into(),
            attach: false,
        },
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "the dispatch must be stashed while the probe is in flight"
    );
    assert!(
        app.dashboard
            .as_ref()
            .unwrap()
            .deferred_dispatch_send
            .is_some()
    );
    assert_eq!(
        app.agents.len(),
        before,
        "no new session before the image attaches"
    );
    let pasted = crate::prompt_images::from_clipboard_data(&crate::clipboard::ImageData {
        data: vec![1, 2, 3],
        mime_type: "image/png".into(),
    });
    let _ = dispatch(
        Action::TaskComplete(TaskResult::ClipboardAttachmentProbed {
            ctx,
            image: crate::app::actions::ProbedAttachment::Image(pasted),
            file_urls: None,
        }),
        &mut app,
    );
    assert_eq!(app.dashboard.as_ref().unwrap().paste_probe_in_flight, 0);
    assert!(
        app.dashboard
            .as_ref()
            .unwrap()
            .deferred_dispatch_send
            .is_none(),
        "the stashed dispatch was consumed"
    );
    assert_eq!(
        app.agents.len(),
        before + 1,
        "the re-issued dispatch created the session"
    );
    let entry = app.agents[&AgentId(1)]
        .session
        .pending_prompts
        .back()
        .expect("the new session has a queued prompt");
    assert_eq!(
        entry.images.len(),
        1,
        "the dispatched prompt carries the pasted image"
    );
}
/// Per-surface stashes: a dispatch send AND a peek reply stashed during the
/// same probe window must both survive (the old single slot let the second
/// stash silently overwrite the first) and both re-issue on completion —
/// dispatch first, then peek.
#[test]
fn dashboard_second_stash_does_not_overwrite_first() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let row = crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0));
    let fields = crate::views::dashboard::peek::compute_peek_fields(&row, &app.agents)
        .expect("agent must be peekable");
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_row(row.clone());
        d.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
            row.clone(),
            fields,
        ));
        d.dispatch.set_text("spawn a fixer");
        d.peek_reply.set_text("please look");
    }
    crate::clipboard::set_clipboard_probe_hook(crate::clipboard::ClipboardProbeHook::with_raster(
        None,
    ));
    {
        let reg = crate::actions::ActionRegistry::defaults();
        let d = app.dashboard.as_mut().unwrap();
        let _ = d.handle_input(
            &Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            &reg,
        );
    }
    let ctx = app
        .dashboard
        .as_ref()
        .unwrap()
        .pending_effects
        .iter()
        .find_map(|e| match e {
            Effect::ProbeClipboardAttachment { ctx, .. } => Some(ctx.clone()),
            _ => None,
        })
        .expect("Cmd+V of an image must defer a probe");
    crate::clipboard::clear_clipboard_probe_hook();
    let effects = dispatch(
        Action::DashboardDispatch {
            text: "spawn a fixer".into(),
            attach: false,
        },
        &mut app,
    );
    assert!(effects.is_empty());
    let effects = dispatch(
        Action::DashboardPeekReply {
            row: row.clone(),
            text: "please look".into(),
            attach: false,
        },
        &mut app,
    );
    assert!(effects.is_empty());
    {
        let d = app.dashboard.as_ref().unwrap();
        assert!(
            d.deferred_dispatch_send.is_some() && d.deferred_peek_send.is_some(),
            "each surface keeps its own stash"
        );
    }
    let before = app.agents.len();
    let pasted = crate::prompt_images::from_clipboard_data(&crate::clipboard::ImageData {
        data: vec![1, 2, 3],
        mime_type: "image/png".into(),
    });
    let effects = dispatch(
        Action::TaskComplete(TaskResult::ClipboardAttachmentProbed {
            ctx,
            image: crate::app::actions::ProbedAttachment::Image(pasted),
            file_urls: None,
        }),
        &mut app,
    );
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.deferred_dispatch_send.is_none() && d.deferred_peek_send.is_none());
    assert_eq!(
        app.agents.len(),
        before + 1,
        "the stashed dispatch spawned its session"
    );
    let reply_sent = effects.iter().any(|e| {
        matches!(
            e,
            Effect::SendPromptBlocks { agent_id, blocks, .. }
                if *agent_id == AgentId(0)
                    && blocks.iter().any(|b| matches!(b, acp::ContentBlock::Image(_)))
        )
    });
    assert!(
        reply_sent,
        "the stashed peek reply must reach the peeked agent with the image; effects = {effects:?}"
    );
}
/// The staged-mode write sits outside `set_yolo_mode_inner`; its own
/// backstop must downgrade Always-Approve under the pin and stay the
/// identity without one.
#[test]
fn apply_pending_dispatch_config_always_approve_blocked_by_policy_pin() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    apply_pending_dispatch_config(
        agent,
        None,
        DashboardDispatchMode::AlwaysApprove,
        Some(POLICY_WARNING),
    );
    assert!(
        !agent.session.is_yolo(),
        "staged Always-Approve must not enable yolo under the pin"
    );
    assert_eq!(
        agent.toast.as_ref().map(|(s, _)| s.as_str()),
        Some(POLICY_WARNING),
    );
    apply_pending_dispatch_config(agent, None, DashboardDispatchMode::AlwaysApprove, None);
    assert!(agent.session.is_yolo());
    assert!(!agent.session.is_auto());
}
#[test]
fn apply_pending_dispatch_config_auto_sets_classifier_mode() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.session.yolo_mode = true;
    apply_pending_dispatch_config(agent, None, DashboardDispatchMode::Auto, None);
    assert!(agent.session.is_auto());
    assert!(!agent.session.is_yolo());
}
/// Dashboard per-agent toggle under the pin: refused, warning lands on
/// the dashboard's OWN error slot (the user is looking at the
/// dashboard, not the agent). OFF stays allowed.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_toggle_auto_approve_blocked_by_policy_pin() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.yolo_policy_block = Some(POLICY_WARNING);
    app.active_view = ActiveView::Welcome;
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_row(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    let effects = dispatch_dashboard_toggle_auto_approve(&mut app);
    assert!(effects.is_empty(), "blocked toggle must not emit effects");
    assert!(
        !app.agents.get(&id).unwrap().session.is_yolo(),
        "toggle ON must be refused under the pin"
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().error_toast.as_deref(),
        Some(format!("{} {POLICY_WARNING}", crate::glyphs::ballot_x()).as_str()),
        "warning must land on the dashboard error slot",
    );
    app.agents.get_mut(&id).unwrap().session.yolo_mode = true;
    let _ = dispatch_dashboard_toggle_auto_approve(&mut app);
    assert!(
        !app.agents.get(&id).unwrap().session.is_yolo(),
        "toggle OFF must still work under the pin"
    );
}
/// Shift+Tab in the peek cycles the PEEKED agent's live mode
/// (Normal → Plan) and leaves the dashboard foregrounded — the same
/// effect as Shift+Tab inside that agent's chat view.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_peek_cycle_mode_cycles_peeked_agent() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let row = crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0));
    let fields = crate::views::dashboard::peek::compute_peek_fields(&row, &app.agents)
        .expect("agent must be peekable");
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_row(row.clone());
        d.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
            row.clone(),
            fields,
        ));
    }
    assert_eq!(app.agents.get(&AgentId(0)).unwrap().plan_mode_pending, None);
    assert!(!app.agents.get(&AgentId(0)).unwrap().session.yolo_mode);
    let _ = dispatch(Action::DashboardPeekCycleMode, &mut app);
    assert_eq!(
        app.agents.get(&AgentId(0)).unwrap().plan_mode_pending,
        Some(true),
        "peek cycle must put the peeked agent into plan mode",
    );
    assert!(
        matches!(app.active_view, ActiveView::AgentDashboard),
        "peek cycle must not switch away from the dashboard, got {:?}",
        app.active_view,
    );
}
/// A dashboard-peek Shift+Tab cycles the peeked agent into plan mode but must
/// NOT attribute a plan-nudge acceptance: the user is on the dashboard, not
/// that agent's prompt, so the nudge (still within TTL) is left intact. This
/// pins that the peek routes through the telemetry-free cycle body.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_peek_cycle_does_not_retire_the_nudge() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let row = crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0));
    let fields = crate::views::dashboard::peek::compute_peek_fields(&row, &app.agents)
        .expect("agent must be peekable");
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_row(row.clone());
        d.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
            row.clone(),
            fields,
        ));
    }
    let _ = app.agents.get_mut(&AgentId(0)).unwrap().ephemeral_tip.show(
        crate::tips::plan_nudge::plan_nudge_tip(),
        &mut std::collections::HashMap::new(),
    );
    let _ = dispatch(Action::DashboardPeekCycleMode, &mut app);
    assert_eq!(
        app.agents.get(&AgentId(0)).unwrap().plan_mode_pending,
        Some(true),
        "peek cycle must still put the peeked agent into plan mode",
    );
    assert_eq!(
        app.agents
            .get(&AgentId(0))
            .unwrap()
            .ephemeral_tip
            .current_key(),
        Some(crate::tips::plan_nudge::PLAN_NUDGE_KEY),
        "the dashboard peek must not retire (or attribute) the nudge",
    );
}
#[test]
fn dashboard_open_or_merges_session_is_worktree_when_probe_is_plain() {
    let repo = crate::test_util::TempGitRepo::init("main");
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.cwd = repo.path.clone();
        agent.session.is_worktree = true;
        agent.is_worktree = false;
        agent.current_branch = Some("stale".into());
    }
    app.active_view = ActiveView::Agent(id);
    let _ = dispatch_open_dashboard(&mut app);
    let agent = &app.agents[&id];
    assert!(
        agent.is_worktree,
        "session.is_worktree must not be clobbered by a plain-repo probe"
    );
    assert_eq!(agent.current_branch.as_deref(), Some("main"));
}
#[test]
fn dashboard_open_clears_stale_agent_is_worktree_when_probe_and_session_false() {
    let repo = crate::test_util::TempGitRepo::init("main");
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.cwd = repo.path.clone();
        agent.session.is_worktree = false;
        agent.is_worktree = true;
        agent.current_branch = Some("stale".into());
    }
    app.active_view = ActiveView::Agent(id);
    let _ = dispatch_open_dashboard(&mut app);
    let agent = &app.agents[&id];
    assert!(
        !agent.is_worktree,
        "stale agent.is_worktree must clear when probe and session are false"
    );
    assert_eq!(agent.current_branch.as_deref(), Some("main"));
}
#[test]
fn dashboard_open_detects_standalone_echo_agent_worktree() {
    let main = crate::test_util::TempGitRepo::init("main-only");
    let clone = main.standalone_clone("wt-branch");
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.cwd = clone.path.clone();
        agent.session.is_worktree = false;
        agent.is_worktree = false;
        agent.current_branch = Some("main-random".into());
        agent.main_repo = None;
    }
    app.active_view = ActiveView::Agent(id);
    let _ = dispatch_open_dashboard(&mut app);
    let agent = &app.agents[&id];
    assert!(agent.is_worktree);
    assert_eq!(agent.current_branch.as_deref(), Some("wt-branch"));
    assert_eq!(
        agent.main_repo.as_deref(),
        Some(crate::test_util::collapsed_path_display(&main.path).as_str())
    );
}
/// Leader-mode independence: opening the dashboard works even when NOT in
/// leader mode. The dashboard renders local sessions regardless; leader
/// mode only adds the roster poll. Every entry point funnels through
/// `Action::OpenDashboard`, so this covers `/dashboard`, `Ctrl+\`,
/// `echoagent dashboard`, and the startup hook.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_works_without_leader() {
    let mut app = test_app_with_agent();
    app.leader_mode = false;
    app.active_view = ActiveView::Agent(AgentId(0));
    let _ = dispatch_open_dashboard(&mut app);
    assert!(
        matches!(app.active_view, ActiveView::AgentDashboard),
        "dashboard must open outside leader mode",
    );
    assert!(
        app.dashboard.is_some(),
        "dashboard state should be created outside leader mode",
    );
}
/// Build a dormant roster entry for the local idle-session tests.
fn idle_roster_entry(session_id: &str, title: &str) -> crate::app::roster::RosterEntry {
    crate::app::roster::RosterEntry {
        session_id: session_id.to_string(),
        title: Some(title.to_string()),
        cwd: "/repo".to_string(),
        is_worktree: false,
        model_id: None,
        yolo: false,
        activity: crate::app::roster::RosterActivity::Dormant,
        last_turn_summary: None,
        resident: false,
        last_change_unix_ms: 1,
        origin: crate::app::roster::RosterOrigin::default(),
    }
}
/// Without a leader there is no live roster to poll, so opening the
/// dashboard must kick off a fetch of the local on-disk idle sessions so
/// the view isn't empty.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_without_leader_fetches_local_sessions() {
    let mut app = test_app_with_agent();
    app.leader_mode = false;
    app.active_view = ActiveView::Agent(AgentId(0));
    let effects = dispatch_open_dashboard(&mut app);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchDashboardSessions)),
        "non-leader dashboard open must fetch the local idle-session list",
    );
}
/// In leader mode the live FleetView roster is the source, so opening must
/// fetch that roster immediately (not wait for the poll tick) and must NOT
/// also fetch the local on-disk list.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_with_leader_fetches_roster_not_local_sessions() {
    let mut app = test_app_with_agent();
    app.leader_mode = true;
    app.active_view = ActiveView::Agent(AgentId(0));
    let effects = dispatch_open_dashboard(&mut app);
    assert!(
        effects.iter().any(|e| matches!(e, Effect::FetchRoster)),
        "leader dashboard open must fetch the live roster immediately",
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::FetchDashboardSessions)),
        "leader dashboard open must not fetch the local on-disk list",
    );
    assert!(
        app.dashboard_sessions_loading,
        "leader open must show Loading sessions until RosterLoaded",
    );
}
#[test]
fn roster_loaded_clears_dashboard_sessions_loading() {
    let mut app = test_app();
    app.leader_mode = true;
    app.dashboard_sessions_loading = true;
    let _ = dispatch(
        Action::TaskComplete(TaskResult::RosterLoaded {
            sessions: vec![idle_roster_entry("sess-live", "Working agent")],
        }),
        &mut app,
    );
    assert!(!app.dashboard_sessions_loading);
    assert_eq!(app.leader_roster.len(), 1);
}
#[test]
fn roster_failed_clears_dashboard_sessions_loading() {
    let mut app = test_app();
    app.leader_mode = true;
    app.dashboard_sessions_loading = true;
    let _ = dispatch(
        Action::TaskComplete(TaskResult::RosterFailed {
            error: "timeout".into(),
        }),
        &mut app,
    );
    assert!(!app.dashboard_sessions_loading);
}
/// `DashboardSessionsLoaded` stores the local idle sessions, and
/// `dashboard_roster()` surfaces them when not in leader mode.
#[test]
fn dashboard_sessions_loaded_feeds_non_leader_roster() {
    let mut app = test_app();
    app.leader_mode = false;
    let _ = dispatch(
        Action::TaskComplete(TaskResult::DashboardSessionsLoaded {
            sessions: vec![idle_roster_entry("sess-idle", "Fix login")],
        }),
        &mut app,
    );
    assert_eq!(app.dashboard_local_sessions.len(), 1);
    let roster = app.dashboard_roster();
    assert_eq!(roster.len(), 1, "non-leader roster uses local sessions");
    assert_eq!(roster[0].session_id, "sess-idle");
}
/// `dashboard_roster()` switches source on `leader_mode`: the live leader
/// roster in leader mode, the local idle-session list otherwise.
#[test]
fn dashboard_roster_switches_on_leader_mode() {
    let mut app = test_app();
    app.leader_roster = vec![idle_roster_entry("leader-sess", "Leader")];
    app.dashboard_local_sessions = vec![idle_roster_entry("local-sess", "Local")];
    app.leader_mode = true;
    assert_eq!(app.dashboard_roster()[0].session_id, "leader-sess");
    app.leader_mode = false;
    assert_eq!(app.dashboard_roster()[0].session_id, "local-sess");
}
/// Seed a model into the app catalog for `/model` tests.
fn seed_model(app: &mut AppView, id: &str, name: &str) {
    let model_id = acp::ModelId::new(std::sync::Arc::from(id));
    app.models.available.insert(
        model_id.clone(),
        acp::ModelInfo::new(model_id, name.to_string()),
    );
}
/// `/model <name>` on the dashboard stages the model for the next
/// spawned agent instead of dispatching a (session-scoped) switch.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_slash_model_stages_pending_model() {
    let mut app = test_app();
    seed_model(&mut app, "echo-agent-4.5", "EchoAgent 4.5");
    open_dashboard(&mut app);
    let effects = dispatch_dashboard_dispatch_slash(&mut app, "/model echo-agent-4.5".into());
    assert!(
        effects.is_empty(),
        "staging a model must not spawn a session"
    );
    assert!(app.agents.is_empty(), "no session should be created");
    let pending = app
        .dashboard
        .as_ref()
        .unwrap()
        .pending_model
        .as_ref()
        .expect("pending_model must be set");
    assert_eq!(pending.id.0.as_ref(), "echo-agent-4.5");
    assert_eq!(pending.display, "EchoAgent 4.5");
    assert!(pending.effort.is_none());
    assert_eq!(
        app.dashboard
            .as_ref()
            .unwrap()
            .models
            .current
            .as_ref()
            .map(|id| id.0.as_ref()),
        Some("echo-agent-4.5"),
        "staging must update the snapshot's current selection",
    );
}
/// A tier-restricted command typed into the dashboard dispatch input must
/// upsell via the feedback toast — not execute, and (crucially) not fall
/// through the unknown-command path, which would spawn a session whose
/// first prompt is the raw slash text.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_slash_restricted_command_upsells_via_toast() {
    let mut app = test_app();
    app.tier_restricted_commands = vec!["imagine".to_string()];
    open_dashboard(&mut app);
    let effects = dispatch_dashboard_dispatch_slash(&mut app, "/imagine a sunset".into());
    assert!(effects.is_empty(), "restricted command must not dispatch");
    assert!(
        app.agents.is_empty(),
        "no session may be spawned for the raw slash text"
    );
    let toast = app
        .dashboard
        .as_ref()
        .unwrap()
        .error_toast
        .as_deref()
        .expect("restricted command must set the upsell toast");
    assert!(
        toast.contains("/imagine") && toast.contains("EchoAgent"),
        "toast must carry the upsell: {toast}"
    );
}
/// A slash command that fails (`CommandResult::Error`) surfaces on
/// the dashboard with the `✗` error prefix — command error strings
/// carry no glyph of their own, and the feedback badge paints the
/// toast verbatim in a neutral colour, so without the prefix an
/// error would be indistinguishable from a success message.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_slash_command_error_gets_error_glyph_prefix() {
    let mut app = test_app();
    seed_model(&mut app, "echo-agent-4.5", "EchoAgent 4.5");
    open_dashboard(&mut app);
    let effects = dispatch_dashboard_dispatch_slash(&mut app, "/model nonexistent".into());
    assert!(effects.is_empty(), "a failed command must not dispatch");
    assert!(app.agents.is_empty(), "no session should be created");
    let toast = app
        .dashboard
        .as_ref()
        .unwrap()
        .error_toast
        .as_deref()
        .expect("a failed slash command must set the feedback toast");
    assert!(
        toast.starts_with(crate::glyphs::ballot_x()),
        "command errors must carry the ✗ prefix, got: {toast:?}",
    );
    assert!(
        toast.contains("nonexistent"),
        "the command's own message must be preserved, got: {toast:?}",
    );
}
/// `/plan` on the dashboard toggles plan mode on/off.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_slash_plan_toggles_pending_plan_mode() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    open_dashboard(&mut app);
    let _ = dispatch_dashboard_dispatch_slash(&mut app, "/plan".into());
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Plan,
        "first /plan must turn plan mode on"
    );
    let _ = dispatch_dashboard_dispatch_slash(&mut app, "/plan".into());
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Normal,
        "second /plan must toggle plan mode off"
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_plan_description_transforms_snapshot_and_chip_ranges() {
    let mut app = test_app();
    open_dashboard(&mut app);
    let command = {
        let dashboard = app.dashboard.as_mut().unwrap();
        dashboard.dispatch.set_text("/plan ");
        dashboard.dispatch.set_cursor(6);
        dashboard
            .dispatch
            .handle_paste("line one\nline two\nline three\nline four");
        dashboard
            .dispatch
            .insert_image(crate::prompt_images::from_clipboard_data(
                &crate::clipboard::ImageData {
                    data: vec![1, 2, 3],
                    mime_type: "image/png".into(),
                },
            ))
            .unwrap();
        dashboard.dispatch.text().to_owned()
    };
    let _ = dispatch_dashboard_dispatch_slash(&mut app, command);
    let agent = app.agents.values().next().expect("plan session");
    let queued = agent.session.pending_prompts.back().expect("plan prompt");
    assert!(!queued.text.starts_with("/plan"));
    assert!(queued.text.starts_with("line one"));
    assert!(queued.text.contains("[Image #1]"));
    assert_eq!(queued.images.len(), 1);
    assert_eq!(queued.chip_elements.len(), 2);
    assert!(
        queued
            .chip_elements
            .iter()
            .all(|chip| chip.range.end <= queued.text.len())
    );
    assert_eq!(
        agent.deferred_session_mode,
        Some(echo_agent_tools::types::SessionMode::Plan)
    );
}
/// The sessions picker modal was removed; `/sessions` survives as an alias
/// of `/dashboard`. It must resolve to the dashboard command and inherit
/// the dashboard feature-flag gate (hidden by canonical name, fail-closed).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_slash_sessions_aliases_dashboard() {
    let mut app = three_agent_app();
    open_dashboard(&mut app);
    let dashboard = app.dashboard.as_mut().unwrap();
    let registry = dashboard.dispatch.slash_controller.registry_mut();
    registry.set_dashboard_visible(false);
    assert!(
        registry.get_for_dispatch("sessions").is_none(),
        "/sessions must inherit the dashboard feature-flag gate"
    );
    registry.set_dashboard_visible(true);
    let cmd = registry
        .get("sessions")
        .expect("/sessions must resolve as an alias of /dashboard");
    assert_eq!(
        cmd.name(),
        "dashboard",
        "/sessions must be an alias of the dashboard command"
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_does_not_advertise_or_dispatch_doctor() {
    let mut app = three_agent_app();
    open_dashboard(&mut app);
    let expected = format!(
        "{} /doctor only works in a session",
        crate::glyphs::ballot_x()
    );
    for name in [
        "doctor",
        "terminal-setup",
        "terminal-check",
        "terminal-info",
    ] {
        let command = format!("/{name}");
        let dashboard = app.dashboard.as_mut().unwrap();
        dashboard.dispatch.set_text(&command);
        dashboard.dispatch.refresh_slash(&app.models);
        assert!(
            !dashboard.dispatch.slash_snapshot().command_recognized,
            "{command} must not be advertised on the session-less dashboard"
        );
        let effects = dispatch_dashboard_dispatch_slash(&mut app, command);
        assert!(effects.is_empty(), "must not enqueue spawn effects");
        assert_eq!(app.agents.len(), 3, "must not add an agent");
        let dashboard = app.dashboard.as_ref().unwrap();
        assert_eq!(dashboard.dispatch.text(), "");
        assert_eq!(dashboard.error_toast.as_deref(), Some(expected.as_str()));
    }
}
/// External-auth hides `/usage` via `visible()`, not session-scope. Typed
/// `/usage` on the dashboard must refuse with the command's message, not
/// claim it only works in a session.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_slash_usage_hidden_for_external_auth() {
    let mut app = three_agent_app();
    app.has_external_auth_provider = true;
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta::default());
    open_dashboard(&mut app);
    let before = app.agents.len();
    let effects = dispatch_dashboard_dispatch_slash(&mut app, "/usage".into());
    assert!(effects.is_empty(), "must not enqueue spawn effects");
    assert_eq!(app.agents.len(), before, "must not add an agent");
    assert_eq!(app.dashboard.as_ref().unwrap().dispatch.text(), "");
    let toast = app
        .dashboard
        .as_ref()
        .unwrap()
        .error_toast
        .as_deref()
        .expect("error toast for gated /usage");
    assert!(
        toast.contains("/usage is not available"),
        "unexpected toast: {toast}"
    );
    assert!(
        !toast.contains("only works in a session"),
        "must not mis-label /usage as session-scoped: {toast}"
    );
    assert!(
        !toast.contains("EchoAgent"),
        "must not upsell billing on external auth: {toast}"
    );
}
/// Session-scoped Action builtins must not spawn an agent whose first
/// prompt is the slash text (registered + not offered → error toast).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_slash_fork_does_not_spawn() {
    let mut app = three_agent_app();
    open_dashboard(&mut app);
    let before = app.agents.len();
    let effects = dispatch_dashboard_dispatch_slash(&mut app, "/fork".into());
    assert!(effects.is_empty(), "must not enqueue spawn effects");
    assert_eq!(app.agents.len(), before, "must not add an agent");
    assert_eq!(app.dashboard.as_ref().unwrap().dispatch.text(), "");
    let toast = app
        .dashboard
        .as_ref()
        .unwrap()
        .error_toast
        .as_deref()
        .expect("error toast for not-offered session command");
    assert!(
        toast.contains("/fork only works in a session"),
        "unexpected toast: {toast}"
    );
}
/// Session-scoped QueueCommand builtins must also error, not spawn
/// with `/compact` as the first prompt.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_slash_compact_does_not_spawn() {
    let mut app = three_agent_app();
    open_dashboard(&mut app);
    let before = app.agents.len();
    let effects = dispatch_dashboard_dispatch_slash(&mut app, "/compact".into());
    assert!(effects.is_empty(), "must not enqueue spawn effects");
    assert_eq!(app.agents.len(), before, "must not add an agent");
    assert_eq!(app.dashboard.as_ref().unwrap().dispatch.text(), "");
    let toast = app
        .dashboard
        .as_ref()
        .unwrap()
        .error_toast
        .as_deref()
        .expect("error toast for not-offered session command");
    assert!(
        toast.contains("/compact only works in a session"),
        "unexpected toast: {toast}"
    );
}
/// Extensions / config-agents modals only mount on an agent view. From the
/// dashboard they must toast (not silently clear the dispatch input).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_slash_session_modals_toast_instead_of_noop() {
    let mut app = three_agent_app();
    open_dashboard(&mut app);
    let before = app.agents.len();
    for name in [
        "hooks",
        "plugins",
        "marketplace",
        "skills",
        "mcps",
        "config-agents",
        "personas",
    ] {
        let command = format!("/{name}");
        let expected = format!(
            "{} /{name} only works in a session. Open an agent first.",
            crate::glyphs::ballot_x()
        );
        let effects = dispatch_dashboard_dispatch_slash(&mut app, command);
        assert!(
            effects.is_empty(),
            "/{name}: must not enqueue effects, got {effects:?}"
        );
        assert_eq!(app.agents.len(), before, "/{name}: must not add an agent");
        assert!(
            matches!(app.active_view, ActiveView::AgentDashboard),
            "/{name}: must stay on dashboard"
        );
        let dashboard = app.dashboard.as_ref().unwrap();
        assert_eq!(
            dashboard.dispatch.text(),
            "",
            "/{name}: dispatch input must clear"
        );
        assert_eq!(
            dashboard.error_toast.as_deref(),
            Some(expected.as_str()),
            "/{name}: unexpected toast"
        );
        for agent in app.agents.values() {
            assert!(
                agent.extensions_modal.is_none(),
                "/{name}: extensions modal must stay closed"
            );
            assert!(
                agent.agents_modal.is_none(),
                "/{name}: agents modal must stay closed"
            );
        }
    }
}
/// Shift+Tab (`DashboardCycleMode`) rotates Normal → Plan → Auto →
/// Always-Approve → Normal when Auto is enabled.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_cycle_mode_rotates_through_modes() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    open_dashboard(&mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Normal
    );
    let _ = dispatch(Action::DashboardCycleMode, &mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Plan
    );
    let _ = dispatch(Action::DashboardCycleMode, &mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Auto
    );
    let _ = dispatch(Action::DashboardCycleMode, &mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::AlwaysApprove
    );
    let _ = dispatch(Action::DashboardCycleMode, &mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Normal
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_cycle_mode_skips_auto_when_gated_off() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    app.auto_mode_gate = false;
    open_dashboard(&mut app);
    let _ = dispatch(Action::DashboardCycleMode, &mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Plan
    );
    let _ = dispatch(Action::DashboardCycleMode, &mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::AlwaysApprove
    );
    let _ = dispatch(Action::DashboardCycleMode, &mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Normal
    );
}
/// Under the managed-policy pin the staged-mode cycle still visits Auto,
/// then skips Always-Approve and explains why.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_cycle_mode_skips_always_approve_under_policy_pin() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    open_dashboard(&mut app);
    app.yolo_policy_block = Some(POLICY_WARNING);
    let _ = dispatch(Action::DashboardCycleMode, &mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Plan
    );
    let _ = dispatch(Action::DashboardCycleMode, &mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Auto
    );
    let _ = dispatch(Action::DashboardCycleMode, &mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert_eq!(
        d.pending_mode,
        DashboardDispatchMode::Normal,
        "Always-Approve must be skipped under the pin"
    );
    assert_eq!(
        d.error_toast.as_deref(),
        Some(format!("{} {POLICY_WARNING}", crate::glyphs::ballot_x()).as_str()),
    );
}
/// Opening the dashboard re-seeds BOTH staged dispatch fields: a model
/// staged in a previous session is cleared and the mode is reset from
/// the app-wide permission mode, so a fresh open never inherits stale staging.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_reseeds_pending_model_and_mode() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    seed_model(&mut app, "echo-agent-4.5", "EchoAgent 4.5");
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.pending_model = Some(crate::views::dashboard::PendingDispatchModel {
            id: acp::ModelId::new(std::sync::Arc::from("echo-agent-4.5")),
            effort: None,
            display: "EchoAgent 4.5".to_string(),
        });
        d.pending_mode = DashboardDispatchMode::Plan;
    }
    app.active_view = ActiveView::Welcome;
    open_dashboard(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.pending_model.is_none(),
        "re-open must clear a previously staged model",
    );
    assert_eq!(
        d.pending_mode,
        DashboardDispatchMode::Normal,
        "re-open must reset the mode from the app-wide permission mode",
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_seeds_auto_from_app_permission_mode() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    app.current_ui.permission_mode = Some("auto".into());
    open_dashboard(&mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Auto
    );
    app.active_view = ActiveView::Welcome;
    app.auto_mode_gate = false;
    open_dashboard(&mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Normal,
        "a gated-off Auto default must seed Normal"
    );
}
/// Peek status follows the LIVE turn activity while running: a turn that's
/// running with no live activity (e.g. permission just granted, waiting for
/// tool results) reports "Working" even though a prior agent message is the
/// newest scrollback block — never the stale "Response". When the turn goes
/// idle the same message becomes the final "Response". An actively
/// streaming message (tracker `Responding`) reads "Response".
#[test]
fn extract_response_type_running_no_activity_is_working() {
    use crate::acp::meta::NotificationMeta;
    use crate::app::agent::AgentState;
    use crate::scrollback::block::RenderBlock;
    use crate::views::dashboard::peek::extract_last_response_type;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::agent_message("prior response"));
    agent.session.state = AgentState::TurnRunning;
    assert!(agent.session.turn_activity().is_none());
    assert_eq!(extract_last_response_type(agent), "Working");
    agent.session.state = AgentState::Idle;
    assert_eq!(extract_last_response_type(agent), "Response");
    let chunk = acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
        acp::ContentBlock::Text(acp::TextContent::new("streaming…".to_string())),
    ));
    let meta = NotificationMeta::default();
    let (session, scrollback) = (&mut agent.session, &mut agent.scrollback);
    session.handle_update(chunk, &meta, scrollback);
    agent.session.state = AgentState::TurnRunning;
    assert!(matches!(
        agent.session.turn_activity(),
        Some(crate::acp::tracker::TurnActivity::Responding)
    ));
    assert_eq!(extract_last_response_type(agent), "Response");
}
/// Peek status: when a tool is actively running (`turn_activity()` is
/// `ToolRunning`) the live activity overrides the scrollback scan — even a
/// still-streaming agent message that's the newest block reports "Working"
/// rather than the stale "Response", because the agent has moved on to the
/// (granted) tool whose own block isn't the newest entry yet.
#[test]
fn extract_response_type_tool_running_overrides_stale_response() {
    use crate::acp::meta::NotificationMeta;
    use crate::app::agent::AgentState;
    use crate::scrollback::block::RenderBlock;
    use crate::views::dashboard::peek::extract_last_response_type;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.state = AgentState::TurnRunning;
    let update = acp::SessionUpdate::ToolCall(
        acp::ToolCall::new(
            acp::ToolCallId::new(std::sync::Arc::from("t1")),
            "ls -la".to_string(),
        )
        .kind(acp::ToolKind::Execute)
        .status(acp::ToolCallStatus::Pending)
        .content(vec![])
        .locations(vec![]),
    );
    let meta = NotificationMeta::default();
    let (session, scrollback) = (&mut agent.session, &mut agent.scrollback);
    session.handle_update(update, &meta, scrollback);
    assert!(
        matches!(
            agent.session.turn_activity(),
            Some(crate::acp::tracker::TurnActivity::ToolRunning { .. })
        ),
        "a pending tool must make turn_activity() ToolRunning",
    );
    agent
        .scrollback
        .push_block(RenderBlock::agent_message("streaming reply"));
    agent.scrollback.set_last_running(true);
    assert_eq!(extract_last_response_type(agent), "Working");
}
/// Always-Approve mode makes the next spawned agent auto-approve.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_always_approve_sets_yolo() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.pending_mode = DashboardDispatchMode::AlwaysApprove;
    }
    let effects = dispatch_dashboard_dispatch(&mut app, "do the thing".into(), false);
    let new_id = *app.agents.keys().next().unwrap();
    assert!(
        app.agents[&new_id].session.is_yolo(),
        "Always-Approve must spawn the agent in auto-approve"
    );
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::CreateSession {
            permission_mode_override: Some(crate::app::actions::PermissionModeKind::AlwaysApprove),
            ..
        }
    )));
    assert!(!app.default_yolo, "per-spawn mode must not change globals");
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_auto_sets_classifier_without_changing_globals() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    app.current_ui.permission_mode = Some("ask".into());
    open_dashboard(&mut app);
    app.dashboard.as_mut().unwrap().pending_mode = DashboardDispatchMode::Auto;
    let effects = dispatch_dashboard_dispatch(&mut app, "do the thing".into(), false);
    let new_id = *app.agents.keys().next().unwrap();
    assert!(app.agents[&new_id].session.is_auto());
    assert!(!app.agents[&new_id].session.is_yolo());
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::CreateSession {
            permission_mode_override: Some(crate::app::actions::PermissionModeKind::Auto),
            ..
        }
    )));
    assert_eq!(app.current_ui.permission_mode.as_deref(), Some("ask"));
    assert!(!app.default_yolo);
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_auto_attach_syncs_mirror_and_new_inherits_auto() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    app.current_ui.permission_mode = Some("ask".into());
    open_dashboard(&mut app);
    app.dashboard.as_mut().unwrap().pending_mode = DashboardDispatchMode::Auto;
    let _ = dispatch_dashboard_dispatch(&mut app, "do the thing".into(), true);
    assert_eq!(app.current_ui.permission_mode.as_deref(), Some("auto"));
    let _ = dispatch_new_session_inner(&mut app, None);
    assert!(
        app.agents[&AgentId(1)].session.is_auto(),
        "/new must inherit the active attached agent's Auto mirror"
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_stale_auto_degrades_when_gate_is_off() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    open_dashboard(&mut app);
    app.dashboard.as_mut().unwrap().pending_mode = DashboardDispatchMode::Auto;
    app.auto_mode_gate = false;
    let effects = dispatch_dashboard_dispatch(&mut app, "do the thing".into(), false);
    let new_id = *app.agents.keys().next().unwrap();
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Normal
    );
    assert!(!app.agents[&new_id].session.is_auto());
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::CreateSession {
            permission_mode_override: Some(crate::app::actions::PermissionModeKind::Ask),
            ..
        }
    )));
}
#[test]
fn auto_gate_kill_switch_clears_staged_dashboard_auto() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    open_dashboard(&mut app);
    app.dashboard.as_mut().unwrap().pending_mode = DashboardDispatchMode::Auto;
    app.auto_mode_gate = false;
    downgrade_displayed_auto_if_gated(&mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().pending_mode,
        DashboardDispatchMode::Normal
    );
}
/// Always-Approve staged but pinned off: plain-Send (stays on the
/// dashboard) must clamp yolo off AND surface the warning on the
/// dashboard's OWN error slot — the new agent's toast is invisible here.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_always_approve_blocked_warns_on_dashboard() {
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    app.yolo_policy_block = Some(POLICY_WARNING);
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.pending_mode = DashboardDispatchMode::AlwaysApprove;
    }
    let _ = dispatch_dashboard_dispatch(&mut app, "do the thing".into(), false);
    let new_id = *app.agents.keys().next().unwrap();
    assert!(
        !app.agents[&new_id].session.is_yolo(),
        "pinned always-approve must spawn the agent in Normal"
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().error_toast.as_deref(),
        Some(format!("{} {POLICY_WARNING}", crate::glyphs::ballot_x()).as_str()),
        "warning must land on the dashboard error slot when the view stays on the dashboard",
    );
}
/// A freshly dispatched agent (queued prompt, session not yet created)
/// classifies as `Working` — so it lands in the Working group right away
/// — and keeps the prompt preview as its title rather than a session-id
/// fallback.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_new_agent_is_working_with_prompt_title() {
    use crate::views::dashboard::row::{build_rows, classify_top_level};
    use crate::views::dashboard::{DashboardRowId, Filter, Grouping, RowState};
    let mut app = test_app();
    open_dashboard(&mut app);
    let _ = dispatch_dashboard_dispatch(&mut app, "fix the login bug".into(), false);
    let new_id = *app.agents.keys().next().unwrap();
    assert_eq!(
        classify_top_level(&app.agents[&new_id]),
        RowState::Working,
        "a queued-prompt agent must classify as Working",
    );
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        Grouping::State,
        &Filter::None,
        None,
    );
    let row = rows
        .iter()
        .find(|r| r.id == DashboardRowId::TopLevel(new_id))
        .expect("new agent row");
    assert_eq!(row.state, RowState::Working);
    assert_eq!(row.label, "fix the login bug");
}
/// A staged model + plan mode are applied to the agent spawned by the
/// next dispatch: the model id threads into `CreateSession`, the effort
/// is stashed as a deferred switch, and plan mode is deferred + optimistic.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_applies_pending_model_and_plan() {
    let mut app = test_app();
    seed_model(&mut app, "echo-agent-4.5", "EchoAgent 4.5");
    open_dashboard(&mut app);
    let model_id = acp::ModelId::new(std::sync::Arc::from("echo-agent-4.5"));
    if let Some(d) = app.dashboard.as_mut() {
        d.pending_model = Some(crate::views::dashboard::PendingDispatchModel {
            id: model_id.clone(),
            effort: Some(echo_agent_runtime::sampling::types::ReasoningEffort::High),
            display: "EchoAgent 4.5".to_string(),
        });
        d.pending_mode = crate::views::dashboard::DashboardDispatchMode::Plan;
    }
    let effects = dispatch_dashboard_dispatch(&mut app, "do the thing".into(), false);
    assert_eq!(app.agents.len(), 1);
    let new_id = *app.agents.keys().next().unwrap();
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::CreateSession { model_id: Some(m), .. } if *m == model_id
    )));
    let agent = &app.agents[&new_id];
    assert_eq!(
        agent.session.deferred_model_switch,
        Some(crate::app::agent::DeferredModelSwitch {
            model_id,
            effort: Some(echo_agent_runtime::sampling::types::ReasoningEffort::High),
            prev_model_id: None,
        }),
        "effort must be stashed for the shell"
    );
    assert_eq!(
        agent.deferred_session_mode,
        Some(echo_agent_tools::types::SessionMode::Plan),
    );
    assert_eq!(agent.plan_mode_pending, Some(true));
}
/// The `[+ New Agent]` button path (`DashboardCreateNewAgentWithDetail`,
/// no queued prompt) applies the same staged model + mode as the dispatch
/// path: the model id threads into `CreateSession`, the effort is stashed
/// as a deferred switch, and plan mode is deferred + optimistic.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_new_agent_button_applies_pending_model_and_plan() {
    let mut app = test_app();
    seed_model(&mut app, "echo-agent-4.5", "EchoAgent 4.5");
    open_dashboard(&mut app);
    let model_id = acp::ModelId::new(std::sync::Arc::from("echo-agent-4.5"));
    if let Some(d) = app.dashboard.as_mut() {
        d.pending_model = Some(crate::views::dashboard::PendingDispatchModel {
            id: model_id.clone(),
            effort: Some(echo_agent_runtime::sampling::types::ReasoningEffort::High),
            display: "EchoAgent 4.5".to_string(),
        });
        d.pending_mode = crate::views::dashboard::DashboardDispatchMode::Plan;
    }
    let effects = dispatch(Action::DashboardCreateNewAgentWithDetail, &mut app);
    assert_eq!(app.agents.len(), 1);
    let new_id = *app.agents.keys().next().unwrap();
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::CreateSession { model_id: Some(m), .. } if *m == model_id
    )));
    let agent = &app.agents[&new_id];
    assert_eq!(
        agent.session.deferred_model_switch,
        Some(crate::app::agent::DeferredModelSwitch {
            model_id,
            effort: Some(echo_agent_runtime::sampling::types::ReasoningEffort::High),
            prev_model_id: None,
        }),
        "effort must be stashed for the shell"
    );
    assert_eq!(
        agent.deferred_session_mode,
        Some(echo_agent_tools::types::SessionMode::Plan),
    );
    assert_eq!(agent.plan_mode_pending, Some(true));
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_new_agent_button_carries_auto_permission_override() {
    use crate::app::actions::PermissionModeKind;
    use crate::views::dashboard::DashboardDispatchMode;
    let mut app = test_app();
    open_dashboard(&mut app);
    app.dashboard.as_mut().unwrap().pending_mode = DashboardDispatchMode::Auto;
    let effects = dispatch(Action::DashboardCreateNewAgentWithDetail, &mut app);
    let new_id = *app.agents.keys().next().unwrap();
    assert!(app.agents[&new_id].session.is_auto());
    assert_eq!(app.current_ui.permission_mode.as_deref(), Some("auto"));
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::CreateSession {
            permission_mode_override: Some(PermissionModeKind::Auto),
            ..
        }
    )));
}
/// The deferred plan `SessionMode` is emitted (and cleared) once the
/// session exists, mirroring the deferred model switch.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_deferred_plan_mode_applied_on_session_created() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let session_id: acp::SessionId = "new-session".into();
    app.agents.get_mut(&id).unwrap().session.session_id = None;
    app.agents.get_mut(&id).unwrap().deferred_session_mode =
        Some(echo_agent_tools::types::SessionMode::Plan);
    let effects = dispatch(
        Action::TaskComplete(TaskResult::SessionCreated {
            agent_id: id,
            session_id: session_id.clone(),
            models: None,
            scheduler_background_loops: None,
        }),
        &mut app,
    );
    assert!(
        app.agents[&id].deferred_session_mode.is_none(),
        "deferred mode must be consumed"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SetSessionMode { session_id: s, .. } if *s == session_id)),
        "SessionCreated must emit SetSessionMode for the deferred plan mode"
    );
}
/// Any non-empty prompt — even a single character — dispatches a
/// new session (the old 4-char floor was relaxed to 1 char).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_single_char_creates_session() {
    let mut app = test_app();
    open_dashboard(&mut app);
    let effects = dispatch_dashboard_dispatch(&mut app, "x".into(), false);
    assert!(!effects.is_empty(), "a single-char prompt must dispatch");
    assert_eq!(
        app.agents.len(),
        1,
        "single-char prompt must create a session"
    );
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.error_toast.is_none(),
        "no error toast for a non-empty prompt"
    );
}
/// A normal prompt creates a session.
///
/// The dispatch path does not auto-select the freshly created row —
/// selection stays where the user left it (None in this empty-state
/// test). Selection is the overview navigation cursor, kept distinct
/// from the freshly-spawned agent.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_prompt_creates_session() {
    let mut app = test_app();
    open_dashboard(&mut app);
    let effects = dispatch_dashboard_dispatch(&mut app, "test prompt".into(), false);
    assert!(!effects.is_empty());
    assert_eq!(app.agents.len(), 1);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.selected.is_none(),
        "post-dispatch selection must stay None to preserve the new-session path, \
             got {:?}",
        d.selected,
    );
}
/// An empty / whitespace-only prompt is rejected — there's no task
/// to seed the new session, so no agent is created.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_empty_prompt_rejected() {
    let mut app = test_app();
    open_dashboard(&mut app);
    let _ = dispatch_dashboard_dispatch(&mut app, "   ".into(), false);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.error_toast.is_some(),
        "empty prompt must set the error toast"
    );
    assert!(
        app.agents.is_empty(),
        "no agent created for an empty prompt"
    );
}
/// The dispatch input ALWAYS spawns a new session — even when a
/// top-level row is selected. The selection is the overview
/// navigation cursor, NOT a reply target; conflating the two
/// trapped the user "stuck replying to the same agent". To talk to
/// an existing agent the user opens it (navigate + Enter) and
/// replies inside its own view.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_with_top_level_selection_creates_new_session() {
    let mut app = test_app_with_agent();
    let selected = AgentId(0);
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(selected));
    }
    let before = app.agents.len();
    let queue_before = app.agents[&selected].session.pending_prompts.len();
    let _ = dispatch_dashboard_dispatch(&mut app, "spawn a brand new agent".into(), false);
    assert_eq!(
        app.agents.len(),
        before + 1,
        "dispatch with a selected row must create a NEW session, not reply",
    );
    assert_eq!(
        app.agents[&selected].session.pending_prompts.len(),
        queue_before,
        "the selected agent's queue must be untouched (no reply enqueued)",
    );
    assert!(
        matches!(app.active_view, ActiveView::AgentDashboard),
        "plain Enter stays on the dashboard, got {:?}",
        app.active_view,
    );
}
/// 2 — Button focused + non-empty + Enter → new session,
/// STAY on the dashboard, no attached_agent.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_enter_button_focused_with_text_creates_and_stays() {
    let mut app = test_app();
    open_dashboard(&mut app);
    assert!(app.dashboard.as_ref().unwrap().new_agent_button_focused);
    let agents_before = app.agents.len();
    let _ = dispatch_dashboard_dispatch(&mut app, "kick off a fresh session".into(), false);
    assert_eq!(
        app.agents.len(),
        agents_before + 1,
        "Enter on button + text must spawn a new agent",
    );
    assert!(
        matches!(app.active_view, ActiveView::AgentDashboard),
        "Enter on button + text must STAY on the dashboard, got {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        None,
        "Enter (no Shift) must NOT set attached_agent",
    );
}
/// 3 — Button focused + non-empty + Ctrl+S → new
/// session AND open detail AND set attached_agent so the
/// overlay chrome paints. Was broken before the new-session
/// attach path got the `attached_agent` write.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_ctrl_s_button_focused_with_text_creates_and_opens() {
    let mut app = test_app();
    open_dashboard(&mut app);
    assert!(app.dashboard.as_ref().unwrap().new_agent_button_focused);
    let _ = dispatch_dashboard_dispatch(&mut app, "kick off and open".into(), true);
    let new_id = *app.agents.keys().last().unwrap();
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == new_id),
        "Ctrl+S on button + text must switch to the new agent's view, \
             got {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(new_id),
        "Ctrl+S must set attached_agent so the overlay chrome paints",
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().selected,
        Some(crate::views::dashboard::DashboardRowId::TopLevel(new_id)),
        "Ctrl+S must snap selection onto the new row (overlay anchor)",
    );
    assert!(
        !app.dashboard.as_ref().unwrap().new_agent_button_focused,
        "selection on the new row implies the button is no longer focused",
    );
}
/// 4 — Row selected + empty prompt + Enter → open detail
/// (no send). Emitted as `DashboardAttach` from the state
/// handler, which the dispatcher routes through
/// `dispatch_dashboard_attach` (sets attached_agent +
/// switches view).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_enter_row_selected_empty_prompt_opens_detail() {
    let mut app = test_app_with_agent();
    let target = AgentId(0);
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_row(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let queue_before = app.agents[&target].session.pending_prompts.len();
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(target),
    );
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == target),
        "Enter on row + empty prompt must open detail, got {:?}",
        app.active_view,
    );
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(target),);
    assert_eq!(
        app.agents[&target].session.pending_prompts.len(),
        queue_before,
        "Enter + empty prompt must NOT enqueue anything",
    );
}
/// Selecting a SUBAGENT row falls through to the
/// new-session path. Subagents have no user prompt channel,
/// so "reply" doesn't apply. Belt-and-braces: a future
/// regression that broadened the reply target match to all
/// `DashboardRowId` variants would hijack the new-session
/// path here.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_with_subagent_selection_creates_new_session() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::Subagent {
            parent: AgentId(0),
            child_session_id: "child-1".into(),
        });
    }
    let agents_before = app.agents.len();
    let _ = dispatch_dashboard_dispatch(&mut app, "a fresh task for a new agent".into(), false);
    assert_eq!(
        app.agents.len(),
        agents_before + 1,
        "subagent selection must NOT redirect dispatch to its parent — \
             reply only applies to top-level rows",
    );
}
/// When nothing is selected, dispatch reaches the
/// new-session path AND leaves selection at None. Combined
/// with `dashboard_dispatch_4_chars_creates_session` this
/// pins the post-dispatch state contract: the user can
/// immediately press Enter again to spawn another session.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_with_no_selection_creates_new_and_leaves_selection_empty() {
    let mut app = test_app();
    open_dashboard(&mut app);
    assert!(app.dashboard.as_ref().unwrap().selected.is_none());
    let _ = dispatch_dashboard_dispatch(&mut app, "first task".into(), false);
    assert_eq!(app.agents.len(), 1);
    assert!(
        app.dashboard.as_ref().unwrap().selected.is_none(),
        "post-dispatch selection must remain None — next Enter spawns another session",
    );
}
/// Attaching a top-level row switches the whole
/// view to the agent's fullscreen view AND sets
/// `attached_agent` as the signal for the session-overlay
/// chrome (bordered frame + Prev/Next/Close).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_attach_top_level_switches_to_agent_view() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let id = AgentId(0);
    assert!(matches!(app.active_view, ActiveView::AgentDashboard));
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id),
    );
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == id),
        "expected ActiveView::Agent({id:?}), got: {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(id),
        "attached_agent must mark this view as a session overlay",
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().selected,
        Some(crate::views::dashboard::DashboardRowId::TopLevel(id)),
    );
}
/// Attach routes through `focus_row`, so a previously selected
/// section header is cleared — the row and section cursors stay
/// mutually exclusive (a bare `selected` assignment used to leave
/// both active).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_attach_clears_selected_section() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let id = AgentId(0);
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_section(crate::views::dashboard::SectionKey::State(
            crate::views::dashboard::RowState::Idle,
        ));
    }
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id),
    );
    let d = app.dashboard.as_ref().unwrap();
    assert_eq!(
        d.selected,
        Some(crate::views::dashboard::DashboardRowId::TopLevel(id)),
    );
    assert!(
        d.selected_section.is_none(),
        "attach must clear the section cursor",
    );
}
/// Attaching a subagent row switches to the
/// parent agent's view AND sets the parent's `active_subagent`
/// so the subagent's takeover is rendered immediately.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_attach_subagent_switches_to_parent_with_subagent_focused() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let parent = AgentId(0);
    let child_sid = "child-1".to_string();
    {
        let agent = app.agents.get_mut(&parent).unwrap();
        agent
            .subagent_sessions
            .insert(child_sid.clone(), make_test_subagent(&child_sid, "sa-1"));
    }
    let child_session = make_test_agent_session(&app, AgentId(1), "child-session");
    let mut child_view = AgentView::new(child_session, ScrollbackState::new());
    crate::app::agent_view::test_fixtures::add_running_execute(&mut child_view);
    assert!(!child_view.is_subagent_view);
    app.agents
        .get_mut(&parent)
        .unwrap()
        .subagent_views
        .insert(child_sid.clone(), Box::new(child_view));
    crate::app::agent_view::test_fixtures::add_running_execute(
        app.agents.get_mut(&parent).unwrap(),
    );
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::Subagent {
            parent,
            child_session_id: child_sid.clone(),
        },
    );
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == parent),
        "expected ActiveView::Agent({parent:?}), got: {:?}",
        app.active_view,
    );
    let parent_view = app.agents.get_mut(&parent).unwrap();
    assert_eq!(parent_view.active_subagent, Some(child_sid.clone()));
    assert!(parent_view.subagent_views[&child_sid].is_subagent_view);
    assert!(
        parent_view.subagent_views[&child_sid]
            .session
            .tracker
            .running_execute_tool_call_id()
            .is_some()
    );
    assert!(
        !parent_view.subagent_views[&child_sid]
            .current_shortcut_hints(&app.registry, false)
            .iter()
            .any(|hint| hint.label == "send to bg")
    );
    let child = parent_view.subagent_views.get_mut(&child_sid).unwrap();
    let area = ratatui::layout::Rect::new(0, 0, 80, 30);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let mut scratch = crate::scrollback::render::ScratchBuffer::new();
    let _ = child.draw(
        area,
        &mut buf,
        &app.registry,
        &mut scratch,
        None,
        false,
        crate::app::agent_view::BannerSlotParams::none(),
        &crate::app::bundle::BundleState::default(),
        false,
        false,
        &mut Vec::new(),
        crate::app::agent_view::AppRenderParams::default(),
    );
    assert!(child.hit_bg_button.rect.is_none());
    let parent_tool = parent_view
        .session
        .tracker
        .running_execute_tool_call_id()
        .map(str::to_owned);
    let child_tool = parent_view.subagent_views[&child_sid]
        .session
        .tracker
        .running_execute_tool_call_id()
        .map(str::to_owned);
    let outcome = app.handle_input(&crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('b'),
            crossterm::event::KeyModifiers::CONTROL,
        ),
    ));
    assert!(matches!(outcome, InputOutcome::Changed));
    let parent_view = app.agents.get(&parent).unwrap();
    assert_eq!(
        parent_view
            .session
            .tracker
            .running_execute_tool_call_id()
            .map(str::to_owned),
        parent_tool
    );
    assert_eq!(
        parent_view.subagent_views[&child_sid]
            .session
            .tracker
            .running_execute_tool_call_id()
            .map(str::to_owned),
        child_tool
    );
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(parent),);
}
/// Regression: attaching to a subagent from the dashboard must lazily load
/// its (resume-deferred) transcript, just like `open_subagent_fullscreen`.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_attach_subagent_lazily_replays_deferred_transcript() {
    let child_sid = "child-dash-defer".to_string();
    let home = tempfile::tempdir().unwrap();
    let session_dir = home
        .path()
        .join("sessions")
        .join(urlencoding::encode("/tmp").as_ref())
        .join(&child_sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("summary.json"), "{}").unwrap();
    let tool_line = format!(
        r#"{{"method":"session/update","params":{{"sessionId":"{child_sid}","update":{{"sessionUpdate":"tool_call","toolCallId":"tc1","title":"Read foo","kind":"read","locations":[{{"path":"/tmp/foo"}}]}}}}}}"#
    );
    std::fs::write(session_dir.join("updates.jsonl"), tool_line + "\n").unwrap();
    crate::app::subagent::set_replay_echo_agent_home_for_tests(Some(home.path().to_path_buf()));
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let parent = AgentId(0);
    app.agents
        .get_mut(&parent)
        .unwrap()
        .subagent_sessions
        .insert(child_sid.clone(), make_test_subagent(&child_sid, "sa-1"));
    let child_session = make_test_agent_session(&app, AgentId(1), &child_sid);
    let child_view = AgentView::new(child_session, ScrollbackState::new());
    app.agents
        .get_mut(&parent)
        .unwrap()
        .insert_subagent_view(child_sid.clone(), Box::new(child_view));
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::Subagent {
            parent,
            child_session_id: child_sid.clone(),
        },
    );
    let agent = app.agents.get(&parent).unwrap();
    let child = agent.subagent_views.get(&child_sid).unwrap();
    let tool_calls = (0..child.scrollback.len())
        .filter(|i| {
            child
                .scrollback
                .entry(*i)
                .is_some_and(|e| matches!(e.block, RenderBlock::ToolCall(_)))
        })
        .count();
    assert_eq!(
        tool_calls, 1,
        "dashboard attach must lazily replay the deferred subagent transcript"
    );
    assert!(
        agent
            .subagent_sessions
            .get(&child_sid)
            .is_some_and(|i| !i.transcript.needs_replay()),
        "dashboard attach must record the child transcript state"
    );
    crate::app::subagent::set_replay_echo_agent_home_for_tests(None);
}
/// `/dashboard` opens to the dashboard view ONLY.
/// No auto-attached popup. The user reaches an agent's view by
/// pressing Enter on its row.
///
/// Opening from an agent now lands in NEW-SESSION mode (the
/// `[+ New Agent]` button focused, no row selected) so typing +
/// Enter dispatches a brand new agent. Previously the dashboard
/// pre-seeded `selected` to the came-from agent, which armed reply
/// mode and trapped the user replying to that one agent.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_does_not_auto_attach_to_focused_agent() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.active_view = ActiveView::Agent(id);
    let _ = dispatch_open_dashboard(&mut app);
    assert!(matches!(app.active_view, ActiveView::AgentDashboard));
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        None,
        "dashboard open must NOT auto-attach a popup",
    );
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.selected.is_none(),
        "open from an agent must NOT pre-arm reply mode, got: {:?}",
        d.selected,
    );
    assert!(
        d.new_agent_button_focused,
        "open must default to the `[+ New Agent]` button (new-session mode)",
    );
    assert!(
        d.list_focused,
        "open with agents must list-focus for navigation",
    );
}
/// Open with ≥1 agent → overview list focused (nav mode); `[+ New Agent]`
/// stays the cursor target so no row is pre-selected (no silent reply mode).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_with_agents_list_focused() {
    let mut app = test_app_with_agent();
    assert!(!app.agents.is_empty());
    open_dashboard(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.list_focused, "nonempty open must focus the overview list");
    assert!(d.new_agent_button_focused);
    assert!(d.selected.is_none());
    app.dashboard.as_mut().unwrap().list_focused = false;
    app.dashboard
        .as_mut()
        .unwrap()
        .focus_row(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
            0,
        )));
    let _ = dispatch_exit_dashboard(&mut app);
    open_dashboard(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.list_focused, "reopen with agents must re-list-focus");
    assert!(
        d.new_agent_button_focused,
        "reopen resets to new-agent button",
    );
    assert!(d.selected.is_none());
}
/// Open with 0 agents → input focused so "open and type to dispatch" works.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_empty_input_focused() {
    let mut app = test_app();
    assert!(app.agents.is_empty());
    open_dashboard(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        !d.list_focused,
        "empty open must keep dispatch input focused",
    );
    assert!(d.new_agent_button_focused);
    assert!(d.selected.is_none());
    app.dashboard.as_mut().unwrap().list_focused = true;
    let _ = dispatch_exit_dashboard(&mut app);
    open_dashboard(&mut app);
    assert!(
        !app.dashboard.as_ref().unwrap().list_focused,
        "reopen while empty must keep input focused",
    );
}
/// Regression — opening the dashboard from an agent and then typing
/// a prompt must DISPATCH A NEW agent, not reply to the agent we came
/// from; and rapid back-to-back dispatches keep spawning new agents
/// (no "stuck to the same agent" from a sticky reply selection).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_dispatch_after_open_from_agent_spawns_new_sessions() {
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::Agent(AgentId(0));
    let _ = dispatch_open_dashboard(&mut app);
    let before = app.agents.len();
    let _ = dispatch_dashboard_dispatch(&mut app, "spawn a fresh agent".into(), false);
    assert_eq!(
        app.agents.len(),
        before + 1,
        "typed dispatch after open-from-agent must create a NEW agent, not reply",
    );
    let _ = dispatch_dashboard_dispatch(&mut app, "and another one".into(), false);
    assert_eq!(
        app.agents.len(),
        before + 2,
        "rapid dispatch must keep spawning new agents",
    );
}
/// Opening from Welcome leaves the `[+ New Agent]`
/// button as the default focus. Previously the dashboard
/// seeded selection to the first agent so Enter would attach
/// without navigating; with the button taking that role,
/// selection stays empty and the button signals what Enter
/// (on an empty prompt) will do.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_from_welcome_focuses_new_agent_button() {
    let mut app = test_app_with_agent();
    app.active_view = ActiveView::Welcome;
    let _ = dispatch_open_dashboard(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert_eq!(
        d.attached_agent, None,
        "no popup overlay on open from Welcome",
    );
    assert!(
        d.selected.is_none(),
        "selection must be empty so the button is the default cursor target, \
             got: {:?}",
        d.selected,
    );
    assert!(
        d.new_agent_button_focused,
        "the `[+ New Agent]` button must be focused as the default",
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_arrow_keys_clear_manual_scroll_flag() {
    let mut app = three_agent_app();
    mark_agent_nonempty(&mut app, AgentId(0));
    open_dashboard(&mut app);
    app.dashboard.as_mut().unwrap().manual_scroll_active = true;
    let _ = dispatch(Action::DashboardSelectNext, &mut app);
    assert!(
        !app.dashboard.as_ref().unwrap().manual_scroll_active,
        "DashboardSelectNext must clear manual_scroll_active so the viewport \
             re-engages the snap-to-selection on the next render",
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_arrow_keys_clear_manual_scroll_flag_even_with_no_selection() {
    let mut app = three_agent_app();
    mark_agent_nonempty(&mut app, AgentId(0));
    open_dashboard(&mut app);
    let d = app.dashboard.as_mut().unwrap();
    d.manual_scroll_active = true;
    assert!(d.new_agent_button_focused);
    assert!(d.selected.is_none());
    let _ = dispatch(Action::DashboardSelectNext, &mut app);
    assert!(!app.dashboard.as_ref().unwrap().manual_scroll_active);
}
/// `dispatch_dashboard_select` is a no-op when the dashboard
/// isn't open. The manual_scroll_active flag's clear must not
/// run on a phantom dashboard.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_select_without_dashboard_is_noop() {
    let mut app = three_agent_app();
    assert!(app.dashboard.is_none());
    let effects = dispatch(Action::DashboardSelectNext, &mut app);
    assert!(effects.is_empty());
    assert!(app.dashboard.is_none());
}
/// Dashboard state is preserved across reopen; leftover exit-alias text
/// must be cleared so the next Enter does not quit.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_exit_alias_clears_dispatch_text() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    app.active_view = ActiveView::Agent(AgentId(0));
    open_dashboard(&mut app);
    app.dashboard.as_mut().unwrap().dispatch.set_text(":wq");
    let _ = dispatch_exit_dashboard(&mut app);
    assert_eq!(
        app.dashboard.as_ref().unwrap().dispatch.text(),
        "",
        "exit-alias text must be cleared so reopen Enter does not quit"
    );
    let _ = dispatch_open_dashboard(&mut app);
    assert_eq!(app.dashboard.as_ref().unwrap().dispatch.text(), "");
}
/// Bare exit aliases in the dashboard dispatch box quit the CLI (same as
/// agent-prompt send) and must not spawn a session.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_bare_exit_quits_cli() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    app.active_view = ActiveView::Agent(AgentId(0));
    open_dashboard(&mut app);
    let before = app.agents.len();
    for text in ["exit", "quit", ":q", ":wq", ":wq!"] {
        app.active_view = ActiveView::AgentDashboard;
        let effects = dispatch_dashboard_dispatch(&mut app, text.into(), false);
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Quit)),
            "{text:?} must quit the CLI, got: {effects:?}"
        );
        assert_eq!(app.agents.len(), before, "{text:?} must not spawn");
    }
}
/// `/exit` / `/quit` on the dashboard also quit the CLI (not spawn, not
/// merely leave the dashboard).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_slash_exit_quits_cli() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    app.active_view = ActiveView::Agent(AgentId(0));
    open_dashboard(&mut app);
    let before = app.agents.len();
    for cmd in ["/exit", "/quit"] {
        app.active_view = ActiveView::AgentDashboard;
        let effects = dispatch_dashboard_dispatch_slash(&mut app, cmd.into());
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Quit)),
            "{cmd} must quit the CLI, got: {effects:?}"
        );
        assert_eq!(app.agents.len(), before, "{cmd} must not spawn");
    }
}
/// Ctrl+\ from the dashboard exits back to
/// whichever agent view was active before. Since `/dashboard`
/// no longer auto-attaches a popup, the previous "close popup,
/// stay in dashboard" intermediate step is gone.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_ctrl_backslash_exits_dashboard() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    assert!(matches!(app.active_view, ActiveView::AgentDashboard));
    let _ = dispatch_open_dashboard(&mut app);
    assert!(
        matches!(app.active_view, ActiveView::Agent(_) | ActiveView::Welcome),
        "expected to exit dashboard, got: {:?}",
        app.active_view,
    );
}
fn insert_second_agent(app: &mut AppView) -> AgentId {
    let id = AgentId(1);
    let session = make_test_agent_session(app, id, "second");
    let mut agent = AgentView::new(session, ScrollbackState::new());
    agent.generated_session_title = Some("Second".into());
    app.agents.insert(id, agent);
    mark_agent_nonempty(app, id);
    id
}
/// Multi-agent: Ctrl+\ out of the dashboard restores the agent we left,
/// not insertion-order first (empty older sessions under leader mode).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_ctrl_backslash_returns_to_same_agent() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = insert_second_agent(&mut app);
    app.active_view = ActiveView::Agent(id2);
    let _ = dispatch_open_dashboard(&mut app);
    let _ = dispatch_open_dashboard(&mut app);
    assert_eq!(app.active_view, ActiveView::Agent(id2));
    assert!(app.dashboard_return.is_none());
    assert_eq!(app.dashboard.as_ref().and_then(|d| d.attached_agent), None);
}
/// Open from Welcome replaces any leftover return target (e.g. after /home).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_from_welcome_clears_stale_return_agent() {
    use crate::app::app_view::DashboardReturn;
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = insert_second_agent(&mut app);
    app.dashboard_return = Some(DashboardReturn::Agent(id2));
    app.active_view = ActiveView::Welcome;
    let _ = dispatch_open_dashboard(&mut app);
    let _ = dispatch_exit_dashboard(&mut app);
    assert_eq!(app.active_view, ActiveView::Agent(AgentId(0)));
}
/// Attach → overlay exit → dashboard exit restores agent + overlay chrome.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_exit_then_exit_returns_to_attached_agent() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = insert_second_agent(&mut app);
    open_dashboard(&mut app);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id2),
    );
    let _ = dispatch_dashboard_overlay_exit(&mut app);
    let _ = dispatch_exit_dashboard(&mut app);
    assert_eq!(app.active_view, ActiveView::Agent(id2));
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.attached_agent),
        Some(id2)
    );
}
/// Subagent attach round-trip keeps child takeover and Subagent row cursor.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_exit_then_exit_restores_subagent_row() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let parent = AgentId(0);
    mark_agent_nonempty(&mut app, parent);
    let child_sid = "child-return".to_string();
    app.agents
        .get_mut(&parent)
        .unwrap()
        .subagent_sessions
        .insert(child_sid.clone(), make_test_subagent(&child_sid, "sa-ret"));
    let child_view = AgentView::new(
        make_test_agent_session(&app, AgentId(1), "child-session"),
        ScrollbackState::new(),
    );
    app.agents
        .get_mut(&parent)
        .unwrap()
        .subagent_views
        .insert(child_sid.clone(), Box::new(child_view));
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::Subagent {
            parent,
            child_session_id: child_sid.clone(),
        },
    );
    let _ = dispatch_dashboard_overlay_exit(&mut app);
    let _ = dispatch_exit_dashboard(&mut app);
    assert_eq!(app.active_view, ActiveView::Agent(parent));
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.attached_agent),
        Some(parent)
    );
    assert_eq!(
        app.agents[&parent].active_subagent.as_deref(),
        Some(child_sid.as_str())
    );
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.selected.clone()),
        Some(crate::views::dashboard::DashboardRowId::Subagent {
            parent,
            child_session_id: child_sid,
        })
    );
}
/// Dead overlay return target: fall back without painting overlay chrome.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_exit_does_not_overlay_fallback_when_return_agent_dead() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = insert_second_agent(&mut app);
    open_dashboard(&mut app);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id2),
    );
    let _ = dispatch_dashboard_overlay_exit(&mut app);
    app.agents.shift_remove(&id2);
    let _ = dispatch_exit_dashboard(&mut app);
    assert_eq!(app.active_view, ActiveView::Agent(AgentId(0)));
    assert_eq!(app.dashboard.as_ref().and_then(|d| d.attached_agent), None);
}
/// `DashboardOverlayExit` returns the user to
/// the dashboard from an attached agent view and clears the
/// overlay state.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_exit_returns_to_dashboard() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let id = AgentId(0);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id),
    );
    assert!(matches!(app.active_view, ActiveView::Agent(a) if a == id));
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(id));
    let _ = dispatch_dashboard_overlay_exit(&mut app);
    assert!(matches!(app.active_view, ActiveView::AgentDashboard));
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, None);
}
/// Overlay Ctrl+X (confirmed second press) — `DashboardOverlayStop`
/// closes the attached session and lands on the DASHBOARD, not on
/// the fallback agent the generic close path would pick while the
/// closed agent is the active view.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_stop_closes_session_and_returns_to_dashboard() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Second".into());
    app.agents.insert(id2, agent2);
    open_dashboard(&mut app);
    let id = AgentId(0);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id),
    );
    assert!(matches!(app.active_view, ActiveView::Agent(a) if a == id));
    let effects = dispatch_dashboard_overlay_stop(&mut app);
    assert!(
        matches!(app.active_view, ActiveView::AgentDashboard),
        "confirmed overlay stop must land on the dashboard, got {:?}",
        app.active_view,
    );
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, None);
    assert!(
        !app.agents.contains_key(&id),
        "the attached session must be closed",
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::UnregisterActiveSession { .. })),
        "close must unregister the session from the leader roster",
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().error_toast.as_deref(),
        Some(format!("{} Session closed", crate::glyphs::check_mark()).as_str()),
    );
}
/// Overlay stop on the ONLY session: the close is refused (same
/// guard as session close), but the user still lands on the
/// dashboard with the refusal toast surfaced there — the session
/// itself survives.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_stop_only_session_refused_lands_on_dashboard() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    open_dashboard(&mut app);
    let id = AgentId(0);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id),
    );
    let effects = dispatch_dashboard_overlay_stop(&mut app);
    assert!(effects.is_empty(), "refused close must produce no effects");
    assert!(matches!(app.active_view, ActiveView::AgentDashboard));
    assert!(
        app.agents.contains_key(&id),
        "the only session must survive the refused close",
    );
    assert!(
        app.dashboard
            .as_ref()
            .unwrap()
            .error_toast
            .as_deref()
            .is_some_and(|t| t.contains("Cannot close")),
        "the refusal toast must surface on the dashboard",
    );
}
/// The close confirm is armed while idle, but a turn can start
/// inside the 2s window (queue drain, a sent prompt). The
/// confirmed press must then CANCEL the turn instead of closing
/// the session — Ctrl+X only ever closes an idle session.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_stop_busy_agent_cancels_instead_of_closing() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    app.agents
        .insert(id2, AgentView::new(session2, ScrollbackState::new()));
    open_dashboard(&mut app);
    let id = AgentId(0);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id),
    );
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let effects = dispatch_dashboard_overlay_stop(&mut app);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CancelTurn { .. })),
        "the confirmed press on a now-busy agent must cancel the turn",
    );
    assert!(
        app.agents.contains_key(&id),
        "the session must NOT be closed while a turn is running",
    );
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == id),
        "the user stays in the overlay, got {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(id),
        "the overlay attachment must survive",
    );
}
/// `/compact` in flight: overlay stop cancels compaction instead of
/// closing the session (same as a running turn).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_stop_compact_running_cancels() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    app.agents
        .insert(id2, AgentView::new(session2, ScrollbackState::new()));
    open_dashboard(&mut app);
    let id = AgentId(0);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id),
    );
    app.agents.get_mut(&id).unwrap().session.state = AgentState::CommandRunning {
        command: crate::app::agent::AgentCommand::Compact,
        started_at: std::time::Instant::now(),
    };
    app.agents.get_mut(&id).unwrap().session.session_id = Some(acp::SessionId::new("sess-compact"));
    let effects = dispatch_dashboard_overlay_stop(&mut app);
    assert!(
        app.agents.contains_key(&id),
        "stop during /compact must not close the session",
    );
    assert!(
        matches!(
            app.agents.get(&id).unwrap().session.state,
            AgentState::CommandCancelling {
                command: crate::app::agent::AgentCommand::Compact,
            }
        ),
        "stop during /compact must enter CommandCancelling, got {:?}",
        app.agents.get(&id).unwrap().session.state,
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CancelTurn { .. })),
        "stop during /compact must emit CancelTurn, got {effects:?}",
    );
}
/// An armed overlay stop-confirm is bound to "this overlay, this
/// agent" — overlay exits / agent switches that happen WITHOUT a
/// key press (mouse clicks on `[Dashboard]` / `[‹]` / `[›]`) must
/// disarm it, while an unrelated pending action (e.g. quit) is
/// left alone.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_mouse_exit_and_cycle_disarm_pending_stop() {
    use crate::app::app_view::PendingAction;
    let arm = |app: &mut AppView| {
        app.pending_action = Some(PendingAction::new(
            Action::DashboardOverlayStop,
            crate::key!('x', CONTROL),
            "close this session",
        ));
    };
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
    );
    arm(&mut app);
    let _ = dispatch_dashboard_overlay_exit(&mut app);
    assert!(
        app.pending_action.is_none(),
        "mouse overlay exit must disarm the pending stop",
    );
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Second".into());
    app.agents.insert(id2, agent2);
    open_dashboard(&mut app);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
    );
    arm(&mut app);
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert!(
        app.pending_action.is_none(),
        "cycling to another agent must disarm the pending stop",
    );
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
    );
    app.pending_action = Some(PendingAction::new(
        Action::Quit,
        crate::key!('q', CONTROL),
        "quit",
    ));
    let _ = dispatch_dashboard_overlay_exit(&mut app);
    assert!(
        app.pending_action.is_some(),
        "an unrelated pending action must NOT be cleared by overlay exit",
    );
}
/// `DashboardOverlayPrev` / `DashboardOverlayNext`
/// cycle through the agent map in insertion order, wrapping at
/// either end.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_wraps_through_agents() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Second".into());
    app.agents.insert(id2, agent2);
    open_dashboard(&mut app);
    let id1 = AgentId(0);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id1),
    );
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(id1));
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert!(matches!(app.active_view, ActiveView::Agent(a) if a == id2));
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(id2));
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(id1));
    let _ = dispatch_dashboard_overlay_cycle(&mut app, -1);
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(id2));
}
/// Cycle respects the dashboard's filter. With a state
/// filter that hides one of two agents, the cycle becomes a
/// no-op (only one visible row to walk through) — the user
/// can clear the filter to reach the other agent.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_respects_filter() {
    let mut app = test_app_with_agent();
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "filtered-out");
    app.agents
        .insert(id2, AgentView::new(session2, ScrollbackState::new()));
    open_dashboard(&mut app);
    let id1 = AgentId(0);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id1),
    );
    if let Some(d) = app.dashboard.as_mut() {
        d.filter =
            crate::views::dashboard::Filter::State(crate::views::dashboard::RowState::Working);
    }
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(id1),
        "cycle through a filter that hides all rows must NOT walk",
    );
}
/// `overlay_cycle_order` returns top-level agents in the
/// same order `render_dashboard` paints them, NOT the agent
/// map's insertion order. The cycle dispatcher reads from
/// this helper, so the `[‹]` / `[›]` chips walk the user
/// through what they actually see.
///
/// `AgentView::new` stamps `last_active_at = Instant::now()`,
/// so freshly-created agents sort DESCENDING by creation
/// (most-recent first), matching the dashboard's "Idle"
/// group ordering.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_order_matches_visible_rows() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "agent2");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Agent 2".into());
    app.agents.insert(id2, agent2);
    let id3 = AgentId(2);
    let session3 = make_test_agent_session(&app, id3, "agent3");
    let mut agent3 = AgentView::new(session3, ScrollbackState::new());
    agent3.generated_session_title = Some("Agent 3".into());
    app.agents.insert(id3, agent3);
    open_dashboard(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    let order = crate::views::dashboard::overlay_cycle_order(d, &app.agents);
    assert_eq!(
        order,
        vec![AgentId(2), AgentId(1), AgentId(0)],
        "cycle order must match visible row order (most-recently-active first)",
    );
    if let Some(d) = app.dashboard.as_mut() {
        d.pinned
            .insert(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
                0,
            )));
    }
    let d = app.dashboard.as_ref().unwrap();
    let order = crate::views::dashboard::overlay_cycle_order(d, &app.agents);
    assert_eq!(
        order,
        vec![AgentId(0), AgentId(2), AgentId(1)],
        "pinned rows must lead the cycle order, just like the visible list",
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
/// Regression: the cycle anchors on the viewed agent, not a stale
/// `attached_agent` left behind by an external session switch.
#[test]
fn dashboard_overlay_cycle_anchors_on_visible_agent_not_stale_attach() {
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "agent2");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Agent 2".into());
    app.agents.insert(id2, agent2);
    let id3 = AgentId(2);
    let session3 = make_test_agent_session(&app, id3, "agent3");
    let mut agent3 = AgentView::new(session3, ScrollbackState::new());
    agent3.generated_session_title = Some("Agent 3".into());
    app.agents.insert(id3, agent3);
    open_dashboard(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    let order = crate::views::dashboard::overlay_cycle_order(d, &app.agents);
    assert_eq!(order, vec![AgentId(2), AgentId(1), AgentId(0)]);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(order[0]),
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(order[0])
    );
    assert!(matches!(app.active_view, ActiveView::Agent(a) if a == order[0]));
    switch_to_agent(&mut app, order[2], SwitchCause::Picker);
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(order[0]),
        "precondition: external switch leaves attached_agent stale on the first row",
    );
    assert!(matches!(app.active_view, ActiveView::Agent(a) if a == order[2]));
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    let landed = match app.active_view {
        ActiveView::Agent(a) => a,
        other => panic!("active_view not an agent: {other:?}"),
    };
    assert_eq!(
        landed, order[0],
        "cycle must anchor on the visible agent (order[2] → wrap → order[0]), \
             not the stale attached_agent (which would land on order[1])",
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(order[0]),
        "the cycle re-attaches the overlay chrome to the landed agent",
    );
}
/// Cycling with only one agent is a no-op (no
/// view switch, no state mutation).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_noop_with_single_agent() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let id = AgentId(0);
    let _ = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(id),
    );
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(id));
    assert!(matches!(app.active_view, ActiveView::Agent(a) if a == id));
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_from_non_overlay_agent_attaches_and_switches() {
    let mut app = test_app_with_agent();
    let id1 = AgentId(0);
    mark_agent_nonempty(&mut app, id1);
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Second".into());
    app.agents.insert(id2, agent2);
    app.active_view = ActiveView::Agent(id1);
    assert!(
        app.dashboard.is_none(),
        "precondition: no dashboard / overlay attached yet",
    );
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == id2),
        "next from a non-overlay agent must switch active_view to the next agent, got {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.attached_agent),
        Some(id2),
        "cycling from a non-overlay agent must attach the overlay chrome to the next agent",
    );
    let _ = dispatch_dashboard_overlay_cycle(&mut app, -1);
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == id1),
        "prev must switch back to the first agent, got {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.attached_agent),
        Some(id1),
        "prev must keep the overlay chrome attached to the now-current agent",
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_works_through_handle_input_after_dashboard_esc_exit() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app_with_agent();
    let id1 = AgentId(0);
    mark_agent_nonempty(&mut app, id1);
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Second".into());
    app.agents.insert(id2, agent2);
    app.active_view = ActiveView::Agent(id1);
    open_dashboard(&mut app);
    assert!(matches!(app.active_view, ActiveView::AgentDashboard));
    let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    for _ in 0..4 {
        if !matches!(app.active_view, ActiveView::AgentDashboard) {
            break;
        }
        if let crate::app::app_view::InputOutcome::Action(a) = app.handle_input(&esc) {
            let _ = dispatch(a, &mut app);
        }
    }
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == id1),
        "Esc must exit the dashboard back to the agent, got {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.attached_agent),
        None,
        "exiting the dashboard via Esc clears attached_agent (the bug's precondition)",
    );
    let ctrl_rbracket = Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL));
    let outcome = app.handle_input(&ctrl_rbracket);
    assert!(
        matches!(
            outcome,
            crate::app::app_view::InputOutcome::Action(Action::DashboardOverlayNext)
        ),
        "Ctrl+] in a non-overlay agent must route to DashboardOverlayNext, got {outcome:?}",
    );
    if let crate::app::app_view::InputOutcome::Action(a) = outcome {
        let _ = dispatch(a, &mut app);
    }
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == id2),
        "Ctrl+] must cycle to the next agent, got {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.attached_agent),
        Some(id2),
        "the cycle must attach the overlay chrome to the next agent",
    );
}
/// A dashboard first materialized by cycling must be fully configured (not
/// just seeded from persisted state), else it renders bare on back-out.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_from_unopened_dashboard_configures_state() {
    let mut app = test_app_with_agent();
    let id1 = AgentId(0);
    mark_agent_nonempty(&mut app, id1);
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Second".into());
    app.agents.insert(id2, agent2);
    app.active_view = ActiveView::Agent(id1);
    app.default_yolo = true;
    assert!(
        app.dashboard.is_none(),
        "precondition: dashboard never opened"
    );
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    let d = app
        .dashboard
        .as_ref()
        .expect("a real cycle must materialize the dashboard");
    assert_eq!(d.attached_agent, Some(id2));
    assert_eq!(
        d.cwd, app.cwd,
        "cycle-materialized dashboard adopts the app cwd"
    );
    assert!(
        matches!(
            d.pending_mode,
            crate::views::dashboard::DashboardDispatchMode::AlwaysApprove
        ),
        "pending_mode must reflect default_yolo (configure_dashboard_state ran), got {:?}",
        d.pending_mode,
    );
}
/// Cycling from a never-opened dashboard honors the auth gate, mirroring
/// `dispatch_open_dashboard` — no ungated materialization.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_unopened_respects_auth_gate() {
    let mut app = test_app_with_agent();
    let id1 = AgentId(0);
    mark_agent_nonempty(&mut app, id1);
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Second".into());
    app.agents.insert(id2, agent2);
    app.active_view = ActiveView::Agent(id1);
    app.auth_state = AuthState::Pending { error: None };
    assert!(app.dashboard.is_none());
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == id1),
        "unauthenticated cycle must not switch agents, got {:?}",
        app.active_view,
    );
    assert!(
        app.dashboard.is_none(),
        "unauthenticated cycle must not materialize the dashboard",
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_non_overlay_single_agent_is_noop() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    mark_agent_nonempty(&mut app, id);
    app.active_view = ActiveView::Agent(id);
    assert!(app.dashboard.is_none());
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == id),
        "single-agent next must not switch views, got {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.attached_agent),
        None,
        "single-agent cycle must not attach overlay chrome",
    );
    let _ = dispatch_dashboard_overlay_cycle(&mut app, -1);
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == id),
        "single-agent prev must not switch views, got {:?}",
        app.active_view,
    );
    assert_eq!(app.dashboard.as_ref().and_then(|d| d.attached_agent), None,);
    assert!(
        app.dashboard.is_none(),
        "single-agent no-op must not materialize the dashboard (no load_persisted side effect)",
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_non_agent_active_view_is_noop() {
    let mut app = test_app_with_agent();
    let id1 = AgentId(0);
    mark_agent_nonempty(&mut app, id1);
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Second".into());
    app.agents.insert(id2, agent2);
    app.active_view = ActiveView::Welcome;
    assert!(app.dashboard.is_none());
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert!(
        matches!(app.active_view, ActiveView::Welcome),
        "cycle with a non-agent active_view must not switch views, got {:?}",
        app.active_view,
    );
    assert!(
        app.dashboard.is_none(),
        "cycle with a non-agent active_view must not materialize the dashboard",
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_non_overlay_noop_when_dashboard_disabled() {
    let _guard =
        crate::test_util::EnvVarGuard::set("ECHO_AGENT_AGENT_DASHBOARD", std::path::Path::new("0"));
    let mut app = test_app_with_agent();
    let id1 = AgentId(0);
    mark_agent_nonempty(&mut app, id1);
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "second");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Second".into());
    app.agents.insert(id2, agent2);
    app.active_view = ActiveView::Agent(id1);
    assert!(app.dashboard.is_none());
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == id1),
        "cycle must be a no-op when the dashboard is disabled, got {:?}",
        app.active_view,
    );
    assert!(
        app.dashboard.is_none(),
        "a disabled dashboard must NOT be materialized by the cycle keys",
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_overlay_cycle_non_overlay_noop_when_current_agent_hidden() {
    let mut app = test_app_with_agent();
    let current = AgentId(0);
    let id1 = AgentId(1);
    let session1 = make_test_agent_session(&app, id1, "first");
    let mut agent1 = AgentView::new(session1, ScrollbackState::new());
    agent1.generated_session_title = Some("First".into());
    app.agents.insert(id1, agent1);
    let id2 = AgentId(2);
    let session2 = make_test_agent_session(&app, id2, "second");
    let mut agent2 = AgentView::new(session2, ScrollbackState::new());
    agent2.generated_session_title = Some("Second".into());
    app.agents.insert(id2, agent2);
    app.active_view = ActiveView::Agent(current);
    ensure_dashboard_state(&mut app);
    let order =
        crate::views::dashboard::overlay_cycle_order(app.dashboard.as_ref().unwrap(), &app.agents);
    assert_eq!(
        order,
        vec![id2, id1],
        "precondition: the empty current agent is hidden from the visible order",
    );
    let _ = dispatch_dashboard_overlay_cycle(&mut app, 1);
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == current),
        "cycle must not jump to a random row when the current agent is filtered out, got {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().and_then(|d| d.attached_agent),
        None,
        "a filtered-out current agent must not attach overlay chrome",
    );
}
/// `DashboardToggleAutoApprove` flips `yolo_mode` on
/// the selected row's owning agent. Reuses `set_yolo_mode` by
/// temporarily switching `active_view`, so the existing toast
/// / persist / queue-drain logic all apply.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_toggle_auto_approve_flips_yolo_on_selected_agent() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.yolo_mode = false;
    app.active_view = ActiveView::Welcome;
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_row(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    let _ = dispatch_dashboard_toggle_auto_approve(&mut app);
    assert!(
        app.agents.get(&id).unwrap().session.yolo_mode,
        "first toggle must turn YOLO on",
    );
    let _ = dispatch_dashboard_toggle_auto_approve(&mut app);
    assert!(
        !app.agents.get(&id).unwrap().session.yolo_mode,
        "second toggle must turn YOLO off",
    );
    assert!(matches!(app.active_view, ActiveView::AgentDashboard));
}
/// No selection → toggle is a no-op + toast.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_toggle_auto_approve_with_no_selection_toasts() {
    let mut app = test_app();
    open_dashboard(&mut app);
    let effects = dispatch_dashboard_toggle_auto_approve(&mut app);
    assert!(effects.is_empty());
    assert!(
        app.dashboard.as_ref().unwrap().error_toast.is_some(),
        "missing selection must surface a toast",
    );
}
/// End-to-end rename flow: begin rename -> type characters -> commit.
/// Untitled fixture agent has no display_name / generated title, so the
/// draft prefills empty; typing then commit emit `RenameSession` and stamp
/// `display_name`.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_rename_end_to_end_top_level_row() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let id = AgentId(0);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    dispatch_dashboard_begin_rename(&mut app);
    assert_eq!(
        app.dashboard
            .as_ref()
            .unwrap()
            .rename
            .as_ref()
            .expect("begin_rename must arm the rename overlay")
            .text(),
        "",
        "untitled agent prefills an empty draft",
    );
    let registry = crate::actions::ActionRegistry::defaults();
    for character in "My renamed session".chars() {
        let outcome = app.dashboard.as_mut().unwrap().handle_input(
            &crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(character),
                crossterm::event::KeyModifiers::NONE,
            )),
            &registry,
        );
        assert!(matches!(
            outcome,
            crate::app::app_view::InputOutcome::Changed
        ));
    }
    assert_eq!(
        app.dashboard
            .as_ref()
            .unwrap()
            .rename
            .as_ref()
            .unwrap()
            .text(),
        "My renamed session",
    );
    let effects = dispatch(Action::DashboardCommitRename, &mut app);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::RenameSession { agent_id, title, kind, .. }
                if *agent_id == id
                    && title == "My renamed session"
                    && *kind == echo_agent_runtime::session::unified_list::SessionKind::Build
        )),
        "commit must emit a RenameSession effect, got {effects:?}",
    );
    assert_eq!(
        app.agents.get(&id).and_then(|a| a.display_name.clone()),
        Some("My renamed session".to_string()),
    );
    assert!(
        app.dashboard.as_ref().unwrap().rename.is_none(),
        "commit must clear the rename overlay",
    );
}
/// Dashboard rename of a chat-kind agent must stamp `kind: Chat` so the
/// shell takes the conversations fork.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_rename_chat_kind_stamps_kind_chat() {
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.chat_kind = true;
    agent.conversation_entry = true;
    open_dashboard(&mut app);
    let id = AgentId(0);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    dispatch_dashboard_begin_rename(&mut app);
    app.dashboard
        .as_mut()
        .and_then(|dashboard| dashboard.rename.as_mut())
        .expect("rename draft")
        .set_text("Chat title");
    let effects = dispatch(Action::DashboardCommitRename, &mut app);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::RenameSession { agent_id, title, kind, .. }
                if *agent_id == id
                    && title == "Chat title"
                    && *kind == echo_agent_runtime::session::unified_list::SessionKind::Chat
        )),
        "chat-lane dashboard rename must send kind=chat, got {effects:?}",
    );
}
/// `DashboardCancelRename` emits no effects and
/// leaves `display_name` untouched. Previously named
/// `dashboard_rename_cancel_via_esc_does_not_emit_effect`
/// but that name implied Esc keystroke routing — the test
/// actually dispatches `Action::DashboardCancelRename` directly.
/// The Esc-keystroke routing is now pinned by the sibling test
/// `dashboard_rename_esc_keystroke_routes_to_cancel`.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_rename_cancel_action_emits_no_effect() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let id = AgentId(0);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    dispatch_dashboard_begin_rename(&mut app);
    app.dashboard
        .as_mut()
        .and_then(|dashboard| dashboard.rename.as_mut())
        .expect("rename draft")
        .set_text("scratch");
    let effects = dispatch(Action::DashboardCancelRename, &mut app);
    assert!(effects.is_empty(), "cancel must not emit effects");
    assert!(
        app.dashboard.as_ref().unwrap().rename.is_none(),
        "cancel must clear the rename overlay",
    );
    assert!(
        app.agents
            .get(&id)
            .and_then(|a| a.display_name.clone())
            .is_none(),
        "cancel must not stamp display_name",
    );
}
/// Drive Esc through `state.handle_input`
/// (the real keystroke path) to verify rename-mode wiring. A
/// future change that rewires Esc to a different action in
/// rename mode would silently break user expectation; the
/// action-level test wouldn't catch it.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_rename_esc_keystroke_routes_to_cancel() {
    use crate::actions::ActionRegistry;
    use crate::app::app_view::InputOutcome;
    use crate::views::dashboard::state::{DashboardState, RenameDraft};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    let registry = ActionRegistry::defaults();
    let mut state = DashboardState::new();
    let id = crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0));
    state.selected = Some(id.clone());
    state.rename = Some(RenameDraft::new(id.clone(), "draft"));
    let esc = Event::Key(KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    });
    let outcome = state.handle_input(&esc, &registry);
    assert!(
        matches!(outcome, InputOutcome::Action(Action::DashboardCancelRename)),
        "Esc in rename mode must produce DashboardCancelRename, got {outcome:?}",
    );
}
/// The dashboard header upgrade CTA: a pinned promo paints `[label]` (+ its
/// configured `cta.caption`, bare when none), arms the click rect (→
/// `AnnouncementsOpenCta(Dashboard)`), and lights the `Ctrl+O` override; a
/// dismissible promo shows the button but keeps Ctrl+O falling through and
/// suppresses any caption; no promo shows nothing.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_upgrade_cta_paints_arms_rect_and_ctrl_o_override() {
    use crate::actions::ActionRegistry;
    use crate::app::app_view::InputOutcome;
    use crate::views::dashboard::HeaderUpgradeCta;
    use crate::views::dashboard::render_dashboard;
    use crate::views::dashboard::state::DashboardState;
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use echo_agent_telemetry::events::AnnouncementCtaSurface;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let registry = ActionRegistry::defaults();
    let mut agents: indexmap::IndexMap<AgentId, crate::app::agent_view::AgentView> =
        indexmap::IndexMap::new();
    let area = Rect::new(0, 0, 140, 20);
    let ctrl_o = || Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    let header_row = |buf: &Buffer, y: u16| -> String {
        (0..area.width)
            .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect()
    };
    const CAPTION: &str = "or use Ctrl+O";
    let mut buf = Buffer::empty(area);
    let mut state = DashboardState::new();
    let _ = render_dashboard(
        &mut buf,
        area,
        &mut state,
        &mut agents,
        &registry,
        None,
        &[],
        false,
        Some(HeaderUpgradeCta {
            label: "Upgrade Account",
            pinned: true,
            caption: Some(CAPTION),
        }),
    );
    assert!(
        state.pinned_upgrade_cta_live,
        "pinned promo lights the Ctrl+O override"
    );
    let rect = state
        .upgrade_cta_hit
        .rect
        .expect("pinned promo arms the header CTA rect");
    let header = header_row(&buf, rect.y);
    assert!(header.contains("[Upgrade Account]"), "header={header:?}");
    assert!(
        header.contains(CAPTION),
        "pinned dashboard promo shows its configured caption; header={header:?}"
    );
    assert!(matches!(
        state.handle_input(&ctrl_o(), &registry),
        InputOutcome::Action(Action::AnnouncementsOpenCta(
            AnnouncementCtaSurface::Keyboard
        ))
    ));
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    });
    assert!(matches!(
        state.handle_input(&click, &registry),
        InputOutcome::Action(Action::AnnouncementsOpenCta(
            AnnouncementCtaSurface::Dashboard
        ))
    ));
    let mut buf = Buffer::empty(area);
    let mut state = DashboardState::new();
    let _ = render_dashboard(
        &mut buf,
        area,
        &mut state,
        &mut agents,
        &registry,
        None,
        &[],
        false,
        Some(HeaderUpgradeCta {
            label: "Upgrade Account",
            pinned: true,
            caption: None,
        }),
    );
    assert!(state.pinned_upgrade_cta_live);
    let rect = state
        .upgrade_cta_hit
        .rect
        .expect("caption-less pinned promo still arms the button");
    let header = header_row(&buf, rect.y);
    assert!(header.contains("[Upgrade Account]"), "header={header:?}");
    assert!(
        !header.contains(CAPTION),
        "absent caption paints nothing after the button; header={header:?}"
    );
    let mut buf = Buffer::empty(area);
    let mut state = DashboardState::new();
    let _ = render_dashboard(
        &mut buf,
        area,
        &mut state,
        &mut agents,
        &registry,
        None,
        &[],
        false,
        Some(HeaderUpgradeCta {
            label: "Upgrade Account",
            pinned: false,
            caption: Some(CAPTION),
        }),
    );
    assert!(!state.pinned_upgrade_cta_live);
    let rect = state
        .upgrade_cta_hit
        .rect
        .expect("dismissible promo still shows the clickable button");
    let header = header_row(&buf, rect.y);
    assert!(header.contains("[Upgrade Account]"), "header={header:?}");
    assert!(
        !header.contains(CAPTION),
        "dismissible dashboard promo suppresses its configured caption; header={header:?}"
    );
    assert!(
        !matches!(
            state.handle_input(&ctrl_o(), &registry),
            InputOutcome::Action(Action::AnnouncementsOpenCta(_))
        ),
        "dismissible promo must not steal Ctrl+O in the dashboard"
    );
    let mut buf = Buffer::empty(area);
    let mut state = DashboardState::new();
    let _ = render_dashboard(
        &mut buf,
        area,
        &mut state,
        &mut agents,
        &registry,
        None,
        &[],
        false,
        None,
    );
    assert!(state.upgrade_cta_hit.rect.is_none());
    assert!(!state.pinned_upgrade_cta_live);
}
/// Empty rename draft cancels without emitting an Effect.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_commit_rename_empty_does_not_emit_effect() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
            0,
        )));
        d.rename = Some(crate::views::dashboard::state::RenameDraft::new(
            crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
            "   ",
        ));
    }
    let effects = dispatch_dashboard_commit_rename(&mut app);
    assert!(
        effects.is_empty(),
        "empty draft must not produce a RenameSession effect"
    );
    assert!(app.dashboard.as_ref().unwrap().rename.is_none());
}
/// Rename on subagent row toasts and refuses.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_begin_rename_on_subagent_row_sets_error_toast() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::Subagent {
            parent: AgentId(0),
            child_session_id: "child".into(),
        });
    }
    dispatch_dashboard_begin_rename(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.rename.is_none());
    assert!(d.error_toast.is_some());
}
/// Begin-rename prefills the draft from the agent's `display_name`.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_begin_rename_prefills_display_name() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().display_name = Some("  My Session Name  ".into());
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    dispatch_dashboard_begin_rename(&mut app);
    assert_eq!(
        app.dashboard
            .as_ref()
            .unwrap()
            .rename
            .as_ref()
            .expect("begin_rename must arm the rename overlay")
            .text(),
        "My Session Name",
        "begin-rename must prefill trimmed display_name",
    );
}
/// Begin-rename falls back to `generated_session_title` when `display_name` is absent.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_begin_rename_prefills_generated_session_title() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.display_name = None;
        agent.generated_session_title = Some("  Auto Title  ".into());
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    dispatch_dashboard_begin_rename(&mut app);
    assert_eq!(
        app.dashboard
            .as_ref()
            .unwrap()
            .rename
            .as_ref()
            .expect("begin_rename must arm the rename overlay")
            .text(),
        "Auto Title",
        "begin-rename must prefill trimmed generated_session_title",
    );
}
/// Non-empty `display_name` wins over `generated_session_title`.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_begin_rename_prefers_display_name_over_generated() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.display_name = Some("User Name".into());
        agent.generated_session_title = Some("Generated Name".into());
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    dispatch_dashboard_begin_rename(&mut app);
    assert_eq!(
        app.dashboard
            .as_ref()
            .unwrap()
            .rename
            .as_ref()
            .expect("begin_rename must arm the rename overlay")
            .text(),
        "User Name",
        "display_name must take precedence over generated_session_title",
    );
}
/// Whitespace-only `display_name` falls through to `generated_session_title`.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_begin_rename_whitespace_display_name_falls_through() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.display_name = Some("   ".into());
        agent.generated_session_title = Some("Generated".into());
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    dispatch_dashboard_begin_rename(&mut app);
    assert_eq!(
        app.dashboard
            .as_ref()
            .unwrap()
            .rename
            .as_ref()
            .expect("begin_rename must arm the rename overlay")
            .text(),
        "Generated",
        "whitespace-only display_name must fall through to generated_session_title",
    );
}
/// Dispatch text + filter survive a close
/// and reopen of the dashboard. The contract is
/// "in-memory state preserved across reopen"; this test pins it.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_state_preserved_across_reopen() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.dispatch.set_text("draft prompt");
        d.filter = crate::views::dashboard::Filter::Substring("foo".into());
    }
    let _ = dispatch_exit_dashboard(&mut app);
    let _ = dispatch_open_dashboard(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert_eq!(
        d.dispatch.text(),
        "draft prompt",
        "dispatch text must survive reopen",
    );
    assert!(
        matches!(d.filter, crate::views::dashboard::Filter::Substring(ref s) if s == "foo"),
        "filter must survive reopen, got {:?}",
        d.filter
    );
}
/// Opening dashboard while it's already open closes it.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_then_open_again_closes() {
    let mut app = test_app_with_agent();
    let _ = dispatch_open_dashboard(&mut app);
    assert!(matches!(app.active_view, ActiveView::AgentDashboard));
    let _ = dispatch_open_dashboard(&mut app);
    assert!(!matches!(app.active_view, ActiveView::AgentDashboard));
}
/// Stale pinned ids are dropped at open.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_drops_pinned_ids_for_missing_agents() {
    let mut app = test_app_with_agent();
    app.dashboard = Some(crate::views::dashboard::DashboardState::new());
    if let Some(d) = app.dashboard.as_mut() {
        d.pinned
            .insert(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
                99,
            )));
    }
    app.active_view = ActiveView::Welcome;
    let _ = dispatch_open_dashboard(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.pinned.is_empty(), "stale pin should be gc'd at open");
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_row_stop_cancels_wake_turn_with_gesture_trigger() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let target = *app.agents.keys().next().unwrap();
    {
        let agent = app.agents.get_mut(&target).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("s0"));
        agent.running_wake_turn = Some(crate::app::agent_view::RunningWakeTurn {
            prompt_id: "task-completed-bg1".into(),
            cancel_sent: false,
        });
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let effects = dispatch_dashboard_stop(&mut app);
    assert!(
        matches!(
            effects.first(),
            Some(crate::app::actions::Effect::CancelTurn {
                trigger: Some(crate::app::actions::CancelTrigger::DashboardStop),
                ..
            })
        ),
        "the row stop must cancel the wake turn with the gesture trigger, got {effects:?}"
    );
    assert!(
        app.agents[&target].wake_turn_cancelling(),
        "the wake marker must record the cancelling phase"
    );
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_row_stop_during_send_over_wake_cancels_wake_not_local_turn() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let target = *app.agents.keys().next().unwrap();
    {
        let agent = app.agents.get_mut(&target).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("s0"));
        agent.session.state = crate::app::agent::AgentState::TurnRunning;
        agent.session.current_prompt_id = Some("user-1".into());
        agent.running_wake_turn = Some(crate::app::agent_view::RunningWakeTurn {
            prompt_id: "task-completed-bg1".into(),
            cancel_sent: false,
        });
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let effects = dispatch_dashboard_stop(&mut app);
    assert!(
        matches!(
            effects.first(),
            Some(crate::app::actions::Effect::CancelTurn {
                trigger: Some(crate::app::actions::CancelTrigger::DashboardStop),
                ..
            })
        ),
        "row stop must still emit cancel, got {effects:?}"
    );
    let agent = &app.agents[&target];
    assert!(
        agent.session.state.is_turn_running(),
        "local user turn is queued behind the wake, not cancelled"
    );
    assert!(
        agent
            .running_wake_turn
            .as_ref()
            .is_some_and(|w| w.cancel_sent),
        "row stop must cancel the shell-front wake"
    );
    assert!(
        agent.pending_cancel_resend.is_none(),
        "auto-resend would hit the promoted user turn"
    );
}
#[test]
fn dashboard_stop_double_press_deletes_top_level() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    for (i, a) in app.agents.values_mut().enumerate() {
        a.session.session_id = Some(acp::SessionId::new(format!("s{i}")));
    }
    open_dashboard(&mut app);
    let target = *app.agents.keys().next().unwrap();
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let _ = dispatch_dashboard_stop(&mut app);
    assert!(app.dashboard.as_ref().unwrap().delete_confirm.is_some());
    let effects = dispatch_dashboard_stop(&mut app);
    assert!(matches!(
        effects.last(),
        Some(crate::app::actions::Effect::DeleteSession { .. })
    ));
}
/// Closing the selected agent moves the cursor DOWN one row (onto the
/// agent that shifts up into its place) instead of dropping it to
/// `None`, which would bounce the next ↑/↓ back to the top of the list.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_moves_selection_down_one() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    for (i, agent) in app.agents.values_mut().enumerate() {
        agent.display_name = Some(format!("agent-{i}"));
        agent.session.session_id = Some(acp::SessionId::new(format!("s{i}")));
    }
    open_dashboard(&mut app);
    let order = dashboard_row_order(&app);
    assert!(order.len() >= 3, "need >=3 rows, got {}", order.len());
    let first = order[0].clone();
    let second = order[1].clone();
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_row(first.clone());
    }
    let _ = dispatch_dashboard_stop(&mut app);
    let effects = dispatch_dashboard_stop(&mut app);
    let crate::views::dashboard::DashboardRowId::TopLevel(first_id) = &first else {
        panic!("first row should be top-level");
    };
    let session_id = app.agents[first_id]
        .session
        .session_id
        .as_ref()
        .expect("session id")
        .to_string();
    assert!(
        matches!(
            effects.last(),
            Some(crate::app::actions::Effect::DeleteSession { .. })
        ),
        "second Ctrl+X must delete, got {effects:?}"
    );
    let _ = dispatch_task_result(
        crate::app::actions::TaskResult::DeleteSessionComplete {
            source: "current".into(),
            session_id,
            after: crate::app::actions::AfterSessionDelete::Dashboard,
        },
        &mut app,
    );
    assert!(!app.agents.contains_key(first_id));
    assert_eq!(app.dashboard.as_ref().unwrap().selected, Some(second));
}
/// Closing the LAST row has no row below it, so the cursor falls back
/// to the previous row rather than disappearing.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_last_row_falls_back_to_previous() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    for (i, agent) in app.agents.values_mut().enumerate() {
        agent.display_name = Some(format!("agent-{i}"));
        agent.session.session_id = Some(acp::SessionId::new(format!("s{i}")));
    }
    open_dashboard(&mut app);
    let order = dashboard_row_order(&app);
    assert!(order.len() >= 3, "need >=3 rows, got {}", order.len());
    let last = order[order.len() - 1].clone();
    let prev = order[order.len() - 2].clone();
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_row(last.clone());
    }
    let _ = dispatch_dashboard_stop(&mut app);
    let effects = dispatch_dashboard_stop(&mut app);
    let crate::views::dashboard::DashboardRowId::TopLevel(last_id) = &last else {
        panic!("last row should be top-level");
    };
    let session_id = app.agents[last_id]
        .session
        .session_id
        .as_ref()
        .expect("session id")
        .to_string();
    assert!(matches!(
        effects.last(),
        Some(crate::app::actions::Effect::DeleteSession { .. })
    ));
    let _ = dispatch_task_result(
        crate::app::actions::TaskResult::DeleteSessionComplete {
            source: "current".into(),
            session_id,
            after: crate::app::actions::AfterSessionDelete::Dashboard,
        },
        &mut app,
    );
    assert!(!app.agents.contains_key(last_id));
    assert_eq!(app.dashboard.as_ref().unwrap().selected, Some(prev));
}
/// First Ctrl+X must NOT plant an `error_toast`. The
/// dispatch-input placeholder is reserved for the user's typing
/// target — the footer's `ShortcutsBar::with_pending` already
/// surfaces the "press Ctrl+X again to close this session"
/// hint via `delete_confirm` and is the canonical place for it.
/// Two copies of the same hint in two different surfaces
/// confused the user (the prompt one stole visual weight).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_does_not_plant_error_toast() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    open_dashboard(&mut app);
    let target = *app.agents.keys().next().unwrap();
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let _ = dispatch_dashboard_stop(&mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.delete_confirm.is_some(),
        "first Ctrl+X on an idle row must arm delete_confirm (footer reads from this)",
    );
    assert!(
        d.error_toast.is_none(),
        "first Ctrl+X must NOT set error_toast — the footer hint is the \
             canonical surface; toast in the dispatch input is duplication. \
             Got: {:?}",
        d.error_toast,
    );
}
/// `DashboardOpenShortcutsHelp` builds the modal state on
/// `DashboardState`. Subsequent presses while the modal is
/// open are no-ops (idempotent) so the user's search query
/// and scroll position survive a stray Ctrl+. tap.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_open_shortcuts_help_builds_modal_idempotently() {
    let mut app = test_app();
    open_dashboard(&mut app);
    assert!(app.dashboard.as_ref().unwrap().shortcuts_modal.is_none());
    let _ = dispatch(Action::DashboardOpenShortcutsHelp, &mut app);
    let modal_first = app
        .dashboard
        .as_ref()
        .unwrap()
        .shortcuts_modal
        .as_ref()
        .map(|m| m.entries.len())
        .unwrap_or(0);
    assert!(
        modal_first > 0,
        "modal must be populated with at least one entry, got entries={modal_first}",
    );
    app.dashboard
        .as_mut()
        .unwrap()
        .shortcuts_modal
        .as_mut()
        .unwrap()
        .state
        .set_query("nav");
    let _ = dispatch(Action::DashboardOpenShortcutsHelp, &mut app);
    assert_eq!(
        app.dashboard
            .as_ref()
            .unwrap()
            .shortcuts_modal
            .as_ref()
            .unwrap()
            .state
            .query(),
        "nav",
        "re-dispatch must NOT rebuild the modal — user's query would vanish",
    );
}
/// `DashboardCloseShortcutsHelp` clears the modal. Mirrors the
/// modal-chrome `CloseRequested` outcome routed through the
/// dashboard-state input handler.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_close_shortcuts_help_clears_modal() {
    let mut app = test_app();
    open_dashboard(&mut app);
    let _ = dispatch(Action::DashboardOpenShortcutsHelp, &mut app);
    assert!(app.dashboard.as_ref().unwrap().shortcuts_modal.is_some());
    let _ = dispatch(Action::DashboardCloseShortcutsHelp, &mut app);
    assert!(app.dashboard.as_ref().unwrap().shortcuts_modal.is_none());
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_new_agent_button_create_with_detail_switches_view() {
    let mut app = test_app();
    open_dashboard(&mut app);
    let agents_before = app.agents.len();
    let effects = dispatch(Action::DashboardCreateNewAgentWithDetail, &mut app);
    assert_eq!(
        app.agents.len(),
        agents_before + 1,
        "create-with-detail must spawn a session",
    );
    let new_id = *app.agents.keys().last().unwrap();
    assert!(
        matches!(app.active_view, ActiveView::Agent(a) if a == new_id),
        "create-with-detail must switch active_view to the new agent, got {:?}",
        app.active_view,
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().attached_agent,
        Some(new_id),
        "create-with-detail must set attached_agent so the overlay paints",
    );
    assert!(!app.dashboard.as_ref().unwrap().new_agent_button_focused);
    assert!(!effects.is_empty(), "session creation must emit effects");
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_focus_new_agent_button_action_clears_selection() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_row(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
            0,
        )));
    }
    assert!(app.dashboard.as_ref().unwrap().selected.is_some());
    let _ = dispatch(Action::DashboardFocusNewAgentButton, &mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.new_agent_button_focused,
        "FocusNewAgentButton must light up the button flag",
    );
    assert!(
        d.selected.is_none(),
        "FocusNewAgentButton must clear `selected` so the invariant holds",
    );
}
/// Up-arrow on the FIRST row hands focus over to the
/// `[+ New Agent]` button — the button behaves as a virtual
/// row at index -1 so the user can walk straight off the top
/// of the list onto it without an extra Esc.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_up_arrow_from_first_row_focuses_button() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_row(crate::views::dashboard::DashboardRowId::TopLevel(id));
    }
    let _ = dispatch(Action::DashboardSelectPrev, &mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.new_agent_button_focused,
        "Up from the first row must focus the button",
    );
    assert!(d.selected.is_none());
}
/// Up-arrow on the button is a no-op (no wrap). Mirrors the
/// agents modal — the cursor sits on the button and stays
/// there until you press Down or click a row.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_up_arrow_on_button_is_noop() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_new_agent_button();
    }
    let _ = dispatch(Action::DashboardSelectPrev, &mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.new_agent_button_focused,
        "Up on button must stay on button"
    );
    assert!(d.selected.is_none());
}
/// Down-arrow on the button walks to the first focusable. With state
/// grouping ON (the default), that's the first section header; a
/// second Down steps into the first row inside it. When there are no
/// rows the cursor stays on the button.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_down_arrow_on_button_selects_first_focusable() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    mark_agent_nonempty(&mut app, id);
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.focus_new_agent_button();
    }
    let _ = dispatch(Action::DashboardSelectNext, &mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.selected_section.is_some(),
        "Down on button must land on the first section header",
    );
    assert!(d.selected.is_none());
    assert!(!d.new_agent_button_focused);
    let _ = dispatch(Action::DashboardSelectNext, &mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert_eq!(
        d.selected,
        Some(crate::views::dashboard::DashboardRowId::TopLevel(id)),
        "second Down must step into the first row",
    );
    assert!(d.selected_section.is_none());
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_down_arrow_on_button_with_no_rows_is_noop() {
    let mut app = test_app();
    open_dashboard(&mut app);
    assert!(app.dashboard.as_ref().unwrap().new_agent_button_focused);
    let _ = dispatch(Action::DashboardSelectNext, &mut app);
    let d = app.dashboard.as_ref().unwrap();
    assert!(
        d.new_agent_button_focused,
        "Down on button with empty row list must keep the button focused",
    );
    assert!(d.selected.is_none());
}
/// Filter parser plumbing: State known.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_filter_state_known_token_via_dispatch() {
    use crate::views::dashboard::{FilterValue, RowState};
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let _ = dispatch(
        crate::app::actions::Action::DashboardSetFilter(FilterValue::State(RowState::Idle)),
        &mut app,
    );
    let d = app.dashboard.as_ref().unwrap();
    assert!(matches!(
        d.filter,
        crate::views::dashboard::Filter::State(RowState::Idle)
    ));
}
/// count=0 returns empty even when scrollback has entries.
#[test]
fn extract_recent_lines_count_zero_is_empty() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::user_prompt("hello"));
    let out = crate::views::dashboard::peek::extract_recent_lines(agent, 0);
    assert!(out.is_empty());
}
/// Empty scrollback returns empty regardless of count.
#[test]
fn extract_recent_lines_empty_scrollback() {
    let app = test_app_with_agent();
    let agent = app.agents.get(&AgentId(0)).unwrap();
    let out = crate::views::dashboard::peek::extract_recent_lines(agent, 6);
    assert!(out.is_empty());
}
/// Pin the placeholder strings for the "no
/// meaningful body" `RenderBlock` variants. A regression that
/// changed `"(tool call)"` to `"(tool)"` would otherwise slip
/// through.
#[test]
fn extract_recent_lines_tool_call_placeholder() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.scrollback.push_block(RenderBlock::tool_call(
        "Read",
        "src/main.rs (50 lines)",
        true,
    ));
    let out = crate::views::dashboard::peek::extract_recent_lines(agent, 6);
    assert_eq!(out, vec!["(tool call)".to_string()]);
}
/// `BgTask` projects to the `(background task)`
/// placeholder.
#[test]
fn extract_recent_lines_bg_task_placeholder() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::bg_task("npm install", "task-1"));
    let out = crate::views::dashboard::peek::extract_recent_lines(agent, 6);
    assert_eq!(out, vec!["(background task)".to_string()]);
}
/// Explicit first-line projection assertion for
/// `RenderBlock::AgentMessage` (no UserPrompt prefix). Asserts
/// the exact result vector to catch regressions that the
/// "evidence by absence" test below would miss.
#[test]
fn extract_recent_lines_agent_message_explicit_first_line() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::agent_message("first\nSECOND\nTHIRD"));
    let out = crate::views::dashboard::peek::extract_recent_lines(agent, 6);
    assert_eq!(
        out,
        vec!["first".to_string()],
        "AgentMessage first-line projection must return exactly the first line",
    );
}
/// Each entry projects to its FIRST line.
#[test]
fn extract_recent_lines_first_line_only() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::user_prompt("line one\nline two\nline three"));
    let out = crate::views::dashboard::peek::extract_recent_lines(agent, 6);
    assert_eq!(out.len(), 1);
    assert!(
        out[0].contains("line one"),
        "first line must be present, got {:?}",
        out[0]
    );
    assert!(
        !out[0].contains("line two"),
        "second line must not appear, got {:?}",
        out[0]
    );
    assert!(
        !out[0].contains("line three"),
        "third line must not appear, got {:?}",
        out[0]
    );
}
/// Two entries returned in chronological order
/// (newest-last after the internal reverse).
#[test]
fn extract_recent_lines_chronological_order() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::user_prompt("first"));
    agent
        .scrollback
        .push_block(RenderBlock::user_prompt("second"));
    let out = crate::views::dashboard::peek::extract_recent_lines(agent, 6);
    assert_eq!(out.len(), 2);
    assert!(out[0].contains("first"));
    assert!(out[1].contains("second"));
}
/// ANSI escapes in scrollback content are stripped.
#[test]
fn extract_recent_lines_strips_ansi() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::user_prompt("hello \x1b[31mevil\x1b[0m world"));
    let out = crate::views::dashboard::peek::extract_recent_lines(agent, 6);
    assert_eq!(out.len(), 1);
    assert!(
        !out[0].contains('\x1b'),
        "ANSI must be stripped, got: {:?}",
        out[0]
    );
    assert!(out[0].contains("evil"));
}
/// A press > 2s after the first re-arms (does NOT close).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_double_press_after_2s_rearms() {
    use std::time::{Duration, Instant};
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    open_dashboard(&mut app);
    let target = *app.agents.keys().next().unwrap();
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let _ = dispatch_dashboard_stop(&mut app);
    if let Some(d) = app.dashboard.as_mut()
        && let Some((_row, at)) = d.delete_confirm.as_mut()
    {
        *at = Instant::now() - Duration::from_secs(3);
    }
    let before_count = app.agents.len();
    let _ = dispatch_dashboard_stop(&mut app);
    assert_eq!(app.agents.len(), before_count);
    assert!(app.dashboard.as_ref().unwrap().delete_confirm.is_some());
}
/// Subagent Ctrl+X bypasses confirm and emits KillSubagent.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_subagent_emits_kill_subagent_effect() {
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let info = make_test_subagent("child-xyz", "sa-xyz");
    agent
        .subagent_sessions
        .insert(info.child_session_id.to_string(), info);
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::Subagent {
            parent: AgentId(0),
            child_session_id: "child-xyz".to_string(),
        });
    }
    let effects = dispatch_dashboard_stop(&mut app);
    assert!(matches!(
        effects.as_slice(),
        [Effect::KillSubagent { subagent_id, .. }] if subagent_id == "sa-xyz"
    ));
    assert!(app.dashboard.as_ref().unwrap().delete_confirm.is_none());
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_delete_complete_returns_from_foreground_agent() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.session_id = Some(acp::SessionId::new("sess-dash"));
    open_dashboard(&mut app);
    app.active_view = ActiveView::Agent(id);
    let _ = dispatch_task_result(
        crate::app::actions::TaskResult::DeleteSessionComplete {
            source: "current".into(),
            session_id: "sess-dash".into(),
            after: crate::app::actions::AfterSessionDelete::Dashboard,
        },
        &mut app,
    );
    assert!(!app.agents.contains_key(&id));
    assert!(matches!(app.active_view, ActiveView::AgentDashboard));
}
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_busy_roster_toasts_without_arming() {
    let mut app = test_app();
    let mut entry = idle_roster_entry("busy-sess", "Busy row");
    entry.activity = crate::app::roster::RosterActivity::Working;
    app.dashboard_local_sessions = vec![entry];
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::Roster {
            session_id: "busy-sess".into(),
        });
    }
    assert!(dispatch_dashboard_stop(&mut app).is_empty());
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.delete_confirm.is_none());
    assert_eq!(
        d.error_toast.as_deref(),
        Some("Stop the session before deleting"),
    );
}
/// A busy top-level row: Ctrl+X cancels the turn and never arms delete.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_busy_top_level_cancels_without_arming() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    let target = *app.agents.keys().next().unwrap();
    {
        let agent = app.agents.get_mut(&target).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("busy-top"));
        agent.session.state = AgentState::TurnRunning;
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let effects = dispatch_dashboard_stop(&mut app);
    assert!(
        matches!(effects.as_slice(), [Effect::CancelTurn { .. }]),
        "busy top-level Ctrl+X must cancel the turn, got {effects:?}",
    );
    assert!(
        app.dashboard.as_ref().unwrap().delete_confirm.is_none(),
        "busy top-level Ctrl+X must NOT arm delete",
    );
    let effects = dispatch_dashboard_stop(&mut app);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::DeleteSession { .. })),
        "a busy row must never emit DeleteSession, got {effects:?}",
    );
    assert!(app.agents.contains_key(&target), "busy row must survive");
}
/// A row that's `Working` only due to background work (turn idle, a
/// scheduled `/loop` live): Ctrl+X stops the background work rather than
/// toasting, and never arms delete — so the row can settle to idle and
/// then be deleted.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_bg_work_row_stops_without_arming() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    let target = *app.agents.keys().next().unwrap();
    {
        let agent = app.agents.get_mut(&target).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("bg-loop"));
        agent.session.state = AgentState::Idle;
        agent.session.scheduled_tasks.insert(
            "loop-1".into(),
            crate::app::agent::ScheduledTaskInfo {
                task_id: "loop-1".into(),
                prompt: "keep going".into(),
                human_schedule: "every 5m".into(),
                created_at: std::time::Instant::now(),
                next_fire_at: None,
                tag: "loop".into(),
                last_subagent_id: None,
            },
        );
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let effects = dispatch_dashboard_stop(&mut app);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::DeleteScheduledTask { .. })),
        "Ctrl+X must stop the scheduled loop, got {effects:?}",
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::DeleteSession { .. })),
        "must not delete a bg-work row, got {effects:?}",
    );
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.delete_confirm.is_none(), "must not arm delete");
    assert!(d.error_toast.is_none(), "stopped work, so no toast");
}
/// Reverting stop-all to the wire default (`ClientUi`) would auto-wake.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_running_bg_task_emits_teardown_kill() {
    use echo_agent_runtime::extensions::task::TaskKillSource;
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    let target = *app.agents.keys().next().unwrap();
    {
        let agent = app.agents.get_mut(&target).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("bg-stop"));
        agent.session.state = AgentState::Idle;
        agent
            .session
            .bg_tasks
            .insert("bg-1".into(), super::make_bg_task("bg-1"));
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let effects = dispatch_dashboard_stop(&mut app);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::KillBgTask {
                task_id,
                source: TaskKillSource::Teardown,
                ..
            } if task_id == "bg-1"
        )),
        "dashboard stop-all must emit Teardown, got {effects:?}"
    );
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::KillBgTask {
                source: TaskKillSource::ClientUi,
                ..
            }
        )),
        "dashboard stop-all must not emit ClientUi, got {effects:?}"
    );
}
/// A row that's `Working` only because of a queued (unsent) prompt: Ctrl+X
/// drops the queue (local, no effect) rather than toasting, and never arms
/// — so the row settles to idle and can then be deleted.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_queued_prompt_row_drops_queue_without_arming() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    let target = *app.agents.keys().next().unwrap();
    {
        let agent = app.agents.get_mut(&target).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("queued"));
        agent.session.state = AgentState::Idle;
        agent.session.enqueue_prompt("do the thing".into());
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let effects = dispatch_dashboard_stop(&mut app);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::DeleteSession { .. })),
        "must not delete a queued-prompt row, got {effects:?}",
    );
    assert!(
        app.agents[&target].session.pending_prompts.is_empty(),
        "the queued prompt must be dropped",
    );
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.delete_confirm.is_none(), "must not arm delete");
    assert!(d.error_toast.is_none(), "dropped the queue, so no toast");
}
/// The `y` / second-`[✗]` confirm re-checks deletability: a row that
/// became busy between arming and confirming must not be deleted.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_delete_confirm_rechecks_settled_row() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    let target = *app.agents.keys().next().unwrap();
    {
        let agent = app.agents.get_mut(&target).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("recheck"));
        agent.session.state = AgentState::Idle;
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let _ = dispatch_dashboard_stop(&mut app);
    assert!(app.dashboard.as_ref().unwrap().delete_confirm.is_some());
    app.agents.get_mut(&target).unwrap().session.state = AgentState::TurnRunning;
    let effects = dispatch_dashboard_delete(&mut app);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::DeleteSession { .. })),
        "a row that went busy between gestures must not delete, got {effects:?}",
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().error_toast.as_deref(),
        Some("Stop the session before deleting"),
    );
}
/// A settled chat-conversation roster row must not arm on Ctrl+X — delete
/// isn't supported for conversations, so a confirm could never succeed.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_stop_conversation_row_does_not_arm() {
    let mut app = test_app();
    let mut entry = idle_roster_entry("conv-1", "Chat row");
    entry.origin.kind = "conversation".into();
    app.dashboard_local_sessions = vec![entry];
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::Roster {
            session_id: "conv-1".into(),
        });
    }
    assert!(dispatch_dashboard_stop(&mut app).is_empty());
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.delete_confirm.is_none(), "conversation row must not arm");
    assert_eq!(
        d.error_toast.as_deref(),
        Some("Deleting chat conversations isn't supported yet"),
    );
}
/// A row with no session id toasts instead of emitting a delete.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_delete_top_level_without_session_id_toasts() {
    let mut app = test_app();
    let _ = dispatch_new_session_inner(&mut app, None);
    let _ = dispatch_new_session_inner(&mut app, None);
    let target = *app.agents.keys().next().unwrap();
    app.agents.get_mut(&target).unwrap().session.session_id = None;
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(target));
    }
    let _ = dispatch_dashboard_stop(&mut app);
    let effects = dispatch_dashboard_stop(&mut app);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::DeleteSession { .. })),
        "a row without a session id must not delete, got {effects:?}",
    );
    assert_eq!(
        app.dashboard.as_ref().unwrap().error_toast.as_deref(),
        Some("No session history to delete"),
    );
}
/// Happy path — matching ids → no panic, queue popped.
/// Also assert the response was actually sent through
/// the oneshot (not just popped). A regression that pops without
/// sending the response would otherwise slip through this test.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_permission_select_happy_path() {
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let mut rx = push_synthetic_permission(agent, 42, vec![("allow", "Allow")]);
    open_dashboard(&mut app);
    let _ = dispatch_dashboard_permission_select(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        42,
        acp::PermissionOptionId::new(std::sync::Arc::from("allow")),
    );
    assert!(app.agents[&AgentId(0)].permission_queue.is_empty());
    let resp = rx
        .try_recv()
        .expect("response oneshot must have a value after dispatch");
    let resp = resp.expect("response must be Ok(RequestPermissionResponse)");
    match resp.outcome {
        acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome {
            option_id,
            ..
        }) => {
            assert_eq!(
                option_id.0.as_ref(),
                "allow",
                "selected option_id must round-trip"
            );
        }
        other => panic!("expected Selected outcome, got {other:?}"),
    }
}
/// Stale request_id — refuses, sets toast, clears peek.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_permission_select_drops_stale_request() {
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let _rx = push_synthetic_permission(agent, 99, vec![("allow", "Allow")]);
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
            crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
            crate::views::dashboard::peek::PeekFields {
                label: "label".into(),
                time_ago: String::new(),
                response_type: "Awaiting your input".into(),
                last_user_message: None,
                question: Some("q?".into()),
                options: vec![("allow".into(), "Allow".into())],
                request_id: Some(123),
                reject_option: None,
            },
        ));
    }
    let _ = dispatch_dashboard_permission_select(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        123,
        acp::PermissionOptionId::new(std::sync::Arc::from("allow")),
    );
    assert_eq!(app.agents[&AgentId(0)].permission_queue.len(), 1);
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.peek.is_none(), "peek must be closed");
    assert!(d.error_toast.is_some(), "toast must surface the mismatch");
}
/// Missing row — toasts, closes peek, returns no effects.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_permission_select_for_missing_row_clears_peek() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
            crate::views::dashboard::DashboardRowId::TopLevel(AgentId(99)),
            crate::views::dashboard::peek::PeekFields {
                label: "label".into(),
                time_ago: String::new(),
                response_type: "Idle".into(),
                last_user_message: None,
                question: None,
                options: Vec::new(),
                request_id: None,
                reject_option: None,
            },
        ));
    }
    let effects = dispatch_dashboard_permission_select(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(99)),
        1,
        acp::PermissionOptionId::new(std::sync::Arc::from("allow")),
    );
    assert!(effects.is_empty());
    let d = app.dashboard.as_ref().unwrap();
    assert!(d.peek.is_none());
    assert!(d.error_toast.is_some());
}
/// Peek reply to an IDLE agent sends immediately: the prompt drains
/// (one `SendPrompt` effect), the turn starts, and the reply draft
/// is cleared.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_peek_reply_to_idle_agent_sends() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
            0,
        )));
        d.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
            crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
            crate::views::dashboard::peek::PeekFields {
                label: "label".into(),
                time_ago: String::new(),
                response_type: "Idle".into(),
                last_user_message: None,
                question: None,
                options: Vec::new(),
                request_id: None,
                reject_option: None,
            },
        ));
        d.peek_reply.set_text("please continue");
    }
    let effects = dispatch_dashboard_peek_reply(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        "please continue".into(),
        false,
    );
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::SendPrompt { text, .. } if text == "please continue"));
    assert!(app.agents[&AgentId(0)].session.state.is_turn_running());
    assert_eq!(app.agents[&AgentId(0)].session.queue_len(), 0);
    assert!(app.dashboard.as_ref().unwrap().peek_reply.text().is_empty());
}
/// Peek reply to a RUNNING agent queues the prompt (no effect) so it
/// drains after the current turn; the draft is still cleared.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_peek_reply_to_running_agent_queues() {
    let mut app = test_app_with_agent();
    {
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.session.state = AgentState::TurnRunning;
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("current turn"));
        agent.scrollback.prepare_layout(80, 24);
        let current = agent.scrollback.len().saturating_sub(1);
        agent.scrollback.set_selected(Some(current));
        agent.scrollback.scroll_to_entry_top(current);
        agent.scrollback.enable_follow_with_preserve();
    }
    open_dashboard(&mut app);
    if let Some(d) = app.dashboard.as_mut() {
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
            0,
        )));
        d.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
            crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
            crate::views::dashboard::peek::PeekFields {
                label: "label".into(),
                time_ago: String::new(),
                response_type: "Running\u{2026}".into(),
                last_user_message: None,
                question: None,
                options: Vec::new(),
                request_id: None,
                reject_option: None,
            },
        ));
        d.peek_reply.set_text("after this");
        d.begin_peek_viewport(
            crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
            &mut app.agents,
        );
    }
    let effects = dispatch_dashboard_peek_reply(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        "after this".into(),
        false,
    );
    assert!(effects.is_empty());
    assert_eq!(app.agents[&AgentId(0)].session.queue_len(), 1);
    assert!(app.agents[&AgentId(0)].session.state.is_turn_running());
    let dashboard = app.dashboard.as_ref().unwrap();
    assert!(dashboard.peek_reply.text().is_empty());
    assert!(
        dashboard
            .peek_viewport
            .as_ref()
            .unwrap()
            .page_flip_entry
            .is_none(),
        "a blocked drain must not claim the current turn as the queued reply"
    );
}
/// Peek reply with an attached image drains into the queued
/// prompt and idle agents send `SendPromptBlocks` (not text-only).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_peek_reply_with_image_sends_blocks() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let reply_text = {
        let d = app.dashboard.as_mut().unwrap();
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
            0,
        )));
        d.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
            crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
            crate::views::dashboard::peek::PeekFields {
                label: "label".into(),
                time_ago: String::new(),
                response_type: "Idle".into(),
                last_user_message: None,
                question: None,
                options: Vec::new(),
                request_id: None,
                reject_option: None,
            },
        ));
        let img = crate::prompt_images::PastedImage {
            element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
            display_number: 0,
            mime_type: "image/png".into(),
            dimensions: Some((10, 10)),
            byte_len: 16,
            encoded_bytes: Some(vec![0u8; 16].into()),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: crate::prompt_images::PromptImagePreview::default(),
        };
        d.peek_reply.insert_image(img).unwrap();
        d.peek_reply.text().to_string()
    };
    let effects = dispatch_dashboard_peek_reply(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        reply_text,
        false,
    );
    assert_eq!(effects.len(), 1);
    assert!(
        matches!(&effects[0], Effect::SendPromptBlocks { .. }),
        "image reply must send blocks, got {:?}",
        effects[0]
    );
    assert!(app.dashboard.as_ref().unwrap().peek_reply.text().is_empty());
    assert!(app.dashboard.as_ref().unwrap().peek_reply.images.is_empty());
}
/// Regression: whitespace around a peek-reply image chip must not
/// desync chip ranges from the stored text (panicked on rewind restore).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_peek_reply_image_with_whitespace_survives_rewind_restore() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let reply_text = {
        let d = app.dashboard.as_mut().unwrap();
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
            0,
        )));
        d.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
            crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
            crate::views::dashboard::peek::PeekFields {
                label: "label".into(),
                time_ago: String::new(),
                response_type: "Idle".into(),
                last_user_message: None,
                question: None,
                options: Vec::new(),
                request_id: None,
                reject_option: None,
            },
        ));
        d.peek_reply.set_text("   ");
        d.peek_reply.set_cursor(3);
        let img = crate::prompt_images::PastedImage {
            element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
            display_number: 0,
            mime_type: "image/png".into(),
            dimensions: Some((10, 10)),
            byte_len: 16,
            encoded_bytes: Some(vec![0u8; 16].into()),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: crate::prompt_images::PromptImagePreview::default(),
        };
        d.peek_reply.insert_image(img).unwrap();
        d.peek_reply.text().to_string()
    };
    let _ = dispatch_dashboard_peek_reply(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        reply_text,
        false,
    );
    let stashed = app.agents[&AgentId(0)]
        .session
        .in_flight_prompt
        .clone()
        .expect("idle send sets in_flight_prompt");
    for chip in &stashed.chip_elements {
        assert!(
            chip.range.end <= stashed.text.len(),
            "chip range {:?} out of bounds for stored text {:?} (len {})",
            chip.range,
            stashed.text,
            stashed.text.len()
        );
    }
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.prompt.set_text(&stashed.text);
    agent.prompt.restore_chip_elements(&stashed.chip_elements);
    agent.prompt.set_images(stashed.images);
    assert!(agent.prompt.text().contains("[Image #1]"));
    assert_eq!(agent.prompt.images.len(), 1);
}
/// Image on peek reply is preserved on the queued entry when
/// the agent is mid-turn.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_peek_reply_with_image_queues_images() {
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().session.state = AgentState::TurnRunning;
    open_dashboard(&mut app);
    let reply_text = {
        let d = app.dashboard.as_mut().unwrap();
        d.selected = Some(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
            0,
        )));
        d.peek = Some(crate::views::dashboard::peek::PeekPanelState::new(
            crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
            crate::views::dashboard::peek::PeekFields {
                label: "label".into(),
                time_ago: String::new(),
                response_type: "Running\u{2026}".into(),
                last_user_message: None,
                question: None,
                options: Vec::new(),
                request_id: None,
                reject_option: None,
            },
        ));
        let img = crate::prompt_images::PastedImage {
            element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
            display_number: 0,
            mime_type: "image/png".into(),
            dimensions: Some((10, 10)),
            byte_len: 16,
            encoded_bytes: Some(vec![0u8; 16].into()),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: crate::prompt_images::PromptImagePreview::default(),
        };
        d.peek_reply.insert_image(img).unwrap();
        d.peek_reply.text().to_string()
    };
    let effects = dispatch_dashboard_peek_reply(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        reply_text,
        false,
    );
    assert!(effects.is_empty());
    let queued = app.agents[&AgentId(0)]
        .session
        .pending_prompts
        .front()
        .expect("queued prompt");
    assert_eq!(queued.images.len(), 1);
    assert!(
        !queued.chip_elements.is_empty(),
        "peek send must snapshot chips for cancel-restore"
    );
    assert!(app.dashboard.as_ref().unwrap().peek_reply.images.is_empty());
}
/// Replying to a subagent row is rejected with a toast (subagents are
/// driven by their parent turn).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_peek_reply_to_subagent_toasts() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let effects = dispatch_dashboard_peek_reply(
        &mut app,
        crate::views::dashboard::DashboardRowId::Subagent {
            parent: AgentId(0),
            child_session_id: "child-1".to_string(),
        },
        "hi".into(),
        false,
    );
    assert!(effects.is_empty());
    assert_eq!(app.agents[&AgentId(0)].session.queue_len(), 0);
    assert!(app.dashboard.as_ref().unwrap().error_toast.is_some());
}
/// Peek "No, type to add feedback" path: resolves the front
/// permission with the `RejectOnce` option and attaches the typed
/// text as `followup_message` meta.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_permission_followup_rejects_with_message() {
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let mut rx = push_synthetic_permission(agent, 5, vec![("allow", "Allow"), ("reject", "No")]);
    open_dashboard(&mut app);
    let effects = dispatch_dashboard_permission_followup(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        5,
        "do it differently".into(),
    );
    assert!(effects.is_empty());
    let resp = rx.try_recv().expect("response sent").expect("ok");
    match resp.outcome {
        acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome {
            option_id,
            ..
        }) => {
            assert_eq!(
                option_id.0.as_ref(),
                "reject",
                "must pick the RejectOnce option"
            );
        }
        other => panic!("expected Selected(reject), got {other:?}"),
    }
    let meta = resp.meta.expect("followup meta present");
    assert_eq!(
        meta.get("followup_message").and_then(|v| v.as_str()),
        Some("do it differently"),
    );
    assert_eq!(app.agents[&AgentId(0)].permission_queue.len(), 0);
}
/// Peek answering of the Ask tool (`AskUserQuestion`): selecting an
/// option sends the ext-response and clears the question view.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_question_answer_sends_and_clears() {
    use crate::views::prompt_widget::StashedPrompt;
    use crate::views::question_view::QuestionViewState;
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        AskUserQuestionMode, Question, QuestionOption,
    };
    let mut app = test_app_with_agent();
    let opt = |label: &str| QuestionOption {
        label: label.to_string(),
        description: String::new(),
        preview: None,
        id: None,
    };
    let question = Question {
        question: "Which DB?".to_string(),
        options: vec![opt("Redis"), opt("Postgres")],
        multi_select: Some(false),
        id: None,
    };
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let qv = QuestionViewState::with_response_tx(
        "tc-1".to_string(),
        vec![question],
        StashedPrompt::default(),
        Some(tx),
        AskUserQuestionMode::Default,
    );
    app.agents.get_mut(&AgentId(0)).unwrap().question_view = Some(qv);
    open_dashboard(&mut app);
    let effects = dispatch_dashboard_question_answer(
        &mut app,
        crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        Some(1),
        String::new(),
    );
    assert!(effects.is_empty());
    assert!(rx.try_recv().is_ok(), "ext-response must be sent");
    assert!(app.agents[&AgentId(0)].question_view.is_none());
}
/// A multi-question Ask form is walked one question at a time in the
/// peek: answering advances to the next question (no submit yet) and
/// resets the panel's per-question draft; the last answer submits.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_question_answer_walks_multiple_questions() {
    use crate::views::dashboard::peek::{PeekPanelState, compute_peek_fields};
    use crate::views::prompt_widget::StashedPrompt;
    use crate::views::question_view::QuestionViewState;
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        AskUserQuestionMode, Question, QuestionOption,
    };
    let mut app = test_app_with_agent();
    let opt = |label: &str| QuestionOption {
        label: label.to_string(),
        description: String::new(),
        preview: None,
        id: None,
    };
    let q1 = Question {
        question: "Which DB?".to_string(),
        options: vec![opt("Redis"), opt("Postgres")],
        multi_select: Some(false),
        id: None,
    };
    let q2 = Question {
        question: "Which cache?".to_string(),
        options: vec![opt("LRU"), opt("LFU")],
        multi_select: Some(false),
        id: None,
    };
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let qv = QuestionViewState::with_response_tx(
        "tc-1".to_string(),
        vec![q1, q2],
        StashedPrompt::default(),
        Some(tx),
        AskUserQuestionMode::Default,
    );
    app.agents.get_mut(&AgentId(0)).unwrap().question_view = Some(qv);
    open_dashboard(&mut app);
    let row = crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0));
    let fields = compute_peek_fields(&row, &app.agents).expect("ask surfaced");
    assert!(fields.question.as_deref().unwrap().starts_with("(1/2)"));
    assert!(fields.request_id.is_none());
    assert_eq!(fields.reject_option, Some(2));
    if let Some(d) = app.dashboard.as_mut() {
        let mut p = PeekPanelState::new(row.clone(), fields);
        p.selected_option = Some(1);
        d.peek = Some(p);
        d.peek_reply.set_text("stale draft");
    }
    let effects = dispatch_dashboard_question_answer(&mut app, row.clone(), Some(1), String::new());
    assert!(effects.is_empty());
    assert!(
        rx.try_recv().is_err(),
        "must not submit until the last question"
    );
    let qv = app.agents[&AgentId(0)].question_view.as_ref().unwrap();
    assert_eq!(qv.active_tab, 1, "advanced to the second question");
    let d = app.dashboard.as_ref().unwrap();
    assert_eq!(d.peek.as_ref().unwrap().selected_option, None);
    assert!(d.peek_reply.text().is_empty());
    let effects = dispatch_dashboard_question_answer(&mut app, row, Some(0), String::new());
    assert!(effects.is_empty());
    assert!(rx.try_recv().is_ok(), "ext-response sent after last answer");
    assert!(app.agents[&AgentId(0)].question_view.is_none());
    assert!(app.dashboard.as_ref().unwrap().peek.is_none());
}
/// The peek panel auto-opens when a row is selected (replacing the
/// new-session input) and closes when the selection clears (e.g. the
/// `[+ New Agent]` button is focused).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_peek_auto_opens_for_selected_row() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    open_dashboard(&mut app);
    let area = Rect::new(0, 0, 80, 40);
    let reg = crate::actions::ActionRegistry::defaults();
    app.dashboard
        .as_mut()
        .unwrap()
        .focus_row(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
            0,
        )));
    let mut buf = Buffer::empty(area);
    let _ = crate::views::dashboard::render_dashboard(
        &mut buf,
        area,
        app.dashboard.as_mut().unwrap(),
        &mut app.agents,
        &reg,
        None,
        &[],
        false,
        None,
    );
    assert!(
        app.dashboard.as_ref().unwrap().peek.is_some(),
        "peek must auto-open for a selected row",
    );
    app.dashboard.as_mut().unwrap().focus_new_agent_button();
    let mut buf2 = Buffer::empty(area);
    let _ = crate::views::dashboard::render_dashboard(
        &mut buf2,
        area,
        app.dashboard.as_mut().unwrap(),
        &mut app.agents,
        &reg,
        None,
        &[],
        false,
        None,
    );
    assert!(
        app.dashboard.as_ref().unwrap().peek.is_none(),
        "peek must close when no row is selected",
    );
}
/// End-to-end: a multi-line peek reply grows the peek box. Rendering
/// the dashboard with a 3-line `peek_reply` draft produces a TALLER
/// dispatch (peek) rect than the same dashboard with a single-line
/// draft — the box sizes to the reply content (Shift+Enter newlines).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn dashboard_peek_box_grows_for_multiline_reply() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let reg = crate::actions::ActionRegistry::defaults();
    let area = Rect::new(0, 0, 80, 40);
    let box_height_for = |reply_text: &str| -> u16 {
        let mut app = test_app_with_agent();
        mark_agent_nonempty(&mut app, AgentId(0));
        open_dashboard(&mut app);
        app.dashboard.as_mut().unwrap().focus_row(
            crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0)),
        );
        let render = |app: &mut AppView| {
            let mut buf = Buffer::empty(area);
            let _ = crate::views::dashboard::render_dashboard(
                &mut buf,
                area,
                app.dashboard.as_mut().unwrap(),
                &mut app.agents,
                &reg,
                None,
                &[],
                false,
                None,
            );
        };
        render(&mut app);
        app.dashboard
            .as_mut()
            .unwrap()
            .peek_reply
            .set_text(reply_text);
        render(&mut app);
        app.dashboard
            .as_ref()
            .unwrap()
            .peek_reply_rect
            .expect("peek reply rect recorded after render")
            .height
    };
    let single = box_height_for("one line");
    let multi = box_height_for("line one\nline two\nline three");
    assert_eq!(single, 1, "single-line reply occupies one row");
    assert!(
        multi > single,
        "multi-line reply must grow the peek reply area, single={single} multi={multi}",
    );
}
/// Empty (no-real-turn) local sessions are hidden from the dashboard:
/// `test_app_with_agent` builds an untitled, message-less, idle agent —
/// exactly the "New session" created on every pager launch.
#[test]
fn build_rows_hides_empty_idle_local_session() {
    use crate::views::dashboard::build_rows;
    let app = test_app_with_agent();
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    assert!(rows.is_empty(), "an empty idle session must not render");
}
/// An empty session that is actively working stays visible — its first
/// user message may not be in scrollback yet, but it is doing real work.
#[test]
fn build_rows_keeps_empty_working_local_session() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().session.state = AgentState::TurnRunning;
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    assert_eq!(rows.len(), 1, "an actively-working session stays visible");
}
/// A session with a generated title renders normally.
#[test]
fn build_rows_keeps_titled_local_session() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    assert_eq!(rows.len(), 1, "a titled session renders");
}
/// A pinned empty session is kept (explicit user intent overrides the
/// empty-session hide).
#[test]
fn build_rows_keeps_pinned_empty_local_session() {
    use crate::views::dashboard::build_rows;
    let app = test_app_with_agent();
    let mut pinned = std::collections::BTreeSet::new();
    pinned.insert(crate::views::dashboard::DashboardRowId::TopLevel(AgentId(
        0,
    )));
    let rows = build_rows(
        &app.agents,
        &pinned,
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    assert_eq!(rows.len(), 1, "a pinned empty session is kept");
}
#[test]
fn dashboard_attach_roster_focuses_existing_local_agent() {
    let mut app = test_app();
    dispatch(Action::NewSession, &mut app);
    let id = AgentId(0);
    dispatch(
        Action::TaskComplete(TaskResult::SessionCreated {
            agent_id: id,
            session_id: "local-owned".into(),
            models: None,
            scheduler_background_loops: None,
        }),
        &mut app,
    );
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .subagent_sessions
            .insert("child-1".into(), make_test_subagent("child-1", "sa-1"));
        agent.active_subagent = Some("child-1".into());
    }
    app.active_view = ActiveView::AgentDashboard;
    ensure_dashboard_state(&mut app);
    let count_before = app.agents.len();
    let effects = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::Roster {
            session_id: "local-owned".into(),
        },
    );
    assert!(effects.is_empty());
    assert!(matches!(app.active_view, ActiveView::Agent(a) if a == id));
    assert_eq!(app.agents.len(), count_before);
    assert!(app.agents[&id].active_subagent.is_none());
    assert_eq!(app.dashboard.as_ref().unwrap().attached_agent, Some(id));
}
/// A conversation-origin roster row attaches via the direct chat load —
/// never local resolution or GCS restore.
#[test]
fn dashboard_attach_conversation_roster_row_loads_as_chat() {
    let mut app = test_app();
    app.leader_mode = false;
    let mut entry = idle_roster_entry("conv-dash-1", "Backend chat");
    entry.cwd = String::new();
    entry.origin.kind = "conversation".into();
    app.dashboard_local_sessions = vec![entry];
    let effects = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::Roster {
            session_id: "conv-dash-1".into(),
        },
    );
    assert!(
        matches!(
            &effects[..],
            [Effect::LoadSession {
                session_id,
                session_cwd: None,
                chat_kind: true,
                ..
            }] if session_id == "conv-dash-1"
        ),
        "expected direct chat LoadSession, got {effects:?}"
    );
}
/// Canary: Build roster rows keep the cross-cwd disk resume.
#[test]
fn dashboard_attach_build_roster_row_keeps_disk_resume() {
    let mut app = test_app();
    app.leader_mode = false;
    app.dashboard_local_sessions = vec![idle_roster_entry("build-dash-1", "Fix login")];
    let effects = dispatch_dashboard_attach(
        &mut app,
        crate::views::dashboard::DashboardRowId::Roster {
            session_id: "build-dash-1".into(),
        },
    );
    assert!(
        matches!(
            &effects[..],
            [Effect::LoadSession {
                session_id,
                session_cwd: Some(cwd),
                chat_kind: false,
                ..
            }] if session_id == "build-dash-1" && cwd == std::path::Path::new("/repo")
        ),
        "expected Build disk resume with roster cwd, got {effects:?}"
    );
}
