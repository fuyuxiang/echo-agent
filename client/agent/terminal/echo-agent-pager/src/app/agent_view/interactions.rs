//! Blocking interaction surfaces: permission prompts, the question view,
//! and the cancel-turn confirm flow (keys, mouse, and submit paths).
#[cfg(test)]
use super::test_fixtures;
use super::{
    AgentView, MULTI_CLICK_TIMEOUT_MS, PeekAnswerOutcome, question_visible_h,
    translate_local_submit,
};
#[cfg(test)]
use crate::actions::ActionRegistry;
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::input::key::RowWalk;
use crate::key;
use crate::views::modal::CancelTurnChoice;
use crate::views::prompt_widget::{EnterOutcome, PromptEvent};
use crate::views::question_view::QUESTION_VIEW_HPAD;
#[cfg(test)]
use crossterm::event::Event;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::Instant;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuestionSwitch {
    Next,
    Prev,
}
impl AgentView {
    /// Handle key input for the permission card. Like the question card it has
    /// an option-row mode and text modes (a followup message to the agent, and
    /// a hand-written always-allow pattern); `Esc` is the ladder back down
    /// through them ([`AgentView::card_esc`]) and `Ctrl+C` is the only cancel.
    pub(super) fn handle_permission_key(&mut self, key: &KeyEvent) -> InputOutcome {
        use crate::views::permission_view::PermissionFocus;
        if key.code == KeyCode::Esc {
            return self.handle_card_esc();
        }
        let perm_content_w = self
            .pane_areas
            .prompt
            .width
            .saturating_sub(QUESTION_VIEW_HPAD) as usize;
        let Some(perm) = self.permission_queue.front_mut() else {
            return InputOutcome::Unchanged;
        };
        if key!('f', CONTROL).matches(key) && perm.has_collapsible_display(perm_content_w) {
            perm.args_expanded = !perm.args_expanded;
            return InputOutcome::Changed;
        }
        match perm.focus {
            PermissionFocus::FollowupInput => {
                if key!('c', CONTROL).matches(key) {
                    return InputOutcome::Action(Action::PermissionCancel);
                }
                match self.prompt.route_enter(key) {
                    EnterOutcome::NewlineInserted => return InputOutcome::Changed,
                    EnterOutcome::Submit => {
                        let text = self.prompt.text().to_string();
                        return InputOutcome::Action(Action::PermissionFollowup(text));
                    }
                    EnterOutcome::PassThrough => {}
                }
                match self.prompt.handle_key(key) {
                    PromptEvent::Edited => InputOutcome::Changed,
                    PromptEvent::Ignored => InputOutcome::Changed,
                }
            }
            PermissionFocus::Options => {
                if key!('c', CONTROL).matches(key) {
                    return InputOutcome::Action(Action::PermissionCancel);
                }
                if let Some(walk) = RowWalk::from_key(key) {
                    perm.active_idx = walk.step(perm.active_idx, perm.options.len());
                    return InputOutcome::Changed;
                }
                if key.code == KeyCode::Char('j') || key.code == KeyCode::Down {
                    if perm.active_idx + 1 < perm.options.len() {
                        perm.active_idx += 1;
                    }
                    return InputOutcome::Changed;
                }
                if key.code == KeyCode::Char('k') || key.code == KeyCode::Up {
                    perm.active_idx = perm.active_idx.saturating_sub(1);
                    return InputOutcome::Changed;
                }
                if key.code == KeyCode::Enter {
                    if let Some(opt) = perm.options.get(perm.active_idx) {
                        return InputOutcome::Action(Action::PermissionSelect(
                            opt.option_id.clone(),
                        ));
                    }
                    return InputOutcome::Changed;
                }
                if let KeyCode::Char(ch @ '1'..='9') = key.code {
                    let idx = (ch as u8 - b'1') as usize;
                    if let Some(opt) = perm.options.get(idx) {
                        return InputOutcome::Action(Action::PermissionSelect(
                            opt.option_id.clone(),
                        ));
                    }
                    return InputOutcome::Changed;
                }
                if key!('o', CONTROL).matches(key) {
                    return InputOutcome::Action(Action::SetYoloMode(!self.session.is_yolo()));
                }
                let is_right = key.code == KeyCode::Right || key.code == KeyCode::Char('>');
                let is_left = key.code == KeyCode::Left || key.code == KeyCode::Char('<');
                if (is_right || is_left) && perm.has_adjustable_scope() {
                    if let Some(ref mut scope) = perm.mcp_scope {
                        if is_left && scope.server_prefix.is_some() {
                            scope.selected = crate::views::permission_view::McpScope::Server;
                        } else if is_right {
                            scope.selected = crate::views::permission_view::McpScope::Tool;
                        }
                        let on_scoped_row = perm
                            .options
                            .get(perm.active_idx)
                            .is_some_and(|o| perm.is_scoped_option(o));
                        if !on_scoped_row && let Some(idx) = perm.scoped_allow_row_idx() {
                            perm.active_idx = idx;
                        }
                    } else if let Some(len) = perm
                        .bash_highlights
                        .as_ref()
                        .map(|h| h.highlighted_words.len())
                    {
                        let on_scoped_row = perm
                            .options
                            .get(perm.active_idx)
                            .is_some_and(|o| perm.is_scoped_option(o));
                        if !on_scoped_row && let Some(idx) = perm.scoped_row_jump_idx() {
                            perm.active_idx = idx;
                        }
                        let active_row = perm
                            .options
                            .get(perm.active_idx)
                            .map(|o| o.option_id.0.as_ref());
                        if active_row
                            == Some(crate::views::permission_view::REJECT_ALWAYS_COMMAND_OPTION_ID)
                        {
                            if is_right && perm.bash_deny_selection_count < len {
                                perm.bash_deny_selection_count += 1;
                            } else if is_left && perm.bash_deny_selection_count > 1 {
                                perm.bash_deny_selection_count -= 1;
                            }
                        } else if active_row
                            == Some(crate::views::permission_view::ALLOW_ALWAYS_COMMAND_OPTION_ID)
                        {
                            perm.bash_selection_count = perm.step_persisting_allow_scope(is_right);
                        }
                    }
                    return InputOutcome::Changed;
                }
                let on_reject_once = perm.options.get(perm.active_idx).is_some_and(|o| {
                    o.kind == agent_client_protocol::PermissionOptionKind::RejectOnce
                });
                if key.code == KeyCode::Char('e')
                    && key.modifiers.is_empty()
                    && perm.has_editable_bash_pattern()
                    && !on_reject_once
                    && let Some(idx) = perm.allow_always_command_idx()
                {
                    perm.active_idx = idx;
                    let initial = crate::views::permission_view::preview_command_text(perm);
                    self.permission_pattern_edit = Some(
                        crate::views::permission_view::PatternEditState::new(initial),
                    );
                    perm.focus = PermissionFocus::PatternEdit;
                    return InputOutcome::Changed;
                }
                if let Some(opt) = perm.options.get(perm.active_idx)
                    && opt.kind == agent_client_protocol::PermissionOptionKind::RejectOnce
                    && crate::input::key::is_text_input_key(key)
                    && matches!(key.code, KeyCode::Char(c) if !c.is_ascii_digit())
                {
                    perm.focus = PermissionFocus::FollowupInput;
                    let _ = self.prompt.handle_key(key);
                    return InputOutcome::Changed;
                }
                InputOutcome::Changed
            }
            PermissionFocus::PatternEdit => {
                if key!('c', CONTROL).matches(key) {
                    return InputOutcome::Action(Action::PermissionCancel);
                }
                let Some(edit) = self.permission_pattern_edit.as_mut() else {
                    perm.focus = PermissionFocus::Options;
                    return InputOutcome::Changed;
                };
                if key.code == KeyCode::Enter {
                    if edit.trimmed().is_some_and(|p| {
                        !echo_agent_workspace::permission::bash_glob_is_catchall(p)
                    }) && let Some(opt) = perm
                        .allow_always_command_idx()
                        .and_then(|idx| perm.options.get(idx))
                    {
                        return InputOutcome::Action(Action::PermissionSelect(
                            opt.option_id.clone(),
                        ));
                    }
                    return InputOutcome::Changed;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let alt = key.modifiers.contains(KeyModifiers::ALT);
                match key.code {
                    KeyCode::Backspace => edit.backspace(),
                    KeyCode::Delete => edit.delete(),
                    KeyCode::Left => edit.move_left(),
                    KeyCode::Right => edit.move_right(),
                    KeyCode::Home => edit.move_home(),
                    KeyCode::End => edit.move_end(),
                    KeyCode::Char('a') if ctrl => edit.move_home(),
                    KeyCode::Char('e') if ctrl => edit.move_end(),
                    KeyCode::Char('u') if ctrl => edit.clear(),
                    KeyCode::Char(c) if !ctrl && !alt => edit.insert_char(c),
                    _ => {}
                }
                InputOutcome::Changed
            }
        }
    }
    pub(super) fn handle_cancel_turn_key(&mut self, key: &KeyEvent) -> InputOutcome {
        if key.code == KeyCode::Esc {
            return self.handle_card_esc();
        }
        let Some(ctv) = self.cancel_turn_view.as_mut() else {
            return InputOutcome::Unchanged;
        };
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.cancel_trigger_hint = Some(crate::app::actions::CancelTrigger::CtrlC);
            return InputOutcome::Action(Action::CancelTurn);
        }
        if let Some(walk) = RowWalk::from_key(key) {
            ctv.active_idx = walk.step(ctv.active_idx, CancelTurnChoice::ALL.len());
            return InputOutcome::Changed;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                ctv.active_idx = (ctv.active_idx + 1).min(CancelTurnChoice::ALL.len() - 1);
                InputOutcome::Changed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                ctv.active_idx = ctv.active_idx.saturating_sub(1);
                InputOutcome::Changed
            }
            KeyCode::Enter => {
                let choice = CancelTurnChoice::ALL[ctv.active_idx];
                InputOutcome::Action(Action::CancelTurnChoice(choice))
            }
            KeyCode::Char(c @ '1'..='4') => {
                let idx = (c as usize) - ('1' as usize);
                let choice = CancelTurnChoice::ALL[idx];
                InputOutcome::Action(Action::CancelTurnChoice(choice))
            }
            _ => InputOutcome::Unchanged,
        }
    }
    /// Mouse handler for the cancel-turn panel. `Moved` moves the
    /// cursor onto the pointed row; `Down(Left)` dispatches the row's
    /// `CancelTurnChoice`. All other events are consumed.
    pub(super) fn handle_cancel_turn_mouse(&mut self, mouse: &MouseEvent) -> InputOutcome {
        if self.cancel_turn_view.is_none() {
            return InputOutcome::Unchanged;
        }
        let hit_idx = self
            .cancel_turn_buttons
            .iter()
            .enumerate()
            .find(|(_, rect)| rect.contains((mouse.column, mouse.row).into()))
            .map(|(idx, _)| idx);
        match mouse.kind {
            MouseEventKind::Moved => {
                let Some(idx) = hit_idx else {
                    return InputOutcome::Unchanged;
                };
                if let Some(ctv) = self.cancel_turn_view.as_mut()
                    && ctv.active_idx != idx
                {
                    ctv.active_idx = idx;
                    return InputOutcome::Changed;
                }
                InputOutcome::Unchanged
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = hit_idx
                    && idx < CancelTurnChoice::ALL.len()
                {
                    if let Some(ctv) = self.cancel_turn_view.as_mut() {
                        ctv.active_idx = idx;
                    }
                    let choice = CancelTurnChoice::ALL[idx];
                    return InputOutcome::Action(Action::CancelTurnChoice(choice));
                }
                InputOutcome::Unchanged
            }
            _ => InputOutcome::Unchanged,
        }
    }
    /// Save the free-text answer the composer is holding and return the card
    /// to its answer rows. A non-blank answer becomes this question's
    /// selection — exclusive with an option row, which single-select clears —
    /// and a blank one is dropped along with the mark.
    pub(super) fn commit_question_freeform(&mut self) {
        use crate::views::question_view::{QuestionFocus, QuestionSelection};
        let text = self.prompt.text().to_string();
        let Some(qv) = self.question_view.as_mut() else {
            return;
        };
        let idx = qv.active_tab;
        let has_text = !text.trim().is_empty();
        if let Some(slot) = qv.per_question_freeform.get_mut(idx) {
            *slot = text;
        }
        if let Some(sel) = qv.per_question_freeform_selected.get_mut(idx) {
            *sel = has_text;
        }
        if has_text {
            if let Some(QuestionSelection::Single(sel)) = qv.selections.get_mut(idx) {
                *sel = None;
            }
        } else if let Some(slot) = qv.per_question_freeform.get_mut(idx) {
            slot.clear();
        }
        if !qv.is_feedback_report() {
            qv.focus = QuestionFocus::Navigation;
        }
        self.last_prompt_click_ms = None;
    }
    /// Handle key input when the question view is active.
    ///
    /// Two modes:
    /// - **Navigation**: j/k move the cursor between answers and Tab/Shift+Tab
    ///   walk the same rows in a loop, Space toggles, Enter advances or edits
    ///   freeform, h/l/[/] cycle questions, 1-9/a-f jump+toggle, Esc unselects,
    ///   Shift-X kills the question tool.
    /// - **InputMode**: all keys go to the prompt widget; Esc exits input mode.
    pub(super) fn handle_question_key(&mut self, key: &KeyEvent) -> InputOutcome {
        use crate::views::question_view::{CursorMotion, QuestionFocus};
        if key.code == KeyCode::Esc {
            return self.handle_card_esc();
        }
        let Some(ref mut qv) = self.question_view else {
            return InputOutcome::Unchanged;
        };
        match qv.focus {
            QuestionFocus::InputMode => {
                if key!('f', CONTROL).matches(key) {
                    qv.fullscreen = !qv.fullscreen;
                    return InputOutcome::Changed;
                }
                if key!('y', CONTROL).matches(key) {
                    return self.dismiss_question_view();
                }
                if key!('c', CONTROL).matches(key) {
                    if qv.is_feedback_report() {
                        return self.clear_feedback_then_dismiss();
                    }
                    qv.focus = QuestionFocus::Navigation;
                    self.last_prompt_click_ms = None;
                    return InputOutcome::Changed;
                }
                if qv.is_feedback_report() && crate::input::key::is_paste_key(key) {
                    let clipboard_text = crate::app::actions::ClipboardTextRead::from_result(
                        crate::clipboard::system_clipboard_read_text(),
                    );
                    return self.handle_paste_key_deferred(clipboard_text);
                }
                match self.prompt.route_enter(key) {
                    EnterOutcome::NewlineInserted => {
                        return InputOutcome::Changed;
                    }
                    EnterOutcome::Submit => {
                        if self.paste_probe_in_flight > 0
                            && self.question_view.as_ref().is_some_and(
                                crate::views::question_view::QuestionViewState::is_feedback_report,
                            )
                        {
                            self.deferred_send =
                                Some(crate::app::agent_view::AgentDeferredSend::SubmitFeedback);
                            return InputOutcome::Changed;
                        }
                        self.commit_question_freeform();
                        if self
                            .question_view
                            .as_ref()
                            .is_some_and(|qv| qv.is_feedback_report())
                        {
                            return self.submit_question_answers(false);
                        }
                        let on_last = self
                            .question_view
                            .as_ref()
                            .is_none_or(|qv| qv.active_tab >= qv.questions.len().saturating_sub(1));
                        if on_last {
                            return self.submit_question_answers(false);
                        }
                        self.swap_question_freeform();
                        if let Some(ref mut qv) = self.question_view {
                            qv.next_question();
                        }
                        self.load_question_freeform();
                        self.ensure_question_cursor_visible();
                        return InputOutcome::Changed;
                    }
                    EnterOutcome::PassThrough => {}
                }
                match self.prompt.handle_key(key) {
                    PromptEvent::Edited => {
                        if let Some(req) = self.prompt.pending_viewer_request.take() {
                            self.open_line_viewer(&req.path, req.initial_range);
                        }
                        InputOutcome::Changed
                    }
                    PromptEvent::Ignored => InputOutcome::Changed,
                }
            }
            QuestionFocus::Navigation => {
                if key!('y', CONTROL).matches(key) {
                    return self.dismiss_question_view();
                }
                if key!('c', CONTROL).matches(key) {
                    return self.submit_question_answers(true);
                }
                if key!('f', CONTROL).matches(key) {
                    qv.fullscreen = !qv.fullscreen;
                    return InputOutcome::Changed;
                }
                let mut needs_scroll_update = false;
                let mut needs_switch_question: Option<QuestionSwitch> = None;
                if qv.is_on_freeform_row()
                    && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                    && matches!(key.code, KeyCode::Char(c) if c != ' ')
                {
                    let text = qv.activate_freeform_input();
                    self.prompt.set_text_preserving(&text);
                    let _ = self.prompt.handle_key(key);
                    return InputOutcome::Changed;
                }
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL =>
                    {
                        qv.move_cursor(CursorMotion::Next);
                        needs_scroll_update = true;
                    }
                    KeyCode::Char('k') | KeyCode::Up
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL =>
                    {
                        qv.move_cursor(CursorMotion::Prev);
                        needs_scroll_update = true;
                    }
                    KeyCode::Char('d') if key.modifiers == KeyModifiers::CONTROL => {
                        qv.move_cursor(CursorMotion::HalfPageDown);
                        needs_scroll_update = true;
                    }
                    KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                        qv.move_cursor(CursorMotion::HalfPageUp);
                        needs_scroll_update = true;
                    }
                    KeyCode::PageDown => {
                        qv.move_cursor(CursorMotion::PageDown);
                        needs_scroll_update = true;
                    }
                    KeyCode::PageUp => {
                        qv.move_cursor(CursorMotion::PageUp);
                        needs_scroll_update = true;
                    }
                    KeyCode::Char('g') if key.modifiers.is_empty() => {
                        qv.move_cursor(CursorMotion::First);
                        needs_scroll_update = true;
                    }
                    KeyCode::Char('G') if key.modifiers == KeyModifiers::SHIFT => {
                        qv.move_cursor(CursorMotion::Last);
                        needs_scroll_update = true;
                    }
                    KeyCode::Char(' ') => {
                        if qv.is_on_freeform_row() {
                            let text = qv.activate_freeform_input();
                            self.prompt.set_text_preserving(&text);
                        } else {
                            let active = qv.active_tab;
                            let cursor = qv.cursor();
                            qv.toggle_option(active, cursor);
                            if matches!(
                                qv.selections.get(active),
                                Some(crate::views::question_view::QuestionSelection::Single(
                                    Some(_)
                                ))
                            ) && let Some(sel) =
                                qv.per_question_freeform_selected.get_mut(active)
                            {
                                *sel = false;
                            }
                        }
                    }
                    KeyCode::Enter => {
                        if qv.is_on_freeform_row() {
                            let text = qv.activate_freeform_input();
                            self.prompt.set_text_preserving(&text);
                        } else {
                            let cursor = qv.cursor();
                            let active = qv.active_tab;
                            qv.select_option(active, cursor);
                            if let Some(sel) = qv.per_question_freeform_selected.get_mut(active) {
                                *sel = false;
                            }
                            let last = qv.questions.len().saturating_sub(1);
                            if qv.active_tab < last {
                                needs_switch_question = Some(QuestionSwitch::Next);
                            } else {
                                return self.submit_question_answers(false);
                            }
                        }
                    }
                    KeyCode::Char('z') if key.modifiers.is_empty() => {
                        if !qv.no_freeform {
                            let freeform_idx = qv.total_items(qv.active_tab).saturating_sub(1);
                            qv.set_cursor(freeform_idx);
                            let text = qv.activate_freeform_input();
                            self.prompt.set_text_preserving(&text);
                        }
                    }
                    KeyCode::Char('l') | KeyCode::Char(']') | KeyCode::Right
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL =>
                    {
                        if qv.questions.len() > 1 {
                            needs_switch_question = Some(QuestionSwitch::Next);
                        }
                    }
                    KeyCode::Char('h') | KeyCode::Char('[') | KeyCode::Left
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL =>
                    {
                        if qv.questions.len() > 1 {
                            needs_switch_question = Some(QuestionSwitch::Prev);
                        }
                    }
                    KeyCode::Char(c)
                        if key.modifiers.is_empty()
                            && crate::views::question_view::option_index_for_key(c).is_some() =>
                    {
                        let idx = crate::views::question_view::option_index_for_key(c).unwrap();
                        let active = qv.active_tab;
                        let opt_count = qv
                            .questions
                            .get(active)
                            .map(|q| q.options.len())
                            .unwrap_or(0);
                        if idx < opt_count {
                            qv.set_cursor(idx);
                            qv.select_option(active, idx);
                            if let Some(sel) = qv.per_question_freeform_selected.get_mut(active) {
                                *sel = false;
                            }
                            let last = qv.questions.len().saturating_sub(1);
                            if qv.active_tab < last {
                                needs_switch_question = Some(QuestionSwitch::Next);
                            } else {
                                return self.submit_question_answers(false);
                            }
                        }
                    }
                    KeyCode::Char('y') if key.modifiers.is_empty() => {
                        if !qv.is_on_freeform_row() {
                            let cursor = qv.cursor();
                            let active = qv.active_tab;
                            if let Some(question) = qv.questions.get(active)
                                && let Some(option) = question.options.get(cursor)
                            {
                                let mut text =
                                    crate::views::question_view::normalize_label(&option.label);
                                if !option.description.is_empty() {
                                    text.push('\n');
                                    text.push_str(&option.description);
                                }
                                self.copy_to_clipboard(&text);
                            }
                        }
                    }
                    KeyCode::Tab | KeyCode::BackTab => {
                        if let Some(walk) = RowWalk::from_key(key) {
                            needs_scroll_update = true;
                            qv.walk_cursor(walk);
                        }
                    }
                    KeyCode::Char('X') if key.modifiers == KeyModifiers::SHIFT => {
                        return self.submit_question_answers(true);
                    }
                    _ => {}
                }
                if let Some(switch) = needs_switch_question {
                    self.last_question_click = None;
                    self.swap_question_freeform();
                    if let Some(ref mut qv) = self.question_view {
                        match switch {
                            QuestionSwitch::Next => qv.next_question(),
                            QuestionSwitch::Prev => qv.prev_question(),
                        }
                    }
                    self.load_question_freeform();
                    needs_scroll_update = true;
                }
                if needs_scroll_update {
                    self.ensure_question_cursor_visible();
                }
                InputOutcome::Changed
            }
        }
    }
    /// The feedback pane has no navigation to return to, so it follows the composer: clear the report, then dismiss once it is empty.
    fn clear_feedback_then_dismiss(&mut self) -> InputOutcome {
        if self.prompt.text().trim().is_empty() {
            return self.submit_question_answers(true);
        }
        self.prompt.set_text("");
        self.commit_question_freeform();
        InputOutcome::Changed
    }
    /// Handle mouse events when the question view is active.
    ///
    /// Scroll wheel scrolls the options list. Clicks on option rows move
    /// cursor and toggle/select. Everything else is consumed (modal-ish).
    pub(super) fn handle_question_mouse(&mut self, mouse: &MouseEvent) -> InputOutcome {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            for &(key_ch, rect) in &self.question_nav_buttons {
                if rect.contains((mouse.column, mouse.row).into()) {
                    if self.question_view.as_ref().is_some_and(|qv| {
                        qv.focus == crate::views::question_view::QuestionFocus::InputMode
                    }) {
                        self.commit_question_freeform();
                    }
                    let key_event = KeyEvent::new(
                        if key_ch == '\n' {
                            KeyCode::Enter
                        } else {
                            KeyCode::Char(key_ch)
                        },
                        KeyModifiers::NONE,
                    );
                    return self.handle_question_key(&key_event);
                }
            }
        }
        let Some(ref mut qv) = self.question_view else {
            return InputOutcome::Changed;
        };
        match mouse.kind {
            MouseEventKind::Moved => {
                let item = self.question_item_at(mouse.column, mouse.row);
                let btn = self
                    .question_nav_buttons
                    .iter()
                    .find(|(_, rect)| rect.contains((mouse.column, mouse.row).into()))
                    .map(|(ch, _)| *ch);
                let changed =
                    item != self.hovered_question_item || btn != self.hovered_question_button;
                self.hovered_question_item = item;
                self.hovered_question_button = btn;
                if changed {
                    InputOutcome::Changed
                } else {
                    InputOutcome::Unchanged
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let delta: i32 = if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                    1
                } else {
                    -1
                };
                let over_inline = qv.focus == crate::views::question_view::QuestionFocus::InputMode
                    && self
                        .inline_prompt_area
                        .is_some_and(|r| r.contains((mouse.column, mouse.row).into()));
                if over_inline {
                    let event = MouseEvent {
                        kind: mouse.kind,
                        column: mouse.column,
                        row: mouse.row,
                        modifiers: mouse.modifiers,
                    };
                    let _ = self.prompt.handle_mouse(&event);
                } else if let Some((scroll_top, scroll_bottom)) = self.question_scroll_region
                    && mouse.row >= scroll_top
                    && mouse.row < scroll_bottom
                {
                    self.apply_question_scroll(delta);
                }
                InputOutcome::Changed
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.last_click = None;
                self.last_text_click = None;
                self.pending_scrollback_click = None;
                if self
                    .hit_question_scrollbar
                    .contains(mouse.column, mouse.row)
                {
                    self.question_scrollbar_dragging = true;
                    self.apply_question_scrollbar_click(mouse.row);
                    return InputOutcome::Changed;
                }
                if self.question_view.as_ref().is_some_and(|q| {
                    q.focus == crate::views::question_view::QuestionFocus::InputMode
                }) {
                    if self
                        .prompt
                        .textarea_area()
                        .contains((mouse.column, mouse.row).into())
                    {
                        let _ = self.prompt.handle_mouse(mouse);
                        if self.prompt_click_is_double()
                            && self.prompt.expand_paste_element_at_cursor()
                        {
                            self.prompt.refresh_slash(&self.session.models);
                        }
                        return InputOutcome::Changed;
                    }
                    self.commit_question_freeform();
                }
                let Some(qv) = self.question_view.as_mut() else {
                    return InputOutcome::Changed;
                };
                let prompt_area = self.pane_areas.prompt;
                let footer_h = 3u16;
                let question_area_bottom =
                    prompt_area.y + prompt_area.height.saturating_sub(footer_h);
                let sticky_freeform_y = question_area_bottom.saturating_sub(1);
                let on_sticky_freeform = !qv.no_freeform
                    && qv.focus != crate::views::question_view::QuestionFocus::InputMode
                    && mouse.row == sticky_freeform_y
                    && mouse.column >= prompt_area.x
                    && mouse.column < prompt_area.x + prompt_area.width;
                if on_sticky_freeform {
                    let freeform_idx = qv
                        .questions
                        .get(qv.active_tab)
                        .map(|q| q.options.len())
                        .unwrap_or(0);
                    let active_tab = qv.active_tab;
                    qv.set_cursor(freeform_idx);
                    let was_selected = qv
                        .per_question_freeform_selected
                        .get(active_tab)
                        .copied()
                        .unwrap_or(false);
                    if was_selected {
                        if let Some(sel) = qv.per_question_freeform_selected.get_mut(active_tab) {
                            *sel = false;
                        }
                    } else {
                        if let Some(sel) = qv.per_question_freeform_selected.get_mut(active_tab) {
                            *sel = true;
                        }
                        if let Some(crate::views::question_view::QuestionSelection::Single(sel)) =
                            qv.selections.get_mut(active_tab)
                        {
                            *sel = None;
                        }
                        let text = qv
                            .per_question_freeform
                            .get(active_tab)
                            .cloned()
                            .unwrap_or_default();
                        self.prompt.set_text_preserving(&text);
                        qv.focus = crate::views::question_view::QuestionFocus::InputMode;
                    }
                    return InputOutcome::Changed;
                }
                let hit_idx = qv.questions.get(qv.active_tab).and_then(|question| {
                    let content_w = prompt_area.width.saturating_sub(QUESTION_VIEW_HPAD) as usize;
                    let scroll = qv
                        .per_question_scroll
                        .get(qv.active_tab)
                        .copied()
                        .unwrap_or(0);
                    crate::views::question_view::item_index_at_screen_row(
                        question,
                        prompt_area,
                        content_w,
                        scroll,
                        mouse.row,
                        qv.focused_preview(),
                        qv.fullscreen,
                        qv.cached_desc_cap,
                        qv.cached_preview_cap,
                        qv.cursor(),
                    )
                });
                let option_count = qv
                    .questions
                    .get(qv.active_tab)
                    .map(|question| question.options.len());
                if let Some((idx, option_count)) = hit_idx.zip(option_count) {
                    if qv.no_freeform && idx >= option_count {
                        return InputOutcome::Changed;
                    }
                    let active_tab = qv.active_tab;
                    qv.set_cursor(idx);
                    if idx < option_count {
                        let now = Instant::now();
                        let is_double_click =
                            self.last_question_click.is_some_and(|(t, prev_idx)| {
                                prev_idx == idx
                                    && now.duration_since(t).as_millis() < MULTI_CLICK_TIMEOUT_MS
                            });
                        if is_double_click {
                            self.last_question_click = None;
                            qv.select_option(active_tab, idx);
                            if let Some(sel) = qv.per_question_freeform_selected.get_mut(active_tab)
                            {
                                *sel = false;
                            }
                            let last = qv.questions.len().saturating_sub(1);
                            if qv.active_tab < last {
                                self.swap_question_freeform();
                                if let Some(ref mut qv) = self.question_view {
                                    qv.next_question();
                                }
                                self.load_question_freeform();
                                self.ensure_question_cursor_visible();
                                return InputOutcome::Changed;
                            } else {
                                return self.submit_question_answers(false);
                            }
                        }
                        self.last_question_click = Some((now, idx));
                        qv.toggle_option(active_tab, idx);
                        if matches!(
                            qv.selections.get(active_tab),
                            Some(crate::views::question_view::QuestionSelection::Single(
                                Some(_)
                            ))
                        ) && let Some(sel) =
                            qv.per_question_freeform_selected.get_mut(active_tab)
                        {
                            *sel = false;
                        }
                    } else {
                        self.last_question_click = None;
                        use crate::views::question_view::QuestionFocus;
                        let tab = qv.active_tab;
                        let text = qv
                            .per_question_freeform
                            .get(tab)
                            .cloned()
                            .unwrap_or_default();
                        self.prompt.set_text_preserving(&text);
                        qv.focus = QuestionFocus::InputMode;
                    }
                }
                InputOutcome::Changed
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.question_scrollbar_dragging {
                    self.apply_question_scrollbar_click(mouse.row);
                } else if qv.focus == crate::views::question_view::QuestionFocus::InputMode {
                    let _ = self.prompt.handle_mouse(mouse);
                }
                InputOutcome::Changed
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.question_scrollbar_dragging = false;
                if qv.focus == crate::views::question_view::QuestionFocus::InputMode {
                    let _ = self.prompt.handle_mouse(mouse);
                }
                InputOutcome::Changed
            }
            _ => InputOutcome::Changed,
        }
    }
    /// Apply a scroll delta to the question view options.
    pub(super) fn apply_question_scroll(&mut self, delta: i32) {
        let Some(ref mut qv) = self.question_view else {
            return;
        };
        let Some(question) = qv.questions.get(qv.active_tab) else {
            return;
        };
        let content_w = self
            .pane_areas
            .prompt
            .width
            .saturating_sub(QUESTION_VIEW_HPAD) as usize;
        let visible_h = question_visible_h(
            self.question_scroll_region,
            self.pane_areas.prompt.height,
            question,
            content_w,
            qv.focused_preview(),
            qv.fullscreen,
            qv.cached_desc_cap,
            qv.cached_preview_cap,
            1 - qv.phantom_freeform_h(),
        );
        let current_scroll = qv
            .per_question_scroll
            .get(qv.active_tab)
            .copied()
            .unwrap_or(0);
        let new_scroll = crate::views::question_view::scroll_offset_for_item_delta(
            question,
            content_w,
            current_scroll,
            delta,
            visible_h,
            qv.cursor(),
            qv.phantom_freeform_h(),
        );
        if let Some(s) = qv.per_question_scroll.get_mut(qv.active_tab) {
            *s = new_scroll;
        }
    }
    /// Apply a scrollbar click/drag at the given screen row for the question view.
    ///
    /// Uses [`scrollbar_click_to_offset`] (same math as `list_pane` and the
    /// scrollback scrollbar) so click/drag is the exact inverse of the thumb.
    fn apply_question_scrollbar_click(&mut self, screen_y: u16) {
        use crate::render::scrollbar::{ScrollbarClickResult, scrollbar_click_to_offset};
        let Some(sb) = self.hit_question_scrollbar.rect else {
            return;
        };
        if sb.height == 0 {
            return;
        }
        let content_w = self
            .pane_areas
            .prompt
            .width
            .saturating_sub(QUESTION_VIEW_HPAD) as usize;
        let Some(ref mut qv) = self.question_view else {
            return;
        };
        let Some(question) = qv.questions.get(qv.active_tab) else {
            return;
        };
        let total_h =
            crate::views::question_view::total_options_height(question, content_w, qv.cursor())
                .saturating_sub(qv.phantom_freeform_h());
        let visible_h = question_visible_h(
            self.question_scroll_region,
            self.pane_areas.prompt.height,
            question,
            content_w,
            qv.focused_preview(),
            qv.fullscreen,
            qv.cached_desc_cap,
            qv.cached_preview_cap,
            1 - qv.phantom_freeform_h(),
        );
        let cell_index = screen_y.saturating_sub(sb.y);
        let result = scrollbar_click_to_offset(cell_index, sb.height, total_h, visible_h);
        let max_scroll = total_h.saturating_sub(visible_h);
        if let Some(s) = qv.per_question_scroll.get_mut(qv.active_tab) {
            match result {
                ScrollbarClickResult::Top => *s = 0,
                ScrollbarClickResult::Bottom => *s = max_scroll,
                ScrollbarClickResult::Offset(offset) => {
                    *s = (offset as u16).min(max_scroll);
                }
            }
        }
    }
    fn ensure_question_cursor_visible(&mut self) {
        let content_w = self
            .pane_areas
            .prompt
            .width
            .saturating_sub(QUESTION_VIEW_HPAD) as usize;
        let Some(ref mut qv) = self.question_view else {
            return;
        };
        let Some(question) = qv.questions.get(qv.active_tab) else {
            return;
        };
        let visible_h = question_visible_h(
            self.question_scroll_region,
            self.pane_areas.prompt.height,
            question,
            content_w,
            qv.focused_preview(),
            qv.fullscreen,
            qv.cached_desc_cap,
            qv.cached_preview_cap,
            1 - qv.phantom_freeform_h(),
        );
        qv.ensure_cursor_visible(visible_h, content_w);
    }
    fn question_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let qv = self.question_view.as_ref()?;
        let prompt_area = self.pane_areas.prompt;
        if prompt_area.area() == 0 || !prompt_area.contains((col, row).into()) {
            return None;
        }
        let question = qv.questions.get(qv.active_tab)?;
        let content_w = prompt_area.width.saturating_sub(QUESTION_VIEW_HPAD) as usize;
        let scroll = qv
            .per_question_scroll
            .get(qv.active_tab)
            .copied()
            .unwrap_or(0);
        if let Some((scroll_top, scroll_bottom)) = self.question_scroll_region {
            if row >= scroll_top && row < scroll_bottom {
                let visual_line = (row - scroll_top) + scroll;
                let options_only_h: u16 = crate::views::question_view::total_options_height(
                    question,
                    content_w,
                    qv.cursor(),
                )
                .saturating_sub(1);
                if visual_line >= options_only_h {
                    return None;
                }
                return Some(crate::views::question_view::item_index_at_visual_line(
                    question,
                    content_w,
                    visual_line,
                    qv.cursor(),
                ));
            }
            let is_input_mode = qv.focus == crate::views::question_view::QuestionFocus::InputMode;
            if !is_input_mode && !qv.no_freeform && row == scroll_bottom {
                return Some(question.options.len());
            }
            return None;
        }
        crate::views::question_view::item_index_at_screen_row(
            question,
            prompt_area,
            content_w,
            scroll,
            row,
            qv.focused_preview(),
            qv.fullscreen,
            qv.cached_desc_cap,
            qv.cached_preview_cap,
            qv.cursor(),
        )
        .filter(|&idx| !qv.no_freeform || idx < question.options.len())
    }
    /// Save the current prompt text into `per_question_freeform[active_tab]`
    /// and load the text for the new `active_tab` into the prompt widget.
    /// Call this BEFORE changing `active_tab`.
    fn swap_question_freeform(&mut self) {
        let Some(ref mut qv) = self.question_view else {
            return;
        };
        let old = qv.active_tab;
        if let Some(slot) = qv.per_question_freeform.get_mut(old) {
            *slot = self.prompt.text().to_string();
        }
    }
    /// Load the freeform text for the current `active_tab` into the prompt.
    /// Call this AFTER changing `active_tab`.
    fn load_question_freeform(&mut self) {
        let Some(ref qv) = self.question_view else {
            return;
        };
        let new_text = qv
            .per_question_freeform
            .get(qv.active_tab)
            .map(|s| s.as_str())
            .unwrap_or("");
        self.prompt.set_text_preserving(new_text);
    }
    /// Dismiss (hide) the question view without submitting answers.
    ///
    /// Restores the original prompt text that was stashed when the question
    /// view opened, so typed "additional context" doesn't leak into the
    /// main prompt. Also clears any stashed (tab-hidden) question view.
    fn dismiss_question_view(&mut self) -> InputOutcome {
        let follows_skip_submit = self.question_view.as_ref().is_some_and(|qv| {
            matches!(
                qv.local_kind,
                Some(crate::views::question_view::LocalQuestionKind::DoctorFix { .. })
                    | Some(crate::views::question_view::LocalQuestionKind::FeedbackTrace { .. })
            )
        });
        if follows_skip_submit {
            return self.submit_question_answers(true);
        }
        if let Some(qv) = self.question_view.take() {
            self.record_question_pause(&qv);
            self.restore_card_prompt(qv.stashed_prompt);
        }
        self.cleanup_question_state();
        InputOutcome::Changed
    }
    /// Retract an interaction modal (permission / question / plan-approval) that
    /// another connected client already resolved.
    ///
    /// In a shared (leader-hosted) session the agent broadcasts the interactive
    /// reverse-request to every pane and resolves first-answer-wins; when any
    /// pane answers, the agent broadcasts `InteractionResolved{tool_call_id}` and
    /// every other pane calls this to drop its copy. Returns `true` if a modal
    /// was dismissed (so the caller redraws). Idempotent: a `tool_call_id` this
    /// pane isn't showing is a silent no-op — including on the pane that
    /// answered, which already cleared its own modal locally. Dropping a
    /// dismissed modal's `response_tx` is harmless: the agent has already
    /// resolved, so any late response for that id is ignored by its gateway.
    pub(crate) fn dismiss_resolved_interaction(&mut self, tool_call_id: &str) -> bool {
        if self
            .question_view
            .as_ref()
            .is_some_and(|qv| qv.tool_call_id == tool_call_id)
        {
            let _ = self.dismiss_question_view();
            return true;
        }
        if let Some(ev) = self.elicitation_view.as_ref()
            && ev.tool_call_id == tool_call_id
        {
            if ev.is_url_waiting() {
                return false;
            }
            if let Some(mut ev) = self.elicitation_view.take() {
                let _ = ev.take_response_tx();
                self.restore_elicitation_prompt(ev.stashed_prompt);
            }
            return true;
        }
        if self
            .pending_elicitation
            .as_ref()
            .is_some_and(|(req, _)| req.tool_call_id == tool_call_id)
        {
            if let Some((_, tx)) = self.pending_elicitation.take() {
                drop(tx);
            }
            return true;
        }
        if self
            .plan_approval_view
            .as_ref()
            .is_some_and(|pav| pav.tool_call_id == tool_call_id)
        {
            let mut pav = self
                .plan_approval_view
                .take()
                .expect("plan_approval_view is Some (just checked)");
            pav.send_stale_cancel();
            self.latest_inline_plan_content = None;
            self.plan_next_comment_id = pav.next_comment_id;
            self.prompt.restore(pav.stashed_prompt);
            self.line_viewer = None;
            self.casual_commenting_range = None;
            self.casual_editing_comment_id = None;
            return true;
        }
        if let Some(pos) = self
            .permission_queue
            .iter()
            .position(|p| p.request.request.tool_call.tool_call_id.0.as_ref() == tool_call_id)
        {
            let was_front = pos == 0;
            let _ = self.permission_queue.remove(pos);
            if was_front {
                super::dispatch::resolve_permission_queue_transition(self);
            }
            return true;
        }
        false
    }
    /// Test-only access to [`submit_question_answers`] so dispatch tests
    /// can verify the full submit/cancel pipeline (including
    /// `prompt.restore` and `cleanup_question_state`) for local
    /// questions, not just the inner `translate_local_submit` shim.
    #[cfg(test)]
    pub(crate) fn submit_question_answers_for_test(&mut self, skipped: bool) -> InputOutcome {
        self.submit_question_answers(skipped)
    }
    #[cfg(test)]
    pub(crate) fn handle_question_key_for_test(&mut self, key: &KeyEvent) -> InputOutcome {
        self.handle_question_key(key)
    }
    #[cfg(test)]
    pub(crate) fn handle_question_mouse_for_test(&mut self, mouse: &MouseEvent) -> InputOutcome {
        self.handle_question_mouse(mouse)
    }
    /// Give back the draft a card displaced when it opened.
    ///
    /// Permission open: write into `permission_stashed_prompt`. Question open:
    /// write into its stash — the question owns the live composer as freeform,
    /// and its close puts the stash back (writing through would first clobber
    /// the freeform and then be clobbered by the question's own restore).
    /// Otherwise restore the live composer (plan freeform when approval is
    /// parked is deliberate; the session draft is already on
    /// `plan_approval_view.stashed_prompt`).
    pub(crate) fn restore_card_prompt(
        &mut self,
        stashed: crate::views::prompt_widget::StashedPrompt,
    ) {
        if self.permission_stashed_prompt.is_some() {
            self.permission_stashed_prompt = Some(stashed);
        } else if let Some(qv) = self.question_view.as_mut() {
            qv.stashed_prompt = stashed;
        } else {
            self.prompt.restore(stashed);
        }
    }
    /// Close out the `/feedback` report pane: Enter advances to the trace
    /// question (when offered) or sends, Esc drops the report.
    fn submit_feedback_pane(
        &mut self,
        mut qv: crate::views::question_view::QuestionViewState,
        skipped: bool,
    ) -> InputOutcome {
        let report = qv.feedback_report();
        if !skipped && report.is_empty() && self.prompt.images.is_empty() {
            crate::unified_log::info(
                "feedback.submit",
                None,
                Some(serde_json::json!({"branch": "empty"})),
            );
            let freeform = qv.activate_freeform_input();
            self.prompt.set_text_preserving(&freeform);
            self.question_view = Some(qv);
            return InputOutcome::Changed;
        }
        if !skipped && qv.feedback_offer_trace {
            let images = self.prompt.drain_images();
            crate::unified_log::info(
                "feedback.submit",
                None,
                Some(serde_json::json!({
                    "branch": "trace_question",
                    "chars": report.chars().count(),
                    "images": images.len(),
                })),
            );
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::FeedbackTraceCardShown {
                    reenables_sharing: qv.feedback_offer_reenables_sharing,
                },
            );
            qv.begin_feedback_trace_stage(report, images);
            self.prompt.set_text_preserving("");
            self.question_view = Some(qv);
            return InputOutcome::Changed;
        }
        let images = if skipped {
            Vec::new()
        } else {
            self.prompt.drain_images()
        };
        crate::unified_log::info(
            "feedback.submit",
            None,
            Some(serde_json::json!({
                "branch": "send",
                "skipped": skipped,
                "chars": report.chars().count(),
                "images": images.len(),
            })),
        );
        self.record_question_pause(&qv);
        self.restore_card_prompt(qv.stashed_prompt);
        self.cleanup_question_state();
        if skipped {
            return InputOutcome::Changed;
        }
        InputOutcome::Action(Action::SendFeedback {
            text: report,
            images: images.into(),
            trace: None,
        })
    }
    pub(super) fn submit_question_answers(&mut self, skipped: bool) -> InputOutcome {
        use echo_agent_tools::implementations::echo_agent_build::ask_user_question::AskUserQuestionExtResponse;
        self.swap_question_freeform();
        let Some(mut qv) = self.question_view.take() else {
            return InputOutcome::Changed;
        };
        if qv.is_feedback_report() {
            return self.submit_feedback_pane(qv, skipped);
        }
        self.record_question_pause(&qv);
        if let Some(kind) = qv.local_kind.take() {
            use crate::views::question_view::LocalQuestionKind;
            let outcome = match (skipped, kind) {
                (true, LocalQuestionKind::DoctorFix { target, .. }) => {
                    InputOutcome::Action(Action::DoctorFixCancelled(target))
                }
                (true, LocalQuestionKind::FeedbackTrace { report, images }) => {
                    InputOutcome::Action(Action::SendFeedback {
                        text: report,
                        images,
                        trace: Some(crate::app::actions::FeedbackTraceChoice::NoUpload),
                    })
                }
                (skipped, kind) => translate_local_submit(&qv, kind, skipped),
            };
            self.prompt.restore(qv.stashed_prompt);
            self.cleanup_question_state();
            return outcome;
        }
        let response = if skipped {
            AskUserQuestionExtResponse::Cancelled
        } else {
            qv.build_accepted_response()
        };
        qv.send_ext_response(response);
        self.prompt.restore(qv.stashed_prompt);
        self.cleanup_question_state();
        let action = if skipped {
            "interview_skip"
        } else {
            "interview_submit"
        };
        echo_agent_telemetry::session_ctx::log_event(echo_agent_telemetry::events::PlanSubmit {
            action: action.to_string(),
        });
        InputOutcome::Changed
    }
    /// Map a screen position to a permission option index.
    ///
    /// Uses the prompt area and permission chrome height to determine which
    /// option row the mouse is over. Returns `None` if outside the options.
    pub(super) fn permission_item_at(&self, _col: u16, row: u16) -> Option<usize> {
        let perm = self.permission_queue.front()?;
        let prompt_area = self.pane_areas.prompt;
        if prompt_area.area() == 0 {
            return None;
        }
        let content_w = prompt_area.width.saturating_sub(QUESTION_VIEW_HPAD) as usize;
        let chrome_h = crate::views::permission_view::permission_chrome_height_pub(
            perm,
            content_w,
            prompt_area.height,
        );
        let options_start_y = prompt_area.y + chrome_h;
        if row < options_start_y {
            return None;
        }
        let idx = (row - options_start_y) as usize;
        if idx < perm.options.len() {
            Some(idx)
        } else {
            None
        }
    }
    /// Clean up question-related visual state after the question view is
    /// dismissed (submit, cancel, or replacement).
    pub(crate) fn cleanup_question_state(&mut self) {
        if self.deferred_send == Some(crate::app::agent_view::AgentDeferredSend::SubmitFeedback) {
            self.deferred_send = None;
        }
        self.hovered_question_item = None;
        self.question_scrollbar_dragging = false;
        self.hit_question_scrollbar.clear();
        self.inline_prompt_area = None;
        self.last_question_click = None;
        self.last_prompt_click_ms = None;
    }
    /// Answer the ACTIVE question of this agent's pending
    /// `AskUserQuestion` from the dashboard peek panel.
    ///
    /// Mirrors the agent view's own Enter handling but sources the
    /// freeform text from the peek (a `freeform` argument) instead of
    /// this view's prompt: `option_idx` selects an option; `None` with
    /// non-empty `freeform` records the "Other" free-text answer. When
    /// more questions remain it advances to the next one
    /// ([`PeekAnswerOutcome::Advanced`]); on the last question it builds +
    /// sends the accepted ext-response, restores the stashed prompt, and
    /// clears question state ([`PeekAnswerOutcome::Submitted`]). Only
    /// valid for an ext ask (`None` `local_kind`); an empty "Other" or a
    /// non-ext question is a [`PeekAnswerOutcome::NoOp`].
    pub(crate) fn dashboard_answer_question(
        &mut self,
        option_idx: Option<usize>,
        freeform: String,
    ) -> PeekAnswerOutcome {
        use crate::views::question_view::QuestionSelection;
        let Some(mut qv) = self.question_view.take() else {
            return PeekAnswerOutcome::NoOp;
        };
        if qv.local_kind.is_some() {
            self.question_view = Some(qv);
            return PeekAnswerOutcome::NoOp;
        }
        let active = qv.active_tab;
        match option_idx {
            Some(idx) => {
                qv.select_option(active, idx);
                if let Some(slot) = qv.per_question_freeform_selected.get_mut(active) {
                    *slot = false;
                }
            }
            None => {
                if freeform.trim().is_empty() {
                    self.question_view = Some(qv);
                    return PeekAnswerOutcome::NoOp;
                }
                if let Some(slot) = qv.per_question_freeform.get_mut(active) {
                    *slot = freeform;
                }
                if let Some(slot) = qv.per_question_freeform_selected.get_mut(active) {
                    *slot = true;
                }
                if let Some(QuestionSelection::Single(sel)) = qv.selections.get_mut(active) {
                    *sel = None;
                }
            }
        }
        if active + 1 < qv.questions.len() {
            qv.next_question();
            self.question_view = Some(qv);
            return PeekAnswerOutcome::Advanced;
        }
        self.record_question_pause(&qv);
        let response = qv.build_accepted_response();
        qv.send_ext_response(response);
        self.prompt.restore(qv.stashed_prompt);
        self.cleanup_question_state();
        PeekAnswerOutcome::Submitted
    }
}
#[cfg(test)]
mod cancel_turn_mouse_tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::agent::{AgentId, AgentSession, AgentState};
    use crate::app::app_view::InputOutcome;
    use crate::scrollback::state::ScrollbackState;
    use crate::views::modal::{CancelTurnChoice, CancelTurnViewState};
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    fn make_agent() -> AgentView {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        AgentView::new(
            AgentSession {
                id: AgentId(0),
                acp_tx: tx,
                session_id: None,
                models: ModelState::default(),
                state: AgentState::Idle,
                tracker: crate::acp::tracker::AcpUpdateTracker::new(),
                cwd: std::path::PathBuf::from("/tmp"),
                is_worktree: false,
                forked_from: None,
                pending_prompts: std::collections::VecDeque::new(),
                next_queue_id: 0,
                yolo_mode: false,
                auto_mode: false,
                prompt_history: Vec::new(),
                prompt_history_loading: false,
                loading_replay: false,
                restore_degree: None,
                rate_limited: false,
                model_incompatible: false,
                credit_limit_blocked: false,
                free_usage_blocked: false,
                available_commands: Vec::new(),
                available_commands_generation: 0,
                available_tools: None,
                model_switch_pending: false,
                user_model_preference: None,
                deferred_model_switch: None,
                bg_tasks: std::collections::BTreeMap::new(),
                bg_tool_call_to_task: std::collections::HashMap::new(),
                scheduled_tasks: std::collections::HashMap::new(),
                in_flight_prompt: None,
                compact_held_prompt: None,
                current_prompt_id: None,
                created_via_new: false,
            },
            ScrollbackState::new(),
        )
    }
    /// Panel with one synthetic Rect per choice, stacked at y=10.
    fn setup_panel(agent: &mut AgentView) {
        agent.cancel_turn_view = Some(CancelTurnViewState {
            active_idx: 0,
            running_count: 2,
        });
        agent.cancel_turn_buttons.clear();
        for (i, _) in CancelTurnChoice::ALL.iter().enumerate() {
            agent.cancel_turn_buttons.push(Rect {
                x: 5,
                y: 10 + i as u16,
                width: 40,
                height: 1,
            });
        }
    }
    fn down(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }
    fn moved(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: col,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }
    #[test]
    fn click_first_row_dispatches_stop_running() {
        let mut agent = make_agent();
        setup_panel(&mut agent);
        let outcome = agent.handle_cancel_turn_mouse(&down(10, 10));
        match outcome {
            InputOutcome::Action(Action::CancelTurnChoice(c)) => {
                assert_eq!(c, CancelTurnChoice::StopRunning);
            }
            other => panic!("expected CancelTurnChoice(StopRunning), got {other:?}"),
        }
        assert_eq!(agent.cancel_turn_view.as_ref().unwrap().active_idx, 0);
    }
    #[test]
    fn click_third_row_dispatches_always_stop() {
        let mut agent = make_agent();
        setup_panel(&mut agent);
        let outcome = agent.handle_cancel_turn_mouse(&down(10, 12));
        match outcome {
            InputOutcome::Action(Action::CancelTurnChoice(c)) => {
                assert_eq!(c, CancelTurnChoice::AlwaysStop);
            }
            other => panic!("expected CancelTurnChoice(AlwaysStop), got {other:?}"),
        }
        assert_eq!(agent.cancel_turn_view.as_ref().unwrap().active_idx, 2);
    }
    #[test]
    fn click_outside_rows_consumes_event_without_action() {
        let mut agent = make_agent();
        setup_panel(&mut agent);
        let outcome = agent.handle_cancel_turn_mouse(&down(10, 50));
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(agent.cancel_turn_view.as_ref().unwrap().active_idx, 0);
    }
    #[test]
    fn hover_moves_cursor_to_pointed_row() {
        let mut agent = make_agent();
        setup_panel(&mut agent);
        assert_eq!(agent.cancel_turn_view.as_ref().unwrap().active_idx, 0);
        let outcome = agent.handle_cancel_turn_mouse(&moved(10, 11));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.cancel_turn_view.as_ref().unwrap().active_idx, 1);
        let outcome = agent.handle_cancel_turn_mouse(&moved(10, 13));
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(agent.cancel_turn_view.as_ref().unwrap().active_idx, 3);
        let outcome = agent.handle_cancel_turn_mouse(&moved(15, 13));
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(agent.cancel_turn_view.as_ref().unwrap().active_idx, 3);
        let outcome = agent.handle_cancel_turn_mouse(&moved(10, 50));
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(agent.cancel_turn_view.as_ref().unwrap().active_idx, 3);
    }
    #[test]
    fn mouse_event_ignored_when_panel_closed() {
        let mut agent = make_agent();
        let outcome = agent.handle_cancel_turn_mouse(&down(10, 10));
        assert!(matches!(outcome, InputOutcome::Unchanged));
    }
    #[test]
    fn esc_dismisses_the_panel_without_cancelling_the_turn() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut agent = make_agent();
        agent.session.state = AgentState::TurnRunning;
        setup_panel(&mut agent);
        let outcome =
            agent.handle_cancel_turn_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "panel Esc must keep the turn running, got {outcome:?}"
        );
        assert!(agent.cancel_turn_view.is_none());
        assert!(agent.session.state.is_turn_running());
    }
}
#[cfg(test)]
mod permission_mouse_tests {
    use super::test_fixtures::make_agent;
    use super::*;
    use crate::app::app_view::InputOutcome;
    use agent_client_protocol as acp;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    use std::sync::Arc;
    use std::time::Duration;
    const OPTIONS_START_Y: u16 = 23;
    fn option(id: &str, kind: acp::PermissionOptionKind) -> acp::PermissionOption {
        acp::PermissionOption::new(
            acp::PermissionOptionId::new(Arc::from(id)),
            id.to_string(),
            kind,
        )
    }
    fn setup_permission(agent: &mut AgentView) {
        let mut perm = super::test_fixtures::make_followup_permission_state();
        perm.focus = crate::views::permission_view::PermissionFocus::Options;
        perm.options = vec![
            option("opt-allow-once", acp::PermissionOptionKind::AllowOnce),
            option("opt-allow-always", acp::PermissionOptionKind::AllowAlways),
            option("opt-reject-once", acp::PermissionOptionKind::RejectOnce),
        ];
        agent.permission_queue.push_back(perm);
        agent.pane_areas.prompt = Rect::new(0, 20, 80, 10);
        assert_eq!(agent.permission_item_at(10, OPTIONS_START_Y), Some(0));
    }
    /// Option-row hit targets track the planned-args rows in both toggle
    /// states (hit-testing and render share the row-budget fn).
    #[test]
    fn permission_item_at_tracks_args_rows_collapsed_and_expanded() {
        let mut agent = make_agent();
        setup_permission(&mut agent);
        agent.pane_areas.prompt = Rect::new(0, 20, 80, 20);
        agent.permission_queue.front_mut().unwrap().description =
            (0..10).map(|i| format!("\"k{i}\": {i},")).collect();
        assert_eq!(agent.permission_item_at(10, 27), None, "chrome row");
        assert_eq!(agent.permission_item_at(10, 28), Some(0));
        assert_eq!(agent.permission_item_at(10, 30), Some(2));
        agent.permission_queue.front_mut().unwrap().args_expanded = true;
        assert_eq!(agent.permission_item_at(10, 28), None, "now a chrome row");
        assert_eq!(agent.permission_item_at(10, 33), Some(0));
        assert_eq!(agent.permission_item_at(10, 35), Some(2));
    }
    fn click_at(agent: &mut AgentView, registry: &ActionRegistry, row: u16) -> InputOutcome {
        agent.handle_input(
            &Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 10,
                row,
                modifiers: crossterm::event::KeyModifiers::empty(),
            }),
            registry,
        )
    }
    fn click_row(agent: &mut AgentView, registry: &ActionRegistry, idx: u16) -> InputOutcome {
        click_at(agent, registry, OPTIONS_START_Y + idx)
    }
    #[test]
    fn double_click_on_permission_row_submits_that_row() {
        let mut agent = make_agent();
        setup_permission(&mut agent);
        let registry = ActionRegistry::defaults();
        let first = click_row(&mut agent, &registry, 1);
        assert!(matches!(first, InputOutcome::Changed));
        assert_eq!(agent.permission_queue.front().unwrap().active_idx, 1);
        let second = click_row(&mut agent, &registry, 1);
        match second {
            InputOutcome::Action(Action::PermissionSelect(id)) => {
                assert_eq!(id.0.as_ref(), "opt-allow-always");
            }
            other => panic!("expected PermissionSelect, got {other:?}"),
        }
        assert!(agent.last_permission_click.is_none());
    }
    #[test]
    fn clicks_on_two_different_rows_select_but_do_not_submit() {
        let mut agent = make_agent();
        setup_permission(&mut agent);
        let registry = ActionRegistry::defaults();
        let first = click_row(&mut agent, &registry, 0);
        assert!(matches!(first, InputOutcome::Changed));
        assert_eq!(agent.permission_queue.front().unwrap().active_idx, 0);
        let second = click_row(&mut agent, &registry, 1);
        assert!(
            matches!(second, InputOutcome::Changed),
            "click on a different row must select, not submit; got {second:?}"
        );
        assert_eq!(agent.permission_queue.front().unwrap().active_idx, 1);
        assert!(matches!(agent.last_permission_click, Some((_, 1))));
    }
    #[test]
    fn slow_second_click_on_same_row_does_not_submit() {
        let mut agent = make_agent();
        setup_permission(&mut agent);
        let registry = ActionRegistry::defaults();
        let first = click_row(&mut agent, &registry, 1);
        assert!(matches!(first, InputOutcome::Changed));
        let stale = Instant::now() - Duration::from_millis(MULTI_CLICK_TIMEOUT_MS as u64 + 50);
        agent.last_permission_click = Some((stale, 1));
        let second = click_row(&mut agent, &registry, 1);
        assert!(
            matches!(second, InputOutcome::Changed),
            "second click after the double-click window must not submit; got {second:?}"
        );
        assert_eq!(agent.permission_queue.front().unwrap().active_idx, 1);
        assert!(matches!(agent.last_permission_click, Some((_, 1))));
    }
    #[test]
    fn click_on_chrome_between_row_clicks_does_not_submit() {
        let mut agent = make_agent();
        setup_permission(&mut agent);
        let registry = ActionRegistry::defaults();
        let first = click_row(&mut agent, &registry, 1);
        assert!(matches!(first, InputOutcome::Changed));
        let chrome = click_at(&mut agent, &registry, OPTIONS_START_Y - 1);
        assert!(matches!(chrome, InputOutcome::Changed));
        assert!(agent.last_permission_click.is_none());
        let third = click_row(&mut agent, &registry, 1);
        assert!(
            matches!(third, InputOutcome::Changed),
            "row click after an intervening chrome click must not submit; got {third:?}"
        );
        assert!(matches!(agent.last_permission_click, Some((_, 1))));
    }
}
#[cfg(test)]
mod permission_scope_key_tests {
    use super::test_fixtures::make_agent;
    use super::*;
    use agent_client_protocol as acp;
    use std::sync::Arc;
    fn option(id: &str, kind: acp::PermissionOptionKind) -> acp::PermissionOption {
        acp::PermissionOption::new(
            acp::PermissionOptionId::new(Arc::from(id)),
            id.to_string(),
            kind,
        )
    }
    /// Bash permission with both scoped rows and a 3-word primary command,
    /// mirroring the prompter's `[allow-always, once, reject, reject-always]`.
    fn setup_bash_permission(agent: &mut AgentView) {
        let mut perm = super::test_fixtures::make_followup_permission_state();
        perm.focus = crate::views::permission_view::PermissionFocus::Options;
        perm.options = vec![
            option(
                "allow-always-command",
                acp::PermissionOptionKind::AllowAlways,
            ),
            option("allow-once", acp::PermissionOptionKind::AllowOnce),
            option("reject-once", acp::PermissionOptionKind::RejectOnce),
            option(
                "reject-always-command",
                acp::PermissionOptionKind::RejectAlways,
            ),
        ];
        perm.bash_highlights = Some(
            echo_agent_workspace::permission::bash_command_splitting::BashCommandHighlights {
                prefix: vec![],
                highlighted_words: vec!["cargo".into(), "test".into(), "--workspace".into()],
                suffix: vec![],
            },
        );
        perm.bash_selection_count = 2;
        perm.bash_deny_selection_count = 2;
        agent.permission_queue.push_back(perm);
    }
    /// ←/→ on the "Never allow" (RejectAlways) row must adjust the scope in
    /// place — never yank the cursor onto the AllowAlways row, where Enter
    /// would persist a whitelist for the words the user was narrowing a deny
    /// for.
    #[test]
    fn scope_keys_keep_cursor_on_reject_always_row() {
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        agent.permission_queue.front_mut().unwrap().active_idx = 3;
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::empty());
        let outcome = agent.handle_permission_key(&left);
        assert!(matches!(outcome, InputOutcome::Changed));
        let perm = agent.permission_queue.front().unwrap();
        assert_eq!(perm.active_idx, 3, "cursor must stay on the deny row");
        assert_eq!(
            perm.bash_deny_selection_count, 1,
            "← on the deny row narrows the deny scope"
        );
        assert_eq!(
            perm.bash_selection_count, 2,
            "the allow scope is untouched from the deny row"
        );
    }
    /// Dangerous commands pin the scope to the full command: ← must not
    /// narrow below it, since enforcement ignores dangerous prefix grants and
    /// a narrowed selection would save a rule that never matches.
    #[test]
    fn scope_left_is_clamped_for_dangerous_commands() {
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        {
            let perm = agent.permission_queue.front_mut().unwrap();
            perm.bash_highlights = Some(
                echo_agent_workspace::permission::bash_command_splitting::BashCommandHighlights {
                    prefix: vec![],
                    highlighted_words: vec!["git".into(), "push".into(), "origin".into()],
                    suffix: vec![],
                },
            );
            perm.bash_selection_count = 3;
            perm.active_idx = 0;
        }
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::empty());
        agent.handle_permission_key(&left);
        {
            let perm = agent.permission_queue.front().unwrap();
            assert_eq!(
                perm.bash_selection_count, 3,
                "← must not narrow a dangerous allow below the full scope"
            );
        }
        {
            let perm = agent.permission_queue.front_mut().unwrap();
            perm.bash_deny_selection_count = 3;
            perm.active_idx = 3;
        }
        agent.handle_permission_key(&left);
        let perm = agent.permission_queue.front().unwrap();
        assert_eq!(
            perm.bash_deny_selection_count, 2,
            "← on the deny row narrows even for dangerous commands"
        );
        assert!(
            perm.has_adjustable_scope(),
            "arrows stay advertised while the deny row is adjustable"
        );
    }
    /// The allow arrow skips an argv-ambiguous intermediate scope (a quoted
    /// arg with a space) that would persist nothing, landing on the next
    /// scope that saves a working grant.
    #[test]
    fn allow_scope_skips_ambiguous_intermediate() {
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        {
            let perm = agent.permission_queue.front_mut().unwrap();
            perm.bash_highlights = Some(
                echo_agent_workspace::permission::bash_command_splitting::BashCommandHighlights {
                    prefix: vec![],
                    highlighted_words: vec![
                        "git".into(),
                        "show".into(),
                        "-e".into(),
                        "A B".into(),
                        "file".into(),
                    ],
                    suffix: vec![],
                },
            );
            perm.bash_selection_count = 3;
            perm.active_idx = 0;
        }
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::empty());
        agent.handle_permission_key(&right);
        assert_eq!(
            agent.permission_queue.front().unwrap().bash_selection_count,
            5,
            "→ must skip the argv-ambiguous scope 4"
        );
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::empty());
        agent.handle_permission_key(&left);
        assert_eq!(
            agent.permission_queue.front().unwrap().bash_selection_count,
            3,
            "← must skip the argv-ambiguous scope 4"
        );
    }
    /// From a non-scoped row ←/→ still jump the cursor to the AllowAlways
    /// row (the discoverability affordance).
    #[test]
    fn scope_keys_still_jump_from_neutral_row() {
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        agent.permission_queue.front_mut().unwrap().active_idx = 1;
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::empty());
        let outcome = agent.handle_permission_key(&right);
        assert!(matches!(outcome, InputOutcome::Changed));
        let perm = agent.permission_queue.front().unwrap();
        assert_eq!(perm.active_idx, 0, "cursor jumps to the AllowAlways row");
        assert_eq!(perm.bash_selection_count, 3, "→ must still expand scope");
    }
    /// Stale bash selection meta without the scoped rows (multi-command
    /// script, gate off, or an old client) must leave ←/→ inert: no cursor
    /// jump and no selection change.
    #[test]
    fn scope_keys_are_inert_without_scoped_rows() {
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        {
            let perm = agent.permission_queue.front_mut().unwrap();
            perm.options = vec![
                option("allow-once", acp::PermissionOptionKind::AllowOnce),
                option("reject-once", acp::PermissionOptionKind::RejectOnce),
            ];
            perm.active_idx = 0;
        }
        for code in [KeyCode::Left, KeyCode::Right] {
            let key = KeyEvent::new(code, KeyModifiers::empty());
            let _ = agent.handle_permission_key(&key);
            let perm = agent.permission_queue.front().unwrap();
            assert_eq!(
                perm.bash_selection_count, 2,
                "selection scope must not change without scoped rows"
            );
            assert_eq!(perm.active_idx, 0, "cursor must not jump");
        }
    }
    /// A decoy `AllowAlways`-kind option sitting before the exact bash allow
    /// row must not capture the arrow jump, the `e` editor entry, or the
    /// editor's Enter submit — all three must target `allow-always-command`.
    #[test]
    fn scoped_actions_target_exact_allow_always_command_id() {
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        {
            let perm = agent.permission_queue.front_mut().unwrap();
            let mut options = vec![option(
                "always-allow",
                acp::PermissionOptionKind::AllowAlways,
            )];
            options.append(&mut perm.options);
            perm.options = options;
            perm.active_idx = 2;
        }
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::empty());
        let _ = agent.handle_permission_key(&right);
        assert_eq!(
            agent.permission_queue.front().unwrap().active_idx,
            1,
            "arrows must jump to allow-always-command, not the decoy"
        );
        agent.permission_queue.front_mut().unwrap().active_idx = 2;
        let e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty());
        let _ = agent.handle_permission_key(&e);
        {
            let perm = agent.permission_queue.front().unwrap();
            assert_eq!(
                perm.focus,
                crate::views::permission_view::PermissionFocus::PatternEdit
            );
            assert_eq!(perm.active_idx, 1, "`e` must land on allow-always-command");
        }
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        let outcome = agent.handle_permission_key(&enter);
        match outcome {
            InputOutcome::Action(Action::PermissionSelect(id)) => {
                assert_eq!(id.0.as_ref(), "allow-always-command");
            }
            other => {
                panic!("expected PermissionSelect(allow-always-command), got {other:?}")
            }
        }
    }
    /// A catch-all pattern must not submit from the editor: the manager would
    /// silently refuse to persist it, and the user would be re-prompted after
    /// believing a rule was saved.
    #[test]
    fn pattern_editor_refuses_catchall_submit() {
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        let e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty());
        let _ = agent.handle_permission_key(&e);
        let ctrl_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        let _ = agent.handle_permission_key(&ctrl_u);
        let star = KeyEvent::new(KeyCode::Char('*'), KeyModifiers::empty());
        let _ = agent.handle_permission_key(&star);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        let outcome = agent.handle_permission_key(&enter);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "catch-all pattern must not submit, got {outcome:?}"
        );
        assert_eq!(
            agent.permission_queue.front().unwrap().focus,
            crate::views::permission_view::PermissionFocus::PatternEdit,
            "editor stays open for the user to narrow the pattern"
        );
    }
    /// With only the exact `reject-always-command` row present (the allow row
    /// may be suppressed as unhonorable), ←/→ adjust the deny scope in place
    /// on that row, and from a neutral row they land on the deny row — never
    /// on an invisible allow count.
    #[test]
    fn reject_only_scoped_row_adjusts_in_place_without_allow_jump() {
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        {
            let perm = agent.permission_queue.front_mut().unwrap();
            perm.options = vec![
                option("allow-once", acp::PermissionOptionKind::AllowOnce),
                option(
                    "reject-always-command",
                    acp::PermissionOptionKind::RejectAlways,
                ),
            ];
            perm.active_idx = 1;
        }
        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::empty());
        let _ = agent.handle_permission_key(&left);
        {
            let perm = agent.permission_queue.front().unwrap();
            assert_eq!(perm.active_idx, 1, "cursor stays on the deny row");
            assert_eq!(
                perm.bash_deny_selection_count, 1,
                "← must still narrow the deny scope"
            );
        }
        agent.permission_queue.front_mut().unwrap().active_idx = 0;
        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::empty());
        let _ = agent.handle_permission_key(&right);
        let perm = agent.permission_queue.front().unwrap();
        assert_eq!(perm.active_idx, 1, "arrows land on the deny row");
        assert_eq!(
            perm.bash_deny_selection_count, 2,
            "→ expands the deny scope"
        );
        assert_eq!(
            perm.bash_selection_count, 2,
            "the invisible allow count is untouched"
        );
    }
    /// Ctrl-F toggles args expansion in both focus modes when the prompt
    /// shows planned MCP args — even when remember_tool_approvals=false
    /// strips the always-allow row and leaves `mcp_scope` unset.
    #[test]
    fn ctrl_f_toggles_args_expansion_when_args_present() {
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        agent.handle_permission_key(&ctrl_f);
        assert!(
            !agent.permission_queue.front().unwrap().args_expanded,
            "Ctrl-F must be a no-op without args"
        );
        {
            let perm = agent.permission_queue.front_mut().unwrap();
            perm.bash_highlights = None;
            perm.description = vec!["{".into(), "  \"k\": 1".into(), "}".into()];
        }
        let outcome = agent.handle_permission_key(&ctrl_f);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.permission_queue.front().unwrap().args_expanded);
        agent.permission_queue.front_mut().unwrap().focus =
            crate::views::permission_view::PermissionFocus::FollowupInput;
        let outcome = agent.handle_permission_key(&ctrl_f);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(!agent.permission_queue.front().unwrap().args_expanded);
    }
    /// Protected-edit prompts reuse `description` for warning prose and
    /// carry the session-edits row; with no MCP args and no long bash there
    /// is nothing collapsible, so Ctrl-F must stay a no-op.
    #[test]
    fn ctrl_f_is_noop_for_protected_edit_description() {
        let mut agent = make_agent();
        let mut perm = super::test_fixtures::make_followup_permission_state();
        perm.focus = crate::views::permission_view::PermissionFocus::Options;
        perm.description = vec![
            "Warning: this file is protected".into(),
            "Edits outside the workspace need approval".into(),
        ];
        perm.options = vec![
            option("allow-once", acp::PermissionOptionKind::AllowOnce),
            option(
                echo_agent_workspace::permission::ALLOW_EDITS_SESSION_OPTION_ID,
                acp::PermissionOptionKind::AllowAlways,
            ),
            option("reject-once", acp::PermissionOptionKind::RejectOnce),
        ];
        agent.permission_queue.push_back(perm);
        agent.pane_areas.prompt = ratatui::layout::Rect::new(0, 20, 80, 10);
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        agent.handle_permission_key(&ctrl_f);
        assert!(
            !agent.permission_queue.front().unwrap().args_expanded,
            "protected-edit description must not toggle"
        );
    }
    /// Ctrl-F toggles a bash script that wraps past the collapsed budget;
    /// short scripts keep it a no-op (nothing collapsible).
    #[test]
    fn ctrl_f_toggles_bash_expansion_when_script_is_long() {
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        agent.pane_areas.prompt = ratatui::layout::Rect::new(0, 20, 80, 10);
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        agent.permission_queue.front_mut().unwrap().bash_command_raw = Some("echo short".into());
        agent.handle_permission_key(&ctrl_f);
        assert!(
            !agent.permission_queue.front().unwrap().args_expanded,
            "Ctrl-F must be a no-op on a short script"
        );
        agent.permission_queue.front_mut().unwrap().bash_command_raw = Some(
            (0..12)
                .map(|i| format!("echo line{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let outcome = agent.handle_permission_key(&ctrl_f);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.permission_queue.front().unwrap().args_expanded);
        let outcome = agent.handle_permission_key(&ctrl_f);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(!agent.permission_queue.front().unwrap().args_expanded);
    }
    /// Ctrl-F is handled before the focus match, so it toggles in Options,
    /// FollowupInput, and PatternEdit alike (the footer hint shows in all
    /// three for the same reason).
    #[test]
    fn ctrl_f_toggles_in_every_focus_mode() {
        use crate::views::permission_view::PermissionFocus;
        let mut agent = make_agent();
        setup_bash_permission(&mut agent);
        agent.pane_areas.prompt = ratatui::layout::Rect::new(0, 20, 80, 10);
        agent.permission_queue.front_mut().unwrap().bash_command_raw = Some(
            (0..12)
                .map(|i| format!("echo line{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        for focus in [
            PermissionFocus::Options,
            PermissionFocus::FollowupInput,
            PermissionFocus::PatternEdit,
        ] {
            {
                let perm = agent.permission_queue.front_mut().unwrap();
                perm.focus = focus;
                perm.args_expanded = false;
            }
            let outcome = agent.handle_permission_key(&ctrl_f);
            assert!(matches!(outcome, InputOutcome::Changed), "{focus:?}");
            assert!(
                agent.permission_queue.front().unwrap().args_expanded,
                "Ctrl-F must expand in {focus:?}"
            );
        }
    }
}
#[cfg(test)]
mod question_no_freeform_tests {
    //! Freeform ("Other") gating for `no_freeform` question modals — e.g.
    //! the EchoAgent upsell. Regression tests for the bug where clicking
    //! under the last option of the upsell selected the (hidden) freeform
    //! row and let the user type into a modal that offers no free text.
    use super::super::test_fixtures::make_agent;
    use crate::actions::ActionRegistry;
    use crate::app::agent_view::AgentView;
    use crate::views::prompt_widget::StashedPrompt;
    use crate::views::question_view::{QuestionFocus, QuestionSelection, QuestionViewState};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    /// Fixed options, single-select — shaped like the free-usage upsell.
    fn upsell_question() -> Question {
        let opt = |label: &str, desc: &str| QuestionOption {
            label: label.into(),
            description: desc.into(),
            preview: None,
            id: None,
        };
        Question {
            question: "You hit your free usage limit.".into(),
            options: vec![
                opt("Upgrade to EchoAgent", "For everyday coding"),
                opt("Upgrade to EchoAgent Heavy", "Highest usage limits"),
            ],
            multi_select: Some(false),
            id: None,
        }
    }
    pub(super) fn open_question(agent: &mut AgentView, no_freeform: bool) {
        let state = QuestionViewState::new(
            "tc-upsell".into(),
            vec![upsell_question()],
            StashedPrompt::default(),
        );
        agent.question_view = Some(if no_freeform {
            state.with_no_freeform()
        } else {
            state
        });
    }
    /// Draw one 80x30 frame so `pane_areas` and `question_scroll_region`
    /// hold the real rendered layout the mouse handler hit-tests against.
    pub(super) fn draw_frame(agent: &mut AgentView) {
        let area = Rect::new(0, 0, 80, 30);
        let reg = ActionRegistry::defaults();
        let bundle = crate::app::bundle::BundleState::default();
        let mut buf = Buffer::empty(area);
        let mut scratch = crate::scrollback::render::ScratchBuffer::new();
        agent.last_terminal_size = (80, 30);
        agent.draw(
            area,
            &mut buf,
            &reg,
            &mut scratch,
            None,
            false,
            crate::app::agent_view::BannerSlotParams::none(),
            &bundle,
            false,
            false,
            &mut Vec::new(),
            crate::app::agent_view::AppRenderParams::default(),
        );
    }
    pub(super) fn down(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }
    fn moved(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: col,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }
    pub(super) fn qv(agent: &AgentView) -> &QuestionViewState {
        agent.question_view.as_ref().expect("question view open")
    }
    /// Clicking the empty rows under the last option (option gap, footer)
    /// must be inert on a `no_freeform` modal: no InputMode, no freeform
    /// selection, no cursor move.
    #[test]
    fn click_below_last_option_is_inert_when_no_freeform() {
        let mut agent = make_agent();
        open_question(&mut agent, true);
        draw_frame(&mut agent);
        let (_, scroll_bottom) = agent.question_scroll_region.expect("scroll region set");
        let col = agent.pane_areas.prompt.x + 5;
        let pane_bottom = agent.pane_areas.prompt.y + agent.pane_areas.prompt.height;
        for row in scroll_bottom..pane_bottom {
            let _ = agent.handle_question_mouse(&down(col, row));
            let state = qv(&agent);
            assert_eq!(
                state.focus,
                QuestionFocus::Navigation,
                "row {row}: click below options must not enter InputMode"
            );
            assert!(
                !state.per_question_freeform_selected[0],
                "row {row}: freeform must not get selected"
            );
            assert!(
                matches!(state.selections[0], QuestionSelection::Single(None)),
                "row {row}: no option may get selected"
            );
            assert_eq!(state.cursor(), 0, "row {row}: cursor must not move");
        }
    }
    /// The last option row of a `no_freeform` modal occupies the screen row
    /// that hosts the sticky freeform row on regular modals — clicking it
    /// must toggle that option, not freeform.
    #[test]
    fn click_last_option_row_toggles_option_when_no_freeform() {
        let mut agent = make_agent();
        open_question(&mut agent, true);
        draw_frame(&mut agent);
        let (_, scroll_bottom) = agent.question_scroll_region.expect("scroll region set");
        let col = agent.pane_areas.prompt.x + 5;
        let last_option_row = scroll_bottom - 1;
        let _ = agent.handle_question_mouse(&down(col, last_option_row));
        let state = qv(&agent);
        assert_eq!(state.focus, QuestionFocus::Navigation);
        assert_eq!(state.cursor(), 1, "cursor lands on the last option");
        assert!(
            matches!(state.selections[0], QuestionSelection::Single(Some(1))),
            "click selects the last option, got {:?}",
            state.selections[0]
        );
        assert!(!state.per_question_freeform_selected[0]);
    }
    /// Hovering the rows below the options must not highlight the
    /// (nonexistent) freeform row on a `no_freeform` modal.
    #[test]
    fn hover_below_last_option_is_inert_when_no_freeform() {
        let mut agent = make_agent();
        open_question(&mut agent, true);
        draw_frame(&mut agent);
        let (_, scroll_bottom) = agent.question_scroll_region.expect("scroll region set");
        let col = agent.pane_areas.prompt.x + 5;
        let _ = agent.handle_question_mouse(&moved(col, scroll_bottom));
        assert_eq!(
            agent.hovered_question_item, None,
            "no phantom freeform hover below the options"
        );
    }
    /// The `z` shortcut (jump to freeform) must be inert on a `no_freeform`
    /// modal.
    #[test]
    fn z_key_is_inert_when_no_freeform() {
        let mut agent = make_agent();
        open_question(&mut agent, true);
        draw_frame(&mut agent);
        let z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
        let _ = agent.handle_question_key(&z);
        let state = qv(&agent);
        assert_eq!(state.focus, QuestionFocus::Navigation);
        assert_eq!(state.cursor(), 0, "z must not move the cursor");
        assert!(!state.per_question_freeform_selected[0]);
    }
    /// Control group: on a regular modal (freeform present) the sticky
    /// freeform row sits one row below the options and clicking it still
    /// selects freeform and enters InputMode, and `z` still works.
    #[test]
    fn freeform_modal_click_and_z_still_enter_input_mode() {
        let mut agent = make_agent();
        open_question(&mut agent, false);
        draw_frame(&mut agent);
        let (_, scroll_bottom) = agent.question_scroll_region.expect("scroll region set");
        let col = agent.pane_areas.prompt.x + 5;
        let _ = agent.handle_question_mouse(&down(col, scroll_bottom));
        {
            let state = qv(&agent);
            assert_eq!(
                state.focus,
                QuestionFocus::InputMode,
                "clicking the sticky freeform row enters InputMode"
            );
            assert!(state.per_question_freeform_selected[0]);
        }
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let _ = agent.handle_question_key(&esc);
        assert_eq!(qv(&agent).focus, QuestionFocus::Navigation);
        let z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
        let _ = agent.handle_question_key(&z);
        assert_eq!(qv(&agent).focus, QuestionFocus::InputMode);
    }
}
#[cfg(test)]
mod question_freeform_chip_tests {
    //! Paste-chip round trip through the question freeform input:
    //! re-entering input mode used to reload the unchanged draft with a
    //! wholesale `set_text`, expanding every chip into raw text.
    use super::super::test_fixtures::make_agent;
    use super::question_no_freeform_tests::{down, draw_frame, open_question, qv};
    use crate::app::agent_view::AgentView;
    use crate::views::prompt_widget::KIND_PASTE;
    use crate::views::question_view::QuestionFocus;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    const PASTE: &str = "line 1\nline 2\nline 3\nline 4\nline 5";
    fn paste_chip_count(agent: &AgentView) -> usize {
        agent
            .prompt
            .textarea()
            .elements()
            .iter()
            .filter(|e| e.kind == KIND_PASTE)
            .count()
    }
    /// Multi-line paste folds into a chip; Esc out and Enter back in must
    /// keep the chip folded (not raw expanded text), and the string slot
    /// keeps the full paste for the submit payload.
    #[test]
    fn paste_chip_survives_input_mode_round_trip() {
        let mut agent = make_agent();
        open_question(&mut agent, false);
        let z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
        let _ = agent.handle_question_key(&z);
        assert_eq!(qv(&agent).focus, QuestionFocus::InputMode);
        let _ = agent.prompt.handle_paste(PASTE);
        assert_eq!(paste_chip_count(&agent), 1, "paste must fold into a chip");
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let _ = agent.handle_question_key(&esc);
        assert_eq!(qv(&agent).focus, QuestionFocus::Navigation);
        assert_eq!(qv(&agent).per_question_freeform[0], PASTE);
        assert!(qv(&agent).per_question_freeform_selected[0]);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let _ = agent.handle_question_key(&enter);
        assert_eq!(qv(&agent).focus, QuestionFocus::InputMode);
        assert_eq!(
            paste_chip_count(&agent),
            1,
            "re-entering input mode must keep the folded chip"
        );
        assert_eq!(agent.prompt.text(), PASTE, "buffer text must round-trip");
    }
    /// A slot rewritten by another surface (e.g. the dashboard peek answer
    /// path) no longer matches the live draft, so re-entry must take the
    /// normal `set_text` path and show the rewritten slot.
    #[test]
    fn rewritten_slot_replaces_stale_draft() {
        let mut agent = make_agent();
        open_question(&mut agent, false);
        let z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
        let _ = agent.handle_question_key(&z);
        let _ = agent.prompt.handle_paste(PASTE);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let _ = agent.handle_question_key(&esc);
        agent.question_view.as_mut().unwrap().per_question_freeform[0] = "peek answer".to_string();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let _ = agent.handle_question_key(&enter);
        assert_eq!(qv(&agent).focus, QuestionFocus::InputMode);
        assert_eq!(
            agent.prompt.text(),
            "peek answer",
            "a stale draft must not shadow the rewritten slot"
        );
        assert_eq!(paste_chip_count(&agent), 0);
    }
    /// Double-click on the chip inside the question freeform input expands
    /// it, exactly like the main prompt; a single click must not.
    #[test]
    fn double_click_expands_chip_in_question_input() {
        let mut agent = make_agent();
        open_question(&mut agent, false);
        let z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
        let _ = agent.handle_question_key(&z);
        let _ = agent.prompt.handle_paste(PASTE);
        assert_eq!(paste_chip_count(&agent), 1);
        draw_frame(&mut agent);
        let ta = agent.prompt.textarea_area();
        assert!(ta.area() > 0, "inline textarea must have rendered");
        let (col, row) = (ta.x + 2, ta.y);
        let _ = agent.handle_question_mouse(&down(col, row));
        assert_eq!(
            paste_chip_count(&agent),
            1,
            "a single click must not expand the chip"
        );
        let _ = agent.handle_question_mouse(&down(col, row));
        assert_eq!(
            paste_chip_count(&agent),
            0,
            "double-click must expand the chip"
        );
        assert_eq!(agent.prompt.text(), PASTE, "content inlined as plain text");
        assert_eq!(
            qv(&agent).focus,
            QuestionFocus::InputMode,
            "expanding must not leave input mode"
        );
    }
    /// A textarea click from before leaving InputMode must not pair with
    /// the first click after re-entry as a double-click (exits clear the
    /// pairing timer).
    #[test]
    fn click_before_exit_does_not_pair_with_click_after_reentry() {
        let mut agent = make_agent();
        open_question(&mut agent, false);
        let z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
        let _ = agent.handle_question_key(&z);
        let _ = agent.prompt.handle_paste(PASTE);
        draw_frame(&mut agent);
        let ta = agent.prompt.textarea_area();
        let (col, row) = (ta.x + 2, ta.y);
        let _ = agent.handle_question_mouse(&down(col, row));
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let _ = agent.handle_question_key(&esc);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let _ = agent.handle_question_key(&enter);
        let _ = agent.handle_question_mouse(&down(col, row));
        assert_eq!(paste_chip_count(&agent), 1, "chip must stay folded");
    }
}
#[cfg(test)]
mod question_answer_focus_tests {
    //! The question card's answer walk. Tab used to hand focus to the
    //! scrollback while the card stayed drawn; these pin the walk that
    //! replaced it.
    use super::super::test_fixtures::make_agent;
    use super::super::{AgentPane, AgentView};
    use super::question_no_freeform_tests::open_question;
    use crate::actions::ActionRegistry;
    use crate::app::app_view::InputOutcome;
    use crate::views::prompt_widget::StashedPrompt;
    use crate::views::question_view::{QuestionFocus, QuestionSelection, QuestionViewState};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::{
        Question, QuestionOption,
    };
    fn question(prompt: &str, labels: &[&str]) -> Question {
        Question {
            question: prompt.into(),
            options: labels
                .iter()
                .map(|label| QuestionOption {
                    label: (*label).into(),
                    description: "why".into(),
                    preview: None,
                    id: None,
                })
                .collect(),
            multi_select: Some(false),
            id: None,
        }
    }
    fn open_two_questions(agent: &mut AgentView) {
        agent.question_view = Some(QuestionViewState::new(
            "tc-tab".into(),
            vec![
                question("First?", &["Alpha", "Beta"]),
                question("Second?", &["Gamma", "Delta"]),
            ],
            StashedPrompt::default(),
        ));
    }
    fn press(agent: &mut AgentView, code: KeyCode, modifiers: KeyModifiers) {
        let _ = agent.handle_question_key_for_test(&KeyEvent::new(code, modifiers));
    }
    fn tab(agent: &mut AgentView) {
        press(agent, KeyCode::Tab, KeyModifiers::NONE);
    }
    fn qv(agent: &AgentView) -> &QuestionViewState {
        agent.question_view.as_ref().expect("question view open")
    }
    /// (question index, cursor row).
    fn stop(agent: &AgentView) -> (usize, usize) {
        (qv(agent).active_tab, qv(agent).cursor())
    }
    fn hint_labels(agent: &AgentView) -> Vec<String> {
        agent
            .current_shortcut_hints(&ActionRegistry::defaults(), false)
            .iter()
            .map(|hint| hint.label.to_string())
            .collect()
    }
    #[test]
    fn tab_stays_inside_the_current_question() {
        let mut agent = make_agent();
        open_two_questions(&mut agent);
        let mut visited = vec![stop(&agent)];
        for _ in 0..6 {
            tab(&mut agent);
            visited.push(stop(&agent));
        }
        assert_eq!(
            visited,
            vec![(0, 0), (0, 1), (0, 2), (0, 0), (0, 1), (0, 2), (0, 0)],
            "Tab loops over question 1's answers; question 2 is only reachable with h/l"
        );
        assert_eq!(
            agent.active_pane,
            AgentPane::Prompt,
            "the card keeps the keyboard the whole way round"
        );
    }
    #[test]
    fn tab_wraps_within_a_single_question() {
        let mut agent = make_agent();
        open_question(&mut agent, false);
        for expected in [1, 2, 0] {
            tab(&mut agent);
            assert_eq!(stop(&agent), (0, expected));
        }
        assert_eq!(agent.active_pane, AgentPane::Prompt);
    }
    /// Both Shift+Tab encodings terminals emit walk the answers backwards.
    #[test]
    fn shift_tab_walks_the_answers_backwards() {
        for (code, modifiers) in [
            (KeyCode::BackTab, KeyModifiers::NONE),
            (KeyCode::Tab, KeyModifiers::SHIFT),
        ] {
            let mut agent = make_agent();
            open_two_questions(&mut agent);
            tab(&mut agent);
            assert_eq!(stop(&agent), (0, 1), "parked on the second answer");
            press(&mut agent, code, modifiers);
            assert_eq!(
                stop(&agent),
                (0, 0),
                "Shift+Tab steps back up the answers ({code:?})"
            );
            press(&mut agent, code, modifiers);
            assert_eq!(
                stop(&agent),
                (0, 2),
                "before the first answer, Shift+Tab wraps to the last one ({code:?})"
            );
            assert_eq!(agent.active_pane, AgentPane::Prompt);
        }
    }
    #[test]
    fn tab_from_the_scrollback_focuses_the_card() {
        let mut agent = make_agent();
        open_two_questions(&mut agent);
        agent.active_pane = AgentPane::Scrollback;
        let registry = ActionRegistry::defaults();
        let outcome = agent
            .handle_scrollback_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &registry);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "Tab in the scrollback focuses the card, got {outcome:?}"
        );
        assert_eq!(agent.active_pane, AgentPane::Prompt);
    }
    #[test]
    fn tab_skips_the_free_text_row_when_the_card_has_none() {
        let mut agent = make_agent();
        open_question(&mut agent, true);
        tab(&mut agent);
        assert_eq!(stop(&agent), (0, 1), "two options, so one step");
        tab(&mut agent);
        assert_eq!(
            stop(&agent),
            (0, 0),
            "the last option wraps to the first when there is no free-text row"
        );
    }
    #[test]
    fn esc_unselects_then_parks_focus_in_the_scrollback() {
        let mut agent = make_agent();
        open_two_questions(&mut agent);
        press(&mut agent, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(
            matches!(qv(&agent).selections[0], QuestionSelection::Single(Some(0))),
            "Space marks the focused answer"
        );
        press(&mut agent, KeyCode::Esc, KeyModifiers::NONE);
        assert!(
            matches!(qv(&agent).selections[0], QuestionSelection::Single(None)),
            "Esc clears the answer"
        );
        assert_eq!(
            agent.active_pane,
            AgentPane::Prompt,
            "clearing an answer must not also move focus"
        );
        press(&mut agent, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            agent.active_pane,
            AgentPane::Scrollback,
            "with nothing left to unselect, Esc hands the keyboard to the scrollback"
        );
        assert!(
            agent.question_view.is_some(),
            "the card stays open and answerable"
        );
        let registry = ActionRegistry::defaults();
        let _ = agent
            .handle_scrollback_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &registry);
        assert_eq!(
            agent.active_pane,
            AgentPane::Prompt,
            "Tab hands the keyboard back to the card"
        );
    }
    #[test]
    fn tab_in_input_mode_stays_with_the_text_field() {
        let mut agent = make_agent();
        open_two_questions(&mut agent);
        press(&mut agent, KeyCode::Char('z'), KeyModifiers::NONE);
        assert_eq!(qv(&agent).focus, QuestionFocus::InputMode);
        tab(&mut agent);
        assert_eq!(
            qv(&agent).focus,
            QuestionFocus::InputMode,
            "Tab must not walk the answers out from under a half-typed answer"
        );
        assert_eq!(agent.active_pane, AgentPane::Prompt);
    }
    /// The reported symptom was the bar promising one thing while Tab did
    /// another, so the bar must name the walk at every stop.
    #[test]
    fn shortcut_hints_name_the_answer_walk() {
        let mut agent = make_agent();
        open_two_questions(&mut agent);
        for step in 0..7 {
            let hints = hint_labels(&agent);
            assert!(
                hints.contains(&"next answer".to_string()),
                "step {step}: the bar advertises the answer walk, got {hints:?}"
            );
            tab(&mut agent);
        }
    }
}
