//! `/jump` picker dispatchers: pure client-side turn navigation.

use crate::app::actions::Effect;
use crate::app::app_view::{ActiveView, AppView};
use crate::scrollback::entry::EntryId;
use crate::views::jump::{JumpRestore, JumpState};

pub(super) fn dispatch_jump_show_picker(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    // Refuse if another prompt overlay owns the input slot (rewind, inline-edit,
    // /btw, or a pending permission/question/cancel-turn/plan overlay) — an
    // opened picker would be hidden but still eat input.
    if agent.jump_slot_taken() {
        return vec![];
    }

    let entries = agent.scrollback.timeline_entries();
    if entries.len() < 2 {
        app.show_toast("Nothing to jump to yet");
        return vec![];
    }

    let restore = JumpRestore {
        bookmark: agent.scrollback.capture_scroll_bookmark(),
        selected: agent.scrollback.selected(),
        follow_mode: agent.scrollback.is_follow_mode(),
    };
    // Open on the turn currently at the viewport top (rows are oldest-first,
    // so the row index is the turn index).
    let selected = agent
        .scrollback
        .active_turn_for_viewport()
        .unwrap_or(entries.len() - 1)
        .min(entries.len() - 1);

    let preview_id = entries[selected].prompt_entry_id;
    agent.jump_state = Some(JumpState {
        entries,
        selected,
        restore,
    });
    // Same top anchor that cursor moves preview and Enter lands on.
    if let Some(idx) = agent.scrollback.index_of_id(preview_id) {
        agent.scrollback.scroll_to_entry_top(idx);
    }
    vec![]
}

pub(super) fn dispatch_jump_picker_select(app: &mut AppView, prompt_id: EntryId) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let Some(js) = agent.jump_state.take() else {
        return vec![];
    };
    // The stable id resolves at the boundary; it fails only if the prompt was
    // removed (async clear/rewind) while the picker was open. Restore the
    // captured viewport so a failed jump never strands the transcript at the
    // last preview scroll.
    if !agent.scrollback.jump_to_entry(prompt_id) {
        agent.restore_jump_viewport(js.restore);
    }
    vec![]
}

pub(super) fn dispatch_jump_dismiss(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    agent.dismiss_jump_picker();
    vec![]
}
