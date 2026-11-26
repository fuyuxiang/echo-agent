//! Tests for async task-result application arms.

use super::super::task_result::{
    X11_PRIMARY_PASTE_HINT, maybe_show_x11_primary_paste_hint, show_clipboard_failure,
    wrap_host_image_request_eligible,
};
use super::*;
use echo_agent_runtime::session::unified_list::ListScope;

fn doctor_target(app: &AppView, id: AgentId) -> crate::app::actions::DoctorFixTarget {
    let agent = &app.agents[&id];
    crate::app::actions::DoctorFixTarget {
        agent_id: id,
        session_id: agent.session.session_id.clone(),
        session_binding_epoch: agent.session_binding_epoch,
        cwd: agent.session.cwd.clone(),
    }
}

#[test]
fn doctor_planning_promotes_initial_session_binding() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().unbind_session_id();
    let target = doctor_target(&app, id);
    app.agents
        .get_mut(&id)
        .unwrap()
        .bind_session_id("bound".into());
    dispatch_task_result(
        TaskResult::DoctorFixPlanned {
            target,
            result: Ok(crate::app::actions::DoctorPlanningOutcome::Plan(Box::new(
                crate::diagnostics::test_fix_plan(temp.path()),
            ))),
        },
        &mut app,
    );
    let Some(crate::views::question_view::LocalQuestionKind::DoctorFix { target, .. }) = app.agents
        [&id]
        .question_view
        .as_ref()
        .and_then(|question| question.local_kind.as_ref())
    else {
        panic!("planning must open the doctor modal");
    };
    assert_eq!(
        target.session_id.as_ref().map(|id| id.0.as_ref()),
        Some("bound")
    );
}

#[test]
fn doctor_planning_rejects_bind_replace_and_unbind_rebind() {
    let temp = tempfile::tempdir().unwrap();
    for replacement in ["bind-replace", "unbind-rebind"] {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        app.agents.get_mut(&id).unwrap().unbind_session_id();
        let target = doctor_target(&app, id);
        let agent = app.agents.get_mut(&id).unwrap();
        agent.bind_session_id("first".into());
        if replacement == "bind-replace" {
            agent.bind_session_id("second".into());
        } else {
            agent.unbind_session_id();
            agent.bind_session_id("first".into());
        }
        dispatch_task_result(
            TaskResult::DoctorFixPlanned {
                target,
                result: Ok(crate::app::actions::DoctorPlanningOutcome::Plan(Box::new(
                    crate::diagnostics::test_fix_plan(temp.path()),
                ))),
            },
            &mut app,
        );
        assert!(app.agents[&id].question_view.is_none(), "{replacement}");
        assert!(
            last_system_text(&app, id).contains("session changed"),
            "{replacement}"
        );
    }
}

#[test]
fn doctor_planning_opens_refuses_remote_and_rejects_stale_identity() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let target = doctor_target(&app, id);

    app.agents.get_mut(&id).unwrap().prompt.set_text("draft");
    let scrollback_len = app.agents[&id].scrollback.len();
    dispatch_task_result(
        TaskResult::DoctorFixPlanned {
            target: target.clone(),
            result: Ok(crate::app::actions::DoctorPlanningOutcome::Plan(Box::new(
                crate::diagnostics::test_fix_plan(temp.path()),
            ))),
        },
        &mut app,
    );
    assert_eq!(app.agents[&id].prompt.text(), "");
    assert_eq!(
        app.agents[&id].scrollback.len(),
        scrollback_len,
        "the confirmation preview belongs only in the question modal"
    );
    let question = app.agents[&id]
        .question_view
        .as_ref()
        .expect("doctor question")
        .questions
        .first()
        .expect("doctor question contents");
    assert!(
        question.options[0]
            .preview
            .as_deref()
            .is_some_and(|preview| preview.contains("Doctor Fix")),
        "the modal must retain the exact fix preview"
    );
    app.agents.get_mut(&id).unwrap().question_view = None;

    dispatch_task_result(
        TaskResult::DoctorFixPlanned {
            target: target.clone(),
            result: Ok(crate::app::actions::DoctorPlanningOutcome::RunLocally(
                "echoagent doctor fix ssh-wrap".to_owned(),
            )),
        },
        &mut app,
    );
    assert!(
        last_system_text(&app, id)
            .contains("On your local computer, run: echoagent doctor fix ssh-wrap")
    );

    app.agents
        .get_mut(&id)
        .unwrap()
        .bind_session_id("replacement".into());
    dispatch_task_result(
        TaskResult::DoctorFixPlanned {
            target: target.clone(),
            result: Ok(crate::app::actions::DoctorPlanningOutcome::Plan(Box::new(
                crate::diagnostics::test_fix_plan(temp.path()),
            ))),
        },
        &mut app,
    );
    assert!(app.agents[&id].question_view.is_none());
    assert!(last_system_text(&app, id).contains("session changed"));
}

#[test]
fn doctor_apply_completion_prefers_initiator_then_active_and_welcome_fallback() {
    let mut app = three_agent_app();
    let initiator = AgentId(0);
    let active = AgentId(1);
    app.active_view = ActiveView::Agent(active);
    let target = doctor_target(&app, initiator);

    dispatch_task_result(
        TaskResult::DoctorFixApplied {
            target: target.clone(),
            result: Err("stale plan".to_owned()),
        },
        &mut app,
    );
    assert_eq!(
        last_system_text(&app, initiator),
        "Could not apply the fix: stale plan"
    );

    app.agents.shift_remove(&initiator);
    dispatch_task_result(
        TaskResult::DoctorFixApplied {
            target: target.clone(),
            result: Err("apply failed".to_owned()),
        },
        &mut app,
    );
    assert_eq!(
        last_system_text(&app, active),
        "Could not apply the fix: apply failed"
    );

    app.agents.clear();
    app.active_view = ActiveView::Welcome;
    dispatch_task_result(
        TaskResult::DoctorFixApplied {
            target,
            result: Err("validator failed".to_owned()),
        },
        &mut app,
    );
    assert_eq!(
        app.startup_warnings.last().unwrap().message,
        "Could not apply the fix: validator failed"
    );
}

#[test]
fn doctor_apply_reload_success_does_not_claim_live_finding_disappeared() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let target = doctor_target(&app, id);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".tmux.conf");
    dispatch_task_result(
        TaskResult::DoctorFixApplied {
            target,
            result: Ok(crate::diagnostics::FixOutcome::new_for_test(
                crate::diagnostics::TMUX_CLIPBOARD_ID,
                crate::diagnostics::FixStatus::Applied,
                path.clone(),
                None,
                crate::diagnostics::FixActivation::RequiresReload,
                None,
            )),
        },
        &mut app,
    );
    let output = last_system_text(&app, id);
    assert!(
        output.starts_with(&format!(
            "Added `set -g set-clipboard on` to `{}`.",
            path.display()
        )),
        "{output}"
    );
    assert!(
        output.contains("Reload tmux with `tmux source-file"),
        "{output}"
    );
    assert!(output.contains("Run /doctor again to verify"), "{output}");
    assert!(!output.contains("0 issues"), "{output}");
    assert!(!output.contains("Environment\n"), "{output}");
}

#[test]
fn doctor_apply_success_only_renders_resolution_instructions() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let target = doctor_target(&app, id);
    let temp = tempfile::tempdir().unwrap();
    dispatch_task_result(
        TaskResult::DoctorFixApplied {
            target,
            result: Ok(crate::diagnostics::FixOutcome::new_for_test(
                crate::diagnostics::SSH_WRAP_ID,
                crate::diagnostics::FixStatus::Applied,
                temp.path().join(".bashrc"),
                None,
                crate::diagnostics::FixActivation::SatisfiedNow,
                Some(crate::diagnostics::ShellKind::Bash),
            )),
        },
        &mut app,
    );
    let output = last_system_text(&app, id);
    assert!(output.starts_with("Set up SSH wrapping in"), "{output}");
    assert!(output.contains("Start a new shell"), "{output}");
    assert!(!output.contains("Environment\n"), "{output}");
    assert!(!output.contains("Findings\n"), "{output}");
}

#[test]
fn stale_auth_copy_timeout_does_not_clear_newer_feedback() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: Some("https://cloud.echo-agent.invalid/auth".to_owned()),
        mode: AuthMode::Command,
    };

    let first_effects = crate::app::dispatch::router::dispatch_copy_auth_url(&mut app, |_| {
        crate::clipboard::ClipboardDelivery::Failed
    });
    let [
        Effect::ScheduleClearAuthCopyFeedback {
            generation: first_generation,
        },
    ] = first_effects.as_slice()
    else {
        panic!("first copy must schedule feedback clear");
    };

    let second_effects = crate::app::dispatch::router::dispatch_copy_auth_url(&mut app, |_| {
        crate::clipboard::ClipboardDelivery::Confirmed
    });
    let [
        Effect::ScheduleClearAuthCopyFeedback {
            generation: second_generation,
        },
    ] = second_effects.as_slice()
    else {
        panic!("second copy must schedule feedback clear");
    };
    assert_ne!(first_generation, second_generation);
    assert_eq!(
        app.auth_clipboard_delivery,
        Some(crate::clipboard::ClipboardDelivery::Confirmed)
    );

    dispatch_task_result(
        TaskResult::AuthCopyFeedbackTimeout {
            generation: *first_generation,
        },
        &mut app,
    );
    assert_eq!(
        app.auth_clipboard_delivery,
        Some(crate::clipboard::ClipboardDelivery::Confirmed),
        "the first copy's stale timeout must preserve the second feedback"
    );

    dispatch_task_result(
        TaskResult::AuthCopyFeedbackTimeout {
            generation: *second_generation,
        },
        &mut app,
    );
    assert_eq!(app.auth_clipboard_delivery, None);
}

#[test]
fn stale_workflows_result_does_not_repaint_replaced_session_modal() {
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().extensions_modal =
        Some(crate::views::extensions_modal::ExtensionsModalState::new(
            crate::views::extensions_modal::ExtensionsTab::Skills,
        ));
    let _ = dispatch(
        Action::TaskComplete(TaskResult::WorkflowsListLoaded {
            agent_id: AgentId(0),
            session_id: acp::SessionId::new("old-session"),
            result: Ok(vec![]),
        }),
        &mut app,
    );
    assert!(matches!(
        app.agents[&AgentId(0)]
            .extensions_modal
            .as_ref()
            .unwrap()
            .workflows_data,
        crate::views::extensions_modal::TabDataState::Loading
    ));
}

fn foreign_resume_hint(
    tool: echo_agent_foreign_sessions::ForeignSessionTool,
) -> echo_agent_foreign_sessions::RecentForeignSession {
    echo_agent_foreign_sessions::RecentForeignSession {
        tool,
        native_id: "native-session".into(),
        age: std::time::Duration::from_secs(30),
    }
}

#[test]
fn foreign_resume_results_require_launch_token_and_canonical_cwd() {
    let mut launch = test_app();
    launch.foreign_session_compat = echo_agent_foreign_sessions::EnabledForeignSessionSources {
        cursor: true,
        ..Default::default()
    };
    let Effect::CanonicalizeForeignResumeCwd {
        requested_cwd,
        launch_token,
    } = launch
        .begin_foreign_resume_detection()
        .expect("pristine launch schedules canonicalization")
    else {
        panic!("expected canonicalization effect");
    };
    let canonical_cwd = dunce::canonicalize(&requested_cwd).unwrap();
    let effects = dispatch(
        Action::TaskComplete(TaskResult::ForeignResumeCwdCanonicalized {
            requested_cwd: requested_cwd.clone(),
            canonical_cwd: Some(canonical_cwd.clone()),
            launch_token,
        }),
        &mut launch,
    );
    assert!(matches!(
        effects.as_slice(),
        [Effect::DetectForeignResumeHint {
            canonical_cwd: effect_cwd,
            launch_token: effect_token,
            ..
        }] if effect_cwd == &canonical_cwd && *effect_token == launch_token
    ));
    assert!(launch.foreign_resume_hint().is_none());

    dispatch(
        Action::TaskComplete(TaskResult::ForeignResumeHintDetected {
            canonical_cwd: canonical_cwd.clone(),
            launch_token,
            hint: Some(foreign_resume_hint(
                echo_agent_foreign_sessions::ForeignSessionTool::Cursor,
            )),
        }),
        &mut launch,
    );
    assert_eq!(
        launch.foreign_resume_hint().map(|hint| hint.tool),
        Some(echo_agent_foreign_sessions::ForeignSessionTool::Cursor)
    );

    let mut stale = test_app();
    stale.foreign_session_compat = launch.foreign_session_compat;
    let Effect::CanonicalizeForeignResumeCwd {
        requested_cwd,
        launch_token,
    } = stale.begin_foreign_resume_detection().unwrap()
    else {
        panic!("expected canonicalization effect");
    };
    let canonical_cwd = dunce::canonicalize(&requested_cwd).unwrap();
    dispatch(
        Action::TaskComplete(TaskResult::ForeignResumeHintDetected {
            canonical_cwd: canonical_cwd.clone(),
            launch_token: launch_token + 1,
            hint: Some(foreign_resume_hint(
                echo_agent_foreign_sessions::ForeignSessionTool::Codex,
            )),
        }),
        &mut stale,
    );
    assert!(stale.foreign_resume_hint().is_none());

    stale.cwd = tempfile::tempdir().unwrap().path().to_path_buf();
    dispatch(
        Action::TaskComplete(TaskResult::ForeignResumeCwdCanonicalized {
            requested_cwd,
            canonical_cwd: Some(canonical_cwd),
            launch_token,
        }),
        &mut stale,
    );
    assert!(stale.foreign_resume_hint().is_none());
}

#[test]
fn foreign_resume_result_rejects_startup_conflict_before_completion() {
    let mut app = test_app();
    app.foreign_session_compat = echo_agent_foreign_sessions::EnabledForeignSessionSources {
        cursor: true,
        ..Default::default()
    };
    let Effect::CanonicalizeForeignResumeCwd {
        requested_cwd,
        launch_token,
    } = app.begin_foreign_resume_detection().unwrap()
    else {
        panic!("expected canonicalization effect");
    };
    let canonical_cwd = dunce::canonicalize(&requested_cwd).unwrap();
    assert!(app.accept_foreign_resume_canonical_cwd(
        launch_token,
        &requested_cwd,
        Some(canonical_cwd.clone()),
    ));
    app.deferred_startup.new_session = true;

    dispatch(
        Action::TaskComplete(TaskResult::ForeignResumeHintDetected {
            canonical_cwd,
            launch_token,
            hint: Some(foreign_resume_hint(
                echo_agent_foreign_sessions::ForeignSessionTool::Cursor,
            )),
        }),
        &mut app,
    );

    assert!(app.foreign_resume_hint().is_none());
}

#[test]
fn x11_primary_hint_requires_canonical_full_miss_outcome() {
    use crate::app::actions::{ClipboardPasteCompletion, ClipboardPasteTarget};
    assert_eq!(
        X11_PRIMARY_PASTE_HINT,
        "Try Shift+Insert to paste selected text"
    );
    let target = ClipboardPasteTarget::AgentPrompt {
        agent_id: AgentId(0),
        images_dir: None,
        from_feedback_pane: false,
    };

    for completion in [
        ClipboardPasteCompletion::Handled,
        ClipboardPasteCompletion::Dropped,
        ClipboardPasteCompletion::Failed(
            crate::app::actions::ClipboardPasteFailure::AttachmentRead,
        ),
    ] {
        let mut app = test_app_with_agent();
        maybe_show_x11_primary_paste_hint(true, completion, &target, &mut app);
        assert!(
            app.agents[&AgentId(0)].toast.is_none(),
            "{completion:?} must not show full-miss guidance"
        );
    }

    let mut app = test_app_with_agent();
    maybe_show_x11_primary_paste_hint(false, ClipboardPasteCompletion::FullMiss, &target, &mut app);
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "non-X11 FullMiss must not show X11 guidance"
    );
}

#[test]
fn wrap_host_image_request_eligible_covers_full_miss_and_attachment_error_only() {
    use crate::app::actions::{ClipboardPasteCompletion, ClipboardPasteFailure};

    // A clean empty miss and a remote read *error* both fall through to the wrap
    // host-image request — the headless-SSH `echoagent wrap` image-paste fix.
    assert!(wrap_host_image_request_eligible(
        ClipboardPasteCompletion::FullMiss
    ));
    assert!(wrap_host_image_request_eligible(
        ClipboardPasteCompletion::Failed(ClipboardPasteFailure::AttachmentRead)
    ));

    // Every other outcome is a real dead end: it must keep toasting and never
    // solicit the host image.
    for completion in [
        ClipboardPasteCompletion::Handled,
        ClipboardPasteCompletion::Dropped,
        ClipboardPasteCompletion::Failed(ClipboardPasteFailure::TextRead),
        ClipboardPasteCompletion::Failed(ClipboardPasteFailure::TargetInsertion),
        ClipboardPasteCompletion::Failed(ClipboardPasteFailure::AlreadyReported),
    ] {
        assert!(
            !wrap_host_image_request_eligible(completion),
            "{completion:?} must not route to the wrap host-image request"
        );
    }
}

#[test]
fn x11_primary_hint_routes_to_originating_agent() {
    let mut app = test_app_with_agent();
    let origin = AgentId(0);
    let active = AgentId(1);
    let session = make_test_agent_session(&app, active, "active-session");
    app.agents
        .insert(active, AgentView::new(session, ScrollbackState::new()));
    app.active_view = ActiveView::Agent(active);
    let target = crate::app::actions::ClipboardPasteTarget::AgentPrompt {
        agent_id: origin,
        images_dir: None,
        from_feedback_pane: false,
    };

    maybe_show_x11_primary_paste_hint(
        true,
        crate::app::actions::ClipboardPasteCompletion::FullMiss,
        &target,
        &mut app,
    );

    assert_eq!(
        app.agents[&origin]
            .toast
            .as_ref()
            .map(|(msg, _)| msg.as_str()),
        Some(X11_PRIMARY_PASTE_HINT),
    );
    assert!(
        app.agents[&active].toast.is_none(),
        "an unrelated active agent must not receive X11 paste guidance"
    );
}

#[test]
fn x11_primary_hint_routes_to_originating_dashboard() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    app.active_view = ActiveView::Agent(AgentId(0));

    maybe_show_x11_primary_paste_hint(
        true,
        crate::app::actions::ClipboardPasteCompletion::FullMiss,
        &crate::app::actions::ClipboardPasteTarget::DashboardDispatch,
        &mut app,
    );

    assert_eq!(
        app.dashboard
            .as_ref()
            .and_then(|dashboard| dashboard.error_toast.as_deref()),
        Some(X11_PRIMARY_PASTE_HINT),
    );
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "an unrelated active agent must not receive dashboard guidance"
    );
}

#[test]
fn clipboard_failure_routes_to_originating_agent_without_duplicate() {
    let mut app = test_app_with_agent();
    let origin = AgentId(0);
    let active = AgentId(1);
    let session = make_test_agent_session(&app, active, "active-session");
    app.agents
        .insert(active, AgentView::new(session, ScrollbackState::new()));
    app.active_view = ActiveView::Agent(active);
    let target = crate::app::actions::ClipboardPasteTarget::AgentPrompt {
        agent_id: origin,
        images_dir: None,
        from_feedback_pane: false,
    };

    show_clipboard_failure(
        &target,
        crate::app::actions::ClipboardPasteFailure::TextRead,
        &mut app,
    );
    assert_eq!(
        app.agents[&origin]
            .toast
            .as_ref()
            .map(|(text, _)| text.as_str()),
        Some("Couldn't read clipboard text")
    );
    assert!(app.agents[&active].toast.is_none());

    app.agents[&origin].toast = Some(("Couldn't save pasted image".to_owned(), 90));
    show_clipboard_failure(
        &target,
        crate::app::actions::ClipboardPasteFailure::AlreadyReported,
        &mut app,
    );
    assert_eq!(
        app.agents[&origin]
            .toast
            .as_ref()
            .map(|(text, _)| text.as_str()),
        Some("Couldn't save pasted image")
    );
}

#[test]
fn clipboard_failure_routes_to_originating_dashboard() {
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    app.active_view = ActiveView::Agent(AgentId(0));

    show_clipboard_failure(
        &crate::app::actions::ClipboardPasteTarget::DashboardDispatch,
        crate::app::actions::ClipboardPasteFailure::AttachmentRead,
        &mut app,
    );

    assert_eq!(
        app.dashboard
            .as_ref()
            .and_then(|dashboard| dashboard.error_toast.as_deref()),
        Some("Couldn't read clipboard contents")
    );
    assert!(app.agents[&AgentId(0)].toast.is_none());
}

#[test]
fn marketplace_list_loaded_sanitizes_components_at_ingestion() {
    use crate::views::extensions_modal::{ExtensionsModalState, ExtensionsTab, TabDataState};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().extensions_modal =
        Some(ExtensionsModalState::new(ExtensionsTab::Marketplace));

    let mut entry = cta_entry("dirty", "not_installed");
    entry.components = Some(echo_agent_hooks_plugins_types::PluginComponents {
        skills: vec![echo_agent_hooks_plugins_types::ComponentItem {
            name: "evil\u{1b}[31mskill".into(),
            description: Some(format!("\u{7}{}", "d".repeat(300))),
        }],
        ..Default::default()
    });
    let response = echo_agent_hooks_plugins_types::MarketplaceListResponse {
        sources: vec![echo_agent_hooks_plugins_types::MarketplaceScanResult {
            source_name: "s".into(),
            source_kind: "git".into(),
            source_url_or_path: "https://example.com/repo.git".into(),
            plugins: vec![entry],
            error: None,
        }],
    };

    dispatch(
        Action::TaskComplete(TaskResult::MarketplaceListLoaded {
            agent_id: id,
            result: Ok(response),
        }),
        &mut app,
    );

    let modal = app.agents[&id].extensions_modal.as_ref().unwrap();
    let TabDataState::Loaded(ref data) = modal.marketplace_data else {
        panic!("marketplace data not loaded");
    };
    let components = data.sources[0].plugins[0].components.as_ref().unwrap();
    assert_eq!(components.skills[0].name, "evil[31mskill");
    let desc = components.skills[0].description.as_deref().unwrap();
    assert_eq!(desc.chars().count(), 120);
    assert!(desc.chars().all(|c| c == 'd'));
}

#[test]
fn plugins_action_success_sets_result_notice_and_autoreload_preserves_it() {
    use crate::views::extensions_modal::{ExtensionsModalState, ExtensionsTab};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        // Simulate a per-row action (`u` update) in flight on row 2.
        modal.pending_entry_index = Some(2);
        app.agents.get_mut(&id).unwrap().extensions_modal = Some(modal);
    }

    // A successful update surfaces its message as a row-anchored result notice
    // (the fix for "I can't tell what happened after r/u"), non-covering.
    dispatch(
        Action::TaskComplete(TaskResult::PluginsActionResult {
            agent_id: id,
            result: Ok(echo_agent_hooks_plugins_types::ActionOutcome {
                status: echo_agent_hooks_plugins_types::OutcomeStatus::Success,
                message: "user/abcd1234/my-plugin: updated".into(),
                requires_reload: true,
                requires_restart: false,
            }),
        }),
        &mut app,
    );
    {
        let modal = app.agents[&id].extensions_modal.as_ref().unwrap();
        let n = modal
            .result_notice
            .as_ref()
            .expect("success must set a result notice");
        assert_eq!(n.message, "user/abcd1234/my-plugin: updated");
        assert_eq!(n.entry_index, Some(2), "anchored to the acted row");
    }

    // The auto-reload chained from `requires_reload` returns a generic message;
    // it must NOT clobber the specific update notice already on screen.
    dispatch(
        Action::TaskComplete(TaskResult::PluginsActionResult {
            agent_id: id,
            result: Ok(echo_agent_hooks_plugins_types::ActionOutcome {
                status: echo_agent_hooks_plugins_types::OutcomeStatus::Success,
                message: "Plugin registry rebuilt: 5 plugin(s).".into(),
                requires_reload: false,
                requires_restart: false,
            }),
        }),
        &mut app,
    );
    let modal = app.agents[&id].extensions_modal.as_ref().unwrap();
    let n = modal.result_notice.as_ref().expect("notice still present");
    assert_eq!(
        n.message, "user/abcd1234/my-plugin: updated",
        "auto-reload must not clobber the triggering action's notice"
    );
}

#[test]
fn tab_wide_action_success_sets_tab_wide_result_notice() {
    use crate::views::extensions_modal::{ExtensionsModalState, ExtensionsTab};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        // Tab-wide reload carries no row anchor.
        modal.pending_entry_index = None;
        app.agents.get_mut(&id).unwrap().extensions_modal = Some(modal);
    }

    dispatch(
        Action::TaskComplete(TaskResult::PluginsActionResult {
            agent_id: id,
            result: Ok(echo_agent_hooks_plugins_types::ActionOutcome {
                status: echo_agent_hooks_plugins_types::OutcomeStatus::Success,
                message: "Plugin registry rebuilt: 7 plugin(s).".into(),
                requires_reload: false,
                requires_restart: false,
            }),
        }),
        &mut app,
    );
    let modal = app.agents[&id].extensions_modal.as_ref().unwrap();
    let n = modal.result_notice.as_ref().expect("result notice set");
    assert_eq!(n.entry_index, None, "tab-wide action → footer status line");
    assert_eq!(n.message, "Plugin registry rebuilt: 7 plugin(s).");
}

#[test]
fn uninstall_result_notice_is_footer_only_not_row_anchored() {
    use crate::views::extensions_modal::{ExtensionsModalState, ExtensionsTab};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        // A row action was in flight, but it's an uninstall — the row goes away.
        modal.pending_entry_index = Some(1);
        modal.last_plugins_action =
            Some(echo_agent_hooks_plugins_types::PluginsAction::Uninstall {
                plugin_id: "user/ab12/gone".into(),
                confirmed: true,
            });
        app.agents.get_mut(&id).unwrap().extensions_modal = Some(modal);
    }

    dispatch(
        Action::TaskComplete(TaskResult::PluginsActionResult {
            agent_id: id,
            result: Ok(echo_agent_hooks_plugins_types::ActionOutcome {
                status: echo_agent_hooks_plugins_types::OutcomeStatus::Success,
                message: "Uninstalled repo \"user/ab12/gone\" (1 plugin(s): gone)".into(),
                requires_reload: true,
                requires_restart: false,
            }),
        }),
        &mut app,
    );
    let modal = app.agents[&id].extensions_modal.as_ref().unwrap();
    let n = modal.result_notice.as_ref().expect("result notice set");
    assert_eq!(
        n.entry_index, None,
        "uninstall removes the row → footer-only, no stale row checkmark"
    );
}

#[test]
fn confirmation_required_builds_plugins_confirmation_with_confirmed_true() {
    use crate::views::extensions_modal::{
        ConfirmationAction, ExtensionsModalState, ExtensionsTab, ModalMessage,
    };

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let mut modal = ExtensionsModalState::new(ExtensionsTab::Plugins);
        modal.picker_state.selected = 2;
        modal.pending_entry_index = Some(3);
        modal.last_plugins_action =
            Some(echo_agent_hooks_plugins_types::PluginsAction::Uninstall {
                plugin_id: "user/ab12/gone".into(),
                confirmed: false,
            });
        app.agents.get_mut(&id).unwrap().extensions_modal = Some(modal);
    }

    dispatch(
        Action::TaskComplete(TaskResult::PluginsActionResult {
            agent_id: id,
            result: Ok(echo_agent_hooks_plugins_types::ActionOutcome {
                status: echo_agent_hooks_plugins_types::OutcomeStatus::ConfirmationRequired,
                message: "Uninstalling removes 2 plugins from this repository.".into(),
                requires_reload: false,
                requires_restart: false,
            }),
        }),
        &mut app,
    );

    let modal = app.agents[&id].extensions_modal.as_ref().unwrap();
    match &modal.modal_message {
        Some(ModalMessage::Confirmation {
            message,
            action,
            pending_entry_index,
        }) => {
            // Server message only; footer owns y/cancel hints.
            assert_eq!(
                message,
                "Uninstalling removes 2 plugins from this repository."
            );
            assert_eq!(*pending_entry_index, Some(3));
            assert_eq!(
                action,
                &ConfirmationAction::Plugins(
                    echo_agent_hooks_plugins_types::PluginsAction::Uninstall {
                        plugin_id: "user/ab12/gone".into(),
                        confirmed: true,
                    }
                )
            );
        }
        other => panic!("expected Confirmation overlay, got {other:?}"),
    }
}

/// Regression (Bugbot): a failed `echo.agent/subagent/cancel` RPC must NOT
/// finalize the row — the subagent may still be running. Only a shell
/// response of "nothing live" finalizes it.
#[test]
fn kill_rpc_failure_does_not_finalize_but_nothing_live_does() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let sid = acp::SessionId::new("test-session".to_owned());
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let mut info = make_test_subagent("child-1", "sa-1");
        info.pending_kill = true;
        agent.subagent_sessions.insert("child-1".into(), info);
    }

    // RPC failed → leave the row (the subagent may still be running).
    dispatch_task_result(
        TaskResult::KillSubagentComplete {
            session_id: sid.clone(),
            subagent_id: "sa-1".into(),
            outcome: SubagentKillOutcome::RpcFailed,
        },
        &mut app,
    );
    assert!(
        !app.agents[&id].subagent_sessions["child-1"].finished,
        "a failed cancel RPC must not finalize the row"
    );

    // Nothing live → no finish coming, so the pager finalizes the row.
    dispatch_task_result(
        TaskResult::KillSubagentComplete {
            session_id: sid,
            subagent_id: "sa-1".into(),
            outcome: SubagentKillOutcome::NothingLive { status: None },
        },
        &mut app,
    );
    assert!(
        app.agents[&id].subagent_sessions["child-1"].finished,
        "nothing-live must finalize the orphan row"
    );
}

/// An already-finished orphan's real terminal status (`NothingLive { status:
/// Some(..) }`) is forwarded to the finalized row, not flattened to "cancelled".
#[test]
fn kill_nothing_live_with_status_stamps_real_terminal_status() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let sid = acp::SessionId::new("test-session".to_owned());
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let mut info = make_test_subagent("child-1", "sa-1");
        info.pending_kill = true;
        agent.subagent_sessions.insert("child-1".into(), info);
    }

    dispatch_task_result(
        TaskResult::KillSubagentComplete {
            session_id: sid,
            subagent_id: "sa-1".into(),
            outcome: SubagentKillOutcome::NothingLive {
                status: Some("completed".into()),
            },
        },
        &mut app,
    );
    let info = &app.agents[&id].subagent_sessions["child-1"];
    assert!(info.finished, "already-finished orphan must be finalized");
    assert_eq!(
        info.status.as_deref(),
        Some("completed"),
        "the shell's real terminal status must be stamped, not 'cancelled'"
    );
}

#[test]
fn cancel_complete_does_nothing() {
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::TaskComplete(TaskResult::CancelComplete), &mut app);
    assert!(effects.is_empty());
}

#[test]
fn switch_model_complete_success_updates_model_and_pushes_message() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("echo-agent-4.5"));

    // Set up available models so the display name can be resolved.
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .models
        .available
        .insert(
            model_id.clone(),
            acp::ModelInfo::new(model_id.clone(), "EchoAgent 4.5".to_string()),
        );
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .model_switch_pending = true;

    let initial_scrollback = app.agents[&id].scrollback.len();

    let effects = dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id: model_id.clone(),
            effort: None,
            result: Ok(()),
            prev_model_id: None,
        }),
        &mut app,
    );

    // Pending flag cleared.
    assert!(!app.agents[&id].session.model_switch_pending);
    // Current model updated.
    assert_eq!(
        app.agents[&id].session.models.current,
        Some(model_id.clone())
    );
    // Success message pushed to scrollback.
    assert_eq!(app.agents[&id].scrollback.len(), initial_scrollback + 1);
    // PersistPreferredModel effect emitted.
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::PersistPreferredModel { model_id: mid, .. } if *mid == model_id.clone()
    ));
}

#[test]
fn switch_model_complete_skips_message_and_persist_when_unchanged() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("echo-agent-4.5"));

    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.models.available.insert(
        model_id.clone(),
        acp::ModelInfo::new(model_id.clone(), "EchoAgent 4.5".to_string()),
    );
    agent.session.models.current = Some(model_id.clone());
    agent.session.models.reasoning_effort = None;
    agent.session.model_switch_pending = true;

    let before = app.agents[&id].scrollback.len();
    let effects = dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id: model_id.clone(),
            effort: None,
            result: Ok(()),
            prev_model_id: None,
        }),
        &mut app,
    );

    assert!(!app.agents[&id].session.model_switch_pending);
    assert_eq!(app.agents[&id].scrollback.len(), before, "no message added");
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::PersistPreferredModel { .. })),
        "no persist effect for no-op switch"
    );
}

#[test]
fn switch_model_complete_persists_resolved_effort_from_catalog_meta() {
    use echo_agent_runtime::sampling::types::ReasoningEffort;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("byok-model-47"));

    let mut meta = serde_json::Map::new();
    meta.insert(
        "supportsReasoningEffort".into(),
        serde_json::Value::Bool(true),
    );
    meta.insert(
        "reasoningEffort".into(),
        serde_json::Value::String("xhigh".into()),
    );
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .models
        .available
        .insert(
            model_id.clone(),
            acp::ModelInfo::new(model_id.clone(), "BYOK Model 4.7".to_string())
                .meta(serde_json::Value::Object(meta).as_object().cloned()),
        );
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .model_switch_pending = true;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id: model_id.clone(),
            effort: None, // user typed `/model Blackbox 4.7` with no effort
            result: Ok(()),
            prev_model_id: None,
        }),
        &mut app,
    );

    assert_eq!(
        app.agents[&id].session.models.reasoning_effort,
        Some(ReasoningEffort::Xhigh)
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistPreferredModel {
            model_id: mid,
            reasoning_effort,
        } => {
            assert_eq!(*mid, model_id);
            assert_eq!(
                *reasoning_effort,
                Some(ReasoningEffort::Xhigh),
                "persisted effort must mirror the live UI effort so a \
                     fresh chat does not reset reasoning to a stale default",
            );
        }
        other => panic!("expected PersistPreferredModel, got {other:?}"),
    }
}

#[test]
fn switch_to_non_reasoning_model_clears_persisted_effort() {
    use echo_agent_runtime::sampling::types::ReasoningEffort;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("echo-agent-4.5"));

    // Simulate prior reasoning effort from a previous model.
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .models
        .reasoning_effort = Some(ReasoningEffort::High);

    // The new model does NOT have supportsReasoningEffort in its meta.
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .models
        .available
        .insert(
            model_id.clone(),
            acp::ModelInfo::new(model_id.clone(), "EchoAgent Build".to_string()),
        );
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .model_switch_pending = true;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id: model_id.clone(),
            effort: None,
            result: Ok(()),
            prev_model_id: None,
        }),
        &mut app,
    );

    assert_eq!(
        app.agents[&id].session.models.reasoning_effort, None,
        "reasoning_effort must be cleared when switching to a non-reasoning model",
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PersistPreferredModel {
            reasoning_effort, ..
        } => {
            assert_eq!(
                *reasoning_effort, None,
                "persisted effort must be None so config.toml clears the stale value",
            );
        }
        other => panic!("expected PersistPreferredModel, got {other:?}"),
    }
}

#[test]
fn switch_model_complete_failure_pushes_error_and_clears_pending() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("bad-model"));

    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .model_switch_pending = true;
    let old_current = app.agents[&id].session.models.current.clone();
    let initial_scrollback = app.agents[&id].scrollback.len();

    let effects = dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id,
            effort: None,
            result: Err(SwitchModelError::Other("model not found".into())),
            prev_model_id: None,
        }),
        &mut app,
    );

    assert!(effects.is_empty());
    // Pending flag cleared.
    assert!(!app.agents[&id].session.model_switch_pending);
    // Current model unchanged.
    assert_eq!(app.agents[&id].session.models.current, old_current);
    // Error message pushed to scrollback.
    assert_eq!(app.agents[&id].scrollback.len(), initial_scrollback + 1);
}

#[test]
fn switch_model_incompatible_agent_shows_question_modal() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("cursor-model"));

    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .model_switch_pending = true;
    let initial_scrollback = app.agents[&id].scrollback.len();

    let err = echo_agent_runtime::agent::config::ModelSwitchIncompatibleAgentError {
        code: "MODEL_SWITCH_INCOMPATIBLE_AGENT".into(),
        active_agent_type: "echo-agent-build".into(),
        required_agent_type: "cursor".into(),
        model_id: "cursor-model".into(),
        suggestion: "start_new_session".into(),
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id,
            effort: None,
            result: Err(SwitchModelError::IncompatibleAgent {
                error: err,
                prev_model_id: None,
            }),
            prev_model_id: None,
        }),
        &mut app,
    );

    // No effects emitted (modal is synchronous state).
    assert!(effects.is_empty());
    // Pending flag cleared.
    assert!(!app.agents[&id].session.model_switch_pending);
    // Question modal is open.
    assert!(app.agents[&id].question_view.is_some());
    let qv = app.agents[&id].question_view.as_ref().unwrap();
    assert!(matches!(
        qv.local_kind,
        Some(crate::views::question_view::LocalQuestionKind::AgentTypeMismatch { .. })
    ));
    // No error message pushed to scrollback.
    assert_eq!(app.agents[&id].scrollback.len(), initial_scrollback);
}

#[test]
fn incompatible_agent_rollback_restores_previous_model() {
    // When SetDefaultModel optimistically updates models.current and the
    // shell rejects with IncompatibleAgent, the handler must roll back
    // models.current to the prev_model_id.
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    let prev_model = acp::ModelId::new(std::sync::Arc::from("original-model"));
    let new_model = acp::ModelId::new(std::sync::Arc::from("cursor-model"));

    // Set up catalog with both models.
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.models.available.insert(
        prev_model.clone(),
        acp::ModelInfo::new(prev_model.clone(), "Original".to_string()),
    );
    agent.session.models.available.insert(
        new_model.clone(),
        acp::ModelInfo::new(new_model.clone(), "Cursor".to_string()),
    );
    // Simulate the optimistic update that set_default_model_inner does.
    agent.session.models.set_current(new_model.clone(), None);
    agent.session.model_switch_pending = true;

    assert_eq!(agent.session.models.current, Some(new_model.clone()));

    let err = echo_agent_runtime::agent::config::ModelSwitchIncompatibleAgentError {
        code: "MODEL_SWITCH_INCOMPATIBLE_AGENT".into(),
        active_agent_type: "echo-agent-build".into(),
        required_agent_type: "cursor".into(),
        model_id: "cursor-model".into(),
        suggestion: "start_new_session".into(),
    };
    dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id: new_model,
            effort: None,
            result: Err(SwitchModelError::IncompatibleAgent {
                error: err,
                prev_model_id: Some(prev_model.clone()),
            }),
            prev_model_id: Some(prev_model.clone()),
        }),
        &mut app,
    );

    // models.current must be rolled back to the previous model.
    assert_eq!(
        app.agents[&id].session.models.current,
        Some(prev_model),
        "models.current must be rolled back on IncompatibleAgent",
    );
}

#[test]
fn incompatible_agent_closes_active_modal() {
    use crate::views::modal::ActiveModal;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("cursor-model"));

    // Open any modal to verify it gets closed.
    let agent = app.agents.get_mut(&id).unwrap();
    agent.active_modal = Some(ActiveModal::CommandPalette {
        entries: vec![],
        state: crate::views::picker::PickerState::input_active(),
        window: crate::views::modal_window::ModalWindowState::new(),
    });
    agent.session.model_switch_pending = true;

    let err = echo_agent_runtime::agent::config::ModelSwitchIncompatibleAgentError {
        code: "MODEL_SWITCH_INCOMPATIBLE_AGENT".into(),
        active_agent_type: "echo-agent-build".into(),
        required_agent_type: "cursor".into(),
        model_id: "cursor-model".into(),
        suggestion: "start_new_session".into(),
    };
    dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id,
            effort: None,
            result: Err(SwitchModelError::IncompatibleAgent {
                error: err,
                prev_model_id: None,
            }),
            prev_model_id: None,
        }),
        &mut app,
    );

    // Active modal must be closed.
    assert!(
        app.agents[&id].active_modal.is_none(),
        "active modal must be closed when IncompatibleAgent fires",
    );
    // Question modal must be open.
    assert!(
        app.agents[&id].question_view.is_some(),
        "question modal must be open",
    );
}

#[test]
fn same_agent_type_switch_no_modal() {
    // Switching between two models with the same (or no) agent type
    // should succeed normally — no modal, no IncompatibleAgent error.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_a = acp::ModelId::new(std::sync::Arc::from("echo-agent-build-a"));
    let model_b = acp::ModelId::new(std::sync::Arc::from("echo-agent-build-b"));

    // Add both models to the catalog (no agentType → both use echo-agent-build).
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.models.available.insert(
        model_a.clone(),
        acp::ModelInfo::new(model_a.clone(), "EchoAgent Build A".to_string()),
    );
    agent.session.models.set_current(model_a, None);
    agent.session.models.available.insert(
        model_b.clone(),
        acp::ModelInfo::new(model_b.clone(), "EchoAgent Build B".to_string()),
    );
    agent.session.model_switch_pending = true;

    // Shell returns Ok (same agent type, no mismatch).
    let effects = dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id: model_b.clone(),
            effort: None,
            result: Ok(()),
            prev_model_id: None,
        }),
        &mut app,
    );

    // Model should be switched, no modal.
    assert_eq!(app.agents[&id].session.models.current, Some(model_b));
    assert!(app.agents[&id].question_view.is_none());
    // Should emit PersistPreferredModel.
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::PersistPreferredModel { .. }))
    );
}

#[test]
fn switch_model_pending_lifecycle() {
    // Full lifecycle: false -> dispatch SwitchModel -> true -> SwitchModelComplete -> false
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("echo-agent-4.5"));

    // Initially false.
    assert!(!app.agents[&id].session.model_switch_pending);

    // Action sets pending.
    dispatch(
        Action::SwitchModel {
            model_id: model_id.clone(),
            effort: None,
        },
        &mut app,
    );
    assert!(app.agents[&id].session.model_switch_pending);

    // TaskResult clears pending.
    dispatch(
        Action::TaskComplete(TaskResult::SwitchModelComplete {
            agent_id: id,
            model_id,
            effort: None,
            result: Ok(()),
            prev_model_id: None,
        }),
        &mut app,
    );
    assert!(!app.agents[&id].session.model_switch_pending);
}

#[test]
fn no_deferred_switch_means_no_extra_effect() {
    // When there is no deferred model switch, SessionCreated should
    // not produce a SwitchModel effect.
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    // Remove session_id to simulate pre-session state.
    app.agents.get_mut(&id).unwrap().session.session_id = None;
    assert!(app.agents[&id].session.deferred_model_switch.is_none());

    let effects = dispatch(
        Action::TaskComplete(TaskResult::SessionCreated {
            agent_id: id,
            session_id: "new-session".into(),
            models: None,
            scheduler_background_loops: None,
        }),
        &mut app,
    );

    // No SwitchModel effect.
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::SwitchModel { .. }))
    );
    assert!(!app.agents[&id].session.model_switch_pending);
}

#[test]
fn session_success_arms_finish_startup_obligation() {
    echo_agent_telemetry::unified_log::redirect_to_temp_for_tests();
    let id = AgentId(0);
    let results = [
        TaskResult::SessionCreated {
            agent_id: id,
            session_id: "new-session".into(),
            models: None,
            scheduler_background_loops: None,
        },
        TaskResult::SessionLoaded {
            agent_id: id,
            session_id: "resumed-session".into(),
            models: None,
            code_restored: false,
            restore_summary: None,
            restore_degree: None,
            running_prompt_id: None,
            scheduler_background_loops: None,
        },
        TaskResult::WorktreeSessionCreated {
            agent_id: id,
            session_id: "worktree-session".into(),
            worktree_path: std::path::PathBuf::from("/tmp/wt"),
            session_cwd: std::path::PathBuf::from("/tmp/wt"),
            models: None,
            scheduler_background_loops: None,
        },
        TaskResult::WorktreeForked {
            agent_id: id,
            session_id: "forked-session".into(),
            worktree_path: std::path::PathBuf::from("/tmp/wt"),
            session_cwd: std::path::PathBuf::from("/tmp/wt"),
            code_restored: false,
            restore_summary: None,
            restore_degree: None,
            resume_session_id: None,
        },
    ];

    for result in results {
        let mut app = test_app_with_agent();
        app.agents.get_mut(&id).unwrap().session.session_id = None;
        app.pending_startup = Some(echo_agent_telemetry::startup::PendingStartup::new());
        let label = format!("{result:?}");

        dispatch(Action::TaskComplete(result), &mut app);

        assert!(
            app.pending_startup.is_none(),
            "a usable session must take the startup obligation: {label}",
        );
    }
}

#[test]
fn bundle_status_ready_populates_state() {
    let mut app = test_app();

    dispatch(
        Action::TaskComplete(TaskResult::BundleStatusReady {
            has_cache: true,
            version: Some("v2".into()),
            personas: vec!["researcher".into(), "auditor".into()],
            roles: vec!["reviewer".into()],
            agents: vec!["default".into()],
            skills: vec!["commit".into(), "code-review".into()],
            persona_details: vec![crate::app::bundle::PersonaDetail {
                name: "researcher".into(),
                description: Some("thorough researcher".into()),
                has_inputs: true,
                has_outputs: false,
                source_path: None,
                scope_label: None,
            }],
            role_details: vec![crate::app::bundle::RoleDetail {
                name: "reviewer".into(),
                description: "code reviewer".into(),
            }],
        }),
        &mut app,
    );

    assert!(app.bundle_state.has_cache);
    assert_eq!(app.bundle_state.version, "v2");
    assert_eq!(app.bundle_state.personas, vec!["researcher", "auditor"]);
    assert_eq!(app.bundle_state.roles, vec!["reviewer"]);
    assert_eq!(app.bundle_state.agents, vec!["default"]);
    assert_eq!(app.bundle_state.skills, vec!["commit", "code-review"]);
    assert_eq!(app.bundle_state.persona_details.len(), 1);
    assert_eq!(app.bundle_state.persona_details[0].name, "researcher");
    assert_eq!(app.bundle_state.role_details.len(), 1);
    assert_eq!(app.bundle_state.role_details[0].name, "reviewer");
}

#[test]
fn bundle_status_failed_logs_but_keeps_state() {
    let mut app = test_app();
    app.bundle_state.has_cache = true;
    app.bundle_state.version = "keep-me".into();

    dispatch(
        Action::TaskComplete(TaskResult::BundleStatusFailed {
            error: "status fetch failed".into(),
        }),
        &mut app,
    );

    assert!(app.bundle_state.has_cache);
    assert_eq!(app.bundle_state.version, "keep-me");
}

#[test]
fn catalog_entry_ready_opens_viewer() {
    let mut app = test_app_with_agent();

    dispatch(
        Action::TaskComplete(TaskResult::CatalogEntryReady {
            kind: "persona".into(),
            name: "researcher".into(),
            content: "instructions = \"deep research\"".into(),
        }),
        &mut app,
    );

    let ActiveView::Agent(id) = app.active_view else {
        panic!("expected agent view");
    };
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.block_viewer.is_some());
    let viewer = agent.block_viewer.as_ref().unwrap();
    assert_eq!(
        viewer.kind,
        crate::views::block_viewer::ViewerKind::PlainText
    );
}

#[test]
fn catalog_entry_failed_shows_system_message() {
    let mut app = test_app_with_agent();
    let initial_len = {
        let ActiveView::Agent(id) = app.active_view else {
            panic!("expected agent view");
        };
        app.agents[&id].scrollback.len()
    };

    let effects = dispatch(
        Action::TaskComplete(TaskResult::CatalogEntryFailed {
            error: "not found".into(),
        }),
        &mut app,
    );

    assert!(effects.is_empty());
    let ActiveView::Agent(id) = app.active_view else {
        panic!("expected agent view");
    };
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.block_viewer.is_none());
    assert!(agent.scrollback.len() > initial_len);
}

#[test]
fn available_commands_refreshed_updates_generation() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let gen_before = app.agents[&id].session.available_commands_generation;

    let commands = vec![
        acp::AvailableCommand::new("commit", "Create a commit").meta(
            serde_json::json!({"scope": "local", "path": "/skill/SKILL.md"})
                .as_object()
                .cloned(),
        ),
    ];
    let effects = dispatch(
        Action::TaskComplete(TaskResult::AvailableCommandsRefreshed {
            agent_id: id,
            commands,
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(
        app.agents[&id].session.available_commands_generation,
        gen_before + 1
    );
    assert_eq!(app.agents[&id].session.available_commands.len(), 1);
    assert_eq!(app.agents[&id].session.available_commands[0].name, "commit");
}

#[test]
fn available_commands_refreshed_empty_is_noop() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let gen_before = app.agents[&id].session.available_commands_generation;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::AvailableCommandsRefreshed {
            agent_id: id,
            commands: vec![],
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(
        app.agents[&id].session.available_commands_generation,
        gen_before,
    );
}

// -- Session deletion from the /resume picker -----------------------

#[test]
fn delete_session_complete_removes_only_matching_source_and_id() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    let mut foreign_same_id = make_picker_entry("s1", "/r");
    foreign_same_id.source = "codex".into();
    let mut remote_same_id = make_picker_entry("s1", "/r");
    remote_same_id.source = "remote".into();
    open_session_picker_with(
        &mut app,
        vec![
            make_picker_entry("s0", "/r"),
            foreign_same_id,
            make_picker_entry("s1", "/r"),
            remote_same_id,
            make_picker_entry("s2", "/r"),
        ],
    );
    let mut welcome_foreign = make_picker_entry("s1", "/r");
    welcome_foreign.source = "codex".into();
    let mut welcome_remote = make_picker_entry("s1", "/r");
    welcome_remote.source = "remote".into();
    app.session_picker_entries = Some(vec![
        welcome_foreign,
        make_picker_entry("s1", "/r"),
        welcome_remote,
    ]);
    if let Some(ActiveModal::SessionPicker { pending_delete, .. }) = get_active_agent_mut(&mut app)
        .unwrap()
        .active_modal
        .as_mut()
    {
        *pending_delete = Some(crate::views::session_picker::PendingDelete {
            source: "local".into(),
            session_id: "s1".into(),
            cwd: "/r".into(),
        });
    }

    let _ = dispatch_task_result(
        TaskResult::DeleteSessionComplete {
            source: "local".into(),
            session_id: "s1".into(),
            after: crate::app::actions::AfterSessionDelete::Stay,
        },
        &mut app,
    );

    let agent = get_active_agent(&app).expect("active agent");
    let Some(ActiveModal::SessionPicker {
        entries: Some(list),
        pending_delete,
        ..
    }) = agent.active_modal.as_ref()
    else {
        panic!("expected SessionPicker modal");
    };
    let identities: Vec<_> = list
        .iter()
        .map(|entry| (entry.source.as_str(), entry.id.as_str()))
        .collect();
    assert_eq!(
        identities,
        vec![
            ("local", "s0"),
            ("codex", "s1"),
            ("remote", "s1"),
            ("local", "s2"),
        ]
    );
    assert!(
        pending_delete.is_none(),
        "pending_delete must be cleared after deletion completes"
    );
    let welcome_identities: Vec<_> = app
        .session_picker_entries
        .as_ref()
        .unwrap()
        .iter()
        .map(|entry| (entry.source.as_str(), entry.id.as_str()))
        .collect();
    assert_eq!(welcome_identities, vec![("codex", "s1"), ("remote", "s1")]);
}

#[test]
fn delete_both_session_clears_modal_and_welcome_content_hits() {
    use crate::views::modal::ActiveModal;
    use crate::views::session_picker::{PickerItem, SourceFilter, build_entry_map};

    let mut app = test_app_with_agent();
    let mut both = make_picker_entry("shared", "/r");
    both.source = "both".into();
    let mut foreign = make_picker_entry("shared", "/r");
    foreign.source = "codex".into();
    open_session_picker_with(&mut app, vec![both.clone(), foreign.clone()]);
    let hit = echo_agent_runtime::extensions::session_search::SearchSessionHit {
        session_id: "shared".into(),
        summary: "shared".into(),
        cwd: "/r".into(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        snippet: Some("deleted content".into()),
        score: 1.0,
        matched_fields: vec![],
    };
    if let Some(ActiveModal::SessionPicker {
        state,
        content_results,
        ..
    }) = get_active_agent_mut(&mut app)
        .unwrap()
        .active_modal
        .as_mut()
    {
        state.set_query("shared");
        *content_results = Some(vec![hit.clone()]);
    }
    app.session_picker_entries = Some(vec![both, foreign]);
    app.session_picker_state.set_query("shared");
    app.session_picker_content_results = Some(vec![hit]);

    let _ = dispatch_task_result(
        TaskResult::DeleteSessionComplete {
            source: "both".into(),
            session_id: "shared".into(),
            after: crate::app::actions::AfterSessionDelete::Stay,
        },
        &mut app,
    );

    let agent = get_active_agent(&app).unwrap();
    let Some(ActiveModal::SessionPicker {
        entries: Some(modal_entries),
        content_results: Some(modal_hits),
        state: modal_state,
        ..
    }) = agent.active_modal.as_ref()
    else {
        panic!("expected modal picker");
    };
    assert_eq!(
        modal_entries
            .iter()
            .map(|entry| (entry.source.as_str(), entry.id.as_str()))
            .collect::<Vec<_>>(),
        vec![("codex", "shared")]
    );
    assert!(modal_hits.is_empty());
    let modal_map = build_entry_map(
        Some(modal_entries),
        Some(modal_hits),
        "shared",
        true,
        false,
        SourceFilter::All,
        None,
    );
    assert!(
        !modal_map
            .iter()
            .any(|item| matches!(item, Some(PickerItem::Content { .. })))
    );
    assert_eq!(modal_state.query(), "shared");

    let welcome_entries = app.session_picker_entries.as_deref().unwrap();
    let welcome_hits = app.session_picker_content_results.as_deref().unwrap();
    assert_eq!(
        welcome_entries
            .iter()
            .map(|entry| (entry.source.as_str(), entry.id.as_str()))
            .collect::<Vec<_>>(),
        vec![("codex", "shared")]
    );
    assert!(welcome_hits.is_empty());
    let welcome_map = build_entry_map(
        Some(welcome_entries),
        Some(welcome_hits),
        "shared",
        false,
        false,
        SourceFilter::All,
        None,
    );
    assert!(
        !welcome_map
            .iter()
            .any(|item| matches!(item, Some(PickerItem::Content { .. })))
    );
}

#[test]
fn delete_remote_session_clears_modal_and_welcome_content_hits() {
    use crate::views::modal::ActiveModal;

    let mut app = test_app_with_agent();
    let mut remote = make_picker_entry("remote-only", "/r");
    remote.source = "remote".into();
    open_session_picker_with(&mut app, vec![remote.clone()]);
    let hit = echo_agent_runtime::extensions::session_search::SearchSessionHit {
        session_id: "remote-only".into(),
        summary: "remote-only".into(),
        cwd: "/r".into(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        snippet: Some("stale deleted content".into()),
        score: 1.0,
        matched_fields: vec![],
    };
    if let Some(ActiveModal::SessionPicker {
        content_results, ..
    }) = get_active_agent_mut(&mut app)
        .unwrap()
        .active_modal
        .as_mut()
    {
        *content_results = Some(vec![hit.clone()]);
    }
    app.session_picker_entries = Some(vec![remote]);
    app.session_picker_content_results = Some(vec![hit]);

    let _ = dispatch_task_result(
        TaskResult::DeleteSessionComplete {
            source: "remote".into(),
            session_id: "remote-only".into(),
            after: crate::app::actions::AfterSessionDelete::Stay,
        },
        &mut app,
    );

    let Some(ActiveModal::SessionPicker {
        entries: Some(modal_entries),
        content_results: Some(modal_hits),
        ..
    }) = app.agents[&AgentId(0)].active_modal.as_ref()
    else {
        panic!("expected modal picker");
    };
    assert!(modal_entries.is_empty());
    assert!(modal_hits.is_empty());
    assert!(app.session_picker_entries.as_ref().unwrap().is_empty());
    assert!(
        app.session_picker_content_results
            .as_ref()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn delete_session_failed_keeps_all_entries() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    open_session_picker_with(
        &mut app,
        vec![make_picker_entry("s0", "/r"), make_picker_entry("s1", "/r")],
    );

    let _ = dispatch_task_result(
        TaskResult::DeleteSessionFailed {
            source: "local".into(),
            session_id: "s1".into(),
            error: "boom".into(),
        },
        &mut app,
    );

    let agent = get_active_agent(&app).expect("active agent");
    let Some(ActiveModal::SessionPicker {
        entries: Some(list),
        ..
    }) = agent.active_modal.as_ref()
    else {
        panic!("expected SessionPicker modal");
    };
    assert_eq!(list.len(), 2, "a failed delete must not remove any entry");
}

#[test]
fn rename_session_failed_keeps_local_display_name_and_pushes_system_block() {
    // Pins the documented design decision: a failed on-disk rename
    // does NOT roll back the local `display_name` cache (the cache
    // is the source of truth for the modal's rendering and the
    // disk write is best-effort). The user sees the failure via
    // the system-block message instead.
    let mut app = test_app_with_agent();
    // Seed the cache as the rename path would have done.
    if let Some(a) = app.agents.get_mut(&AgentId(0)) {
        a.display_name = Some("optimistic title".into());
    }
    let scrollback_len_before = app.agents[&AgentId(0)].scrollback.len();

    let _effects = dispatch_task_result(
        TaskResult::RenameSessionFailed {
            agent_id: AgentId(0),
            error: "boom".into(),
        },
        &mut app,
    );

    // No rollback.
    assert_eq!(
        app.agents[&AgentId(0)].display_name.as_deref(),
        Some("optimistic title"),
        "display_name must NOT roll back on RenameSessionFailed"
    );
    // System block appended with the error.
    let scrollback = &app.agents[&AgentId(0)].scrollback;
    assert_eq!(
        scrollback.len(),
        scrollback_len_before + 1,
        "system block must be appended"
    );
    let last = scrollback.entry(scrollback.len() - 1).expect("last entry");
    let text = match &last.block {
        crate::scrollback::block::RenderBlock::System(b) => b.text.clone(),
        other => panic!("expected System block, got {other:?}"),
    };
    assert!(
        text.contains("Couldn't rename session: boom"),
        "system block must surface the error; got: {text:?}"
    );
}

#[test]
fn reset_session_title_failed_restores_pin_and_pushes_system_block() {
    let mut app = test_app_with_agent();
    if let Some(a) = app.agents.get_mut(&AgentId(0)) {
        a.display_name = Some("Manual".into());
        a.generated_session_title = Some("Auto".into());
    }
    let _ = dispatch_reset_session_title(&mut app);
    assert!(app.agents[&AgentId(0)].display_name.is_none());
    assert_eq!(
        app.agents[&AgentId(0)].generated_session_title.as_deref(),
        Some("Auto")
    );
    let scrollback_len_before = app.agents[&AgentId(0)].scrollback.len();

    let _effects = dispatch_task_result(
        TaskResult::ResetSessionTitleFailed {
            agent_id: AgentId(0),
            error: "boom".into(),
            previous_display_name: Some("Manual".into()),
            previous_generated_title: Some("Auto".into()),
        },
        &mut app,
    );

    assert_eq!(
        app.agents[&AgentId(0)].display_name.as_deref(),
        Some("Manual"),
        "failed unpin must restore the optimistic-cleared pin"
    );
    assert_eq!(
        app.agents[&AgentId(0)].generated_session_title.as_deref(),
        Some("Auto"),
        "failed unpin must restore the pre-clear generated title"
    );
    let scrollback = &app.agents[&AgentId(0)].scrollback;
    assert_eq!(
        scrollback.len(),
        scrollback_len_before + 1,
        "system block must be appended"
    );
    let last = scrollback.entry(scrollback.len() - 1).expect("last entry");
    let text = match &last.block {
        crate::scrollback::block::RenderBlock::System(b) => b.text.clone(),
        other => panic!("expected System block, got {other:?}"),
    };
    assert!(
        text.contains("Couldn't reset session title: boom"),
        "system block must surface the error; got: {text:?}"
    );
}

#[test]
fn reset_session_title_failed_does_not_restore_after_unpin_fanout() {
    let mut app = test_app_with_agent();
    if let Some(a) = app.agents.get_mut(&AgentId(0)) {
        a.display_name = Some("Manual".into());
        a.generated_session_title = Some("Auto".into());
    }
    let _ = dispatch_reset_session_title(&mut app);
    if let Some(a) = app.agents.get_mut(&AgentId(0)) {
        a.title_unpin_committed = true;
        a.display_name = None;
        a.generated_session_title = Some("Auto".into());
    }

    let _effects = dispatch_task_result(
        TaskResult::ResetSessionTitleFailed {
            agent_id: AgentId(0),
            error: "transport dropped".into(),
            previous_display_name: Some("Manual".into()),
            previous_generated_title: Some("Auto".into()),
        },
        &mut app,
    );

    let agent = &app.agents[&AgentId(0)];
    assert!(
        agent.display_name.is_none(),
        "dropped RPC after fan-out must not re-pin"
    );
    assert_eq!(agent.generated_session_title.as_deref(), Some("Auto"));
    assert!(!agent.title_unpin_committed);
    let last = agent
        .scrollback
        .entry(agent.scrollback.len() - 1)
        .expect("last entry");
    let text = match &last.block {
        crate::scrollback::block::RenderBlock::System(b) => b.text.clone(),
        other => panic!("expected System block, got {other:?}"),
    };
    assert!(
        text.contains("Session title reset to auto"),
        "committed unpin should confirm, not error; got: {text:?}"
    );
}

#[test]
fn reset_session_title_complete_pushes_system_block() {
    let mut app = test_app_with_agent();
    if let Some(a) = app.agents.get_mut(&AgentId(0)) {
        a.display_name = None;
        a.generated_session_title = None;
    }
    let scrollback_len_before = app.agents[&AgentId(0)].scrollback.len();

    let _effects = dispatch_task_result(
        TaskResult::ResetSessionTitleComplete {
            agent_id: AgentId(0),
        },
        &mut app,
    );

    assert!(app.agents[&AgentId(0)].display_name.is_none());
    let scrollback = &app.agents[&AgentId(0)].scrollback;
    assert_eq!(scrollback.len(), scrollback_len_before + 1);
    let last = scrollback.entry(scrollback.len() - 1).expect("last entry");
    let text = match &last.block {
        crate::scrollback::block::RenderBlock::System(b) => b.text.clone(),
        other => panic!("expected System block, got {other:?}"),
    };
    assert!(
        text.contains("Session title reset to auto"),
        "got: {text:?}"
    );
}

// ── GateRefreshed subscription flow ─────────────────────────────

/// Regression: when the 30s gate poll detects the subscription gate has
/// been lifted, it must emit `CheckSubscription` so the shell refreshes
/// the JWT. Without this the auth token still lacks the subscription
/// claim and all API calls return 403.
#[test]
fn gate_refreshed_emits_check_subscription_on_gate_lift() {
    let mut app = test_app();
    // User starts gated (no subscription).
    app.gate = Some(echo_agent_runtime::auth::GateInfo {
        message: "EchoAgent subscription required".into(),
        url: Some("https://cloud.echo-agent.invalid/echoagent".into()),
        label: Some("Subscribe".into()),
    });
    assert!(!app.has_access());

    // Server-side settings now show no gate (user purchased subscription).
    let settings = echo_agent_runtime::util::config::RemoteSettings::default();
    let effects = dispatch_task_result(
        TaskResult::GateRefreshed {
            settings: Some(settings),
        },
        &mut app,
    );

    // Gate must be lifted.
    assert!(app.has_access(), "gate should be lifted");
    assert!(app.welcome_prompt_focused, "prompt should be focused");

    // Must emit CheckSubscription to trigger shell-side JWT refresh.
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CheckSubscription { verify: None })),
        "must emit CheckSubscription to refresh JWT; got: {effects:?}"
    );
}

/// When the gate poll returns settings that still have a gate, no
/// effects should be emitted and the user stays blocked.
#[test]
fn gate_refreshed_no_effect_when_still_gated() {
    let mut app = test_app();
    app.gate = Some(echo_agent_runtime::auth::GateInfo {
        message: "Subscribe".into(),
        url: None,
        label: None,
    });

    let settings = echo_agent_runtime::util::config::RemoteSettings {
        gate_message: Some("Subscribe".into()),
        ..Default::default()
    };
    let effects = dispatch_task_result(
        TaskResult::GateRefreshed {
            settings: Some(settings),
        },
        &mut app,
    );

    assert!(!app.has_access(), "gate should remain");
    assert!(effects.is_empty(), "no effects when still gated");
}

/// When the user was never gated, GateRefreshed is a no-op.
#[test]
fn gate_refreshed_no_effect_when_already_unblocked() {
    let mut app = test_app();
    assert!(app.has_access()); // no gate

    let settings = echo_agent_runtime::util::config::RemoteSettings::default();
    let effects = dispatch_task_result(
        TaskResult::GateRefreshed {
            settings: Some(settings),
        },
        &mut app,
    );

    assert!(effects.is_empty(), "no effects when already unblocked");
}

/// A gate newly imposed by the 30s settings poll (possibly stale) must be
/// deferred for live verification instead of painting the paywall directly:
/// the gate is held out of `app.gate` and a `CheckSubscription` +
/// verify-timeout pair is emitted.
#[test]
fn gate_refreshed_newly_blocked_defers_gate_for_verification() {
    let mut app = test_app();
    assert!(app.has_access()); // ungated

    let settings = echo_agent_runtime::util::config::RemoteSettings {
        gate_message: Some("Subscribe".into()),
        ..Default::default()
    };
    let effects = dispatch_task_result(
        TaskResult::GateRefreshed {
            settings: Some(settings),
        },
        &mut app,
    );

    assert!(
        app.has_access(),
        "deferred gate must not show as paywall before verification"
    );
    assert!(app.pending_gate_verification.is_some());
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CheckSubscription { verify: Some(_) })),
        "must live-check before showing the paywall; got: {effects:?}"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ScheduleGateVerifyTimeout { .. })),
        "must arm the verification timeout; got: {effects:?}"
    );
}

// ── Stale-gate verification resolution ──────────────────────────

fn test_gate() -> echo_agent_runtime::auth::GateInfo {
    echo_agent_runtime::auth::GateInfo {
        message: "Subscribe".into(),
        url: None,
        label: None,
    }
}

/// The live check confirmed access (meta without a gate): the deferred
/// stale gate is dropped and the paywall never shows.
#[test]
fn verify_check_with_meta_resolves_pending_gate() {
    let mut app = test_app();
    let _effs = app.impose_gate(test_gate());
    assert!(app.has_access());

    let meta = serde_json::to_value(echo_agent_runtime::auth::AuthMeta::default()).unwrap();
    dispatch_task_result(
        TaskResult::CheckSubscriptionComplete {
            verify: Some(app.gate_verify_gen),
            meta: Some(meta),
        },
        &mut app,
    );

    assert!(app.has_access(), "live check says subscribed — no paywall");
    assert!(app.pending_gate_verification.is_none());
}

/// The live check confirmed the block (meta WITH a gate): the paywall
/// shows with the authoritative gate.
#[test]
fn verify_check_with_gated_meta_shows_gate() {
    let mut app = test_app();
    let _effs = app.impose_gate(test_gate());

    let meta = serde_json::to_value(echo_agent_runtime::auth::AuthMeta {
        gate: Some(test_gate()),
        ..Default::default()
    })
    .unwrap();
    dispatch_task_result(
        TaskResult::CheckSubscriptionComplete {
            verify: Some(app.gate_verify_gen),
            meta: Some(meta),
        },
        &mut app,
    );

    assert!(!app.has_access(), "verified gate must show");
    assert!(app.pending_gate_verification.is_none());
}

/// The verification's own check failed (meta None) while its stale gate
/// was deferred: err on blocking — the deferred gate is promoted.
#[test]
fn verify_check_failure_promotes_pending_gate() {
    let mut app = test_app();
    let _effs = app.impose_gate(test_gate());

    let effects = dispatch_task_result(
        TaskResult::CheckSubscriptionComplete {
            verify: Some(app.gate_verify_gen),
            meta: None,
        },
        &mut app,
    );

    assert!(!app.has_access(), "check failed — deferred gate must show");
    assert!(app.pending_gate_verification.is_none());
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SchedulePaywallCheck)),
        "freshly shown gate must arm the 5s auto-lift chain; got: {effects:?}"
    );
}

/// A failed GENERIC check (watch / focus / paywall chain — no generation)
/// must never promote a deferred gate: only the deferral's own
/// generation-scoped check or timeout may (a superseded or unrelated check
/// failing is not evidence about the current verification).
#[test]
fn check_subscription_complete_failure_leaves_pending_gate_untouched() {
    let mut app = test_app();
    let _effs = app.impose_gate(test_gate());

    let effects = dispatch_task_result(
        TaskResult::CheckSubscriptionComplete {
            verify: None,
            meta: None,
        },
        &mut app,
    );

    assert!(effects.is_empty());
    assert!(
        app.has_access(),
        "generic check failure must not promote the deferred gate"
    );
    assert!(
        app.pending_gate_verification.is_some(),
        "verification must stay in flight"
    );
}

/// A failed verification check from a SUPERSEDED deferral (older
/// generation) must not promote the newer pending gate.
#[test]
fn verify_check_stale_generation_failure_is_ignored() {
    let mut app = test_app();
    let _effs = app.impose_gate(test_gate());
    let stale_gen = app.gate_verify_gen;
    // Second deferral supersedes the first (its check is in flight).
    let _effs = app.impose_gate(test_gate());

    let effects = dispatch_task_result(
        TaskResult::CheckSubscriptionComplete {
            verify: Some(stale_gen),
            meta: None,
        },
        &mut app,
    );

    assert!(effects.is_empty());
    assert!(
        app.has_access(),
        "superseded verification failure must not promote the newer gate"
    );
    assert!(app.pending_gate_verification.is_some());
}

/// A check failure with no deferred gate (the plain paywall-poller path)
/// must not invent a gate.
#[test]
fn check_subscription_complete_failure_without_pending_gate_is_noop() {
    let mut app = test_app();
    dispatch_task_result(
        TaskResult::CheckSubscriptionComplete {
            verify: None,
            meta: None,
        },
        &mut app,
    );
    assert!(app.has_access());
}

/// The verification window expired before the live check resolved:
/// err on blocking — the deferred gate is promoted, and the freshly shown
/// paywall gets the 5s auto-lift chain.
#[test]
fn gate_verify_timeout_promotes_pending_gate() {
    let mut app = test_app();
    let _effs = app.impose_gate(test_gate());
    assert!(app.has_access());

    let effects = dispatch_task_result(
        TaskResult::GateVerifyTimeout {
            generation: app.gate_verify_gen,
        },
        &mut app,
    );

    assert!(!app.has_access(), "timeout — deferred gate must show");
    assert!(app.pending_gate_verification.is_none());
    assert!(
        app.paywall_check_started.is_some(),
        "promoted gate must arm the paywall auto-check chain"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SchedulePaywallCheck)),
        "promoted gate must schedule the 5s chain; got: {effects:?}"
    );
}

/// The timeout fires after the check already resolved the gate: no-op.
#[test]
fn gate_verify_timeout_noop_when_already_resolved() {
    let mut app = test_app();
    let _effs = app.impose_gate(test_gate());
    let generation = app.gate_verify_gen;
    // Live check resolved first (access confirmed).
    let meta = serde_json::to_value(echo_agent_runtime::auth::AuthMeta::default()).unwrap();
    dispatch_task_result(
        TaskResult::CheckSubscriptionComplete {
            verify: None,
            meta: Some(meta),
        },
        &mut app,
    );

    dispatch_task_result(TaskResult::GateVerifyTimeout { generation }, &mut app);
    assert!(
        app.has_access(),
        "stale timeout must not re-impose the gate"
    );
}

/// A timeout from a SUPERSEDED verification (older generation) must not
/// promote a newer deferred gate whose own live check is still in flight.
#[test]
fn gate_verify_timeout_stale_generation_is_ignored() {
    let mut app = test_app();
    // First deferral resolves (access confirmed) ...
    let _effs = app.impose_gate(test_gate());
    let stale_gen = app.gate_verify_gen;
    let meta = serde_json::to_value(echo_agent_runtime::auth::AuthMeta::default()).unwrap();
    dispatch_task_result(
        TaskResult::CheckSubscriptionComplete {
            verify: None,
            meta: Some(meta),
        },
        &mut app,
    );
    // ... then a SECOND gate is deferred (check in flight).
    let _effs = app.impose_gate(test_gate());
    assert!(app.has_access());

    // The FIRST deferral's timer fires now — it must not promote the
    // second deferral's pending gate.
    let effects = dispatch_task_result(
        TaskResult::GateVerifyTimeout {
            generation: stale_gen,
        },
        &mut app,
    );

    assert!(effects.is_empty());
    assert!(
        app.has_access(),
        "stale-generation timer must not promote the newer pending gate"
    );
    assert!(
        app.pending_gate_verification.is_some(),
        "the newer verification must stay in flight"
    );
}

/// A verified gate landing via `CheckSubscriptionComplete` (gated meta while
/// ungated) must arm the 5s paywall auto-check chain — verify-before-paywall
/// paths never went through the login-path chain start.
#[test]
fn verified_gate_via_check_complete_starts_paywall_chain() {
    let mut app = test_app();
    let _effs = app.impose_gate(test_gate());

    let meta = serde_json::to_value(echo_agent_runtime::auth::AuthMeta {
        gate: Some(test_gate()),
        ..Default::default()
    })
    .unwrap();
    let effects = dispatch_task_result(
        TaskResult::CheckSubscriptionComplete {
            verify: None,
            meta: Some(meta),
        },
        &mut app,
    );

    assert!(!app.has_access());
    assert!(
        app.paywall_check_started.is_some(),
        "verified gate must arm the paywall auto-check chain"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SchedulePaywallCheck)),
        "verified gate must schedule the 5s chain; got: {effects:?}"
    );

    // Steady-state paywall-poller responses (already gated) must NOT fan
    // out extra timers.
    let meta = serde_json::to_value(echo_agent_runtime::auth::AuthMeta {
        gate: Some(test_gate()),
        ..Default::default()
    })
    .unwrap();
    let effects = dispatch_task_result(
        TaskResult::CheckSubscriptionComplete {
            verify: None,
            meta: Some(meta),
        },
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "already-gated check responses must not schedule more timers; got: {effects:?}"
    );
}

/// `GateRefreshed` with gate-free settings while a deferred gate awaits
/// verification must drop the pending copy — the fresh settings are newer
/// than the stale snapshot that produced it — and still run the lift
/// bookkeeping (`CheckSubscription` for the JWT refresh), since the pending
/// deferral means the user was conceptually blocked.
#[test]
fn gate_refreshed_without_gate_clears_pending_verification() {
    let mut app = test_app();
    let _effs = app.impose_gate(test_gate());
    let generation = app.gate_verify_gen;

    let settings = echo_agent_runtime::util::config::RemoteSettings::default();
    let effects = dispatch_task_result(
        TaskResult::GateRefreshed {
            settings: Some(settings),
        },
        &mut app,
    );

    assert!(app.pending_gate_verification.is_none());
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CheckSubscription { verify: None })),
        "settings-confirmed lift of a pending gate must refresh the JWT; got: {effects:?}"
    );
    // The still-armed timer must find nothing to promote.
    dispatch_task_result(TaskResult::GateVerifyTimeout { generation }, &mut app);
    assert!(
        app.has_access(),
        "cleared pending gate must not resurface via the timer"
    );
}

/// Logout clears any deferred gate and the check debounce.
#[test]
fn logout_clears_pending_gate_verification() {
    let mut app = test_app();
    let _effs = app.impose_gate(test_gate());

    dispatch_task_result(TaskResult::LogoutComplete, &mut app);

    assert!(app.pending_gate_verification.is_none());
    assert!(app.last_subscription_check_at.is_none());
}

/// `apply_setting_rollback` on a known key reverts the in-memory
/// cache without emitting any new effects.
#[test]
fn rollback_known_key_reverts_cache_and_no_effect() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    // Toggle to true first.
    let _ = dispatch(Action::SetCompactMode(true), &mut app);
    assert!(app.current_ui.compact_mode);
    // Now simulate persist failure that rolls back to false.
    let effects = dispatch(
        Action::TaskComplete(TaskResult::SettingPersistFailed {
            key: "compact_mode",
            rollback_value: SettingValue::Bool(false),
            error: "test-error".into(),
        }),
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "rollback path must NOT emit any new Effects (would loop)",
    );
    // Cache is reverted.
    assert!(!app.current_ui.compact_mode);
}

/// `apply_setting_rollback` on an unknown key surfaces a tracing
/// error and a secondary toast, but does not panic.
#[test]
fn rollback_unknown_key_does_not_panic() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    let effects = dispatch(
        Action::TaskComplete(TaskResult::SettingPersistFailed {
            key: "nonexistent_setting",
            rollback_value: SettingValue::Bool(true),
            error: "test-error".into(),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    // No assertion on cache state (no arm to rollback through);
    // the toast surfaces the inconsistency to the user.
}

#[test]
fn persist_failed_toast_contains_key_and_error() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    let _ = dispatch(
        Action::TaskComplete(TaskResult::SettingPersistFailed {
            key: "compact_mode",
            rollback_value: SettingValue::Bool(false),
            error: "permission denied".into(),
        }),
        &mut app,
    );
    let toast = read_toast(&app);
    assert!(toast.contains("compact_mode"));
    assert!(toast.contains("permission denied"));
    assert!(toast.contains('\u{2717}'));
}

/// Rollback path must revert BOTH `app.current_ui` AND the
/// thread-local cache — failing only on one is the bug class
/// the inner/outer split exists to prevent.
#[test]
fn rollback_reverts_thread_local_cache_too() {
    use crate::settings::SettingValue;
    std::thread::spawn(|| {
        let mut app = test_app_with_agent();
        // Optimistic set: cache → true.
        let _ = dispatch(Action::SetCompactMode(true), &mut app);
        assert!(crate::appearance::cache::load());

        // Synthesize failure: rollback to false.
        let _ = dispatch(
            Action::TaskComplete(TaskResult::SettingPersistFailed {
                key: "compact_mode",
                rollback_value: SettingValue::Bool(false),
                error: "test-error".into(),
            }),
            &mut app,
        );
        assert!(
            !app.current_ui.compact_mode,
            "rollback must update app.current_ui"
        );
        assert!(
            !crate::appearance::cache::load(),
            "rollback must also revert the cache — otherwise the renderer \
                 still shows the optimistic value while current_ui shows the rolled-back one"
        );
    })
    .join()
    .unwrap();
}

/// `set_yolo_mode_inner` is the backstop: even a (stale) rollback
/// value of "always-approve" must not re-enable yolo under the pin.
#[test]
fn rollback_to_always_approve_blocked_by_policy_pin() {
    use crate::settings::SettingValue;
    let mut app = test_app_with_agent();
    app.yolo_policy_block = Some(POLICY_WARNING);

    let effects = dispatch(
        Action::TaskComplete(TaskResult::SettingPersistFailed {
            key: "permission_mode",
            rollback_value: SettingValue::Enum("always-approve"),
            error: "permission denied".into(),
        }),
        &mut app,
    );

    assert!(effects.is_empty(), "rollback path never re-emits effects");
    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "inner backstop must hold on the rollback path"
    );
    assert!(!app.default_yolo);
}

// -- Degraded conversations lane (SessionListLoaded.partial) ----------

/// A degraded conversations lane surfaces an actionable notice instead of
/// the misleading "No sessions found" toast.
#[test]
fn session_list_partial_no_oauth_surfaces_login_hint() {
    let mut app = test_app_with_agent();
    open_session_picker_with(&mut app, vec![]);
    let _ = dispatch(
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::Cwd,
            sessions: vec![],
            partial: Some(crate::app::effects::ConversationsPartial::NoOauth),
            seq: 0,
            query: None,
        }),
        &mut app,
    );
    assert!(
        read_toast(&app).contains("/login"),
        "no_oauth must point at /login"
    );
}

/// Notice fires once per relaxed run; survives search, re-arms on a cwd-scoped browse.
#[test]
fn session_list_relax_surfaces_notice_once() {
    let relax_response = || {
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::Repo,
            sessions: vec![make_picker_entry("local-other-cwd-1", "/elsewhere")],
            partial: None,
            seq: 0,
            query: None,
        })
    };

    let mut app = test_app_with_agent();
    open_session_picker_with(&mut app, vec![]);
    let _ = dispatch(relax_response(), &mut app);
    assert!(
        read_toast(&app).contains("this repo"),
        "the relaxed scope must be explained"
    );

    app.agents.get_mut(&AgentId(0)).unwrap().toast = None;
    let _ = dispatch(relax_response(), &mut app);
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "the relax notice must not repeat while the scope is unchanged"
    );

    let _ = dispatch(
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::Cwd,
            sessions: vec![],
            partial: None,
            seq: 0,
            query: Some("needle".into()),
        }),
        &mut app,
    );
    let _ = dispatch(relax_response(), &mut app);
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "a search response must not re-arm the relax notice"
    );

    let _ = dispatch(
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::Cwd,
            sessions: vec![make_picker_entry("local-here-1", "/here")],
            partial: None,
            seq: 0,
            query: None,
        }),
        &mut app,
    );
    let _ = dispatch(relax_response(), &mut app);
    assert!(
        read_toast(&app).contains("this repo"),
        "a scope change back to relaxed must notify again"
    );
}

/// Welcome view can't render toasts, so the one-shot notice must not latch there.
#[test]
fn session_list_relax_on_welcome_does_not_latch() {
    let mut app = test_app();
    assert!(matches!(app.active_view, ActiveView::Welcome));
    let _ = dispatch(
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::All,
            sessions: vec![make_picker_entry("local-other-cwd-1", "/elsewhere")],
            partial: None,
            seq: 0,
            query: None,
        }),
        &mut app,
    );
    assert!(
        app.session_picker_relaxed_notified_for.is_none(),
        "a notice that cannot render must not latch"
    );
}

/// The notice is keyed by browse cwd: a different directory re-notifies even
/// though the prior latch is set.
#[test]
fn session_list_relax_renotifies_when_cwd_changes() {
    let relax = || {
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::Repo,
            sessions: vec![make_picker_entry("local-other-cwd-1", "/elsewhere")],
            partial: None,
            seq: 0,
            query: None,
        })
    };

    let mut app = test_app_with_agent();
    open_session_picker_with(&mut app, vec![]);

    app.cwd = std::path::PathBuf::from("/repo/a");
    let _ = dispatch(relax(), &mut app);
    assert!(
        read_toast(&app).contains("this repo"),
        "the first cwd must notify"
    );

    app.agents.get_mut(&AgentId(0)).unwrap().toast = None;
    app.cwd = std::path::PathBuf::from("/repo/b");
    let _ = dispatch(relax(), &mut app);
    assert!(
        read_toast(&app).contains("this repo"),
        "a different cwd must re-notify even with the prior latch set"
    );
}

/// Canary: an empty list without a degraded lane keeps the generic toast.
#[test]
fn session_list_empty_without_partial_keeps_generic_toast() {
    let mut app = test_app_with_agent();
    open_session_picker_with(&mut app, vec![]);
    let _ = dispatch(
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::Cwd,
            sessions: vec![],
            partial: None,
            seq: 0,
            query: None,
        }),
        &mut app,
    );
    assert!(read_toast(&app).contains("No sessions found"));
}

/// Non-empty degraded list under chat mode (welcome-fallback branch):
/// entries land AND the retry notice surfaces; Build mode stays silent.
#[test]
fn session_list_nonempty_partial_toasts_retry_in_chat_mode_only() {
    let mut app = test_app_with_agent();
    app.chat_mode = true;
    let _ = dispatch(
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::Cwd,
            sessions: vec![make_conversation_entry("conv-part-1")],
            partial: Some(crate::app::effects::ConversationsPartial::Timeout),
            seq: 0,
            query: None,
        }),
        &mut app,
    );
    assert!(
        app.session_picker_entries.is_some(),
        "entries must still land on a degraded lane"
    );
    assert!(
        read_toast(&app).contains("retry"),
        "timeout must surface the retry notice"
    );

    // Build-mode canary: stays silent on a degraded lane.
    let mut app = test_app_with_agent();
    let _ = dispatch(
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::Cwd,
            sessions: vec![make_picker_entry("local-part-1", "/r")],
            partial: Some(crate::app::effects::ConversationsPartial::Timeout),
            seq: 0,
            query: None,
        }),
        &mut app,
    );
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "Build-mode non-empty degraded list stays silent"
    );
}

/// Modal variant of the non-empty degraded-lane notice: same chat-mode-only
/// gating as the welcome-fallback branch.
#[test]
fn session_list_nonempty_partial_modal_toasts_in_chat_mode_only() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    app.chat_mode = true;
    open_session_picker_with(&mut app, vec![]);
    let _ = dispatch(
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::Cwd,
            sessions: vec![make_conversation_entry("conv-part-m1")],
            partial: Some(crate::app::effects::ConversationsPartial::Timeout),
            seq: 0,
            query: None,
        }),
        &mut app,
    );
    let agent = get_active_agent(&app).expect("active agent");
    assert!(
        matches!(
            agent.active_modal.as_ref(),
            Some(ActiveModal::SessionPicker {
                entries: Some(list),
                ..
            }) if list.len() == 1
        ),
        "entries must land in the open modal on a degraded lane"
    );
    assert!(
        read_toast(&app).contains("retry"),
        "chat-mode modal must surface the retry notice"
    );

    // Build-mode canary: the open modal stays silent.
    let mut app = test_app_with_agent();
    open_session_picker_with(&mut app, vec![]);
    let _ = dispatch(
        Action::TaskComplete(TaskResult::SessionListLoaded {
            scope: ListScope::Cwd,
            sessions: vec![make_picker_entry("local-part-m1", "/r")],
            partial: Some(crate::app::effects::ConversationsPartial::Timeout),
            seq: 0,
            query: None,
        }),
        &mut app,
    );
    assert!(
        app.agents[&AgentId(0)].toast.is_none(),
        "Build-mode modal non-empty degraded list stays silent"
    );
}
