//! Tests for the action router, model switching, slash commands, and other cross-cutting dispatch behavior.
use super::*;
#[test]
fn auth_copy_dispatch_preserves_all_delivery_states() {
    for delivery in [
        crate::clipboard::ClipboardDelivery::Confirmed,
        crate::clipboard::ClipboardDelivery::Unverified,
        crate::clipboard::ClipboardDelivery::Failed,
    ] {
        let mut app = test_app();
        app.auth_state = AuthState::Authenticating {
            request_seq: 1,
            handle: None,
            auth_url: Some("https://cloud.echo-agent.invalid/auth".to_owned()),
            mode: AuthMode::Command,
        };
        let effects = crate::app::dispatch::router::dispatch_copy_auth_url(&mut app, |url| {
            assert_eq!(url, "https://cloud.echo-agent.invalid/auth");
            delivery
        });
        assert_eq!(app.auth_clipboard_delivery, Some(delivery));
        assert_eq!(app.auth_clipboard_feedback_generation, 1);
        assert!(matches!(
            effects.as_slice(),
            [Effect::ScheduleClearAuthCopyFeedback { generation: 1 }]
        ));
    }
}
#[test]
fn external_prompt_editor_arms_typed_request_and_preserves_composer_modes() {
    use crate::app::agent_view::PromptInputMode;
    for mode in [
        PromptInputMode::Normal,
        PromptInputMode::Bash,
        PromptInputMode::Remember,
    ] {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        app.screen_mode = crate::app::ScreenMode::Minimal;
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .prompt
            .set_screen_mode(crate::app::ScreenMode::Minimal);
        agent.prompt_input_mode = mode;
        agent.prompt.set_text("draft with\nnewlines");
        let effects = dispatch(Action::EditPromptExternal, &mut app);
        assert!(effects.is_empty());
        let request = app.pending_editor.take().expect("editor request");
        match request {
            crate::app::external_editor::PendingEditorRequest::PromptDraft {
                agent_id,
                original_text,
            } => {
                assert_eq!(agent_id, id);
                assert_eq!(original_text, "draft with\nnewlines");
            }
            other => panic!("expected prompt draft request, got {other:?}"),
        }
        assert_eq!(app.agents[&id].prompt_input_mode, mode);
        assert_eq!(app.agents[&id].prompt.text(), "draft with\nnewlines");
    }
}
#[test]
fn external_prompt_editor_arms_in_fullscreen_and_refuses_owned_input() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().prompt.set_text("draft");
    let _ = dispatch(Action::EditPromptExternal, &mut app);
    assert!(
        matches!(
            app.pending_editor.take(),
            Some(crate::app::external_editor::PendingEditorRequest::PromptDraft { .. })
        ),
        "full TUI arms the request without requiring prompt-pane focus"
    );
    app.screen_mode = crate::app::ScreenMode::Minimal;
    app.agents.get_mut(&id).unwrap().active_pane = ActivePane::Scrollback;
    let _ = dispatch(Action::EditPromptExternal, &mut app);
    assert!(
        matches!(
            app.pending_editor,
            Some(crate::app::external_editor::PendingEditorRequest::PromptDraft { .. })
        ),
        "the composer stays the editing surface with scrollback focused"
    );
    app.pending_editor = None;
    app.agents.get_mut(&id).unwrap().cancel_turn_view =
        Some(crate::views::modal::CancelTurnViewState {
            active_idx: 0,
            running_count: 1,
        });
    let _ = dispatch(Action::EditPromptExternal, &mut app);
    assert!(app.pending_editor.is_none(), "modal owner must refuse");
    assert_eq!(app.agents[&id].prompt.text(), "draft");
    app.agents.get_mut(&id).unwrap().cancel_turn_view = None;
    app.agents.get_mut(&id).unwrap().prompt_mode = PromptMode::EditingQueued {
        id: 1,
        original: "queued".to_owned(),
        server_id: None,
        kind: crate::app::agent::QueueEntryKind::Prompt,
    };
    let _ = dispatch(Action::EditPromptExternal, &mut app);
    assert!(app.pending_editor.is_none(), "queue edit must refuse");
    assert_eq!(app.agents[&id].prompt.text(), "draft");
    app.agents.get_mut(&id).unwrap().prompt_mode = PromptMode::Normal;
    app.agents.get_mut(&id).unwrap().prompt.set_text("/");
    let models = app.agents[&id].session.models.clone();
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .refresh_slash(&models);
    assert!(app.agents[&id].prompt.any_dropdown_open());
    let _ = dispatch(Action::EditPromptExternal, &mut app);
    assert!(app.pending_editor.is_none(), "dropdown owner must refuse");
}
#[test]
fn external_prompt_editor_refuses_elements_with_visible_message() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.screen_mode = crate::app::ScreenMode::Minimal;
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .set_screen_mode(crate::app::ScreenMode::Minimal);
    let agent = app.agents.get_mut(&id).unwrap();
    let pasted = "one\ntwo\nthree\nfour";
    let _ = agent.prompt.handle_paste(pasted);
    assert!(!agent.prompt.textarea.elements().is_empty());
    let _ = dispatch(Action::EditPromptExternal, &mut app);
    assert!(app.pending_editor.is_none());
    assert_eq!(app.agents[&id].prompt.text(), pasted);
    assert!(!app.agents[&id].prompt.textarea.elements().is_empty());
    assert!(
        app.agents[&id]
            .scrollback
            .iter_entries()
            .any(|(_, entry)| entry.block.searchable_text().as_deref()
                == Some(crate::app::external_editor::ATTACHMENT_MESSAGE))
    );
    let agent = app.agents.get_mut(&id).unwrap();
    agent.prompt.set_text("");
    agent.prompt.textarea.insert_element(
        "@src/main.rs",
        crate::views::prompt_widget::KIND_FILE_REF,
        None,
    );
    let file_ref_text = agent.prompt.text().to_owned();
    let _ = dispatch(Action::EditPromptExternal, &mut app);
    assert!(app.pending_editor.is_none());
    assert_eq!(app.agents[&id].prompt.text(), file_ref_text);
    assert!(!app.agents[&id].prompt.textarea.elements().is_empty());
    let agent = app.agents.get_mut(&id).unwrap();
    agent.prompt.set_text("");
    let image = crate::prompt_images::PastedImage {
        element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
        display_number: 0,
        mime_type: "image/png".to_owned(),
        dimensions: Some((8, 8)),
        byte_len: 1,
        encoded_bytes: Some(vec![0].into()),
        source_path: None,
        staged_temp_path: None,
        session_image_path: None,
        preview: crate::prompt_images::PromptImagePreview::default(),
    };
    agent.prompt.insert_image(image).unwrap();
    let image_text = agent.prompt.text().to_owned();
    let _ = dispatch(Action::EditPromptExternal, &mut app);
    assert!(app.pending_editor.is_none());
    assert_eq!(app.agents[&id].prompt.text(), image_text);
    assert_eq!(app.agents[&id].prompt.images.len(), 1);
}
#[test]
fn external_prompt_editor_refuses_voice_and_pending_paste_with_visible_messages() {
    use crate::app::agent_view::AgentDeferredSend;
    use crate::app::app_view::{VoiceState, VoiceTarget};
    for voice_state in [
        VoiceState::ColdStart {
            hold: false,
            target: VoiceTarget::Agent(AgentId(0)),
        },
        VoiceState::Recording {
            hold: false,
            target: VoiceTarget::Agent(AgentId(0)),
            interim: Some("partial".to_owned()),
        },
        VoiceState::Stopping {
            target: VoiceTarget::Agent(AgentId(0)),
            interim: Some("partial".to_owned()),
        },
    ] {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        app.screen_mode = crate::app::ScreenMode::Minimal;
        app.voice_state = voice_state;
        app.agents.get_mut(&id).unwrap().prompt.set_text("draft");
        let _ = dispatch(Action::EditPromptExternal, &mut app);
        assert!(app.pending_editor.is_none());
        assert_eq!(app.agents[&id].prompt.text(), "draft");
        assert!(
            app.agents[&id]
                .scrollback
                .iter_entries()
                .any(|(_, entry)| entry.block.searchable_text().as_deref()
                    == Some(crate::app::external_editor::VOICE_MESSAGE))
        );
    }
    for (probes, deferred_send) in [
        (1, None),
        (1, Some(AgentDeferredSend::SendPrompt)),
        (0, Some(AgentDeferredSend::SendPrompt)),
    ] {
        let mut app = test_app_with_agent();
        let id = AgentId(0);
        app.screen_mode = crate::app::ScreenMode::Minimal;
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.set_text("draft");
        agent.paste_probe_in_flight = probes;
        agent.deferred_send = deferred_send;
        let _ = dispatch(Action::EditPromptExternal, &mut app);
        assert!(app.pending_editor.is_none());
        assert_eq!(app.agents[&id].prompt.text(), "draft");
        assert_eq!(app.agents[&id].paste_probe_in_flight, probes);
        assert_eq!(app.agents[&id].deferred_send, deferred_send);
        assert!(
            app.agents[&id]
                .scrollback
                .iter_entries()
                .any(|(_, entry)| entry.block.searchable_text().as_deref()
                    == Some(crate::app::external_editor::PASTE_MESSAGE))
        );
    }
}
#[test]
fn deferred_paste_completion_after_refused_editor_does_not_implicitly_send_without_stash() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.screen_mode = crate::app::ScreenMode::Minimal;
    let agent = app.agents.get_mut(&id).unwrap();
    agent.prompt.set_text("draft");
    let draft_len = agent.prompt.text().len();
    agent.prompt.set_cursor(draft_len);
    agent.paste_probe_in_flight = 1;
    let _ = dispatch(Action::EditPromptExternal, &mut app);
    assert!(app.pending_editor.is_none());
    let effects = dispatch(
        Action::TaskComplete(TaskResult::ClipboardAttachmentProbed {
            ctx: crate::app::actions::ClipboardPasteContext {
                target: crate::app::actions::ClipboardPasteTarget::AgentPrompt {
                    agent_id: id,
                    images_dir: None,
                    from_feedback_pane: false,
                },
                source: crate::app::actions::ClipboardPasteSource::ClipboardKey {
                    text: crate::app::actions::ClipboardTextRead::Success(Some(
                        "pasted".to_owned(),
                    )),
                    tip_showing: false,
                },
            },
            image: crate::app::actions::ProbedAttachment::NoRaster,
            file_urls: None,
        }),
        &mut app,
    );
    assert!(effects.is_empty(), "no deferred submit was armed");
    assert_eq!(app.agents[&id].prompt.text(), "draftpasted");
    assert!(app.agents[&id].session.pending_prompts.is_empty());
}
#[test]
fn external_prompt_editor_result_replaces_or_clears_without_sending() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    app.agents.get_mut(&id).unwrap().prompt.set_text("original");
    crate::app::external_editor::apply_prompt_text(&mut app, id, "edited\n".to_owned());
    assert_eq!(app.agents[&id].prompt.text(), "edited\n");
    assert!(app.agents[&id].session.state.is_turn_running());
    assert!(app.agents[&id].session.pending_prompts.is_empty());
    crate::app::external_editor::apply_prompt_text(&mut app, id, String::new());
    assert_eq!(app.agents[&id].prompt.text(), "");
    assert!(app.agents[&id].session.state.is_turn_running());
}
#[test]
fn editor_failure_targets_original_agent_and_vanished_agent_is_safe() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().prompt.set_text("original");
    crate::app::external_editor::report_prompt_failure(&mut app, id, "editor failed");
    assert_eq!(app.agents[&id].prompt.text(), "original");
    assert!(
        app.agents[&id]
            .scrollback
            .iter_entries()
            .any(|(_, entry)| entry.block.searchable_text().as_deref() == Some("editor failed"))
    );
    app.agents.shift_remove(&id);
    crate::app::external_editor::apply_prompt_text(&mut app, id, "ignored".to_owned());
    crate::app::external_editor::report_prompt_failure(&mut app, id, "ignored");
    assert!(app.agents.is_empty());
}
#[test]
fn config_editor_action_still_uses_typed_request() {
    let mut app = test_app_with_agent();
    let path = std::path::PathBuf::from("/tmp/agent-config.md");
    let _ = dispatch(
        Action::SuspendForEditor {
            path: path.clone(),
            refresh_agents_modal: Some(crate::views::agents_modal::AgentsTab::Agents),
        },
        &mut app,
    );
    assert!(matches!(
        app.pending_editor,
        Some(crate::app::external_editor::PendingEditorRequest::ConfigFile {
            path: ref queued,
            refresh_agents_modal: Some(crate::views::agents_modal::AgentsTab::Agents),
        }) if queued == &path
    ));
}
fn seed_foreign_resume_hint(
    app: &mut AppView,
    tool: echo_agent_foreign_sessions::ForeignSessionTool,
) {
    app.foreign_session_compat = echo_agent_foreign_sessions::EnabledForeignSessionSources {
        claude: true,
        codex: true,
        cursor: true,
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
    app.apply_foreign_resume_detection(
        launch_token,
        &canonical_cwd,
        Some(echo_agent_foreign_sessions::RecentForeignSession {
            tool,
            native_id: "native-id".into(),
            age: std::time::Duration::from_secs(60),
        }),
    );
}
/// Sending feedback is a submit: it retires the active ephemeral tip.
#[test]
fn send_feedback_clears_active_ephemeral_tip() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    let _ = agent.ephemeral_tip.show(
        crate::tips::EphemeralTip::new("t", ratatui::text::Line::from("hint")),
        &mut std::collections::HashMap::new(),
    );
    assert!(agent.ephemeral_tip.is_active());
    let _ = dispatch(
        Action::SendFeedback {
            text: "it broke".into(),
            images: Default::default(),
            trace: Some(crate::app::actions::FeedbackTraceChoice::NoUpload),
        },
        &mut app,
    );
    assert!(
        !app.agents.get(&id).unwrap().ephemeral_tip.is_active(),
        "feedback submit must clear the tip"
    );
}
/// Sending a remember note is a submit: it retires the active ephemeral tip.
#[test]
fn send_remember_note_clears_active_ephemeral_tip() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    let _ = agent.ephemeral_tip.show(
        crate::tips::EphemeralTip::new("t", ratatui::text::Line::from("hint")),
        &mut std::collections::HashMap::new(),
    );
    assert!(agent.ephemeral_tip.is_active());
    let _ = dispatch(Action::SendRememberNote("remember this".into()), &mut app);
    assert!(
        !app.agents.get(&id).unwrap().ephemeral_tip.is_active(),
        "remember-note submit must clear the tip"
    );
}
#[test]
fn quit_returns_quit_effect() {
    let mut app = test_app();
    let effects = dispatch(Action::Quit, &mut app);
    assert!(matches!(effects.as_slice(), [Effect::Quit]));
}
#[test]
fn resume_foreign_session_consumes_hint_and_uses_each_tools_prompt() {
    use echo_agent_foreign_sessions::ForeignSessionTool;
    for (tool, prompt) in [
        (ForeignSessionTool::Claude, "/resume-claude native-id"),
        (ForeignSessionTool::Codex, "/resume-codex native-id"),
        (ForeignSessionTool::Cursor, "/resume-cursor native-id"),
    ] {
        let mut app = test_app();
        seed_foreign_resume_hint(&mut app, tool);
        let effects = dispatch(Action::ResumeForeignSession, &mut app);
        assert!(app.foreign_resume_hint().is_none());
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::CreateSession { .. }))
        );
        assert_eq!(
            app.agents[&AgentId(0)]
                .session
                .pending_prompts
                .front()
                .map(|pending| pending.text.as_str()),
            Some(prompt)
        );
    }
}
#[test]
fn resume_foreign_session_without_hint_is_noop() {
    let mut app = test_app();
    assert!(app.foreign_resume_hint().is_none());
    let effects = dispatch(Action::ResumeForeignSession, &mut app);
    assert!(effects.is_empty(), "no hint → no effects");
    assert!(app.foreign_resume_hint().is_none());
}
#[test]
fn resume_foreign_session_stashes_prompt_behind_trust_and_auth() {
    use echo_agent_foreign_sessions::ForeignSessionTool;
    for (tool, prompt, auth_pending) in [
        (ForeignSessionTool::Codex, "/resume-codex native-id", false),
        (ForeignSessionTool::Cursor, "/resume-cursor native-id", true),
    ] {
        let mut app = test_app();
        if auth_pending {
            app.auth_state = AuthState::Pending { error: None };
        } else {
            app.trust_state = TrustState::Pending {
                workspace: std::path::PathBuf::from("/work/proj"),
            };
        }
        seed_foreign_resume_hint(&mut app, tool);
        app.deferred_startup.session =
            Some(crate::app::session_startup::DeferredSessionStartup::Load {
                session_id: "must-not-load".into(),
                session_cwd: Some(std::path::PathBuf::from("/other")),
                chat_kind: true,
            });
        app.deferred_startup.worktree = true;
        app.deferred_startup.worktree_label = Some("stale".into());
        app.deferred_startup.worktree_ref = Some("stale-ref".into());
        app.deferred_startup.preferred_session_id = Some("stale-id".into());
        app.deferred_startup.new_session = true;
        app.deferred_startup.prompt = Some("stale prompt".into());
        app.deferred_startup.open_dashboard = true;
        app.deferred_startup.pending_chat = true;
        assert!(
            app.foreign_resume_hint().is_some(),
            "the explicit nudge remains available to supersede deferred intents"
        );
        let effects = dispatch(Action::ResumeForeignSession, &mut app);
        assert!(effects.is_empty());
        assert!(app.foreign_resume_hint().is_none());
        assert_eq!(app.deferred_startup.prompt.as_deref(), Some(prompt));
        assert!(app.deferred_startup.session.is_none());
        assert!(!app.deferred_startup.worktree);
        assert!(app.deferred_startup.worktree_label.is_none());
        assert!(app.deferred_startup.worktree_ref.is_none());
        assert!(app.deferred_startup.preferred_session_id.is_none());
        assert!(!app.deferred_startup.new_session);
        assert!(!app.deferred_startup.open_dashboard);
        assert!(!app.deferred_startup.pending_chat);
    }
}
#[test]
fn follow_up_chip_does_not_execute_slash_command() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    assert!(!app.agents[&id].session.is_yolo());
    let effects = dispatch(Action::SubmitFollowUp("/always-approve".into()), &mut app);
    assert!(
        !app.agents[&id].session.is_yolo(),
        "a /always-approve chip must NOT flip YOLO mode"
    );
    assert!(
        matches!(&effects[..], [Effect::SendPrompt { text, .. }] if text == "/always-approve"),
        "chip text must be submitted literally, got {effects:?}"
    );
}
#[test]
fn follow_up_chip_does_not_execute_exit_alias() {
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SubmitFollowUp("quit".into()), &mut app);
    assert!(
        matches!(&effects[..], [Effect::SendPrompt { text, .. }] if text == "quit"),
        "bare 'quit' chip must be a literal prompt, got {effects:?}"
    );
}
#[test]
fn chip_submit_while_running_clears_follow_up_chips() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.apply_follow_ups("resp-1".into(), vec!["Summarize".into()]);
        assert!(agent.follow_ups.is_some(), "precondition: chips shown");
        agent.session.state = AgentState::TurnRunning;
    }
    let effects = dispatch(Action::SubmitFollowUp("Summarize".into()), &mut app);
    assert!(
        matches!(&effects[..], [Effect::SendPrompt { text, .. }] if text == "Summarize"),
        "chip must immediate-send while running, got {effects:?}"
    );
    assert_eq!(app.agents[&id].session.queue_len(), 0);
    assert!(
        app.agents[&id].follow_ups.is_none(),
        "immediate-send chip path must clear chips"
    );
}
#[test]
fn chip_submit_while_reconnect_pending_keeps_chips_and_does_not_send() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents
        .get_mut(&id)
        .unwrap()
        .apply_follow_ups("resp-1".into(), vec!["Summarize".into()]);
    app.reconnect_pending = true;
    let effects = dispatch(Action::SubmitFollowUp("Summarize".into()), &mut app);
    assert!(
        effects.is_empty(),
        "a reconnect-pending submit must emit no effect, got {effects:?}"
    );
    assert_eq!(app.agents[&id].session.queue_len(), 0);
    assert!(
        app.agents[&id].follow_ups.is_some(),
        "reconnect-pending submit must NOT clear the chips"
    );
    app.reconnect_pending = false;
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let effects2 = dispatch(Action::SubmitFollowUp("Summarize".into()), &mut app);
    assert!(
        matches!(&effects2[..], [Effect::SendPrompt { text, .. }] if text == "Summarize"),
        "after reconnect clears, the chip must submit, got {effects2:?}"
    );
    assert!(
        app.agents[&id].follow_ups.is_none(),
        "a proceeding submit must clear the chips"
    );
}
#[test]
fn mark_turn_finished_clears_start_and_stamps_active() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent.turn_started_at = Some(std::time::Instant::now());
    agent.last_active_at = None;
    agent.mark_turn_finished(crate::app::cancel_latency::TurnEnd::Completed);
    assert!(
        agent.turn_started_at.is_none(),
        "turn_started_at must be cleared"
    );
    assert!(
        agent.last_active_at.is_some(),
        "last_active_at must be stamped"
    );
}
fn critical_announcement(id: &str) -> echo_agent_announcements::RemoteAnnouncement {
    echo_agent_announcements::RemoteAnnouncement {
        id: Some(id.into()),
        title: Some(format!("{id} title")),
        message: Some(format!("{id} message")),
        severity: Some("critical".into()),
        ..Default::default()
    }
}
fn promo_announcement(id: &str) -> echo_agent_announcements::RemoteAnnouncement {
    echo_agent_announcements::RemoteAnnouncement {
        id: Some(id.into()),
        message: Some(format!("{id} message")),
        severity: Some("promo".into()),
        cta: Some(echo_agent_announcements::AnnouncementCta {
            label: Some("Go".into()),
            url: Some(format!("https://echo-agent.invalid/{id}")),
            caption: None,
        }),
        ..Default::default()
    }
}
/// Id of the item the banner slot currently selects (None = banner closed).
fn shown_banner_id(app: &AppView) -> Option<String> {
    crate::views::announcements::first_session_announcement(
        &app.active_announcements,
        &app.hidden_announcement_ids,
    )
    .and_then(|a| a.id.clone())
}
/// `AnnouncementsOpenCta(surface)` re-resolves through the slot gate and opens
/// the promo url (observed via the `ECHO_AGENT_TEST_OPEN_URL_FILE` seam, from every
/// surface); a critical owning the slot — or no usable cta — makes it a silent
/// no-op (no open, so a stale prior-frame click can't leak the promo url).
#[serial_test::serial(ECHO_AGENT_TEST_OPEN_URL_FILE)]
#[test]
fn announcements_open_cta_opens_promo_and_noops_under_critical() {
    use echo_agent_telemetry::events::AnnouncementCtaSurface;
    let url_file =
        std::env::temp_dir().join(format!("echo-agent-cta-open-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&url_file);
    unsafe { std::env::set_var("ECHO_AGENT_TEST_OPEN_URL_FILE", &url_file) };
    let opened = || std::fs::read_to_string(&url_file).unwrap_or_default();
    let mut app = test_app_with_agent();
    app.active_announcements = vec![promo_announcement("promo-open")];
    for surface in [
        AnnouncementCtaSurface::Banner,
        AnnouncementCtaSurface::Welcome,
        AnnouncementCtaSurface::Header,
        AnnouncementCtaSurface::Dashboard,
        AnnouncementCtaSurface::Keyboard,
    ] {
        let _ = std::fs::write(&url_file, "");
        let effects = dispatch(Action::AnnouncementsOpenCta(surface), &mut app);
        assert!(effects.is_empty(), "open is a side effect, not an Effect");
        assert!(
            opened()
                .lines()
                .any(|l| l == "https://echo-agent.invalid/promo-open"),
            "surface {surface:?} must open the promo url; got {:?}",
            opened()
        );
    }
    let _ = std::fs::write(&url_file, "");
    app.active_announcements = vec![
        critical_announcement("crit-a"),
        promo_announcement("promo-open"),
    ];
    let _ = dispatch(
        Action::AnnouncementsOpenCta(AnnouncementCtaSurface::Keyboard),
        &mut app,
    );
    assert!(
        opened().trim().is_empty(),
        "a critical slot owner must make the open a no-op; got {:?}",
        opened()
    );
    let _ = std::fs::write(&url_file, "");
    app.active_announcements = vec![];
    let _ = dispatch(
        Action::AnnouncementsOpenCta(AnnouncementCtaSurface::Banner),
        &mut app,
    );
    assert!(opened().trim().is_empty(), "no cta → no open");
    unsafe { std::env::remove_var("ECHO_AGENT_TEST_OPEN_URL_FILE") };
    let _ = std::fs::remove_file(&url_file);
}
/// `AnnouncementCtaShown` latches once per (announcement, surface): first
/// frame with an armed CTA rect emits, later frames don't, and a NEW
/// announcement id re-emits on the same surfaces.
#[test]
fn cta_impressions_latch_once_per_surface_and_reemit_for_new_id() {
    use crate::app::app_view::ActiveView;
    use echo_agent_telemetry::events::AnnouncementCtaSurface;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.active_announcements = vec![promo_announcement("promo-a")];
    let rect = Some(ratatui::layout::Rect::new(0, 0, 4, 1));
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.hit_announcement_cta.set(rect);
        agent.hit_upgrade_cta.set(rect);
    }
    app.log_announcement_cta_impressions();
    let logged = &app.announcement_cta_impressions_logged;
    assert_eq!(logged.len(), 2);
    assert!(logged.contains(&("promo-a".to_string(), AnnouncementCtaSurface::Banner)));
    assert!(logged.contains(&("promo-a".to_string(), AnnouncementCtaSurface::Header)));
    app.log_announcement_cta_impressions();
    assert_eq!(app.announcement_cta_impressions_logged.len(), 2);
    app.active_announcements = vec![promo_announcement("promo-b")];
    app.log_announcement_cta_impressions();
    let logged = &app.announcement_cta_impressions_logged;
    assert_eq!(logged.len(), 4);
    assert!(logged.contains(&("promo-b".to_string(), AnnouncementCtaSurface::Banner)));
    assert!(logged.contains(&("promo-b".to_string(), AnnouncementCtaSurface::Header)));
}
/// No impression without a painted button under a promo slot owner: a
/// critical preempting the slot, a hidden promo, and cleared (unpainted)
/// rects all emit nothing — the same gate the click dispatch resolves.
#[test]
fn cta_impressions_respect_slot_gate_and_paint() {
    use crate::app::app_view::ActiveView;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.active_view = ActiveView::Agent(id);
    let rect = Some(ratatui::layout::Rect::new(0, 0, 4, 1));
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.hit_announcement_cta.set(rect);
        agent.hit_upgrade_cta.set(rect);
    }
    app.active_announcements = vec![critical_announcement("crit"), promo_announcement("p")];
    app.log_announcement_cta_impressions();
    assert!(app.announcement_cta_impressions_logged.is_empty());
    app.active_announcements = vec![promo_announcement("p")];
    app.hidden_announcement_ids = ["p".to_string()].into_iter().collect();
    app.log_announcement_cta_impressions();
    assert!(app.announcement_cta_impressions_logged.is_empty());
    app.hidden_announcement_ids.clear();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.hit_announcement_cta.clear();
        agent.hit_upgrade_cta.clear();
    }
    app.log_announcement_cta_impressions();
    assert!(app.announcement_cta_impressions_logged.is_empty());
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_subagent = Some("child-sid".into());
        agent.hit_announcement_cta.clear();
        agent.hit_upgrade_cta.clear();
    }
    app.active_announcements = vec![promo_announcement("p2")];
    app.log_announcement_cta_impressions();
    assert!(app.announcement_cta_impressions_logged.is_empty());
}
/// Frame occluders (the goal-detail class) leave rects armed and block clicks
/// at dispatch time — impressions mirror the OSC 8 drop-whole rule: an
/// occluded CTA is not counted until an overlay-free frame shows it clean.
#[test]
fn cta_impressions_suppressed_while_rect_occluded() {
    use crate::app::app_view::ActiveView;
    use echo_agent_telemetry::events::AnnouncementCtaSurface;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.active_view = ActiveView::Agent(id);
    app.active_announcements = vec![promo_announcement("p")];
    let rect = ratatui::layout::Rect::new(0, 0, 4, 1);
    let overlay = ratatui::layout::Rect::new(0, 0, 80, 1);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.hit_announcement_cta.set(Some(rect));
        agent.hit_upgrade_cta.set(Some(rect));
        agent.frame_occluder_rects.push(overlay);
    }
    app.log_announcement_cta_impressions();
    assert!(app.announcement_cta_impressions_logged.is_empty());
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.frame_occluder_rects.clear();
    }
    app.log_announcement_cta_impressions();
    let logged = &app.announcement_cta_impressions_logged;
    assert_eq!(logged.len(), 2);
    assert!(logged.contains(&("p".to_string(), AnnouncementCtaSurface::Banner)));
    assert!(logged.contains(&("p".to_string(), AnnouncementCtaSurface::Header)));
    app.log_announcement_cta_impressions();
    assert_eq!(app.announcement_cta_impressions_logged.len(), 2);
}
/// The welcome hero and dashboard surfaces latch from their own armed rects
/// (only the active view's rects are consulted).
#[test]
fn cta_impressions_cover_welcome_and_dashboard_surfaces() {
    use crate::app::app_view::ActiveView;
    use crate::views::dashboard::state::DashboardState;
    use echo_agent_telemetry::events::AnnouncementCtaSurface;
    let mut app = test_app();
    app.active_announcements = vec![promo_announcement("p")];
    let rect = Some(ratatui::layout::Rect::new(0, 0, 4, 1));
    app.active_view = ActiveView::Welcome;
    app.welcome_upgrade_cta_rect = rect;
    app.log_announcement_cta_impressions();
    let logged = &app.announcement_cta_impressions_logged;
    assert!(logged.contains(&("p".to_string(), AnnouncementCtaSurface::Welcome)));
    app.active_view = ActiveView::AgentDashboard;
    let mut dash = DashboardState::new();
    dash.upgrade_cta_hit.set(rect);
    app.dashboard = Some(dash);
    app.log_announcement_cta_impressions();
    let logged = &app.announcement_cta_impressions_logged;
    assert!(logged.contains(&("p".to_string(), AnnouncementCtaSurface::Dashboard)));
    app.active_announcements = vec![promo_announcement("q")];
    app.log_announcement_cta_impressions();
    let logged = &app.announcement_cta_impressions_logged;
    assert!(!logged.contains(&("q".to_string(), AnnouncementCtaSurface::Welcome)));
    assert_eq!(logged.len(), 3);
}
#[test]
fn dispatch_send_prompt_announcements_via_registry() {
    let mut app = test_app_with_agent();
    let agent_id = AgentId(0);
    switch_to_agent(&mut app, agent_id, SwitchCause::New);
    app.active_announcements = vec![critical_announcement("crit-a")];
    let effects = dispatch(Action::SendPrompt("/announcements hide".into()), &mut app);
    assert!(
            effects.iter().any(
                |e| matches!(e, Effect::PersistAnnouncementsHidden { hidden_ids } if hidden_ids.contains("crit-a"))
            ),
            "expected persist effect carrying the hidden id, got {effects:?}"
        );
    assert!(app.hidden_announcement_ids.contains("crit-a"));
    assert_eq!(shown_banner_id(&app), None, "hidden critical closes banner");
    assert!(app.agents[&agent_id].prompt.text().is_empty());
    let initial_scrollback_len = app.agents[&agent_id].scrollback.len();
    let initial_queue_len = app.agents[&agent_id].session.queue_len();
    let effects = dispatch(Action::SendPrompt("/announcements foo".into()), &mut app);
    assert!(effects.is_empty(), "expected no effects, got {effects:?}");
    assert_eq!(app.agents[&agent_id].session.queue_len(), initial_queue_len);
    assert_eq!(
        app.agents[&agent_id].scrollback.len(),
        initial_scrollback_len + 1,
        "expected usage message in scrollback"
    );
}
/// Hide records only the currently-SHOWN critical's id: with `[A, B]`,
/// hiding A reveals B, hiding B closes the banner, and a later push with a
/// new id (C) re-arms it without any user action.
#[test]
fn announcements_hide_is_per_id_so_new_critical_reappears() {
    let mut app = test_app();
    app.active_announcements = vec![
        critical_announcement("outage-a"),
        critical_announcement("outage-b"),
    ];
    assert_eq!(shown_banner_id(&app).as_deref(), Some("outage-a"));
    let effects = dispatch(Action::AnnouncementsHide, &mut app);
    assert_eq!(effects.len(), 1);
    assert!(app.hidden_announcement_ids.contains("outage-a"));
    assert_eq!(
        shown_banner_id(&app).as_deref(),
        Some("outage-b"),
        "hiding the shown critical must reveal the next one"
    );
    let effects = dispatch(Action::AnnouncementsHide, &mut app);
    assert_eq!(effects.len(), 1);
    assert_eq!(shown_banner_id(&app), None, "all hidden closes the banner");
    let effects = dispatch(Action::AnnouncementsHide, &mut app);
    assert!(effects.is_empty(), "expected no effects, got {effects:?}");
    app.active_announcements
        .push(critical_announcement("outage-c"));
    assert_eq!(
        shown_banner_id(&app).as_deref(),
        Some("outage-c"),
        "new critical id must re-arm the banner"
    );
}
/// Show clears the hide keys of every live critical (stacked hides
/// un-hide in one step) and leaves unrelated ids alone.
#[test]
fn announcements_show_clears_visible_critical_ids_only() {
    let mut app = test_app();
    app.hidden_announcement_ids = ["outage-a".to_string(), "unrelated".to_string()]
        .into_iter()
        .collect();
    app.active_announcements = vec![critical_announcement("outage-a")];
    assert_eq!(shown_banner_id(&app), None);
    let effects = dispatch(Action::AnnouncementsShow, &mut app);
    assert!(
            effects.iter().any(
                |e| matches!(e, Effect::PersistAnnouncementsHidden { hidden_ids } if !hidden_ids.contains("outage-a"))
            ),
            "expected persist effect without the un-hidden id, got {effects:?}"
        );
    assert_eq!(shown_banner_id(&app).as_deref(), Some("outage-a"));
    assert!(
        app.hidden_announcement_ids.contains("unrelated"),
        "ids not currently visible must survive show (prune owns cleanup)"
    );
    let effects = dispatch(Action::AnnouncementsShow, &mut app);
    assert!(effects.is_empty(), "expected no effects, got {effects:?}");
    app.active_announcements.clear();
    let effects = dispatch(Action::AnnouncementsHide, &mut app);
    assert!(effects.is_empty(), "expected no effects, got {effects:?}");
}
/// Hide targets the banner-slot item: the critical while one owns the slot,
/// then the promo the slot reveals — each per-ID with a persist effect.
#[test]
fn announcements_hide_targets_slot_owner_critical_then_promo() {
    let mut app = test_app();
    app.active_announcements = vec![
        promo_announcement("promo-a"),
        critical_announcement("outage-a"),
    ];
    assert_eq!(
        shown_banner_id(&app).as_deref(),
        Some("outage-a"),
        "critical wins the slot regardless of list order"
    );
    let effects = dispatch(Action::AnnouncementsHide, &mut app);
    assert_eq!(effects.len(), 1);
    assert!(app.hidden_announcement_ids.contains("outage-a"));
    assert!(
        !app.hidden_announcement_ids.contains("promo-a"),
        "hide must only record the shown item"
    );
    assert_eq!(
        shown_banner_id(&app).as_deref(),
        Some("promo-a"),
        "hiding the critical hands the slot to the promo"
    );
    let effects = dispatch(Action::AnnouncementsHide, &mut app);
    assert_eq!(effects.len(), 1);
    assert!(app.hidden_announcement_ids.contains("promo-a"));
    assert_eq!(shown_banner_id(&app), None, "all hidden closes the banner");
    let effects = dispatch(Action::AnnouncementsHide, &mut app);
    assert!(effects.is_empty(), "expected no effects, got {effects:?}");
}
/// `dismissible: false` pins the slot owner: hide is a silent no-op (no key
/// write, no persist effect, owner stays shown) — and a dismissible item
/// owning the slot later still hides normally.
#[test]
fn announcements_hide_noops_for_non_dismissible_owner() {
    let mut app = test_app();
    let mut pinned = critical_announcement("pinned-crit");
    pinned.dismissible = Some(false);
    app.active_announcements = vec![pinned, promo_announcement("promo-a")];
    assert_eq!(shown_banner_id(&app).as_deref(), Some("pinned-crit"));
    let effects = dispatch(Action::AnnouncementsHide, &mut app);
    assert!(
        effects.is_empty(),
        "non-dismissible hide must not persist, got {effects:?}"
    );
    assert!(
        app.hidden_announcement_ids.is_empty(),
        "non-dismissible hide must not write a hide key"
    );
    assert_eq!(
        shown_banner_id(&app).as_deref(),
        Some("pinned-crit"),
        "pinned owner stays shown"
    );
    app.active_announcements.remove(0);
    assert_eq!(shown_banner_id(&app).as_deref(), Some("promo-a"));
    let effects = dispatch(Action::AnnouncementsHide, &mut app);
    assert_eq!(effects.len(), 1);
    assert!(app.hidden_announcement_ids.contains("promo-a"));
    assert_eq!(shown_banner_id(&app), None);
}
/// Show also clears hidden promo keys (one show un-hides the whole slot).
#[test]
fn announcements_show_clears_hidden_promo_ids() {
    let mut app = test_app();
    app.hidden_announcement_ids = ["promo-a".to_string(), "unrelated".to_string()]
        .into_iter()
        .collect();
    app.active_announcements = vec![promo_announcement("promo-a")];
    assert_eq!(shown_banner_id(&app), None);
    let effects = dispatch(Action::AnnouncementsShow, &mut app);
    assert!(
        effects.iter().any(
            |e| matches!(e, Effect::PersistAnnouncementsHidden { hidden_ids } if !hidden_ids.contains("promo-a"))
        ),
        "expected persist effect without the un-hidden promo id, got {effects:?}"
    );
    assert_eq!(shown_banner_id(&app).as_deref(), Some("promo-a"));
    assert!(
        app.hidden_announcement_ids.contains("unrelated"),
        "ids not currently visible must survive show (prune owns cleanup)"
    );
}
#[test]
fn switch_model_dispatch_produces_effect_and_sets_pending() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("echo-agent-4.5"));
    assert!(!app.agents[&id].session.model_switch_pending);
    let effects = dispatch(
        Action::SwitchModel {
            model_id: model_id.clone(),
            effort: None,
        },
        &mut app,
    );
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::SwitchModel { model_id: mid, .. } if mid == &model_id));
    assert!(app.agents[&id].session.model_switch_pending);
    assert!(app.agents[&id].session.state.is_idle());
}
#[test]
fn switch_model_allowed_when_agent_chat_kind() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().chat_kind = true;
    let model_id = acp::ModelId::new(std::sync::Arc::from("auto"));
    let effects = dispatch(
        Action::SwitchModel {
            model_id: model_id.clone(),
            effort: None,
        },
        &mut app,
    );
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::SwitchModel { model_id: mid, .. } if mid == &model_id));
    assert!(app.agents[&id].session.model_switch_pending);
}
#[test]
fn switch_model_allowed_when_app_chat_mode() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.chat_mode = true;
    let model_id = acp::ModelId::new(std::sync::Arc::from("auto"));
    let effects = dispatch(
        Action::SwitchModel {
            model_id: model_id.clone(),
            effort: None,
        },
        &mut app,
    );
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::SwitchModel { model_id: mid, .. } if mid == &model_id));
    assert!(app.agents[&id].session.model_switch_pending);
}
#[test]
fn agent_type_mismatch_cancel_is_noop() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("cursor-model"));
    let agent_count_before = app.agents.len();
    let effects = dispatch(
        Action::AgentTypeMismatchAnswered {
            start_new: false,
            model_id,
            effort: None,
        },
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(app.agents.len(), agent_count_before);
    assert!(matches!(app.active_view, ActiveView::Agent(a) if a == id));
}
#[test]
fn agent_type_mismatch_with_effort_stashes_deferred_switch() {
    use echo_agent_runtime::sampling::types::ReasoningEffort;
    let mut app = test_app_with_agent();
    let model_id = acp::ModelId::new(std::sync::Arc::from("cursor-reasoning"));
    let effort = Some(ReasoningEffort::High);
    let effects = dispatch(
        Action::AgentTypeMismatchAnswered {
            start_new: true,
            model_id: model_id.clone(),
            effort,
        },
        &mut app,
    );
    let create = effects
        .iter()
        .find(|e| matches!(e, Effect::CreateSession { .. }));
    assert!(create.is_some(), "expected CreateSession effect");
    match create.unwrap() {
        Effect::CreateSession { model_id: mid, .. } => {
            assert_eq!(mid.as_ref(), Some(&model_id));
        }
        _ => unreachable!(),
    }
    if let ActiveView::Agent(new_aid) = app.active_view {
        let agent = &app.agents[&new_aid];
        assert_eq!(
            agent.session.deferred_model_switch,
            Some(crate::app::agent::DeferredModelSwitch {
                model_id,
                effort,
                prev_model_id: None,
            }),
            "effort override must be stashed for the shell via deferred_model_switch",
        );
    } else {
        panic!("expected active view to be an Agent");
    }
}
#[test]
fn deferred_model_switch_still_works_for_cli_override() {
    let mut app = test_app();
    let cli_model = acp::ModelId::new(std::sync::Arc::from("cli-override"));
    app.cli_model_override = Some(cli_model.clone());
    dispatch(Action::NewSession, &mut app);
    let id = AgentId(0);
    assert_eq!(
        app.agents[&id].session.deferred_model_switch,
        Some(crate::app::agent::DeferredModelSwitch {
            model_id: cli_model,
            effort: None,
            prev_model_id: None,
        }),
        "CLI -m override must still populate deferred_model_switch",
    );
}
#[test]
fn test_helper_agent_uses_generation_zero() {
    let app = test_app_with_agent();
    let id = AgentId(0);
    assert!(app.agents[&id].session.available_commands.is_empty());
    assert_eq!(app.agents[&id].session.available_commands_generation, 0);
    assert!(!app.agents[&id].session.model_switch_pending);
}
#[test]
fn slash_exit_dispatches_quit() {
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SendPrompt("/exit".into()), &mut app);
    assert!(
        effects.last().is_some_and(|e| matches!(e, Effect::Quit)),
        "expected Quit as last effect, got: {effects:?}"
    );
}
#[test]
fn slash_quit_alias_dispatches_quit() {
    let mut app = test_app_with_agent();
    let effects = dispatch(Action::SendPrompt("/quit".into()), &mut app);
    assert!(effects.last().is_some_and(|e| matches!(e, Effect::Quit)));
}
#[test]
fn slash_new_does_not_cancel_running_turn() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;
    let effects = dispatch(Action::SendPrompt("/new".into()), &mut app);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CreateSession { .. })),
        "expected CreateSession, got: {effects:?}",
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::CancelTurn { .. }))
    );
    assert!(
        app.agents[&id].session.state.is_turn_running(),
        "old agent's turn must remain running"
    );
}
#[test]
fn slash_new_uses_active_agent_cwd() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let agent_cwd = PathBuf::from("/custom/agent/cwd");
    app.agents.get_mut(&id).unwrap().session.cwd = agent_cwd.clone();
    let effects = dispatch(Action::SendPrompt("/new".into()), &mut app);
    let create = effects
        .iter()
        .find(|e| matches!(e, Effect::CreateSession { .. }));
    assert!(create.is_some(), "expected CreateSession effect");
    match create.unwrap() {
        Effect::CreateSession { cwd, .. } => assert_eq!(cwd, &agent_cwd),
        _ => unreachable!(),
    }
    let new_id = AgentId(1);
    assert!(!app.agents[&new_id].session.is_worktree);
}
#[test]
fn slash_model_invalid_arg_produces_scrollback_error() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let initial_scrollback = app.agents[&id].scrollback.len();
    let effects = dispatch(Action::SendPrompt("/model nonexistent".into()), &mut app);
    assert!(effects.is_empty(), "error should not produce effects");
    assert_eq!(app.agents[&id].scrollback.len(), initial_scrollback + 1);
    assert!(app.agents[&id].prompt.text().is_empty());
}
#[test]
fn slash_model_no_args_produces_scrollback_error() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let initial_scrollback = app.agents[&id].scrollback.len();
    let effects = dispatch(Action::SendPrompt("/model".into()), &mut app);
    assert!(effects.is_empty());
    assert_eq!(app.agents[&id].scrollback.len(), initial_scrollback + 1);
}
#[test]
fn slash_hooks_opens_modal() {
    let mut app = test_app_with_agent();
    app.appearance.disable_plugins = false;
    let id = AgentId(0);
    let effects = dispatch(Action::SendPrompt("/hooks".into()), &mut app);
    assert!(app.agents[&id].extensions_modal.is_some());
    assert_eq!(effects.len(), 6);
}
#[test]
fn acp_bootstrap_command_appears_in_autocomplete() {
    let mut app = test_app();
    app.bootstrap_acp_commands = vec![acp::AvailableCommand::new(
        "flush".to_string(),
        "Flush memory".to_string(),
    )];
    dispatch(Action::NewSession, &mut app);
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.sync_acp_commands(
            &agent.session.available_commands,
            agent.session.available_tools.as_ref(),
            &agent.session.models,
        );
    }
    let models = app.agents[&id].session.models.clone();
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .textarea
        .insert_str("/flu");
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .refresh_slash(&models);
    let snap = app.agents[&id].prompt.slash_snapshot();
    assert!(snap.open, "dropdown should be open");
    assert!(
        snap.matches.iter().any(|r| r.display == "/flush"),
        "bootstrap ACP command should appear in matches, got: {:?}",
        snap.matches.iter().map(|r| &r.display).collect::<Vec<_>>()
    );
}
#[test]
fn acp_bootstrap_command_executes_as_passthrough() {
    let mut app = test_app();
    app.bootstrap_acp_commands = vec![acp::AvailableCommand::new(
        "flush".to_string(),
        "Flush memory".to_string(),
    )];
    dispatch(Action::NewSession, &mut app);
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.session_id = Some("sess-1".into());
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.sync_acp_commands(
            &agent.session.available_commands,
            agent.session.available_tools.as_ref(),
            &agent.session.models,
        );
    }
    let effects = dispatch(Action::SendPrompt("/flush".into()), &mut app);
    assert_eq!(effects.len(), 1);
    assert!(
        matches!(&effects[0], Effect::SendPrompt { text, .. } if text == "/flush"),
        "ACP command should passthrough, got: {effects:?}"
    );
}
#[test]
fn acp_runtime_update_replaces_commands_in_autocomplete() {
    let mut app = test_app();
    app.bootstrap_acp_commands = vec![acp::AvailableCommand::new(
        "old-cmd".to_string(),
        "Old command".to_string(),
    )];
    dispatch(Action::NewSession, &mut app);
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.sync_acp_commands(
            &agent.session.available_commands,
            agent.session.available_tools.as_ref(),
            &agent.session.models,
        );
    }
    app.agents.get_mut(&id).unwrap().session.available_commands = vec![acp::AvailableCommand::new(
        "new-cmd".to_string(),
        "New command".to_string(),
    )];
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .available_commands_generation += 1;
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.sync_acp_commands(
            &agent.session.available_commands,
            agent.session.available_tools.as_ref(),
            &agent.session.models,
        );
    }
    let models = app.agents[&id].session.models.clone();
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .textarea
        .insert_str("/new");
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .refresh_slash(&models);
    let snap = app.agents[&id].prompt.slash_snapshot();
    assert!(
        snap.matches.iter().any(|r| r.display == "/new-cmd"),
        "new ACP command should appear"
    );
    assert!(
        !snap.matches.iter().any(|r| r.display == "/old-cmd"),
        "old ACP command should be replaced"
    );
}
#[test]
fn acp_command_colliding_with_builtin_skipped_in_autocomplete() {
    let mut app = test_app();
    app.bootstrap_acp_commands = vec![
        acp::AvailableCommand::new(
            "exit".to_string(),
            "ACP exit (should be skipped)".to_string(),
        ),
        acp::AvailableCommand::new("flush".to_string(), "Flush memory".to_string()),
    ];
    dispatch(Action::NewSession, &mut app);
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.sync_acp_commands(
            &agent.session.available_commands,
            agent.session.available_tools.as_ref(),
            &agent.session.models,
        );
    }
    let registry = app.agents[&id].prompt.slash_controller.registry();
    let exit_cmd = registry.get("exit").unwrap();
    assert_eq!(exit_cmd.description(), "Quit the application");
    assert!(registry.get("flush").is_some());
}
#[test]
fn acp_command_with_arg_hint_shows_placeholder() {
    let mut app = test_app();
    app.bootstrap_acp_commands = vec![
        acp::AvailableCommand::new("search".to_string(), "Search codebase".to_string()).input(
            Some(acp::AvailableCommandInput::Unstructured(
                acp::UnstructuredCommandInput::new("<query>".to_string()),
            )),
        ),
    ];
    dispatch(Action::NewSession, &mut app);
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.sync_acp_commands(
            &agent.session.available_commands,
            agent.session.available_tools.as_ref(),
            &agent.session.models,
        );
    }
    let registry = app.agents[&id].prompt.slash_controller.registry();
    let search_cmd = registry.get("search").unwrap();
    assert!(search_cmd.takes_args());
    assert!(!search_cmd.args_required());
    assert_eq!(search_cmd.arg_placeholder(), Some("<query>"));
}
#[test]
fn acp_command_with_args_passthrough_includes_args() {
    let mut app = test_app();
    app.bootstrap_acp_commands = vec![
        acp::AvailableCommand::new("search".to_string(), "Search codebase".to_string()).input(
            Some(acp::AvailableCommandInput::Unstructured(
                acp::UnstructuredCommandInput::new("<query>".to_string()),
            )),
        ),
    ];
    dispatch(Action::NewSession, &mut app);
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.session_id = Some("sess-1".into());
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.sync_acp_commands(
            &agent.session.available_commands,
            agent.session.available_tools.as_ref(),
            &agent.session.models,
        );
    }
    let effects = dispatch(Action::SendPrompt("/search find bugs".into()), &mut app);
    assert_eq!(effects.len(), 1);
    assert!(
        matches!(&effects[0], Effect::SendPrompt { text, .. } if text == "/search find bugs"),
        "ACP passthrough should preserve args, got: {effects:?}"
    );
}
#[test]
fn generation_lifecycle_bootstrap_through_runtime_update() {
    let mut app = test_app();
    app.bootstrap_acp_commands = vec![acp::AvailableCommand::new(
        "initial".to_string(),
        "Initial command".to_string(),
    )];
    dispatch(Action::NewSession, &mut app);
    let id = AgentId(0);
    assert_eq!(app.agents[&id].session.available_commands_generation, 1);
    assert_eq!(app.agents[&id].session.available_commands.len(), 1);
    assert_eq!(app.agents[&id].acp_synced_generation, 0);
    app.agents.get_mut(&id).unwrap().session.available_commands = vec![acp::AvailableCommand::new(
        "updated".to_string(),
        "Updated command".to_string(),
    )];
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .available_commands_generation += 1;
    assert_eq!(app.agents[&id].session.available_commands_generation, 2);
    assert_eq!(
        app.agents[&id].session.available_commands[0].name,
        "updated"
    );
}
#[test]
fn tick_propagates_available_commands_to_bootstrap() {
    let mut app = test_app();
    app.bootstrap_acp_commands = vec![acp::AvailableCommand::new(
        "compact".to_string(),
        "Builtin only".to_string(),
    )];
    dispatch(Action::NewSession, &mut app);
    let id = AgentId(0);
    app.active_view = crate::app::app_view::ActiveView::Agent(id);
    let skill_meta = serde_json::json!({
        "scope": "user",
        "path": "/home/user/.echo-agent/skills/pick-best/SKILL.md",
    });
    app.agents.get_mut(&id).unwrap().session.available_commands = vec![
        acp::AvailableCommand::new("compact".to_string(), "Builtin".to_string()),
        acp::AvailableCommand::new("pick-best".to_string(), "Parallel tournament".to_string())
            .meta(skill_meta.as_object().cloned()),
    ];
    app.agents
        .get_mut(&id)
        .unwrap()
        .session
        .available_commands_generation += 1;
    app.tick();
    assert_eq!(
        app.bootstrap_acp_commands.len(),
        2,
        "bootstrap should now include the skill"
    );
    assert!(
        app.bootstrap_acp_commands
            .iter()
            .any(|c| c.name == "pick-best"),
        "pick-best should be in bootstrap_acp_commands, got: {:?}",
        app.bootstrap_acp_commands
            .iter()
            .map(|c| &c.name)
            .collect::<Vec<_>>()
    );
    dispatch(Action::NewSession, &mut app);
    let new_id = AgentId(1);
    assert_eq!(app.agents[&new_id].session.available_commands.len(), 2);
    assert!(
        app.agents[&new_id]
            .session
            .available_commands
            .iter()
            .any(|c| c.name == "pick-best")
    );
}
#[test]
fn all_constructor_paths_initialize_slash_fields() {
    let mut app = test_app();
    dispatch(Action::NewSession, &mut app);
    {
        let s = &app.agents[&AgentId(0)].session;
        assert_eq!(s.available_commands_generation, 1);
        assert!(!s.model_switch_pending);
    }
    dispatch(Action::LoadSession("sess-1".into(), None, false), &mut app);
    {
        let s = &app.agents[&AgentId(1)].session;
        assert_eq!(s.available_commands_generation, 1);
        assert!(!s.model_switch_pending);
    }
    app.cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    app.cwd_has_git_ancestor = true;
    dispatch(
        Action::NewWorktreeSession {
            load_session_id: None,
            label: None,
            git_ref: None,
        },
        &mut app,
    );
    {
        let s = &app.agents[&AgentId(2)].session;
        assert_eq!(s.available_commands_generation, 1);
        assert!(!s.model_switch_pending);
    }
    let test_app = test_app_with_agent();
    {
        let s = &test_app.agents[&AgentId(0)].session;
        assert_eq!(s.available_commands_generation, 0);
        assert!(!s.model_switch_pending);
    }
}
#[test]
fn deferred_switch_overwritten_by_second_switch() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_a = acp::ModelId::new(std::sync::Arc::from("model-a"));
    let model_b = acp::ModelId::new(std::sync::Arc::from("model-b"));
    app.agents.get_mut(&id).unwrap().session.session_id = None;
    dispatch(
        Action::SwitchModel {
            model_id: model_a.clone(),
            effort: None,
        },
        &mut app,
    );
    dispatch(
        Action::SwitchModel {
            model_id: model_b.clone(),
            effort: None,
        },
        &mut app,
    );
    assert_eq!(
        app.agents[&id].session.deferred_model_switch,
        Some(crate::app::agent::DeferredModelSwitch {
            model_id: model_b.clone(),
            effort: None,
            prev_model_id: Some(model_a),
        })
    );
}
#[test]
fn pick_over_cli_seed_keeps_display_as_rollback_target() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let displayed = acp::ModelId::new(std::sync::Arc::from("displayed-model"));
    let cli_model = acp::ModelId::new(std::sync::Arc::from("cli-model"));
    let picked = acp::ModelId::new(std::sync::Arc::from("picked-model"));
    let agent = app.agents.get_mut(&id).unwrap();
    agent.session.session_id = None;
    agent.session.models.current = Some(displayed.clone());
    agent.session.deferred_model_switch = Some(crate::app::agent::DeferredModelSwitch {
        model_id: cli_model,
        effort: None,
        prev_model_id: None,
    });
    dispatch(
        Action::SwitchModel {
            model_id: picked.clone(),
            effort: None,
        },
        &mut app,
    );
    assert_eq!(
        app.agents[&id].session.deferred_model_switch,
        Some(crate::app::agent::DeferredModelSwitch {
            model_id: picked,
            effort: None,
            prev_model_id: Some(displayed),
        })
    );
}
#[test]
fn deferred_switch_updates_display_and_persists() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let model_id = acp::ModelId::new(std::sync::Arc::from("model-b"));
    app.agents.get_mut(&id).unwrap().session.session_id = None;
    let effects = dispatch(
        Action::SwitchModel {
            model_id: model_id.clone(),
            effort: None,
        },
        &mut app,
    );
    let agent = &app.agents[&id];
    assert_eq!(
        agent.session.models.current,
        Some(model_id.clone()),
        "pre-session pick must update the displayed model immediately"
    );
    assert_eq!(
        agent.session.deferred_model_switch,
        Some(crate::app::agent::DeferredModelSwitch {
            model_id: model_id.clone(),
            effort: None,
            prev_model_id: None,
        }),
        "switch must still round-trip once the session exists"
    );
    assert!(
        !agent.session.model_switch_pending,
        "nothing is in flight yet — the queue must not be blocked"
    );
    assert!(
        matches!(
            &effects[..],
            [Effect::PersistPreferredModel { model_id: m, .. }] if m == &model_id
        ),
        "expected a single PersistPreferredModel effect, got {effects:?}"
    );
    let effects = dispatch(
        Action::SwitchModel {
            model_id: model_id.clone(),
            effort: None,
        },
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "unchanged pre-session pick must not re-persist, got {effects:?}"
    );
}
#[test]
fn request_bundle_status_emits_effect() {
    let mut app = test_app();
    let effects = dispatch(Action::RequestBundleStatus, &mut app);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::FetchBundleStatus));
}
/// Conversation-entry resume stamps LoadSession.chat_kind; process chat_mode
/// alone does not set the entry bit (effects still stamp via SessionFlags).
#[test]
fn conversation_entry_load_sets_chat_kind_bit() {
    let mut app = test_app();
    let effects = dispatch(Action::LoadSession("conv-id".into(), None, true), &mut app);
    assert!(matches!(
        &effects[..],
        [Effect::LoadSession {
            session_id,
            chat_kind: true,
            ..
        }] if session_id == "conv-id"
    ));
    let agent = app.agents.values().next().expect("agent");
    assert!(agent.chat_kind, "conversation entry → agent chat_kind");
    assert!(
        agent.conversation_entry,
        "conversation entry must stamp conversation_entry for rename kind"
    );
    assert_eq!(
        agent.rename_kind(),
        echo_agent_runtime::session::unified_list::SessionKind::Chat
    );
    let rename = dispatch(
        Action::RenameSession {
            title: "conv title".into(),
        },
        &mut app,
    );
    assert!(
        matches!(
            &rename[..],
            [Effect::RenameSession { kind, .. }]
                if *kind == echo_agent_runtime::session::unified_list::SessionKind::Chat
        ),
        "conversation-entry rename must send kind=chat, got {rename:?}"
    );
    let reset = dispatch(Action::ResetSessionTitleToAuto, &mut app);
    assert!(
        reset.is_empty(),
        "conversation-entry --auto must refuse client-side, got {reset:?}"
    );
}
/// Process-wide `--chat` + non-conversation resume of a non-disk id still
/// loads (gateway conversation) with agent chat_kind from sticky mode.
#[test]
fn chat_mode_resume_without_local_disk_loads_as_chat() {
    let mut app = test_app();
    app.chat_mode = true;
    let effects = dispatch(
        Action::LoadSession("remote-conv-only".into(), None, false),
        &mut app,
    );
    assert!(matches!(
        &effects[..],
        [Effect::LoadSession {
            session_id,
            chat_kind: false,
            ..
        }] if session_id == "remote-conv-only"
    ));
    let agent = app.agents.values().next().expect("agent");
    assert!(
        agent.chat_kind,
        "sticky --chat must set agent chat_kind even without entry bit"
    );
    assert!(
        agent.conversation_entry,
        "sticky --chat gateway resume (no local disk) opens as chat"
    );
    assert_eq!(
        agent.rename_kind(),
        echo_agent_runtime::session::unified_list::SessionKind::Chat
    );
    assert!(
        agent.app_chat_mode,
        "app.chat_mode must propagate to AgentView::app_chat_mode"
    );
    let rename = dispatch(
        Action::RenameSession {
            title: "gw title".into(),
        },
        &mut app,
    );
    assert!(
        matches!(
            &rename[..],
            [Effect::RenameSession { kind, .. }]
                if *kind == echo_agent_runtime::session::unified_list::SessionKind::Chat
        ),
        "sticky --chat gateway resume rename must send kind=chat, got {rename:?}"
    );
}
/// Sticky `--chat` + history-bypass local-disk load stays Build for rename.
/// Without the bypass flag this same disk row is refused (see
/// `chat_mode_refuses_local_build_disk_load`).
#[cfg(feature = "local-workspace")]
#[test]
fn load_sticky_chat_history_bypass_rename_kind_is_build() {
    let cwd = PathBuf::from(format!("/tmp/chat-mode-hist-bypass-{}", std::process::id()));
    let session_id = format!("local-build-disk-{}", std::process::id());
    let sess_dir = plant_local_build_session(&cwd, &session_id);
    let mut app = test_app();
    app.cwd = cwd;
    app.chat_mode = true;
    app.welcome_history_load_as_build = true;
    let effects = dispatch(
        Action::LoadSession(session_id.clone(), None, false),
        &mut app,
    );
    let _ = std::fs::remove_dir_all(&sess_dir);
    assert!(
        matches!(
            &effects[..],
            [Effect::LoadSession {
                session_id: sid,
                chat_kind: false,
                ..
            }] if sid == &session_id
        ),
        "history-bypass must load the local disk row, got {effects:?}"
    );
    let agent = app.agents.values().next().expect("agent");
    assert!(
        agent.chat_kind,
        "sticky --chat still sets the UI chat_kind bit"
    );
    assert!(
        !agent.conversation_entry,
        "history-bypass local build must not open as chat"
    );
    assert_eq!(
        agent.rename_kind(),
        echo_agent_runtime::session::unified_list::SessionKind::Build
    );
    let rename = dispatch(
        Action::RenameSession {
            title: "local title".into(),
        },
        &mut app,
    );
    assert!(
        matches!(
            &rename[..],
            [Effect::RenameSession { kind, title, .. }]
                if *kind == echo_agent_runtime::session::unified_list::SessionKind::Build
                    && title == "local title"
        ),
        "history-bypass rename must send kind=build, got {rename:?}"
    );
}
/// Process-wide `--chat` refuses local Build disk rows (no LoadSession).
#[test]
fn chat_mode_refuses_local_build_disk_load() {
    let cwd = PathBuf::from(format!(
        "/tmp/chat-mode-build-refuse-{}",
        std::process::id()
    ));
    let session_id = format!("build-disk-{}", std::process::id());
    let sess_dir = plant_local_build_session(&cwd, &session_id);
    let mut app = test_app();
    app.cwd = cwd;
    app.chat_mode = true;
    let effects = dispatch(Action::LoadSession(session_id, None, false), &mut app);
    let _ = std::fs::remove_dir_all(&sess_dir);
    assert!(
        effects.is_empty(),
        "local Build under --chat must refuse, got {effects:?}"
    );
    assert!(
        app.agents.is_empty(),
        "refuse must not allocate an agent slot"
    );
}
/// Conversation entry under `--chat` still loads even if a local path exists.
#[test]
fn chat_mode_allows_conversation_entry_even_if_local_path() {
    let cwd = PathBuf::from(format!("/tmp/chat-mode-conv-ok-{}", std::process::id()));
    let session_id = format!("conv-also-local-{}", std::process::id());
    let sess_dir = plant_local_build_session(&cwd, &session_id);
    let mut app = test_app();
    app.cwd = cwd;
    app.chat_mode = true;
    let effects = dispatch(Action::LoadSession(session_id, None, true), &mut app);
    let _ = std::fs::remove_dir_all(&sess_dir);
    assert!(matches!(
        &effects[..],
        [Effect::LoadSession {
            chat_kind: true,
            ..
        }]
    ));
    let agent = app.agents.values().next().expect("agent");
    assert!(
        agent.conversation_entry,
        "conversation-entry bit must stamp conversation_entry even if a local path exists"
    );
    assert_eq!(
        agent.rename_kind(),
        echo_agent_runtime::session::unified_list::SessionKind::Chat
    );
}
#[test]
fn view_catalog_entry_emits_fetch_effect() {
    let mut app = test_app_with_agent();
    let effects = dispatch(
        Action::ViewCatalogEntry {
            kind: "persona".into(),
            name: "researcher".into(),
        },
        &mut app,
    );
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::FetchCatalogEntry { kind, name }
        if kind == "persona" && name == "researcher"
    ));
}
/// End-to-end regression test for the "always re-asks" requirement.
///
/// Drives the full user-visible production pipeline twice, with no
/// manual modal poking between rounds:
///   round 1: dispatch(Action::Fork) -> modal opens.
///            select option 0 ("Yes") on the modal.
///            submit_question_answers(skipped=false)
///              -> InputOutcome::Action(ForkAnswered { worktree=true })
///              -> question_view cleared by the same submit call.
///            dispatch(inner Action) -> placeholder + Effect.
///   round 2: switch focus back to parent (Y-inert, picker cause).
///            dispatch(Action::Fork) -> modal MUST re-open.
///
/// This catches BOTH:
///   (a) "no persistence in dispatch_fork" -- the only remaining
///       source of truth for whether the modal opens is the absence
///       of `args.worktree_override`; and
///   (b) "submit_question_answers clears question_view" -- a future
///       refactor that breaks the clear would also break "always
///       re-asks" in production (open_fork_question refuses when a
///       question is already on screen), so we exercise it here.
#[test]
fn dispatch_fork_no_flag_always_reopens_modal_after_previous_answer() {
    use crate::views::question_view::QuestionSelection;
    let mut app = fork_test_app();
    app.fork_worktree_mode = crate::app::app_view::WorktreeMode::Ask;
    let effects = dispatch(Action::Fork(fork_args(None, None)), &mut app);
    assert!(effects.is_empty(), "round 1: no effects until answered");
    let qv1 = app.agents[&AgentId(0)]
        .question_view
        .as_ref()
        .expect("round 1: modal opened");
    assert_eq!(
        qv1.questions[0].options.len(),
        4,
        "round 1: modal offers exactly 4 options (Yes/No/Always/Never)"
    );
    {
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        let qv = agent.question_view.as_mut().expect("modal still present");
        qv.selections[0] = QuestionSelection::Single(Some(0));
    }
    let outcome = app
        .agents
        .get_mut(&AgentId(0))
        .unwrap()
        .submit_question_answers_for_test(false);
    let inner = match outcome {
        crate::app::app_view::InputOutcome::Action(a) => a,
        other => panic!("expected InputOutcome::Action, got {other:?}"),
    };
    assert!(
        matches!(inner, Action::ForkAnswered { worktree: true, .. }),
        "submit must produce ForkAnswered with worktree=true, got {inner:?}"
    );
    assert!(
        app.agents[&AgentId(0)].question_view.is_none(),
        "submit must clear question_view on the parent agent"
    );
    let effects = dispatch(inner, &mut app);
    assert!(matches!(
        effects.as_slice(),
        [Effect::CreateWorktreeSession { .. }]
    ));
    switch_to_agent(&mut app, AgentId(0), SwitchCause::Picker);
    let effects = dispatch(Action::Fork(fork_args(None, None)), &mut app);
    assert!(effects.is_empty(), "round 2: no effects until answered");
    let qv2 = app.agents[&AgentId(0)]
        .question_view
        .as_ref()
        .expect("round 2: modal must re-open (choice never persisted)");
    assert_eq!(
        qv2.questions[0].options.len(),
        4,
        "round 2: modal still offers exactly 4 options (Yes/No/Always/Never)"
    );
}
#[test]
fn translate_local_submit_skipped_returns_changed_with_no_action() {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let q = Question {
        question: "?".into(),
        options: vec![QuestionOption {
            label: "x".into(),
            description: "x".into(),
            preview: None,
            id: None,
        }],
        multi_select: Some(false),
        id: None,
    };
    let state = QuestionViewState::new(
        "x".into(),
        vec![q],
        crate::views::prompt_widget::StashedPrompt::default(),
    );
    let kind = LocalQuestionKind::Fork {
        directive: Some("dropped".into()),
    };
    let outcome = crate::app::agent_view::translate_local_submit_for_test(&state, kind, true);
    assert!(matches!(
        outcome,
        crate::app::app_view::InputOutcome::Changed
    ));
}
#[test]
fn translate_local_submit_no_selection_returns_changed_no_action() {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let q = Question {
        question: "?".into(),
        options: (0..2)
            .map(|_| QuestionOption {
                label: "opt".into(),
                description: String::new(),
                preview: None,
                id: None,
            })
            .collect(),
        multi_select: Some(false),
        id: None,
    };
    let state = QuestionViewState::new(
        "x".into(),
        vec![q],
        crate::views::prompt_widget::StashedPrompt::default(),
    );
    let kind = LocalQuestionKind::Fork { directive: None };
    let outcome = crate::app::agent_view::translate_local_submit_for_test(&state, kind, false);
    assert!(matches!(
        outcome,
        crate::app::app_view::InputOutcome::Changed
    ));
}
#[test]
fn translate_local_submit_out_of_range_index_returns_changed_no_action() {
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let q = Question {
        question: "?".into(),
        options: (0..2)
            .map(|_| QuestionOption {
                label: "opt".into(),
                description: String::new(),
                preview: None,
                id: None,
            })
            .collect(),
        multi_select: Some(false),
        id: None,
    };
    let mut state = QuestionViewState::new(
        "x".into(),
        vec![q],
        crate::views::prompt_widget::StashedPrompt::default(),
    );
    state.selections[0] = crate::views::question_view::QuestionSelection::Single(Some(99));
    let kind = LocalQuestionKind::Fork { directive: None };
    let outcome = crate::app::agent_view::translate_local_submit_for_test(&state, kind, false);
    assert!(matches!(
        outcome,
        crate::app::app_view::InputOutcome::Changed
    ));
}
#[test]
fn handle_ask_user_question_does_not_push_system_block_when_displaced_acp_modal() {
    use crate::views::question_view::QuestionViewState;
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    let mut app = fork_test_app();
    let id = AgentId(0);
    let stashed = app.agents.get_mut(&id).unwrap().prompt.stash();
    let q = Question {
        question: "first ACP question?".into(),
        options: vec![QuestionOption {
            label: "ok".into(),
            description: "ok".into(),
            preview: None,
            id: None,
        }],
        multi_select: Some(false),
        id: None,
    };
    app.agents.get_mut(&id).unwrap().question_view =
        Some(QuestionViewState::new("first-acp".into(), vec![q], stashed));
    let scrollback_len_before = app.agents[&id].scrollback.len();
    let (args, _rx) = make_ask_user_question_args("second-acp");
    let handled = crate::app::acp_handler::handle_ask_user_question(args, &mut app);
    assert!(handled);
    let qv = app.agents[&id].question_view.as_ref().unwrap();
    assert_eq!(qv.tool_call_id, "second-acp");
    assert_eq!(
        app.agents[&id].scrollback.len(),
        scrollback_len_before,
        "no system block when displaced modal was an ACP question, not a local one"
    );
}
#[test]
fn close_active_agent_no_parent_switches_to_first_surviving_peer() {
    let mut app = three_agent_app();
    switch_to_agent(&mut app, AgentId(1), SwitchCause::Picker);
    dispatch_sessions_confirm_close(&mut app, AgentId(1));
    assert!(matches!(app.active_view, ActiveView::Agent(id) if id == AgentId(0)));
    assert!(!app.agents.contains_key(&AgentId(1)));
}
#[test]
fn close_active_agent_with_dead_parent_falls_back_to_surviving_peer() {
    let mut app = three_agent_app();
    set_forked_from(&mut app, AgentId(2), AgentId(99));
    switch_to_agent(&mut app, AgentId(2), SwitchCause::Picker);
    dispatch_sessions_confirm_close(&mut app, AgentId(2));
    assert!(matches!(app.active_view, ActiveView::Agent(id) if id == AgentId(0)));
}
#[test]
fn entry_title_uses_display_name_when_set() {
    use crate::views::session_title::entry_title;
    let mut app = test_app_with_agent();
    if let Some(a) = app.agents.get_mut(&AgentId(0)) {
        a.display_name = Some("custom title".into());
    }
    let title = entry_title(&app.agents[&AgentId(0)]);
    assert_eq!(title, "custom title");
}
#[test]
fn find_agent_by_session_id_returns_none_for_unknown() {
    let mut app = test_app_with_agent();
    assert!(find_agent_by_session_id(&mut app.agents, "nonexistent").is_none());
}
#[test]
fn find_agent_by_session_id_returns_none_when_session_id_is_none() {
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().session.session_id = None;
    assert!(find_agent_by_session_id(&mut app.agents, "test-session").is_none());
}
#[test]
fn find_agent_by_session_id_finds_inactive_agent() {
    let mut app = two_agent_app_with_bg_task();
    let found = find_agent_by_session_id(&mut app.agents, "sess-B");
    assert!(found.is_some());
    assert_eq!(
        found.unwrap().session.session_id,
        Some(acp::SessionId::new("sess-B"))
    );
}
/// Cross-setting smoke test.
/// Verifies that the dispatcher routes each Action to the
/// correct setter (catches a copy-paste registration bug
/// where two setters were swapped). The original 5-setting
/// matrix shrank to 2 after the user-feedback drop of
/// `session_picker_grouped` / `load_envrc` / `use_leader`.
#[test]
fn pr13_each_setter_writes_to_its_own_mirror() {
    let mut app = test_app_with_agent();
    assert_eq!(app.show_tips, None);
    assert_eq!(app.auto_update, None);
    let _ = dispatch(Action::SetShowTips(false), &mut app);
    assert_eq!(app.show_tips, Some(false));
    assert_eq!(app.auto_update, None);
    let _ = dispatch(Action::SetAutoUpdate(false), &mut app);
    assert_eq!(app.auto_update, Some(false));
    assert_eq!(app.show_tips, Some(false));
}
/// Three-way alignment pin: the PAGER registry default must agree
/// with `PagerLocalSnapshot::default()` (covered by
/// `defaults_match_pager_state` in `registry::tests`) AND with
/// `AgentView::new`'s runtime initializer. This is the third leg
/// of the triangle that was previously missing — the
/// registry test alone can't see `AgentView::new`'s constant.
#[test]
fn pager_registry_default_matches_agent_view_new_initializer() {
    use crate::settings::{SettingKind, SettingOwner, SettingsRegistry};
    let app = test_app_with_agent();
    let agent = app
        .agents
        .get(&AgentId(0))
        .expect("test_app_with_agent must create AgentId(0)");
    let reg = SettingsRegistry::defaults();
    for meta in reg.all() {
        if meta.owner != SettingOwner::Pager {
            continue;
        }
        match (meta.key, &meta.kind) {
            ("multiline_mode", SettingKind::Bool { default }) => {
                assert_eq!(
                    *default, agent.multiline_mode,
                    "registry default for `multiline_mode` ({default}) drifts from \
                         AgentView::new's initializer ({}). Update one to match the \
                         other — the registry is the contract surface.",
                    agent.multiline_mode,
                );
            }
            ("plan_mode", SettingKind::Enum { default, .. }) => {
                let effective = agent.plan_mode_pending.unwrap_or(agent.plan_mode_active);
                let expected = if effective { "on" } else { "off" };
                assert_eq!(
                    *default, expected,
                    "registry default for `plan_mode` (`{default}`) drifts from \
                         AgentView::new's initializer (effective={effective} → \
                         expected `{expected}`). Update one to match the other — \
                         the registry is the contract surface.",
                );
            }
            ("respect_manual_folds", SettingKind::Bool { default }) => {
                let live = agent
                    .scrollback
                    .appearance()
                    .scrollback
                    .scroll
                    .respect_manual_folds;
                assert_eq!(
                    *default, live,
                    "registry default for `respect_manual_folds` ({default}) drifts \
                         from the agent's default appearance config ({live}). Update one \
                         to match the other — ScrollConfig::default() is the source of \
                         truth.",
                );
            }
            _ => {
                panic!(
                    "PAGER setting `{}` has no arm in \
                     pager_registry_default_matches_agent_view_new_initializer — \
                     add an arm that pins the registry default against the runtime \
                     initializer in `AgentView::new` (or, for future fields, the \
                     equivalent runtime construction site).",
                    meta.key,
                )
            }
        }
    }
}
/// If the user picks the regular "Yes, proceed" option (NOT
/// enable-always-approve), the dispatcher must behave exactly as
/// before — no PersistPermissionMode effect, no YOLO flip. Pins
/// that the new code path is gated strictly on the id check.
#[test]
fn regular_allow_once_does_not_trigger_always_approve_persist() {
    use std::sync::Arc;
    let mut app = test_app_with_agent();
    let _response_rx = enqueue_permission_with_enable_always_approve(&mut app);
    let effects = dispatch(
        Action::PermissionSelect(acp::PermissionOptionId::new(Arc::from("opt-allow-once"))),
        &mut app,
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::PersistPermissionMode { .. })),
        "picking the regular AllowOnce option must NOT emit PersistPermissionMode — \
             the always-approve mode is opt-in via the dedicated option only",
    );
    assert!(
        !app.agents[&AgentId(0)].session.is_yolo(),
        "session.yolo_mode must remain OFF when the regular AllowOnce option is picked",
    );
    assert!(
        !app.default_yolo,
        "app.default_yolo must remain OFF when the regular AllowOnce option is picked",
    );
}
/// Launch-time blocked `--yolo` in the TUI: the one-shot notice is
/// surfaced (toast + durable system line) on the first agent view and
/// consumed so later switches stay quiet.
#[test]
fn switch_to_agent_surfaces_launch_block_notice_once() {
    let mut app = test_app();
    app.yolo_launch_block_notice = Some(POLICY_WARNING);
    let id = AgentId(0);
    let session = make_test_agent_session(&app, id, "test-session");
    app.agents
        .insert(id, AgentView::new(session, ScrollbackState::new()));
    switch_to_agent(&mut app, id, SwitchCause::New);
    let agent = &app.agents[&id];
    assert_eq!(
        agent.toast.as_ref().map(|(s, _)| s.as_str()),
        Some(POLICY_WARNING),
    );
    let system_texts: Vec<&str> = agent
        .scrollback
        .iter_entries()
        .filter_map(|(_, e)| match &e.block {
            crate::scrollback::block::RenderBlock::System(s) => Some(s.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        system_texts,
        vec![POLICY_WARNING],
        "warning must land in the transcript exactly once",
    );
    assert_eq!(
        app.yolo_launch_block_notice, None,
        "one-shot must be consumed"
    );
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "test-session-2");
    app.agents
        .insert(id2, AgentView::new(session2, ScrollbackState::new()));
    switch_to_agent(&mut app, id2, SwitchCause::New);
    assert!(app.agents[&id2].toast.is_none());
    assert_eq!(app.agents[&id2].scrollback.iter_entries().count(), 0);
}
/// Switching to a non-auto/non-yolo agent re-anchors a stale global
/// `"auto"` mirror (left by a different agent) to `"ask"`, so the cycle's
/// `sync_active_auto_flag` derive can't copy that Auto onto the now-active
/// agent.
#[test]
fn switch_to_agent_reanchors_stale_global_auto() {
    let mut app = test_app_with_agent();
    let id2 = AgentId(1);
    let session2 = make_test_agent_session(&app, id2, "test-session-2");
    app.agents
        .insert(id2, AgentView::new(session2, ScrollbackState::new()));
    app.next_agent_id = 2;
    app.current_ui.permission_mode = Some("auto".into());
    switch_to_agent(&mut app, id2, SwitchCause::Picker);
    assert_eq!(
        app.current_ui.permission_mode.as_deref(),
        Some("ask"),
        "switching to a non-auto agent must clear the stale global auto"
    );
    assert!(!app.agents[&id2].session.is_auto());
}
#[test]
fn show_tasks_empty_commits_empty_message() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    let effects = dispatch(Action::ShowTasks, &mut app);
    assert!(effects.is_empty(), "got: {effects:?}");
    assert_eq!(agent_scrollback_len(&app), before + 1);
    assert_eq!(
        last_system_text(&app, AgentId(0)),
        "No background tasks, workflows, or subagents."
    );
}
#[test]
fn show_tasks_lists_a_scheduled_task() {
    use crate::app::agent::ScheduledTaskInfo;
    let mut app = test_app_with_agent();
    {
        let agent = app.agents.get_mut(&AgentId(0)).unwrap();
        agent.session.scheduled_tasks.insert(
            "t1".to_string(),
            ScheduledTaskInfo {
                task_id: "t1".to_string(),
                prompt: "check CI status".to_string(),
                human_schedule: "every 5m".to_string(),
                created_at: std::time::Instant::now(),
                next_fire_at: None,
                tag: "loop".to_string(),
                last_subagent_id: None,
            },
        );
    }
    let effects = dispatch(Action::ShowTasks, &mut app);
    assert!(effects.is_empty(), "got: {effects:?}");
    let text = last_system_text(&app, AgentId(0));
    assert!(text.contains("Task (1):"), "got: {text:?}");
    assert!(
        text.contains("loop · every 5m · check CI status"),
        "got: {text:?}"
    );
    assert!(text.contains("scheduled"), "got: {text:?}");
}
#[test]
fn show_tasks_no_active_agent_is_noop() {
    let mut app = test_app();
    let effects = dispatch(Action::ShowTasks, &mut app);
    assert!(effects.is_empty(), "ShowTasks without an agent is a no-op");
}
/// classify_top_level decision matrix.
#[test]
fn classify_top_level_branches() {
    use crate::views::dashboard::{RowState, classify_top_level};
    let mut app = test_app_with_agent();
    let agent = app.agents.get(&AgentId(0)).unwrap();
    assert_eq!(classify_top_level(agent), RowState::Idle);
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.session.state = AgentState::TurnRunning;
    assert_eq!(classify_top_level(agent), RowState::Working);
    agent.session.state = AgentState::Idle;
    agent.session.loading_replay = true;
    assert_eq!(classify_top_level(agent), RowState::Working);
    agent.session.loading_replay = false;
}
/// For Idle rows, `last_change_at` is the
/// frozen `last_active_at` anchor. Building the row twice in
/// rapid succession against a fixed `last_active_at` yields
/// nearly-identical `elapsed()` values (within tolerance).
#[test]
fn build_rows_idle_anchor_is_frozen_last_active_at() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let anchor = std::time::Instant::now() - std::time::Duration::from_secs(300);
    agent.last_active_at = Some(anchor);
    agent.turn_started_at = None;
    agent.session.state = crate::app::agent::AgentState::Idle;
    let rows1 = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    let row1 = &rows1[0];
    assert_eq!(row1.state, crate::views::dashboard::RowState::Idle);
    let elapsed1 = row1.last_change_at.elapsed().unwrap_or_default();
    assert!(
        elapsed1 >= std::time::Duration::from_secs(299),
        "expected >= 299s, got {elapsed1:?}",
    );
    let rows2 = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    let elapsed2 = rows2[0].last_change_at.elapsed().unwrap_or_default();
    assert!(
        elapsed2 >= std::time::Duration::from_secs(299),
        "idle anchor must stay frozen across rebuilds, got {elapsed2:?}",
    );
}
/// Working rows anchor at `turn_started_at` so
/// the age column shows the LIVE elapsed time within the current
/// turn rather than the time since the previous turn ended.
#[test]
fn build_rows_working_anchor_is_turn_started_at() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let turn_start = std::time::Instant::now() - std::time::Duration::from_secs(5);
    let stale = std::time::Instant::now() - std::time::Duration::from_secs(600);
    agent.turn_started_at = Some(turn_start);
    agent.last_active_at = Some(stale);
    agent.session.state = crate::app::agent::AgentState::TurnRunning;
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    let row = &rows[0];
    assert_eq!(row.state, crate::views::dashboard::RowState::Working);
    let elapsed = row.last_change_at.elapsed().unwrap_or_default();
    assert!(
        elapsed >= std::time::Duration::from_secs(4)
            && elapsed < std::time::Duration::from_secs(30),
        "expected ~5s (turn_started_at anchor), got {elapsed:?}",
    );
}
/// Defensive test for the fallback
/// path when both `turn_started_at` and `last_active_at` are
/// `None`. The row's `last_change_at` projects the *frozen*
/// process-wide `fallback_epoch`, so two consecutive builds yield
/// stable `last_change_at` values (within sampling jitter) rather
/// than re-anchoring at `now` and showing "0s" every frame.
#[test]
fn build_rows_fallback_anchor_is_frozen_when_last_active_at_is_none() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    mark_agent_nonempty(&mut app, AgentId(0));
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.last_active_at = None;
    agent.turn_started_at = None;
    agent.session.state = crate::app::agent::AgentState::Idle;
    let rows1 = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    let rows2 = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    let (t1, t2) = (rows1[0].last_change_at, rows2[0].last_change_at);
    let drift = t1.duration_since(t2).unwrap_or_else(|e| e.duration());
    assert!(
        drift < std::time::Duration::from_secs(1),
        "fallback anchor must be frozen across rebuilds, drifted {drift:?}",
    );
}
/// While the turn is IDLE the peek header label reflects the TYPE of the
/// most recent agent block (Response / Edit / Thought / …) via the
/// scrollback scan. The most recent block wins; a fresh user prompt is a
/// turn boundary with no agent response after it → "Idle". (The RUNNING
/// case follows live turn activity — see the `extract_response_type_*`
/// tests.)
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn peek_label_reflects_last_response_type() {
    use crate::scrollback::block::RenderBlock;
    use crate::views::dashboard::peek::extract_last_response_type;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::agent_message("hi"));
    assert_eq!(extract_last_response_type(agent), "Response");
    agent
        .scrollback
        .push_block(RenderBlock::tool_call("edit", "src/x.rs", true));
    assert_eq!(extract_last_response_type(agent), "Edit");
    agent.scrollback.push_block(RenderBlock::thinking("hmm"));
    assert_eq!(extract_last_response_type(agent), "Thought");
    agent
        .scrollback
        .push_block(RenderBlock::user_prompt("do it"));
    assert_eq!(extract_last_response_type(agent), "Idle");
}
/// agent.question_view.is_some() → NeedsInput.
#[test]
fn classify_top_level_question_view_some_is_needs_input() {
    use crate::views::dashboard::{RowState, classify_top_level};
    use crate::views::question_view::QuestionViewState;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.question_view = Some(QuestionViewState::new(
        "tc-1".to_string(),
        Vec::new(),
        crate::views::prompt_widget::StashedPrompt::default(),
    ));
    assert_eq!(classify_top_level(agent), RowState::NeedsInput);
}
/// ANSI escapes in `display_name` are stripped at row
/// build time.
#[test]
fn top_level_label_strips_control_characters() {
    use crate::views::dashboard::build_rows;
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.display_name = Some("a\x1b[31mevil\x1b[0m".to_string());
    let rows = build_rows(
        &app.agents,
        &std::collections::BTreeSet::new(),
        &[],
        None,
        crate::views::dashboard::Grouping::State,
        &crate::views::dashboard::Filter::None,
        None,
    );
    let top = rows.iter().find(|r| r.indent == 0).expect("top row");
    assert!(
        !top.label.contains('\x1b'),
        "label must not retain \\x1b: {:?}",
        top.label
    );
    assert!(top.label.contains("evil"));
}
/// Build a synthetic MouseEvent for tests.
fn mouse_event(
    kind: crossterm::event::MouseEventKind,
    col: u16,
    row: u16,
) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind,
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }
}
/// Left-click on a row selects it (single click).
/// Single left-click on a row attaches the
/// conversation immediately (was: selects only, required
/// double-click to attach). The user explicitly reported the
/// previous click-to-select behaviour as unresponsive.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn mouse_left_click_attaches_immediately() {
    use crossterm::event::{Event, MouseButton, MouseEventKind};
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let d = app.dashboard.as_mut().unwrap();
    let id = crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0));
    d.row_rects
        .push((id.clone(), ratatui::layout::Rect::new(0, 5, 80, 1)));
    d.selected = None;
    let outcome = d.handle_input(
        &Event::Mouse(mouse_event(MouseEventKind::Down(MouseButton::Left), 10, 5)),
        &crate::actions::ActionRegistry::defaults(),
    );
    match outcome {
        crate::app::app_view::InputOutcome::Action(
            crate::app::actions::Action::DashboardAttach(actual),
        ) => {
            assert_eq!(actual, id, "single click must attach the clicked row");
        }
        other => panic!("expected DashboardAttach on single click, got {other:?}"),
    }
    assert_eq!(d.selected, Some(id));
}
/// Every left-click attaches, including
/// rapid repeated clicks. The previous design used a 500ms window
/// to distinguish single (select) from double (attach) click;
/// the new design makes every click attach so the user's mental
/// model "click = open" always holds.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn mouse_repeated_click_keeps_attaching() {
    use crossterm::event::{Event, MouseButton, MouseEventKind};
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let d = app.dashboard.as_mut().unwrap();
    let id = crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0));
    d.row_rects
        .push((id.clone(), ratatui::layout::Rect::new(0, 5, 80, 1)));
    let reg = crate::actions::ActionRegistry::defaults();
    let outcome1 = d.handle_input(
        &Event::Mouse(mouse_event(MouseEventKind::Down(MouseButton::Left), 10, 5)),
        &reg,
    );
    match outcome1 {
        crate::app::app_view::InputOutcome::Action(
            crate::app::actions::Action::DashboardAttach(actual),
        ) => assert_eq!(actual, id),
        other => panic!("expected DashboardAttach on first click, got {other:?}"),
    }
    let outcome2 = d.handle_input(
        &Event::Mouse(mouse_event(MouseEventKind::Down(MouseButton::Left), 10, 5)),
        &reg,
    );
    match outcome2 {
        crate::app::app_view::InputOutcome::Action(
            crate::app::actions::Action::DashboardAttach(actual),
        ) => assert_eq!(actual, id),
        other => panic!("expected DashboardAttach on second click, got {other:?}"),
    }
}
/// Clicks after the previous 500ms-double-click
/// window also attach (the previous test asserted single-click
/// behaviour for >500ms-apart clicks; now every click attaches).
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn mouse_click_after_long_pause_still_attaches() {
    use crossterm::event::{Event, MouseButton, MouseEventKind};
    use std::time::{Duration, Instant};
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let d = app.dashboard.as_mut().unwrap();
    let id = crate::views::dashboard::DashboardRowId::TopLevel(AgentId(0));
    d.row_rects
        .push((id.clone(), ratatui::layout::Rect::new(0, 5, 80, 1)));
    let reg = crate::actions::ActionRegistry::defaults();
    let _ = d.handle_input(
        &Event::Mouse(mouse_event(MouseEventKind::Down(MouseButton::Left), 10, 5)),
        &reg,
    );
    if let Some((_, t)) = d.last_click.as_mut() {
        *t = Instant::now() - Duration::from_millis(600);
    }
    let outcome = d.handle_input(
        &Event::Mouse(mouse_event(MouseEventKind::Down(MouseButton::Left), 10, 5)),
        &reg,
    );
    match outcome {
        crate::app::app_view::InputOutcome::Action(
            crate::app::actions::Action::DashboardAttach(actual),
        ) => assert_eq!(actual, id),
        other => panic!("expected DashboardAttach, got {other:?}"),
    }
}
/// Click on the peek close-button rect closes the peek.
#[serial_test::serial(ECHO_AGENT_AGENT_DASHBOARD)]
#[test]
fn mouse_click_on_peek_close_rect_clears_peek() {
    use crossterm::event::{Event, MouseButton, MouseEventKind};
    let mut app = test_app_with_agent();
    open_dashboard(&mut app);
    let d = app.dashboard.as_mut().unwrap();
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
    d.peek_close_rect = Some(ratatui::layout::Rect::new(70, 8, 3, 1));
    let reg = crate::actions::ActionRegistry::defaults();
    let outcome = d.handle_input(
        &Event::Mouse(mouse_event(MouseEventKind::Down(MouseButton::Left), 71, 8)),
        &reg,
    );
    assert!(matches!(
        outcome,
        crate::app::app_view::InputOutcome::Changed
    ));
    assert!(d.peek.is_none());
    assert!(d.peek_close_rect.is_none());
}
/// Same refusal at the content-hit worktree entry point.
#[test]
fn pick_content_session_in_worktree_refuses_conversation_row() {
    let mut app = test_app_with_agent();
    open_session_picker_with(&mut app, vec![make_conversation_entry("conv-wt-2")]);
    let effects = dispatch(
        Action::PickContentSessionInWorktree {
            session_id: "conv-wt-2".into(),
            cwd: String::new(),
        },
        &mut app,
    );
    assert!(effects.is_empty(), "no worktree effects, got {effects:?}");
    assert!(
        !app.deferred_startup.pending_chat,
        "refusal must not set the one-shot chat bit"
    );
    assert!(read_toast(&app).contains("worktree"));
}
/// Delete acts on local disk + registry; conversation rows have neither.
#[test]
fn delete_session_refuses_conversation_row() {
    let mut app = test_app_with_agent();
    open_session_picker_with(&mut app, vec![make_conversation_entry("conv-del-1")]);
    let effects = dispatch(
        Action::DeleteSession {
            source: "conversation".into(),
            session_id: "conv-del-1".into(),
            cwd: String::new(),
        },
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "no DeleteSession effect for a conversation row, got {effects:?}"
    );
    assert!(read_toast(&app).contains("isn't supported"));
}
/// Expanding a conversation card must not read `chat_history.jsonl`
/// (it doesn't exist); the row still toggles open.
#[test]
fn expand_conversation_card_skips_detail_load() {
    use crate::views::modal::ActiveModal;
    let mut app = test_app_with_agent();
    open_session_picker_with(&mut app, vec![make_conversation_entry("conv-exp-1")]);
    let effects = dispatch(
        Action::ExpandSessionCard {
            source: "conversation".into(),
            session_id: "conv-exp-1".into(),
        },
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "no LoadCardDetail for a conversation row, got {effects:?}"
    );
    let agent = get_active_agent(&app).expect("active agent");
    let Some(ActiveModal::SessionPicker { state, .. }) = agent.active_modal.as_ref() else {
        panic!("expected SessionPicker modal");
    };
    assert!(state.expanded.contains(&0), "row still toggles open");
}
/// Canary: Build rows still lazy-load card detail on expand.
#[test]
fn expand_build_card_still_loads_detail() {
    let mut app = test_app_with_agent();
    open_session_picker_with(&mut app, vec![make_picker_entry("local-exp-1", "/r")]);
    let effects = dispatch(
        Action::ExpandSessionCard {
            source: "local".into(),
            session_id: "local-exp-1".into(),
        },
        &mut app,
    );
    assert!(
        matches!(&effects[..], [Effect::LoadCardDetail { .. }]),
        "expected LoadCardDetail, got {effects:?}"
    );
}
/// Welcome-screen variants of the conversation-card expand exemption.
#[test]
fn welcome_expand_conversation_card_skips_detail_load() {
    let mut app = test_app();
    app.session_picker_entries = Some(vec![make_conversation_entry("conv-exp-w1")]);
    let effects = dispatch(
        Action::ExpandSessionCard {
            source: "conversation".into(),
            session_id: "conv-exp-w1".into(),
        },
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "no LoadCardDetail for a welcome conversation row, got {effects:?}"
    );
    assert!(
        app.session_picker_state.expanded.contains(&0),
        "row still toggles open"
    );
    let mut app = test_app();
    app.session_picker_entries = Some(vec![make_picker_entry("local-exp-w1", "/r")]);
    let effects = dispatch(
        Action::ExpandSessionCard {
            source: "local".into(),
            session_id: "local-exp-w1".into(),
        },
        &mut app,
    );
    assert!(
        matches!(&effects[..], [Effect::LoadCardDetail { .. }]),
        "expected LoadCardDetail, got {effects:?}"
    );
}
/// Collect the active agent's system-block texts.
fn system_texts(app: &AppView, id: AgentId) -> Vec<String> {
    app.agents[&id]
        .scrollback
        .iter_entries()
        .filter_map(|(_, e)| match &e.block {
            crate::scrollback::block::RenderBlock::System(s) => Some(s.text.clone()),
            _ => None,
        })
        .collect()
}
#[test]
fn toggle_fps_hud_round_trips() {
    let mut app = test_app();
    assert!(!app.fps_hud.enabled(), "FPS HUD must start off");
    let _ = dispatch(Action::ToggleFpsHud, &mut app);
    assert!(app.fps_hud.enabled());
    let _ = dispatch(Action::ToggleFpsHud, &mut app);
    assert!(!app.fps_hud.enabled());
}
/// `/debug` bare: one system line reporting every toggle's state.
#[test]
fn show_debug_status_emits_toggle_states_to_transcript() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    if app.scroll_state.scroll_log_active() {
        let _ = app.scroll_state.toggle_scroll_log();
    }
    let _ = dispatch(Action::ToggleFpsHud, &mut app);
    let _ = dispatch(Action::ShowDebugStatus, &mut app);
    let texts = system_texts(&app, id);
    let status = texts
        .iter()
        .find(|t| t.starts_with("debug toggles:"))
        .unwrap_or_else(|| panic!("status line missing, got {texts:?}"));
    assert!(status.contains("fps on"), "got: {status}");
    assert!(status.contains("log off"), "got: {status}");
    assert!(
        status.contains("/debug"),
        "must point at the command: {status}"
    );
}
/// `/debug log`: the toggle flips the recorder and echoes where it writes.
#[test]
fn toggle_scroll_log_flips_recorder_and_reports_path() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    if app.scroll_state.scroll_log_active() {
        let _ = app.scroll_state.toggle_scroll_log();
    }
    let _ = dispatch(Action::ToggleScrollLog, &mut app);
    assert!(app.scroll_state.scroll_log_active());
    let _ = dispatch(Action::ToggleScrollLog, &mut app);
    assert!(!app.scroll_state.scroll_log_active());
    let texts = system_texts(&app, id);
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("scroll log: recording to")),
        "enable must echo the log path, got {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "scroll log: off"),
        "disable must be confirmed, got {texts:?}"
    );
}
