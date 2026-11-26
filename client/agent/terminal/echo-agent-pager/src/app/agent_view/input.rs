//! Top-level input routing for [`AgentView`]: `handle_input` fans events
//! out to the active pane/overlay handlers; pane and input-mode setters.
#[cfg(test)]
use super::paste::paste_key_tests;
#[cfg(test)]
use super::test_fixtures;
use super::{
    AgentPane, AgentView, BlockingCard, CtaPhase, EscStep, InputMode, KeyOwner,
    MULTI_CLICK_TIMEOUT_MS, PromptInputMode, active_contexts_for_pane, format_key_for_log,
    is_link_modifier_for_key, is_mouse_reporting_toggle_chord, resolve_action,
};
use crate::actions::{ActionId, ActionRegistry, When};
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::key;
use crate::views::modal::ActiveModal;
use crate::views::plan_approval_view::PlanApprovalFocus;
use crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::time::Instant;
/// External-editor access to the ordinary composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalPromptEditorAccess {
    Ready,
    Attachments,
    PastePending,
    OwnedElsewhere,
}
impl AgentView {
    /// The composer is the logical editing surface regardless of pane focus
    /// (like the other global chords); overlays and dropdowns still own input.
    pub(crate) fn external_prompt_editor_access(&self) -> ExternalPromptEditorAccess {
        let owned_elsewhere = !matches!(self.prompt_mode, super::PromptMode::Normal)
            || self.active_subagent.is_some()
            || self.active_modal.is_some()
            || self.extensions_modal.is_some()
            || self.agents_modal.is_some()
            || self.persona_detail.is_some()
            || self.scrollback_search.is_some()
            || self.line_viewer.is_some()
            || self.image_viewer.is_some()
            || self.video_viewer.is_some()
            || self.block_viewer.is_some()
            || self.gboom.is_some()
            || self.show_goal_detail
            || self.btw_focused
            || self.blocking_card().is_some()
            || self.plan_approval_view.is_some()
            || self.casual_commenting_range.is_some()
            || self.rewind_state.is_some()
            || self.inline_edit.is_some()
            || self.jump_state.is_some()
            || self.prompt.any_dropdown_open();
        if owned_elsewhere {
            ExternalPromptEditorAccess::OwnedElsewhere
        } else if self.paste_probe_in_flight > 0 || self.deferred_send.is_some() {
            ExternalPromptEditorAccess::PastePending
        } else if !self.prompt.textarea.elements().is_empty() || !self.prompt.images.is_empty() {
            ExternalPromptEditorAccess::Attachments
        } else {
            ExternalPromptEditorAccess::Ready
        }
    }
    /// True when the scrollback pane is focused with nothing layered on top —
    /// no viewer, modal, btw, or open search. This is the precise state in
    /// which a bare `q`/`Esc` should close the enclosing surface (the subagent
    /// fullscreen view or the dashboard session overlay). Both close-key guards
    /// share this one predicate so a future sub-state addition can't make the
    /// mirrored checks drift apart.
    pub(crate) fn is_bare_scrollback(&self) -> bool {
        self.active_pane == AgentPane::Scrollback
            && self.block_viewer.is_none()
            && self.line_viewer.is_none()
            && self.active_modal.is_none()
            && self.image_viewer.is_none()
            && self.video_viewer.is_none()
            && self.gboom.is_none()
            && self.extensions_modal.is_none()
            && self.agents_modal.is_none()
            && self.persona_detail.is_none()
            && self.btw_state.is_none()
            && self.scrollback_search.is_none()
    }
    /// Whether no input-demanding overlay — a [`BlockingCard`] or the plan
    /// approval — is awaiting a response.
    pub(crate) fn no_input_overlay_pending(&self) -> bool {
        self.blocking_card().is_none() && self.plan_approval_view.is_none()
    }
    /// Whether FocusGained should move focus from Scrollback → Prompt.
    ///
    pub(crate) fn should_restore_prompt_on_focus_gained(&self) -> bool {
        if self.active_pane != AgentPane::Scrollback {
            return false;
        }
        if self.active_modal.is_some() {
            return false;
        }
        if !self.no_input_overlay_pending() {
            return true;
        }
        !self.vim_mode && self.session.state.is_idle()
    }
    /// Surfaces that own input ahead of the dashboard overlay cascade.
    /// That cascade runs before `handle_input`, so without this guard Left/Esc
    /// on an empty prompt would exit the overlay instead of reaching `/gboom`
    /// (turn/close), video (seek/close), image (close), `/agents`, persona
    /// detail, or the block viewer.
    pub(super) fn modal_owns_input(&self) -> bool {
        self.extensions_modal.is_some()
            || self.active_modal.is_some()
            || self.gboom.is_some()
            || self.video_viewer.is_some()
            || self.image_viewer.is_some()
            || self.agents_modal.is_some()
            || self.persona_detail.is_some()
            || self.block_viewer.is_some()
    }
    /// Prompt pane focused with an empty draft and no overlay or prompt-local
    /// sub-state owning keys — the state where a bare Left backs out of the
    /// dashboard overlay (mirror of the dashboard's Right = open detail). A
    /// non-empty draft (Left = caret move), scrollback focus (Left = collapse),
    /// an active history search, and an open `@` file-search dropdown (which
    /// owns Right/Up/Down picker nav) all fail the guard, leaving those
    /// behaviours untouched. The dropdown is only open while the draft holds
    /// an `@` token, so `text().is_empty()` already covers it — the explicit
    /// check keeps the predicate honest if that coupling ever changes. An open
    /// modal or media surface ([`Self::modal_owns_input`]) also fails the guard
    /// so those own Esc/Left rather than the overlay back-out stealing them. An
    /// open `/jump` picker fails it too, so the picker owns Esc/Left instead of
    /// being left latent.
    pub(crate) fn is_empty_focused_prompt(&self) -> bool {
        self.active_pane == AgentPane::Prompt
            && self.prompt.text().is_empty()
            && !self.prompt.history_search.is_active()
            && !self.prompt.file_search_visible()
            && self.no_input_overlay_pending()
            && !self.modal_owns_input()
            && self.jump_state.is_none()
    }
    pub(crate) fn workflow_runs_newest_first(
        &self,
    ) -> Vec<&crate::views::workflows::WorkflowRunSnapshot> {
        self.workflow_runs.iter().rev().collect()
    }
    /// No per-pane `Esc` consumer is pending (text selection, link highlight,
    /// goal detail, rewind overlay, open `/btw` panel, or open `/jump` picker),
    /// so `Esc` is free to back out of the dashboard overlay rather than
    /// clear/dismiss one of them first. Shared by both overlay back-out guards
    /// so a future Esc consumer is added once here.
    pub(crate) fn no_esc_consumer_pending(&self) -> bool {
        self.persistent_text_selection.is_none()
            && self.highlighted_link_idx.is_none()
            && !self.show_goal_detail
            && !self.show_workflows
            && self.rewind_state.is_none()
            && self.btw_state.is_none()
            && self.jump_state.is_none()
    }
    /// Effective screen mode of this process, as injected per agent at
    /// session creation (`apply_app_scoped_gates` →
    /// `PromptWidget::set_screen_mode`; the mode is fixed for the process
    /// lifetime). The global-free minimal check for per-agent input policy —
    /// unwired test agents default to Fullscreen, and tests opt in with
    /// `prompt.set_screen_mode(ScreenMode::Minimal)` instead of mutating the
    /// `MINIMAL_MODE_ACTIVE` process global.
    pub(crate) fn is_minimal_mode(&self) -> bool {
        self.prompt.slash_controller.screen_mode().is_minimal()
    }
    /// Whether a bare Esc pressed right now would reach
    /// [`Self::try_handle_esc_policy`]'s mid-turn cancel (assuming a turn is
    /// running — callers gate on that): the hint-bar predicate deciding when
    /// to advertise `Esc` instead of `Ctrl+C` for CancelTurn. Composed from
    /// the same predicates input routing uses, so the hint cannot claim Esc
    /// while a higher-priority consumer (dropdown, search, viewer/modal,
    /// agents/persona modal, needs-input overlay, queued-prompt or inline
    /// edit, subagent-view close, selection/link/goal/rewind/btw/jump,
    /// latent composer mode) would steal the press. Conservative on purpose:
    /// when false, the registry `Ctrl+C` is shown, which always cancels.
    /// `esc_owned_before_agent` is the app-level ownership snapshot
    /// (`AppView::esc_owned_before_agent`: voice dictation listening or
    /// pending cold-start, a focused dev tracing pane, the top-level cloud /
    /// import-Claude modals, and the dashboard's attached-agent popup — all
    /// consume Esc before any agent routing), passed down by the draw path.
    pub(crate) fn esc_would_cancel_turn(&self, esc_owned_before_agent: bool) -> bool {
        if esc_owned_before_agent
            || !crate::app::esc_cancels_turn(self.is_minimal_mode(), self.vim_mode)
        {
            return false;
        }
        let pane_clear = match self.active_pane {
            AgentPane::Prompt => {
                !self.modal_owns_input()
                    && self.block_viewer.is_none()
                    && self.line_viewer.is_none()
                    && !self.prompt.any_dropdown_open()
                    && !self.prompt.prompt_suggestion_visible()
                    && self.prompt_input_mode == PromptInputMode::Normal
            }
            AgentPane::Scrollback => self.is_bare_scrollback(),
            _ => false,
        };
        pane_clear
            && matches!(self.prompt_mode, crate::app::queue_edit::PromptMode::Normal)
            && self.inline_edit.is_none()
            && !self.is_subagent_view
            && self.agents_modal.is_none()
            && self.persona_detail.is_none()
            && self.no_esc_consumer_pending()
            && self.no_input_overlay_pending()
    }
    /// Esc on the prompt pane in a dashboard overlay backs out to the dashboard list (the prompt-focus mirror of the Left-arrow back-out), but only
    /// for an empty, Normal-mode composer with no per-pane Esc consumer pending. Beyond [`Self::is_empty_focused_prompt`] it also requires
    /// `PromptInputMode::Normal` (so a Bash/Remember empty prompt keeps Esc as its mode-exit, matching the full-screen view) and
    /// [`Self::no_esc_consumer_pending`] (so Esc still clears or dismisses a pending text selection / link highlight / goal detail / rewind first;
    /// Esc, unlike Left, is their consumer). A non-empty draft fails the guard so Esc still arms "press again to clear".
    /// Used only in the overlay cascade; the full-screen Esc policy (clear / rewind while idle; mid-turn cancel or swallow) is untouched.
    ///
    /// Also gated to an idle agent (no running, cancelling, or wake turn):
    /// while one is in flight, Esc must fall through to
    /// [`Self::try_handle_esc_policy`] (running → cancel in minimal / non-vim
    /// mode, swallow in vim mode; cancelling → retry CancelTurn), not detach
    /// to the dashboard. Detach mid-turn stays on
    /// Ctrl+\ / Left.
    pub(crate) fn overlay_esc_backs_out_from_prompt(&self) -> bool {
        self.is_empty_focused_prompt()
            && self.prompt_input_mode == PromptInputMode::Normal
            && self.no_esc_consumer_pending()
            && !self.session.state.is_turn_running()
            && !self.session.state.is_cancelling()
            && !self.wake_turn_active()
    }
    /// True when a pending plan / Q&A overlay is at its top navigation state
    /// (nothing left for `Esc` to clear), so the next `Esc` backs out of the
    /// dashboard overlay instead of dead-ending. Graduated: earlier presses
    /// keep their in-overlay meaning. Dashboard-overlay only; the overlay
    /// stays pending (no answer sent).
    pub(crate) fn overlay_esc_backs_out(&self) -> bool {
        if !self.in_dashboard_overlay {
            return false;
        }
        if self.modal_owns_input() {
            return false;
        }
        if !self.no_input_overlay_pending()
            && self.is_bare_scrollback()
            && self.no_esc_consumer_pending()
        {
            return true;
        }
        if self.plan_approval_view.is_some() {
            return self.plan_overlay_at_back_out_top();
        }
        self.card_esc() == Some(EscStep::BackOutOverlay)
    }
    /// Whether the pending plan-approval overlay is at a state where `Esc` /
    /// `Left` have nothing else to do, so they back out to the dashboard:
    ///   - `Preview` line viewer: no-ops once the input bar, accepted search
    ///     matcher, and visual selection are all cleared (those consume `Esc`
    ///     first, keeping the back-out graduated).
    ///   - `Preview` with no viewer, or empty `Prompt` feedback: one-press
    ///     exit; a typed draft keeps `Esc`'s step-back behaviour.
    fn plan_overlay_at_back_out_top(&self) -> bool {
        use crate::views::plan_approval_view::PlanApprovalFocus;
        let Some(pav) = self.plan_approval_view.as_ref() else {
            return false;
        };
        match pav.focus {
            PlanApprovalFocus::Preview => match self.line_viewer.as_ref() {
                Some(v) => {
                    v.list_state.input_mode().is_none()
                        && v.list_state.matcher().is_none()
                        && !v.list_state.visual_mode
                }
                None => self.active_pane != AgentPane::Scrollback,
            },
            PlanApprovalFocus::Prompt => {
                self.line_viewer.is_none()
                    && self.active_pane != AgentPane::Scrollback
                    && self.prompt.text().is_empty()
                    && !self.prompt.file_search_visible()
            }
            PlanApprovalFocus::Commenting => false,
        }
    }
    /// True when a pending overlay has no in-overlay use for a bare `Left`, so
    /// it backs out of the dashboard overlay: the plan line-viewer `Preview`
    /// and the single-question Q&A navigation surface. Plan feedback (caret
    /// move) and multi-question Q&A (previous question) keep `Left`.
    /// Dashboard-overlay only.
    pub(crate) fn overlay_left_backs_out(&self) -> bool {
        use crate::views::plan_approval_view::PlanApprovalFocus;
        use crate::views::question_view::QuestionFocus;
        if !self.in_dashboard_overlay {
            return false;
        }
        if self.modal_owns_input() {
            return false;
        }
        if let Some(pav) = self.plan_approval_view.as_ref() {
            return pav.focus == PlanApprovalFocus::Preview
                && self.line_viewer.as_ref().is_some_and(|v| {
                    v.list_state.input_mode().is_none()
                        && v.list_state.matcher().is_none()
                        && !v.list_state.visual_mode
                });
        }
        if let Some(qv) = self.question_view.as_ref() {
            return self.active_pane != AgentPane::Scrollback
                && qv.focus == QuestionFocus::Navigation
                && qv.questions.len() <= 1;
        }
        false
    }
    /// Handle a terminal event when this agent view is active.
    ///
    /// Routes key events through three levels:
    /// 1. Pane-specific (prompt widget or scrollback navigation)
    /// 2. Agent-level (cancel, yolo -- checked if pane didn't consume)
    /// 3. Return Unchanged (bubbles to app_view for global actions)
    pub fn handle_input(&mut self, ev: &Event, registry: &ActionRegistry) -> InputOutcome {
        self.handle_input_inner(ev, registry, false)
    }
    /// Enable prompt-focused conversation paging on a normal full-TUI agent surface.
    pub(in crate::app) fn handle_input_with_prompt_paging(
        &mut self,
        ev: &Event,
        registry: &ActionRegistry,
    ) -> InputOutcome {
        self.handle_input_inner(ev, registry, true)
    }
    /// Route minimal-only `/btw` ownership before the unchanged shared router.
    pub(in crate::app) fn handle_minimal_input(
        &mut self,
        ev: &Event,
        registry: &ActionRegistry,
    ) -> InputOutcome {
        match self.handle_minimal_btw_input(ev) {
            crate::minimal_api::MinimalBtwInput::Handled(outcome) => *outcome,
            crate::minimal_api::MinimalBtwInput::Occluded => {
                let jump_dismissed = self.dismiss_jump_picker_if_suppressed();
                let suspended = crate::minimal_api::suspend_minimal_btw(self);
                let outcome = if jump_dismissed
                    && matches!(
                        ev,
                        Event::Key(key)
                            if key.kind != KeyEventKind::Release
                                && key.code == KeyCode::Esc
                                && key.modifiers.is_empty()
                    ) {
                    InputOutcome::Changed
                } else {
                    self.handle_input(ev, registry)
                };
                if let Some(suspended) = suspended {
                    crate::minimal_api::restore_minimal_btw(self, suspended);
                }
                outcome
            }
            crate::minimal_api::MinimalBtwInput::Delegate => self.handle_input(ev, registry),
        }
    }
    /// Handle only minimal `/btw` dismissal and keyboard scrolling.
    fn handle_minimal_btw_input(&mut self, ev: &Event) -> crate::minimal_api::MinimalBtwInput {
        use crate::minimal_api::MinimalBtwInput::{Delegate, Handled, Occluded};
        if !crate::minimal_api::minimal_btw_surface_available(self) {
            return Occluded;
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && key.code == KeyCode::Esc
            && key.modifiers.is_empty()
            && self.btw_state.is_some()
        {
            return Handled(Box::new(self.dismiss_btw_panel()));
        }
        if self.active_pane != AgentPane::Prompt
            || !self.btw_focused
            || !crate::minimal_api::minimal_btw_geometry_is_paintable(self.last_btw_area)
        {
            return Delegate;
        }
        let Some(btw_scroll_max) = self.btw_state.as_ref().and_then(|btw| {
            matches!(btw, crate::views::btw_overlay::BtwOverlayState::Done { .. }).then(|| {
                let content_width = self.last_btw_area.width.saturating_sub(4) as usize;
                let max_body = self.last_btw_area.height.saturating_sub(2) as usize;
                btw.max_scroll_offset(content_width, max_body)
            })
        }) else {
            return Delegate;
        };
        if btw_scroll_max == 0 {
            return Delegate;
        }
        let Event::Key(key) = ev else {
            return Delegate;
        };
        if key.kind == KeyEventKind::Release || !key.modifiers.is_empty() {
            return Delegate;
        }
        let page = self.last_btw_area.height.saturating_sub(2).max(1) as usize;
        let Some(btw) = self.btw_state.as_mut() else {
            return Delegate;
        };
        match key.code {
            KeyCode::Up => btw.scroll_up(1),
            KeyCode::Down => btw.scroll_down(1, btw_scroll_max),
            KeyCode::PageUp => btw.scroll_up(page),
            KeyCode::PageDown => btw.scroll_down(page, btw_scroll_max),
            _ => return Delegate,
        }
        self.clear_btw_drag_state();
        Handled(Box::new(InputOutcome::Changed))
    }
    fn handle_input_inner(
        &mut self,
        ev: &Event,
        registry: &ActionRegistry,
        prompt_paging: bool,
    ) -> InputOutcome {
        if self.scrollback_drag_latched() {
            match ev {
                Event::Mouse(MouseEvent {
                    kind:
                        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left),
                    ..
                }) => {}
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved | MouseEventKind::Up(_),
                    ..
                }) if self.left_mouse_down => {
                    self.finish_stuck_drag_as_lost_up();
                    self.reset_wedged_mouse_reporting();
                    return InputOutcome::Changed;
                }
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved | MouseEventKind::Drag(_),
                    ..
                }) => {}
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(_),
                    ..
                }) => self.finish_stuck_drag_as_lost_up(),
                _ => self.clear_stuck_scrollback_drag(),
            }
        }
        if let Some(ref child_sid) = self.active_subagent.clone() {
            if let Event::Key(key) = ev
                && key.kind != KeyEventKind::Release
                && key!('q', CONTROL).matches(key)
            {
                return InputOutcome::Unchanged;
            }
            if let Event::Mouse(mouse) = ev
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                && self
                    .hit_subagent_frame_close
                    .contains(mouse.column, mouse.row)
            {
                self.close_subagent_fullscreen();
                return InputOutcome::Changed;
            }
            if let Event::Mouse(mouse) = ev
                && matches!(mouse.kind, MouseEventKind::Moved)
                && self
                    .hit_subagent_frame_close
                    .update_hover(mouse.column, mouse.row)
            {
                return InputOutcome::Changed;
            }
            let child_in_scrollback = self
                .subagent_views
                .get(child_sid)
                .is_some_and(|c| c.is_bare_scrollback());
            if child_in_scrollback
                && let Event::Key(key) = ev
                && key.kind != KeyEventKind::Release
                && (key!('q').matches(key) || key.code == KeyCode::Esc)
            {
                self.close_subagent_fullscreen();
                return InputOutcome::Changed;
            }
            if let Some(child_view) = self.subagent_views.get_mut(child_sid) {
                child_view.mark_as_subagent_view();
                return child_view.handle_input_inner(ev, registry, prompt_paging);
            }
            return InputOutcome::Unchanged;
        }
        if self.dismiss_jump_picker_if_suppressed()
            && let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && key.code == KeyCode::Esc
            && key.modifiers.is_empty()
        {
            return InputOutcome::Changed;
        }
        if let Event::Paste(text) = ev
            && let Some(outcome) = self.try_handle_wrap_host_image_paste(text)
        {
            return outcome;
        }
        if let Event::Key(key) = ev
            && key.kind == KeyEventKind::Press
        {
            if key.code == KeyCode::Char('d')
                && key.modifiers.is_empty()
                && let Some(t) = self.esc_pressed_at.take()
                && t.elapsed() < std::time::Duration::from_millis(500)
            {
                return InputOutcome::Action(Action::DumpInputLog);
            }
            if key.code == KeyCode::Esc {
                self.esc_pressed_at = Some(std::time::Instant::now());
            } else {
                self.esc_pressed_at = None;
            }
        }
        if let Some(viewer) = &mut self.image_viewer {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key!('q', CONTROL).matches(key) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_image_viewer_key(key)
                }
                Event::Mouse(mouse) => {
                    use crate::views::modal_window::{ModalWindowOutcome, handle_modal_mouse};
                    let outcome = handle_modal_mouse(
                        &mut viewer.modal_state,
                        mouse.kind,
                        mouse.column,
                        mouse.row,
                    );
                    match outcome {
                        ModalWindowOutcome::CloseRequested => {
                            self.handle_image_viewer_key(&crossterm::event::KeyEvent::new(
                                KeyCode::Esc,
                                crossterm::event::KeyModifiers::NONE,
                            ))
                        }
                        _ => InputOutcome::Changed,
                    }
                }
                _ => InputOutcome::Changed,
            };
        }
        if self.video_viewer.is_some() {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key!('q', CONTROL).matches(key) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_video_viewer_key(key)
                }
                _ => InputOutcome::Changed,
            };
        }
        if self.gboom.is_some() {
            return match ev {
                Event::Key(key) if key.kind == KeyEventKind::Release => {
                    self.handle_gboom_release(key)
                }
                Event::Key(key) => {
                    if key!('q', CONTROL).matches(key) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_gboom_key(key)
                }
                Event::Mouse(mouse) => self.handle_gboom_mouse(mouse),
                _ => InputOutcome::Changed,
            };
        }
        if let Some(outcome) = self.handle_workflows_overlay_input(ev) {
            return outcome;
        }
        if self.show_goal_detail && self.goal_state.is_some() {
            if let Event::Key(key) = ev
                && key.kind != KeyEventKind::Release
            {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('g') | KeyCode::Char('q') => {
                        self.show_goal_detail = false;
                        return InputOutcome::Changed;
                    }
                    _ => {
                        return InputOutcome::Changed;
                    }
                }
            }
            if let Event::Mouse(mouse) = ev
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                && self.hit_goal_close.contains(mouse.column, mouse.row)
            {
                self.show_goal_detail = false;
                return InputOutcome::Changed;
            }
            if let Event::Mouse(mouse) = ev
                && matches!(mouse.kind, MouseEventKind::Moved)
                && self.hit_goal_close.update_hover(mouse.column, mouse.row)
            {
                return InputOutcome::Changed;
            }
            if matches!(ev, Event::Mouse(_) | Event::Paste(_)) {
                return InputOutcome::Changed;
            }
        }
        if self.btw_state.is_some()
            && let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && key.code == KeyCode::Esc
            && key.modifiers.is_empty()
        {
            return self.dismiss_btw_panel();
        }
        if self.btw_state.is_some()
            && let Event::Mouse(mouse) = ev
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.hit_btw_close.contains(mouse.column, mouse.row)
        {
            return self.dismiss_btw_panel();
        }
        if self.btw_state.is_some()
            && let Event::Mouse(mouse) = ev
            && matches!(mouse.kind, MouseEventKind::Moved)
            && self.hit_btw_close.update_hover(mouse.column, mouse.row)
        {
            return InputOutcome::Changed;
        }
        let btw_scroll_max = if self.active_pane == AgentPane::Prompt
            && self.btw_focused
            && let Some(btw) = self.btw_state.as_ref()
            && matches!(btw, crate::views::btw_overlay::BtwOverlayState::Done { .. })
        {
            let content_width = self.last_btw_area.width.saturating_sub(4) as usize;
            let max_body = self.last_btw_area.height.saturating_sub(2) as usize;
            btw.max_scroll_offset(content_width, max_body)
        } else {
            0
        };
        if btw_scroll_max > 0
            && let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && key.modifiers.is_empty()
            && let Some(btw) = self.btw_state.as_mut()
        {
            let page = self.last_btw_area.height.saturating_sub(2).max(1) as usize;
            match key.code {
                KeyCode::Up => {
                    btw.scroll_up(1);
                    self.clear_btw_drag_state();
                    return InputOutcome::Changed;
                }
                KeyCode::Down => {
                    btw.scroll_down(1, btw_scroll_max);
                    self.clear_btw_drag_state();
                    return InputOutcome::Changed;
                }
                KeyCode::PageUp => {
                    btw.scroll_up(page);
                    self.clear_btw_drag_state();
                    return InputOutcome::Changed;
                }
                KeyCode::PageDown => {
                    btw.scroll_down(page, btw_scroll_max);
                    self.clear_btw_drag_state();
                    return InputOutcome::Changed;
                }
                _ => {}
            }
        }
        if self.active_modal.is_some() {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if registry.lookup(key, When::Always) == Some(ActionId::Quit) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_modal_key_with_registry(key, registry)
                }
                Event::Mouse(mouse) => self.handle_modal_mouse_with_registry(mouse, registry),
                Event::Paste(text) => self.handle_modal_paste(text, registry),
                _ => InputOutcome::Changed,
            };
        }
        if self.line_viewer.is_some() && self.focused_card() != Some(BlockingCard::Permission) {
            if let Event::Mouse(mouse) = ev
                && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && self.hit_voice_stop_button.contains(mouse.column, mouse.row)
            {
                return InputOutcome::Action(Action::VoiceToggle);
            }
            let plan_prompt_focused = self
                .plan_approval_view
                .as_ref()
                .is_some_and(|p| p.focus != PlanApprovalFocus::Preview);
            let casual_commenting = self.is_casual_commenting();
            if !plan_prompt_focused && !casual_commenting {
                return match ev {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if let Some(outcome) =
                            self.try_plan_overlay_agent_action(key, registry, false)
                        {
                            return outcome;
                        }
                        self.handle_line_viewer_key(key)
                    }
                    Event::Paste(text) => {
                        self.line_viewer
                            .as_mut()
                            .map_or(InputOutcome::Unchanged, |viewer| {
                                if viewer.list_state.handle_paste(text, &viewer.lines) {
                                    InputOutcome::Changed
                                } else {
                                    InputOutcome::Unchanged
                                }
                            })
                    }
                    Event::Mouse(mouse) => self.handle_line_viewer_mouse(mouse),
                    _ => InputOutcome::Changed,
                };
            }
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if let Some(outcome) = self.try_plan_overlay_agent_action(key, registry, true) {
                        return outcome;
                    }
                    if casual_commenting {
                        self.handle_casual_plan_feedback_key(key)
                    } else {
                        self.handle_plan_feedback_key(key)
                    }
                }
                Event::Paste(text) => self.route_popup_paste(text),
                Event::Mouse(mouse) => {
                    let in_prompt = self
                        .pane_areas
                        .prompt
                        .contains((mouse.column, mouse.row).into());
                    if self.route_plan_prompt_mouse_drag(mouse, in_prompt) {
                        self.prompt.handle_mouse(mouse);
                        InputOutcome::Changed
                    } else {
                        self.handle_line_viewer_mouse(mouse)
                    }
                }
                _ => InputOutcome::Changed,
            };
        }
        if self.extensions_modal.is_some() {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if registry.lookup(key, When::Always).is_some() {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_extensions_modal_key(key)
                }
                Event::Mouse(mouse) => self.handle_extensions_modal_mouse(mouse),
                Event::Paste(text) => self.handle_extensions_modal_paste(text),
                _ => InputOutcome::Changed,
            };
        }
        if self.persona_detail.is_some() {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if registry.lookup(key, When::Always).is_some() {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_persona_detail_key(key)
                }
                Event::Mouse(mouse) => self.handle_persona_detail_mouse(mouse),
                Event::Paste(text) => self.handle_persona_detail_paste(text),
                _ => InputOutcome::Changed,
            };
        }
        if self.agents_modal.is_some() {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if registry.lookup(key, When::Always).is_some() {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_agents_modal_key(key)
                }
                Event::Mouse(mouse) => self.handle_agents_modal_mouse(mouse),
                Event::Paste(text) => self.handle_agents_modal_paste(text),
                _ => InputOutcome::Changed,
            };
        }
        if self.block_viewer.is_some() {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if registry.lookup(key, When::Always).is_some() {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_block_viewer_key(key)
                }
                Event::Mouse(mouse) => self.handle_block_viewer_mouse(mouse),
                Event::Paste(text) => {
                    self.block_viewer
                        .as_mut()
                        .map_or(InputOutcome::Unchanged, |viewer| {
                            if viewer.handle_paste(text) {
                                InputOutcome::Changed
                            } else {
                                InputOutcome::Unchanged
                            }
                        })
                }
                _ => InputOutcome::Changed,
            };
        }
        if self.active_modal.is_some() {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if registry.lookup(key, When::Always) == Some(ActionId::Quit) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_modal_key_with_registry(key, registry)
                }
                Event::Mouse(mouse) => self.handle_modal_mouse_with_registry(mouse, registry),
                Event::Paste(text) => self.handle_modal_paste(text, registry),
                _ => InputOutcome::Changed,
            };
        }
        if self.focused_card() == Some(BlockingCard::Permission) {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key!('q', CONTROL).matches(key) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_permission_key(key)
                }
                Event::Mouse(mouse) => {
                    self.scrollbar_dragging = false;
                    let in_followup = self.permission_queue.front().is_some_and(|p| {
                        p.focus == crate::views::permission_view::PermissionFocus::FollowupInput
                    });
                    match mouse.kind {
                        MouseEventKind::Moved => {
                            let item = self.permission_item_at(mouse.column, mouse.row);
                            if item != self.hovered_permission_item {
                                self.hovered_permission_item = item;
                                InputOutcome::Changed
                            } else {
                                InputOutcome::Unchanged
                            }
                        }
                        MouseEventKind::Down(MouseButton::Left) => {
                            if let Some(idx) = self.permission_item_at(mouse.column, mouse.row) {
                                let now = Instant::now();
                                let is_double_click =
                                    self.last_permission_click.is_some_and(|(t, prev_idx)| {
                                        prev_idx == idx
                                            && now.duration_since(t).as_millis()
                                                < MULTI_CLICK_TIMEOUT_MS
                                    });
                                if let Some(perm) = self.permission_queue.front_mut() {
                                    perm.active_idx = idx;
                                    perm.focus =
                                        crate::views::permission_view::PermissionFocus::Options;
                                    self.permission_pattern_edit = None;
                                    if is_double_click {
                                        self.last_permission_click = None;
                                        if let Some(opt) = perm.options.get(idx) {
                                            return InputOutcome::Action(Action::PermissionSelect(
                                                opt.option_id.clone(),
                                            ));
                                        }
                                    }
                                }
                                self.last_permission_click = Some((now, idx));
                            } else {
                                self.last_permission_click = None;
                                if in_followup {
                                    self.prompt.handle_mouse(mouse);
                                }
                            }
                            InputOutcome::Changed
                        }
                        _ => {
                            if in_followup {
                                self.prompt.handle_mouse(mouse);
                            }
                            InputOutcome::Changed
                        }
                    }
                }
                Event::Paste(text) => {
                    let front_focus = self.permission_queue.front().map(|p| p.focus);
                    match front_focus {
                        Some(crate::views::permission_view::PermissionFocus::FollowupInput) => {
                            self.route_popup_paste(text)
                        }
                        Some(crate::views::permission_view::PermissionFocus::PatternEdit) => {
                            if let Some(edit) = self.permission_pattern_edit.as_mut() {
                                for ch in text.chars().filter(|c| *c != '\n' && *c != '\r') {
                                    edit.insert_char(ch);
                                }
                            }
                            InputOutcome::Changed
                        }
                        _ => InputOutcome::Changed,
                    }
                }
                _ => InputOutcome::Changed,
            };
        }
        if self.key_owner() == KeyOwner::PlanApproval {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if let Some(outcome) = self.try_plan_overlay_agent_action(key, registry, true) {
                        return outcome;
                    }
                    self.handle_plan_feedback_key(key)
                }
                Event::Paste(text) => {
                    if self
                        .plan_approval_view
                        .as_ref()
                        .is_some_and(|view| view.focus != PlanApprovalFocus::Preview)
                    {
                        self.route_popup_paste(text)
                    } else {
                        InputOutcome::Unchanged
                    }
                }
                Event::Mouse(mouse) => {
                    let mut changed = false;
                    match mouse.kind {
                        MouseEventKind::Moved => {
                            changed |= self.hit_plan_button.update_hover(mouse.column, mouse.row);
                            changed |= self
                                .hit_plan_approval_status
                                .update_hover(mouse.column, mouse.row);
                            changed |= self.hit_context.update_hover(mouse.column, mouse.row);
                            changed |= self.hit_credits.update_hover(mouse.column, mouse.row);
                        }
                        MouseEventKind::Down(MouseButton::Left) => {
                            if self.hit_voice_stop_button.contains(mouse.column, mouse.row) {
                                return InputOutcome::Action(Action::VoiceToggle);
                            }
                            if self.hit_plan_button.contains(mouse.column, mouse.row) {
                                self.reopen_plan_approval();
                                return InputOutcome::Changed;
                            }
                            if self
                                .hit_plan_approval_status
                                .contains(mouse.column, mouse.row)
                            {
                                self.reopen_plan_approval();
                                return InputOutcome::Changed;
                            }
                        }
                        _ => {}
                    }
                    let in_prompt = self
                        .pane_areas
                        .prompt
                        .contains((mouse.column, mouse.row).into());
                    if self.route_plan_prompt_mouse_drag(mouse, in_prompt) {
                        self.prompt.handle_mouse(mouse);
                        return InputOutcome::Changed;
                    }
                    if changed {
                        InputOutcome::Changed
                    } else {
                        InputOutcome::Unchanged
                    }
                }
                _ => InputOutcome::Changed,
            };
        }
        if self.focused_card() == Some(BlockingCard::Question) {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key!('q', CONTROL).matches(key) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_question_key(key)
                }
                Event::Mouse(mouse) => self.handle_question_mouse(mouse),
                Event::Paste(text) => {
                    let in_input = self
                        .question_view
                        .as_ref()
                        .map(|qv| qv.focus == crate::views::question_view::QuestionFocus::InputMode)
                        .unwrap_or(false);
                    if in_input {
                        self.route_question_paste(text)
                    } else {
                        InputOutcome::Changed
                    }
                }
                _ => InputOutcome::Changed,
            };
        }
        if self.focused_card() == Some(BlockingCard::McpElicitation) {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key!('q', CONTROL).matches(key) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_elicitation_key(key)
                }
                Event::Paste(text) => self.handle_elicitation_paste(text),
                Event::Mouse(mouse) => self.handle_elicitation_mouse(mouse),
                _ => InputOutcome::Changed,
            };
        }
        if self.rewind_state.is_some() {
            return match ev {
                Event::Key(key) if key.kind != crossterm::event::KeyEventKind::Release => {
                    if key!('q', CONTROL).matches(key) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_rewind_key(key)
                }
                Event::Mouse(mouse) => self.handle_rewind_mouse(mouse),
                _ => InputOutcome::Unchanged,
            };
        }
        if self.inline_edit.is_some() {
            return match ev {
                Event::Key(key) if key.kind != crossterm::event::KeyEventKind::Release => {
                    if key!('q', CONTROL).matches(key) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_inline_edit_key(key)
                }
                Event::Mouse(mouse) => self.handle_inline_edit_mouse(mouse),
                Event::Paste(text) => {
                    if let Some(ref mut edit) = self.inline_edit {
                        edit.textarea.insert_str(text);
                    }
                    InputOutcome::Changed
                }
                _ => InputOutcome::Unchanged,
            };
        }
        if self.jump_state.is_some() {
            return match ev {
                Event::Key(key) if key.kind != crossterm::event::KeyEventKind::Release => {
                    if key!('q', CONTROL).matches(key) {
                        return InputOutcome::Unchanged;
                    }
                    if registry.matches_id(ActionId::CancelTurn, key)
                        && (self.stoppable_activity_running() || self.any_cancel_pending())
                    {
                        self.dismiss_jump_picker();
                        return self
                            .handle_agent_action_with_registry(ActionId::CancelTurn, registry);
                    }
                    self.handle_jump_key(key)
                }
                Event::Mouse(mouse) => self.handle_jump_mouse(mouse),
                _ => InputOutcome::Unchanged,
            };
        }
        if self.focused_card() == Some(BlockingCard::CancelTurn) {
            return match ev {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key!('q', CONTROL).matches(key) {
                        return InputOutcome::Unchanged;
                    }
                    self.handle_cancel_turn_key(key)
                }
                Event::Mouse(mouse) => self.handle_cancel_turn_mouse(mouse),
                _ => InputOutcome::Unchanged,
            };
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && registry.matches_id(ActionId::SendToBackground, key)
        {
            return self.handle_agent_action_with_registry(ActionId::SendToBackground, registry);
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && key!('y', CONTROL).matches(key)
            && self.ephemeral_tip.current_key()
                == Some(crate::tips::word_select::WORD_SELECT_TIP_KEY)
            && self.ephemeral_tip_can_render()
            && self.word_select_tip_prompt_snapshot.as_deref() == Some(self.prompt.text())
        {
            return InputOutcome::Action(Action::AcceptWordSelectTip);
        }
        let outcome = match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => match self.active_pane {
                AgentPane::Prompt => self.handle_prompt_key(key, registry, prompt_paging),
                AgentPane::Scrollback => self.handle_scrollback_key(key, registry),
                AgentPane::Todo => self.handle_todo_key(key, registry),
                AgentPane::Queue => self.handle_queue_key(key, registry),
                AgentPane::Tasks => self.handle_bg_tasks_key(key, registry),
                AgentPane::Catalog => self.handle_catalog_key(key, registry),
            },
            Event::Paste(text) => {
                if self.active_pane == AgentPane::Scrollback
                    && let Some(outcome) = self.handle_scrollback_search_paste(text)
                {
                    return outcome;
                }
                if self.active_pane == AgentPane::Scrollback
                    && !self.vim_mode
                    && self.no_input_overlay_pending()
                {
                    return InputOutcome::ActionThenForward(Action::FocusPrompt);
                }
                if self.active_pane == AgentPane::Prompt {
                    self.ephemeral_tip
                        .clear(crate::tips::clipboard_focus::CLIPBOARD_IMAGE_TIP_KEY);
                    self.btw_focused = false;
                    if let Some((outcome, _)) = self.try_handle_dropped_paths_paste(text) {
                        return outcome;
                    }
                    self.probe_attachment_around_bracketed_insert(text, |view| {
                        view.insert_bracketed_prompt_text(text)
                    })
                } else {
                    let consumed = match self.active_pane {
                        AgentPane::Todo => self.todo.handle_paste(text),
                        AgentPane::Tasks => self.tasks.handle_paste(text),
                        AgentPane::Catalog => self.catalog.handle_paste(text),
                        AgentPane::Queue => self.queue.handle_paste(text),
                        AgentPane::Prompt | AgentPane::Scrollback => false,
                    };
                    if consumed {
                        InputOutcome::Changed
                    } else {
                        InputOutcome::Unchanged
                    }
                }
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            _ => InputOutcome::Unchanged,
        };
        if !matches!(outcome, InputOutcome::Unchanged) {
            return outcome;
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && key!('t', CONTROL).matches(key)
        {
            self.todo.overlay.toggle();
            self.todo.on_state_change();
            if self.todo.overlay.focused {
                self.set_active_pane(AgentPane::Todo, false);
            } else if self.active_pane == AgentPane::Todo {
                self.set_active_pane(AgentPane::Scrollback, false);
            }
            return InputOutcome::Changed;
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && registry.matches_id(ActionId::ToggleTasks, key)
        {
            self.tasks.overlay.toggle();
            self.tasks.on_state_change();
            if self.tasks.overlay.focused {
                self.set_active_pane(AgentPane::Tasks, false);
            } else if self.active_pane == AgentPane::Tasks {
                self.set_active_pane(AgentPane::Scrollback, false);
            }
            return InputOutcome::Changed;
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && registry.matches_id(ActionId::OpenSessions, key)
        {
            self.active_modal = Some(ActiveModal::SessionPicker {
                state: crate::views::picker::PickerState::default(),
                entries: None,
                loading: true,
                lanes: Default::default(),
                previous_palette: None,
                window: crate::views::modal_window::ModalWindowState::new(),
                content_results: None,
                content_loading: false,
                deep_search_seq: 0,
                entries_query: None,
                source_filter: crate::views::session_picker::SourceFilter::default(),
                pending_delete: None,
            });
            return InputOutcome::Action(Action::FetchSessionList);
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && registry.matches_id(ActionId::ToggleQueue, key)
            && (self.queue.is_visible() || !self.visible_queue_is_empty())
        {
            self.toggle_queue_pane();
            return InputOutcome::Changed;
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && registry.lookup(key, When::AgentScreen) == Some(ActionId::OpenExtensions)
        {
            crate::actions::log_shortcut_used(
                key,
                ActionId::OpenExtensions,
                When::AgentScreen.telemetry_name(),
            );
            return InputOutcome::Action(Action::OpenExtensionsModal {
                tab: crate::views::extensions_modal::ExtensionsTab::Plugins,
                trigger: echo_agent_telemetry::events::ExtensionsModalTrigger::KeyboardShortcut,
            });
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && key!('/', CONTROL).matches(key)
            && matches!(
                self.plugin_cta.phase,
                CtaPhase::Matched { .. } | CtaPhase::Error { .. }
            )
        {
            self.connect_matched_plugin();
            return InputOutcome::Changed;
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && self.active_pane != AgentPane::Prompt
            && (key!('p', CONTROL).matches(key)
                || key.code == KeyCode::Char('?')
                || (key.code == KeyCode::Char('/') && key.modifiers.contains(KeyModifiers::SHIFT)))
        {
            self.active_modal = Some(crate::views::modal::ActiveModal::CommandPalette {
                entries: crate::views::modal::default_palette_entries(
                    self.sharing_enabled,
                    &self.prompt.slash_controller,
                ),
                state: crate::views::picker::PickerState::input_active(),
                window: crate::views::modal_window::ModalWindowState::new(),
            });
            return InputOutcome::Changed;
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && key.code == KeyCode::Char('g')
            && key.modifiers.is_empty()
            && (self.goal_state.is_some() || !self.workflow_runs.is_empty())
        {
            let has_active_workflow = self.workflow_runs.iter().any(|run| !run.is_terminal());
            return InputOutcome::Action(if self.goal_state.is_some() && !has_active_workflow {
                Action::ToggleGoalDetail
            } else {
                Action::ToggleWorkflows
            });
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
        {
            if is_mouse_reporting_toggle_chord(key) {
                let looked_up = registry.lookup(key, When::ScrollbackFocused);
                crate::unified_log::info(
                    "mouse_reporting_toggle.key",
                    None,
                    Some(serde_json::json!({
                        "path": "agent_view.scrollback_or_pane",
                        "active_pane": format!("{:?}", self.active_pane),
                        "key": format_key_for_log(key),
                        "lookup": looked_up.map(|id| format!("{id:?}")),
                        "action_registered": registry
                            .find(ActionId::ToggleMouseCapture)
                            .is_some(),
                    })),
                );
            }
            if let Some(action_id) = registry.lookup(key, When::AgentScreen) {
                return self.handle_agent_action_with_registry(action_id, registry);
            }
        }
        if let Event::Key(key) = ev
            && self.update_hovered_link(is_link_modifier_for_key(key))
        {
            return InputOutcome::Changed;
        }
        if let Event::Key(key) = ev
            && key.kind != KeyEventKind::Release
            && matches!(self.active_pane, AgentPane::Prompt | AgentPane::Scrollback)
            && let Some(outcome) = self.try_handle_esc_policy(key)
        {
            return outcome;
        }
        InputOutcome::Unchanged
    }
    /// Handle an agent-level action using the compatibility fullscreen registry.
    /// Runtime key dispatch uses [`Self::handle_agent_action_with_registry`].
    #[cfg(test)]
    pub(super) fn handle_agent_action(&mut self, action_id: ActionId) -> InputOutcome {
        let registry = ActionRegistry::defaults();
        self.handle_agent_action_with_registry(action_id, &registry)
    }
    /// Model/palette while plan approval owns the keyboard. `typing`: bare
    /// keys (e.g. `?`) go to the prompt; only Ctrl/Super/Alt chords pass.
    fn try_plan_overlay_agent_action(
        &mut self,
        key: &crossterm::event::KeyEvent,
        registry: &ActionRegistry,
        typing: bool,
    ) -> Option<InputOutcome> {
        if registry.lookup(key, When::Always) == Some(ActionId::Quit) {
            return Some(InputOutcome::Unchanged);
        }
        let action_id = registry.lookup(key, When::AgentScreen)?;
        match action_id {
            ActionId::ModelPicker | ActionId::CommandPalette => {
                if typing
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT)
                {
                    return None;
                }
                Some(self.handle_agent_action_with_registry(action_id, registry))
            }
            _ => None,
        }
    }
    /// Handle an agent-level action (from registry lookup with `When::AgentScreen`).
    pub(super) fn handle_agent_action_with_registry(
        &mut self,
        action_id: ActionId,
        registry: &ActionRegistry,
    ) -> InputOutcome {
        match action_id {
            ActionId::CancelTurn => {
                if self.stoppable_activity_running() {
                    self.cancel_trigger_hint = Some(crate::app::actions::CancelTrigger::CtrlC);
                    return InputOutcome::Action(Action::CancelTurn);
                }
                if self.any_cancel_pending() {
                    return InputOutcome::Action(Action::Quit);
                }
                if crate::app::minimal_mode_active()
                    && self.session.state.is_idle()
                    && self.prompt.text().is_empty()
                {
                    return InputOutcome::Action(Action::Quit);
                }
                InputOutcome::Unchanged
            }
            ActionId::ToggleYolo => {
                if self.pinned_upgrade_cta_live {
                    InputOutcome::Action(Action::AnnouncementsOpenCta(
                        echo_agent_telemetry::events::AnnouncementCtaSurface::Keyboard,
                    ))
                } else {
                    InputOutcome::Action(Action::SetYoloMode(!self.session.is_yolo()))
                }
            }
            ActionId::SendToBackground => {
                if !self.is_subagent_view
                    && self
                        .session
                        .tracker
                        .running_execute_tool_call_id()
                        .is_some()
                {
                    InputOutcome::Action(Action::DemoteToBackground)
                } else {
                    InputOutcome::Changed
                }
            }
            ActionId::EditPromptExternal => {
                if self.external_prompt_editor_access()
                    == ExternalPromptEditorAccess::OwnedElsewhere
                {
                    InputOutcome::Changed
                } else {
                    InputOutcome::Action(Action::EditPromptExternal)
                }
            }
            ActionId::CommandPalette => {
                self.active_modal = Some(crate::views::modal::ActiveModal::CommandPalette {
                    entries: crate::views::modal::default_palette_entries(
                        self.sharing_enabled,
                        &self.prompt.slash_controller,
                    ),
                    state: crate::views::picker::PickerState::input_active(),
                    window: crate::views::modal_window::ModalWindowState::new(),
                });
                InputOutcome::Changed
            }
            ActionId::ModelPicker => {
                let command = "model";
                if let Some(cmd) = self.prompt.slash_controller.registry().get(command) {
                    let ctx = self.prompt.slash_controller.app_ctx(&self.session.models);
                    if let Some(items) = cmd.suggest_args(&ctx, "")
                        && !items.is_empty()
                    {
                        self.active_modal = Some(crate::views::modal::ActiveModal::ArgPicker {
                            command: command.to_string(),
                            args_query: String::new(),
                            items: items.clone(),
                            original_items: items,
                            state: crate::views::picker::PickerState::input_active(),
                            previous_palette: None,
                            window: crate::views::modal_window::ModalWindowState::new(),
                        });
                        return InputOutcome::Changed;
                    }
                }
                InputOutcome::Changed
            }
            ActionId::ShortcutsHelp => {
                use crate::views::shortcuts_help;
                let mut contexts = active_contexts_for_pane(self.active_pane);
                if self.in_dashboard_overlay {
                    contexts.push(crate::actions::When::DashboardOverlay);
                }
                let entries = shortcuts_help::build_entries(&contexts, registry, self.vim_mode);
                let state = shortcuts_help::build_initial_picker_state(&entries);
                self.active_modal = Some(crate::views::modal::ActiveModal::ShortcutsHelp {
                    entries,
                    state,
                    window: Default::default(),
                    filter_active: false,
                    collapsed_sections: crate::views::shortcuts_help::default_collapsed(),
                    expanded_ids: std::collections::HashSet::new(),
                    mode: crate::views::shortcuts_help::ShortcutsHelpMode::Browse,
                });
                InputOutcome::Changed
            }
            ActionId::OpenSettings => InputOutcome::Action(Action::OpenSettings),
            ActionId::ToggleMouseCapture => {
                crate::unified_log::info(
                    "mouse_reporting_toggle.handle_agent_action",
                    None,
                    Some(serde_json::json!({
                        "returning": "Action::ToggleMouseCapture",
                    })),
                );
                InputOutcome::Action(Action::ToggleMouseCapture)
            }
            other => resolve_action(Some(other)).unwrap_or(InputOutcome::Unchanged),
        }
    }
    /// Returns `true` if the switch happened immediately, `false` if blocked.
    pub(crate) fn set_active_pane(&mut self, target: AgentPane, force: bool) -> bool {
        if target != AgentPane::Scrollback {
            self.scrollback_search = None;
        }
        if force {
            if target != AgentPane::Todo {
                self.todo.overlay.focused = false;
            }
            if target != AgentPane::Tasks {
                self.tasks.overlay.focused = false;
            }
            if target != AgentPane::Catalog {
                self.catalog.overlay.focused = false;
            }
            if target != AgentPane::Queue {
                self.queue.overlay.focused = false;
            }
            self.active_pane = target;
            return true;
        }
        if let Some(switched) = self.editing_lock_on_pane_switch(target) {
            return switched;
        }
        if target != AgentPane::Todo {
            self.todo.overlay.focused = false;
        }
        if target != AgentPane::Tasks {
            self.tasks.overlay.focused = false;
        }
        if target != AgentPane::Catalog {
            self.catalog.overlay.focused = false;
        }
        if target != AgentPane::Queue {
            self.queue.overlay.focused = false;
        }
        self.active_pane = target;
        true
    }
    pub(crate) fn set_input_mode(&mut self, mode: InputMode) {
        self.input_mode = mode;
        if mode == InputMode::Vim
            && self.prompt.text().trim().is_empty()
            && self.active_pane == AgentPane::Prompt
        {
            let _switched = self.set_active_pane(AgentPane::Scrollback, false);
        }
    }
    /// Propagate a vim-mode change to this view AND every nested
    /// subagent view.
    ///
    /// `ToggleVimMode` / `SetVimMode` only walk the top-level
    /// `app.agents`, so without this an already-open subagent view keeps
    /// its stale `vim_mode`. The bug that surfaces: the user opens a
    /// subagent, runs `/vim-mode`, presses Tab to focus the subagent's
    /// scrollback, and `j`/`k` forward to the prompt (the vim-OFF
    /// fallback) instead of navigating — because the subagent view never
    /// saw the toggle.
    pub(crate) fn set_vim_mode_recursive(&mut self, enabled: bool) {
        self.vim_mode = enabled;
        for child in self.subagent_views.values_mut() {
            child.set_vim_mode_recursive(enabled);
        }
    }
    #[cfg(test)]
    pub(crate) fn is_simple_mode(&self) -> bool {
        self.input_mode == InputMode::Simple
    }
}
#[cfg(test)]
mod background_and_tasks_shortcut_tests {
    use super::super::AgentPane;
    use super::super::test_fixtures::{add_running_bg_task, add_running_execute, make_agent};
    use crate::actions::ActionRegistry;
    use crate::app::actions::Action;
    use crate::app::app_view::InputOutcome;
    use crate::views::history_search::HistoryEntry;
    use crate::views::list_pane::InputBarMode;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }
    fn assert_demotes(outcome: InputOutcome) {
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::DemoteToBackground)
        ));
    }
    fn seed_file_search(agent: &mut super::super::AgentView) {
        agent.prompt.set_text("@src");
        agent.prompt.set_cursor(2);
        let context = crate::views::file_search::context::detect("@src", "@src".len())
            .expect("file-search context");
        agent.prompt.file_search.set_test_state(
            context,
            vec![echo_agent_workspace::file_system::FuzzyMatchResult {
                path: nucleo::Utf32String::from("src/lib.rs"),
                score: 100,
                indices: Vec::new(),
                is_dir: false,
            }],
            0,
        );
    }
    #[test]
    fn ctrl_b_is_consumed_when_ineligible_and_demotes_when_eligible() {
        let registry = ActionRegistry::defaults();
        for pane in [AgentPane::Prompt, AgentPane::Scrollback] {
            let mut agent = make_agent();
            agent.set_active_pane(pane, true);
            agent.prompt.set_text("draft");
            let cursor = agent.prompt.text().len();
            agent.prompt.set_cursor(cursor);
            assert!(matches!(
                agent.handle_input(&ctrl('b'), &registry),
                InputOutcome::Changed
            ));
            assert_eq!(agent.active_pane, pane);
            assert_eq!(agent.prompt.text(), "draft");
            assert_eq!(agent.prompt.cursor(), cursor);
            assert!(!agent.tasks.overlay.focused);
            add_running_execute(&mut agent);
            assert_demotes(agent.handle_input(&ctrl('b'), &registry));
            assert_eq!(agent.active_pane, pane);
            assert_eq!(agent.prompt.text(), "draft");
            assert_eq!(agent.prompt.cursor(), cursor);
        }
    }
    #[test]
    fn ctrl_b_preempts_history_browse_and_search_without_mutating_them() {
        let registry = ActionRegistry::defaults();
        for browse in [true, false] {
            let mut agent = make_agent();
            agent.set_active_pane(AgentPane::Prompt, true);
            let history = [HistoryEntry {
                text: "earlier prompt".into(),
            }];
            if browse {
                assert!(agent.prompt.history_search.activate_browse(&history, ""));
                agent.prompt.set_text("earlier prompt");
            } else {
                assert!(agent.prompt.history_search.activate(&history, "query"));
                agent.prompt.set_text("query");
            }
            let text = agent.prompt.text().to_string();
            let cursor = agent.prompt.cursor();
            let selected = agent.prompt.history_search.selected;
            add_running_execute(&mut agent);
            assert_demotes(agent.handle_input(&ctrl('b'), &registry));
            assert!(agent.prompt.history_search.is_active());
            assert_eq!(agent.prompt.history_search.is_browse(), browse);
            assert_eq!(agent.prompt.history_search.selected, selected);
            assert_eq!(agent.prompt.text(), text);
            assert_eq!(agent.prompt.cursor(), cursor);
        }
    }
    #[test]
    fn shortcuts_key_tears_down_history_and_opens_cheatsheet() {
        use crate::views::modal::ActiveModal;
        let registry = ActionRegistry::defaults();
        let history = [HistoryEntry {
            text: "earlier prompt".into(),
        }];
        for key in [
            KeyEvent::new(KeyCode::Char('.'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
        ] {
            for browse in [true, false] {
                let mut agent = make_agent();
                agent.set_active_pane(AgentPane::Prompt, true);
                if browse {
                    assert!(agent.prompt.history_search.activate_browse(&history, ""));
                    agent.prompt.set_text("earlier prompt");
                } else {
                    assert!(agent.prompt.history_search.activate(&history, "query"));
                    agent.prompt.set_text("query");
                }
                let out = agent.handle_prompt_key_with_registry_for_test(&key, &registry);
                assert!(matches!(out, InputOutcome::Changed));
                assert!(
                    matches!(agent.active_modal, Some(ActiveModal::ShortcutsHelp { .. })),
                    "shortcuts key must open the cheatsheet"
                );
                assert!(
                    !agent.prompt.history_search.is_active(),
                    "history overlay must be torn down first"
                );
            }
        }
    }
    #[test]
    fn ctrl_b_preempts_file_search_without_mutating_it() {
        let registry = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.set_active_pane(AgentPane::Prompt, true);
        seed_file_search(&mut agent);
        assert!(agent.prompt.file_search_visible());
        let context = agent.prompt.file_search.context().cloned();
        let selected = agent.prompt.file_search.selected();
        let cursor = agent.prompt.cursor();
        add_running_execute(&mut agent);
        assert_demotes(agent.handle_input(&ctrl('b'), &registry));
        assert_eq!(agent.prompt.file_search.context(), context.as_ref());
        assert_eq!(agent.prompt.file_search.selected(), selected);
        assert_eq!(agent.prompt.text(), "@src");
        assert_eq!(agent.prompt.cursor(), cursor);
    }
    #[test]
    fn ctrl_b_preempts_tasks_search_and_filter_without_mutating_them() {
        let registry = ActionRegistry::defaults();
        for (open_key, expected_mode) in [('/', InputBarMode::Search), ('f', InputBarMode::Filter)]
        {
            let mut agent = make_agent();
            add_running_bg_task(&mut agent);
            agent.tasks.overlay.visible = true;
            agent.tasks.overlay.focused = true;
            agent.set_active_pane(AgentPane::Tasks, true);
            assert!(
                agent
                    .tasks
                    .handle_key(&KeyEvent::new(KeyCode::Char(open_key), KeyModifiers::NONE,))
            );
            for c in "needle".chars() {
                assert!(
                    agent
                        .tasks
                        .handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE,))
                );
            }
            add_running_execute(&mut agent);
            assert_demotes(agent.handle_input(&ctrl('b'), &registry));
            assert_eq!(agent.active_pane, AgentPane::Tasks);
            assert!(agent.tasks.overlay.visible);
            assert!(agent.tasks.overlay.focused);
            assert_eq!(agent.tasks.list_state.input_mode(), Some(expected_mode));
            assert_eq!(agent.tasks.list_state.input_text(), "needle");
        }
    }
    #[test]
    fn fullscreen_child_ctrl_b_never_demotes_child_or_parent() {
        let registry = ActionRegistry::defaults();
        let child_sid = "child-sid".to_string();
        let mut parent = make_agent();
        add_running_execute(&mut parent);
        assert!(
            parent
                .session
                .tracker
                .running_execute_tool_call_id()
                .is_some()
        );
        let mut child = make_agent();
        add_running_execute(&mut child);
        assert!(
            child
                .session
                .tracker
                .running_execute_tool_call_id()
                .is_some()
        );
        child.set_active_pane(AgentPane::Scrollback, true);
        parent
            .subagent_views
            .insert(child_sid.clone(), Box::new(child));
        assert!(!parent.subagent_views[&child_sid].is_subagent_view);
        parent.open_subagent_fullscreen(child_sid.clone());
        assert!(parent.subagent_views[&child_sid].is_subagent_view);
        let outcome = parent.handle_input(&ctrl('b'), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(!matches!(
            outcome,
            InputOutcome::Action(Action::DemoteToBackground)
        ));
        assert_eq!(parent.active_subagent.as_deref(), Some(child_sid.as_str()));
        assert!(
            parent
                .session
                .tracker
                .running_execute_tool_call_id()
                .is_some()
        );
        assert!(
            parent.subagent_views[&child_sid]
                .session
                .tracker
                .running_execute_tool_call_id()
                .is_some()
        );
        let child = &parent.subagent_views[&child_sid];
        assert!(child.is_subagent_view);
        assert!(
            !child
                .current_shortcut_hints(&registry, false)
                .iter()
                .any(|hint| hint.label == "send to bg")
        );
        assert!(child.hit_bg_button.rect.is_none());
    }
    #[test]
    fn ctrl_g_toggles_tasks_and_never_demotes() {
        let registry = ActionRegistry::defaults();
        let mut agent = make_agent();
        agent.prompt.set_text("draft");
        let draft_len = agent.prompt.text().len();
        agent.prompt.set_cursor(draft_len);
        agent.set_active_pane(AgentPane::Prompt, true);
        add_running_execute(&mut agent);
        let first = agent.handle_input(&ctrl('g'), &registry);
        assert!(matches!(first, InputOutcome::Changed));
        assert_eq!(agent.active_pane, AgentPane::Tasks);
        assert!(agent.tasks.overlay.visible);
        assert!(agent.tasks.overlay.focused);
        assert_eq!(agent.prompt.text(), "draft");
        assert_eq!(agent.prompt.cursor(), draft_len);
        let second = agent.handle_input(&ctrl('g'), &registry);
        assert!(matches!(
            second,
            InputOutcome::Action(Action::FocusScrollback)
        ));
        assert!(!agent.tasks.overlay.visible);
        assert!(!agent.tasks.overlay.focused);
    }
}
#[cfg(test)]
mod command_palette_input_default_tests {
    use super::test_fixtures::make_agent;
    use crate::actions::ActionId;
    use crate::views::modal::ActiveModal;
    /// Type-to-find: the command palette opens directly in INPUT mode
    /// (`search_active = true`) so a letter filters immediately. Under vim, Esc
    /// drops to nav and `i` re-enters input (covered by the PTY scenario).
    #[test]
    fn command_palette_opens_in_input_mode() {
        let mut agent = make_agent();
        let _ = agent.handle_agent_action(ActionId::CommandPalette);
        let Some(ActiveModal::CommandPalette { state, .. }) = &agent.active_modal else {
            panic!("expected CommandPalette modal to be open");
        };
        assert!(
            state.search_active,
            "command palette must open in input mode (search_active=true)"
        );
    }
}
#[cfg(test)]
mod btw_focus_tests {
    use super::test_fixtures::make_agent;
    use super::{AgentPane, AgentView};
    use crate::actions::ActionRegistry;
    use crate::app::app_view::InputOutcome;
    use crate::views::btw_overlay::BtwOverlayState;
    use crate::views::jump::{JumpRestore, JumpState};
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;
    /// Idle agent focused on the prompt (the realistic state while `/btw` is
    /// open). `make_agent` starts in scrollback focus (vim default), and these
    /// tests don't render, so we focus the prompt and seed `last_btw_area`
    /// (keyboard scrollability reads from it). 80x14 → 76-col body, 12 rows.
    fn prompt_focused_agent() -> AgentView {
        let mut agent = make_agent();
        agent.set_active_pane(AgentPane::Prompt, true);
        agent.last_btw_area = Rect::new(0, 0, 80, 14);
        agent
    }
    /// A `/btw` answer with far more lines than the panel can show, so it is
    /// always scrollable regardless of the test terminal width.
    fn long_btw_answer() -> String {
        (0..40)
            .map(|i| format!("line{i:02}"))
            .collect::<Vec<_>>()
            .join("  \n")
    }
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn done_scroll_offset(agent: &AgentView) -> usize {
        agent
            .btw_state
            .as_ref()
            .expect("btw panel present")
            .scroll_offset()
    }
    fn minimal_btw_agent() -> AgentView {
        let mut agent = prompt_focused_agent();
        let request_id = crate::minimal_api::start_minimal_btw(&mut agent, "q".into());
        assert!(crate::minimal_api::finish_minimal_btw(
            &mut agent,
            request_id,
            Ok(long_btw_answer())
        ));
        agent
    }
    fn assert_minimal_btw_active(agent: &AgentView, surface: &str) {
        assert!(
            agent.btw_state.is_some(),
            "{surface} Esc must leave the latent /btw panel intact"
        );
        assert!(
            matches!(
                agent.minimal_btw_lifecycle,
                Some(crate::minimal_api::MinimalBtwLifecycle::Active { .. })
            ),
            "{surface} Esc must restore the complete minimal /btw lifecycle"
        );
    }
    #[test]
    fn focused_panel_scrolls_with_arrows() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.btw_focused = true;
        assert!(matches!(
            agent.handle_input(&key(KeyCode::Down), &reg),
            InputOutcome::Changed
        ));
        assert_eq!(done_scroll_offset(&agent), 1);
        agent.handle_input(&key(KeyCode::Down), &reg);
        assert_eq!(done_scroll_offset(&agent), 2);
        agent.handle_input(&key(KeyCode::Up), &reg);
        assert_eq!(done_scroll_offset(&agent), 1);
        assert!(agent.btw_focused);
    }
    #[test]
    fn focused_panel_owns_page_keys_before_prompt_paging() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.btw_focused = true;
        let outcome = agent.handle_input_with_prompt_paging(&key(KeyCode::PageDown), &reg);
        assert!(matches!(outcome, InputOutcome::Changed));
        let after_down = done_scroll_offset(&agent);
        assert!(after_down > 0);
        let outcome = agent.handle_input_with_prompt_paging(&key(KeyCode::PageUp), &reg);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(done_scroll_offset(&agent) < after_down);
        assert!(agent.btw_focused);
    }
    #[test]
    fn visible_unfocused_panel_allows_prompt_paging() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.btw_focused = false;
        let outcome = agent.handle_input_with_prompt_paging(&key(KeyCode::PageDown), &reg);
        assert!(
            matches!(
                &outcome,
                InputOutcome::Action(crate::app::actions::Action::PageDown)
            ),
            "an unfocused /btw panel has declined PageDown ownership: {outcome:?}"
        );
        assert_eq!(done_scroll_offset(&agent), 0);
    }
    #[test]
    fn typing_returns_focus_to_prompt() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.btw_focused = true;
        agent.handle_input(&key(KeyCode::Char('h')), &reg);
        assert!(
            !agent.btw_focused,
            "typing should return focus to the prompt"
        );
        assert_eq!(done_scroll_offset(&agent), 0);
        assert_eq!(agent.prompt.text(), "h");
    }
    #[test]
    fn arrows_move_prompt_cursor_when_prompt_focused() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.prompt.set_text("line one\nline two");
        agent.prompt.set_cursor(0);
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.btw_focused = false;
        let out = agent.handle_input(&key(KeyCode::Down), &reg);
        assert!(matches!(out, InputOutcome::Changed));
        assert!(
            agent.prompt.cursor() > 0,
            "Down should move the prompt cursor to the next line"
        );
        assert_eq!(
            done_scroll_offset(&agent),
            0,
            "the /btw panel must not scroll while the prompt is focused"
        );
    }
    #[test]
    fn nonscrollable_answer_never_captures_arrows() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.prompt.set_text("hello");
        agent.prompt.set_cursor(0);
        agent.btw_state = Some(BtwOverlayState::done("q".into(), "short".into()));
        agent.btw_focused = true;
        agent.handle_input(&key(KeyCode::Down), &reg);
        assert!(
            !agent.btw_focused,
            "a non-scrollable panel hands the arrows back to the prompt"
        );
        assert_eq!(done_scroll_offset(&agent), 0);
    }
    #[test]
    fn scrollback_pane_does_not_capture_arrows() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.set_active_pane(AgentPane::Scrollback, true);
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.btw_focused = true;
        agent.handle_input(&key(KeyCode::Down), &reg);
        assert_eq!(
            done_scroll_offset(&agent),
            0,
            "the /btw panel must not scroll while the scrollback pane is focused"
        );
    }
    #[test]
    fn esc_dismisses_panel_and_clears_focus() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.btw_focused = true;
        agent.handle_input(&key(KeyCode::Esc), &reg);
        assert!(agent.btw_state.is_none(), "Esc dismisses the /btw panel");
        assert!(!agent.btw_focused, "dismissing the panel clears its focus");
    }
    #[test]
    fn minimal_permission_owns_esc_over_hidden_btw() {
        let mut agent = minimal_btw_agent();
        let reg = ActionRegistry::defaults();
        agent
            .permission_queue
            .push_back(super::test_fixtures::make_followup_permission_state());
        agent.handle_minimal_input(&key(KeyCode::Esc), &reg);
        assert_minimal_btw_active(&agent, "permission");
        assert_eq!(
            agent.permission_queue.len(),
            1,
            "Esc preserves the pending permission"
        );
        assert_eq!(
            agent
                .permission_queue
                .front()
                .map(|permission| permission.focus),
            Some(crate::views::permission_view::PermissionFocus::Options),
            "permission handled Esc by returning focus to options"
        );
    }
    #[test]
    fn minimal_modal_and_viewers_own_esc_over_hidden_btw() {
        let reg = ActionRegistry::defaults();
        let mut agents = minimal_btw_agent();
        agents.agents_modal = Some(crate::views::agents_modal::AgentsModalState::new(
            std::path::Path::new("/nonexistent"),
            &std::collections::HashMap::new(),
            &crate::app::bundle::BundleState::default(),
            None,
            None,
            None,
        ));
        agents.handle_minimal_input(&key(KeyCode::Esc), &reg);
        assert!(agents.agents_modal.is_none(), "agents modal handled Esc");
        assert_minimal_btw_active(&agents, "agents modal");
        let mut block = minimal_btw_agent();
        block.block_viewer = Some(crate::views::block_viewer::BlockViewerPane::for_plain_text(
            "t", "content",
        ));
        block.handle_minimal_input(&key(KeyCode::Esc), &reg);
        assert!(block.block_viewer.is_none(), "block viewer handled Esc");
        assert_minimal_btw_active(&block, "block viewer");
        let mut video = minimal_btw_agent();
        video.video_viewer = Some(crate::prompt_images::VideoViewerState::test_stub());
        video.handle_minimal_input(&key(KeyCode::Esc), &reg);
        assert!(video.video_viewer.is_none(), "video viewer handled Esc");
        assert_minimal_btw_active(&video, "video viewer");
        let mut goal = minimal_btw_agent();
        goal.goal_state = Some(crate::app::agent::GoalDisplayState::test_stub());
        goal.show_goal_detail = true;
        goal.handle_minimal_input(&key(KeyCode::Esc), &reg);
        assert!(!goal.show_goal_detail, "goal detail handled Esc");
        assert_minimal_btw_active(&goal, "goal detail");
    }
    #[test]
    fn minimal_btw_surface_owner_covers_shared_modal_cascade() {
        let mut agent = minimal_btw_agent();
        assert!(crate::minimal_api::minimal_btw_surface_available(&agent));
        agent.image_viewer = Some(
            crate::prompt_images::ImageViewerState::open_from_path_deferred(std::path::Path::new(
                "x.png",
            )),
        );
        assert!(!crate::minimal_api::minimal_btw_surface_available(&agent));
        agent.image_viewer = None;
        agent.gboom = Some(crate::gboom::GboomState::new());
        assert!(!crate::minimal_api::minimal_btw_surface_available(&agent));
        agent.gboom = None;
        agent.block_viewer = Some(crate::views::block_viewer::BlockViewerPane::for_plain_text(
            "t", "content",
        ));
        assert!(!crate::minimal_api::minimal_btw_surface_available(&agent));
    }
    #[test]
    fn fullscreen_keeps_btw_first_esc_precedence() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent
            .permission_queue
            .push_back(super::test_fixtures::make_followup_permission_state());
        agent.handle_input(&key(KeyCode::Esc), &reg);
        assert!(agent.btw_state.is_none());
        assert!(!agent.permission_queue.is_empty());
    }
    #[test]
    fn minimal_does_not_scroll_unpainted_btw_geometry() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.btw_focused = true;
        agent.last_btw_area = Rect::default();
        agent.handle_minimal_input(&key(KeyCode::Down), &reg);
        assert_eq!(done_scroll_offset(&agent), 0);
    }
    /// A hidden `/jump` picker shadowed by the `/btw` panel must not let one Esc
    /// close both: the first Esc drops the shadowed picker (and is spent there),
    /// the panel survives, and only a second Esc dismisses it.
    #[test]
    fn esc_over_shadowed_jump_picker_spares_btw_panel() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.jump_state = Some(JumpState {
            entries: Vec::new(),
            selected: 0,
            restore: JumpRestore {
                bookmark: None,
                selected: None,
                follow_mode: false,
            },
        });
        agent.handle_input(&key(KeyCode::Esc), &reg);
        assert!(
            agent.jump_state.is_none(),
            "first Esc drops the shadowed picker"
        );
        assert!(
            agent.btw_state.is_some(),
            "the /btw panel survives the picker-dismissing Esc"
        );
        agent.handle_input(&key(KeyCode::Esc), &reg);
        assert!(
            agent.btw_state.is_none(),
            "a second Esc dismisses the /btw panel"
        );
    }
    #[test]
    fn clicking_panel_refocuses_it() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.btw_focused = false;
        agent.set_active_pane(AgentPane::Scrollback, true);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        agent.handle_mouse(&click);
        assert!(agent.btw_focused, "clicking the panel refocuses it");
        assert_eq!(agent.active_pane, AgentPane::Prompt);
        agent.handle_input(&key(KeyCode::Down), &reg);
        assert_eq!(done_scroll_offset(&agent), 1);
    }
    #[test]
    fn pasting_into_prompt_returns_focus() {
        let mut agent = prompt_focused_agent();
        let reg = ActionRegistry::defaults();
        agent.btw_state = Some(BtwOverlayState::done("q".into(), long_btw_answer()));
        agent.btw_focused = true;
        agent.handle_input(&Event::Paste("a".repeat(5000)), &reg);
        assert!(!agent.btw_focused, "pasting into the prompt returns focus");
    }
}
#[cfg(test)]
mod focus_gained_restore_tests {
    use super::paste_key_tests::make_question_view_state_in_input_mode;
    use super::test_fixtures::{
        make_agent, make_followup_permission_state, make_plan_approval_view_state,
    };
    use super::{AgentPane, AgentView};
    use crate::app::agent::AgentState;
    use crate::views::modal::{ActiveModal, CancelTurnViewState};
    fn scrollback_agent() -> AgentView {
        let mut agent = make_agent();
        agent.active_pane = AgentPane::Scrollback;
        agent
    }
    fn with_permission(agent: &mut AgentView) {
        agent
            .permission_queue
            .push_back(make_followup_permission_state());
    }
    #[test]
    fn should_restore_prompt_on_focus_gained_permission_vim_turn_running() {
        let mut agent = scrollback_agent();
        agent.vim_mode = true;
        agent.session.state = AgentState::TurnRunning;
        with_permission(&mut agent);
        assert!(agent.should_restore_prompt_on_focus_gained());
    }
    #[test]
    fn should_restore_prompt_on_focus_gained_permission_non_vim_turn_running() {
        let mut agent = scrollback_agent();
        agent.vim_mode = false;
        agent.session.state = AgentState::TurnRunning;
        with_permission(&mut agent);
        assert!(agent.should_restore_prompt_on_focus_gained());
    }
    #[test]
    fn should_restore_prompt_on_focus_gained_idle_non_vim_no_overlay() {
        let mut agent = scrollback_agent();
        agent.vim_mode = false;
        agent.session.state = AgentState::Idle;
        assert!(agent.should_restore_prompt_on_focus_gained());
    }
    #[test]
    fn should_restore_prompt_on_focus_gained_idle_vim_no_overlay() {
        let mut agent = scrollback_agent();
        agent.vim_mode = true;
        agent.session.state = AgentState::Idle;
        assert!(!agent.should_restore_prompt_on_focus_gained());
    }
    #[test]
    fn should_restore_prompt_on_focus_gained_busy_non_vim_no_overlay() {
        let mut agent = scrollback_agent();
        agent.vim_mode = false;
        agent.session.state = AgentState::TurnRunning;
        assert!(!agent.should_restore_prompt_on_focus_gained());
    }
    #[test]
    fn should_restore_prompt_on_focus_gained_permission_already_prompt() {
        let mut agent = make_agent();
        agent.active_pane = AgentPane::Prompt;
        agent.vim_mode = true;
        agent.session.state = AgentState::TurnRunning;
        with_permission(&mut agent);
        assert!(!agent.should_restore_prompt_on_focus_gained());
    }
    #[test]
    fn should_restore_prompt_on_focus_gained_permission_with_modal() {
        let mut agent = scrollback_agent();
        agent.vim_mode = true;
        agent.session.state = AgentState::TurnRunning;
        with_permission(&mut agent);
        agent.active_modal = Some(ActiveModal::CommandPalette {
            entries: crate::views::modal::default_palette_entries(
                false,
                &agent.prompt.slash_controller,
            ),
            state: crate::views::picker::PickerState::input_active(),
            window: crate::views::modal_window::ModalWindowState::new(),
        });
        assert!(!agent.should_restore_prompt_on_focus_gained());
    }
    #[test]
    fn should_restore_prompt_on_focus_gained_plan_approval_vim() {
        let mut agent = scrollback_agent();
        agent.vim_mode = true;
        agent.session.state = AgentState::TurnRunning;
        agent.plan_approval_view = Some(make_plan_approval_view_state());
        assert!(agent.should_restore_prompt_on_focus_gained());
    }
    #[test]
    fn should_restore_prompt_on_focus_gained_question_vim() {
        let mut agent = scrollback_agent();
        agent.vim_mode = true;
        agent.session.state = AgentState::TurnRunning;
        agent.question_view = Some(make_question_view_state_in_input_mode());
        assert!(agent.should_restore_prompt_on_focus_gained());
    }
    #[test]
    fn should_restore_prompt_on_focus_gained_cancel_turn_vim() {
        let mut agent = scrollback_agent();
        agent.vim_mode = true;
        agent.session.state = AgentState::TurnRunning;
        agent.cancel_turn_view = Some(CancelTurnViewState {
            active_idx: 0,
            running_count: 1,
        });
        assert!(agent.should_restore_prompt_on_focus_gained());
    }
}
#[cfg(test)]
mod esc_would_cancel_turn_tests {
    use super::test_fixtures::make_agent;
    use super::{AgentPane, AgentView};
    use crate::app::agent::AgentState;
    /// Running-turn agent on the prompt pane with no Esc consumers layered.
    fn running_agent(vim_mode: bool) -> AgentView {
        let mut agent = make_agent();
        agent.session.state = AgentState::TurnRunning;
        agent.active_pane = AgentPane::Prompt;
        agent.vim_mode = vim_mode;
        agent
    }
    #[test]
    fn gate_non_vim_true_vim_false_minimal_overrides_vim() {
        assert!(running_agent(false).esc_would_cancel_turn(false));
        assert!(!running_agent(true).esc_would_cancel_turn(false));
        let mut agent = running_agent(true);
        agent
            .prompt
            .set_screen_mode(crate::app::ScreenMode::Minimal);
        assert!(agent.esc_would_cancel_turn(false));
    }
    #[test]
    fn app_level_esc_owner_suppresses_esc_hint() {
        assert!(!running_agent(false).esc_would_cancel_turn(true));
    }
    #[test]
    fn queued_edit_and_inline_edit_steal_esc() {
        let mut agent = running_agent(false);
        agent.prompt_mode = crate::app::queue_edit::PromptMode::EditingQueued {
            id: 1,
            original: "queued row".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        assert!(
            !agent.esc_would_cancel_turn(false),
            "queued-prompt editing owns Esc (discard edit), not cancel"
        );
        let mut agent = running_agent(false);
        agent.inline_edit = Some(crate::app::inline_edit::InlineEditState {
            entry_id: crate::scrollback::entry::EntryId::new(1),
            prompt_index: 0,
            original: "sent".into(),
            textarea: echo_agent_ratatui_textarea::TextArea::new(),
            textarea_state: echo_agent_ratatui_textarea::TextAreaState::default(),
            last_text_area: None,
            last_rect: None,
        });
        assert!(
            !agent.esc_would_cancel_turn(false),
            "an open inline prompt edit owns Esc (dismiss), not cancel"
        );
    }
    #[test]
    fn subagent_fullscreen_view_owns_esc() {
        let mut agent = running_agent(false);
        agent.is_subagent_view = true;
        agent.active_pane = AgentPane::Scrollback;
        assert!(
            !agent.esc_would_cancel_turn(false),
            "Esc in a fullscreen subagent view closes the child, not cancel"
        );
    }
    #[test]
    fn agents_and_persona_modals_steal_esc() {
        let mut agent = running_agent(false);
        agent.agents_modal = Some(crate::views::agents_modal::AgentsModalState::new(
            std::path::Path::new("/nonexistent"),
            &std::collections::HashMap::new(),
            &crate::app::bundle::BundleState::default(),
            None,
            None,
            None,
        ));
        assert!(
            !agent.esc_would_cancel_turn(false),
            "an open agents modal owns Esc (close), not cancel"
        );
        let mut agent = running_agent(false);
        agent.persona_detail =
            Some(crate::views::persona_detail::PersonaDetailState::from_name_only("researcher"));
        assert!(
            !agent.esc_would_cancel_turn(false),
            "an open persona detail owns Esc (back/close), not cancel"
        );
    }
    #[test]
    fn bare_scrollback_true_but_open_search_steals_esc() {
        let mut agent = running_agent(false);
        agent.active_pane = AgentPane::Scrollback;
        assert!(agent.esc_would_cancel_turn(false), "bare scrollback");
        agent.scrollback_search = Some(crate::scrollback::search::ScrollbackSearchState::open());
        assert!(
            !agent.esc_would_cancel_turn(false),
            "an open scrollback search dismisses Esc, so the hint must not claim it cancels"
        );
    }
    #[test]
    fn open_slash_dropdown_steals_esc() {
        let mut agent = running_agent(false);
        agent.prompt.set_text("/he");
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(
            agent.prompt.slash_open(),
            "precondition: slash dropdown open"
        );
        assert!(
            !agent.esc_would_cancel_turn(false),
            "an open slash dropdown dismisses Esc, so the hint must not claim it cancels"
        );
    }
    #[test]
    fn latent_composer_mode_and_other_panes_keep_ctrl_c() {
        let mut agent = running_agent(false);
        agent.prompt_input_mode = super::PromptInputMode::Bash;
        assert!(
            !agent.esc_would_cancel_turn(false),
            "a latent bash composer owns the empty-prompt Esc as its mode-exit"
        );
        let mut agent = running_agent(false);
        agent.active_pane = AgentPane::Queue;
        assert!(
            !agent.esc_would_cancel_turn(false),
            "panes that never reach the Esc policy must not advertise Esc"
        );
    }
}
#[cfg(test)]
mod jump_backout_key_tests {
    use super::test_fixtures::make_agent;
    use super::{AgentPane, AgentView};
    use crate::actions::ActionRegistry;
    use crate::app::actions::Action;
    use crate::app::agent::{AgentCommand, AgentState};
    use crate::app::app_view::InputOutcome;
    use crate::views::jump::{JumpRestore, JumpState};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    fn open_jump(agent: &mut AgentView) {
        agent.jump_state = Some(JumpState {
            entries: Vec::new(),
            selected: 0,
            restore: JumpRestore {
                bookmark: None,
                selected: None,
                follow_mode: false,
            },
        });
    }
    fn ctrl_c() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
    }
    /// In the dashboard overlay, a bare Esc backs out via
    /// `no_esc_consumer_pending`; the open `/jump` picker must count as a
    /// consumer so Esc dismisses it (restoring the viewport) instead of
    /// exiting the overlay and leaving the picker latent.
    #[test]
    fn jump_picker_is_an_esc_consumer() {
        let mut agent = make_agent();
        assert!(
            agent.no_esc_consumer_pending(),
            "baseline: no Esc consumer pending"
        );
        open_jump(&mut agent);
        assert!(
            !agent.no_esc_consumer_pending(),
            "an open /jump picker consumes Esc"
        );
    }
    /// The Left-arrow mirror: an open picker fails `is_empty_focused_prompt`
    /// so the overlay Left back-out defers to the picker's own handling.
    #[test]
    fn jump_picker_defeats_empty_focused_prompt() {
        let mut agent = make_agent();
        agent.set_active_pane(AgentPane::Prompt, true);
        assert!(
            agent.is_empty_focused_prompt(),
            "baseline: empty prompt focused"
        );
        open_jump(&mut agent);
        assert!(
            !agent.is_empty_focused_prompt(),
            "an open /jump picker owns Esc/Left in the overlay back-out"
        );
    }
    /// `/jump` must not swallow Ctrl+C while `/compact` is running — same
    /// hatch as a running turn.
    #[test]
    fn jump_picker_ctrl_c_cancels_compact() {
        let mut agent = make_agent();
        agent.session.state = AgentState::CommandRunning {
            command: AgentCommand::Compact,
            started_at: std::time::Instant::now(),
        };
        open_jump(&mut agent);
        let outcome = agent.handle_input(&ctrl_c(), &ActionRegistry::defaults());
        assert!(
            agent.jump_state.is_none(),
            "Ctrl+C during /compact must dismiss the jump picker"
        );
        assert!(
            matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
            "Ctrl+C during /compact with /jump open must cancel, got {outcome:?}"
        );
    }
    /// Once a wake cancel is in flight, Ctrl+C must reach the same quit
    /// escalation as a stuck normal cancel instead of re-sending forever.
    #[test]
    fn ctrl_c_escalates_to_quit_while_wake_cancel_is_stuck() {
        let mut agent = make_agent();
        agent.running_wake_turn = Some(crate::app::agent_view::RunningWakeTurn {
            prompt_id: "task-completed-bg1".into(),
            cancel_sent: false,
        });
        let outcome = agent.handle_input(&ctrl_c(), &ActionRegistry::defaults());
        assert!(
            matches!(outcome, InputOutcome::Action(Action::CancelTurn)),
            "the first Ctrl+C cancels the wake turn, got {outcome:?}"
        );
        agent.running_wake_turn.as_mut().unwrap().cancel_sent = true;
        let outcome = agent.handle_input(&ctrl_c(), &ActionRegistry::defaults());
        assert!(
            matches!(outcome, InputOutcome::Action(Action::Quit)),
            "Ctrl+C during a stuck wake cancel must escalate to quit, got {outcome:?}"
        );
    }
}
#[cfg(test)]
mod plan_approval_model_handoff_tests {
    use super::test_fixtures::{make_agent, make_plan_approval_view_state};
    use crate::actions::ActionRegistry;
    use crate::key;
    use crate::views::modal::ActiveModal;
    use agent_client_protocol as acp;
    use crossterm::event::Event;
    use std::sync::Arc;
    #[test]
    fn model_picker_during_plan_approval() {
        let mut agent = make_agent();
        let id = acp::ModelId::new(Arc::from("test-model"));
        agent.session.models.available.insert(
            id.clone(),
            acp::ModelInfo::new(id, "Test Model".to_string()),
        );
        agent.plan_approval_view = Some(make_plan_approval_view_state());
        agent.reopen_plan_approval();
        let reg = ActionRegistry::defaults();
        agent.handle_input(&Event::Key(key!('m', CONTROL).to_key_event()), &reg);
        assert!(matches!(
            agent.active_modal,
            Some(ActiveModal::ArgPicker { ref command, .. }) if command == "model"
        ));
        agent.handle_input(&Event::Key(key!(Esc).to_key_event()), &reg);
        assert!(agent.active_modal.is_none());
        assert!(agent.plan_approval_view.is_some());
    }
}
#[cfg(test)]
mod voice_stop_click_during_plan_review_tests {
    use super::test_fixtures::{make_agent, make_plan_approval_view_state};
    use crate::actions::ActionRegistry;
    use crate::app::actions::Action;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    fn stop_click(col: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }
    /// Recording-row [stop] click keeps working while the plan approval's
    /// line-viewer overlay owns mouse routing — the row stays visible (the
    /// overlay excludes it), so the viewer must not swallow the click.
    #[test]
    fn stop_click_dispatches_voice_toggle_under_plan_approval_viewer() {
        let mut agent = make_agent();
        agent.plan_approval_view = Some(make_plan_approval_view_state());
        agent.reopen_plan_approval();
        assert!(agent.line_viewer.is_some(), "approval must open the viewer");
        agent.hit_voice_stop_button.rect = Some(Rect::new(90, 30, 6, 1));
        let outcome = agent.handle_input(&stop_click(91, 30), &ActionRegistry::defaults());
        assert!(
            matches!(outcome, InputOutcome::Action(Action::VoiceToggle)),
            "[stop] click under the plan viewer must dispatch VoiceToggle, got {outcome:?}"
        );
    }
    /// Same intercept on the approval's feedback surface (viewer closed,
    /// prompt pane focused).
    #[test]
    fn stop_click_dispatches_voice_toggle_in_plan_feedback() {
        let mut agent = make_agent();
        agent.plan_approval_view = Some(make_plan_approval_view_state());
        assert!(agent.line_viewer.is_none());
        agent.set_active_pane(super::AgentPane::Prompt, false);
        agent.hit_voice_stop_button.rect = Some(Rect::new(90, 30, 6, 1));
        let outcome = agent.handle_input(&stop_click(91, 30), &ActionRegistry::defaults());
        assert!(
            matches!(outcome, InputOutcome::Action(Action::VoiceToggle)),
            "[stop] click during plan feedback must dispatch VoiceToggle, got {outcome:?}"
        );
    }
}
#[cfg(test)]
mod rich_textarea_paste_routing_tests {
    use super::test_fixtures::make_agent;
    use crate::actions::ActionRegistry;
    use crate::app::inline_edit::InlineEditState;
    use crate::scrollback::entry::EntryId;
    use crossterm::event::Event;
    use echo_agent_ratatui_textarea::{TextArea, TextAreaState};
    #[test]
    fn inline_edit_receives_raw_multiline_paste_without_touching_prompt() {
        let mut agent = make_agent();
        agent.prompt.set_text("hidden prompt");
        let mut textarea = TextArea::new();
        textarea.set_text("ab");
        textarea.set_cursor(1);
        agent.inline_edit = Some(InlineEditState {
            entry_id: EntryId::new(1),
            prompt_index: 0,
            original: "ab".to_owned(),
            textarea,
            textarea_state: TextAreaState::default(),
            last_text_area: None,
            last_rect: None,
        });
        let _ = agent.handle_input(
            &Event::Paste("中\nline".to_owned()),
            &ActionRegistry::defaults(),
        );
        assert_eq!(
            agent.inline_edit.as_ref().map(|edit| edit.textarea.text()),
            Some("a中\nlineb")
        );
        assert_eq!(agent.prompt.text(), "hidden prompt");
    }
}
/// Pasting while the scrollback pane holds the keyboard (prompt unfocused) must land in
/// the composer, mirroring how a typed character focus-forwards into the prompt.
#[cfg(test)]
mod scrollback_paste_focus_forward_tests {
    use super::test_fixtures::{make_agent, make_followup_permission_state};
    use super::{AgentPane, AgentView};
    use crate::actions::ActionRegistry;
    use crate::app::actions::Action;
    use crate::app::app_view::InputOutcome;
    use crossterm::event::Event;
    fn scrollback_agent() -> (AgentView, ActionRegistry) {
        let mut agent = make_agent();
        agent.vim_mode = false;
        agent.set_active_pane(AgentPane::Scrollback, true);
        (agent, ActionRegistry::defaults())
    }
    /// The `ActionThenForward` round-trip the event loop performs: dispatch `FocusPrompt`
    /// to focus the prompt pane, then re-process the same paste through it so the text lands.
    #[test]
    fn paste_from_scrollback_round_trip_lands_in_composer() {
        let (mut agent, reg) = scrollback_agent();
        let paste = Event::Paste("pasted text".to_owned());
        assert!(matches!(
            agent.handle_input(&paste, &reg),
            InputOutcome::ActionThenForward(Action::FocusPrompt)
        ));
        agent.set_active_pane(AgentPane::Prompt, false);
        let out = agent.handle_input(&paste, &reg);
        assert!(matches!(out, InputOutcome::Changed));
        assert_eq!(agent.prompt.text(), "pasted text");
    }
    /// A parked blocking card stays parked: `FocusPrompt` would unpark it and the
    /// overlay would swallow the re-dispatched paste, so a paste here is inert.
    #[test]
    fn paste_from_scrollback_does_not_unpark_a_pending_overlay() {
        let (mut agent, reg) = scrollback_agent();
        agent
            .permission_queue
            .push_back(make_followup_permission_state());
        assert!(agent.parked_card().is_some(), "card should be parked");
        assert!(agent.focused_card().is_none());
        let out = agent.handle_input(&Event::Paste("hello".to_owned()), &reg);
        assert!(
            matches!(out, InputOutcome::Unchanged),
            "paste must not unpark a pending overlay, got {out:?}"
        );
        assert!(agent.parked_card().is_some(), "card must stay parked");
        assert_eq!(agent.active_pane, AgentPane::Scrollback);
    }
    fn make_test_png(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(width, height, Rgba([128, 64, 32, 255]));
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }
    /// A dragged image arrives as a `file://` bracketed paste; from a focused
    /// scrollback it takes the same focus-forward round trip as a text paste.
    #[test]
    fn dragging_image_while_scrollback_focused_attaches_to_composer() {
        let (mut agent, reg) = scrollback_agent();
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("drag.png");
        std::fs::write(&png, make_test_png(8, 8)).unwrap();
        let drop = Event::Paste(format!("file://{}", png.display()));
        assert!(matches!(
            agent.handle_input(&drop, &reg),
            InputOutcome::ActionThenForward(Action::FocusPrompt)
        ));
        agent.set_active_pane(AgentPane::Prompt, false);
        let out = agent.handle_input(&drop, &reg);
        assert!(matches!(out, InputOutcome::Changed));
        assert_eq!(agent.prompt.images.len(), 1);
        assert!(agent.prompt.text().contains("[Image #1]"));
    }
}
