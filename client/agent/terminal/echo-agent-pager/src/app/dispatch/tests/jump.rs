//! Tests for the `/jump` picker dispatchers.

use super::*;

fn push_turns(app: &mut AppView, id: AgentId, n: usize) {
    let agent = app.agents.get_mut(&id).unwrap();
    for i in 0..n {
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt(format!("question {i}")));
        let tall = (0..8)
            .map(|p| format!("answer {i} para {p}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        agent
            .scrollback
            .push_block(RenderBlock::agent_message(tall));
    }
    agent.scrollback.prepare_layout(80, 6);
}

#[test]
fn show_picker_needs_two_turns() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 1);

    let effects = dispatch(Action::JumpShowPicker, &mut app);
    assert!(effects.is_empty());
    assert!(
        app.agents[&id].jump_state.is_none(),
        "a single turn has nothing to jump to"
    );
}

#[test]
fn show_picker_snapshots_viewport_and_opens_on_active_turn() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    app.agents.get_mut(&id).unwrap().scrollback.goto_bottom();

    dispatch(Action::JumpShowPicker, &mut app);

    let agent = &app.agents[&id];
    let js = agent.jump_state.as_ref().expect("picker open");
    assert_eq!(js.entries.len(), 3);
    assert_eq!(js.entries[0].preview, "question 0");
    assert_eq!(js.selected, 2, "opens on the turn at the viewport top");
    assert!(
        js.restore.bookmark.is_some(),
        "captured a viewport bookmark"
    );
    assert!(js.restore.follow_mode, "goto_bottom left follow on");
}

#[test]
fn show_picker_refused_while_rewind_open() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    app.agents.get_mut(&id).unwrap().rewind_state = Some(
        crate::views::rewind::RewindState::new_cancel_offer(0, None, None),
    );

    dispatch(Action::JumpShowPicker, &mut app);
    assert!(app.agents[&id].jump_state.is_none());
}

#[test]
fn show_picker_refused_while_inline_edit_open() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    assert!(
        app.agents.get_mut(&id).unwrap().enter_inline_edit(0),
        "entered inline edit on the first prompt"
    );

    dispatch(Action::JumpShowPicker, &mut app);
    assert!(
        app.agents[&id].jump_state.is_none(),
        "picker must not stack on an open inline edit (wheel scroll would leak)"
    );
}

#[test]
fn show_picker_refused_while_input_overlay_pending() {
    // A pending permission / question / cancel-turn / plan-approval overlay
    // suppresses the picker's rendering, so opening one would be invisible but
    // still eat wheel/keys — `/jump` must refuse.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    app.agents.get_mut(&id).unwrap().cancel_turn_view =
        Some(crate::views::modal::CancelTurnViewState {
            active_idx: 0,
            running_count: 1,
        });

    dispatch(Action::JumpShowPicker, &mut app);
    assert!(
        app.agents[&id].jump_state.is_none(),
        "/jump must not open behind a pending input overlay"
    );
}

#[test]
fn scroll_drops_hidden_jump_picker_behind_input_overlay() {
    // If an input overlay arrives (async) after the picker opened, the picker
    // is hidden but `jump_state` lingers; a wheel event must drop it instead of
    // scrolling a cursor the user can't see (and shifting the transcript).
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    app.agents.get_mut(&id).unwrap().scrollback.goto_bottom();

    dispatch(Action::JumpShowPicker, &mut app);
    assert!(app.agents[&id].jump_state.is_some(), "picker opened");

    app.agents.get_mut(&id).unwrap().cancel_turn_view =
        Some(crate::views::modal::CancelTurnViewState {
            active_idx: 0,
            running_count: 1,
        });
    app.agents.get_mut(&id).unwrap().handle_scroll(1, 0, 0);

    let agent = &app.agents[&id];
    assert!(
        agent.jump_state.is_none(),
        "a hidden picker is dropped on scroll, not driven"
    );
    assert!(
        agent.cancel_turn_view.is_some(),
        "the suppressing overlay is untouched"
    );
}

#[test]
fn key_drops_hidden_jump_picker_behind_input_overlay() {
    // The key-path mirror: with an input overlay pending (and, as here, the
    // scrollback pane focused so the pane-gated cancel-turn panel is skipped),
    // a key must drop the hidden picker instead of the picker handling it.
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);

    dispatch(Action::JumpShowPicker, &mut app);
    assert!(app.agents[&id].jump_state.is_some(), "picker opened");

    app.agents.get_mut(&id).unwrap().cancel_turn_view =
        Some(crate::views::modal::CancelTurnViewState {
            active_idx: 0,
            running_count: 1,
        });
    let reg = crate::actions::ActionRegistry::defaults();
    let ev = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let _ = app.agents.get_mut(&id).unwrap().handle_input(&ev, &reg);

    assert!(
        app.agents[&id].jump_state.is_none(),
        "a hidden picker is dropped before it can handle keys"
    );
}

#[test]
fn ctrl_c_stays_cancellable_with_jump_open() {
    use crate::app::agent::AgentState;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    // /jump must not swallow the advertised Ctrl+C while a turn is running.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    dispatch(Action::JumpShowPicker, &mut app);
    assert!(app.agents[&id].jump_state.is_some(), "picker opened");

    let reg = crate::actions::ActionRegistry::defaults();
    let ev = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    let outcome = app.agents.get_mut(&id).unwrap().handle_input(&ev, &reg);

    assert!(
        app.agents[&id].jump_state.is_none(),
        "Ctrl+C dismissed the picker"
    );
    assert!(
        matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
        "and cancelled the running turn, got {outcome:?}"
    );
}

#[test]
fn show_picker_refused_while_btw_open() {
    // /btw owns the prompt slot; opening /jump behind it would split input.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    app.agents.get_mut(&id).unwrap().btw_state = Some(
        crate::views::btw_overlay::BtwOverlayState::done("q".into(), "a".into()),
    );

    dispatch(Action::JumpShowPicker, &mut app);
    assert!(
        app.agents[&id].jump_state.is_none(),
        "/jump must not open behind /btw"
    );
}

#[test]
fn session_reload_dismisses_jump_picker() {
    // jump_state indexes the pre-reload transcript; a reconnect must drop it.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    dispatch(Action::JumpShowPicker, &mut app);
    assert!(app.agents[&id].jump_state.is_some(), "picker opened");

    app.agents.get_mut(&id).unwrap().begin_session_reload(1);
    assert!(
        app.agents[&id].jump_state.is_none(),
        "reload cleared the picker"
    );
}

#[test]
fn picker_select_jumps_and_closes() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    app.agents.get_mut(&id).unwrap().scrollback.goto_bottom();

    dispatch(Action::JumpShowPicker, &mut app);
    let target_id = app.agents[&id].jump_state.as_ref().unwrap().entries[0].prompt_entry_id;
    let target_entry = app.agents[&id].scrollback.index_of_id(target_id).unwrap();

    dispatch(Action::JumpPickerSelect(target_id), &mut app);

    let agent = &app.agents[&id];
    assert!(agent.jump_state.is_none(), "picker closed");
    assert_eq!(agent.scrollback.selected(), Some(target_entry));
    assert_eq!(agent.scrollback.current_turn(), Some(0));
    assert!(!agent.scrollback.is_follow_mode());
}

#[test]
fn picker_select_uses_stable_id_across_removal() {
    // The picker carries a stable EntryId, so removing an earlier entry (which
    // shifts every positional index) still lands the jump on the intended
    // prompt — a positional turn index would target the wrong block.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 4);
    dispatch(Action::JumpShowPicker, &mut app);

    let (first_id, target_id) = {
        let entries = &app.agents[&id].jump_state.as_ref().unwrap().entries;
        (
            entries[0].prompt_entry_id,
            entries.last().unwrap().prompt_entry_id,
        )
    };
    // Remove the first turn's prompt, shifting the positional indices.
    app.agents
        .get_mut(&id)
        .unwrap()
        .scrollback
        .remove_entry(first_id);

    dispatch(Action::JumpPickerSelect(target_id), &mut app);

    let agent = &app.agents[&id];
    assert!(agent.jump_state.is_none(), "picker closed");
    let expected = agent
        .scrollback
        .index_of_id(target_id)
        .expect("target prompt still present");
    assert_eq!(
        agent.scrollback.selected(),
        Some(expected),
        "stable id lands on the intended prompt even after indices shifted"
    );
}

#[test]
fn picker_select_restores_viewport_on_out_of_range_turn() {
    // A turn index can go stale if the turn list shrank (async clear/rewind)
    // while the picker was open; selecting it must restore the captured
    // viewport instead of stranding the transcript at the last preview.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    app.agents.get_mut(&id).unwrap().scrollback.goto_bottom();
    let at_bottom = app.agents[&id].scrollback.scroll_offset();

    dispatch(Action::JumpShowPicker, &mut app);
    // Move the preview far from the snapshot so a restore is observable.
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let first_id = agent.jump_state.as_ref().unwrap().entries[0].prompt_entry_id;
        let first = agent.scrollback.index_of_id(first_id).unwrap();
        agent.scrollback.scroll_to_entry_center(first);
    }
    assert_ne!(app.agents[&id].scrollback.scroll_offset(), at_bottom);

    dispatch(
        Action::JumpPickerSelect(crate::scrollback::entry::EntryId::new(999_999)),
        &mut app,
    );

    let agent = &app.agents[&id];
    assert!(agent.jump_state.is_none(), "picker closed");
    assert_eq!(
        agent.scrollback.scroll_offset(),
        at_bottom,
        "a failed jump restores the captured viewport"
    );
    assert!(agent.scrollback.is_follow_mode(), "follow restored");
}

#[test]
fn rewind_dismisses_open_jump_picker() {
    // The mirror of `show_picker_refused_while_rewind_open`: starting rewind
    // while the picker is open must dismiss it (and restore its viewport), so
    // the input-shadowed picker can't reappear stale once rewind closes.
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    app.agents.get_mut(&id).unwrap().scrollback.goto_bottom();
    let before_offset = app.agents[&id].scrollback.scroll_offset();

    dispatch(Action::JumpShowPicker, &mut app);
    // Preview a far turn so the viewport actually moved under the picker.
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let first_id = agent.jump_state.as_ref().unwrap().entries[0].prompt_entry_id;
        let first = agent.scrollback.index_of_id(first_id).unwrap();
        agent.scrollback.scroll_to_entry_center(first);
    }
    assert!(app.agents[&id].jump_state.is_some());
    assert_ne!(app.agents[&id].scrollback.scroll_offset(), before_offset);

    dispatch(Action::Rewind, &mut app);

    let agent = &app.agents[&id];
    assert!(
        agent.jump_state.is_none(),
        "rewind dismissed the jump picker"
    );
    assert!(agent.rewind_state.is_some(), "rewind opened");
    assert_eq!(
        agent.scrollback.scroll_offset(),
        before_offset,
        "the jump viewport was restored before rewind took over"
    );
}

#[test]
fn inline_edit_dismisses_open_jump_picker() {
    // The mirror of `show_picker_refused_while_inline_edit_open`: entering
    // inline edit while the picker is open dismisses it so it can't reappear
    // stale. (Inline edit re-centers on the edited entry, so only the picker
    // teardown is asserted, not the viewport.)
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    app.agents.get_mut(&id).unwrap().scrollback.goto_bottom();

    dispatch(Action::JumpShowPicker, &mut app);
    assert!(app.agents[&id].jump_state.is_some());

    let entered = app.agents.get_mut(&id).unwrap().enter_inline_edit(0);
    assert!(entered, "entered inline edit on the first prompt");

    let agent = &app.agents[&id];
    assert!(
        agent.jump_state.is_none(),
        "entering inline edit dismissed the jump picker"
    );
    assert!(agent.inline_edit.is_some(), "inline edit opened");
}

#[test]
fn dismiss_restores_viewport() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    push_turns(&mut app, id, 3);
    {
        let sb = &mut app.agents.get_mut(&id).unwrap().scrollback;
        sb.goto_bottom();
    }
    let before_offset = app.agents[&id].scrollback.scroll_offset();
    let before_selected = app.agents[&id].scrollback.selected();

    dispatch(Action::JumpShowPicker, &mut app);
    // Preview a far-away turn so the transcript actually moved.
    {
        let agent = app.agents.get_mut(&id).unwrap();
        let first_id = agent.jump_state.as_ref().unwrap().entries[0].prompt_entry_id;
        let first = agent.scrollback.index_of_id(first_id).unwrap();
        agent.scrollback.scroll_to_entry_center(first);
    }
    assert_ne!(app.agents[&id].scrollback.scroll_offset(), before_offset);

    dispatch(Action::JumpDismiss, &mut app);

    let agent = &app.agents[&id];
    assert!(agent.jump_state.is_none());
    assert_eq!(agent.scrollback.scroll_offset(), before_offset);
    assert_eq!(agent.scrollback.selected(), before_selected);
    assert!(agent.scrollback.is_follow_mode(), "follow restored");
}
