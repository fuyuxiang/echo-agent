//! Mouse input handling for [`AgentView`]: the `handle_mouse` event handler
//! (click-to-focus, hit-testing of cached click rects, per-pane click
//! dispatch) and the scrollbar click helper.
//!
//! Extracted from `agent_view.rs` as a sibling `impl AgentView` block (same
//! pattern as `queue_edit.rs`).
//!
//! Hit-tests here assume the cached rects come from the last rendered frame.
use super::actions::Action;
use super::agent_view::{
    AgentPane, AgentView, CONTEXT_CLICK_DEBOUNCE_MS, CtaPhase, MULTI_CLICK_TIMEOUT_MS, PromptMode,
    TextClickState, is_link_modifier_held, is_text_selection_on_double_click,
};
use super::app_view::InputOutcome;
use crate::scrollback::block::BlockContent;
use crate::views::btw_overlay::BTW_OVERLAY_ENTRY_IDX;
use crate::views::prompt_widget::PromptEvent;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use std::time::Instant;
impl AgentView {
    /// Time-paired multi-click check for the prompt textarea. Pairing is
    /// time-only (no coordinates); a mispaired action is one undo step.
    /// Records the click for the next pairing.
    pub(super) fn prompt_click_is_double(&mut self) -> bool {
        let now = std::time::Instant::now();
        let is_double = self
            .last_prompt_click_ms
            .is_some_and(|last| now.duration_since(last).as_millis() < MULTI_CLICK_TIMEOUT_MS);
        self.last_prompt_click_ms = Some(now);
        is_double
    }
    /// Handle mouse events: click-to-focus, forward to prompt textarea.
    ///
    /// Scroll events are handled at app level (not here).
    pub(super) fn handle_mouse(&mut self, mouse: &MouseEvent) -> InputOutcome {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.left_mouse_down = true;
                if self.hit_todo_close.contains(mouse.column, mouse.row) {
                    self.todo.overlay.escape();
                    self.todo.on_state_change();
                    if self.active_pane == AgentPane::Todo {
                        self.set_active_pane(AgentPane::Scrollback, false);
                    }
                    return InputOutcome::Changed;
                }
                if self.hit_queue_close.contains(mouse.column, mouse.row) {
                    self.queue.overlay.escape();
                    self.queue.on_state_change();
                    if self.active_pane == AgentPane::Queue {
                        self.set_active_pane(AgentPane::Scrollback, false);
                    }
                    return InputOutcome::Changed;
                }
                if self.hit_bg_status.contains(mouse.column, mouse.row) {
                    self.tasks.overlay.toggle();
                    self.tasks.on_state_change();
                    if self.tasks.overlay.focused {
                        self.set_active_pane(AgentPane::Tasks, false);
                    } else if self.active_pane == AgentPane::Tasks {
                        self.set_active_pane(AgentPane::Scrollback, false);
                    }
                    return InputOutcome::Changed;
                }
                if self.hit_goal_status.contains(mouse.column, mouse.row) {
                    if !self.workflow_runs.is_empty() {
                        self.show_workflows = !self.show_workflows;
                        if self.show_workflows {
                            self.workflows_view.reset();
                            self.show_goal_detail = false;
                        }
                    } else if self.goal_state.is_some() {
                        self.show_goal_detail = !self.show_goal_detail;
                    }
                    return InputOutcome::Changed;
                }
                if self.hit_context.contains(mouse.column, mouse.row) {
                    let now = Instant::now();
                    let too_soon = self.last_context_click_at.is_some_and(|t| {
                        now.duration_since(t).as_millis() < CONTEXT_CLICK_DEBOUNCE_MS
                    });
                    if too_soon {
                        return InputOutcome::Unchanged;
                    }
                    self.last_context_click_at = Some(now);
                    return InputOutcome::Action(Action::ShowContextInfo);
                }
                if self.hit_plan_button.contains(mouse.column, mouse.row) {
                    if self.plan_approval_view.is_some() {
                        self.reopen_plan_approval();
                    } else {
                        self.show_plan_preview();
                    }
                    return InputOutcome::Changed;
                }
                if self
                    .hit_plan_approval_status
                    .contains(mouse.column, mouse.row)
                {
                    if self.plan_approval_view.is_some() {
                        self.reopen_plan_approval();
                    } else {
                        self.show_plan_preview();
                    }
                    return InputOutcome::Changed;
                }
                if self.hit_catalog_close.contains(mouse.column, mouse.row) {
                    self.catalog.overlay.escape();
                    self.catalog.on_state_change();
                    if self.active_pane == AgentPane::Catalog {
                        self.set_active_pane(AgentPane::Scrollback, false);
                    }
                    return InputOutcome::Changed;
                }
                if self.hit_bg_close.contains(mouse.column, mouse.row) {
                    self.tasks.overlay.escape();
                    self.tasks.on_state_change();
                    if self.active_pane == AgentPane::Tasks {
                        self.set_active_pane(AgentPane::Scrollback, false);
                    }
                    return InputOutcome::Changed;
                }
                if self.hit_bg_button.contains(mouse.column, mouse.row)
                    && !self.pos_occluded(mouse.column, mouse.row)
                {
                    return InputOutcome::Action(Action::DemoteToBackground);
                }
                if self.hit_cancel_button.contains(mouse.column, mouse.row)
                    && !self.pos_occluded(mouse.column, mouse.row)
                {
                    self.cancel_trigger_hint = Some(crate::app::actions::CancelTrigger::Mouse);
                    return InputOutcome::Action(Action::CancelTurn);
                }
                if self
                    .privacy_banner
                    .hit_opt_in
                    .contains(mouse.column, mouse.row)
                    && !self.pos_occluded(mouse.column, mouse.row)
                {
                    return InputOutcome::Action(Action::PrivacyBannerOptIn);
                }
                if self
                    .privacy_banner
                    .hit_opt_out
                    .contains(mouse.column, mouse.row)
                    && !self.pos_occluded(mouse.column, mouse.row)
                {
                    return InputOutcome::Action(Action::PrivacyBannerOptOut);
                }
                if self
                    .privacy_banner
                    .hit_terms
                    .contains(mouse.column, mouse.row)
                    && !self.pos_occluded(mouse.column, mouse.row)
                {
                    return InputOutcome::Action(Action::OpenUrl(
                        crate::views::privacy_banner::PRIVACY_BANNER_TERMS_URL.to_string(),
                    ));
                }
                if self
                    .privacy_banner
                    .hit_policy
                    .contains(mouse.column, mouse.row)
                    && !self.pos_occluded(mouse.column, mouse.row)
                {
                    return InputOutcome::Action(Action::OpenUrl(
                        crate::views::privacy_banner::PRIVACY_BANNER_POLICY_URL.to_string(),
                    ));
                }
                if self.hit_watching_cue.contains(mouse.column, mouse.row)
                    && !self.pos_occluded(mouse.column, mouse.row)
                {
                    let was_visible = self.tasks.overlay.visible;
                    self.tasks.overlay.toggle();
                    self.tasks.on_state_change();
                    if self.tasks.overlay.focused {
                        self.set_active_pane(AgentPane::Tasks, false);
                    } else if self.active_pane == AgentPane::Tasks {
                        self.set_active_pane(AgentPane::Scrollback, false);
                    }
                    if !was_visible && !self.watching_cue_toast_shown {
                        self.watching_cue_toast_shown = true;
                        self.show_toast("Tip: Ctrl+G toggles the tasks pane");
                    }
                    return InputOutcome::Changed;
                }
                if self.hit_announcement_hide.contains(mouse.column, mouse.row)
                    && !self.pos_occluded(mouse.column, mouse.row)
                {
                    return InputOutcome::Action(Action::AnnouncementsHide);
                }
                if self.hit_announcement_cta.contains(mouse.column, mouse.row)
                    && !self.pos_occluded(mouse.column, mouse.row)
                {
                    return InputOutcome::Action(Action::AnnouncementsOpenCta(
                        echo_agent_telemetry::events::AnnouncementCtaSurface::Banner,
                    ));
                }
                if self
                    .plugin_cta
                    .hit_dismiss
                    .contains(mouse.column, mouse.row)
                    && let CtaPhase::Matched { name, .. } | CtaPhase::Error { name, .. } =
                        &self.plugin_cta.phase
                {
                    let plugin_id = name.clone();
                    if let Err(e) = echo_agent_runtime::config::add_dismissed_plugin_cta(&plugin_id)
                    {
                        tracing::warn!(error = %e, "couldn't persist plugin CTA dismissal");
                    }
                    self.plugin_cta.dismissed.insert(plugin_id.clone());
                    echo_agent_telemetry::session_ctx::log_event(
                        echo_agent_telemetry::events::PluginCtaDismissed {
                            plugin_name: plugin_id,
                        },
                    );
                    self.plugin_cta.phase = CtaPhase::Hidden;
                    self.plugin_cta.hit_connect.clear();
                    self.plugin_cta.hit_dismiss.clear();
                    return InputOutcome::Changed;
                }
                if self
                    .plugin_cta
                    .hit_connect
                    .contains(mouse.column, mouse.row)
                    && matches!(
                        self.plugin_cta.phase,
                        CtaPhase::Matched { .. } | CtaPhase::Error { .. }
                    )
                {
                    self.connect_matched_plugin();
                    return InputOutcome::Changed;
                }
                if let Some(idx) = self.follow_up_chip_at(mouse.column, mouse.row)
                    && let Some(text) = self
                        .follow_ups
                        .as_ref()
                        .and_then(|f| f.suggestions.get(idx))
                        .cloned()
                {
                    return InputOutcome::Action(Action::SubmitFollowUp(text));
                }
                if self.hit_voice_stop_button.contains(mouse.column, mouse.row) {
                    return InputOutcome::Action(Action::VoiceToggle);
                }
                if self.hit_upgrade_cta.contains(mouse.column, mouse.row)
                    && !self.pos_occluded(mouse.column, mouse.row)
                {
                    return InputOutcome::Action(Action::AnnouncementsOpenCta(
                        echo_agent_telemetry::events::AnnouncementCtaSurface::Header,
                    ));
                }
                if self.hit_cwd.contains(mouse.column, mouse.row) {
                    let path = self.session.cwd.display().to_string();
                    self.copy_to_clipboard(&path);
                    return InputOutcome::Changed;
                }
                if self.hit_follow_indicator.contains(mouse.column, mouse.row) {
                    self.scrollback.goto_bottom();
                    return InputOutcome::Changed;
                }
                if self
                    .hit_response_top_indicator
                    .contains(mouse.column, mouse.row)
                {
                    self.scrollback.prev_response();
                    return InputOutcome::Changed;
                }
                if let Some(hd_area) = self.history_dropdown_area
                    && hd_area.contains((mouse.column, mouse.row).into())
                    && self.prompt.history_search.is_active()
                {
                    let vis_row = (mouse.row - hd_area.y) as usize;
                    let sel = self.prompt.history_search.selected;
                    let visible_rows = hd_area.height as usize;
                    let scroll_offset = if sel >= visible_rows {
                        sel - (visible_rows - 1)
                    } else {
                        0
                    };
                    let row_idx = scroll_offset + vis_row;
                    self.prompt.history_search.set_hovered(Some(row_idx));
                    if self.prompt.history_search.select_hovered()
                        && let Some(text) = self
                            .prompt
                            .history_search
                            .selected_text()
                            .map(str::to_owned)
                    {
                        self.prompt.history_search.deactivate();
                        self.accept_history_entry(&text);
                    }
                    self.set_active_pane(AgentPane::Prompt, false);
                    return InputOutcome::Changed;
                }
                if let Some(dd_area) = self.dropdown_items_area
                    && dd_area.contains((mouse.column, mouse.row).into())
                {
                    let has_scrollbar =
                        self.prompt.file_search.result_count() > dd_area.height as usize;
                    let on_scrollbar = has_scrollbar
                        && mouse.column >= dd_area.x + dd_area.width.saturating_sub(2);
                    if on_scrollbar {
                        let click_frac =
                            (mouse.row - dd_area.y) as f64 / dd_area.height.max(1) as f64;
                        let target =
                            (click_frac * self.prompt.file_search.result_count() as f64) as usize;
                        let max = self.prompt.file_search.result_count().saturating_sub(1);
                        self.prompt.file_search.move_selection(
                            target.min(max) as isize - self.prompt.file_search.selected() as isize,
                        );
                    } else {
                        let row_idx = (mouse.row - dd_area.y) as usize
                            + self.prompt.file_search.scroll_offset();
                        self.prompt.file_search.set_hovered(Some(row_idx));
                        if self.prompt.file_search.select_hovered() {
                            self.prompt.accept_file_search_result();
                        }
                    }
                    self.set_active_pane(AgentPane::Prompt, false);
                    return InputOutcome::Changed;
                }
                if let Some(dd_area) = self.slash_dropdown_items_area
                    && dd_area.contains((mouse.column, mouse.row).into())
                {
                    let snap = self.prompt.slash_snapshot();
                    let has_scrollbar = self.slash_dropdown_hit.has_scrollbar;
                    let on_scrollbar = has_scrollbar
                        && mouse.column >= dd_area.x + dd_area.width.saturating_sub(2);
                    if on_scrollbar {
                        let click_frac =
                            (mouse.row - dd_area.y) as f64 / dd_area.height.max(1) as f64;
                        let target = (click_frac * snap.matches.len() as f64) as usize;
                        let max = snap.matches.len().saturating_sub(1);
                        let delta = target.min(max) as isize - snap.selected as isize;
                        self.prompt.slash_move_selection(delta);
                        self.prompt.slash_preview_current_selection();
                    } else if let Some(&item_idx) = self
                        .slash_dropdown_hit
                        .row_items
                        .get((mouse.row - dd_area.y) as usize)
                    {
                        self.prompt.set_slash_hovered(Some(item_idx));
                        if self.prompt.select_slash_hovered() {
                            self.prompt.slash_commit_preview();
                            self.prompt.accept_slash_completion(&self.session.models);
                        }
                    }
                    self.set_active_pane(AgentPane::Prompt, false);
                    return InputOutcome::Changed;
                }
                if let Some(dd_area) = self.completion_dropdown_items_area
                    && dd_area.contains((mouse.column, mouse.row).into())
                {
                    let dd = &self.prompt.suggestions.dropdown;
                    let has_scrollbar = dd.items.len() > dd_area.height as usize;
                    let on_scrollbar = has_scrollbar
                        && mouse.column >= dd_area.x + dd_area.width.saturating_sub(2);
                    if on_scrollbar {
                        let click_frac =
                            (mouse.row - dd_area.y) as f64 / dd_area.height.max(1) as f64;
                        let total = self.prompt.suggestions.dropdown.items.len();
                        let target = (click_frac * total as f64) as usize;
                        let max = total.saturating_sub(1);
                        let delta = target.min(max) as isize
                            - self.prompt.suggestions.dropdown.selected as isize;
                        self.prompt.completion_dropdown_move(delta);
                    } else {
                        let offset = crate::views::completion_dropdown::scroll_offset(
                            &self.prompt.suggestions.dropdown,
                        );
                        let row_idx = (mouse.row - dd_area.y) as usize + offset;
                        self.prompt.set_completion_hovered(Some(row_idx));
                        if self.prompt.select_completion_hovered() {
                            self.accept_completion_dropdown_item();
                        }
                    }
                    self.set_active_pane(AgentPane::Prompt, false);
                    return InputOutcome::Changed;
                }
                if self.hit_scrollbar.contains(mouse.column, mouse.row) {
                    self.set_active_pane(AgentPane::Scrollback, false);
                    self.scrollbar_dragging = true;
                    self.apply_scrollbar_click(mouse.row);
                    return InputOutcome::Changed;
                }
                if let Some(rail) = self.timeline_rail.as_ref()
                    && rail.rect.contains((mouse.column, mouse.row).into())
                {
                    let target = rail
                        .hit(mouse.column, mouse.row)
                        .and_then(|hit| crate::views::timeline::chevron_target(rail, hit));
                    if let Some(turn_idx) = target {
                        self.set_active_pane(AgentPane::Scrollback, false);
                        self.scrollback.jump_to_turn(turn_idx);
                        return InputOutcome::Changed;
                    }
                    return InputOutcome::Unchanged;
                }
                if self.hit_sb_copy.contains(mouse.column, mouse.row) {
                    return InputOutcome::Action(Action::CopyBlockContent);
                }
                if self.hit_sb_view.contains(mouse.column, mouse.row) {
                    return InputOutcome::Action(Action::OpenBlockViewer);
                }
                if self.last_btw_area.area() > 0
                    && self
                        .last_btw_area
                        .contains((mouse.column, mouse.row).into())
                    && !self.hit_btw_close.contains(mouse.column, mouse.row)
                {
                    self.set_active_pane(AgentPane::Prompt, false);
                    self.btw_focused = true;
                    if is_link_modifier_held(mouse.modifiers)
                        && self.try_arm_link_click(mouse.column, mouse.row)
                    {
                        self.pending_scrollback_click = None;
                        return InputOutcome::Changed;
                    }
                    self.pending_link_click = None;
                    if self.begin_pending_btw_text_drag(mouse) {
                        return InputOutcome::Changed;
                    }
                    return InputOutcome::Unchanged;
                }
                match self.pane_areas.hit_test(mouse.column, mouse.row) {
                    Some(AgentPane::Todo) => {
                        self.set_active_pane(AgentPane::Todo, false);
                        self.todo.handle_mouse(
                            mouse.kind,
                            mouse.column,
                            mouse.row,
                            self.pane_areas.todo,
                        );
                        InputOutcome::Changed
                    }
                    Some(AgentPane::Queue) => {
                        if let Some(id) = self.queue.delete_click(mouse.column, mouse.row) {
                            let (is_server, row) = self.resolve_queue_row(id);
                            if is_server {
                                if let (Some(_sid), Some(row)) =
                                    (self.session.session_id.as_ref(), row)
                                    && let Some(server_id) = row.server_id
                                {
                                    self.shared_queue.retain(|e| e.id != server_id);
                                    if self.visible_queue_is_empty() {
                                        self.hide_queue_pane();
                                    }
                                    return InputOutcome::Action(Action::QueueRemoveShared {
                                        id: server_id,
                                        expected_version: row.version,
                                    });
                                }
                                return InputOutcome::Changed;
                            }
                            let was_drain_blocked = self.drain_blocked();
                            self.remove_local_queue_row(id);
                            if was_drain_blocked {
                                return InputOutcome::Action(Action::DrainQueue);
                            }
                            return InputOutcome::Changed;
                        }
                        if let Some(id) = self.queue.send_now_click(mouse.column, mouse.row)
                            && self.session.state.is_turn_running()
                            && let InputOutcome::Action(action) = self.force_interject_queue_row(id)
                        {
                            return InputOutcome::Action(action);
                        }
                        if let Some(id) = self.queue.edit_click(mouse.column, mouse.row)
                            && (!matches!(self.prompt_mode, PromptMode::EditingQueued { .. })
                                || self.set_active_pane(AgentPane::Queue, false))
                        {
                            let (is_server, row) = self.resolve_queue_row(id);
                            self.enter_queue_edit(id, is_server, row);
                            return InputOutcome::Changed;
                        }
                        self.set_active_pane(AgentPane::Queue, false);
                        self.queue.handle_mouse(
                            mouse.kind,
                            mouse.column,
                            mouse.row,
                            self.pane_areas.queue,
                        );
                        InputOutcome::Changed
                    }
                    Some(AgentPane::Prompt) => {
                        let was_collapsed = self.active_pane != AgentPane::Prompt
                            && self.scrollback.appearance().prompt.collapse_unfocused;
                        self.set_active_pane(AgentPane::Prompt, false);
                        self.btw_focused = false;
                        if !was_collapsed {
                            if matches!(self.prompt.handle_mouse(mouse), PromptEvent::Edited) {
                                self.prompt.refresh_slash(&self.session.models);
                                if let Some(eff) = self.notify_suggestion_text_changed() {
                                    self.pending_effects.push(eff);
                                }
                            }
                            if self.prompt_click_is_double() {
                                if self.prompt.file_ref_near_cursor()
                                    && let Some((path, initial_range)) =
                                        self.prompt.file_ref_element_at_cursor()
                                {
                                    self.prompt.textarea.begin_undo_group();
                                    self.open_line_viewer(
                                        &std::path::PathBuf::from(path),
                                        initial_range,
                                    );
                                } else if self.prompt.expand_paste_element_at_cursor() {
                                    self.prompt.refresh_slash(&self.session.models);
                                }
                            }
                        }
                        InputOutcome::Changed
                    }
                    Some(AgentPane::Tasks) => {
                        use crate::views::tasks_pane::TaskEntryId;
                        self.set_active_pane(AgentPane::Tasks, false);
                        for (entry_id, rect) in &self.tasks.kill_button_rects {
                            if rect.contains((mouse.column, mouse.row).into()) {
                                match entry_id {
                                    TaskEntryId::BgTask(tid) => {
                                        return InputOutcome::Action(Action::KillBgTask(
                                            tid.clone(),
                                        ));
                                    }
                                    TaskEntryId::Agent(sid) => {
                                        return InputOutcome::Action(Action::KillSubagent(
                                            sid.clone(),
                                        ));
                                    }
                                    TaskEntryId::Scheduled(tid) => {
                                        return InputOutcome::Action(Action::CancelScheduledTask(
                                            tid.clone(),
                                        ));
                                    }
                                    TaskEntryId::Workflow(name) => {
                                        return InputOutcome::Action(
                                            Action::SendSlashCommandPreservingDraft(format!(
                                                "/workflow stop {name}"
                                            )),
                                        );
                                    }
                                }
                            }
                        }
                        for (entry_id, rect) in &self.tasks.view_button_rects {
                            if rect.contains((mouse.column, mouse.row).into()) {
                                match entry_id {
                                    TaskEntryId::BgTask(tid) => {
                                        let already_open = self
                                            .block_viewer
                                            .as_ref()
                                            .and_then(|v| v.bg_task_id.as_deref())
                                            == Some(tid);
                                        if already_open {
                                            self.block_viewer = None;
                                            return InputOutcome::Changed;
                                        }
                                        if let Some(task) = self.session.bg_tasks.get(tid) {
                                            let entry_id =
                                                task.scrollback_entry_id.unwrap_or_else(|| {
                                                    crate::scrollback::entry::EntryId::new(0)
                                                });
                                            let is_running = task.status
                                                == crate::app::agent::BgTaskStatus::Running;
                                            self.block_viewer = Some(
                                                crate::views::block_viewer::BlockViewerPane::for_bg_task(
                                                    entry_id,
                                                    tid,
                                                    &task.stdout,
                                                    is_running,
                                                ),
                                            );
                                            self.set_active_pane(AgentPane::Scrollback, true);
                                            return InputOutcome::Changed;
                                        }
                                    }
                                    TaskEntryId::Agent(sid) => {
                                        if let Some(child_sid) = self
                                            .subagent_sessions
                                            .iter()
                                            .find(|(_, info)| {
                                                info.subagent_id.as_ref() == sid.as_str()
                                            })
                                            .map(|(k, _)| k.clone())
                                            && self.subagent_views.contains_key(&child_sid)
                                        {
                                            self.open_subagent_fullscreen(child_sid);
                                            return InputOutcome::Changed;
                                        }
                                    }
                                    TaskEntryId::Scheduled(tid) => {
                                        if let Some(sid) = self
                                            .session
                                            .scheduled_tasks
                                            .get(tid)
                                            .and_then(|info| info.last_subagent_id.clone())
                                            && let Some(child_sid) = self
                                                .subagent_sessions
                                                .iter()
                                                .find(|(_, info)| {
                                                    info.subagent_id.as_ref() == sid.as_str()
                                                })
                                                .map(|(k, _)| k.clone())
                                            && self.subagent_views.contains_key(&child_sid)
                                        {
                                            self.open_subagent_fullscreen(child_sid);
                                            return InputOutcome::Changed;
                                        }
                                    }
                                    TaskEntryId::Workflow(_) => {}
                                }
                            }
                        }
                        self.tasks.handle_mouse(
                            mouse.kind,
                            mouse.column,
                            mouse.row,
                            self.pane_areas.tasks,
                        );
                        if let Some(group) = self.tasks.selected_header_group() {
                            self.tasks.toggle_group(group);
                            return InputOutcome::Changed;
                        }
                        let now = Instant::now();
                        if let Some(last) = self.last_bg_click
                            && now.duration_since(last).as_millis() < MULTI_CLICK_TIMEOUT_MS
                        {
                            if let Some(task_id) =
                                self.tasks.selected_task_id().map(|s| s.to_string())
                                && let Some(task) = self.session.bg_tasks.get(&task_id)
                            {
                                let entry_id = task
                                    .scrollback_entry_id
                                    .unwrap_or_else(|| crate::scrollback::entry::EntryId::new(0));
                                let is_running =
                                    task.status == crate::app::agent::BgTaskStatus::Running;
                                self.block_viewer =
                                    Some(crate::views::block_viewer::BlockViewerPane::for_bg_task(
                                        entry_id,
                                        &task_id,
                                        &task.stdout,
                                        is_running,
                                    ));
                                self.set_active_pane(AgentPane::Scrollback, true);
                                self.last_bg_click = None;
                                return InputOutcome::Changed;
                            }
                            if let Some(child_sid) = self.tasks.selected_child_session_id()
                                && self.subagent_views.contains_key(child_sid)
                            {
                                self.open_subagent_fullscreen(child_sid.to_string());
                                self.last_bg_click = None;
                                return InputOutcome::Changed;
                            }
                            if let Some(crate::views::tasks_pane::TaskEntry::Workflow {
                                name,
                                ..
                            }) = self.tasks.selected_entry()
                            {
                                let name = name.clone();
                                self.open_workflow_detail(&name);
                                self.last_bg_click = None;
                                return InputOutcome::Changed;
                            }
                        }
                        self.last_bg_click = Some(now);
                        InputOutcome::Changed
                    }
                    Some(AgentPane::Catalog) => {
                        self.set_active_pane(AgentPane::Catalog, false);
                        self.catalog.handle_mouse(
                            mouse.kind,
                            mouse.column,
                            mouse.row,
                            self.pane_areas.catalog,
                        );
                        InputOutcome::Changed
                    }
                    Some(AgentPane::Scrollback) => {
                        self.set_active_pane(AgentPane::Scrollback, false);
                        if self.block_viewer.is_some() {
                            self.handle_block_viewer_mouse(mouse);
                            return InputOutcome::Changed;
                        }
                        self.persistent_text_selection = None;
                        self.table_selection_geometry = None;
                        self.selection_created_at = None;
                        if is_link_modifier_held(mouse.modifiers)
                            && self.try_arm_link_click(mouse.column, mouse.row)
                        {
                            self.pending_scrollback_click = None;
                            return InputOutcome::Changed;
                        }
                        self.pending_link_click = None;
                        self.last_question_click = None;
                        self.last_permission_click = None;
                        self.pending_scrollback_click = Some((mouse.column, mouse.row));
                        tracing::debug!(
                            event = "scrollback_mouse_down",
                            col = mouse.column,
                            row = mouse.row,
                            area = ?self.pane_areas.scrollback,
                            content_area = ?self.last_scrollback_selection_model.content_area,
                            ranges = self.last_scrollback_selection_model.ranges.len(),
                            blocks = self.last_scrollback_selection_model.visible_blocks.len(),
                            hovered_entry = ?self.hovered_entry,
                            "scrollback mouse down"
                        );
                        if self.begin_pending_text_drag(mouse) {
                            return InputOutcome::Changed;
                        }
                        self.begin_pending_block_drag(mouse);
                        self.deferred_text_press = Some((mouse.column, mouse.row));
                        InputOutcome::Changed
                    }
                    None => {
                        let scrollback = self.pane_areas.scrollback;
                        let prompt = self.pane_areas.prompt;
                        let above_prompt_strip = self.block_viewer.is_none()
                            && scrollback.area() > 0
                            && prompt.area() > 0
                            && mouse.row >= scrollback.y.saturating_add(scrollback.height)
                            && mouse.row < prompt.y;
                        if above_prompt_strip {
                            self.persistent_text_selection = None;
                            self.table_selection_geometry = None;
                            self.selection_created_at = None;
                            self.deferred_text_press = Some((mouse.column, mouse.row));
                            InputOutcome::Changed
                        } else {
                            InputOutcome::Unchanged
                        }
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.pending_link_click = None;
                tracing::debug!(
                    event = "scrollback_mouse_drag",
                    col = mouse.column,
                    row = mouse.row,
                    pending = ?self.pending_text_drag,
                    active = ?self.drag_selection,
                    "scrollback mouse drag"
                );
                self.handle_scrollback_drag_motion(mouse)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.left_mouse_down = false;
                tracing::debug!(
                    event = "scrollback_mouse_up",
                    col = mouse.column,
                    row = mouse.row,
                    pending = ?self.pending_text_drag,
                    active = ?self.drag_selection,
                    "scrollback mouse up"
                );
                if self.scrollbar_dragging {
                    self.scrollbar_dragging = false;
                }
                self.deferred_text_press = None;
                if self.drag_selection.is_some() {
                    return if self.finish_text_drag() {
                        InputOutcome::Changed
                    } else {
                        InputOutcome::Unchanged
                    };
                }
                if self.block_drag_selection.is_some() {
                    return if self.finish_block_drag() {
                        InputOutcome::Changed
                    } else {
                        InputOutcome::Unchanged
                    };
                }
                let had_pending_text_drag = self.pending_text_drag.take().is_some();
                let _had_pending_block_drag = self.pending_block_drag.take().is_some();
                if let Some((lc, lr, target)) = self.pending_link_click.take()
                    && mouse.column == lc
                    && mouse.row == lr
                {
                    return InputOutcome::Action(Action::OpenLink(target));
                }
                if self.active_pane == AgentPane::Scrollback {
                    if let Some((click_col, click_row)) = self.pending_scrollback_click.take() {
                        let now = Instant::now();
                        if is_text_selection_on_double_click() {
                            let exact_text_hit = self
                                .last_scrollback_selection_model
                                .hit_test_text_exact(click_col, click_row);
                            if let Some(hit) = exact_text_hit {
                                let text_click_count = self.count_text_click(now, &hit);
                                let handled = match text_click_count {
                                    1 => {
                                        self.persistent_text_selection = None;
                                        self.table_selection_geometry = None;
                                        self.selection_created_at = None;
                                        if hit.entry_idx != BTW_OVERLAY_ENTRY_IDX {
                                            self.scrollback.set_selected(Some(hit.entry_idx));
                                        }
                                        true
                                    }
                                    2 => {
                                        self.select_word_at(&hit);
                                        true
                                    }
                                    3 => {
                                        if !self.select_cell_at(&hit) {
                                            self.select_paragraph_at(&hit);
                                        }
                                        true
                                    }
                                    _ => false,
                                };
                                if handled {
                                    self.last_text_click = Some(TextClickState {
                                        time: now,
                                        entry_idx: hit.entry_idx,
                                        range_id: hit.range_id,
                                        block_line_idx: hit.block_line_idx,
                                        col_within_range: hit.col_within_range,
                                        click_count: text_click_count,
                                    });
                                    self.last_click = None;
                                    return InputOutcome::Changed;
                                }
                                self.last_text_click = None;
                            }
                        }
                        self.last_text_click = None;
                        if let Some(outcome) = self.handle_inline_media_click(click_col, click_row)
                        {
                            return outcome;
                        }
                        let hit_idx = self
                            .scrollback
                            .entry_index_at_screen_row(click_row, self.pane_areas.scrollback);
                        if let Some(idx) = hit_idx {
                            let credit_click = self
                                .scrollback
                                .entry(idx)
                                .and_then(|entry| {
                                    if let crate::scrollback::block::RenderBlock::CreditLimit(
                                        ref blk,
                                    ) = entry.block
                                    {
                                        use crate::scrollback::blocks::CreditLimitCardAction;
                                        let choice = match blk.action {
                                            CreditLimitCardAction::PurchaseCredits => {
                                                echo_agent_telemetry::events::CreditLimitChoice::PurchaseCredits
                                            }
                                            CreditLimitCardAction::EnablePayg
                                            | CreditLimitCardAction::IncreasePaygLimit => {
                                                echo_agent_telemetry::events::CreditLimitChoice::PayAsYouGo
                                            }
                                        };
                                        Some((blk.url.clone(), choice))
                                    } else {
                                        None
                                    }
                                });
                            if let Some((url, choice)) = credit_click
                                && let Some((area, _, _)) = self
                                    .scrollback
                                    .entry_screen_area(idx, self.pane_areas.scrollback)
                            {
                                let url_row = area.y + area.height.saturating_sub(2);
                                if click_row >= url_row {
                                    self.scrollback.set_selected(Some(idx));
                                    echo_agent_telemetry::session_ctx::log_event(echo_agent_telemetry::events::CreditLimitUpsellClicked {
                                        surface: echo_agent_telemetry::events::CreditLimitUpsellSurface::InlineCard,
                                        choice,
                                    });
                                    self.open_url_or_show(&url);
                                    self.last_click = None;
                                    return InputOutcome::Changed;
                                }
                            }
                            let selectable = self
                                .scrollback
                                .get(idx)
                                .is_some_and(|e| e.block.is_selectable());
                            if selectable {
                                let header_row_click = self
                                    .scrollback
                                    .get_cached_entry_layouts()
                                    .and_then(|l| l.get(idx))
                                    .is_some_and(
                                        crate::scrollback::EntryLayoutInfo::is_expanded_verb_header,
                                    )
                                    && self
                                        .scrollback
                                        .entry_screen_area(idx, self.pane_areas.scrollback)
                                        .is_some_and(|(a, _, _)| click_row == a.y);
                                let (last_click, show_word_select_tip) =
                                    self.handle_scrollback_click(now, idx, header_row_click);
                                self.last_click = last_click;
                                if show_word_select_tip {
                                    return InputOutcome::Action(Action::ShowWordSelectTip);
                                }
                                return InputOutcome::Changed;
                            }
                        } else if let Some((last_time, last_idx, last_count)) = self.last_click
                            && now.duration_since(last_time).as_millis() < MULTI_CLICK_TIMEOUT_MS
                            && last_count >= 2
                        {
                            let (last_click, show_word_select_tip) =
                                self.handle_scrollback_click(now, last_idx, false);
                            self.last_click = last_click;
                            if show_word_select_tip {
                                return InputOutcome::Action(Action::ShowWordSelectTip);
                            }
                            return InputOutcome::Changed;
                        }
                        if had_pending_text_drag {
                            tracing::debug!(
                                event = "pending_text_drag_released_without_click_target",
                                col = click_col,
                                row = click_row,
                                "scrollback pending text drag released without valid click target"
                            );
                        }
                    }
                } else {
                    self.pending_scrollback_click = None;
                }
                if self.active_pane == AgentPane::Todo {
                    self.todo.handle_mouse(
                        mouse.kind,
                        mouse.column,
                        mouse.row,
                        self.pane_areas.todo,
                    );
                }
                if self.active_pane == AgentPane::Prompt {
                    let event = self.prompt.handle_mouse(mouse);
                    if matches!(event, PromptEvent::Edited)
                        && let Some(eff) = self.notify_suggestion_text_changed()
                    {
                        self.pending_effects.push(eff);
                    }
                    InputOutcome::Changed
                } else {
                    InputOutcome::Unchanged
                }
            }
            MouseEventKind::Moved => {
                tracing::debug!(
                    event = "scrollback_mouse_moved",
                    col = mouse.column,
                    row = mouse.row,
                    pending = ?self.pending_text_drag,
                    active = ?self.drag_selection,
                    left_mouse_down = self.left_mouse_down,
                    "scrollback mouse moved"
                );
                let suppress_scrollback_hover = self.pending_text_drag.is_some()
                    || self.drag_selection.is_some()
                    || self.pending_block_drag.is_some()
                    || self.block_drag_selection.is_some()
                    || self.deferred_text_press.is_some();
                let hit = self.pane_areas.hit_test(mouse.column, mouse.row);
                let on_rail = self
                    .timeline_rail
                    .as_ref()
                    .is_some_and(|r| r.rect.contains((mouse.column, mouse.row).into()));
                let new_timeline_hover = self
                    .timeline_rail
                    .as_ref()
                    .and_then(|r| r.hit(mouse.column, mouse.row));
                let new_hover = if on_rail || suppress_scrollback_hover {
                    None
                } else {
                    hit.and_then(|pane| match pane {
                        AgentPane::Scrollback => self
                            .scrollback
                            .entry_index_at_screen_row(mouse.row, self.pane_areas.scrollback)
                            .filter(|&idx| {
                                self.scrollback
                                    .get(idx)
                                    .is_some_and(|e| e.block.is_selectable())
                            }),
                        AgentPane::Todo
                        | AgentPane::Queue
                        | AgentPane::Prompt
                        | AgentPane::Tasks
                        | AgentPane::Catalog => None,
                    })
                };
                let new_prompt_hover = hit == Some(AgentPane::Prompt)
                    && self.scrollback.appearance().prompt.mouse_hover;
                let old_mouse_pos = self.last_mouse_pos;
                self.last_mouse_pos = (mouse.column, mouse.row);
                if old_mouse_pos != self.last_mouse_pos {
                    self.last_mouse_moved_at = Some(Instant::now());
                }
                let mut changed =
                    new_hover != self.hovered_entry || new_prompt_hover != self.hovered_prompt;
                if new_hover.is_some()
                    && old_mouse_pos != self.last_mouse_pos
                    && let Some(idx) = new_hover
                    && let Some(entry) = self.scrollback.get(idx)
                    && (entry.block.is_user_prompt()
                        || matches!(
                            entry.block,
                            crate::scrollback::block::RenderBlock::AgentMessage(_)
                                | crate::scrollback::block::RenderBlock::Btw(_)
                        )
                        || entry.hook_data.as_ref().is_some_and(|hd| hd.has_content()))
                {
                    changed = true;
                }
                self.hovered_entry = new_hover;
                self.hovered_prompt = new_prompt_hover;
                if new_timeline_hover != self.timeline_hover {
                    self.timeline_hover = new_timeline_hover;
                    self.sync_timeline_hover_preview();
                    changed = true;
                }
                changed |= self
                    .set_hovered_follow_up_chip(self.follow_up_chip_at(mouse.column, mouse.row));
                changed |= self.hit_context.update_hover(mouse.column, mouse.row);
                changed |= self.hit_credits.update_hover(mouse.column, mouse.row);
                changed |= self.hit_todo_close.update_hover(mouse.column, mouse.row);
                changed |= self.hit_queue_close.update_hover(mouse.column, mouse.row);
                if matches!(
                    self.pane_areas.hit_test(mouse.column, mouse.row),
                    Some(AgentPane::Queue)
                ) {
                    changed |= self.queue.update_delete_hover(mouse.column, mouse.row);
                    changed |= self.queue.update_send_now_hover(mouse.column, mouse.row);
                    changed |= self.queue.update_edit_hover(mouse.column, mouse.row);
                    changed |= self.queue.update_row_hover(mouse.column, mouse.row);
                } else {
                    changed |= self.queue.clear_delete_hover();
                    changed |= self.queue.clear_send_now_hover();
                    changed |= self.queue.clear_edit_hover();
                    changed |= self.queue.clear_row_hover();
                }
                changed |= self.hit_plan_button.update_hover(mouse.column, mouse.row);
                changed |= self
                    .hit_plan_approval_status
                    .update_hover(mouse.column, mouse.row);
                changed |= self
                    .hit_follow_indicator
                    .update_hover(mouse.column, mouse.row);
                changed |= self
                    .hit_response_top_indicator
                    .update_hover(mouse.column, mouse.row);
                changed |= self.hit_cancel_button.update_hover(mouse.column, mouse.row);
                changed |= self.hit_bg_button.update_hover(mouse.column, mouse.row);
                changed |= self.hit_watching_cue.update_hover(mouse.column, mouse.row);
                changed |= self
                    .hit_announcement_hide
                    .update_hover(mouse.column, mouse.row);
                changed |= self
                    .hit_announcement_cta
                    .update_hover(mouse.column, mouse.row);
                changed |= self
                    .privacy_banner
                    .hit_opt_in
                    .update_hover(mouse.column, mouse.row);
                changed |= self
                    .privacy_banner
                    .hit_opt_out
                    .update_hover(mouse.column, mouse.row);
                changed |= self
                    .privacy_banner
                    .hit_terms
                    .update_hover(mouse.column, mouse.row);
                changed |= self
                    .privacy_banner
                    .hit_policy
                    .update_hover(mouse.column, mouse.row);
                changed |= self
                    .plugin_cta
                    .hit_connect
                    .update_hover(mouse.column, mouse.row);
                changed |= self
                    .plugin_cta
                    .hit_dismiss
                    .update_hover(mouse.column, mouse.row);
                changed |= self
                    .hit_voice_stop_button
                    .update_hover(mouse.column, mouse.row);
                changed |= self.hit_bg_status.update_hover(mouse.column, mouse.row);
                changed |= self.hit_goal_status.update_hover(mouse.column, mouse.row);
                changed |= self.hit_bg_close.update_hover(mouse.column, mouse.row);
                changed |= self.hit_catalog_close.update_hover(mouse.column, mouse.row);
                changed |= self.hit_cwd.update_hover(mouse.column, mouse.row);
                changed |= self.hit_upgrade_cta.update_hover(mouse.column, mouse.row);
                {
                    let new_kill = self
                        .tasks
                        .kill_button_rects
                        .iter()
                        .find(|(_, rect)| rect.contains((mouse.column, mouse.row).into()))
                        .map(|(tid, _)| tid.clone());
                    if new_kill != self.tasks.hovered_kill {
                        self.tasks.hovered_kill = new_kill;
                        changed = true;
                    }
                    let new_view = self
                        .tasks
                        .view_button_rects
                        .iter()
                        .find(|(_, rect)| rect.contains((mouse.column, mouse.row).into()))
                        .map(|(tid, _)| tid.clone());
                    if new_view != self.tasks.hovered_view {
                        self.tasks.hovered_view = new_view;
                        changed = true;
                    }
                }
                changed |= self.hit_sb_copy.update_hover(mouse.column, mouse.row);
                changed |= self.hit_sb_view.update_hover(mouse.column, mouse.row);
                if let Some(hd_area) = self.history_dropdown_area {
                    let hs_count = self.prompt.history_search.result_count();
                    let has_sb = hs_count > hd_area.height as usize;
                    let on_sb =
                        has_sb && mouse.column >= hd_area.x + hd_area.width.saturating_sub(2);
                    let new_hover = if hd_area.contains((mouse.column, mouse.row).into()) && !on_sb
                    {
                        let vis_row = (mouse.row - hd_area.y) as usize;
                        let sel = self.prompt.history_search.selected;
                        let visible_rows = hd_area.height as usize;
                        let scroll_offset = if sel >= visible_rows {
                            sel - (visible_rows - 1)
                        } else {
                            0
                        };
                        Some(scroll_offset + vis_row)
                    } else {
                        None
                    };
                    changed |= self.prompt.history_search.set_hovered(new_hover);
                } else {
                    changed |= self.prompt.history_search.set_hovered(None);
                }
                if let Some(dd_area) = self.dropdown_items_area {
                    let has_scrollbar =
                        self.prompt.file_search.result_count() > dd_area.height as usize;
                    let on_scrollbar = has_scrollbar
                        && mouse.column >= dd_area.x + dd_area.width.saturating_sub(2);
                    let new_dd_hover =
                        if dd_area.contains((mouse.column, mouse.row).into()) && !on_scrollbar {
                            Some(
                                (mouse.row - dd_area.y) as usize
                                    + self.prompt.file_search.scroll_offset(),
                            )
                        } else {
                            None
                        };
                    changed |= self.prompt.file_search.set_hovered(new_dd_hover);
                } else {
                    changed |= self.prompt.file_search.set_hovered(None);
                }
                if let Some(dd_area) = self.slash_dropdown_items_area {
                    let has_scrollbar = self.slash_dropdown_hit.has_scrollbar;
                    let on_scrollbar = has_scrollbar
                        && mouse.column >= dd_area.x + dd_area.width.saturating_sub(2);
                    let new_hover =
                        if dd_area.contains((mouse.column, mouse.row).into()) && !on_scrollbar {
                            self.slash_dropdown_hit
                                .row_items
                                .get((mouse.row - dd_area.y) as usize)
                                .copied()
                        } else {
                            None
                        };
                    changed |= self.prompt.set_slash_hovered(new_hover);
                } else {
                    changed |= self.prompt.set_slash_hovered(None);
                }
                if let Some(dd_area) = self.completion_dropdown_items_area {
                    let dd = &self.prompt.suggestions.dropdown;
                    let has_scrollbar = dd.items.len() > dd_area.height as usize;
                    let on_scrollbar = has_scrollbar
                        && mouse.column >= dd_area.x + dd_area.width.saturating_sub(2);
                    let new_hover =
                        if dd_area.contains((mouse.column, mouse.row).into()) && !on_scrollbar {
                            let offset = crate::views::completion_dropdown::scroll_offset(dd);
                            Some((mouse.row - dd_area.y) as usize + offset)
                        } else {
                            None
                        };
                    changed |= self.prompt.set_completion_hovered(new_hover);
                } else {
                    changed |= self.prompt.set_completion_hovered(None);
                }
                changed |= self.update_hovered_link(is_link_modifier_held(mouse.modifiers));
                if changed {
                    InputOutcome::Changed
                } else {
                    InputOutcome::Unchanged
                }
            }
            _ => InputOutcome::Unchanged,
        }
    }
    /// Apply a scrollbar click/drag at the given screen row.
    ///
    /// Uses [`scrollbar_click_to_offset`] (same math as the thumb renderer)
    /// so the resulting scroll position is the exact inverse of where the
    /// thumb would be drawn for that offset.
    pub(super) fn apply_scrollbar_click(&mut self, screen_y: u16) -> bool {
        use crate::render::scrollbar::{ScrollbarClickResult, scrollbar_click_to_offset};
        let Some(sb) = self.hit_scrollbar.rect else {
            return false;
        };
        if sb.height == 0 {
            return false;
        }
        let cell_index = screen_y.saturating_sub(sb.y);
        let (_, viewport_height, total_height) = self.scrollback.scroll_info();
        let scale = if total_height > u16::MAX as usize {
            (total_height / u16::MAX as usize) + 1
        } else {
            1
        };
        let scaled_total = (total_height / scale) as u16;
        let result =
            scrollbar_click_to_offset(cell_index, sb.height, scaled_total, viewport_height);
        match result {
            ScrollbarClickResult::Top => {
                self.scrollback.goto_top();
            }
            ScrollbarClickResult::Bottom => {
                self.scrollback.goto_bottom();
            }
            ScrollbarClickResult::Offset(offset) => {
                self.scrollback
                    .set_scroll_offset(offset.saturating_mul(scale));
            }
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::agent::AgentState;
    use crate::app::agent_view::PromptMode;
    use crate::app::agent_view::test_fixtures::{
        make_running_agent, running_agent_local_only, test_pasted_image,
    };
    use crossterm::event::KeyModifiers;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    /// Render the queue, locate the action-button cell where `hit` resolves
    /// to `selected_id`, and dispatch a left-click on it; returns the outcome.
    fn click_queue_button(
        agent: &mut AgentView,
        selected_id: u64,
        hit: impl Fn(&AgentView, u16, u16) -> Option<u64>,
    ) -> InputOutcome {
        agent.queue.list_state.select_by_id(selected_id);
        let area = Rect::new(0, 0, 80, 6);
        let mut buf = Buffer::empty(area);
        let layout_cfg = crate::appearance::LayoutConfig::default();
        let running = agent.session.state.is_turn_running();
        agent
            .queue
            .render(area, &mut buf, true, &layout_cfg, None, running);
        agent.pane_areas.queue = area;
        let mut found = None;
        'find: for row in area.y..area.y + area.height {
            for col in area.x..area.x + area.width {
                if hit(agent, col, row) == Some(selected_id) {
                    found = Some((col, row));
                    break 'find;
                }
            }
        }
        let (col, row) = found.expect("queue button must render for the selected row");
        agent.handle_mouse(&MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::empty(),
        })
    }
    /// Left-click the row's `[Interject]` (send-now) button.
    fn click_send_now(agent: &mut AgentView, selected_id: u64) -> InputOutcome {
        click_queue_button(agent, selected_id, |a, c, r| a.queue.send_now_click(c, r))
    }
    /// Left-click the row's `[cancel]` (delete) button.
    fn click_delete(agent: &mut AgentView, selected_id: u64) -> InputOutcome {
        click_queue_button(agent, selected_id, |a, c, r| a.queue.delete_click(c, r))
    }
    /// Left-click the row's `[edit]` button.
    fn click_edit(agent: &mut AgentView, selected_id: u64) -> InputOutcome {
        click_queue_button(agent, selected_id, |a, c, r| a.queue.edit_click(c, r))
    }
    /// Mouse "Send now" (interject) on the last local row keeps the pane open
    /// when a server row remains — the third sibling site of the same fix.
    #[test]
    fn mouse_send_now_last_local_row_keeps_pane_open_when_server_remains() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Queue;
        agent.session.pending_prompts[0]
            .images
            .push(test_pasted_image());
        let ids = agent.queue.entry_ids();
        let outcome = click_send_now(&mut agent, ids[1]);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, images }) => {
                assert_eq!(text, "local one");
                assert_eq!(images.len(), 1, "row image must ride the send-now");
            }
            other => panic!("expected SendPromptNow action, got {other:?}"),
        }
        assert!(agent.session.pending_prompts.is_empty());
        assert_eq!(agent.shared_queue.len(), 1);
        assert!(agent.queue.overlay.visible);
        assert!(agent.queue.overlay.focused);
        assert_eq!(agent.active_pane, AgentPane::Queue);
    }
    /// Hide via the mouse "Send now" path (site 3): with no server rows left,
    /// interjecting the last local row empties the merged view → hide.
    #[test]
    fn mouse_send_now_last_local_row_hides_pane_when_shared_queue_empty() {
        let mut agent = running_agent_local_only();
        let ids = agent.queue.entry_ids();
        assert_eq!(ids.len(), 1);
        let outcome = click_send_now(&mut agent, ids[0]);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "local one")
            }
            other => panic!("expected SendPromptNow action, got {other:?}"),
        }
        assert!(agent.session.pending_prompts.is_empty());
        assert!(!agent.queue.overlay.visible);
        assert!(!agent.queue.overlay.focused);
        assert_eq!(agent.active_pane, AgentPane::Scrollback);
    }
    /// Send-now `[Interject]` on the lone local row while it is being
    /// DIRTY-edited: the removal must discard the edit via the canonical
    /// helper — the old inline removal stranded `EditingQueued` and armed
    /// the invisible modal.
    #[test]
    fn mouse_send_now_edited_lone_local_row_discards_edit_without_orphaned_modal() {
        let mut agent = running_agent_local_only();
        let ids = agent.queue.entry_ids();
        agent.stashed_prompt = Some(crate::views::prompt_widget::StashedPrompt {
            text: "draft".into(),
            cursor: 0,
            images: Vec::new(),
            chip_elements: Vec::new(),
            image_counter: 0,
            image_undo_stash: Vec::new(),
        });
        agent.prompt_mode = PromptMode::EditingQueued {
            id: ids[0],
            original: "local one".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        agent.prompt.set_text("local one EDITED");
        let outcome = click_send_now(&mut agent, ids[0]);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "local one")
            }
            other => panic!("expected SendPromptNow action, got {other:?}"),
        }
        assert!(agent.session.pending_prompts.is_empty());
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert!(
            agent.active_modal.is_none(),
            "orphaned EditConfirm would brick input"
        );
        assert_eq!(agent.prompt.text(), "draft");
        assert!(!agent.queue.overlay.visible);
    }
    /// Mouse `[cancel]` of the FRONT local row being edited while idle:
    /// discarding the edit releases the drain block, so the click must kick
    /// `DrainQueue` like the modal Delete arm (the row behind must not sit
    /// stuck until an unrelated trigger).
    #[test]
    fn mouse_delete_edited_front_row_while_idle_kicks_drain() {
        let mut agent = running_agent_local_only();
        agent.session.state = AgentState::Idle;
        agent.session.enqueue_prompt("local two".to_string());
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            None,
            None,
            &agent.send_now_painted_blocks,
        );
        let ids = agent.queue.entry_ids();
        agent.stashed_prompt = Some(crate::views::prompt_widget::StashedPrompt {
            text: "draft".into(),
            cursor: 0,
            images: Vec::new(),
            chip_elements: Vec::new(),
            image_counter: 0,
            image_undo_stash: Vec::new(),
        });
        agent.prompt_mode = PromptMode::EditingQueued {
            id: ids[0],
            original: "local one".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        agent.prompt.set_text("local one EDITED");
        let outcome = click_delete(&mut agent, ids[0]);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::DrainQueue)),
            "deleting the edited front row must kick the drain, got {outcome:?}"
        );
        assert_eq!(agent.session.pending_prompts.len(), 1);
        assert_eq!(agent.session.pending_prompts[0].text, "local two");
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert!(agent.active_modal.is_none());
        assert_eq!(agent.prompt.text(), "draft");
    }
    /// Mouse `[edit]` on a queued row enters the same queued-edit flow as the
    /// keyboard `e` (`QueueEvent::EditSelected` → `enter_queue_edit`): the
    /// composer loads the row text and the prompt pane takes focus in
    /// `EditingQueued` mode, leaving the row itself queued.
    #[test]
    fn mouse_edit_click_enters_queued_edit_mode() {
        let mut agent = running_agent_local_only();
        let ids = agent.queue.entry_ids();
        let outcome = click_edit(&mut agent, ids[0]);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "edit click redraws without dispatching an action, got {outcome:?}"
        );
        match &agent.prompt_mode {
            PromptMode::EditingQueued {
                id,
                original,
                server_id,
                ..
            } => {
                assert_eq!(*id, ids[0]);
                assert_eq!(original, "local one");
                assert!(
                    server_id.is_none(),
                    "local row must not take the server edit path"
                );
            }
            other => panic!("expected EditingQueued, got {other:?}"),
        }
        assert_eq!(agent.prompt.text(), "local one");
        assert_eq!(agent.active_pane, AgentPane::Prompt);
        assert_eq!(agent.session.pending_prompts.len(), 1);
    }
    /// Clicking another row's `[edit]` while a DIRTY queued edit is active
    /// must not re-enter `enter_queue_edit` — that would bypass the
    /// dirty-edit lock and overwrite `stashed_prompt`, so Esc would restore
    /// the edit text instead of the user's original draft. The click falls
    /// through to the pane switch, which the lock blocks.
    #[test]
    fn mouse_edit_click_during_dirty_edit_preserves_first_edit_and_stash() {
        let mut agent = make_running_agent();
        let ids = agent.queue.entry_ids();
        agent.stashed_prompt = Some(crate::views::prompt_widget::StashedPrompt {
            text: "draft".into(),
            cursor: 0,
            images: Vec::new(),
            chip_elements: Vec::new(),
            image_counter: 0,
            image_undo_stash: Vec::new(),
        });
        agent.prompt_mode = PromptMode::EditingQueued {
            id: ids[1],
            original: "local one".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        agent.prompt.set_text("local one EDITED");
        agent.active_pane = AgentPane::Prompt;
        let outcome = click_edit(&mut agent, ids[0]);
        assert!(matches!(outcome, InputOutcome::Changed), "got {outcome:?}");
        match &agent.prompt_mode {
            PromptMode::EditingQueued { id, original, .. } => {
                assert_eq!(*id, ids[1], "the first edit's target row must survive");
                assert_eq!(original, "local one");
            }
            other => panic!("expected the first edit to stay active, got {other:?}"),
        }
        assert_eq!(agent.prompt.text(), "local one EDITED");
        assert_eq!(
            agent.stashed_prompt.as_ref().map(|s| s.text.as_str()),
            Some("draft"),
            "the pre-edit draft must survive for Esc-restore"
        );
        assert!(
            agent.pending_effects.is_empty(),
            "no hold effect may be emitted for the clicked row"
        );
    }
    /// Same guard for a dirty SERVER-row edit: clicking another row's
    /// `[edit]` must not replace the edit (which would strand the first
    /// row's combine hold) nor emit a second `QueueHoldEdit`.
    #[test]
    fn mouse_edit_click_during_dirty_server_edit_keeps_hold_target() {
        let mut agent = make_running_agent();
        let ids = agent.queue.entry_ids();
        agent.prompt_mode = PromptMode::EditingQueued {
            id: ids[0],
            original: "server one".into(),
            server_id: Some("p1".into()),
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        agent.prompt.set_text("server one EDITED");
        agent.active_pane = AgentPane::Prompt;
        let outcome = click_edit(&mut agent, ids[1]);
        assert!(matches!(outcome, InputOutcome::Changed), "got {outcome:?}");
        match &agent.prompt_mode {
            PromptMode::EditingQueued { id, server_id, .. } => {
                assert_eq!(*id, ids[0], "the held server row must stay the edit target");
                assert_eq!(server_id.as_deref(), Some("p1"));
            }
            other => panic!("expected the server edit to stay active, got {other:?}"),
        }
        assert_eq!(agent.prompt.text(), "server one EDITED");
        assert!(
            agent.pending_effects.is_empty(),
            "no second QueueHoldEdit may be emitted while one row is held"
        );
    }
    /// Clicking another row's `[edit]` while a CLEAN (unchanged) edit is
    /// active must open the clicked row's edit on the SAME click: the
    /// canonical pane switch exits the clean edit — releasing its server
    /// combine hold — and the arm then enters the clicked row instead of
    /// letting the click die on the pane switch.
    #[test]
    fn mouse_edit_click_during_clean_edit_switches_to_clicked_row() {
        let mut agent = make_running_agent();
        let ids = agent.queue.entry_ids();
        agent.stashed_prompt = Some(crate::views::prompt_widget::StashedPrompt {
            text: "draft".into(),
            cursor: 0,
            images: Vec::new(),
            chip_elements: Vec::new(),
            image_counter: 0,
            image_undo_stash: Vec::new(),
        });
        agent.prompt_mode = PromptMode::EditingQueued {
            id: ids[0],
            original: "server one".into(),
            server_id: Some("p1".into()),
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        agent.prompt.set_text("server one");
        agent.active_pane = AgentPane::Prompt;
        let outcome = click_edit(&mut agent, ids[1]);
        assert!(matches!(outcome, InputOutcome::Changed), "got {outcome:?}");
        match &agent.prompt_mode {
            PromptMode::EditingQueued {
                id,
                original,
                server_id,
                ..
            } => {
                assert_eq!(*id, ids[1], "one click must open the clicked row's edit");
                assert_eq!(original, "local one");
                assert!(server_id.is_none());
            }
            other => panic!("expected EditingQueued for the clicked row, got {other:?}"),
        }
        assert_eq!(agent.prompt.text(), "local one");
        assert_eq!(agent.active_pane, AgentPane::Prompt);
        assert!(
            agent.pending_effects.iter().any(|e| matches!(
                e,
                crate::app::actions::Effect::QueueReleaseEdit { id, .. } if id == "p1"
            )),
            "clean exit must release the held server row, effects = {:?}",
            agent.pending_effects
        );
        assert_eq!(
            agent.stashed_prompt.as_ref().map(|s| s.text.as_str()),
            Some("draft")
        );
    }
    /// Mouse edit on an optimistic server row toasts and does not emit HoldEdit.
    #[test]
    fn mouse_edit_click_on_optimistic_server_row_toasts() {
        let mut agent = make_running_agent();
        agent.optimistic_queue_ids.insert("p1".into());
        let ids = agent.queue.entry_ids();
        let outcome = click_edit(&mut agent, ids[0]);
        assert!(matches!(outcome, InputOutcome::Changed), "got {outcome:?}");
        assert!(
            matches!(agent.prompt_mode, PromptMode::Normal),
            "an unconfirmed echo must not be editable"
        );
        assert_eq!(agent.prompt.text(), "");
        assert_eq!(
            agent.toast.as_ref().map(|(message, _)| message.as_str()),
            Some(crate::app::queue_edit::STILL_QUEUEING_TOAST),
        );
        assert!(
            agent.pending_effects.is_empty(),
            "no QueueHoldEdit may be emitted for a row the shell doesn't have"
        );
    }
    /// A synthetic left-click on a rendered follow-up chip yields the LITERAL
    /// `SubmitFollowUp` action (never a slash-command path).
    #[test]
    fn follow_up_chip_click_yields_literal_submit_action() {
        use crate::app::agent_view::test_fixtures::make_agent;
        let mut agent = make_agent();
        agent.apply_follow_ups("resp-1".into(), vec!["/always-approve".into()]);
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        let theme = crate::theme::Theme::current();
        let suggestions = agent.follow_ups.as_ref().unwrap().suggestions.clone();
        agent.follow_up_chips =
            crate::views::agent::render_follow_ups(area, &mut buf, &theme, &suggestions, None);
        let r = agent.follow_up_chips[0];
        let outcome = agent.handle_mouse(&MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: r.x + 1,
            row: r.y,
            modifiers: KeyModifiers::empty(),
        });
        match outcome {
            InputOutcome::Action(Action::SubmitFollowUp(text)) => {
                assert_eq!(text, "/always-approve", "even a /-prefixed chip is literal");
            }
            other => panic!("expected SubmitFollowUp, got {other:?}"),
        }
    }
    /// Double-click on a `[Pasted: N lines]` chip expands it into plain
    /// editable text; the first click only places the cursor on the chip.
    #[test]
    fn double_click_on_paste_chip_expands_it() {
        use crate::app::agent_view::test_fixtures::make_agent;
        use crate::views::prompt_widget::PromptStyle;
        let mut agent = make_agent();
        agent.active_pane = AgentPane::Prompt;
        let text = "line1\nline2\nline3\nline4";
        agent.prompt.handle_paste(text);
        assert_eq!(agent.prompt.textarea.elements().len(), 1);
        let area = Rect::new(0, 0, 40, 4);
        agent.pane_areas.prompt = area;
        let mut buf = Buffer::empty(area);
        let style = PromptStyle {
            chrome: false,
            vpad_top: 0,
            ..Default::default()
        };
        agent.prompt.draw(&mut buf, area, None, &style, None, None);
        let ta = agent.prompt.textarea_area();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: ta.x + 1,
            row: ta.y,
            modifiers: KeyModifiers::empty(),
        };
        agent.handle_mouse(&click);
        assert_eq!(
            agent.prompt.textarea.elements().len(),
            1,
            "single click must not expand the chip"
        );
        agent.handle_mouse(&click);
        assert!(
            agent.prompt.textarea.elements().is_empty(),
            "double-click must expand the chip"
        );
        assert_eq!(agent.prompt.textarea.text(), text);
    }
}
