//! Plan surfaces: plan chip/preview, plan approval + feedback, and casual
//! plan commenting (incl. the casual-commenting test fixture).
use super::AgentView;
#[cfg(test)]
use super::{ActivePane, InputMode, test_fixtures};
#[cfg(test)]
use crate::actions::ActionRegistry;
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::views::file_search::line_viewer::LineViewerState;
use crate::views::list_pane::ListItem;
use crate::views::plan_approval_view::{
    PlanApprovalFocus, PlanApprovalViewState, PlanComment, PlanReviewSource,
};
use crate::views::prompt_widget::{EnterOutcome, PromptEvent};
#[cfg(test)]
use crossterm::event::KeyModifiers;
use crossterm::event::{KeyCode, KeyEvent};
/// Telemetry for every way a plan review resolves ("build", "abandon",
/// "revise").
fn log_plan_submit(action: &str) {
    use echo_agent_telemetry::events::PlanSubmit;
    use echo_agent_telemetry::session_ctx::log_event;
    log_event(PlanSubmit {
        action: action.to_string(),
    });
}
impl AgentView {
    /// Resolve the absolute path to the plan file for this session.
    fn plan_file_path(&self) -> Option<std::path::PathBuf> {
        let session_id = self.session.session_id.as_ref()?;
        let cwd_str = self.session.cwd.to_string_lossy().into_owned();
        let encoded_cwd = urlencoding::encode(&cwd_str);
        Some(
            echo_agent_runtime::util::echo_agent_home::echo_agent_home()
                .join("sessions")
                .join(encoded_cwd.as_ref())
                .join(session_id.0.as_ref())
                .join("plan.md"),
        )
    }
    /// Whether the current line viewer is showing a plan preview.
    pub(super) fn is_plan_viewer(&self) -> bool {
        self.line_viewer.as_ref().is_some_and(|v| {
            v.kind == crate::views::file_search::line_viewer::LineViewerKind::PlanPreview
        })
    }
    /// Whether the user is currently composing a comment via the prompt
    /// input inside the *casual* plan preview (the modal opened with no
    /// `plan_approval_view`). Mirrors the `pav.focus == Commenting`
    /// check used by the plan-approval path so the prompt/footer
    /// behaves identically across both modes.
    pub(super) fn is_casual_commenting(&self) -> bool {
        self.plan_approval_view.is_none()
            && self.is_plan_viewer()
            && self.casual_commenting_range.is_some()
    }
    /// Whether the prompt "auto" (LLM classifier mode) flag should render.
    /// Extracted for unit testing the precedence: auto shows only when the
    /// session is in auto mode and neither yolo (always-approve wins) nor plan
    /// is active.
    pub(super) fn auto_flag_visible(&self, effective_plan: bool) -> bool {
        self.session.is_auto() && !self.session.is_yolo() && !effective_plan
    }
    /// Whether plan content is available for preview.
    fn plan_preview_available(&self) -> bool {
        self.plan_body_for_preview().is_some()
    }
    /// Whether the "plan" status-bar chip should be rendered.
    ///
    /// Visible while plan mode is active, or always when the user has set
    /// `show_plan_chip = true` in `pager.toml`. Hidden by default once the
    /// user exits plan mode.
    pub(super) fn should_show_plan_chip(
        &self,
        appearance: &crate::appearance::AppearanceConfig,
    ) -> bool {
        (self.plan_mode_active || appearance.show_plan_chip) && self.plan_preview_available()
    }
    fn inline_plan_content(&self) -> Option<&str> {
        self.plan_approval_view
            .as_ref()
            .filter(|p| p.source == PlanReviewSource::Inline)
            .and_then(|p| p.plan_content.as_deref())
            .filter(|s| !s.trim().is_empty())
    }
    /// Resolve the plan body for the line-viewer preview.
    ///
    /// Prefers content carried on the approval request (inline plan-creation or
    /// the shell-read file body), then falls back to the on-disk plan file.
    /// Request body first keeps file-backed previews working when the path
    /// resolution fails or the file disappears between intercept and open.
    pub(super) fn plan_body_for_preview(&self) -> Option<String> {
        if let Some(content) = self
            .plan_approval_view
            .as_ref()
            .and_then(|p| p.plan_content.as_deref())
            .filter(|s| !s.trim().is_empty())
        {
            return Some(content.to_owned());
        }
        if let Some(content) = self
            .latest_inline_plan_content
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            return Some(content.to_owned());
        }
        self.plan_file_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .filter(|s| !s.trim().is_empty())
    }
    /// Open the plan preview when content exists, or when plan approval is
    /// parked with an empty body (so the decision surface always pops).
    pub(crate) fn show_plan_preview_if_available(&mut self) {
        if self.plan_preview_available() || self.plan_approval_view.is_some() {
            self.show_plan_preview();
        }
    }
    /// Show the plan in the line viewer overlay or a "no plan" toast.
    ///
    /// When plan approval is parked without a body, opens a placeholder
    /// preview so the user always sees a decision surface (a/s/q) instead of
    /// a dead "Waiting on plan approval" line with a no-op Tab:plan.
    pub fn show_plan_preview(&mut self) {
        let body = self.plan_body_for_preview();
        let approval_empty = self
            .plan_approval_view
            .as_ref()
            .is_some_and(|p| !p.has_plan);
        let Some(mut viewer) = (if let Some(content) = body {
            LineViewerState::open_markdown_content("plan.md", content, None)
        } else if approval_empty {
            LineViewerState::open_markdown_content(
                "plan.md",
                crate::views::plan_approval_view::EMPTY_PLAN_PLACEHOLDER.to_owned(),
                None,
            )
        } else if let Some(plan_path) = self.plan_file_path() {
            LineViewerState::open_markdown(&plan_path, None)
        } else {
            None
        }) else {
            self.show_toast("No plan written yet.");
            return;
        };
        viewer.kind = crate::views::file_search::line_viewer::LineViewerKind::PlanPreview;
        viewer.title_override = Some(if approval_empty {
            "plan.md (empty)".to_string()
        } else {
            "plan.md".to_string()
        });
        viewer.fullscreen = true;
        {
            let plan = viewer.plan_mut();
            plan.show_action_buttons = self.plan_approval_view.is_none();
            plan.feedback_active = self.plan_approval_view.is_some();
        }
        if let Some(ref pav) = self.plan_approval_view
            && !pav.comments.is_empty()
        {
            viewer.rebuild_with_comments(&pav.comments);
        } else if !self.plan_comments.is_empty() {
            viewer.rebuild_with_comments(&self.plan_comments);
        }
        self.line_viewer = Some(viewer);
    }
    /// Test fixture: drive the agent into casual-commenting state
    /// (line viewer open in plan-preview mode + `casual_commenting_range`
    /// armed) so the `Event::Paste` plan-feedback arm at ~1539 is
    /// reachable from a unit test without spawning the real
    /// keystroke pipeline. Consolidates three field mutations into
    /// one helper so a future refactor of casual-commenting state
    /// only has to update this fixture rather than every test that
    /// reaches into the fields by name.
    #[cfg(test)]
    pub(crate) fn enter_casual_commenting_for_test(&mut self) {
        let mut viewer =
            crate::views::file_search::line_viewer::LineViewerState::open_markdown_content(
                "test.md",
                "hello\n".to_owned(),
                None,
            )
            .expect("fixture must open the line viewer");
        viewer.kind = crate::views::file_search::line_viewer::LineViewerKind::PlanPreview;
        self.line_viewer = Some(viewer);
        self.casual_commenting_range = Some(0..1);
    }
    pub(crate) fn approve_plan(&mut self) -> InputOutcome {
        let Some(mut pav) = self.plan_approval_view.take() else {
            return InputOutcome::Changed;
        };
        let freeform = {
            let t = self.prompt.text_without_image_chips();
            let trimmed = t.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        };
        Self::merge_live_images_into_stash(&mut self.prompt, &mut pav.stashed_prompt);
        let review_comments = {
            let formatted = pav.format_feedback(freeform.as_deref());
            if formatted.trim().is_empty() {
                None
            } else {
                Some(format!(
                    "The user approved the plan with the following review comments:\n\n{}",
                    formatted
                ))
            }
        };
        pav.send_approved();
        self.close_plan_review(pav, "build");
        if let Some(text) = review_comments {
            return InputOutcome::Action(Action::Interject {
                text,
                images: vec![],
            });
        }
        InputOutcome::Changed
    }
    /// Fold freeform-only images into the session draft. Prefill clones share
    /// `display_number` *and* payload with the session image and are dropped;
    /// number reuse after freeform clear (Ctrl+C resets the counter) is not a
    /// clone and must renumber-merge. New images get matching `[Image #N]` chip
    /// text/elements so `restore` can re-bind them.
    fn merge_live_images_into_stash(
        prompt: &mut crate::views::prompt_widget::PromptWidget,
        session: &mut crate::views::prompt_widget::StashedPrompt,
    ) {
        let live = prompt.drain_images();
        for mut img in live {
            if session.images.iter().any(|s| {
                s.display_number == img.display_number && Self::same_image_payload(s, &img)
            }) {
                crate::prompt_images::cleanup_temp_file(&img);
                continue;
            }
            session.image_counter = session.image_counter.max(
                session
                    .images
                    .iter()
                    .map(|i| i.display_number)
                    .max()
                    .unwrap_or(0),
            );
            session.image_counter += 1;
            let dn = session.image_counter;
            img.display_number = dn;
            if !session.text.is_empty()
                && !session.text.ends_with(' ')
                && !session.text.ends_with('\n')
            {
                session.text.push(' ');
            }
            let placeholder = crate::prompt_images::display_text(dn);
            let start = session.text.len();
            session.text.push_str(&placeholder);
            let end = session.text.len();
            session.text.push(' ');
            session.chip_elements.push(crate::app::agent::ChipElement {
                range: start..end,
                kind: crate::views::prompt_widget::KIND_IMAGE,
                display: None,
            });
            session.images.push(img);
            session.cursor = session.text.len();
        }
    }
    /// Content identity for prefill-clone detection (not display_number alone).
    fn same_image_payload(
        a: &crate::prompt_images::PastedImage,
        b: &crate::prompt_images::PastedImage,
    ) -> bool {
        match (&a.encoded_bytes, &b.encoded_bytes) {
            (Some(ea), Some(eb)) if ea == eb => return true,
            _ => {}
        }
        match (&a.session_image_path, &b.session_image_path) {
            (Some(pa), Some(pb)) if pa == pb => return true,
            _ => {}
        }
        match (&a.source_path, &b.source_path) {
            (Some(pa), Some(pb)) if pa == pb => return true,
            _ => {}
        }
        false
    }
    pub(crate) fn abandon_plan(&mut self) -> InputOutcome {
        let Some(mut pav) = self.plan_approval_view.take() else {
            return InputOutcome::Changed;
        };
        pav.send_abandoned();
        self.close_plan_review(pav, "abandon");
        InputOutcome::Changed
    }
    /// Shared teardown for the two plan-review decisions that end the
    /// review (approve and abandon). The shell leaves plan mode as a
    /// result, but its confirming `CurrentModeUpdate("default")` is
    /// fire-and-forget and only arrives after the exit tool runs — so
    /// flip the mode indicator optimistically here (a lost update would
    /// otherwise leave the badge stuck on "plan"), restore the
    /// pre-review UI, and log the decision.
    ///
    /// Not for the revision path (`send_plan_feedback`): the shell
    /// stays in plan mode there, so the indicator must stay on.
    fn close_plan_review(&mut self, pav: PlanApprovalViewState, action: &'static str) {
        self.plan_mode_pending = Some(false);
        self.plan_freeform_prefill_deferred = false;
        self.latest_inline_plan_content = None;
        self.plan_next_comment_id = pav.next_comment_id;
        self.prompt.restore(pav.stashed_prompt);
        self.line_viewer = None;
        self.casual_commenting_range = None;
        self.casual_editing_comment_id = None;
        log_plan_submit(action);
    }
    fn send_plan_feedback(&mut self, feedback: Option<String>) -> InputOutcome {
        let Some(mut pav) = self.plan_approval_view.take() else {
            return InputOutcome::Changed;
        };
        let formatted = pav.format_feedback(feedback.as_deref());
        let to_send = if formatted.trim().is_empty() {
            feedback
        } else {
            Some(formatted)
        };
        if crate::app::minimal_mode_active()
            && let Some(msg) = to_send.as_deref().map(str::trim).filter(|s| !s.is_empty())
        {
            self.scrollback
                .push_block(crate::scrollback::RenderBlock::user_prompt(msg.to_string()));
        }
        Self::merge_live_images_into_stash(&mut self.prompt, &mut pav.stashed_prompt);
        pav.send_cancelled(to_send);
        if pav.source == PlanReviewSource::Inline {
            self.latest_inline_plan_content = None;
        }
        self.plan_freeform_prefill_deferred = false;
        self.plan_next_comment_id = pav.next_comment_id;
        self.prompt.restore(pav.stashed_prompt);
        self.line_viewer = None;
        self.prompt.textarea.cancel_undo_group();
        self.show_toast("Plan revision sent.");
        log_plan_submit("revise");
        InputOutcome::Changed
    }
    pub(crate) fn reopen_plan_approval(&mut self) {
        if let Some(ref mut pav) = self.plan_approval_view {
            pav.focus = PlanApprovalFocus::Preview;
        }
        self.show_plan_preview_if_available();
        if self.line_viewer.is_none() {
            if let Some(ref mut pav) = self.plan_approval_view {
                pav.focus = PlanApprovalFocus::Prompt;
            }
        } else if let Some(ref mut viewer) = self.line_viewer {
            viewer.plan_mut().feedback_active = true;
        }
    }
    fn leave_plan_commenting_restore_freeform(&mut self) {
        let stashed = if let Some(ref mut pav) = self.plan_approval_view {
            pav.commenting_range = None;
            pav.editing_comment_id = None;
            pav.stashed_feedback_prompt.take()
        } else {
            None
        };
        if let Some(stashed) = stashed {
            self.prompt.restore(stashed);
        } else {
            self.prompt.set_text("");
        }
    }
    pub(super) fn discard_in_progress_comment(&mut self) {
        self.leave_plan_commenting_restore_freeform();
    }
    pub(super) fn handle_plan_feedback_key(&mut self, key: &KeyEvent) -> InputOutcome {
        let is_commenting = self
            .plan_approval_view
            .as_ref()
            .is_some_and(|pav| pav.focus == PlanApprovalFocus::Commenting);
        if crate::input::key::RowWalk::from_key(key).is_some() {
            let focus = self.plan_approval_view.as_ref().map(|p| p.focus);
            match focus {
                Some(PlanApprovalFocus::Prompt) | Some(PlanApprovalFocus::Commenting) => {
                    if self.line_viewer.is_none() {
                        self.show_plan_preview_if_available();
                    }
                    if let Some(ref mut pav) = self.plan_approval_view {
                        pav.focus = PlanApprovalFocus::Preview;
                    }
                    if let Some(ref mut viewer) = self.line_viewer {
                        viewer.plan_mut().feedback_active = true;
                    }
                }
                Some(PlanApprovalFocus::Preview) => {
                    if let Some(ref mut pav) = self.plan_approval_view {
                        pav.focus = PlanApprovalFocus::Prompt;
                    }
                }
                None => {}
            }
            if is_commenting {
                self.discard_in_progress_comment();
            }
            return InputOutcome::Changed;
        }
        if key.code == KeyCode::Esc {
            if self.prompt.file_search_visible() {
                self.prompt.file_search.clear_context();
                return InputOutcome::Changed;
            }
            if is_commenting {
                if let Some(ref mut pav) = self.plan_approval_view {
                    pav.focus = PlanApprovalFocus::Preview;
                }
                self.discard_in_progress_comment();
                return InputOutcome::Changed;
            }
            if let Some(ref mut pav) = self.plan_approval_view {
                pav.focus = PlanApprovalFocus::Preview;
            }
            return InputOutcome::Changed;
        }
        if !is_commenting
            && key.code == KeyCode::Char('a')
            && key.modifiers.is_empty()
            && self.prompt.text_without_image_chips().trim().is_empty()
            && !self.prompt.file_search_visible()
        {
            return self.approve_plan();
        }
        match self.prompt.route_enter(key) {
            EnterOutcome::NewlineInserted => return InputOutcome::Changed,
            EnterOutcome::Submit => {
                if is_commenting {
                    return self.save_plan_comment();
                }
                let freeform_text = self.prompt.text_without_image_chips();
                let has_comments = self
                    .plan_approval_view
                    .as_ref()
                    .is_some_and(|pav| !pav.comments.is_empty());
                let prompt_focused = self
                    .plan_approval_view
                    .as_ref()
                    .is_some_and(|pav| pav.focus == PlanApprovalFocus::Prompt);
                if prompt_focused {
                    if freeform_text.trim().is_empty() && !has_comments {
                        self.show_toast("Type revision notes, or press a to approve.");
                        return InputOutcome::Changed;
                    }
                    let freeform = {
                        let trimmed = freeform_text.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_owned())
                        }
                    };
                    return self.send_plan_feedback(freeform);
                }
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
    pub(super) fn enter_plan_commenting(&mut self) -> InputOutcome {
        let viewer = match self.line_viewer.as_mut() {
            Some(v) => v,
            None => return InputOutcome::Changed,
        };
        if let Some(vi) = viewer.list_state.selected_index() {
            let pi = viewer.list_state.to_physical(vi);
            if let Some(comment_id) = viewer.lines.get(pi).and_then(|item| item.comment_id())
                && let Some(pav) = self.plan_approval_view.as_mut()
                && let Some(comment) = pav.comments.iter().find(|c| c.id == comment_id)
            {
                let comment_text = comment.text.clone();
                let comment_range = comment.line_range.clone();
                if pav.stashed_feedback_prompt.is_none() {
                    pav.stashed_feedback_prompt = Some(self.prompt.stash());
                }
                pav.editing_comment_id = Some(comment_id);
                pav.commenting_range = Some(comment_range);
                pav.focus = PlanApprovalFocus::Commenting;
                self.prompt.set_text(&comment_text);
                return InputOutcome::Changed;
            }
        }
        let range = viewer.selected_line_range();
        let Some(range) = range else {
            return InputOutcome::Changed;
        };
        if viewer.list_state.visual_mode {
            let start_vi = viewer.list_state.multi_range().map(|r| r.start);
            if let Some(start_vi) = start_vi {
                let start_pi = viewer.list_state.to_physical(start_vi);
                let start_id = viewer.lines.get(start_pi).map(|l| l.stable_id());
                viewer.list_state.exit_visual_mode();
                if let Some(id) = start_id {
                    viewer.list_state.select_by_id(id);
                }
            } else {
                viewer.list_state.exit_visual_mode();
            }
        }
        if let Some(ref mut pav) = self.plan_approval_view {
            if pav.stashed_feedback_prompt.is_none() {
                pav.stashed_feedback_prompt = Some(self.prompt.stash());
            }
            pav.commenting_range = Some(range);
            pav.editing_comment_id = None;
            pav.focus = PlanApprovalFocus::Commenting;
        }
        self.prompt.set_text("");
        InputOutcome::Changed
    }
    fn save_plan_comment(&mut self) -> InputOutcome {
        let text = self.prompt.text().to_string();
        if text.trim().is_empty() {
            return InputOutcome::Changed;
        }
        let pav = match self.plan_approval_view.as_mut() {
            Some(pav) => pav,
            None => return InputOutcome::Changed,
        };
        let range = match pav.commenting_range.take() {
            Some(r) => r,
            None => return InputOutcome::Changed,
        };
        if let Some(edit_id) = pav.editing_comment_id.take() {
            if let Some(comment) = pav.comments.iter_mut().find(|c| c.id == edit_id) {
                comment.text = text;
                comment.line_range = range;
            }
        } else {
            let id = pav.next_comment_id;
            pav.next_comment_id += 1;
            pav.comments.push(PlanComment {
                id,
                line_range: range,
                text,
            });
        }
        pav.focus = PlanApprovalFocus::Preview;
        let comments = pav.comments.clone();
        if let Some(ref mut viewer) = self.line_viewer {
            viewer.rebuild_with_comments(&comments);
        }
        if let Some(stashed) = pav.stashed_feedback_prompt.take() {
            self.prompt.restore(stashed);
        } else {
            self.prompt.set_text("");
        }
        InputOutcome::Changed
    }
    pub(super) fn delete_plan_comment_at_cursor(&mut self) -> InputOutcome {
        let viewer = match self.line_viewer.as_ref() {
            Some(v) => v,
            None => return InputOutcome::Changed,
        };
        let vi = match viewer.list_state.selected_index() {
            Some(vi) => vi,
            None => return InputOutcome::Changed,
        };
        let pi = viewer.list_state.to_physical(vi);
        let comment_id = match viewer.lines.get(pi).and_then(|item| item.comment_id()) {
            Some(id) => id,
            None => return InputOutcome::Changed,
        };
        if let Some(ref mut pav) = self.plan_approval_view {
            pav.comments.retain(|c| c.id != comment_id);
            let comments = pav.comments.clone();
            if let Some(ref mut viewer) = self.line_viewer {
                viewer.rebuild_with_comments(&comments);
            }
        }
        InputOutcome::Changed
    }
    /// Enter casual commenting mode from the plan preview.
    ///
    /// If the cursor is on a comment line, enter edit mode for that comment.
    /// If the cursor is on a source line, capture the line range and enter
    /// new-comment mode.
    pub(super) fn enter_casual_plan_commenting(&mut self) -> InputOutcome {
        let viewer = match self.line_viewer.as_mut() {
            Some(v) => v,
            None => return InputOutcome::Changed,
        };
        if let Some(vi) = viewer.list_state.selected_index() {
            let pi = viewer.list_state.to_physical(vi);
            if let Some(comment_id) = viewer.lines.get(pi).and_then(|item| item.comment_id())
                && let Some(comment) = self.plan_comments.iter().find(|c| c.id == comment_id)
            {
                let comment_text = comment.text.clone();
                let comment_range = comment.line_range.clone();
                if self.casual_stashed_prompt.is_none() {
                    self.casual_stashed_prompt = Some(self.prompt.stash());
                }
                self.casual_editing_comment_id = Some(comment_id);
                self.casual_commenting_range = Some(comment_range);
                self.prompt.set_text(&comment_text);
                return InputOutcome::Changed;
            }
        }
        let range = viewer.selected_line_range();
        let Some(range) = range else {
            return InputOutcome::Changed;
        };
        if viewer.list_state.visual_mode {
            let start_vi = viewer.list_state.multi_range().map(|r| r.start);
            if let Some(start_vi) = start_vi {
                let start_pi = viewer.list_state.to_physical(start_vi);
                let start_id = viewer.lines.get(start_pi).map(|l| l.stable_id());
                viewer.list_state.exit_visual_mode();
                if let Some(id) = start_id {
                    viewer.list_state.select_by_id(id);
                }
            } else {
                viewer.list_state.exit_visual_mode();
            }
        }
        if self.casual_stashed_prompt.is_none() {
            self.casual_stashed_prompt = Some(self.prompt.stash());
        }
        self.casual_commenting_range = Some(range);
        self.casual_editing_comment_id = None;
        self.prompt.set_text("");
        InputOutcome::Changed
    }
    /// Save the current casual comment (new or edited) and rebuild the viewer.
    pub(super) fn save_casual_plan_comment(&mut self) -> InputOutcome {
        let text = self.prompt.text().to_owned();
        if text.trim().is_empty() {
            return self.cancel_casual_plan_commenting();
        }
        let range = match self.casual_commenting_range.take() {
            Some(r) => r,
            None => return self.cancel_casual_plan_commenting(),
        };
        if let Some(edit_id) = self.casual_editing_comment_id.take() {
            if let Some(comment) = self.plan_comments.iter_mut().find(|c| c.id == edit_id) {
                comment.text = text;
                comment.line_range = range;
            }
        } else {
            let id = self.plan_next_comment_id;
            self.plan_next_comment_id += 1;
            self.plan_comments.push(PlanComment {
                id,
                line_range: range,
                text,
            });
        }
        if let Some(stashed) = self.casual_stashed_prompt.take() {
            self.prompt.restore(stashed);
        } else {
            self.prompt.set_text("");
        }
        let comments = self.plan_comments.clone();
        if let Some(ref mut viewer) = self.line_viewer {
            viewer.rebuild_with_comments(&comments);
        }
        InputOutcome::Changed
    }
    /// Cancel casual plan commenting without saving.
    pub(super) fn cancel_casual_plan_commenting(&mut self) -> InputOutcome {
        self.casual_commenting_range = None;
        self.casual_editing_comment_id = None;
        if let Some(stashed) = self.casual_stashed_prompt.take() {
            self.prompt.restore(stashed);
        } else {
            self.prompt.set_text("");
        }
        InputOutcome::Changed
    }
    /// Key handler used while the user is composing a casual plan
    /// comment via the prompt input. Mirrors `handle_plan_feedback_key`
    /// (which serves the plan-approval Commenting focus) so the UX is
    /// identical: Enter saves, Esc cancels, Tab cancels back to the
    /// modal, and everything else routes to the prompt textarea.
    pub(super) fn handle_casual_plan_feedback_key(&mut self, key: &KeyEvent) -> InputOutcome {
        if key.code == KeyCode::Esc {
            if self.prompt.file_search_visible() {
                self.prompt.file_search.clear_context();
                return InputOutcome::Changed;
            }
            return self.cancel_casual_plan_commenting();
        }
        match self.prompt.route_enter(key) {
            EnterOutcome::NewlineInserted => return InputOutcome::Changed,
            EnterOutcome::Submit => return self.save_casual_plan_comment(),
            EnterOutcome::PassThrough => {}
        }
        if key.code == KeyCode::Tab && key.modifiers.is_empty() {
            return self.cancel_casual_plan_commenting();
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
    /// Delete the casual comment under the cursor in the plan preview.
    pub(super) fn delete_casual_plan_comment_at_cursor(&mut self) -> InputOutcome {
        let viewer = match self.line_viewer.as_ref() {
            Some(v) => v,
            None => return InputOutcome::Unchanged,
        };
        let vi = match viewer.list_state.selected_index() {
            Some(vi) => vi,
            None => return InputOutcome::Unchanged,
        };
        let pi = viewer.list_state.to_physical(vi);
        let comment_id = match viewer.lines.get(pi).and_then(|item| item.comment_id()) {
            Some(id) => id,
            None => return InputOutcome::Unchanged,
        };
        self.plan_comments.retain(|c| c.id != comment_id);
        let comments = self.plan_comments.clone();
        if let Some(ref mut viewer) = self.line_viewer {
            viewer.rebuild_with_comments(&comments);
        }
        InputOutcome::Changed
    }
    pub(super) fn send_casual_plan_comments(&mut self) -> InputOutcome {
        if self.plan_comments.is_empty() {
            self.show_toast("No comments to send.");
            return InputOutcome::Changed;
        }
        let plan_content = self.inline_plan_content().map(str::to_owned).or_else(|| {
            let path = self.plan_file_path()?;
            std::fs::read_to_string(path).ok()
        });
        let body = crate::views::plan_approval_view::format_plan_comments(
            &self.plan_comments,
            plan_content.as_deref(),
        );
        let text = format!("Plan feedback:\n\n{body}");
        self.plan_comments.clear();
        self.plan_next_comment_id = 0;
        self.cancel_line_viewer();
        self.show_toast("Plan feedback sent.");
        InputOutcome::Action(Action::SendPrompt(text))
    }
}
#[cfg(test)]
mod prompt_flag_tests {
    use super::test_fixtures::make_agent;
    /// The prompt "auto" (classifier) mode flag shows only when the session is
    /// in Auto and neither yolo (always-approve wins) nor plan is active.
    #[test]
    fn auto_flag_visible_precedence() {
        let mut agent = make_agent();
        assert!(!agent.auto_flag_visible(false));
        agent.session.auto_mode = true;
        assert!(agent.auto_flag_visible(false));
        assert!(!agent.auto_flag_visible(true));
        agent.session.yolo_mode = true;
        assert!(!agent.auto_flag_visible(false));
        agent.session.yolo_mode = false;
        assert!(agent.auto_flag_visible(false));
    }
}
#[cfg(test)]
mod plan_chip_tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::agent::{AgentId, AgentSession, AgentState};
    use crate::appearance::AppearanceConfig;
    use crate::scrollback::state::ScrollbackState;
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
    #[test]
    fn plan_chip_hidden_after_exit_by_default() {
        let mut agent = make_agent();
        agent.plan_mode_active = false;
        let appearance = AppearanceConfig::default();
        assert!(!appearance.show_plan_chip);
        assert!(!agent.should_show_plan_chip(&appearance));
    }
    #[test]
    fn plan_chip_visible_while_plan_mode_active() {
        let mut agent = make_agent();
        agent.plan_mode_active = true;
        let appearance = AppearanceConfig::default();
        assert!(!agent.should_show_plan_chip(&appearance));
    }
    #[test]
    fn plan_chip_visible_when_config_overrides() {
        let mut agent = make_agent();
        agent.plan_mode_active = false;
        let appearance = AppearanceConfig {
            show_plan_chip: true,
            ..Default::default()
        };
        assert!(!agent.should_show_plan_chip(&appearance));
    }
    #[test]
    fn set_input_mode_vim_empty_prompt_switches_to_scrollback_and_j_selects_next() {
        crate::appearance::cache::set_simple_mode(true);
        let mut agent = make_agent();
        agent.vim_mode = true;
        agent.set_active_pane(ActivePane::Prompt, true);
        agent.set_input_mode(InputMode::Vim);
        assert_eq!(agent.active_pane, ActivePane::Scrollback);
        assert!(!agent.is_simple_mode());
        let registry = ActionRegistry::defaults();
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let outcome = agent.handle_scrollback_key(&j, &registry);
        assert!(matches!(outcome, InputOutcome::Action(Action::SelectNext)));
    }
    #[test]
    fn set_input_mode_vim_nonempty_prompt_keeps_pane() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        agent.prompt.set_text("draft");
        agent.set_input_mode(InputMode::Vim);
        assert_eq!(agent.active_pane, ActivePane::Prompt);
    }
    #[test]
    fn set_input_mode_simple_from_scrollback_leaves_pane_unchanged() {
        let mut agent = make_agent();
        agent.vim_mode = true;
        agent.set_active_pane(ActivePane::Scrollback, true);
        agent.set_input_mode(InputMode::Simple);
        assert_eq!(agent.active_pane, ActivePane::Scrollback);
        assert!(agent.is_simple_mode());
        let registry = ActionRegistry::defaults();
        let x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        let outcome = agent.handle_scrollback_key(&x, &registry);
        assert_eq!(agent.active_pane, ActivePane::Scrollback);
        assert!(matches!(outcome, InputOutcome::Unchanged));
    }
    #[test]
    fn new_agent_respects_persisted_simple_mode_for_mode_and_pane() {
        crate::appearance::cache::set_simple_mode(true);
        let a1 = make_agent();
        assert!(a1.is_simple_mode());
        assert_eq!(a1.active_pane, ActivePane::Prompt);
        crate::appearance::cache::set_simple_mode(false);
        let a2 = make_agent();
        assert!(!a2.is_simple_mode());
        assert_eq!(a2.active_pane, ActivePane::Scrollback);
    }
    #[test]
    fn set_input_mode_reconciles_pane_orthogonal_to_active_modal_field() {
        let mut agent = make_agent();
        agent.set_active_pane(ActivePane::Prompt, true);
        agent.active_modal = None;
        agent.set_input_mode(InputMode::Vim);
        assert_eq!(agent.active_pane, ActivePane::Scrollback);
        assert!(agent.active_modal.is_none());
    }
    #[test]
    fn scrollback_j_with_vim_mode_off_forwards_to_prompt() {
        crate::appearance::cache::set_vim_mode(false);
        let mut agent = make_agent();
        agent.vim_mode = false;
        agent.set_active_pane(ActivePane::Scrollback, true);
        let registry = ActionRegistry::defaults();
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let outcome = agent.handle_scrollback_key(&j, &registry);
        assert!(
            matches!(
                outcome,
                InputOutcome::ActionThenForward(Action::FocusPrompt)
            ),
            "vim-off: bare 'j' in scrollback must forward to prompt; got {outcome:?}"
        );
    }
    #[test]
    fn scrollback_j_with_vim_mode_on_selects_next() {
        crate::appearance::cache::set_vim_mode(true);
        let mut agent = make_agent();
        agent.vim_mode = true;
        agent.set_active_pane(ActivePane::Scrollback, true);
        let registry = ActionRegistry::defaults();
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let outcome = agent.handle_scrollback_key(&j, &registry);
        assert!(
            matches!(outcome, InputOutcome::Action(Action::SelectNext)),
            "vim-on: bare 'j' in scrollback must dispatch SelectNext; got {outcome:?}"
        );
    }
    #[test]
    fn scrollback_arrow_down_works_in_both_modes() {
        let registry = ActionRegistry::defaults();
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let mut a_off = make_agent();
        a_off.vim_mode = false;
        a_off.set_active_pane(ActivePane::Scrollback, true);
        assert!(matches!(
            a_off.handle_scrollback_key(&down, &registry),
            InputOutcome::Action(Action::SelectNext)
        ));
        let mut a_on = make_agent();
        a_on.vim_mode = true;
        a_on.set_active_pane(ActivePane::Scrollback, true);
        assert!(matches!(
            a_on.handle_scrollback_key(&down, &registry),
            InputOutcome::Action(Action::SelectNext)
        ));
    }
}
#[cfg(test)]
mod plan_approval_enter_tests {
    use super::test_fixtures::make_agent;
    use super::*;
    use crate::views::plan_approval_view::PlanApprovalFocus;
    fn enter_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }
    fn agent_with_revise_prompt() -> AgentView {
        let mut agent = make_agent();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let request = crate::views::plan_approval_view::ExitPlanModeExtRequest {
            session_id: "test-session".into(),
            tool_call_id: "call-1".into(),
            plan_content: Some("# Plan\n\n## Step 1\nDo something".into()),
        };
        let mut pav = crate::views::plan_approval_view::PlanApprovalViewState::new(
            request,
            crate::views::prompt_widget::StashedPrompt {
                text: String::new(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            },
            tx,
        );
        pav.focus = PlanApprovalFocus::Prompt;
        agent.plan_approval_view = Some(pav);
        agent.prompt.set_text("");
        agent
    }
    #[test]
    fn empty_enter_on_revise_prompt_does_not_approve() {
        let mut agent = agent_with_revise_prompt();
        let outcome = agent.handle_plan_feedback_key(&enter_key());
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(
            agent.plan_approval_view.is_some(),
            "empty Enter must leave plan approval open"
        );
        assert_eq!(
            agent.toast.as_ref().map(|(msg, _)| msg.as_str()),
            Some("Type revision notes, or press a to approve.")
        );
    }
    #[test]
    fn enter_with_revision_text_requests_changes() {
        let mut agent = agent_with_revise_prompt();
        agent.prompt.set_text("please use auth middleware");
        let outcome = agent.handle_plan_feedback_key(&enter_key());
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.plan_approval_view.is_none());
        assert_eq!(
            agent.toast.as_ref().map(|(msg, _)| msg.as_str()),
            Some("Plan revision sent.")
        );
    }
    #[test]
    fn empty_enter_with_pending_comments_still_requests_changes() {
        let mut agent = agent_with_revise_prompt();
        if let Some(ref mut pav) = agent.plan_approval_view {
            pav.comments.push(PlanComment {
                id: 1,
                line_range: 0..1,
                text: "nit".into(),
            });
        }
        let outcome = agent.handle_plan_feedback_key(&enter_key());
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.plan_approval_view.is_none());
        assert_eq!(
            agent.toast.as_ref().map(|(msg, _)| msg.as_str()),
            Some("Plan revision sent.")
        );
    }
    #[test]
    fn a_on_empty_revise_prompt_approves() {
        let mut agent = agent_with_revise_prompt();
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let outcome = agent.handle_plan_feedback_key(&a);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.plan_approval_view.is_none(), "`a` must approve");
        assert_ne!(
            agent.toast.as_ref().map(|(msg, _)| msg.as_str()),
            Some("Plan revision sent.")
        );
    }
    #[test]
    fn a_with_pending_comments_and_empty_freeform_approves() {
        let mut agent = agent_with_revise_prompt();
        if let Some(ref mut pav) = agent.plan_approval_view {
            pav.comments.push(PlanComment {
                id: 1,
                line_range: 0..1,
                text: "nit".into(),
            });
        }
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let outcome = agent.handle_plan_feedback_key(&a);
        assert!(
            agent.plan_approval_view.is_none(),
            "empty freeform + comments: `a` must approve with comments"
        );
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::Interject { .. })
        ));
    }
    #[test]
    fn a_with_nonempty_freeform_types_letter() {
        let mut agent = agent_with_revise_prompt();
        agent.prompt.set_text("notes");
        agent.prompt.set_cursor(agent.prompt.text().len());
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let _ = agent.handle_plan_feedback_key(&a);
        assert!(
            agent.plan_approval_view.is_some(),
            "non-empty freeform: `a` must type into the revision notes"
        );
        assert_eq!(agent.prompt.text(), "notesa");
    }
    #[test]
    fn tab_out_of_commenting_restores_freeform() {
        let mut agent = agent_with_revise_prompt();
        agent.prompt.set_text("keep my freeform notes");
        if let Some(ref mut pav) = agent.plan_approval_view {
            pav.stashed_feedback_prompt = Some(agent.prompt.stash());
            pav.commenting_range = Some(0..1);
            pav.focus = PlanApprovalFocus::Commenting;
        }
        agent.prompt.set_text("unsaved comment draft");
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        let _ = agent.handle_plan_feedback_key(&tab);
        assert_eq!(agent.prompt.text(), "keep my freeform notes");
        assert_eq!(
            agent.plan_approval_view.as_ref().map(|p| p.focus),
            Some(PlanApprovalFocus::Preview)
        );
    }
    #[test]
    fn reopen_plan_approval_does_not_clobber_session_draft() {
        let mut agent = agent_with_revise_prompt();
        if let Some(ref mut pav) = agent.plan_approval_view {
            pav.stashed_prompt = crate::views::prompt_widget::StashedPrompt {
                text: "session draft from mid-thinking".into(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            };
        }
        agent.prompt.set_text("revision freeform");
        agent.reopen_plan_approval();
        assert_eq!(
            agent
                .plan_approval_view
                .as_ref()
                .map(|p| p.stashed_prompt.text.as_str()),
            Some("session draft from mid-thinking"),
        );
        assert_eq!(agent.prompt.text(), "revision freeform");
        agent.abandon_plan();
        assert_eq!(agent.prompt.text(), "session draft from mid-thinking");
    }
    #[test]
    fn approve_includes_freeform_notes() {
        let mut agent = agent_with_revise_prompt();
        if let Some(ref mut pav) = agent.plan_approval_view {
            pav.stashed_prompt = crate::views::prompt_widget::StashedPrompt {
                text: "session draft".into(),
                cursor: 0,
                images: Vec::new(),
                chip_elements: Vec::new(),
                image_counter: 0,
                image_undo_stash: Vec::new(),
            };
        }
        agent.prompt.set_text("please also fix auth");
        let outcome = agent.approve_plan();
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::Interject { ref text, .. })
                if text.contains("please also fix auth")
        ));
        assert_eq!(
            agent.prompt.text(),
            "session draft",
            "approve restores session draft after including freeform"
        );
    }
    #[test]
    fn approve_does_not_duplicate_prefilled_session_images() {
        let mut agent = agent_with_revise_prompt();
        let session_img = crate::prompt_images::PastedImage {
            element_id: echo_agent_ratatui_textarea::ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: Some((100, 80)),
            byte_len: 16,
            encoded_bytes: Some(vec![0u8; 16].into()),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: crate::prompt_images::PromptImagePreview::default(),
        };
        if let Some(ref mut pav) = agent.plan_approval_view {
            pav.stashed_prompt = crate::views::prompt_widget::StashedPrompt {
                text: "see [Image #1] ".into(),
                cursor: 0,
                images: vec![session_img.clone()],
                chip_elements: vec![crate::app::agent::ChipElement {
                    range: 4..14,
                    kind: crate::views::prompt_widget::KIND_IMAGE,
                    display: None,
                }],
                image_counter: 1,
                image_undo_stash: Vec::new(),
            };
        }
        let mut freeform_img = session_img;
        freeform_img.element_id = echo_agent_ratatui_textarea::ElementId::from_raw(2);
        agent.prompt.set_text("see [Image #1] ");
        agent.prompt.set_images(vec![freeform_img]);
        agent.approve_plan();
        assert_eq!(agent.prompt.images.len(), 1);
        assert_eq!(agent.prompt.images[0].display_number, 1);
    }
    #[test]
    fn approve_merges_new_freeform_image_despite_reused_display_number() {
        let mut agent = agent_with_revise_prompt();
        let session_img = crate::prompt_images::PastedImage {
            element_id: echo_agent_ratatui_textarea::ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: Some((100, 80)),
            byte_len: 16,
            encoded_bytes: Some(vec![1u8; 16].into()),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: crate::prompt_images::PromptImagePreview::default(),
        };
        if let Some(ref mut pav) = agent.plan_approval_view {
            pav.stashed_prompt = crate::views::prompt_widget::StashedPrompt {
                text: "session [Image #1] ".into(),
                cursor: 0,
                images: vec![session_img],
                chip_elements: vec![crate::app::agent::ChipElement {
                    range: 8..18,
                    kind: crate::views::prompt_widget::KIND_IMAGE,
                    display: None,
                }],
                image_counter: 1,
                image_undo_stash: Vec::new(),
            };
        }
        agent.prompt.set_text("");
        let new_img = crate::prompt_images::PastedImage {
            element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
            display_number: 0,
            mime_type: "image/png".into(),
            dimensions: Some((100, 80)),
            byte_len: 16,
            encoded_bytes: Some(vec![9u8; 16].into()),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: crate::prompt_images::PromptImagePreview::default(),
        };
        agent
            .prompt
            .insert_image(new_img)
            .expect("paste freeform image after clear");
        assert_eq!(
            agent.prompt.images[0].display_number, 1,
            "precondition: freeform counter reset reuses #1"
        );
        agent.approve_plan();
        assert_eq!(
            agent.prompt.images.len(),
            2,
            "new freeform image must merge beside session image"
        );
        let numbers: Vec<_> = agent
            .prompt
            .images
            .iter()
            .map(|i| i.display_number)
            .collect();
        assert!(
            numbers.contains(&1) && numbers.contains(&2),
            "got {numbers:?}"
        );
        assert!(
            agent.prompt.text().contains("[Image #2]"),
            "renumbered chip must appear in session draft text, got {:?}",
            agent.prompt.text()
        );
    }
    #[test]
    fn approve_strips_image_chips_from_interjection_text() {
        let mut agent = agent_with_revise_prompt();
        agent.prompt.set_text("also check auth ");
        let img = crate::prompt_images::PastedImage {
            element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
            display_number: 0,
            mime_type: "image/png".into(),
            dimensions: Some((100, 80)),
            byte_len: 16,
            encoded_bytes: Some(vec![0u8; 16].into()),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: crate::prompt_images::PromptImagePreview::default(),
        };
        agent
            .prompt
            .insert_image(img)
            .expect("insert freeform image chip");
        assert!(
            agent.prompt.text().contains("[Image #1]"),
            "precondition: chip in freeform text"
        );
        let outcome = agent.approve_plan();
        match outcome {
            InputOutcome::Action(Action::Interject { text, images }) => {
                assert!(
                    !text.contains("[Image #"),
                    "approve interjection must not leak image chip tokens, got {text:?}"
                );
                assert!(
                    text.contains("also check auth"),
                    "non-chip freeform text must still ship, got {text:?}"
                );
                assert!(
                    images.is_empty(),
                    "approve interjection stays text-only; images merge into session draft"
                );
            }
            other => panic!("expected Interject with freeform, got {other:?}"),
        }
        assert!(
            agent.prompt.text().contains("[Image #1]"),
            "merged freeform image must restore with a chip in session draft text, got {:?}",
            agent.prompt.text()
        );
        assert_eq!(
            agent.prompt.images.len(),
            1,
            "freeform image must merge into restored session draft"
        );
        let stashed = agent.prompt.stash();
        agent.prompt.restore(stashed);
        assert_eq!(agent.prompt.images.len(), 1);
        assert!(agent.prompt.text().contains("[Image #1]"));
    }
    #[test]
    fn a_on_image_only_freeform_approves() {
        let mut agent = agent_with_revise_prompt();
        let img = crate::prompt_images::PastedImage {
            element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
            display_number: 0,
            mime_type: "image/png".into(),
            dimensions: Some((100, 80)),
            byte_len: 16,
            encoded_bytes: Some(vec![0u8; 16].into()),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: crate::prompt_images::PromptImagePreview::default(),
        };
        agent
            .prompt
            .insert_image(img)
            .expect("insert freeform image chip");
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let outcome = agent.handle_plan_feedback_key(&a);
        assert!(
            agent.plan_approval_view.is_none(),
            "image-only freeform: `a` must approve (not type the letter)"
        );
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(
            agent.prompt.text().contains("[Image #1]"),
            "freeform image folds into restored session draft"
        );
    }
    #[test]
    fn image_only_freeform_enter_toasts_instead_of_empty_revision() {
        let mut agent = agent_with_revise_prompt();
        let img = crate::prompt_images::PastedImage {
            element_id: echo_agent_ratatui_textarea::ElementId::from_raw(0),
            display_number: 0,
            mime_type: "image/png".into(),
            dimensions: Some((100, 80)),
            byte_len: 16,
            encoded_bytes: Some(vec![0u8; 16].into()),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: crate::prompt_images::PromptImagePreview::default(),
        };
        agent
            .prompt
            .insert_image(img)
            .expect("insert freeform image chip");
        let outcome = agent.handle_plan_feedback_key(&enter_key());
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(
            agent.plan_approval_view.is_some(),
            "image-only freeform must not cancel the plan with empty feedback"
        );
        assert_eq!(
            agent.toast.as_ref().map(|(msg, _)| msg.as_str()),
            Some("Type revision notes, or press a to approve.")
        );
    }
}
/// The mode indicator renders
/// `plan_mode_pending.unwrap_or(plan_mode_active)`, and the shell's
/// confirming `CurrentModeUpdate("default")` only arrives after the exit
/// tool runs (and can be lost entirely). Resolving the review with a
/// decision must therefore optimistically clear the effective plan mode
/// on BOTH decision paths — approve and abandon.
#[cfg(test)]
mod plan_approval_optimistic_mode_tests {
    use super::test_fixtures::make_agent;
    use super::*;
    use agent_client_protocol as acp;
    fn agent_in_plan_mode_with_approval() -> (
        AgentView,
        tokio::sync::oneshot::Receiver<echo_agent_acp::AcpResult<acp::ExtResponse>>,
    ) {
        let mut agent = make_agent();
        agent.plan_mode_active = true;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let request = crate::views::plan_approval_view::ExitPlanModeExtRequest {
            session_id: "test-session".into(),
            tool_call_id: "call-1".into(),
            plan_content: Some("# Plan\n\n## Step 1\nDo something".into()),
        };
        let pav = crate::views::plan_approval_view::PlanApprovalViewState::new(
            request,
            agent.prompt.stash(),
            tx,
        );
        agent.plan_approval_view = Some(pav);
        (agent, rx)
    }
    fn effective_plan_mode(agent: &AgentView) -> bool {
        agent.plan_mode_pending.unwrap_or(agent.plan_mode_active)
    }
    #[test]
    fn approve_plan_optimistically_clears_plan_mode() {
        let (mut agent, mut rx) = agent_in_plan_mode_with_approval();
        assert!(effective_plan_mode(&agent));
        agent.approve_plan();
        assert_eq!(agent.plan_mode_pending, Some(false));
        assert!(
            !effective_plan_mode(&agent),
            "indicator must leave plan mode immediately on approve, \
             not wait for the shell's CurrentModeUpdate"
        );
        let raw = rx
            .try_recv()
            .expect("approval response must be sent")
            .expect("Ok");
        let parsed: serde_json::Value = serde_json::from_str(raw.0.get()).unwrap();
        assert_eq!(parsed["outcome"], "approved");
    }
    /// Approve with review comments takes the early `Action::Interject`
    /// return — the optimistic clear must happen before that branch.
    #[test]
    fn approve_plan_with_comments_still_clears_plan_mode() {
        let (mut agent, _rx) = agent_in_plan_mode_with_approval();
        if let Some(ref mut pav) = agent.plan_approval_view {
            pav.comments
                .push(crate::views::plan_approval_view::PlanComment {
                    id: 1,
                    line_range: 1..2,
                    text: "use the existing helper".into(),
                });
        }
        let outcome = agent.approve_plan();
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::Interject { .. })
        ));
        assert_eq!(agent.plan_mode_pending, Some(false));
        assert!(!effective_plan_mode(&agent));
    }
    #[test]
    fn abandon_plan_optimistically_clears_plan_mode() {
        let (mut agent, _rx) = agent_in_plan_mode_with_approval();
        agent.abandon_plan();
        assert_eq!(agent.plan_mode_pending, Some(false));
        assert!(!effective_plan_mode(&agent));
    }
}
