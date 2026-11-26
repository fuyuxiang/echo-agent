//! Tests for feedback / remember / btw / recap dispatchers.

use super::*;
use crate::app::dispatch::{recap_unavailable_toast, scrollback_has_user_messages};

fn send_minimal_btw(app: &mut AppView, question: &str) -> uuid::Uuid {
    match dispatch(Action::SendBtw(question.into()), app).as_slice() {
        [
            Effect::SendBtw {
                minimal_request_id: Some(id),
                ..
            },
        ] => *id,
        other => panic!("expected correlated minimal /btw effect, got {other:?}"),
    }
}

fn esc() -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ))
}

#[test]
fn recap_unavailable_toast_empty_vs_with_messages() {
    assert_eq!(recap_unavailable_toast(false), "No messages yet");
    assert_eq!(recap_unavailable_toast(true), "Couldn't generate recap");
}

#[test]
fn manual_recap_with_no_messages_toasts_empty_state_and_skips_request() {
    let mut app = test_app_with_agent();
    app.session_recap_available = true;
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.set_text("/recap");
        assert!(!scrollback_has_user_messages(&agent.scrollback));
    }

    let effects = dispatch(Action::SendRecap { auto: false }, &mut app);

    assert!(
        effects.is_empty(),
        "empty session must not fire echo.agent/recap: {effects:?}"
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_recap_entry.is_none(), "no loading spinner");
    assert_eq!(
        agent.toast.as_ref().map(|(s, _)| s.as_str()),
        Some("No messages yet"),
        "empty session should say No messages yet, not Couldn't generate recap"
    );
    assert_eq!(agent.prompt.text(), "", "slash command text is cleared");
}

#[test]
fn manual_recap_with_messages_requests_and_shows_spinner() {
    let mut app = test_app_with_agent();
    app.session_recap_available = true;
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("hello"));
        assert!(scrollback_has_user_messages(&agent.scrollback));
    }

    let effects = dispatch(Action::SendRecap { auto: false }, &mut app);

    assert!(
        matches!(effects.as_slice(), [Effect::SendRecap { auto: false, .. }]),
        "expected SendRecap effect, got {effects:?}"
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(
        agent.pending_recap_entry.is_some(),
        "manual recap shows a loading spinner when there is something to summarize"
    );
    assert!(agent.toast.is_none());
}

/// Regression: during session/load, scrollback is batched so
/// `turn_count()` stays 0 until `end_batch`, but UserPrompt entries may already
/// be present. Manual `/recap` must still request a recap.
#[test]
fn manual_recap_during_batch_load_with_prompts_still_requests() {
    let mut app = test_app_with_agent();
    app.session_recap_available = true;
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.scrollback.begin_batch();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("hello from resume"));
        // Batched push defers rebuild_turns — turn index is stale, entries aren't.
        assert_eq!(agent.scrollback.turn_count(), 0);
        assert!(scrollback_has_user_messages(&agent.scrollback));
    }

    let effects = dispatch(Action::SendRecap { auto: false }, &mut app);

    assert!(
        matches!(effects.as_slice(), [Effect::SendRecap { auto: false, .. }]),
        "batched resume with user prompts must still fire echo.agent/recap: {effects:?}"
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_recap_entry.is_some());
    assert!(agent.toast.is_none());
    // Clean up batch for the test fixture (not required for the assertion).
    app.agents.get_mut(&id).unwrap().scrollback.end_batch();
}

/// While session replay is still streaming, don't claim "No messages yet" even
/// if scrollback looks empty — history may arrive on the next notification.
#[test]
fn manual_recap_while_loading_replay_still_requests() {
    let mut app = test_app_with_agent();
    app.session_recap_available = true;
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.loading_replay = true;
        assert!(!scrollback_has_user_messages(&agent.scrollback));
    }

    let effects = dispatch(Action::SendRecap { auto: false }, &mut app);

    assert!(
        matches!(effects.as_slice(), [Effect::SendRecap { auto: false, .. }]),
        "loading_replay must not short-circuit to No messages yet: {effects:?}"
    );
    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_recap_entry.is_some());
    assert!(agent.toast.is_none());
}

#[test]
fn recap_request_transport_failure_with_no_turns_uses_empty_toast() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let session_id = app.agents[&id].session.session_id.clone().unwrap();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let spinner = agent
            .scrollback
            .push(crate::scrollback::entry::ScrollbackEntry::running(
                RenderBlock::session_event(SessionEvent::Recap {
                    summary: String::new(),
                    auto: false,
                }),
            ));
        agent.pending_recap_entry = Some(spinner);
        assert!(!scrollback_has_user_messages(&agent.scrollback));
    }

    dispatch(
        Action::TaskComplete(TaskResult::RecapRequested {
            session_id,
            auto: false,
            error: Some("transport down".into()),
        }),
        &mut app,
    );

    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_recap_entry.is_none());
    assert_eq!(
        agent.toast.as_ref().map(|(s, _)| s.as_str()),
        Some("No messages yet")
    );
}

#[test]
fn recap_request_transport_failure_with_turns_uses_generic_toast() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let session_id = app.agents[&id].session.session_id.clone().unwrap();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("hello"));
        let spinner = agent
            .scrollback
            .push(crate::scrollback::entry::ScrollbackEntry::running(
                RenderBlock::session_event(SessionEvent::Recap {
                    summary: String::new(),
                    auto: false,
                }),
            ));
        agent.pending_recap_entry = Some(spinner);
        assert!(scrollback_has_user_messages(&agent.scrollback));
    }

    dispatch(
        Action::TaskComplete(TaskResult::RecapRequested {
            session_id,
            auto: false,
            error: Some("transport down".into()),
        }),
        &mut app,
    );

    let agent = app.agents.get(&id).unwrap();
    assert!(agent.pending_recap_entry.is_none());
    assert_eq!(
        agent.toast.as_ref().map(|(s, _)| s.as_str()),
        Some("Couldn't generate recap")
    );
}

#[test]
fn minimal_btw_response_after_esc_is_ignored() {
    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().active_pane = crate::app::agent_view::AgentPane::Prompt;
    let request_id = send_minimal_btw(&mut app, "side question");

    let _ = app.handle_input(&esc());
    assert!(app.agents[&id].btw_state.is_none());

    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: id,
            result: Ok("late".into()),
            minimal_request_id: Some(request_id),
        }),
        &mut app,
    );

    assert!(app.agents[&id].btw_state.is_none());
}

#[test]
fn minimal_done_dismisses_to_exactly_one_btw_block() {
    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().active_pane = ActivePane::Prompt;
    let request_id = send_minimal_btw(&mut app, "original question");
    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: id,
            result: Ok("original answer".into()),
            minimal_request_id: Some(request_id),
        }),
        &mut app,
    );

    let _ = app.handle_input(&esc());

    let btw_blocks: Vec<_> = app.agents[&id]
        .scrollback
        .iter_entries()
        .filter_map(|(_, entry)| match &entry.block {
            RenderBlock::Btw(block) => Some(block),
            _ => None,
        })
        .collect();
    assert_eq!(btw_blocks.len(), 1);
    assert_eq!(btw_blocks[0].question, "original question");
    assert_eq!(btw_blocks[0].content().text(), "original answer");
}

#[test]
fn minimal_btw_requests_stay_independent_across_two_agents() {
    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    let first = AgentId(0);
    let second = AgentId(1);
    insert_placeholder_agent(&mut app, second);

    let first_old = send_minimal_btw(&mut app, "first old");
    let first_current = send_minimal_btw(&mut app, "first new");

    switch_to_agent(&mut app, second, SwitchCause::Picker);
    let second_request = send_minimal_btw(&mut app, "second");

    // Deliver the background first-agent responses while the second agent is active.
    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: first,
            result: Ok("stale first answer".into()),
            minimal_request_id: Some(first_old),
        }),
        &mut app,
    );
    assert!(matches!(
        app.agents[&first].btw_state,
        Some(crate::views::btw_overlay::BtwOverlayState::Loading { ref question })
            if question == "first new"
    ));
    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: first,
            result: Ok("current first answer".into()),
            minimal_request_id: Some(first_current),
        }),
        &mut app,
    );
    assert!(matches!(
        app.agents[&first].btw_state,
        Some(crate::views::btw_overlay::BtwOverlayState::Done { ref question, .. })
            if question == "first new"
    ));
    assert!(matches!(
        app.agents[&second].btw_state,
        Some(crate::views::btw_overlay::BtwOverlayState::Loading { ref question })
            if question == "second"
    ));

    // Dismiss the active second request, then its later response must be ignored.
    app.agents.get_mut(&second).unwrap().active_pane = ActivePane::Prompt;
    let _ = app.handle_input(&esc());
    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: second,
            result: Ok("late second answer".into()),
            minimal_request_id: Some(second_request),
        }),
        &mut app,
    );
    assert!(app.agents[&second].btw_state.is_none());
    assert!(app.agents[&second].minimal_btw_lifecycle.is_none());
    assert!(matches!(
        app.agents[&first].btw_state,
        Some(crate::views::btw_overlay::BtwOverlayState::Done { ref question, .. })
            if question == "first new"
    ));

    // Reverse delivery order on fresh requests: active second completes first,
    // then the background first response still resolves only the first panel.
    switch_to_agent(&mut app, first, SwitchCause::Picker);
    let first_request = send_minimal_btw(&mut app, "first reverse");
    switch_to_agent(&mut app, second, SwitchCause::Picker);
    let second_request = send_minimal_btw(&mut app, "second reverse");
    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: second,
            result: Ok("second reverse answer".into()),
            minimal_request_id: Some(second_request),
        }),
        &mut app,
    );
    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: first,
            result: Ok("first reverse answer".into()),
            minimal_request_id: Some(first_request),
        }),
        &mut app,
    );
    assert!(matches!(
        app.agents[&second].btw_state,
        Some(crate::views::btw_overlay::BtwOverlayState::Done { ref question, .. })
            if question == "second reverse"
    ));
    assert!(matches!(
        app.agents[&first].btw_state,
        Some(crate::views::btw_overlay::BtwOverlayState::Done { ref question, .. })
            if question == "first reverse"
    ));
}

#[test]
fn fullscreen_btw_response_after_dismiss_keeps_existing_behavior() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let effects = dispatch(Action::SendBtw("side question".into()), &mut app);
    assert!(matches!(
        effects.as_slice(),
        [Effect::SendBtw {
            minimal_request_id: None,
            ..
        }]
    ));
    app.agents.get_mut(&id).unwrap().btw_state = None;

    dispatch(
        Action::TaskComplete(TaskResult::BtwResponse {
            agent_id: id,
            result: Ok("late".into()),
            minimal_request_id: None,
        }),
        &mut app,
    );

    assert!(matches!(
        app.agents[&id].btw_state,
        Some(crate::views::btw_overlay::BtwOverlayState::Done { ref question, .. })
            if question.is_empty()
    ));
}

#[test]
fn btw_no_session_feedback_is_mode_specific() {
    let id = AgentId(0);

    let mut minimal = test_app_with_agent();
    minimal.screen_mode = crate::app::ScreenMode::Minimal;
    minimal.agents.get_mut(&id).unwrap().session.session_id = None;
    assert!(dispatch(Action::SendBtw("q".into()), &mut minimal).is_empty());
    assert!(minimal.agents[&id].toast.is_none());
    assert!(last_system_text(&minimal, id).contains("No active session"));

    let mut fullscreen = test_app_with_agent();
    fullscreen.agents.get_mut(&id).unwrap().session.session_id = None;
    assert!(dispatch(Action::SendBtw("q".into()), &mut fullscreen).is_empty());
    assert_eq!(
        fullscreen.agents[&id]
            .toast
            .as_ref()
            .map(|(text, _)| text.as_str()),
        Some("No active session")
    );
    assert_eq!(fullscreen.agents[&id].scrollback.len(), 0);
}

/// Bare `/feedback` opens a freeform ask-user-style pane (not prompt chrome).
#[test]
fn enter_feedback_mode_opens_local_question_pane() {
    use crate::app::dispatch::FEEDBACK_QUESTION_LABEL;
    use crate::views::question_view::{LocalQuestionKind, QuestionFocus};

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt_input_mode = crate::app::agent_view::PromptInputMode::Bash;
        agent.prompt.set_text("draft");
    }

    let effects = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    assert!(effects.is_empty(), "pane open is synchronous: {effects:?}");

    let agent = app.agents.get(&id).unwrap();
    let qv = agent
        .question_view
        .as_ref()
        .expect("bare /feedback must open a question pane");
    assert!(
        matches!(qv.local_kind, Some(LocalQuestionKind::Feedback)),
        "local kind should be Feedback, got {:?}",
        qv.local_kind
    );
    assert_eq!(qv.questions.len(), 1);
    assert!(
        qv.questions[0].options.is_empty(),
        "feedback pane is freeform-only"
    );
    assert_eq!(
        qv.questions[0].question, FEEDBACK_QUESTION_LABEL,
        "pane label is the whole question; guidance is the composer placeholder"
    );
    assert_eq!(
        qv.focus,
        QuestionFocus::InputMode,
        "should start ready to type freeform"
    );
    assert_eq!(
        agent.prompt_input_mode,
        crate::app::agent_view::PromptInputMode::Bash,
        "the mode rides with the draft: the stash carries no mode, so clearing it here would return the draft to a plain composer"
    );
}

/// Eligible sessions: Enter on the report advances to the trace-consent
/// question instead of sending; turning trace upload on is preselected.
#[test]
fn feedback_enter_offers_trace_question_when_eligible() {
    use crate::app::app_view::InputOutcome;
    use crate::views::question_view::{FEEDBACK_TRACE_QUESTION_LABEL, QuestionSelection};

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    app.agents.get_mut(&AgentId(0)).unwrap().active_pane =
        crate::app::agent_view::AgentPane::Prompt;
    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    app.agents
        .get_mut(&AgentId(0))
        .unwrap()
        .prompt
        .set_text("the tool crashed on empty input");

    let outcome = app
        .agents
        .get_mut(&AgentId(0))
        .unwrap()
        .submit_question_answers_for_test(false);
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Enter must advance to the trace question, not send yet: {outcome:?}"
    );
    let qv = app.agents[&AgentId(0)]
        .question_view
        .as_ref()
        .expect("trace question stays open");
    assert!(qv.is_feedback_trace());
    assert_eq!(qv.questions[0].question, FEEDBACK_TRACE_QUESTION_LABEL);
    assert_eq!(qv.questions[0].options.len(), 3);
    assert!(
        matches!(qv.selections[0], QuestionSelection::Single(Some(0))),
        "turning trace upload on is the default"
    );
}

/// Ineligible sessions skip the trace question: Enter sends immediately.
#[test]
fn feedback_enter_sends_directly_without_trace_offer() {
    use crate::app::app_view::InputOutcome;

    let mut app = app_with_feedback_pane("the tool crashed on empty input");
    match app
        .agents
        .get_mut(&AgentId(0))
        .unwrap()
        .submit_question_answers_for_test(false)
    {
        InputOutcome::Action(Action::SendFeedback {
            text, trace: None, ..
        }) => {
            assert_eq!(text, "the tool crashed on empty input")
        }
        other => panic!("expected immediate send, got {other:?}"),
    }
    assert!(
        app.agents[&AgentId(0)].question_view.is_none(),
        "pane closes on send"
    );
}

/// Enter on the preselected trace option turns trace upload on: this
/// report's archive uploads and the consent persists.
#[test]
fn feedback_trace_enter_turns_on_trace_upload() {
    use crate::app::actions::FeedbackTraceChoice;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = app_with_feedback_trace_question("clipboard is broken over ssh");
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let outcome =
        agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let InputOutcome::Action(action) = outcome else {
        panic!("Enter on the trace question must send: {outcome:?}");
    };
    match &action {
        Action::SendFeedback {
            text,
            images: _,
            trace: Some(FeedbackTraceChoice::AlwaysUpload),
        } => assert_eq!(text, "clipboard is broken over ssh"),
        other => panic!("expected the turn-on choice, got {other:?}"),
    }
    assert!(agent.question_view.is_none(), "card closes on send");

    let effects = dispatch(action, &mut app);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SendFeedback { .. })),
        "report must post: {effects:?}"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::UploadFeedbackTrace { .. })),
        "this report's archive must upload: {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistSetting {
                key: "trace_upload",
                ..
            }
        )),
        "the consent must persist: {effects:?}"
    );
}

/// "Yes, always upload" uploads now and persists `[telemetry] trace_upload`.
#[test]
fn feedback_trace_always_upload_persists_setting() {
    use crate::app::actions::FeedbackTraceChoice;

    let mut app = test_app_with_agent();
    let effects = dispatch(
        Action::SendFeedback {
            text: "clipboard is broken over ssh".into(),
            images: Default::default(),
            trace: Some(FeedbackTraceChoice::AlwaysUpload),
        },
        &mut app,
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::UploadFeedbackTrace { .. })),
        "always includes this report's archive: {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistSetting {
                key: "trace_upload",
                value: crate::settings::SettingValue::Bool(true),
                ..
            }
        )),
        "always must persist the consent: {effects:?}"
    );
}

/// "Opt out and don't ask again" sends the report alone, latches the offer off
/// for this session, and persists `[features] feedback_trace_card = false`.
#[test]
fn feedback_trace_never_ask_persists_suppression() {
    use crate::app::actions::FeedbackTraceChoice;

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    let effects = dispatch(
        Action::SendFeedback {
            text: "clipboard is broken over ssh".into(),
            images: Default::default(),
            trace: Some(FeedbackTraceChoice::NeverAsk),
        },
        &mut app,
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SendFeedback { .. })),
        "the report still sends: {effects:?}"
    );
    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, Effect::UploadFeedbackTrace { .. })),
        "never-ask must not upload: {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistSetting {
                key: "feedback_trace_card",
                value: crate::settings::SettingValue::Bool(false),
                ..
            }
        )),
        "never-ask must persist the suppression: {effects:?}"
    );
    assert!(!app.feedback_trace_offer(), "offer latches off");
    assert!(
        app.feedback_trace_choice_latched,
        "sticky across auth-meta refreshes"
    );
}

/// ↓↓ walks to "Opt out and don't ask again"; Enter maps to `NeverAsk`.
#[test]
fn feedback_trace_third_option_maps_to_never_ask() {
    use crate::app::actions::FeedbackTraceChoice;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = app_with_feedback_trace_question("clipboard is broken over ssh");
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let outcome =
        agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(
            outcome,
            InputOutcome::Action(Action::SendFeedback {
                trace: Some(FeedbackTraceChoice::NeverAsk),
                ..
            })
        ),
        "third option is don't-ask-again: {outcome:?}"
    );
}

/// ↓ walks to "Opt out this time"; Enter sends the report alone.
#[test]
fn feedback_trace_no_upload_sends_report_alone() {
    use crate::app::actions::FeedbackTraceChoice;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = app_with_feedback_trace_question("clipboard is broken over ssh");
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let outcome =
        agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let InputOutcome::Action(action) = outcome else {
        panic!("Enter on the trace question must send: {outcome:?}");
    };
    assert!(
        matches!(
            &action,
            Action::SendFeedback {
                trace: Some(FeedbackTraceChoice::NoUpload),
                ..
            }
        ),
        "second option is no-upload: {action:?}"
    );

    let effects = dispatch(action, &mut app);
    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, Effect::UploadFeedbackTrace { .. })),
        "no-upload must not archive: {effects:?}"
    );
}

/// Esc on the trace question skips the upload but still sends the report the
/// user already committed with Enter.
#[test]
fn feedback_trace_esc_skips_upload_but_sends() {
    use crate::app::actions::FeedbackTraceChoice;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = app_with_feedback_trace_question("clipboard is broken over ssh");
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    let outcome =
        agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    match outcome {
        InputOutcome::Action(Action::SendFeedback {
            text,
            images: _,
            trace: Some(FeedbackTraceChoice::NoUpload),
        }) => assert_eq!(text, "clipboard is broken over ssh"),
        other => panic!("Esc must skip the trace, not the report: {other:?}"),
    }
    assert!(agent.question_view.is_none(), "card closes on skip-send");
}

#[test]
fn feedback_report_brackets_type_not_tabs() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    app.agents.get_mut(&AgentId(0)).unwrap().active_pane =
        crate::app::agent_view::AgentPane::Prompt;
    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
    assert!(
        agent.prompt.text().contains('['),
        "bracket must type: {:?}",
        agent.prompt.text()
    );
}

/// "Always upload" is a persistent consent: the same session must not offer
/// the trace question again.
#[test]
fn feedback_always_upload_stops_reoffering_this_session() {
    use crate::app::actions::FeedbackTraceChoice;

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    let _ = dispatch(
        Action::SendFeedback {
            text: "clipboard is broken over ssh".into(),
            images: Default::default(),
            trace: Some(FeedbackTraceChoice::AlwaysUpload),
        },
        &mut app,
    );
    assert!(
        !app.feedback_trace_offer(),
        "a persisted consent must clear the session-local offer"
    );

    // A later auth-meta refresh (login, subscription check) recomputes the
    // offer from shell config, which the persisted consent reaches
    // asynchronously — it must not resurrect the question.
    let meta = echo_agent_runtime::auth::AuthMeta {
        feedback_trace_offer: true,
        ..Default::default()
    };
    app.apply_auth_meta(&meta);
    assert!(
        !app.feedback_trace_offer(),
        "auth-meta refresh must not re-offer after 'always upload'"
    );

    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    assert!(
        !app.agents[&AgentId(0)]
            .question_view
            .as_ref()
            .expect("pane")
            .feedback_offer_trace,
        "the next /feedback must send directly"
    );
}

/// An individual coding-data opt-out does NOT suppress the offer — the card
/// is exactly how opted-out users switch trace upload (and sharing) back
/// on. The turn-on option must advertise that it re-enables sharing.
#[test]
fn feedback_opted_out_still_gets_trace_offer() {
    use crate::app::app_view::InputOutcome;

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    app.coding_data_retention_opt_out = true;
    app.agents.get_mut(&AgentId(0)).unwrap().active_pane =
        crate::app::agent_view::AgentPane::Prompt;
    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    assert!(
        agent
            .question_view
            .as_ref()
            .expect("pane")
            .feedback_offer_trace,
        "opted-out users must still be asked (self-serve way back on)"
    );
    agent.prompt.set_text("clipboard is broken over ssh");
    let outcome = agent.submit_question_answers_for_test(false);
    assert!(matches!(outcome, InputOutcome::Changed), "{outcome:?}");
    let qv = agent.question_view.as_ref().expect("trace card");
    assert!(qv.is_feedback_trace());
    assert!(
        qv.questions[0].options[0]
            .description
            .contains("coding data sharing back on"),
        "the turn-on option must disclose the opt-in side effect: {:?}",
        qv.questions[0].options[0].description
    );
}

#[test]
fn feedback_team_admin_never_gets_trace_offer() {
    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    app.team_name = Some("acme".into());
    app.team_role = Some("Admin".into());
    app.agents.get_mut(&AgentId(0)).unwrap().active_pane =
        crate::app::agent_view::AgentPane::Prompt;
    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    assert!(
        !app.agents[&AgentId(0)]
            .question_view
            .as_ref()
            .expect("pane")
            .feedback_offer_trace,
        "a team admin's /feedback must send directly, no consent card"
    );
}

/// Turning trace upload on while opted out is the switch-back-on
/// affordance: it flips coding-data sharing through the standard write
/// path, and this report's upload waits for that write to be confirmed
/// (the storage proxy refuses uploads while the account is opted out).
#[test]
fn feedback_turn_on_while_opted_out_reenables_sharing_then_uploads() {
    use crate::app::actions::FeedbackTraceChoice;

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    app.coding_data_retention_opt_out = true;
    let effects = dispatch(
        Action::SendFeedback {
            text: "clipboard is broken over ssh".into(),
            images: Default::default(),
            trace: Some(FeedbackTraceChoice::AlwaysUpload),
        },
        &mut app,
    );
    assert!(
        !app.coding_data_retention_opt_out,
        "turn-on must flip sharing back on (optimistic)"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SetCodingDataSharing { opted_in: true, .. })),
        "the server-side sharing write must be issued: {effects:?}"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::UploadFeedbackTrace { .. })),
        "the upload must wait for the opt-in to land: {effects:?}"
    );
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::PersistSetting {
                key: "trace_upload",
                ..
            }
        )),
        "the consent must not persist before the opt-in lands: {effects:?}"
    );
    let seq = app
        .feedback_trace_upload_pending
        .as_ref()
        .expect("upload parked on the sharing write")
        .seq;

    // The opt-in confirmation releases the parked upload and the deferred
    // consent persist.
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CodingDataSharingUpdated {
            agent_id: AgentId(0),
            opted_in: true,
            seq,
        }),
        &mut app,
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::UploadFeedbackTrace { .. })),
        "confirmed opt-in must release the parked upload: {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistSetting {
                key: "trace_upload",
                value: crate::settings::SettingValue::Bool(true),
                ..
            }
        )),
        "confirmed opt-in must persist the consent: {effects:?}"
    );
    assert!(
        app.feedback_trace_upload_pending.is_none(),
        "the parked upload is consumed"
    );
}

/// A write that confirms with `opted_in = false` (server kept sharing off)
/// must behave like the failure path: no upload, no persist, latch undone so
/// the card can re-offer.
#[test]
fn feedback_turn_on_unlatches_when_the_confirm_keeps_sharing_off() {
    use crate::app::actions::FeedbackTraceChoice;

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    app.coding_data_retention_opt_out = true;
    let _ = dispatch(
        Action::SendFeedback {
            text: "clipboard is broken over ssh".into(),
            images: Default::default(),
            trace: Some(FeedbackTraceChoice::AlwaysUpload),
        },
        &mut app,
    );
    let seq = app
        .feedback_trace_upload_pending
        .as_ref()
        .expect("upload parked on the sharing write")
        .seq;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::CodingDataSharingUpdated {
            agent_id: AgentId(0),
            opted_in: false,
            seq,
        }),
        &mut app,
    );
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::UploadFeedbackTrace { .. } | Effect::PersistSetting { .. }
        )),
        "a confirm that keeps sharing off must not upload or persist: {effects:?}"
    );
    assert!(
        app.feedback_trace_upload_pending.is_none(),
        "the parked upload is consumed"
    );
    assert!(
        app.feedback_trace_offer() && !app.feedback_trace_choice_latched,
        "the card must be able to re-offer"
    );
}

/// A failed opt-in write drops the parked upload: the storage proxy would
/// still refuse it, and the user already sees the failure toast. The report
/// itself was sent before the upload was parked.
#[test]
fn feedback_turn_on_upload_is_dropped_when_the_opt_in_write_fails() {
    use crate::app::actions::FeedbackTraceChoice;

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    app.coding_data_retention_opt_out = true;
    let send_effects = dispatch(
        Action::SendFeedback {
            text: "clipboard is broken over ssh".into(),
            images: Default::default(),
            trace: Some(FeedbackTraceChoice::AlwaysUpload),
        },
        &mut app,
    );
    let seq = app
        .feedback_trace_upload_pending
        .as_ref()
        .expect("upload parked on the sharing write")
        .seq;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::CodingDataSharingFailed {
            agent_id: AgentId(0),
            error: "boom".into(),
            rollback_to_opted_in: false,
            seq,
        }),
        &mut app,
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::UploadFeedbackTrace { .. })),
        "a failed opt-in must not upload: {effects:?}"
    );
    assert!(
        app.feedback_trace_upload_pending.is_none(),
        "the parked upload is dropped, not leaked"
    );
    assert!(
        app.coding_data_retention_opt_out,
        "the optimistic flip is rolled back"
    );
    // Neither dispatch persisted the consent: it was deferred to the confirm,
    // which never came.
    assert!(
        !send_effects.iter().chain(effects.iter()).any(|e| matches!(
            e,
            Effect::PersistSetting {
                key: "trace_upload",
                ..
            }
        )),
        "a failed opt-in must never persist trace_upload: {send_effects:?} {effects:?}"
    );
    assert!(
        app.feedback_trace_offer() && !app.feedback_trace_choice_latched,
        "the card must be able to re-offer after a failed opt-in"
    );
}

/// An ACP question displacing the trace-consent card must not drop the
/// report the user already committed with Enter: it sends without a trace,
/// like Esc/skip.
#[test]
fn acp_question_displacing_trace_card_still_sends_report() {
    let mut app = app_with_feedback_trace_question("clipboard is broken over ssh");
    let (args, _rx) = make_ask_user_question_args("acp-driven-question");
    assert!(crate::app::acp_handler::handle_ask_user_question(
        args, &mut app
    ));

    assert!(
        app.pending_effects.iter().any(|e| matches!(
            e,
            Effect::SendFeedback { feedback_text, .. }
                if feedback_text == "clipboard is broken over ssh"
        )),
        "displaced trace card must still post the report: {:?}",
        app.pending_effects
    );
    let agent = &app.agents[&AgentId(0)];
    assert_eq!(
        agent
            .question_view
            .as_ref()
            .expect("ACP question is now active")
            .tool_call_id,
        "acp-driven-question"
    );
    assert!(
        last_system_text(&app, AgentId(0)).contains("sent without a trace"),
        "the user must learn what happened to the report"
    );
}

/// Ctrl-Y (and any other `dismiss_question_view` caller) on the trace-consent
/// card must send the committed report without a trace, like Esc/skip —
/// never drop it silently.
#[test]
fn dismissing_the_trace_card_still_sends_the_report() {
    use crate::app::actions::FeedbackTraceChoice;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = app_with_feedback_trace_question("clipboard is broken over ssh");
    let ctrl_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
    let outcome = app
        .agents
        .get_mut(&AgentId(0))
        .unwrap()
        .handle_question_key_for_test(&ctrl_y);
    assert!(
        matches!(
            outcome,
            InputOutcome::Action(Action::SendFeedback {
                ref text,
                trace: Some(FeedbackTraceChoice::NoUpload),
                ..
            }) if text == "clipboard is broken over ssh"
        ),
        "dismissal must post the committed report without a trace: {outcome:?}"
    );
}

/// A fresh install initializes before login, so the connection-time snapshot
/// of the trace offer is `false`; the authenticate meta must refresh it or
/// the first post-login `/feedback` silently skips the consent question.
#[test]
fn auth_meta_refreshes_feedback_trace_offer() {
    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = false; // initialize ran logged-out

    let meta = echo_agent_runtime::auth::AuthMeta {
        feedback_trace_offer: true,
        coding_data_retention_opt_out: false,
        ..Default::default()
    };
    app.apply_auth_meta(&meta);
    assert!(app.feedback_trace_offer(), "login must refresh the offer");

    app.agents.get_mut(&AgentId(0)).unwrap().active_pane =
        crate::app::agent_view::AgentPane::Prompt;
    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    assert!(
        app.agents[&AgentId(0)]
            .question_view
            .as_ref()
            .expect("pane")
            .feedback_offer_trace,
        "post-login /feedback must offer the trace question"
    );
}

/// Builds the trace-consent stage the way the UI reaches it: open, type, Enter.
fn app_with_feedback_trace_question(report: &str) -> crate::app::app_view::AppView {
    use crate::app::app_view::InputOutcome;

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    app.agents.get_mut(&AgentId(0)).unwrap().active_pane =
        crate::app::agent_view::AgentPane::Prompt;
    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.prompt.set_text(report);
    let outcome = agent.submit_question_answers_for_test(false);
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Enter should open the trace question: {outcome:?}"
    );
    assert!(
        agent
            .question_view
            .as_ref()
            .is_some_and(|qv| qv.is_feedback_trace()),
        "trace question must be up"
    );
    app
}

/// Full-TUI `/feedback {text}` opens the same pane with the text already in the box.
#[test]
fn open_feedback_pane_prefills_inline_text() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.set_text("unrelated draft");
    }

    let effects = dispatch(
        Action::OpenFeedbackPane {
            prefill: Some("the tool crashed on empty input".into()),
            images: Default::default(),
        },
        &mut app,
    );
    assert!(
        effects.is_empty(),
        "opening the pane needs no effects: {effects:?}"
    );

    let agent = app.agents.get(&id).unwrap();
    let qv = agent
        .question_view
        .as_ref()
        .expect("prefilled /feedback must open a question pane");
    assert_eq!(
        qv.feedback_report(),
        "the tool crashed on empty input",
        "prefill must land in the report box"
    );
    assert_eq!(
        agent.prompt.text(),
        "the tool crashed on empty input",
        "composer shows the prefill while the card has input focus"
    );
}

/// No session: bare `/feedback` shows a notice instead of opening the pane.
#[test]
fn enter_feedback_mode_requires_session() {
    let id = AgentId(0);

    let mut fullscreen = test_app_with_agent();
    fullscreen.agents.get_mut(&id).unwrap().session.session_id = None;
    assert!(
        dispatch(
            Action::OpenFeedbackPane {
                prefill: None,
                images: Default::default()
            },
            &mut fullscreen
        )
        .is_empty()
    );
    let agent = fullscreen.agents.get(&id).unwrap();
    assert!(agent.question_view.is_none());
    assert_eq!(
        agent.toast.as_ref().map(|(t, _)| t.as_str()),
        Some("No active session")
    );
    assert_eq!(agent.scrollback.len(), 0);

    // Minimal mode: toast is invisible, so use a system block instead.
    let mut minimal = test_app_with_agent();
    minimal.screen_mode = crate::app::ScreenMode::Minimal;
    minimal.agents.get_mut(&id).unwrap().session.session_id = None;
    assert!(
        dispatch(
            Action::OpenFeedbackPane {
                prefill: None,
                images: Default::default()
            },
            &mut minimal
        )
        .is_empty()
    );
    let agent = minimal.agents.get(&id).unwrap();
    assert!(agent.question_view.is_none());
    assert!(agent.toast.is_none(), "minimal must not rely on toast");
    assert!(
        last_system_text(&minimal, id).contains("No active session"),
        "minimal must show a system notice"
    );
}

/// Busy question slot: minimal mode uses a system notice, not a toast.
#[test]
fn enter_feedback_mode_busy_question_is_mode_specific() {
    use crate::views::prompt_widget::StashedPrompt;
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::Question;

    let id = AgentId(0);
    let occupy = |agent: &mut crate::app::agent_view::AgentView| {
        let q = Question {
            question: "busy?".into(),
            options: vec![],
            multi_select: Some(false),
            id: None,
        };
        agent.question_view = Some(
            QuestionViewState::new("busy".into(), vec![q], StashedPrompt::default())
                .with_local_kind(LocalQuestionKind::Feedback),
        );
    };

    let mut fullscreen = test_app_with_agent();
    occupy(fullscreen.agents.get_mut(&id).unwrap());
    assert!(
        dispatch(
            Action::OpenFeedbackPane {
                prefill: None,
                images: Default::default()
            },
            &mut fullscreen
        )
        .is_empty()
    );
    assert_eq!(
        fullscreen.agents[&id]
            .toast
            .as_ref()
            .map(|(t, _)| t.as_str()),
        Some("Finish answering the current question first")
    );

    let mut minimal = test_app_with_agent();
    minimal.screen_mode = crate::app::ScreenMode::Minimal;
    occupy(minimal.agents.get_mut(&id).unwrap());
    assert!(
        dispatch(
            Action::OpenFeedbackPane {
                prefill: None,
                images: Default::default()
            },
            &mut minimal
        )
        .is_empty()
    );
    assert!(minimal.agents[&id].toast.is_none());
    assert!(last_system_text(&minimal, id).contains("Finish answering the current question first"));
}

/// Casual commenting parks its draft and keeps the composer live, the opposite of a permission, so closing a card over it restores into
/// the composer and leaves the parked draft alone.
#[test]
fn casual_commenting_keeps_its_parked_draft_when_a_card_closes() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let id = AgentId(0);
    let mut app = test_app_with_agent();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        agent.prompt.set_text("pre-comment draft");
        agent.casual_stashed_prompt = Some(agent.prompt.stash());
        agent.prompt.set_text("the casual comment");
    }

    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    let agent = app.agents.get_mut(&id).unwrap();
    assert!(
        agent.question_view.is_some(),
        "a parked casual comment does not block the pane"
    );
    agent.prompt.set_text("a report");
    agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));

    assert_eq!(
        agent.prompt.text(),
        "the casual comment",
        "the live comment comes back to the composer"
    );
    assert_eq!(
        agent
            .casual_stashed_prompt
            .as_ref()
            .map(|s| s.text.as_str()),
        Some("pre-comment draft"),
        "the parked pre-comment draft must survive"
    );
}

/// A line viewer outranks every card for keys, so opening the pane under one would leave a box the user cannot type into.
#[test]
fn enter_feedback_mode_refuses_under_a_line_viewer() {
    let id = AgentId(0);
    let mut app = test_app_with_agent();
    let path = std::env::temp_dir().join("feedback_guard_line_viewer.txt");
    std::fs::write(&path, "a preview line\n").unwrap();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        agent.open_line_viewer(&path, None);
        assert!(agent.line_viewer.is_some(), "the preview is open");
    }

    assert!(
        dispatch(
            Action::OpenFeedbackPane {
                prefill: None,
                images: Default::default()
            },
            &mut app
        )
        .is_empty()
    );

    assert!(
        app.agents[&id].question_view.is_none(),
        "the pane must not open under a viewer that owns the keyboard"
    );
    let _ = std::fs::remove_file(&path);
}

/// A plan approval owns the composer and outranks the pane for keys, so the pane must refuse rather than open unreachable.
#[test]
fn enter_feedback_mode_refuses_under_a_plan_approval() {
    let id = AgentId(0);
    let mut app = test_app_with_agent();
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.set_text("draft the plan stashed");
        agent.plan_approval_view =
            Some(crate::app::agent_view::test_fixtures::make_plan_approval_view_state());
    }

    assert!(
        dispatch(
            Action::OpenFeedbackPane {
                prefill: None,
                images: Default::default()
            },
            &mut app
        )
        .is_empty()
    );

    let agent = &app.agents[&id];
    assert!(
        agent.question_view.is_none(),
        "the pane must not open under a plan approval"
    );
    assert_eq!(
        agent.toast.as_ref().map(|(t, _)| t.as_str()),
        Some("Close or answer what's open before sending feedback")
    );
    assert_eq!(
        agent.prompt.text(),
        "draft the plan stashed",
        "refusing must not stash or blank the composer"
    );
}

/// A failed send surfaces the error and leaves the composer alone. The shell persisted the report locally before the POST, so nothing is lost here.
#[test]
fn feedback_failed_reports_the_error_and_spares_the_composer() {
    let id = AgentId(0);
    let mut app = test_app_with_agent();
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .set_text("unrelated draft");

    let _ = dispatch(
        Action::TaskComplete(crate::app::actions::TaskResult::FeedbackFailed {
            agent_id: id,
            error: "disabled".into(),
        }),
        &mut app,
    );

    assert!(last_system_text(&app, id).contains("Couldn't send feedback"));
    assert_eq!(
        app.agents[&id].prompt.text(),
        "unrelated draft",
        "a failed report must not land in the composer, which sends to the model"
    );
}

/// Inline `/feedback <text>` with no session has nowhere to send, so it says so instead of failing silently.
#[test]
fn send_feedback_without_a_session_says_so() {
    let id = AgentId(0);
    let mut app = test_app_with_agent();
    app.agents.get_mut(&id).unwrap().session.session_id = None;

    assert!(
        dispatch(
            Action::SendFeedback {
                text: "long report".into(),
                images: Default::default(),
                trace: Some(crate::app::actions::FeedbackTraceChoice::NoUpload),
            },
            &mut app
        )
        .is_empty()
    );

    assert!(last_system_text(&app, id).contains("No active session"));
}

fn app_with_feedback_pane(report: &str) -> crate::app::app_view::AppView {
    let mut app = test_app_with_agent();
    // Typing a slash command means the keyboard is on the prompt, and a card parked in the scrollback owns no keys.
    app.agents.get_mut(&AgentId(0)).unwrap().active_pane =
        crate::app::agent_view::AgentPane::Prompt;
    let effects = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    assert!(effects.is_empty(), "pane open is synchronous: {effects:?}");
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.prompt.set_text(report);
    app
}

/// Enter on the feedback pane sends the report through the production submit path and closes the pane. Empty Enter holds the pane open instead.
#[test]
fn feedback_pane_enter_sends_report() {
    use crate::app::app_view::InputOutcome;

    let mut app = app_with_feedback_pane("  the tool crashed on empty input  ");
    let outcome = app
        .agents
        .get_mut(&AgentId(0))
        .unwrap()
        .submit_question_answers_for_test(false);
    match outcome {
        InputOutcome::Action(Action::SendFeedback {
            text,
            images,
            trace: None,
        }) => {
            assert_eq!(text, "the tool crashed on empty input");
            assert!(images.is_empty(), "no images were pasted into the pane");
        }
        other => panic!("expected SendFeedback action, got {other:?}"),
    }
    let agent = &app.agents[&AgentId(0)];
    assert!(agent.question_view.is_none(), "pane must close on send");
    assert_eq!(agent.prompt.text(), "", "composer returns to the stash");

    let mut empty = app_with_feedback_pane("   ");
    let outcome = empty
        .agents
        .get_mut(&AgentId(0))
        .unwrap()
        .submit_question_answers_for_test(false);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        empty.agents[&AgentId(0)].question_view.is_some(),
        "blank Enter must keep the pane open"
    );
}

/// Screenshot-only feedback must submit, not hit the empty-report hold-open.
#[test]
fn feedback_pane_enter_sends_image_only_report() {
    use crate::app::app_view::InputOutcome;

    let mut app = app_with_feedback_pane("");
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent
        .prompt
        .insert_image(test_pasted_png())
        .expect("chip inserts");
    assert!(agent.prompt.text().contains("[Image #1]"));

    let outcome = agent.submit_question_answers_for_test(false);
    match outcome {
        InputOutcome::Action(Action::SendFeedback {
            text,
            images,
            trace: None,
        }) => {
            assert_eq!(text, "[Image #1]");
            assert_eq!(images.len(), 1, "the pasted image travels with the report");
        }
        other => panic!("expected SendFeedback action, got {other:?}"),
    }
    assert!(
        app.agents[&AgentId(0)].question_view.is_none(),
        "pane must close on an image-only send"
    );
}

/// Paste-then-immediate-Enter must not drop the screenshot.
#[test]
fn feedback_pane_enter_during_paste_probe_defers_then_reissues_with_image() {
    use crate::app::agent_view::AgentDeferredSend;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = app_with_feedback_pane("screenshot incoming");
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.paste_probe_in_flight = 1;

    let outcome =
        agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        outcome,
        crate::app::app_view::InputOutcome::Changed
    ));
    assert!(agent.question_view.is_some(), "pane holds for the probe");
    assert_eq!(agent.deferred_send, Some(AgentDeferredSend::SubmitFeedback));

    agent.paste_probe_in_flight = 0;
    agent
        .prompt
        .insert_image(test_pasted_png())
        .expect("chip inserts");
    let kind = agent
        .take_deferred_send_after_paste()
        .expect("stash drains once probes settle");
    match agent.resume_deferred_send(kind) {
        Some(Action::SendFeedback {
            text,
            images,
            trace: None,
        }) => {
            assert!(text.contains("screenshot incoming"));
            assert!(text.contains("[Image #1]"));
            assert_eq!(images.len(), 1, "the probed image travels with it");
        }
        other => panic!("expected SendFeedback reissue, got {other:?}"),
    }
    assert!(agent.question_view.is_none(), "reissue closes the pane");

    // Dismissing the pane during the probe window clears the stash so it
    // cannot fire on a later pane; a manually built reissue is also a no-op.
    let mut gone = app_with_feedback_pane("dismissed");
    let agent = gone.agents.get_mut(&AgentId(0)).unwrap();
    agent.deferred_send = Some(AgentDeferredSend::SubmitFeedback);
    agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(agent.question_view.is_none(), "Esc dismisses the pane");
    assert_eq!(agent.deferred_send, None, "dismissal drops the stash");
    assert!(
        agent
            .resume_deferred_send(AgentDeferredSend::SubmitFeedback)
            .is_none()
    );
}

fn test_pasted_png() -> crate::prompt_images::PastedImage {
    crate::prompt_images::from_clipboard_data(&crate::clipboard::ImageData {
        data: vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        mime_type: "image/png".to_string(),
    })
}

/// Full TUI inline `/feedback <text>` composed alongside a pasted image:
/// the chip survives the composer wipe, shows up live in the prefilled
/// pane, and travels with the submitted report.
#[test]
fn inline_feedback_carries_composer_images_into_the_pane() {
    use crate::app::app_view::InputOutcome;

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.active_pane = crate::app::agent_view::AgentPane::Prompt;
        agent.prompt.set_text("/feedback broken thing ");
        let end = agent.prompt.text().len();
        agent.prompt.set_cursor(end);
        agent.prompt.insert_image(test_pasted_png()).expect("chip");
    }
    let composed = app.agents[&id].prompt.text().to_string();
    assert!(composed.contains("[Image #1]"));

    let _ = dispatch(Action::SendPrompt(composed), &mut app);

    let agent = app.agents.get_mut(&id).unwrap();
    assert!(
        agent
            .question_view
            .as_ref()
            .is_some_and(|qv| qv.is_feedback_report()),
        "inline /feedback must open the prefilled pane"
    );
    assert_eq!(
        agent.prompt.images.len(),
        1,
        "the composer image must be adopted as a live chip"
    );
    assert!(agent.prompt.text().contains("[Image #1]"));

    match agent.submit_question_answers_for_test(false) {
        InputOutcome::Action(Action::SendFeedback {
            text,
            images,
            trace: None,
        }) => {
            assert_eq!(text, "broken thing [Image #1]");
            assert_eq!(images.len(), 1, "the adopted image travels on submit");
        }
        other => panic!("expected SendFeedback action, got {other:?}"),
    }
}

/// Minimal-mode inline `/feedback <text>` submits immediately; a composer
/// image must ride the submission instead of dying with the composer wipe.
#[test]
fn minimal_inline_feedback_sends_composer_images() {
    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.set_text("/feedback broken thing ");
        let end = agent.prompt.text().len();
        agent.prompt.set_cursor(end);
        agent.prompt.insert_image(test_pasted_png()).expect("chip");
    }
    let composed = app.agents[&id].prompt.text().to_string();

    let effects = dispatch(Action::SendPrompt(composed), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::SendFeedback {
                feedback_text,
                images,
                ..
            }] if feedback_text == "broken thing [Image #1]" && images.len() == 1
        ),
        "expected an immediate send carrying the image, got {effects:?}"
    );
    assert_eq!(
        app.agents[&id].prompt.text(),
        "",
        "the composer is consumed by the inline command"
    );
}

/// The trace-consent stage must not lose the report's attachments: an
/// image pasted into the pane still travels after the consent answer.
#[test]
fn feedback_trace_stage_carries_report_images() {
    use crate::app::actions::FeedbackTraceChoice;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    app.agents.get_mut(&AgentId(0)).unwrap().active_pane =
        crate::app::agent_view::AgentPane::Prompt;
    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.prompt.set_text("screenshot attached ");
    let end = agent.prompt.text().len();
    agent.prompt.set_cursor(end);
    agent.prompt.insert_image(test_pasted_png()).expect("chip");

    let outcome = agent.submit_question_answers_for_test(false);
    assert!(
        matches!(outcome, InputOutcome::Changed),
        "Enter should advance to the trace question: {outcome:?}"
    );
    assert!(
        agent
            .question_view
            .as_ref()
            .is_some_and(|qv| qv.is_feedback_trace()),
        "trace question must be up"
    );

    // Esc skips the consent question; the committed report (image included)
    // still sends.
    let outcome =
        agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    match outcome {
        InputOutcome::Action(Action::SendFeedback {
            text,
            images,
            trace: Some(FeedbackTraceChoice::NoUpload),
        }) => {
            assert_eq!(text, "screenshot attached [Image #1]");
            assert_eq!(images.len(), 1, "the attachment survives the consent stage");
        }
        other => panic!("Esc must send the committed report: {other:?}"),
    }
}

/// A trace-consent card dropped with its view (session close, agent
/// teardown) owns its attachments' staged temp files and must delete them.
#[test]
fn feedback_trace_card_dropped_with_view_cleans_staged_temp_files() {
    use crate::app::app_view::InputOutcome;

    let mut app = test_app_with_agent();
    app.shell_feedback_trace_offer = true;
    app.agents.get_mut(&AgentId(0)).unwrap().active_pane =
        crate::app::agent_view::AgentPane::Prompt;
    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );

    let dir = tempfile::tempdir().unwrap();
    let staged = dir.path().join("staged.png");
    std::fs::write(&staged, b"staged").unwrap();
    let mut image = test_pasted_png();
    image.staged_temp_path = Some(staged.clone());

    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.prompt.set_text("screenshot attached ");
    let end = agent.prompt.text().len();
    agent.prompt.set_cursor(end);
    agent.prompt.insert_image(image).expect("chip");

    // Enter commits the report into the trace card; the image now lives
    // inside `LocalQuestionKind::FeedbackTrace`.
    let outcome = agent.submit_question_answers_for_test(false);
    assert!(matches!(outcome, InputOutcome::Changed));
    assert!(
        agent
            .question_view
            .as_ref()
            .is_some_and(|qv| qv.is_feedback_trace()),
        "trace question must be up"
    );

    // Tear the view down without answering, as a session close would.
    agent.question_view = None;
    assert!(
        !staged.exists(),
        "a dropped consent card must release its staged files"
    );
}

/// `dispatch_send_feedback` bailing before the send (no agent view) still
/// owns the attachments and must delete their staged temp files.
#[test]
fn send_feedback_without_agent_view_cleans_staged_temp_files() {
    let mut app = test_app_with_agent();
    app.active_view = crate::app::app_view::ActiveView::AgentDashboard;

    let dir = tempfile::tempdir().unwrap();
    let staged = dir.path().join("staged.png");
    std::fs::write(&staged, b"staged").unwrap();
    let mut image = test_pasted_png();
    image.staged_temp_path = Some(staged.clone());

    let effects = dispatch(
        Action::SendFeedback {
            text: "it broke".into(),
            images: vec![image].into(),
            trace: None,
        },
        &mut app,
    );
    assert!(effects.is_empty(), "the send must bail: {effects:?}");
    assert!(
        !staged.exists(),
        "a bailed send must release its staged files"
    );
}

/// Driven through the key handler: the pane keeps input focus, so an Esc falling through to the shared commit path leaves the user stuck in the box.
#[test]
fn feedback_pane_esc_key_dismisses_and_drops_the_report() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = app_with_feedback_pane("half-written report");
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.handle_question_key_for_test(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(agent.question_view.is_none(), "Esc must close the pane");
    assert_eq!(
        agent.prompt.text(),
        "",
        "the report stays out of the composer, which sends to the model"
    );
}

/// Dismissing gives the pre-slash draft back untouched, and the report goes nowhere near it.
#[test]
fn feedback_pane_dismiss_leaves_the_composer_alone() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .set_text("draft from before");
    dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .set_text("the report");
    app.agents
        .get_mut(&id)
        .unwrap()
        .submit_question_answers_for_test(true);

    assert_eq!(
        app.agents[&id].prompt.text(),
        "draft from before",
        "the pre-slash draft comes back untouched"
    );

    dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );
    assert_eq!(
        app.agents[&id]
            .question_view
            .as_ref()
            .unwrap()
            .feedback_report(),
        "",
        "reopening starts empty"
    );
}

/// An ACP question displacing the pane drops the report, and must not push it into the composer on the way out.
#[test]
fn acp_question_displacing_feedback_pane_drops_the_report() {
    let mut app = app_with_feedback_pane("report in progress");
    let id = AgentId(0);

    let (args, _rx) = make_ask_user_question_args("acp-driven-question");
    assert!(crate::app::acp_handler::handle_ask_user_question(
        args, &mut app
    ));

    let agent = &app.agents[&id];
    assert_eq!(
        agent
            .question_view
            .as_ref()
            .expect("ACP question is now active")
            .tool_call_id,
        "acp-driven-question"
    );
    assert_eq!(
        agent.prompt.text(),
        "",
        "the displaced report must not land in the composer, which sends to the model"
    );
}

/// Ctrl+C on the feedback pane follows the composer: clear the report, then dismiss once the box is empty. It never parks the pane in navigation.
#[test]
fn feedback_pane_ctrl_c_clears_then_dismisses() {
    use crate::views::question_view::QuestionFocus;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let mut app = app_with_feedback_pane("typed before ctrl-c");
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();

    let outcome = agent.handle_question_key_for_test(&ctrl_c);
    let qv = agent.question_view.as_ref().expect("pane stays open");
    assert_eq!(qv.focus, QuestionFocus::InputMode);
    assert_eq!(qv.feedback_report(), "", "first Ctrl+C clears the report");
    assert_eq!(agent.prompt.text(), "");
    assert!(
        matches!(outcome, crate::app::app_view::InputOutcome::Changed),
        "clearing is a redraw, not a send: {outcome:?}"
    );

    agent.handle_question_key_for_test(&ctrl_c);
    assert!(
        agent.question_view.is_none(),
        "Ctrl+C on an empty box dismisses the pane"
    );
}

/// Clicking outside the box keeps the report box up. There is no question card to fall back to.
#[test]
fn feedback_pane_click_outside_keeps_input_focus() {
    use crate::views::question_view::QuestionFocus;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    let mut app = app_with_feedback_pane("mid-report");
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    agent.handle_question_mouse_for_test(&MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });

    let qv = agent.question_view.as_ref().expect("pane stays open");
    assert_eq!(qv.focus, QuestionFocus::InputMode);
    assert_eq!(qv.feedback_report(), "mid-report");
}

/// A permission blanks the composer and holds its text, so closing the pane has to hand the draft to that stash. Otherwise the permission
/// restores the report into the composer later, which is the one place it must never reach.
#[test]
fn permission_holding_the_composer_gets_the_draft_back_not_the_report() {
    let id = AgentId(0);
    let mut app = test_app_with_agent();
    app.agents.get_mut(&id).unwrap().active_pane = crate::app::agent_view::AgentPane::Prompt;
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .set_text("pre-slash draft");
    let _ = dispatch(
        Action::OpenFeedbackPane {
            prefill: None,
            images: Default::default(),
        },
        &mut app,
    );

    let agent = app.agents.get_mut(&id).unwrap();
    agent.prompt.set_text("report the permission interrupted");
    // What a permission enqueue does: take the composer's text and blank it.
    agent.permission_stashed_prompt = Some(agent.prompt.stash());
    agent.prompt.set_text("");

    agent.handle_question_key_for_test(&crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('y'),
        crossterm::event::KeyModifiers::CONTROL,
    ));

    assert!(agent.question_view.is_none(), "Ctrl+Y closes the pane");
    assert_eq!(
        agent
            .permission_stashed_prompt
            .as_ref()
            .map(|s| s.text.as_str()),
        Some("pre-slash draft"),
        "the permission must hand back the draft, not the report"
    );
}

/// Pane submit must not wipe a stashed pre-`/feedback` draft.
#[test]
fn send_feedback_preserves_composer_draft() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.prompt.set_text("keep this draft");
    }

    let effects = dispatch(
        Action::SendFeedback {
            text: "report".into(),
            images: Default::default(),
            trace: Some(crate::app::actions::FeedbackTraceChoice::NoUpload),
        },
        &mut app,
    );
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::SendFeedback {
                feedback_text,
                ..
            }] if feedback_text == "report"
        ),
        "expected SendFeedback effect, got {effects:?}"
    );
    assert_eq!(
        app.agents.get(&id).unwrap().prompt.text(),
        "keep this draft",
        "composer draft must survive SendFeedback"
    );
}
