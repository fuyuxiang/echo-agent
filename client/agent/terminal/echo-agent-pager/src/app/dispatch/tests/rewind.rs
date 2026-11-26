//! Tests for conversation rewind dispatchers and prompt-entry lookup.

use super::*;

#[test]
fn cancel_does_not_rewind_when_in_flight_block_committed() {
    // Minimal-mode regression: a user-prompt block commits to native
    // scrollback immediately (it is never `is_running`), and a committed
    // block can't be "un-printed". Cancelling must NOT rewind such a block —
    // doing so would `remove_entry` it from state while the printed copy
    // stays on screen AND restore the text into the input, showing the prompt
    // twice (dogfood bug: double-Esc on a just-promoted queued prompt).
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("queued prompt".into()), &mut app);
    assert!(app.agents[&id].session.in_flight_prompt.is_some());
    assert_eq!(app.agents[&id].scrollback.len(), 1);

    // Simulate minimal's commit pass printing the user block into native
    // scrollback (sets the entry's `committed` flag).
    let entry_id = app.agents[&id]
        .session
        .in_flight_prompt
        .as_ref()
        .unwrap()
        .scrollback_entry;
    let idx = app.agents[&id].scrollback.index_of_id(entry_id).unwrap();
    app.agents
        .get_mut(&id)
        .unwrap()
        .scrollback
        .mark_committed(idx);

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::CancelTurn { .. }));

    // Standard cancel, NOT the rewind: the prompt is not restored to the
    // input and the committed block stays in scrollback (no duplicate).
    assert!(
        app.agents[&id].prompt.text().is_empty(),
        "committed in-flight block must not be rewound into the input"
    );
    assert_eq!(
        app.agents[&id].scrollback.len(),
        1,
        "committed block must stay in scrollback (it's already printed)"
    );
    assert!(app.agents[&id].session.state.is_cancelling());
}

#[test]
fn rewind_then_resubmit_drains_immediately_and_discards_orphan() {
    // After a rewind, state is Idle so a follow-up prompt can drain
    // without waiting for the cancelled turn's PromptResponse.
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("first".into()), &mut app);
    let first_pid = app.agents[&id].session.current_prompt_id.clone();
    assert!(first_pid.is_some());
    dispatch(Action::CancelTurn, &mut app);
    assert!(app.agents[&id].session.state.is_idle());
    assert!(app.agents[&id].session.current_prompt_id.is_none());

    // User edits and re-submits without waiting.
    let effects = dispatch(Action::SendPrompt("second".into()), &mut app);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::SendPrompt { text, .. } if text == "second"));
    assert!(app.agents[&id].session.state.is_turn_running());
    let second_pid = app.agents[&id].session.current_prompt_id.clone();
    assert!(second_pid.is_some());
    assert_ne!(first_pid, second_pid);

    // The cancelled "first" PromptResponse arrives mid-second-turn,
    // carrying first_pid. Mismatch with current_prompt_id (second_pid)
    // → discarded. State for "second" is untouched.
    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Ok(acp::PromptResponse::new(acp::StopReason::Cancelled).meta(
                serde_json::json!({ "promptId": first_pid })
                    .as_object()
                    .cloned(),
            )),
            http_status: None,
            prompt_id: None,
        }),
        &mut app,
    );
    assert!(app.agents[&id].session.state.is_turn_running());
    assert_eq!(app.agents[&id].session.current_prompt_id, second_pid);
}

/// Ctrl+C rewind cancel carries the rewound turn's prompt id.
#[test]
fn cancel_rewind_effect_carries_the_rewound_prompt_id() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("rewind me".into()), &mut app);
    let pid = app.agents[&id]
        .session
        .current_prompt_id
        .clone()
        .expect("turn running");

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                rewind_prompt_id: Some(p),
                ..
            }] if *p == pid
        ),
        "rewind cancel must carry the captured prompt id, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(agent.session.state.is_idle());
    assert_eq!(agent.prompt.text(), "rewind me");
    assert!(agent.is_rewound_prompt(&pid));
}

/// No prompt id → no optimistic rewind; send a standard cancel.
#[test]
fn cancel_without_prompt_id_skips_rewind_and_sends_normal_cancel() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("cannot rewind".into()), &mut app);
    assert!(app.agents[&id].session.in_flight_prompt.is_some());
    // Simulate the id being gone while the stash survives.
    app.agents.get_mut(&id).unwrap().session.current_prompt_id = None;

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                rewind_prompt_id: None,
                ..
            }]
        ),
        "id-less cancel must not request a rewind, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(
        agent.prompt.text().is_empty(),
        "no optimistic composer restore without an id"
    );
    assert_eq!(
        agent.scrollback.len(),
        1,
        "the prompt block stays in scrollback (standard cancel)"
    );
    assert!(agent.session.state.is_cancelling());
}

/// Set up an app whose agent has one user prompt + one agent message in
/// the transcript, is inline-editing that prompt with `edited` typed in,
/// and has an unrelated draft sitting in the composer.
fn app_mid_inline_edit(edited: &str) -> AppView {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::user_prompt("fix the bug"));
    agent
        .scrollback
        .push_block(RenderBlock::agent_message("done"));
    agent.scrollback.prepare_layout(80, 40);
    assert!(agent.enter_inline_edit(0));
    agent
        .inline_edit
        .as_mut()
        .unwrap()
        .textarea
        .set_text(edited);
    agent.prompt.set_text("composer draft");
    app
}

/// Rewind point for the fixture's single prompt.
fn rewind_point(prompt_index: usize) -> crate::views::rewind::RewindPointInfo {
    crate::views::rewind::RewindPointInfo {
        prompt_index,
        created_at: String::new(),
        num_file_snapshots: 0,
        prompt_preview: Some("fix the bug".into()),
        has_file_changes: false,
    }
}

/// Points-loaded task result carrying the fixture's single rewind point.
fn points_loaded(id: AgentId) -> Action {
    Action::TaskComplete(TaskResult::RewindPointsLoaded {
        agent_id: id,
        points: vec![rewind_point(0)],
    })
}

/// Successful rewind/execute response (conversation-only).
fn rewind_success(target: usize, prompt_text: &str) -> crate::views::rewind::RewindResponse {
    crate::views::rewind::RewindResponse {
        success: true,
        target_prompt_index: target,
        reverted_files: vec![],
        clean_files: vec![],
        conflicts: vec![],
        error: None,
        mode: Some("conversation_only".into()),
        prompt_text: Some(prompt_text.into()),
    }
}

/// Drive an idle inline-edit submit through to execute: points fetch →
/// confirm (setting on) → Executing. Returns the effects of the confirm step.
fn drive_inline_submit_to_execute(app: &mut AppView) -> Vec<Effect> {
    let id = AgentId(0);
    let effects = dispatch(Action::InlineEditSubmit, app);
    assert!(
        matches!(&effects[0], Effect::FetchRewindPoints { .. }),
        "got {effects:?}"
    );
    dispatch(points_loaded(id), app);
    // Confirm-before-rewind (default on) gates every target, including 0.
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm { .. }
    ));
    dispatch(Action::RewindConfirm(0), app)
}

/// Submitting an inline edit enters the exact same flow as `/rewind`: a
/// Loading overlay + a points fetch pre-targeted at the edited prompt. The
/// editor stays open behind the flow and nothing is stashed yet.
#[test]
fn inline_edit_submit_enters_rewind_flow_via_points_fetch() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);

    let effects = dispatch(Action::InlineEditSubmit, &mut app);

    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::FetchRewindPoints { .. }));
    let agent = &app.agents[&id];
    let state = agent.rewind_state.as_ref().expect("rewind flow entered");
    assert!(matches!(
        state.phase,
        crate::views::rewind::RewindPhase::Loading
    ));
    assert_eq!(
        state.selected_prompt_index,
        Some(0),
        "pre-targeted at the edited prompt"
    );
    assert!(agent.inline_edit.is_some(), "editor stays open");
    assert!(
        agent.pending_inline_resubmit.is_none(),
        "nothing stashed before an execute"
    );
}

/// A submit whose text is unchanged (or empty) has nothing to do: the
/// editor just closes; no rewind flow, no effects.
#[test]
fn inline_edit_submit_with_unchanged_text_closes_editor() {
    let mut app = app_mid_inline_edit("fix the bug");
    let id = AgentId(0);

    let effects = dispatch(Action::InlineEditSubmit, &mut app);

    assert!(effects.is_empty());
    let agent = &app.agents[&id];
    assert!(agent.inline_edit.is_none(), "editor closed");
    assert!(agent.rewind_state.is_none(), "no rewind flow entered");
    assert!(agent.scrollback.inline_edit_height().is_none());
}

/// Points loaded with a pre-selected target skip the picker and open confirm
/// when the setting is on; the editor stays open behind it.
#[test]
fn inline_edit_points_loaded_opens_target_zero_confirm_over_open_editor() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    dispatch(Action::InlineEditSubmit, &mut app);

    let effects = dispatch(points_loaded(id), &mut app);
    assert!(
        effects.is_empty(),
        "confirm setting on waits for Yes/No, got {effects:?}"
    );

    let agent = &app.agents[&id];
    assert!(matches!(
        agent.rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm {
            target_prompt_index: 0,
            ..
        }
    ));
    assert!(agent.inline_edit.is_some(), "editor still open");
    assert!(agent.pending_inline_resubmit.is_none());
}

/// Classic `/rewind` with a selected turn also lands on the confirm
/// when confirm-before-rewind is on (default).
#[test]
fn classic_rewind_target_zero_opens_confirm() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("fix the bug"));
        agent
            .scrollback
            .push_block(RenderBlock::agent_message("done"));
        agent.scrollback.prepare_layout(80, 40);
        agent.scrollback.set_selected(Some(0));
    }

    let effects = dispatch(Action::Rewind, &mut app);
    assert!(
        matches!(&effects[0], Effect::FetchRewindPoints { .. }),
        "got {effects:?}"
    );
    let effects = dispatch(points_loaded(id), &mut app);
    assert!(
        effects.is_empty(),
        "confirm setting on waits for Yes/No, got {effects:?}"
    );

    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm {
            target_prompt_index: 0,
            ..
        }
    ));
}

/// Settings action updates the live confirm-before-rewind value.
#[test]
fn set_confirm_before_rewind_updates_live_value() {
    let mut app = test_app_with_agent();
    assert!(app.current_ui.confirm_before_rewind_enabled());

    let effects = dispatch(Action::SetConfirmBeforeRewind(false), &mut app);
    assert!(
        matches!(
            &effects[0],
            Effect::PersistSetting {
                key: "confirm_before_rewind",
                value: crate::settings::SettingValue::Bool(false),
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(!app.current_ui.confirm_before_rewind_enabled());
    assert_eq!(app.current_ui.confirm_before_rewind, Some(false));

    let effects = dispatch(Action::SetConfirmBeforeRewind(false), &mut app);
    assert!(
        effects.is_empty(),
        "idempotent when already false, got {effects:?}"
    );
}

/// Multi-turn fixture with two user prompts for picker / non-zero target tests.
fn app_with_two_turns() -> AppView {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("sess".to_string()));
        for i in 0..2 {
            let mut b = UserPromptBlock::new(format!("turn {i}"));
            b.prompt_index = Some(i);
            agent.scrollback.push_block(RenderBlock::UserPrompt(b));
            agent
                .scrollback
                .push_block(RenderBlock::agent_message("ok"));
        }
        agent.scrollback.prepare_layout(80, 40);
    }
    app
}

/// With confirm-before-rewind off, picking a non-zero turn executes immediately.
#[test]
fn picker_select_nonzero_target_executes_immediately_when_confirm_off() {
    let mut app = app_with_two_turns();
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Picker { .. }
    ));

    let effects = dispatch(Action::RewindPickerSelect(1), &mut app);
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 1,
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 1
        }
    ));
}

/// With confirm-before-rewind on (default), picking a non-zero target opens confirm.
#[test]
fn picker_select_nonzero_target_opens_confirm_when_setting_on() {
    let mut app = app_with_two_turns();
    assert!(app.current_ui.confirm_before_rewind_enabled());
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );

    let effects = dispatch(Action::RewindPickerSelect(1), &mut app);
    assert!(
        effects.is_empty(),
        "confirm setting on waits, got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm {
            target_prompt_index: 1,
            active_idx: 0,
            ..
        }
    ));
}

/// Picking any target (including 0) opens confirm when the setting is on.
#[test]
fn picker_select_target_zero_opens_confirm() {
    let mut app = app_with_two_turns();
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );

    let effects = dispatch(Action::RewindPickerSelect(0), &mut app);
    assert!(
        effects.is_empty(),
        "confirm setting on waits for Yes/No, got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm {
            target_prompt_index: 0,
            active_idx: 0,
            ..
        }
    ));
}

/// Confirm Yes executes conversation-only rewind.
#[test]
fn confirm_yes_executes_rewind() {
    let mut app = app_with_two_turns();
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    dispatch(Action::RewindPickerSelect(1), &mut app);
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm {
            target_prompt_index: 1,
            ..
        }
    ));

    let effects = dispatch(Action::RewindConfirm(1), &mut app);
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 1,
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 1
        }
    ));
}

/// "Yes, and don't ask again" turns the setting off and executes this rewind.
#[test]
fn confirm_never_ask_persists_setting_off_and_executes() {
    let mut app = app_with_two_turns();
    assert!(app.current_ui.confirm_before_rewind_enabled());
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    dispatch(Action::RewindPickerSelect(1), &mut app);

    let effects = dispatch(Action::RewindConfirmNeverAsk(1), &mut app);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistSetting {
                key: "confirm_before_rewind",
                value: crate::settings::SettingValue::Bool(false),
                ..
            }
        )),
        "must persist setting off, got {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::RewindExecute {
                target_prompt_index: 1,
                ..
            }
        )),
        "must execute rewind, got {effects:?}"
    );
    assert!(!app.current_ui.confirm_before_rewind_enabled());
    assert_eq!(app.current_ui.confirm_before_rewind, Some(false));
    assert!(
        app.agents[&id].toast.is_none(),
        "never-ask must not toast settings checkmark, got {:?}",
        app.agents[&id].toast
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 1
        }
    ));
}

/// With confirm off, target 0 executes immediately (same as non-zero targets).
#[test]
fn picker_select_target_zero_executes_immediately_when_confirm_off() {
    let mut app = app_with_two_turns();
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );

    let effects = dispatch(Action::RewindPickerSelect(0), &mut app);
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 0,
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 0
        }
    ));
}

/// Non-zero success keeps earlier turns, truncates from the target, and toasts.
#[test]
fn rewind_success_nonzero_target_keeps_prefix_and_toasts() {
    let mut app = app_with_two_turns();
    let id = AgentId(0);
    let len_before = app.agents[&id].scrollback.len();

    dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: rewind_success(1, "turn 1"),
        }),
        &mut app,
    );

    let agent = &app.agents[&id];
    assert!(
        agent.scrollback.len() < len_before,
        "tail from target 1 must drop"
    );
    assert!(matches!(
        &agent.scrollback.entry(0).unwrap().block,
        RenderBlock::UserPrompt(b) if b.text == "turn 0"
    ));
    assert!(matches!(
        &agent.scrollback.entry(1).unwrap().block,
        RenderBlock::AgentMessage(_)
    ));
    assert_eq!(
        agent.scrollback.len(),
        2,
        "only turn 0 (prompt + reply) remains"
    );
    assert_eq!(
        agent.toast.as_ref().map(|(m, _)| m.as_str()),
        Some("Reverted conversation")
    );
}

/// Target-0 confirm → execute: the edited text is stashed exactly when the
/// rewind executes; on success the transcript truncates at the prompt, the
/// edited text is resubmitted from there, the editor closes, and the
/// composer draft survives (no "Reverted conversation" system note).
#[test]
fn inline_edit_conversation_only_success_resubmits_and_closes_editor() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    dispatch(Action::InlineEditSubmit, &mut app);
    dispatch(points_loaded(id), &mut app);
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm { .. }
    ));

    let effects = dispatch(Action::RewindConfirm(0), &mut app);
    assert!(matches!(
        &effects[0],
        Effect::RewindExecute {
            target_prompt_index: 0,
            ..
        }
    ));
    {
        let agent = &app.agents[&id];
        assert_eq!(
            agent.pending_inline_resubmit.as_deref(),
            Some("fix the bug properly"),
            "stash armed at execute time"
        );
        assert!(
            agent.inline_edit.is_some(),
            "editor open until the rewind lands"
        );
    }

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: rewind_success(0, "fix the bug"),
        }),
        &mut app,
    );

    assert!(
        effects.iter().any(
            |e| matches!(e, Effect::SendPrompt { text, .. } if text == "fix the bug properly")
        ),
        "edited prompt must be sent, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(agent.inline_edit.is_none(), "editor closed on success");
    assert!(agent.scrollback.inline_edit_height().is_none());
    assert!(agent.pending_inline_resubmit.is_none());
    assert_eq!(
        agent.prompt.text(),
        "composer draft",
        "composer draft survives"
    );
    // Transcript truncated at the prompt; only the resubmitted prompt block
    // remains (no "Reverted conversation" system note).
    assert_eq!(agent.scrollback.len(), 1);
    match &agent.scrollback.entry(0).unwrap().block {
        RenderBlock::UserPrompt(b) => assert_eq!(b.text, "fix the bug properly"),
        other => panic!("expected resubmitted user prompt, got {other:?}"),
    }
}

/// No / Esc dismiss from confirm during inline edit restores the editor draft.
#[test]
fn inline_edit_dismiss_from_confirm_keeps_editor() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    dispatch(Action::InlineEditSubmit, &mut app);
    dispatch(points_loaded(id), &mut app);
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm { .. }
    ));

    dispatch(Action::RewindDismiss, &mut app);

    let agent = &app.agents[&id];
    assert!(agent.rewind_state.is_none(), "overlay dismissed");
    assert_eq!(
        agent
            .inline_edit
            .as_ref()
            .expect("editor still open")
            .textarea
            .text(),
        "fix the bug properly"
    );
    assert!(agent.pending_inline_resubmit.is_none());
    assert_eq!(
        agent.prompt.text(),
        "composer draft",
        "composer draft restored on dismiss"
    );
}

/// Inline-edit of an older prompt with confirm off: points load executes
/// immediately — resubmit armed.
#[test]
fn inline_edit_nonzero_target_points_loaded_executes_immediately_when_confirm_off() {
    let mut app = test_app_with_agent();
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("sess".to_string()));
        for i in 0..2 {
            let mut b = UserPromptBlock::new(format!("turn {i}"));
            b.prompt_index = Some(i);
            agent.scrollback.push_block(RenderBlock::UserPrompt(b));
            agent
                .scrollback
                .push_block(RenderBlock::agent_message("ok"));
        }
        agent.scrollback.prepare_layout(80, 40);
        // Entry 2 is the second user prompt (index 1).
        assert!(agent.enter_inline_edit(2));
        agent
            .inline_edit
            .as_mut()
            .unwrap()
            .textarea
            .set_text("turn 1 edited");
        agent.prompt.set_text("composer draft");
    }

    let effects = dispatch(Action::InlineEditSubmit, &mut app);
    assert!(matches!(&effects[0], Effect::FetchRewindPoints { .. }));

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 1,
                ..
            }
        ),
        "got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(matches!(
        agent.rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 1
        }
    ));
    assert_eq!(
        agent.pending_inline_resubmit.as_deref(),
        Some("turn 1 edited")
    );
    assert!(
        agent.inline_edit.is_some(),
        "editor open until execute lands"
    );
}

/// Inline-edit of an older prompt with confirm on (default): opens confirm.
#[test]
fn inline_edit_nonzero_target_opens_confirm_when_setting_on() {
    let mut app = test_app_with_agent();
    assert!(app.current_ui.confirm_before_rewind_enabled());
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("sess".to_string()));
        for i in 0..2 {
            let mut b = UserPromptBlock::new(format!("turn {i}"));
            b.prompt_index = Some(i);
            agent.scrollback.push_block(RenderBlock::UserPrompt(b));
            agent
                .scrollback
                .push_block(RenderBlock::agent_message("ok"));
        }
        agent.scrollback.prepare_layout(80, 40);
        assert!(agent.enter_inline_edit(2));
        agent
            .inline_edit
            .as_mut()
            .unwrap()
            .textarea
            .set_text("turn 1 edited");
    }

    dispatch(Action::InlineEditSubmit, &mut app);
    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    assert!(effects.is_empty(), "confirm setting on, got {effects:?}");
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm {
            target_prompt_index: 1,
            active_idx: 0,
            ..
        }
    ));
    assert!(app.agents[&id].pending_inline_resubmit.is_none());
}

/// Points-loaded begin_rewind path: confirm off executes immediately.
#[test]
fn inline_edit_target_zero_executes_immediately_when_confirm_off() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);

    dispatch(Action::InlineEditSubmit, &mut app);
    let effects = dispatch(points_loaded(id), &mut app);
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 0,
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 0
        }
    ));
    assert_eq!(
        app.agents[&id].pending_inline_resubmit.as_deref(),
        Some("fix the bug properly")
    );
}

/// Classic points-loaded path with confirm off executes immediately.
#[test]
fn classic_points_loaded_target_zero_executes_when_confirm_off() {
    let mut app = test_app_with_agent();
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("fix the bug"));
        agent
            .scrollback
            .push_block(RenderBlock::agent_message("done"));
        agent.scrollback.prepare_layout(80, 40);
        agent.scrollback.set_selected(Some(0));
    }

    dispatch(Action::Rewind, &mut app);
    let effects = dispatch(points_loaded(id), &mut app);
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 0,
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 0
        }
    ));
}

/// Dismissing the confirm aborts: the overlay closes, nothing was stashed,
/// and the editor is still open with the edit intact.
#[test]
fn inline_edit_dismiss_from_confirm_returns_to_editor() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    dispatch(Action::InlineEditSubmit, &mut app);
    dispatch(points_loaded(id), &mut app);

    dispatch(Action::RewindDismiss, &mut app);

    let agent = &app.agents[&id];
    assert!(agent.rewind_state.is_none());
    assert!(agent.pending_inline_resubmit.is_none());
    assert_eq!(
        agent
            .inline_edit
            .as_ref()
            .expect("editor still open")
            .textarea
            .text(),
        "fix the bug properly"
    );
}

/// Submitting mid-turn raises the same cancel-offer `/rewind` does,
/// pre-targeted at the edited prompt, over the still-open editor;
/// confirming cancels the turn and re-enters the flow via a points fetch.
#[test]
fn inline_edit_busy_submit_cancel_offer_confirm_cancels_and_fetches_points() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = crate::app::agent::AgentState::TurnRunning;

    let effects = dispatch(Action::InlineEditSubmit, &mut app);
    assert!(effects.is_empty(), "no effects yet: {effects:?}");
    {
        let agent = &app.agents[&id];
        let state = agent.rewind_state.as_ref().unwrap();
        assert!(matches!(
            state.phase,
            crate::views::rewind::RewindPhase::CancelOffer { .. }
        ));
        assert_eq!(state.selected_prompt_index, Some(0));
        assert!(agent.inline_edit.is_some(), "editor open behind the offer");
        assert!(agent.pending_inline_resubmit.is_none());
    }

    let effects = dispatch(Action::RewindCancelOffer, &mut app);
    assert!(matches!(&effects[0], Effect::CancelTurn { .. }));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchRewindPoints { .. })),
        "got {effects:?}"
    );
    let agent = &app.agents[&id];
    let state = agent.rewind_state.as_ref().unwrap();
    assert!(matches!(
        state.phase,
        crate::views::rewind::RewindPhase::Loading
    ));
    assert_eq!(
        state.selected_prompt_index,
        Some(0),
        "target survives the cancel"
    );
    assert!(agent.inline_edit.is_some(), "editor still open");
}

/// Dismissing the mid-turn cancel-offer ("let it finish") returns straight
/// to the still-open editor with the edit intact.
#[test]
fn inline_edit_busy_cancel_offer_dismiss_returns_to_editor() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = crate::app::agent::AgentState::TurnRunning;
    dispatch(Action::InlineEditSubmit, &mut app);

    dispatch(Action::RewindDismiss, &mut app);

    let agent = &app.agents[&id];
    assert!(agent.rewind_state.is_none());
    assert_eq!(
        agent
            .inline_edit
            .as_ref()
            .expect("editor open")
            .textarea
            .text(),
        "fix the bug properly"
    );
}

/// A failed execute drops the stashed resubmit but leaves the editor open
/// behind the error overlay — dismissing the error returns to editing.
#[test]
fn inline_edit_execute_failure_keeps_editor_open() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    drive_inline_submit_to_execute(&mut app);
    assert!(app.agents[&id].pending_inline_resubmit.is_some());

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteFailed {
            agent_id: id,
            error: "boom".into(),
        }),
        &mut app,
    );

    assert!(effects.is_empty());
    let agent = &app.agents[&id];
    assert!(
        agent.pending_inline_resubmit.is_none(),
        "stash dies with its rewind"
    );
    match &agent.rewind_state.as_ref().unwrap().phase {
        crate::views::rewind::RewindPhase::Error { message } => {
            assert_eq!(message, "boom");
        }
        other => panic!("expected Error, got {other:?}"),
    }
    assert_eq!(
        agent
            .inline_edit
            .as_ref()
            .expect("editor open")
            .textarea
            .text(),
        "fix the bug properly"
    );
    assert_eq!(agent.scrollback.len(), 2, "transcript untouched");
}

/// A rewind/execute response with `success: false` likewise drops the
/// stash, shows the error overlay, and leaves the editor open.
#[test]
fn inline_edit_unsuccessful_response_keeps_editor_open() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    drive_inline_submit_to_execute(&mut app);

    let mut response = rewind_success(0, "fix the bug");
    response.success = false;
    response.error = Some("conflict".into());
    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response,
        }),
        &mut app,
    );

    assert!(effects.is_empty());
    let agent = &app.agents[&id];
    assert!(agent.pending_inline_resubmit.is_none());
    match &agent.rewind_state.as_ref().unwrap().phase {
        crate::views::rewind::RewindPhase::Error { message } => {
            assert_eq!(message, "conflict");
        }
        other => panic!("expected Error, got {other:?}"),
    }
    assert!(agent.inline_edit.is_some(), "editor stays open");
    assert_eq!(agent.scrollback.len(), 2, "transcript untouched");
}

/// An edited prompt that happens to start with "/" is resubmitted verbatim
/// as a prompt (literal), not executed as a slash command on the
/// already-truncated transcript.
#[test]
fn inline_edit_resubmit_sends_slash_text_literally() {
    let mut app = app_mid_inline_edit("/etc/hosts is wrong, fix it");
    let id = AgentId(0);
    drive_inline_submit_to_execute(&mut app);

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: rewind_success(0, "fix the bug"),
        }),
        &mut app,
    );

    assert!(
        effects.iter().any(
            |e| matches!(e, Effect::SendPrompt { text, .. } if text == "/etc/hosts is wrong, fix it")
        ),
        "slash-lookalike edit must be sent as a prompt, got {effects:?}"
    );
}

/// If the user switches views while the rewind is in flight, the edited
/// text falls back into that agent's composer without clobbering an
/// existing draft (it is appended on a new line).
#[test]
fn inline_edit_rewind_success_after_view_switch_appends_to_draft() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    drive_inline_submit_to_execute(&mut app);
    app.active_view = ActiveView::Welcome;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: rewind_success(0, "fix the bug"),
        }),
        &mut app,
    );

    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, Effect::SendPrompt { .. })),
        "no resubmit while the view is elsewhere, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert_eq!(
        agent.prompt.text(),
        "composer draft\nfix the bug properly",
        "draft preserved, edited text appended"
    );
    assert!(agent.inline_edit.is_none(), "editor closed on success");
}

#[test]
fn inline_edit_view_switch_preserves_image_draft_when_appending_resubmit() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    let draft_text = {
        let agent = app.agents.get_mut(&id).unwrap();
        let end = agent.prompt.text().len();
        agent.prompt.set_cursor(end);
        agent
            .prompt
            .insert_image(crate::prompt_images::from_clipboard_data(
                &crate::clipboard::ImageData {
                    data: vec![1, 2, 3],
                    mime_type: "image/png".into(),
                },
            ))
            .unwrap();
        agent.prompt.text().to_owned()
    };
    drive_inline_submit_to_execute(&mut app);
    app.active_view = ActiveView::Welcome;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: rewind_success(0, "fix the bug"),
        }),
        &mut app,
    );

    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect, Effect::SendPrompt { .. }))
    );
    let agent = app.agents.get_mut(&id).unwrap();
    assert_eq!(
        agent.prompt.text(),
        format!("{draft_text}\nfix the bug properly")
    );
    let restored_image_ids = agent
        .prompt
        .textarea
        .elements()
        .iter()
        .filter(|element| element.kind == crate::views::prompt_widget::KIND_IMAGE)
        .map(|element| element.id)
        .collect::<Vec<_>>();
    assert_eq!(
        restored_image_ids.len(),
        1,
        "restored draft must retain one image chip",
    );
    let images = agent.prompt.drain_images();
    assert_eq!(images.len(), 1, "reconciliation must retain the image");
    assert_eq!(
        images[0].element_id, restored_image_ids[0],
        "restored image must bind to the re-registered chip element",
    );
}

#[test]
fn stacked_rewinds_each_get_their_own_pid_and_orphans_drop_independently() {
    // Two rewinds → two cancelled PRs to drain. Each carries its own
    // promptId; both fail to match current_prompt_id (None) and are
    // silently discarded with no banner.
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("a".into()), &mut app);
    let pid_a = app.agents[&id].session.current_prompt_id.clone();
    dispatch(Action::CancelTurn, &mut app);
    dispatch(Action::SendPrompt("b".into()), &mut app);
    let pid_b = app.agents[&id].session.current_prompt_id.clone();
    dispatch(Action::CancelTurn, &mut app);
    assert_ne!(pid_a, pid_b);
    assert!(app.agents[&id].session.current_prompt_id.is_none());

    let pr = |pid: &Option<String>| {
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Ok(acp::PromptResponse::new(acp::StopReason::Cancelled)
                .meta(serde_json::json!({ "promptId": pid }).as_object().cloned())),
            http_status: None,
            prompt_id: None,
        })
    };
    dispatch(pr(&pid_a), &mut app);
    dispatch(pr(&pid_b), &mut app);
    assert_eq!(app.agents[&id].scrollback.len(), 0);
    assert!(app.agents[&id].session.state.is_idle());
}

fn user_block(text: &str, pi: Option<usize>) -> RenderBlock {
    let mut b = UserPromptBlock::new(text);
    b.prompt_index = pi;
    RenderBlock::UserPrompt(b)
}

/// A successful conversation rewind truncates the transcript tail
/// (`remove_from`) — the purge must fire exactly once.
#[test]
fn rewind_success_truncation_releases_retained_memory() {
    use crate::memory_release::test_support;
    test_support::install_counting_hook();

    let response = crate::views::rewind::RewindResponse {
        success: true,
        target_prompt_index: 0,
        reverted_files: Vec::new(),
        clean_files: Vec::new(),
        conflicts: Vec::new(),
        error: None,
        mode: Some("conversation_only".into()),
        prompt_text: Some("alpha".into()),
    };

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    if let Some(agent) = app.agents.get_mut(&id) {
        agent.scrollback.push_block(user_block("alpha", Some(0)));
        agent.scrollback.push_block(RenderBlock::agent_message("a"));
    }
    let len_before = app.agents[&id].scrollback.len();
    let before = test_support::calls();
    dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response,
        }),
        &mut app,
    );
    assert!(
        app.agents[&id].scrollback.len() < len_before,
        "fixture sanity: the conversation rewind must truncate entries"
    );
    assert_eq!(
        test_support::calls(),
        before + 1,
        "the rewound tail dropped — exactly one purge"
    );
}

/// A successful rewind confirms via a toast in the full TUI; minimal mode
/// keeps the scrollback system block (it never renders toasts).
#[test]
fn rewind_success_toasts_in_full_tui_and_commits_system_block_in_minimal() {
    let response = crate::views::rewind::RewindResponse {
        success: true,
        target_prompt_index: 0,
        reverted_files: Vec::new(),
        clean_files: Vec::new(),
        conflicts: Vec::new(),
        error: None,
        mode: Some("conversation_only".into()),
        prompt_text: None,
    };

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    if let Some(agent) = app.agents.get_mut(&id) {
        agent.scrollback.push_block(user_block("alpha", Some(0)));
        agent.scrollback.push_block(RenderBlock::agent_message("a"));
    }
    dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: response.clone(),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].toast.as_ref().map(|(m, _)| m.as_str()),
        Some("Reverted conversation")
    );
    assert_eq!(
        app.agents[&id].scrollback.len(),
        0,
        "the confirmation must not land in scrollback in the full TUI"
    );

    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    if let Some(agent) = app.agents.get_mut(&id) {
        agent.scrollback.push_block(user_block("alpha", Some(0)));
        agent.scrollback.push_block(RenderBlock::agent_message("a"));
    }
    dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response,
        }),
        &mut app,
    );
    assert!(app.agents[&id].toast.is_none());
    assert_eq!(last_system_text(&app, id), "Reverted conversation");
}

#[test]
fn primary_path_returns_correct_idx_for_each_prompt() {
    let mut sb = ScrollbackState::new();
    let alpha = sb.push_block(user_block("alpha", Some(0)));
    sb.push_block(RenderBlock::agent_message("a"));
    let bravo = sb.push_block(user_block("bravo", Some(1)));
    sb.push_block(RenderBlock::agent_message("b"));
    let charlie = sb.push_block(user_block("charlie", Some(2)));
    sb.push_block(RenderBlock::agent_message("c"));

    let alpha_idx = sb.index_of_id(alpha).unwrap();
    let bravo_idx = sb.index_of_id(bravo).unwrap();
    let charlie_idx = sb.index_of_id(charlie).unwrap();

    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 0),
        Some(alpha_idx)
    );
    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 1),
        Some(bravo_idx)
    );
    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 2),
        Some(charlie_idx)
    );
}

/// Interjections render as standard user prompts but the shell never
/// numbers them — the positional fallback must skip them or every mapping
/// after an interjection is off by one.
#[test]
fn fallback_path_skips_interjections() {
    let mut sb = ScrollbackState::new();
    let alpha = sb.push_block(user_block("alpha", None));
    sb.push_block(RenderBlock::agent_message("a"));
    sb.push_block(RenderBlock::interjection_prompt("mid-turn steer"));
    sb.push_block(RenderBlock::agent_message("a2"));
    let bravo = sb.push_block(user_block("bravo", None));

    let alpha_idx = sb.index_of_id(alpha).unwrap();
    let bravo_idx = sb.index_of_id(bravo).unwrap();

    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 0),
        Some(alpha_idx)
    );
    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 1),
        Some(bravo_idx),
        "index 1 must map to the next real prompt, not the interjection"
    );
}

/// Selecting an interjection (or an entry after it within the same turn)
/// anchors rewind on the enclosing turn's prompt, not the next turn's.
#[test]
fn shell_prompt_index_at_resolves_interjection_to_enclosing_turn() {
    use super::super::rewind::shell_prompt_index_at;

    let mut sb = ScrollbackState::new();
    sb.push_block(user_block("alpha", Some(0)));
    sb.push_block(RenderBlock::agent_message("a"));
    let ij = sb.push_block(RenderBlock::interjection_prompt("mid-turn steer"));
    sb.push_block(RenderBlock::agent_message("a2"));
    sb.push_block(user_block("bravo", Some(1)));

    let ij_idx = sb.index_of_id(ij).unwrap();
    assert_eq!(shell_prompt_index_at(&sb, ij_idx), Some(0));
    // A block after the interjection but before the next prompt still
    // belongs to turn 0.
    assert_eq!(shell_prompt_index_at(&sb, ij_idx + 1), Some(0));
}

/// Legacy meta-less scrollbacks: the positional count inside
/// `shell_prompt_index_at` must also exclude interjections.
#[test]
fn shell_prompt_index_at_counting_fallback_skips_interjections() {
    use super::super::rewind::shell_prompt_index_at;

    let mut sb = ScrollbackState::new();
    sb.push_block(user_block("alpha", None));
    sb.push_block(RenderBlock::interjection_prompt("steer"));
    let bravo = sb.push_block(user_block("bravo", None));

    let bravo_idx = sb.index_of_id(bravo).unwrap();
    assert_eq!(shell_prompt_index_at(&sb, bravo_idx), Some(1));
}

#[test]
fn fallback_path_returns_correct_idx_when_prompt_index_is_none() {
    let mut sb = ScrollbackState::new();
    let alpha = sb.push_block(user_block("alpha", None));
    sb.push_block(RenderBlock::agent_message("a"));
    let bravo = sb.push_block(user_block("bravo", None));
    sb.push_block(RenderBlock::agent_message("b"));
    let charlie = sb.push_block(user_block("charlie", None));
    sb.push_block(RenderBlock::agent_message("c"));

    let alpha_idx = sb.index_of_id(alpha).unwrap();
    let bravo_idx = sb.index_of_id(bravo).unwrap();
    let charlie_idx = sb.index_of_id(charlie).unwrap();

    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 0),
        Some(alpha_idx)
    );
    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 1),
        Some(bravo_idx)
    );
    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 2),
        Some(charlie_idx)
    );
}
