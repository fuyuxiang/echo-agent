//! Secondary pane input: scrollback keys and search, todo/tool-usage panes,
//! background tasks, subagent catalog, and the pane-aware scroll router.
use super::{ActivePane, AgentPane, AgentView, overlay_action_to_outcome, resolve_action};
use crate::actions::{ActionId, ActionRegistry, When};
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::key;
use crate::scrollback::ScrollbackSearchState;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
impl AgentView {
    /// Scrollback-focused key handling.
    ///
    /// When the block viewer is open, routes keys to the viewer.
    /// Otherwise, uses ActionRegistry for keybinding lookup.
    pub(super) fn handle_scrollback_key(
        &mut self,
        key: &KeyEvent,
        registry: &ActionRegistry,
    ) -> InputOutcome {
        if let Some(outcome) = self.handle_scrollback_search_key(key) {
            return outcome;
        }
        let viewer_has_input = self
            .block_viewer
            .as_ref()
            .is_some_and(|v| v.list_state.input_mode().is_some());
        let allow_i_alt = self.vim_mode;
        if !viewer_has_input
            && (matches!(key.code, KeyCode::Tab | KeyCode::Char(' '))
                || (allow_i_alt && matches!(key.code, KeyCode::Char('i'))))
        {
            if self.parked_card().is_some() {
                self.set_active_pane(AgentPane::Prompt, false);
                return InputOutcome::Changed;
            }
            if key.code == KeyCode::Tab
                && self.tasks.overlay.visible
                && self.set_active_pane(AgentPane::Tasks, false)
            {
                self.tasks.overlay.focused = true;
                return InputOutcome::Changed;
            }
            return InputOutcome::Action(Action::FocusPrompt);
        }
        if key!(Enter).matches(key)
            && let Some(target) = self.highlighted_link_target().cloned()
        {
            self.highlighted_link_idx = None;
            return InputOutcome::Action(Action::OpenLink(target));
        }
        if crate::app::inline_edit::INLINE_EDIT_ENABLED
            && key!(Enter).matches(key)
            && !self.scrollback.is_selected_group_header()
            && let Some(idx) = self.scrollback.selected()
            && self
                .scrollback
                .entry(idx)
                .is_some_and(|e| e.block.is_user_prompt())
            && self.enter_inline_edit(idx)
        {
            return InputOutcome::Changed;
        }
        if key!(Enter).matches(key)
            && !self.scrollback.is_selected_group_header()
            && let Some(idx) = self.scrollback.selected()
            && let Some(entry) = self.scrollback.entry(idx)
            && let crate::scrollback::block::RenderBlock::Subagent(ref sb) = entry.block
        {
            let child_sid = sb.child_session_id.clone();
            if self.subagent_views.contains_key(&child_sid) {
                self.open_subagent_fullscreen(child_sid);
                return InputOutcome::Changed;
            }
        }
        if self.vim_mode
            && key!('x').matches(key)
            && !self.scrollback.is_selected_group_header()
            && let Some(idx) = self.scrollback.selected()
            && let Some(entry) = self.scrollback.entry(idx)
            && let crate::scrollback::block::RenderBlock::BgTask(ref bt) = entry.block
            && self
                .session
                .bg_tasks
                .get(&bt.task_id)
                .is_some_and(|t| t.status == crate::app::agent::BgTaskStatus::Running)
        {
            return InputOutcome::Action(Action::KillBgTask(bt.task_id.clone()));
        }
        if key.code == KeyCode::Esc
            && key.modifiers.is_empty()
            && self.persistent_text_selection.take().is_some()
        {
            self.table_selection_geometry = None;
            self.selection_created_at = None;
            return InputOutcome::Changed;
        }
        if key.code == KeyCode::Esc
            && key.modifiers.is_empty()
            && self.highlighted_link_idx.take().is_some()
        {
            return InputOutcome::Changed;
        }
        if self.vim_mode
            && key!('/').matches(key)
            && self.no_input_overlay_pending()
            && self.btw_state.is_none()
        {
            if self.scrollback.is_empty() {
                return InputOutcome::ActionThenForward(Action::FocusPrompt);
            }
            self.open_scrollback_search(None);
            return InputOutcome::Changed;
        }
        if registry.lookup(key, When::ScrollbackFocused) == Some(ActionId::ToggleMouseCapture) {
            return InputOutcome::Action(Action::ToggleMouseCapture);
        }
        if let Some(outcome) =
            resolve_action(registry.lookup_with_mode(key, When::ScrollbackFocused, self.vim_mode))
        {
            return outcome;
        }
        if !self.vim_mode
            && let KeyCode::Char(c) = key.code
            && (c.is_ascii_alphabetic() || c == '/')
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
        {
            return InputOutcome::ActionThenForward(Action::FocusPrompt);
        }
        InputOutcome::Unchanged
    }
    /// Focus the scrollback pane and open an incremental search over it.
    ///
    /// Shared by the vim `/` key and the `/find` slash command so both entry
    /// points land in the same state, refocusing scrollback when `/find` is run
    /// from the prompt in simple mode.
    ///
    /// Only opens the search if the pane switch succeeds: a dirty queued-prompt
    /// edit blocks the switch (showing the confirm modal) and returns false, so
    /// opening search then would strand an invisible session on the prompt — the
    /// search bar and key handling are gated on scrollback being focused.
    ///
    /// `initial_query` (the `/find <word>` argument) is fed through the same
    /// keystroke path so a pre-filled search behaves identically to typing the
    /// word into the bar: a composing regex query with immediate highlights.
    pub(crate) fn open_scrollback_search(&mut self, initial_query: Option<&str>) {
        if self.set_active_pane(AgentPane::Scrollback, false) {
            self.scrollback_search = Some(ScrollbackSearchState::open());
            if let Some(query) = initial_query {
                self.set_scrollback_search_query(query);
            }
        }
    }
    /// Step to the next (`forward`) or previous match and scroll it into view.
    /// Shared by the `n`/`N` keys and the `↓`/`↑` arrows.
    fn navigate_search(&mut self, forward: bool) -> Option<InputOutcome> {
        if let Some(search) = self.scrollback_search.as_mut() {
            if forward {
                search.next();
            } else {
                search.prev();
            }
        }
        self.reveal_current_search_match();
        Some(InputOutcome::Changed)
    }
    /// Bottom scrollback rows to reserve for the search UI (divider + bar):
    /// two when search is active, clamped to the rows that actually exist so a
    /// very short region never pushes the bar below the scrollback rect.
    pub(super) fn search_reserved_rows(scrollback_height: u16, search_active: bool) -> u16 {
        if search_active {
            scrollback_height.min(2)
        } else {
            0
        }
    }
    /// Handle a key while the scrollback search overlay is open.
    ///
    /// Returns `None` when search isn't open (or, while browsing, for keys that
    /// should fall through to normal scrollback handling). While composing the
    /// query the bar is modal and swallows other keys.
    fn handle_scrollback_search_key(&mut self, key: &KeyEvent) -> Option<InputOutcome> {
        let composing = self.scrollback_search.as_ref()?.is_composing();
        let non_text = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER;
        if key.code == KeyCode::Esc {
            self.scrollback_search = None;
            return Some(InputOutcome::Changed);
        }
        if key.modifiers.is_empty() {
            match key.code {
                KeyCode::Down => return self.navigate_search(true),
                KeyCode::Up => return self.navigate_search(false),
                _ => {}
            }
        }
        if composing {
            if key.code == KeyCode::Enter {
                if self.scrollback_search.as_ref()?.query().is_empty() {
                    self.scrollback_search = None;
                } else {
                    if let Some(search) = self.scrollback_search.as_mut() {
                        search.accept();
                    }
                    self.reveal_current_search_match();
                }
                return Some(InputOutcome::Changed);
            }
            let outcome = self
                .scrollback_search
                .as_mut()?
                .apply_query_key(key, &self.scrollback);
            Some(match outcome {
                crate::input::line_editor::LineEditOutcome::TextChanged
                | crate::input::line_editor::LineEditOutcome::CursorChanged
                | crate::input::line_editor::LineEditOutcome::HandledNoChange => {
                    InputOutcome::Changed
                }
                crate::input::line_editor::LineEditOutcome::Unhandled => InputOutcome::Unchanged,
            })
        } else {
            match key.code {
                KeyCode::Char('n') if key.modifiers.is_empty() => self.navigate_search(true),
                KeyCode::Char('N') if !key.modifiers.intersects(non_text) => {
                    self.navigate_search(false)
                }
                _ => None,
            }
        }
    }
    pub(super) fn handle_scrollback_search_paste(&mut self, text: &str) -> Option<InputOutcome> {
        let search = self.scrollback_search.as_mut()?;
        if !search.is_composing() {
            return Some(InputOutcome::Unchanged);
        }
        let outcome = search.apply_query_paste(text, &self.scrollback);
        Some(match outcome {
            crate::input::line_editor::LineEditOutcome::TextChanged
            | crate::input::line_editor::LineEditOutcome::CursorChanged
            | crate::input::line_editor::LineEditOutcome::HandledNoChange => InputOutcome::Changed,
            crate::input::line_editor::LineEditOutcome::Unhandled => InputOutcome::Unchanged,
        })
    }
    /// Enqueue `query` for the background scan. Results (and the reveal) arrive
    /// later via [`poll_scrollback_search`](Self::poll_scrollback_search); the
    /// highlight updates immediately because it reads the UI-side matcher.
    fn set_scrollback_search_query(&mut self, query: &str) {
        if let Some(search) = self.scrollback_search.as_mut() {
            search.update_query(query, &self.scrollback);
        }
    }
    /// Poll the background search daemon for new results, revealing the freshly
    /// parked match when they change. Returns `true` if the UI should redraw.
    pub(crate) fn poll_scrollback_search(&mut self) -> bool {
        let changed = self.scrollback_search.as_mut().is_some_and(|s| s.poll());
        if changed {
            self.reveal_current_search_match();
        }
        changed
    }
    /// Scroll the current search match into view via `reveal_entry_line`.
    fn reveal_current_search_match(&mut self) {
        let target = self
            .scrollback_search
            .as_ref()
            .and_then(|s| s.current())
            .map(|m| (m.entry_id, m.line_in_entry));
        if let Some((id, line)) = target
            && let Some(idx) = self.scrollback.index_of_id(id)
        {
            self.scrollback.reveal_entry_line(idx, line);
        }
    }
    /// Todo-pane-focused key handling.
    ///
    /// Routes structural keys through the shared overlay handler, then
    /// content keys through `TodoPane::handle_key`.
    pub(super) fn handle_todo_key(
        &mut self,
        key: &KeyEvent,
        _registry: &ActionRegistry,
    ) -> InputOutcome {
        use crate::views::overlay::{handle_overlay_key, handle_overlay_nav_key};
        if key!('t', CONTROL).matches(key) {
            self.todo.overlay.toggle();
            self.todo.on_state_change();
            if !self.todo.overlay.focused {
                return InputOutcome::Action(Action::FocusScrollback);
            }
            return InputOutcome::Changed;
        }
        let has_input = self.todo.list_state.input_mode().is_some();
        let action = handle_overlay_key(&mut self.todo.overlay, key).or_else(|| {
            if !has_input {
                handle_overlay_nav_key(&mut self.todo.overlay, key)
            } else {
                None
            }
        });
        if let Some(action) = action {
            self.todo.on_state_change();
            if !self.todo.overlay.visible || !self.todo.overlay.focused {
                self.set_active_pane(AgentPane::Scrollback, false);
            }
            return overlay_action_to_outcome(action);
        }
        if self.todo.handle_key(key) {
            InputOutcome::Changed
        } else {
            InputOutcome::Unchanged
        }
    }
    /// Bg-task-pane-focused key handling.
    pub(super) fn handle_bg_tasks_key(
        &mut self,
        key: &KeyEvent,
        registry: &ActionRegistry,
    ) -> InputOutcome {
        use crate::views::overlay::{handle_overlay_key, handle_overlay_nav_key};
        use crate::views::tasks_pane::TaskEntry;
        if registry.matches_id(ActionId::ToggleTasks, key) {
            self.tasks.overlay.toggle();
            self.tasks.on_state_change();
            if !self.tasks.overlay.focused {
                return InputOutcome::Action(Action::FocusScrollback);
            }
            return InputOutcome::Changed;
        }
        if self.tasks.list_state.input_mode().is_none()
            && let Some(group) = self.tasks.selected_header_group()
        {
            if key!(Right).matches(key) {
                self.tasks.set_group_collapsed(group, false);
                return InputOutcome::Changed;
            }
            if key!(Left).matches(key) {
                self.tasks.set_group_collapsed(group, true);
                return InputOutcome::Changed;
            }
        }
        let is_open_key = self.tasks.list_state.input_mode().is_none()
            && (key!(Enter).matches(key) || key!('f', CONTROL).matches(key));
        if is_open_key {
            if let Some(group) = self.tasks.selected_header_group() {
                self.tasks.toggle_group(group);
                return InputOutcome::Changed;
            }
            match self.tasks.selected_entry() {
                Some(TaskEntry::BgTask { task_id, .. }) => {
                    let task_id = task_id.clone();
                    if let Some(task) = self.session.bg_tasks.get(&task_id) {
                        let entry_id = task
                            .scrollback_entry_id
                            .unwrap_or_else(|| crate::scrollback::entry::EntryId::new(0));
                        let is_running = task.status == crate::app::agent::BgTaskStatus::Running;
                        self.block_viewer =
                            Some(crate::views::block_viewer::BlockViewerPane::for_bg_task(
                                entry_id,
                                &task_id,
                                &task.stdout,
                                is_running,
                            ));
                        self.set_active_pane(AgentPane::Scrollback, true);
                        return InputOutcome::Changed;
                    }
                }
                Some(TaskEntry::Agent {
                    child_session_id, ..
                }) => {
                    let child_sid = child_session_id.clone();
                    if self.subagent_views.contains_key(&child_sid) {
                        self.open_subagent_fullscreen(child_sid);
                        return InputOutcome::Changed;
                    }
                }
                Some(TaskEntry::Scheduled { .. }) => {}
                Some(TaskEntry::Workflow { name, .. }) => {
                    let name = name.clone();
                    self.open_workflow_detail(&name);
                    return InputOutcome::Changed;
                }
                Some(TaskEntry::Header { .. }) => {}
                None => {}
            }
        }
        if key!('x').matches(key) && self.tasks.list_state.input_mode().is_none() {
            match self.tasks.selected_entry() {
                Some(TaskEntry::BgTask { task_id, .. }) => {
                    let task_id = task_id.clone();
                    if self
                        .session
                        .bg_tasks
                        .get(&task_id)
                        .is_some_and(|t| t.status == crate::app::agent::BgTaskStatus::Running)
                    {
                        return InputOutcome::Action(Action::KillBgTask(task_id));
                    }
                }
                Some(TaskEntry::Agent { subagent_id, .. }) => {
                    let subagent_id = subagent_id.clone();
                    if self.subagent_sessions.values().any(|s| {
                        s.subagent_id.as_ref() == subagent_id && s.is_running() && !s.pending_kill
                    }) {
                        return InputOutcome::Action(Action::KillSubagent(subagent_id));
                    }
                }
                Some(TaskEntry::Scheduled { task_id, .. }) => {
                    return InputOutcome::Action(Action::CancelScheduledTask(task_id.clone()));
                }
                Some(TaskEntry::Workflow {
                    name, stoppable, ..
                }) => {
                    if *stoppable {
                        return InputOutcome::Action(Action::SendSlashCommandPreservingDraft(
                            format!("/workflow stop {name}"),
                        ));
                    }
                }
                Some(TaskEntry::Header { .. }) => {}
                None => {}
            }
        }
        if key!('y').matches(key)
            && self.tasks.list_state.input_mode().is_none()
            && let Some(task_id) = self.tasks.selected_task_id().map(|s| s.to_string())
            && let Some(task) = self.session.bg_tasks.get(&task_id)
            && !task.stdout.is_empty()
        {
            let text = task.stdout.clone();
            self.copy_to_clipboard(&text);
            return InputOutcome::Changed;
        }
        if key!(Tab).matches(key) && self.tasks.list_state.input_mode().is_none() {
            self.tasks.overlay.focused = false;
            return InputOutcome::Action(Action::FocusPrompt);
        }
        let has_input = self.tasks.list_state.input_mode().is_some();
        let action = handle_overlay_key(&mut self.tasks.overlay, key).or_else(|| {
            if !has_input {
                handle_overlay_nav_key(&mut self.tasks.overlay, key)
            } else {
                None
            }
        });
        if let Some(action) = action {
            self.tasks.on_state_change();
            if !self.tasks.overlay.visible || !self.tasks.overlay.focused {
                self.set_active_pane(AgentPane::Scrollback, false);
            }
            return overlay_action_to_outcome(action);
        }
        if self.tasks.handle_key(key) {
            InputOutcome::Changed
        } else {
            InputOutcome::Unchanged
        }
    }
    /// Subagent-pane-focused key handling.
    pub(super) fn handle_catalog_key(
        &mut self,
        key: &KeyEvent,
        _registry: &ActionRegistry,
    ) -> InputOutcome {
        use crate::views::overlay::{handle_overlay_key, handle_overlay_nav_key};
        let has_input = self.catalog.list_state.input_mode().is_some();
        let action = handle_overlay_key(&mut self.catalog.overlay, key).or_else(|| {
            if !has_input {
                handle_overlay_nav_key(&mut self.catalog.overlay, key)
            } else {
                None
            }
        });
        if let Some(action) = action {
            self.catalog.on_state_change();
            if !self.catalog.overlay.visible || !self.catalog.overlay.focused {
                self.set_active_pane(AgentPane::Scrollback, false);
            }
            return overlay_action_to_outcome(action);
        }
        if key.code == crossterm::event::KeyCode::Enter
            && key.modifiers == crossterm::event::KeyModifiers::NONE
        {
            if let Some((kind, name)) = self.catalog.selected_entry() {
                return InputOutcome::Action(Action::ViewCatalogEntry {
                    kind: kind.to_owned(),
                    name: name.to_owned(),
                });
            }
            return InputOutcome::Unchanged;
        }
        if self.catalog.handle_key(key) {
            InputOutcome::Changed
        } else {
            InputOutcome::Unchanged
        }
    }
    /// Handle a normalized scroll event at a screen position.
    ///
    /// Hit-tests against pane areas to decide what to scroll:
    /// - Scrollback area → scroll the scrollback (uses accelerated line count)
    /// - Prompt area → forward to textarea (which has its own scroll logic)
    ///
    /// Positive `lines` = scroll down, negative = scroll up.
    pub fn handle_scroll(&mut self, lines: i32, col: u16, row: u16) {
        if self.show_workflows {
            let runs = self.workflow_runs_newest_first();
            let mut view = self.workflows_view.clone();
            view.handle_scroll(lines, col, row, &runs);
            self.workflows_view = view;
            return;
        }
        if self.show_goal_detail {
            return;
        }
        if let Some(ref mut modal) = self.active_modal {
            use crate::views::modal::ActiveModal;
            match modal {
                ActiveModal::CommandPalette { state, .. }
                | ActiveModal::ArgPicker { state, .. }
                | ActiveModal::SessionPicker { state, .. }
                | ActiveModal::DocPicker { state, .. } => {
                    let delta = lines.unsigned_abs() as usize;
                    let current = state.scroll_offset.unwrap_or(0);
                    let new_offset = if lines > 0 {
                        current + delta
                    } else {
                        current.saturating_sub(delta)
                    };
                    state.scroll_offset = Some(new_offset);
                    state.hovered = None;
                    return;
                }
                ActiveModal::DocViewer { scroll, .. }
                | ActiveModal::RememberNoteReview { scroll, .. } => {
                    crate::views::modal::apply_doc_scroll_delta(scroll, lines);
                    return;
                }
                _ => {}
            }
        }
        if let Some(ref mut viewer) = self.block_viewer {
            viewer.handle_scroll(lines);
            return;
        }
        if self.rewind_state.is_some() {
            if let Some(ref mut rw) = self.rewind_state {
                crate::views::rewind::move_cursor(&mut rw.phase, lines.signum());
                self.sync_rewind_anchor_to_picker();
            }
            return;
        }
        self.dismiss_jump_picker_if_suppressed();
        if let Some(ref mut js) = self.jump_state {
            crate::views::jump::move_cursor(js, lines.signum());
            self.sync_jump_preview();
            return;
        }
        if let Some(ref mut viewer) = self.line_viewer {
            if let Some(area) = viewer.last_popup_area
                && (area.contains((col, row).into()) || viewer.list_state.scrollbar_hit(col, row))
            {
                viewer
                    .list_state
                    .handle_scroll_event(lines, col, row, &viewer.lines);
            }
            return;
        }
        if let Some(ref mut btw) = self.btw_state
            && matches!(btw, crate::views::btw_overlay::BtwOverlayState::Done { .. })
            && self.last_btw_area.area() > 0
            && self.last_btw_area.contains((col, row).into())
        {
            use crate::views::btw_overlay::DONE_MAX_BODY_LINES;
            let max_body = DONE_MAX_BODY_LINES as usize;
            let content_width = self.last_btw_area.width.saturating_sub(4) as usize;
            let max_off = btw.max_scroll_offset(content_width, max_body);
            if lines > 0 {
                btw.scroll_down(lines as usize, max_off);
            } else {
                btw.scroll_up((-lines) as usize);
            }
            return;
        }
        if let Some(hd_area) = self.history_dropdown_area
            && hd_area.contains((col, row).into())
            && self.prompt.history_search.is_active()
        {
            let moved = if lines > 0 {
                self.prompt.history_search.move_down()
            } else if lines < 0 {
                self.prompt.history_search.move_up()
            } else {
                false
            };
            if moved && self.prompt.history_search.is_browse() {
                self.populate_prompt_from_history_selection();
            }
            return;
        }
        if let Some(dd_area) = self.dropdown_items_area
            && dd_area.contains((col, row).into())
        {
            self.prompt
                .file_search
                .move_selection(lines.signum() as isize);
            return;
        }
        if let Some(dd_area) = self.slash_dropdown_items_area
            && dd_area.contains((col, row).into())
        {
            self.prompt.slash_scroll_selection(lines.signum() as isize);
            self.prompt.slash_preview_current_selection();
            return;
        }
        if let Some(dd_area) = self.completion_dropdown_items_area
            && dd_area.contains((col, row).into())
        {
            self.prompt
                .completion_dropdown_scroll(lines.signum() as isize);
            return;
        }
        if self.question_view.is_some() && self.pane_areas.prompt.contains((col, row).into()) {
            if self
                .inline_prompt_area
                .is_some_and(|r| r.contains((col, row).into()))
            {
                let kind = if lines > 0 {
                    MouseEventKind::ScrollDown
                } else {
                    MouseEventKind::ScrollUp
                };
                let event = MouseEvent {
                    kind,
                    column: col,
                    row,
                    modifiers: crossterm::event::KeyModifiers::NONE,
                };
                let _ = self.prompt.handle_mouse(&event);
            } else if let Some((scroll_top, scroll_bottom)) = self.question_scroll_region
                && row >= scroll_top
                && row < scroll_bottom
            {
                self.apply_question_scroll(lines);
            }
            return;
        }
        let target = self
            .pane_areas
            .hit_test(col, row)
            .unwrap_or(ActivePane::Scrollback);
        match target {
            ActivePane::Scrollback => {
                if lines > 0 {
                    self.scrollback.scroll_down(lines as u16);
                } else {
                    self.scrollback.scroll_up((-lines) as u16);
                }
            }
            ActivePane::Todo => {
                self.todo.handle_scroll(lines, col, row);
            }
            ActivePane::Queue => {
                self.queue.handle_scroll(lines, col, row);
            }
            ActivePane::Tasks => {
                self.tasks.handle_scroll(lines, col, row);
            }
            ActivePane::Catalog => {
                self.catalog.handle_scroll(lines, col, row);
            }
            ActivePane::Prompt => {
                if self.question_view.is_some() {
                    return;
                }
                let kind = if lines > 0 {
                    MouseEventKind::ScrollDown
                } else {
                    MouseEventKind::ScrollUp
                };
                let event = MouseEvent {
                    kind,
                    column: col,
                    row,
                    modifiers: KeyModifiers::NONE,
                };
                self.prompt.handle_mouse(&event);
            }
        }
    }
}
#[cfg(test)]
mod scroll_granularity_tests {
    use super::super::test_fixtures::make_agent;
    use crate::views::suggestion_controller::{
        CompletionDropdownState, CompletionItemParsed, SuggestionSource,
    };
    use ratatui::layout::Rect;
    /// Selection dropdowns step exactly one item per wheel dispatch: a
    /// 3-line notch (or accelerated trackpad flush) must not skip items.
    #[test]
    fn wheel_notch_over_slash_dropdown_moves_selection_one_step() {
        let mut agent = make_agent();
        agent.prompt.set_text("/");
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(agent.prompt.slash_open(), "precondition: dropdown open");
        assert!(
            agent.prompt.slash_snapshot().matches.len() >= 3,
            "precondition: enough builtin commands to skip over"
        );
        assert_eq!(agent.prompt.slash_snapshot().selected, 0);
        agent.slash_dropdown_items_area = Some(Rect::new(0, 0, 40, 8));
        agent.handle_scroll(3, 5, 4);
        assert_eq!(
            agent.prompt.slash_snapshot().selected,
            1,
            "3-line wheel notch must move the slash selection by exactly 1"
        );
        agent.handle_scroll(-3, 5, 4);
        assert_eq!(
            agent.prompt.slash_snapshot().selected,
            0,
            "-3-line wheel notch must move the slash selection by exactly -1"
        );
    }
    fn completion_item(label: &str) -> CompletionItemParsed {
        CompletionItemParsed {
            display: label.into(),
            description: String::new(),
            insert_text: label.into(),
            source: SuggestionSource::History,
            priority: 0,
            replace_range: None,
            token_text: None,
            truncated: false,
        }
    }
    #[test]
    fn wheel_notch_over_completion_dropdown_moves_selection_one_step() {
        let mut agent = make_agent();
        agent.prompt.suggestions.dropdown = CompletionDropdownState {
            open: true,
            items: vec![
                completion_item("a"),
                completion_item("b"),
                completion_item("c"),
            ],
            selected: 0,
            ..Default::default()
        };
        agent.completion_dropdown_items_area = Some(Rect::new(0, 0, 40, 8));
        agent.handle_scroll(3, 5, 4);
        assert_eq!(
            agent.prompt.suggestions.dropdown.selected, 1,
            "3-line wheel notch must move the completion selection by exactly 1"
        );
        agent.handle_scroll(-3, 5, 4);
        assert_eq!(
            agent.prompt.suggestions.dropdown.selected, 0,
            "-3-line wheel notch must move the completion selection by exactly -1"
        );
    }
    #[test]
    fn wheel_over_fullscreen_overlays_never_scrolls_panes_beneath() {
        let mut agent = make_agent();
        agent.pane_areas.scrollback = Rect::new(0, 0, 80, 10);
        for i in 0..30 {
            agent
                .scrollback
                .push_block(crate::scrollback::block::RenderBlock::agent_message(
                    format!("line {i}"),
                ));
        }
        agent.scrollback.prepare_layout(80, 10);
        agent.scrollback.scroll_up(5);
        let before = agent.scrollback.scroll_info().0;
        assert!(before > 0, "setup: scrollback holds a real offset");
        agent.show_workflows = true;
        agent.handle_scroll(3, 5, 4);
        agent.handle_scroll(-3, 5, 4);
        assert_eq!(
            agent.scrollback.scroll_info().0,
            before,
            "wheel must not leak through the /workflow runs modal"
        );
        agent.show_workflows = false;
        agent.show_goal_detail = true;
        agent.handle_scroll(-3, 5, 4);
        assert_eq!(
            agent.scrollback.scroll_info().0,
            before,
            "wheel must not leak through the goal detail overlay"
        );
    }
}
#[cfg(test)]
mod mouse_reporting_registry_tests {
    use super::super::{AgentPane, test_fixtures::make_agent};
    use crate::actions::ActionRegistry;
    use crate::app::actions::Action;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    fn ctrl_r() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
    }
    #[test]
    fn mouse_toggle_chord_follows_live_mode_registry() {
        for mode in [
            crate::app::ScreenMode::Fullscreen,
            crate::app::ScreenMode::Inline,
        ] {
            let mut agent = make_agent();
            agent.set_active_pane(AgentPane::Scrollback, true);
            let registry = ActionRegistry::defaults_with_config_for(mode, true);
            assert!(matches!(
                agent.handle_input(&ctrl_r(), &registry),
                InputOutcome::Action(Action::ToggleMouseCapture)
            ));
        }
        let mut agent = make_agent();
        agent.set_active_pane(AgentPane::Scrollback, true);
        let registry =
            ActionRegistry::defaults_with_config_for(crate::app::ScreenMode::Minimal, true);
        assert!(
            registry
                .find(crate::actions::ActionId::ToggleMouseCapture)
                .is_none()
        );
        assert!(!matches!(
            agent.handle_input(&ctrl_r(), &registry),
            InputOutcome::Action(Action::ToggleMouseCapture)
        ));
    }
}
#[cfg(test)]
mod paste_routing_tests {
    use super::super::{AgentPane, test_fixtures::make_agent};
    use crate::actions::ActionRegistry;
    use crate::app::app_view::InputOutcome;
    use crate::scrollback::ScrollbackSearchState;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    #[test]
    fn scrollback_search_paste_stays_scoped_and_browse_is_inert() {
        let mut agent = make_agent();
        agent.vim_mode = false;
        agent.set_active_pane(AgentPane::Scrollback, true);
        agent.prompt.set_text("hidden prompt");
        agent.scrollback_search = Some(ScrollbackSearchState::open());
        let registry = ActionRegistry::defaults();
        let _ = agent.handle_input(&Event::Paste("ab".to_owned()), &registry);
        let _ = agent.handle_input(
            &Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            &registry,
        );
        let outcome = agent.handle_input(&Event::Paste("中\r\n".to_owned()), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert_eq!(
            agent
                .scrollback_search
                .as_ref()
                .map(ScrollbackSearchState::query),
            Some("a中b")
        );
        assert_eq!(agent.prompt.text(), "hidden prompt");
        agent.scrollback_search.as_mut().unwrap().accept();
        let outcome = agent.handle_input(&Event::Paste("ignored".to_owned()), &registry);
        assert!(matches!(outcome, InputOutcome::Unchanged));
        assert_eq!(
            agent
                .scrollback_search
                .as_ref()
                .map(ScrollbackSearchState::query),
            Some("a中b")
        );
        assert_eq!(agent.prompt.text(), "hidden prompt");
        agent.scrollback_search = None;
        let outcome = agent.handle_input(&Event::Paste("forwarded".to_owned()), &registry);
        assert!(matches!(
            outcome,
            InputOutcome::ActionThenForward(crate::app::actions::Action::FocusPrompt)
        ));
        assert_eq!(agent.prompt.text(), "hidden prompt");
    }
}
