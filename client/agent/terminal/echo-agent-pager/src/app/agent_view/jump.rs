//! `/jump` picker: transcript preview syncing and key/mouse handling.

use super::AgentView;
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::views::jump::{
    JumpInput, JumpRestore, handle_jump_key, jump_activate, jump_row_at, move_cursor,
    set_jump_cursor,
};
use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};

impl AgentView {
    /// Close the `/jump` picker (if open) and restore the viewport it opened
    /// from. Shared by the `Esc` dismiss path and the rewind / inline-edit entry
    /// points, so a shadowed picker can't reappear stale.
    pub(crate) fn dismiss_jump_picker(&mut self) {
        if let Some(js) = self.jump_state.take() {
            self.restore_jump_viewport(js.restore);
        }
    }

    /// Re-pin the viewport the picker captured (width-stable bookmark), restore
    /// the prior selection, and re-arm follow mode. Shared by `Esc` dismiss and
    /// the failed-jump restore, so both stay consistent under a resize.
    pub(crate) fn restore_jump_viewport(&mut self, restore: JumpRestore) {
        self.scrollback.set_selected(restore.selected);
        if let Some(bookmark) = restore.bookmark {
            self.scrollback.restore_scroll_bookmark(bookmark);
        }
        if restore.follow_mode {
            self.scrollback.enable_follow();
        }
    }

    /// True when another prompt overlay owns the input slot, so the `/jump`
    /// picker must not open and an open one must be dismissed: rewind, inline
    /// edit, the `/btw` panel, or a pending permission / question / cancel-turn /
    /// plan-approval overlay. One predicate keeps dispatch, key, mouse, and
    /// scroll routing from disagreeing on the owner.
    pub(crate) fn jump_slot_taken(&self) -> bool {
        self.rewind_state.is_some()
            || self.inline_edit.is_some()
            || self.btw_state.is_some()
            || !self.no_input_overlay_pending()
    }

    /// Drop the picker when another overlay owns the input slot
    /// ([`Self::jump_slot_taken`]), so it can't eat wheel/keys while hidden.
    /// Returns whether it dropped one, so an `Esc` caller can spend that key
    /// here rather than let it also dismiss the overlay shadowing the picker
    /// (e.g. the `/btw` panel). Called at the input and scroll entry points.
    pub(super) fn dismiss_jump_picker_if_suppressed(&mut self) -> bool {
        if self.jump_state.is_some() && self.jump_slot_taken() {
            self.dismiss_jump_picker();
            return true;
        }
        false
    }

    /// Live-scroll the transcript to the turn under the picker cursor,
    /// anchored at the viewport TOP — where `jump_to_turn` lands — so the
    /// preview shows exactly what Enter commits to. (Rewind centers
    /// instead: it previews a cut point and needs both sides visible.)
    pub(super) fn sync_jump_preview(&mut self) {
        let Some(prompt_id) = self
            .jump_state
            .as_ref()
            .and_then(|js| js.entries.get(js.selected))
            .map(|entry| entry.prompt_entry_id)
        else {
            return;
        };
        // Resolve the stable id at the boundary; a removal since capture just
        // means no preview scroll rather than landing on the wrong block.
        if let Some(idx) = self.scrollback.index_of_id(prompt_id) {
            self.scrollback.scroll_to_entry_top(idx);
        }
    }

    pub(super) fn handle_jump_key(&mut self, key: &KeyEvent) -> InputOutcome {
        let Some(ref state) = self.jump_state else {
            return InputOutcome::Unchanged;
        };
        match handle_jump_key(state, key) {
            JumpInput::MoveUp => {
                if let Some(ref mut js) = self.jump_state {
                    move_cursor(js, -1);
                    self.sync_jump_preview();
                }
                InputOutcome::Changed
            }
            JumpInput::MoveDown => {
                if let Some(ref mut js) = self.jump_state {
                    move_cursor(js, 1);
                    self.sync_jump_preview();
                }
                InputOutcome::Changed
            }
            other => Self::jump_input_to_outcome(other),
        }
    }

    /// Map a terminal `JumpInput` to its `InputOutcome`. Shared by the key,
    /// mouse, and wheel paths so they can't drift.
    fn jump_input_to_outcome(input: JumpInput) -> InputOutcome {
        match input {
            JumpInput::Select(id) => InputOutcome::Action(Action::JumpPickerSelect(id)),
            JumpInput::Dismissed => InputOutcome::Action(Action::JumpDismiss),
            JumpInput::MoveUp | JumpInput::MoveDown | JumpInput::Consumed => InputOutcome::Changed,
        }
    }

    /// `Moved` moves the cursor (and previews); `Down(Left)` activates the
    /// row (Enter-equivalent). Row geometry comes from `jump_row_at`.
    pub(super) fn handle_jump_mouse(&mut self, mouse: &MouseEvent) -> InputOutcome {
        let Some(js) = self.jump_state.as_mut() else {
            return InputOutcome::Unchanged;
        };

        let area = self.pane_areas.prompt;
        let Some(idx) = jump_row_at(js, area, mouse.column, mouse.row) else {
            return InputOutcome::Unchanged;
        };

        match mouse.kind {
            MouseEventKind::Moved => {
                if set_jump_cursor(js, idx) {
                    self.sync_jump_preview();
                    InputOutcome::Changed
                } else {
                    InputOutcome::Unchanged
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                set_jump_cursor(js, idx);
                let activated = jump_activate(js);
                Self::jump_input_to_outcome(activated)
            }
            _ => InputOutcome::Unchanged,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::scrollback::block::RenderBlock;
    use crate::views::jump::{JumpRestore, JumpState};

    #[test]
    fn preview_scrolls_to_cursor_turn() {
        let mut agent = crate::test_util::make_agent_view(None, "/tmp");
        agent.scrollback.push_block(RenderBlock::user_prompt("Q1"));
        for i in 0..20 {
            agent
                .scrollback
                .push_block(RenderBlock::agent_message(format!("para {i}")));
        }
        agent.scrollback.push_block(RenderBlock::user_prompt("Q2"));
        agent.scrollback.push_block(RenderBlock::agent_message("a"));
        agent.scrollback.prepare_layout(80, 6);
        agent.scrollback.goto_bottom();
        let at_bottom = agent.scrollback.scroll_offset();

        agent.jump_state = Some(JumpState {
            entries: agent.scrollback.timeline_entries(),
            selected: 0,
            restore: JumpRestore {
                bookmark: agent.scrollback.capture_scroll_bookmark(),
                selected: None,
                follow_mode: true,
            },
        });

        agent.sync_jump_preview();
        assert!(
            agent.scrollback.scroll_offset() < at_bottom,
            "previewing turn 1 scrolls the transcript up"
        );
    }
}
