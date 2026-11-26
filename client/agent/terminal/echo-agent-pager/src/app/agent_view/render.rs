//! Frame rendering for [`AgentView`]: the `draw` entry point plus shortcut
//! hints and the subagent fullscreen view.
use super::{
    ActivePane, AgentPane, AgentView, AgentViewLayout, BlockingCard, CtaPhase, EscStep,
    InlineMediaHitAreas, KeyOwner, MODE_BANNER_FADE_TICKS, PromptMode, collect_citation_links,
    dropdown_content_inset, dropdown_items_width, record_dot_pulse, render_dropdown_chrome,
    supports_osc22,
};
use crate::actions::{ActionId, ActionRegistry};
use crate::key;
use crate::render::SafeBuf;
use crate::render::line_utils::truncate_line;
use crate::scrollback::block::BlockContent;
use crate::scrollback::layout::HorizontalLayout;
use crate::scrollback::render::ScratchBuffer;
use crate::scrollback::text_selection::{
    ResolvedSelectionBoundaries, ResolvedSelectionModel, render_active_selection_overlay,
    render_block_drag_overlay, render_persistent_selection_overlay,
};
use crate::theme::Theme;
use crate::views::agent::AgentViewLayoutParams;
use crate::views::btw_overlay::BTW_OVERLAY_ENTRY_IDX;
use crate::views::modal;
use crate::views::plan_approval_view::PlanApprovalFocus;
use crate::views::prompt_widget::{PromptBg, PromptFlag, PromptInfo, PromptStyle};
use crate::views::question_view::{QUESTION_VIEW_HPAD, feedback_input};
use crate::views::shortcuts_bar::{HintItem, PendingHint, ShortcutsBar};
use crate::views::{agent, turn_status};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use std::collections::HashSet;
use std::time::Instant;
/// AppView-owned per-frame inputs to [`AgentView::draw`]: state the agent
/// view cannot see itself (the voice pipeline, app-level Esc ownership, the
/// status row).
/// Grouped (mirroring `WelcomeRenderParams`) so the next app-level render
/// fact extends this struct instead of every `draw` call site; tests take
/// `Default` and override only what they exercise.
#[derive(Default)]
pub struct AppRenderParams<'a> {
    /// Voice feature available (shows the mic affordances).
    pub voice_available: bool,
    /// Mic open and streaming on the active surface — drives the recording
    /// row and the prompt voice overlay.
    pub voice_listening: bool,
    /// Interim transcript for the prompt overlay while dictating.
    pub voice_interim: Option<&'a str>,
    /// App-level Esc ownership snapshot — single producer
    /// `AppView::esc_owned_before_agent` (voice listening / cold-start,
    /// focused dev tracing pane, cloud / import-Claude modals, dashboard
    /// attached-agent popup). Feeds the hint path so the bar never
    /// advertises `Esc cancel` while an app-level owner would consume it.
    pub esc_owned_before_agent: bool,
    /// The status row this frame paints, or `Off` when this frame has none.
    pub status_line: crate::views::status_line::StatusLineFrame,
}
/// What the bottom shortcuts bar renders this frame.
enum ShortcutsBarContent {
    /// A blocking surface's own keys, rendered as given.
    Surface(Vec<HintItem>),
    /// The focused pane's keys, trimmed to the compact bar with the
    /// cheatsheet hint appended.
    Pane(Vec<HintItem>),
    /// Nothing: the row belongs to a surface that paints it itself.
    Hidden,
}
impl AgentView {
    pub(crate) fn update_scrollback_selection_state(
        &mut self,
        model: ResolvedSelectionModel,
        boundaries: ResolvedSelectionBoundaries,
    ) {
        self.last_scrollback_selection_model = model;
        self.last_scrollback_selection_boundaries = boundaries;
    }
    fn clear_scrollback_selection_state(&mut self) {
        self.update_scrollback_selection_state(Default::default(), Default::default());
    }
    /// Keep [`Self::timeline_hover_preview`] in sync with [`Self::timeline_hover`].
    /// Called when hover changes (mouse Moved or rail rebuild under a
    /// stationary pointer) so render can borrow the cached text.
    pub(crate) fn sync_timeline_hover_preview(&mut self) {
        match self.timeline_hover {
            Some(crate::views::timeline::TimelineHit::Tick(turn_idx)) => {
                if self
                    .timeline_hover_preview
                    .as_ref()
                    .is_some_and(|(idx, _)| *idx == turn_idx)
                {
                    return;
                }
                self.timeline_hover_preview = self
                    .scrollback
                    .turn_preview(turn_idx)
                    .map(|text| (turn_idx, text));
            }
            _ => self.timeline_hover_preview = None,
        }
    }
    /// Open the fullscreen subagent view for `child_sid`, replaying child
    /// `updates.jsonl` when scrollback only has the injected task prompt.
    pub(crate) fn open_subagent_fullscreen(&mut self, child_sid: String) {
        if let Some(child) = self.subagent_views.get_mut(&child_sid) {
            child.mark_as_subagent_view();
        } else {
            return;
        }
        if self.active_subagent.as_deref() != Some(child_sid.as_str()) {
            self.close_subagent_fullscreen();
        }
        let replay_outcome = crate::app::subagent::ensure_subagent_child_replayed(self, &child_sid);
        tracing::debug!(child_sid = %child_sid, ?replay_outcome, "opened subagent fullscreen");
        self.active_subagent = Some(child_sid);
    }
    /// Close the fullscreen subagent takeover (if any), evicting the closed
    /// child when finished (see [`crate::app::subagent::evict_finished_child_view`]
    /// for rationale and guards). All close sites route through here.
    pub(crate) fn close_subagent_fullscreen(&mut self) {
        if let Some(child_sid) = self.active_subagent.take() {
            let _ = crate::app::subagent::evict_finished_child_view(self, &child_sid);
        }
    }
    /// Fetch a child view for applying a live update, hydrating a resumed
    /// child's inherited transcript first so the incoming block never closes
    /// the replay window (see
    /// [`crate::app::subagent::replay_resumed_child_before_live_block`]). The
    /// funnel for every apply that can be a resumed child's *first* live block:
    /// the ACP and EchoAgent child ingresses and the finish-path finalize. Reads and
    /// precedence-exempt writers (background task blocks, tool-call argument
    /// deltas, each transitively preceded by a funneled block) use the field
    /// directly.
    pub(crate) fn child_view_for_live_update_mut(
        &mut self,
        child_sid: &str,
    ) -> Option<&mut AgentView> {
        crate::app::subagent::replay_resumed_child_before_live_block(self, child_sid);
        self.subagent_views.get_mut(child_sid).map(|v| &mut **v)
    }
    /// Shortcut hints for the plan-approval prompt/comment focus states.
    ///
    /// Shared by `draw` and the cheatsheet Current section. `Tab:plan` is
    /// omitted when there is nothing to open (empty approval and no line
    /// viewer) so the footer never advertises a dead key.
    fn plan_approval_shortcut_hints(
        &self,
        pav: &crate::views::plan_approval_view::PlanApprovalViewState,
    ) -> Vec<HintItem> {
        match pav.focus {
            PlanApprovalFocus::Commenting => {
                vec![
                    HintItem::new(key!(Enter), "save comment"),
                    HintItem::new(key!(Esc), "cancel"),
                ]
            }
            PlanApprovalFocus::Prompt => {
                let has_content = !pav.comments.is_empty() || !self.prompt.text().trim().is_empty();
                if has_content {
                    vec![
                        HintItem::new(key!(Enter), "request changes"),
                        HintItem::new(key!(Tab), "plan"),
                        HintItem::new(key!(Esc), "back"),
                    ]
                } else {
                    vec![
                        HintItem::new(key!('a'), "approve"),
                        HintItem::new(key!(Tab), "plan"),
                        HintItem::new(key!(Esc), "back"),
                    ]
                }
            }
            PlanApprovalFocus::Preview => {
                vec![
                    HintItem::new(key!('y'), "copy plan"),
                    HintItem::new(key!(Tab), "prompt"),
                ]
            }
        }
    }
    /// Rows to give the bare `/feedback` report box: report-sized where the terminal allows it, never past `cap`.
    /// Its rules plus one text row is the floor, so a squeezed box still shows what the user is typing.
    fn feedback_editor_h(&self, inner_width: u16, cap: u16, theme: &Theme) -> u16 {
        let floor = feedback_input::MIN_HEIGHT;
        let rows = feedback_input::HEIGHT.clamp(floor, cap.max(floor));
        self.prompt
            .desired_height(
                feedback_input::width(inner_width),
                &feedback_input::style(theme),
                true,
                cap.max(rows),
            )
            .max(rows)
    }
    /// The one-line freeform answer row, growing with what the user types.
    fn freeform_editor_h(&self, inner_width: u16, cap: u16, style: &PromptStyle) -> u16 {
        let question_text_w = crate::views::question_view::inline_text_width(inner_width);
        self.prompt
            .desired_height(question_text_w, style, false, cap)
    }
    /// The `Esc` hint for the focused card, named by the rung the key actually
    /// takes ([`EscStep`]).
    fn card_esc_hint(&self) -> HintItem {
        HintItem::new(key!(Esc), self.card_esc().map_or("back", EscStep::label))
    }
    /// Shortcut hints for an open `ask_user_question` card.
    fn question_shortcut_hints(
        &self,
        qv: &crate::views::question_view::QuestionViewState,
    ) -> Vec<HintItem> {
        use crate::views::question_view::QuestionFocus;
        let esc = self.card_esc_hint();
        match qv.focus {
            QuestionFocus::InputMode if self.prompt.file_search_visible() => {
                vec![
                    HintItem::paired(key!(Up), key!(Down), "nav"),
                    HintItem::new(key!(Tab), "accept"),
                    HintItem::new(key!(Right), "drill"),
                    esc,
                ]
            }
            QuestionFocus::InputMode if qv.is_feedback_report() => {
                vec![HintItem::new(key!(Enter), "send"), esc]
            }
            QuestionFocus::InputMode => vec![HintItem::new(key!(Enter), "submit"), esc],
            QuestionFocus::Navigation if qv.is_feedback_trace() => vec![esc],
            QuestionFocus::Navigation => {
                vec![
                    HintItem::new(key!(Tab), "next answer"),
                    esc,
                    HintItem::new(key!('X'), "dismiss"),
                ]
            }
        }
    }
    fn permission_shortcut_hints(
        &self,
        perm: &crate::views::permission_view::PermissionViewState,
    ) -> Vec<HintItem> {
        use crate::views::permission_view::PermissionFocus;
        let perm_content_w = self
            .pane_areas
            .prompt
            .width
            .saturating_sub(QUESTION_VIEW_HPAD) as usize;
        let ctrl_f_hint = perm.has_collapsible_display(perm_content_w).then(|| {
            let label = if perm.args_expanded {
                "collapse"
            } else {
                "expand"
            };
            HintItem::new(key!('f', CONTROL), label)
        });
        match perm.focus {
            PermissionFocus::FollowupInput => {
                let mut hints = vec![HintItem::new(key!(Enter), "send")];
                hints.extend(ctrl_f_hint);
                hints.push(self.card_esc_hint());
                hints
            }
            PermissionFocus::PatternEdit => {
                let mut hints = vec![HintItem::new(key!(Enter), "save")];
                hints.extend(ctrl_f_hint);
                hints.push(self.card_esc_hint());
                hints
            }
            PermissionFocus::Options => {
                use crate::input::key::KeyShortcut;
                use crossterm::event::{KeyCode, KeyModifiers};
                let n = perm.options.len().min(9) as u8;
                let last_ch = char::from(b'0' + n.max(1));
                let last_key = KeyShortcut::new(KeyCode::Char(last_ch), KeyModifiers::NONE);
                let mut hints = vec![HintItem::paired(key!('1'), last_key, "select")];
                if perm.options.len() > 1 {
                    hints.push(HintItem::new(key!(Tab), "next option"));
                }
                if perm.has_adjustable_scope() {
                    hints.push(HintItem::paired(key!(Left), key!(Right), "scope"));
                }
                if perm.has_editable_bash_pattern() {
                    hints.push(HintItem::new(key!('e'), "edit pattern"));
                }
                hints.extend(ctrl_f_hint);
                hints.push(HintItem::new(key!('o', CONTROL), "always-approve"));
                hints.push(HintItem::new(key!('c', CONTROL), "cancel"));
                hints.push(self.card_esc_hint());
                hints
            }
        }
    }
    /// Returns the *exact* hints the bottom shortcuts bar would render right now.
    ///
    /// Single source of truth for context-sensitive shortcuts (pane, overlays,
    /// sub-modes, selection state, turn running, plan/queue). Both the bar
    /// renderer and the Ctrl+. cheatsheet Current section delegate here, so
    /// they are guaranteed identical and every shortcut makes sense in the
    /// active context.
    ///
    /// Known transient: when a subagent is fullscreen (`active_subagent.is_some()`),
    /// draw returns early and the child renders its own bar; Current on the parent
    /// still reflects parent context (documented limitation, pre-existing before
    /// this change).
    ///
    /// `esc_owned_before_agent`: app-level Esc ownership snapshot
    /// (`AppView::esc_owned_before_agent`); the draw path passes its param
    /// of the same name.
    pub fn current_shortcut_hints(
        &self,
        registry: &ActionRegistry,
        esc_owned_before_agent: bool,
    ) -> Vec<HintItem> {
        match self.shortcuts_bar_content(registry, esc_owned_before_agent) {
            ShortcutsBarContent::Surface(hints) | ShortcutsBarContent::Pane(hints) => hints,
            ShortcutsBarContent::Hidden => vec![],
        }
    }
    fn plan_approval_bar(
        &self,
        pav: &crate::views::plan_approval_view::PlanApprovalViewState,
    ) -> ShortcutsBarContent {
        let hints = self.plan_approval_shortcut_hints(pav);
        if hints.is_empty() {
            ShortcutsBarContent::Hidden
        } else {
            ShortcutsBarContent::Surface(hints)
        }
    }
    /// The bar names the surface the keys actually reach, so it asks
    /// [`AgentView::key_owner`] rather than keeping an order of its own.
    fn shortcuts_bar_content(
        &self,
        registry: &ActionRegistry,
        esc_owned_before_agent: bool,
    ) -> ShortcutsBarContent {
        use crate::views::shortcuts_bar::HintItem;
        match self.key_owner() {
            KeyOwner::LineViewer => self.line_viewer_bar(),
            KeyOwner::BlockViewer => ShortcutsBarContent::Surface(
                self.block_viewer
                    .as_ref()
                    .map(|viewer| viewer.shortcuts_hints())
                    .unwrap_or_default(),
            ),
            KeyOwner::Card(BlockingCard::Permission) => ShortcutsBarContent::Surface(
                self.focused_permission()
                    .map(|perm| self.permission_shortcut_hints(perm))
                    .unwrap_or_default(),
            ),
            KeyOwner::PlanApproval => self
                .plan_approval_view
                .as_ref()
                .map_or(ShortcutsBarContent::Hidden, |pav| {
                    self.plan_approval_bar(pav)
                }),
            KeyOwner::Card(BlockingCard::CancelTurn) => ShortcutsBarContent::Surface(vec![
                HintItem::paired(key!('1'), key!('4'), "select"),
                HintItem::new(key!(Tab), "next choice"),
                HintItem::new(key!(Enter), "confirm"),
                self.card_esc_hint(),
            ]),
            KeyOwner::Card(BlockingCard::Question) => ShortcutsBarContent::Surface(
                self.focused_question()
                    .map(|qv| self.question_shortcut_hints(qv))
                    .unwrap_or_default(),
            ),
            KeyOwner::Card(BlockingCard::McpElicitation) => ShortcutsBarContent::Surface(vec![
                HintItem::new(key!(Enter), "accept / toggle"),
                HintItem::new(key!('d'), "decline"),
                HintItem::new(key!('c', CONTROL), "cancel"),
                HintItem::new(key!(Tab), "next field"),
                self.card_esc_hint(),
            ]),
            KeyOwner::Pane => {
                ShortcutsBarContent::Pane(self.normal_pane_hints(registry, esc_owned_before_agent))
            }
        }
    }
    /// An open line viewer paints its own hints over this row further down
    /// `draw`, so the bar is silent — except in the two states where the
    /// viewer defers: the plan-approval prompt, whose keys the viewer's
    /// intercept forwards, and a casual comment draft.
    fn line_viewer_bar(&self) -> ShortcutsBarContent {
        use crate::views::shortcuts_bar::HintItem;
        if let Some(pav) = self
            .plan_approval_view
            .as_ref()
            .filter(|pav| pav.focus != PlanApprovalFocus::Preview)
        {
            return self.plan_approval_bar(pav);
        }
        if self.is_casual_commenting() {
            return ShortcutsBarContent::Surface(vec![
                HintItem::new(key!(Enter), "save comment"),
                HintItem::new(key!(Esc), "cancel"),
            ]);
        }
        ShortcutsBarContent::Hidden
    }
    /// Shared "normal pane" hints: flag computation + `build_hints` + queue hint.
    /// Single source of truth for the two former duplicated blocks in
    /// `current_shortcut_hints` and `draw`.
    fn normal_pane_hints(
        &self,
        registry: &ActionRegistry,
        esc_owned_before_agent: bool,
    ) -> Vec<HintItem> {
        let fold_label = self.selected_fold_label();
        let is_editing = matches!(self.prompt_mode, PromptMode::EditingQueued { .. });
        let selected_entry = self
            .scrollback
            .selected()
            .and_then(|idx| self.scrollback.entry(idx));
        let selected_is_group_header = self
            .scrollback
            .selected()
            .is_some_and(|idx| self.scrollback.entry_content_hidden_by_group(idx));
        let (selected_supports_copy, selected_meta_label, selected_supports_fullscreen) =
            if self.active_pane == ActivePane::Catalog {
                (false, None, self.catalog.selected_entry().is_some())
            } else if self.active_pane == ActivePane::Tasks {
                let has_selected = self.tasks.selected_task_id().is_some_and(|tid| {
                    self.session
                        .bg_tasks
                        .get(tid)
                        .is_some_and(|t| !t.stdout.is_empty())
                });
                let has_any_selected = self.tasks.selected_task_id().is_some();
                (has_selected, None, has_any_selected)
            } else if self.active_pane == ActivePane::Scrollback {
                let is_bg_task = selected_entry.is_some_and(|e| {
                    matches!(e.block, crate::scrollback::block::RenderBlock::BgTask(_))
                });
                if is_bg_task {
                    let has_stdout = selected_entry
                        .and_then(|e| {
                            if let crate::scrollback::block::RenderBlock::BgTask(b) = &e.block {
                                Some(&b.task_id)
                            } else {
                                None
                            }
                        })
                        .is_some_and(|tid| {
                            self.session
                                .bg_tasks
                                .get(tid)
                                .is_some_and(|t| !t.stdout.is_empty())
                        });
                    (has_stdout, None, true)
                } else {
                    let is_viewable_subagent = selected_entry.is_some_and(|e| {
                        if let crate::scrollback::block::RenderBlock::Subagent(ref sb) = e.block {
                            self.subagent_views.contains_key(&sb.child_session_id)
                        } else {
                            false
                        }
                    });
                    (
                        !selected_is_group_header
                            && selected_entry.is_some_and(|e| e.block.supports_copy()),
                        selected_entry
                            .and_then(|e| e.block.copy_meta_label())
                            .filter(|_| !selected_is_group_header),
                        selected_entry.is_some_and(|e| e.block.supports_fullscreen())
                            || is_viewable_subagent,
                    )
                }
            } else {
                (
                    !selected_is_group_header
                        && selected_entry.is_some_and(|e| e.block.supports_copy()),
                    selected_entry
                        .and_then(|e| e.block.copy_meta_label())
                        .filter(|_| !selected_is_group_header),
                    selected_entry.is_some_and(|e| e.block.supports_fullscreen()),
                )
            };
        let can_demote = !self.is_subagent_view
            && self
                .session
                .tracker
                .running_execute_tool_call_id()
                .is_some();
        let selected_can_kill = if self.active_pane == ActivePane::Catalog {
            false
        } else if self.active_pane == ActivePane::Tasks {
            self.tasks
                .selected_task_id()
                .and_then(|tid| self.session.bg_tasks.get(tid))
                .is_some_and(|t| {
                    t.status == crate::app::agent::BgTaskStatus::Running && !t.pending_kill
                })
        } else if self.active_pane == ActivePane::Scrollback {
            !selected_is_group_header
                && selected_entry
                    .and_then(|e| {
                        if let crate::scrollback::block::RenderBlock::BgTask(b) = &e.block {
                            Some(&b.task_id)
                        } else {
                            None
                        }
                    })
                    .and_then(|tid| self.session.bg_tasks.get(tid))
                    .is_some_and(|t| {
                        t.status == crate::app::agent::BgTaskStatus::Running && !t.pending_kill
                    })
        } else {
            false
        };
        let thinking_label = self.scrollback.thinking_fold_label();
        let selected_is_user_prompt = selected_entry.is_some_and(|e| e.block.is_user_prompt());
        let selected_is_agent_message = selected_entry.is_some_and(|e| e.block.is_agent_message());
        let selected_is_credit_limit = selected_entry.is_some_and(|e| e.block.is_credit_limit());
        let mut hints = agent::build_hints(
            self.active_pane,
            self.parked_card()
                .map_or_else(agent::prompt_focus_hint, BlockingCard::focus_hint),
            &self.prompt,
            registry,
            is_editing,
            fold_label,
            self.scrollback.selected_group_header_fold_label(),
            thinking_label,
            if self.active_pane == ActivePane::Tasks {
                self.tasks.show_done()
            } else {
                self.todo.show_done()
            },
            selected_supports_copy,
            selected_meta_label,
            selected_supports_fullscreen,
            can_demote,
            selected_can_kill,
            self.multiline_mode,
            self.vim_mode,
            self.is_subagent_view,
            (self.session.state.is_turn_running() || self.wake_turn_active())
                && !self.renders_parked(),
            self.esc_would_cancel_turn(esc_owned_before_agent),
            !self.visible_queue_is_empty(),
            selected_is_user_prompt,
            selected_is_agent_message,
            selected_is_credit_limit,
            crate::terminal::terminal_context().shift_enter_unavailable(),
            self.scrollback_search.as_ref(),
        );
        if (self.queue.is_visible() || !self.visible_queue_is_empty())
            && self.active_pane != ActivePane::Queue
            && !matches!(self.prompt_mode, PromptMode::EditingQueued { .. })
            && let Some(def) = registry.find(ActionId::ToggleQueue)
        {
            hints.push(def.hint());
        }
        if self.in_dashboard_overlay {
            hints.insert(
                0,
                HintItem::new(
                    registry
                        .find(ActionId::DashboardOverlayStop)
                        .map(|def| def.default_key)
                        .unwrap_or(key!('x', CONTROL)),
                    "stop",
                ),
            );
            if self.overlay_can_cycle {
                hints.insert(
                    0,
                    HintItem::paired(key!('[', CONTROL), key!(']', CONTROL), "prev/next agent"),
                );
            }
            hints.insert(0, HintItem::new(key!('\\', CONTROL), "dashboard"));
        }
        hints
    }
    /// Render the agent view into the given area.
    ///
    /// Thin orchestrator: computes layout, then calls shared widgets and
    /// agent-specific overlay helpers in sequence. Each component takes
    /// only the state it needs — no arg threading.
    ///
    /// Returns cursor position if the prompt is focused (for terminal cursor).
    /// Render a fullscreen subagent view — replaces the ENTIRE parent view.
    ///
    /// Draws:
    /// 1. Background fill
    /// 2. Title bar: icon + type + description + meta + badge + progress + [elapsed] + [✗]
    /// 3. Border frame (single-line box)
    /// 4. Child `AgentView::draw()` inside the frame
    #[allow(clippy::too_many_arguments)]
    fn draw_subagent_fullscreen(
        &mut self,
        child_sid: &str,
        area: Rect,
        buf: &mut Buffer,
        registry: &ActionRegistry,
        scratch: &mut ScratchBuffer,
        theme: &Theme,
        bundle_state: &crate::app::bundle::BundleState,
    ) -> (
        Option<(u16, u16)>,
        Option<crate::terminal::overlay::PostFlush>,
    ) {
        use crate::app::subagent::{format_context_badge, format_subagent_label};
        use ratatui::style::Modifier;
        use unicode_width::UnicodeWidthStr;
        let appearance = self.scrollback.appearance().clone();
        let layout_cfg = &appearance.scrollback.layout;
        let compact = appearance.prompt.compact;
        agent::fill_background(buf, area, layout_cfg, compact, theme);
        let padded = Rect {
            x: area.x + layout_cfg.eff_hpad_left(compact),
            y: area.y + layout_cfg.eff_outer_vpad(compact),
            width: area.width.saturating_sub(
                layout_cfg.eff_hpad_left(compact) + layout_cfg.eff_hpad_right(compact),
            ),
            height: area
                .height
                .saturating_sub(layout_cfg.eff_outer_vpad(compact) * 2),
        };
        if padded.width < 10 || padded.height < 5 {
            return (None, crate::terminal::overlay::clear().map(Into::into));
        }
        let border_color = theme.selection_border;
        let frame = match crate::views::picker::render_bordered_frame(
            buf,
            padded,
            border_color,
            theme.bg_base,
        ) {
            Some(f) => f,
            None => {
                return (None, crate::terminal::overlay::clear().map(Into::into));
            }
        };
        let title_y = frame.title_row.y;
        let _title_row = frame.title_row;
        let inner = frame.content;
        let _border_style = Style::default().fg(border_color);
        let info = self.subagent_sessions.get(child_sid);
        let raw_description = info.map(|s| s.description.as_ref()).unwrap_or("subagent");
        let is_running = info.is_some_and(|s| s.is_running());
        let elapsed = info
            .map(|s| crate::util::format_duration(s.display_elapsed()))
            .unwrap_or_default();
        let (type_label, description): (String, String) = match info {
            Some(s) => format_subagent_label(s),
            None => (String::new(), raw_description.to_string()),
        };
        let icon = if is_running {
            let spinner_frames = crate::glyphs::dot_spinner_frames();
            let tick = self.tasks.tick_count();
            let frame_idx = (tick / 4) as usize % spinner_frames.len();
            spinner_frames[frame_idx]
        } else if info.and_then(|s| s.status.as_deref()) == Some("completed") {
            crate::glyphs::check_mark()
        } else {
            crate::glyphs::ballot_x()
        };
        let icon_color = if is_running {
            theme.accent_running
        } else if info.and_then(|s| s.status.as_deref()) == Some("completed") {
            theme.accent_success
        } else {
            theme.accent_error
        };
        let label_color = if info.is_some_and(|s| s.pending_kill) {
            theme.accent_error
        } else if is_running {
            theme.accent_running
        } else if info.and_then(|s| s.status.as_deref()) == Some("completed") {
            theme.accent_success
        } else {
            theme.accent_error
        };
        let meta = info
            .and_then(|s| s.model.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();
        let badge = info.map(format_context_badge).unwrap_or("");
        let activity_label: Option<String> = if is_running {
            self.subagent_views.get(child_sid).and_then(|cv| {
                cv.resolve_turn_activity()
                    .map(|a| crate::app::subagent::format_activity_label(&a))
                    .or_else(|| cv.session.state.is_busy().then(|| "Waiting".to_string()))
            })
        } else {
            None
        };
        let title_x = padded.x + 1;
        buf.set_span_safe(
            title_x,
            title_y,
            &Span::styled(format!(" {icon}"), Style::default().fg(icon_color)),
            3,
        );
        let close_text = "[\u{2717}]";
        let close_width: u16 = close_text.width() as u16;
        let elapsed_text = elapsed.clone();
        let right_margin: u16 = 1;
        let badge_width = if badge.is_empty() {
            0
        } else {
            badge.width() as u16 + 1
        };
        let activity_width: u16 = activity_label
            .as_deref()
            .map(|s| s.width() as u16 + 3)
            .unwrap_or(0);
        let right_width = activity_width
            + elapsed_text.width() as u16
            + 1
            + close_width
            + right_margin
            + badge_width;
        let desc_start_x = title_x + 3;
        let avail = padded.width.saturating_sub(5 + right_width) as usize;
        let type_text = if type_label.is_empty() {
            String::new()
        } else if description.is_empty() {
            type_label.clone()
        } else {
            format!("{type_label} ")
        };
        let meta_text = if meta.is_empty() {
            String::new()
        } else {
            format!(" {meta}")
        };
        let overhead = type_text.width() + meta_text.width();
        let desc_max = avail.saturating_sub(overhead);
        let desc_display = crate::render::line_utils::truncate_str(&description, desc_max);
        if !type_text.is_empty() {
            buf.set_span_safe(
                desc_start_x,
                title_y,
                &Span::styled(&type_text, Style::default().fg(label_color)),
                type_text.width() as u16,
            );
        }
        let desc_x = desc_start_x + type_text.width() as u16;
        buf.set_span_safe(
            desc_x,
            title_y,
            &Span::styled(
                &desc_display,
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            desc_display.width() as u16,
        );
        let after_desc_x = desc_x + desc_display.width() as u16;
        if !meta_text.is_empty() {
            buf.set_span_safe(
                after_desc_x,
                title_y,
                &Span::styled(&meta_text, Style::default().fg(theme.gray)),
                meta_text.width() as u16,
            );
        }
        let mut rx = padded.x + padded.width.saturating_sub(right_margin + close_width + 1);
        let close_style = if self.hit_subagent_frame_close.hovered {
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.gray)
        };
        buf.set_span_safe(
            rx,
            title_y,
            &Span::styled(close_text, close_style),
            close_width,
        );
        self.hit_subagent_frame_close.rect = Some(Rect::new(rx, title_y, close_width, 1));
        rx = rx.saturating_sub(elapsed_text.width() as u16 + 1);
        buf.set_span_safe(
            rx,
            title_y,
            &Span::styled(&elapsed_text, Style::default().fg(theme.gray)),
            elapsed_text.width() as u16,
        );
        if let Some(activity) = activity_label.as_deref() {
            let segment = format!("{activity} \u{00b7} ");
            let w = segment.width() as u16;
            rx = rx.saturating_sub(w);
            buf.set_span_safe(
                rx,
                title_y,
                &Span::styled(segment, Style::default().fg(theme.gray)),
                w,
            );
        }
        if !badge.is_empty() {
            rx = rx.saturating_sub(badge.width() as u16 + 1);
            buf.set_span_safe(
                rx,
                title_y,
                &Span::styled(badge, Style::default().fg(theme.gray_dim)),
                badge.width() as u16,
            );
        }
        let mut child_post_flush = None;
        if inner.width > 5
            && inner.height > 3
            && let Some(child_view) = self.subagent_views.get_mut(child_sid)
        {
            child_view.mark_as_subagent_view();
            let (_, post_flush) = child_view.draw(
                inner,
                buf,
                registry,
                scratch,
                None,
                false,
                super::BannerSlotParams::none(),
                bundle_state,
                false,
                false,
                &mut Vec::new(),
                AppRenderParams::default(),
            );
            child_post_flush = post_flush;
        }
        (None, child_post_flush)
    }
    pub fn should_show_tip(&mut self) -> bool {
        false
    }
    /// Left side of the question card footer, which offers different keys for the report box than for the answer rows.
    fn question_footer_hints(
        qv: &crate::views::question_view::QuestionViewState,
        feedback_pane: bool,
        hint_style: Style,
        hint_key: Style,
    ) -> Vec<Span<'static>> {
        if feedback_pane {
            Self::feedback_footer_hints(hint_style, hint_key)
        } else {
            Self::answer_footer_hints(qv, hint_style, hint_key)
        }
    }
    /// The report box offers only a newline: nothing to navigate or copy, and the shortcuts bar below already carries Esc.
    fn feedback_footer_hints(hint_style: Style, hint_key: Style) -> Vec<Span<'static>> {
        let newline_key = if crate::terminal::terminal_context().shift_enter_unavailable() {
            "Alt+Enter"
        } else {
            "Shift+Enter"
        };
        vec![
            Span::styled(newline_key, hint_key),
            Span::styled(" newline", hint_style),
        ]
    }
    /// Walking and copying the answer rows, counter first when the card holds more than one question.
    fn answer_footer_hints(
        qv: &crate::views::question_view::QuestionViewState,
        hint_style: Style,
        hint_key: Style,
    ) -> Vec<Span<'static>> {
        let mut left_spans: Vec<Span<'static>> = Vec::new();
        if qv.questions.len() > 1 {
            let counter = format!("[{}/{}] ", qv.active_tab + 1, qv.questions.len());
            left_spans.push(Span::styled(counter, hint_style));
        }
        left_spans.push(Span::styled("\u{2191}/\u{2193}", hint_key));
        left_spans.push(Span::styled(" navigate", hint_style));
        if qv.questions.len() > 1 {
            left_spans.push(Span::styled(" \u{b7} ", hint_style));
            left_spans.push(Span::styled("\u{2190}/\u{2192}", hint_key));
            left_spans.push(Span::styled(" question", hint_style));
        }
        left_spans.push(Span::styled(" \u{b7} ", hint_style));
        left_spans.push(Span::styled("y", hint_key));
        left_spans.push(Span::styled(" copy", hint_style));
        left_spans
    }
    /// `area` is the screen region assigned to this agent view.
    /// When a tracing overlay is visible, this is smaller than `f.area()`.
    #[allow(clippy::too_many_arguments)]
    /// Render the agent into `area`.
    ///
    /// `in_dashboard_overlay` is `true` when this view is being
    /// rendered inside the dashboard's session-overlay; it appends
    /// `Ctrl+\\:dashboard` (and, when `overlay_can_cycle`, the
    /// `Ctrl+[/]:prev/next agent` chip) to the bottom shortcuts bar so the
    /// user can discover keyboard back-out and agent navigation from
    /// inside the agent view itself (not just from the overlay header).
    ///
    /// `overlay_can_cycle` mirrors the header `[‹]`/`[›]` gate: true when
    /// the visible overlay cycle order has more than one agent. Callers
    /// derive it from the same `position` used for the header chips.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        registry: &ActionRegistry,
        scratch: &mut ScratchBuffer,
        pending_hint: Option<PendingHint>,
        overlay_focused: bool,
        banner: super::BannerSlotParams<'_>,
        bundle_state: &crate::app::bundle::BundleState,
        in_dashboard_overlay: bool,
        overlay_can_cycle: bool,
        link_spans_out: &mut Vec<echo_agent_ratatui_inline::LinkSpan>,
        app_params: AppRenderParams<'_>,
    ) -> (
        Option<(u16, u16)>,
        Option<crate::terminal::overlay::PostFlush>,
    ) {
        let AppRenderParams {
            voice_available,
            voice_listening,
            voice_interim,
            esc_owned_before_agent,
            status_line,
        } = app_params;
        self.scrollback.begin_frame();
        self.in_dashboard_overlay = in_dashboard_overlay;
        self.overlay_can_cycle = overlay_can_cycle;
        let super::BannerSlotParams {
            height: banner_height,
            announcements: banner_announcements,
            hidden_ids: hidden_announcement_ids,
            privacy_banner,
            mouse_pos,
            tip,
        } = banner;
        self.session_banner_active = crate::views::announcements::first_session_announcement(
            banner_announcements,
            hidden_announcement_ids,
        )
        .is_some();
        self.privacy_banner.active = privacy_banner;
        self.pinned_upgrade_cta_live =
            crate::views::announcements::promo_cta(banner_announcements, hidden_announcement_ids)
                .is_some_and(|(owner, _, _)| !crate::views::announcements::is_dismissible(owner));
        self.frame_occluder_rects.clear();
        self.clear_scrollback_selection_state();
        self.refresh_prompt_suggestion_gate();
        let theme = Theme::current();
        let link_active_style = Style::default()
            .fg(theme.link_fg)
            .add_modifier(ratatui::style::Modifier::UNDERLINED | ratatui::style::Modifier::BOLD);
        self.note_terminal_size((area.width, area.height));
        #[allow(unused_assignments)]
        let mut scrollback_inline_media = Vec::new();
        #[allow(unused_assignments)]
        let mut scrollback_diagram_affordances: Vec<
            crate::scrollback::render::DiagramAffordancePlacement,
        > = Vec::new();
        if self.inline_media_active
            && (self.image_viewer.is_some()
                || self.video_viewer.is_some()
                || self.gboom.is_some()
                || self.block_viewer.is_some()
                || self.extensions_modal.is_some()
                || self.agents_modal.is_some()
                || self.btw_state.is_some()
                || self.line_viewer.is_some()
                || self.active_modal.is_some())
        {
            self.inline_media_active = false;
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                for &id in self.inline_media_ids.values() {
                    let clear = crate::terminal::image::clear_kitty_image(id);
                    let _ = std::io::Write::write_all(stderr, clear.as_bytes());
                }
            });
            self.inline_media_ids.clear();
            self.inline_media_iterm_emitted.clear();
        }
        if let Some(ref child_sid) = self.active_subagent.clone() {
            if let Some(esc) = self.take_own_inline_media_clear_escapes() {
                echo_agent_runtime::util::with_locked_stderr(|stderr| {
                    let _ = std::io::Write::write_all(stderr, esc.as_bytes());
                });
            }
            self.hit_announcement_hide.clear();
            self.hit_announcement_cta.clear();
            self.hit_upgrade_cta.clear();
            self.privacy_banner.clear_hits();
            return self.draw_subagent_fullscreen(
                &child_sid.clone(),
                area,
                buf,
                registry,
                scratch,
                &theme,
                bundle_state,
            );
        }
        if let Some(esc) = self.take_subagent_inline_media_clear_escapes() {
            echo_agent_runtime::util::with_locked_stderr(|stderr| {
                let _ = std::io::Write::write_all(stderr, esc.as_bytes());
            });
        }
        let appearance = self.scrollback.appearance().clone();
        let layout_cfg = &appearance.scrollback.layout;
        let scrollbar_cfg = &appearance.scrollback.scrollbar;
        let model_id = self
            .session
            .models
            .current_model_name()
            .unwrap_or_else(|| "unknown".to_string());
        let effective_plan = self.plan_mode_pending.unwrap_or(self.plan_mode_active);
        let casual_commenting = self.is_casual_commenting();
        let prompt_focused = if self.plan_approval_view.is_some() {
            self.plan_approval_view
                .as_ref()
                .is_some_and(|pav| pav.focus != PlanApprovalFocus::Preview)
        } else if casual_commenting {
            true
        } else {
            self.active_pane == AgentPane::Prompt && !overlay_focused
        };
        let prompt_style = PromptStyle {
            focused: prompt_focused,
            show_prefix: appearance.prompt.show_prefix,
            vpad_top: 1,
            compact: appearance.prompt.compact,
            chrome: true,
            chrome_pad_left: layout_cfg.block_pad_left,
            chrome_pad_right: layout_cfg.block_pad_right,
            bg: PromptBg::Default,
            accent_color_override: if let Some(c) = self.prompt_input_mode.accent_color(&theme) {
                Some(c)
            } else if effective_plan || casual_commenting {
                Some(theme.accent_plan)
            } else {
                None
            },
            border_color_override: if effective_plan || casual_commenting {
                crate::render::color::blend_color(theme.bg_base, theme.accent_plan, 0.4)
            } else {
                None
            },
            prefix_override: if let Some(p) = self.prompt_input_mode.prefix_override(&theme) {
                Some(p)
            } else if casual_commenting
                || self
                    .plan_approval_view
                    .as_ref()
                    .is_some_and(|pav| pav.focus == PlanApprovalFocus::Commenting)
            {
                Some((
                    if crate::glyphs::is_legacy_windows_console() {
                        "\u{2022} "
                    } else {
                        "\u{25CF} "
                    },
                    theme.accent_plan,
                ))
            } else {
                None
            },
            placeholder_when_focused: false,
            placeholder_override: if let Some(ph) = self
                .prompt_input_mode
                .placeholder_override(self.multiline_mode)
            {
                Some(ph)
            } else if casual_commenting
                || self
                    .plan_approval_view
                    .as_ref()
                    .is_some_and(|pav| pav.focus == PlanApprovalFocus::Commenting)
            {
                Some("Type your comment...")
            } else if self
                .plan_approval_view
                .as_ref()
                .is_some_and(|pav| pav.focus == PlanApprovalFocus::Prompt)
            {
                Some("Type revision notes...")
            } else {
                None
            },
            show_accent_line: false,
            show_borders: true,
            title: self.prompt_caption(),
            image_preview: true,
        };
        let next = crate::views::session_title::rename_source_title_raw(self)
            .map(crate::views::session_title::sanitize_display_text);
        if self.prompt.slash_current_title() != next.as_deref() {
            self.prompt
                .set_slash_current_title(next.map(|s| s.into_owned()));
            if self.prompt.slash_open() {
                self.prompt.refresh_slash(&self.session.models);
            }
        }
        let compact = appearance.prompt.compact;
        let inner_width = AgentViewLayout::inner_width(area, layout_cfg, compact);
        let banner_height = if banner_height > 0 {
            if let Some(tip_text) = tip {
                if self.session_banner_active {
                    banner_height
                } else {
                    banner_height.max(crate::tips::render::tip_height(
                        inner_width.saturating_sub(crate::tips::render::HINT_INSET),
                        tip_text,
                    ))
                }
            } else {
                banner_height
            }
        } else {
            banner_height
        };
        let banner_height = if privacy_banner {
            banner_height.max(crate::views::privacy_banner::height(inner_width))
        } else {
            banner_height
        };
        let tip_row_visible =
            self.ephemeral_tip_renderable(area.height) && self.ephemeral_tip.is_active();
        let banner_height = banner_height.max(u16::from(tip_row_visible));
        let max_prompt_height = area.height / 2;
        let base_prompt_height = if !prompt_focused && appearance.prompt.collapse_unfocused {
            self.prompt
                .desired_height(inner_width, &prompt_style, true, max_prompt_height)
                .min(prompt_style.vpad_top + 1 + prompt_style.info_block(true))
        } else {
            self.prompt
                .desired_height(inner_width, &prompt_style, true, max_prompt_height)
        };
        let overlay_content_w = inner_width.saturating_sub(QUESTION_VIEW_HPAD) as usize;
        let slot_card = self.blocking_card();
        let permission_view_h = if slot_card == Some(BlockingCard::Permission) {
            if let Some(perm) = self.permission_queue.front() {
                crate::views::permission_view::permission_view_height(
                    perm,
                    area.height,
                    overlay_content_w,
                )
            } else {
                0
            }
        } else {
            0
        };
        let question_view_h = if slot_card == Some(BlockingCard::Question) {
            if let Some(ref mut qv) = self.question_view {
                crate::views::question_view::question_view_height(
                    qv,
                    area.height,
                    overlay_content_w,
                )
            } else {
                0
            }
        } else {
            0
        };
        let elicitation_view_h = if slot_card == Some(BlockingCard::McpElicitation) {
            if let Some(ref ev) = self.elicitation_view {
                crate::views::elicitation_view::elicitation_view_height(
                    ev,
                    area.height,
                    overlay_content_w,
                )
            } else {
                0
            }
        } else {
            0
        };
        let rewind_view_h =
            if permission_view_h == 0 && question_view_h == 0 && elicitation_view_h == 0 {
                if let Some(ref rw) = self.rewind_state {
                    crate::views::rewind::rewind_overlay_height(&rw.phase, area.height)
                } else {
                    0
                }
            } else {
                0
            };
        let cancel_turn_view_h =
            if slot_card == Some(BlockingCard::CancelTurn) && rewind_view_h == 0 {
                modal::cancel_turn_panel_height(area.height)
            } else {
                0
            };
        let jump_view_h = if !self.jump_slot_taken() {
            if let Some(ref js) = self.jump_state {
                crate::views::jump::jump_overlay_height(js, area.height)
            } else {
                0
            }
        } else {
            0
        };
        let is_question_input_mode = self
            .question_view
            .as_ref()
            .map(|qv| qv.focus == crate::views::question_view::QuestionFocus::InputMode)
            .unwrap_or(false);
        let question_input_style = PromptStyle {
            focused: true,
            show_prefix: true,
            vpad_top: 0,
            chrome: false,
            chrome_pad_left: 0,
            chrome_pad_right: 0,
            bg: PromptBg::Panel(theme.bg_visual),
            accent_color_override: None,
            border_color_override: None,
            prefix_override: None,
            placeholder_when_focused: false,
            placeholder_override: None,
            compact: false,
            show_accent_line: false,
            show_borders: false,
            title: None,
            image_preview: true,
        };
        let feedback_pane = self
            .question_view
            .as_ref()
            .is_some_and(|qv| qv.is_feedback_report());
        let inline_prompt_max = ((area.height as u32) / 3).clamp(3, 15) as u16;
        let question_prompt_body_h = if question_view_h == 0 || !is_question_input_mode {
            0
        } else if feedback_pane {
            self.feedback_editor_h(inner_width, inline_prompt_max, &theme)
        } else {
            self.freeform_editor_h(inner_width, inline_prompt_max, &question_input_style)
        };
        let is_permission_followup = self.permission_queue.front().is_some_and(|p| {
            p.focus == crate::views::permission_view::PermissionFocus::FollowupInput
        });
        let perm_inline_prompt_max = ((area.height as u32) / 3).clamp(3, 15) as u16;
        let perm_inline_prompt_h: u16 = if permission_view_h > 0 && is_permission_followup {
            let style = PromptStyle {
                focused: true,
                show_prefix: false,
                vpad_top: 0,
                chrome: false,
                chrome_pad_left: 0,
                chrome_pad_right: 0,
                bg: PromptBg::Panel(theme.bg_visual),
                accent_color_override: None,
                border_color_override: None,
                prefix_override: None,
                placeholder_when_focused: false,
                placeholder_override: None,
                compact: false,
                show_accent_line: false,
                show_borders: false,
                title: None,
                image_preview: true,
            };
            let perm_text_w = crate::views::permission_view::inline_text_width(inner_width);
            self.prompt
                .desired_height(perm_text_w, &style, false, perm_inline_prompt_max)
                .max(1)
        } else {
            0
        };
        let question_footer_h: u16 = if question_view_h > 0 { 3 } else { 0 };
        let prompt_height = if permission_view_h > 0 {
            if is_permission_followup && perm_inline_prompt_h > 1 {
                permission_view_h + perm_inline_prompt_h.saturating_sub(1)
            } else {
                permission_view_h
            }
        } else if question_view_h > 0 {
            let freeform_offset = if is_question_input_mode { 1u16 } else { 0 };
            question_view_h.saturating_sub(freeform_offset)
                + question_prompt_body_h
                + question_footer_h
        } else if elicitation_view_h > 0 {
            elicitation_view_h
        } else if rewind_view_h > 0 {
            rewind_view_h
        } else if jump_view_h > 0 {
            jump_view_h
        } else if cancel_turn_view_h > 0 {
            cancel_turn_view_h
        } else {
            base_prompt_height
        };
        let prompt_height =
            prompt_height.max(prompt_style.vpad_top + 1 + prompt_style.info_block(true));
        let prompt_height = if self.is_subagent_view {
            0
        } else {
            prompt_height
        };
        {
            use crate::app::agent::PENDING_KILL_TIMEOUT_SECS;
            let now = Instant::now();
            for task in self.session.bg_tasks.values_mut() {
                if let Some(requested) = task.kill_requested_at
                    && now.duration_since(requested).as_secs() >= PENDING_KILL_TIMEOUT_SECS
                {
                    task.pending_kill = false;
                    task.kill_requested_at = None;
                }
            }
            for info in self.subagent_sessions.values_mut() {
                if let Some(requested) = info.kill_requested_at
                    && now.duration_since(requested).as_secs() >= PENDING_KILL_TIMEOUT_SECS
                {
                    info.pending_kill = false;
                    info.kill_requested_at = None;
                }
            }
        }
        let queued_cron_ids: HashSet<&str> = self
            .session
            .pending_prompts
            .iter()
            .filter(|p| p.kind == crate::app::agent::QueueEntryKind::Cron)
            .filter_map(|p| p.task_id.as_deref())
            .collect();
        self.tasks.sync(
            &self.session.bg_tasks,
            &self.subagent_sessions,
            &self.session.scheduled_tasks,
            self.cron_task_id.as_deref(),
            &queued_cron_ids,
            &self.workflow_runs,
        );
        if self.active_pane == ActivePane::Tasks && !self.tasks.is_visible() {
            self.active_pane = ActivePane::Scrollback;
        }
        self.catalog.sync_from_bundle(bundle_state);
        if self.active_pane == ActivePane::Catalog && !self.catalog.is_visible() {
            self.active_pane = ActivePane::Scrollback;
        }
        self.catalog.sync_from_bundle(bundle_state);
        if self.active_pane == ActivePane::Catalog && !self.catalog.is_visible() {
            self.active_pane = ActivePane::Scrollback;
        }
        let viewer_open = self.active_subagent.is_some();
        let tasks_height = if viewer_open {
            0
        } else {
            self.tasks.desired_height(area.height)
        };
        let catalog_height = if viewer_open {
            0
        } else {
            self.catalog.desired_height(area.height)
        };
        let todo_height = if viewer_open {
            0
        } else {
            self.todo.desired_height(area.height)
        };
        self.sync_queue_pane();
        if self.active_pane == ActivePane::Queue && !self.queue.is_visible() {
            self.active_pane = ActivePane::Scrollback;
        }
        let queue_height = self.queue.desired_height();
        let drain_blocked = self.drain_blocked();
        let watchers = self.watchers();
        let parked = self.renders_parked();
        let wake_display_state = self.wake_display_state();
        let turn_status_height = if turn_status::should_show(
            wake_display_state.unwrap_or(&self.session.state),
            drain_blocked,
            self.mcp_init_progress.as_ref(),
            watchers,
            parked,
        ) {
            1
        } else {
            0
        };
        let prompt_gap = if appearance.prompt.compact
            || (turn_status_height > 0 && !appearance.turn_status.gap)
            || area.height <= agent::SHORT_TERMINAL_ROWS
        {
            0
        } else {
            1
        };
        let voice_recording_height = if voice_listening { 1 } else { 0 };
        let _tool_usage_height = 0u16;
        let btw_height =
            crate::views::btw_overlay::btw_panel_height(self.btw_state.as_ref(), inner_width);
        let cta_height = match &self.plugin_cta.phase {
            _ if privacy_banner => 0,
            CtaPhase::Hidden => 0,
            CtaPhase::Matched { .. } if self.prompt.text().trim().is_empty() => 0,
            _ => 1,
        };
        let follow_ups_height = u16::from(self.follow_ups.is_some());
        let timeline_width = crate::views::timeline::rail_width(
            appearance.show_timeline,
            self.is_subagent_view,
            area.width,
            self.scrollback.turn_count(),
        );
        let mut layout_params = AgentViewLayoutParams {
            area,
            layout_cfg: *layout_cfg,
            scrollbar_cfg: *scrollbar_cfg,
            timeline_width,
            prompt_height,
            tasks_height,
            catalog_height,
            todo_height,
            queue_height,
            btw_height,
            turn_status_height,
            banner_height,
            cta_height,
            follow_ups_height,
            prompt_gap,
            voice_recording_height,
            shortcuts_height: 1,
            status_line_height: status_line.height(),
            compact,
        };
        if question_view_h > 0 {
            layout_params.prompt_height =
                prompt_height.min(AgentViewLayout::rows_available_for_prompt(layout_params));
        }
        let mut layout = AgentViewLayout::compute(layout_params);
        let search_active =
            self.scrollback_search.is_some() && self.active_pane == AgentPane::Scrollback;
        let search_reserved_rows =
            Self::search_reserved_rows(layout.scrollback.height, search_active);
        if search_reserved_rows > 0 {
            layout.scrollback.height -= search_reserved_rows;
            layout.scrollback_content.height = layout
                .scrollback_content
                .height
                .saturating_sub(search_reserved_rows);
        }
        let overlay_blocks_rail_hover = self.jump_state.is_some()
            || self.rewind_state.is_some()
            || self.blocking_card().is_some()
            || self.block_viewer.is_some();
        if layout.timeline_width > 0 {
            self.sync_pending_user_input_marks();
            self.scrollback.set_cwd(Some(self.session.cwd.clone()));
            let _ = self.sync_inline_edit_layout(layout.scrollback_content.width);
            self.scrollback.prepare_layout(
                layout.scrollback_content.width,
                layout.scrollback_content.height,
            );
            let viewport = crate::views::timeline::RailViewport {
                active: self.scrollback.active_turn_for_viewport(),
                up_target: self.scrollback.turn_above_viewport_top(),
                down_target: self.scrollback.turn_below_viewport_top(),
                at_bottom: !self.scrollback.has_content_below(),
            };
            match crate::views::timeline::compute_rail(
                layout.scrollback,
                layout.timeline_x,
                self.scrollback.turn_count(),
                viewport,
            ) {
                Some(rail) => {
                    self.timeline_rail = Some(rail);
                    if overlay_blocks_rail_hover {
                        self.timeline_hover = None;
                        self.timeline_hover_preview = None;
                    } else {
                        let (col, row) = self.last_mouse_pos;
                        let new_hover = self.timeline_rail.as_ref().and_then(|r| r.hit(col, row));
                        if new_hover != self.timeline_hover {
                            self.timeline_hover = new_hover;
                            self.sync_timeline_hover_preview();
                        }
                    }
                }
                None => {
                    self.timeline_rail = None;
                    self.timeline_hover = None;
                    self.timeline_hover_preview = None;
                    layout = AgentViewLayout::compute(AgentViewLayoutParams {
                        timeline_width: 0,
                        ..layout_params
                    });
                    if search_reserved_rows > 0 {
                        layout.scrollback.height -= search_reserved_rows;
                        layout.scrollback_content.height = layout
                            .scrollback_content
                            .height
                            .saturating_sub(search_reserved_rows);
                    }
                    self.scrollback.prepare_layout(
                        layout.scrollback_content.width,
                        layout.scrollback_content.height,
                    );
                }
            }
        } else {
            self.timeline_rail = None;
            self.timeline_hover = None;
            self.timeline_hover_preview = None;
        }
        agent::fill_background(buf, area, layout_cfg, compact, &theme);
        let mut status_line_link_spans: Vec<echo_agent_ratatui_inline::LinkSpan> = Vec::new();
        if let Some(padding) = status_line.padding()
            && layout.status_line.height > 0
        {
            if let Some(width) =
                crate::views::status_line::inner_width(layout.status_line.width, padding)
            {
                self.last_status_line_size = Some(crate::views::status_line::RowSize {
                    cols: width,
                    lines: layout.status_line.height,
                });
            }
            if let Some(display) = status_line.display() {
                status_line_link_spans = crate::views::status_line::render_status_line(
                    buf,
                    layout.status_line,
                    display,
                    padding,
                    &theme,
                );
            }
        }
        use crate::views::agent_status::AgentStatusBar;
        use crate::views::context_bar;
        let mut status = AgentStatusBar::new(&theme);
        if let Some(url) = self.highlighted_link_url() {
            let max_len = layout.status_bar.width.saturating_sub(20) as usize;
            let display = if url.len() > max_len {
                let truncated: String = url.chars().take(max_len.saturating_sub(1)).collect();
                format!("{truncated}\u{2026}")
            } else {
                url.to_string()
            };
            let link_style = Style::default().fg(theme.link_fg).bg(theme.bg_base);
            status.push("link_url", Line::from(Span::styled(display, link_style)));
        }
        let task_counts = self.tasks.status_counts(
            &self.session.bg_tasks,
            &self.subagent_sessions,
            &self.session.scheduled_tasks,
            &self.workflow_runs,
        );
        if let Some(line) = crate::views::agent_status::task_status_line(
            task_counts,
            &theme,
            self.hit_bg_status.hovered,
        ) {
            status.push("bg_tasks", line);
        }
        if self.should_show_plan_chip(&appearance) {
            let mut plan_style = Style::default().fg(theme.accent_plan).bg(theme.bg_base);
            if self.hit_plan_button.hovered {
                plan_style = plan_style.add_modifier(ratatui::style::Modifier::BOLD);
            }
            status.push("plan", Line::from(Span::styled("plan", plan_style)));
        }
        if let Some(ref goal) = self.goal_state {
            let tick = self.tasks.tick_count() as usize;
            let active_subagent_tokens: u64 = self
                .subagent_sessions
                .values()
                .filter(|s| !s.finished && s.workflow_run_id.is_none())
                .filter_map(|s| s.tokens_used)
                .sum();
            status.push(
                "goal",
                crate::views::agent_status::goal_status_line(
                    goal,
                    &theme,
                    self.hit_goal_status.hovered,
                    tick,
                    self.context_state.as_ref().map(|c| c.used),
                    active_subagent_tokens,
                ),
            );
        }
        if let Some(mcp_line) = self.mcp_init_progress.as_ref().and_then(|p| {
            crate::views::agent_status::mcp_status_line(p, self.scrollback.animation_tick(), &theme)
        }) {
            status.push("mcp", mcp_line);
        }
        #[cfg(feature = "local-workspace")]
        if self.chat_kind || self.app_chat_mode {
            let label = self
                .workspace_mode
                .status_label(self.workspace_mode_cli_locked);
            let mut mode_style = Style::default().fg(theme.accent_user).bg(theme.bg_base);
            if self.workspace_mode_cli_locked {
                mode_style = mode_style.add_modifier(ratatui::style::Modifier::DIM);
            }
            status.push(
                "workspace_mode",
                Line::from(Span::styled(label, mode_style)),
            );
        }
        let ctx_used = self.context_state.as_ref().map(|c| c.used);
        let model_window = self.session.models.get_context_window();
        let ctx_total = self
            .context_state
            .as_ref()
            .and_then(|c| (c.total > 0).then_some(c.total))
            .or(model_window);
        if let Some(ctx_line) = context_bar::context_bar_line_for_session(
            ctx_used,
            ctx_total,
            self.hit_context.hovered,
            &theme,
            self.chat_kind,
        ) {
            status.push("context", ctx_line);
        }
        let areas = status.render(buf, layout.status_bar);
        self.hit_bg_status.rect = areas.get("bg_tasks").copied();
        self.hit_goal_status.rect = areas.get("goal").copied();
        self.hit_context.rect = areas.get("context").copied();
        self.hit_credits.rect = areas.get("credits").copied();
        self.hit_plan_button.rect = areas.get("plan").copied();
        let home = std::env::var("HOME").ok();
        let display = self.session.cwd.display().to_string();
        let short = match &home {
            Some(h) if display.starts_with(h.as_str()) => {
                format!("~{}", &display[h.len()..])
            }
            _ => display,
        };
        let cwd_style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
        use unicode_width::UnicodeWidthStr;
        let mut parts: Vec<Span> = Vec::new();
        let mut path_offset: u16 = 0;
        let lazy_git = crate::git_info::cwd_git_info_lazy(&self.session.cwd);
        let branch = self
            .current_branch
            .clone()
            .or_else(|| lazy_git.as_ref().and_then(|i| i.branch.clone()));
        let git_text = branch.map(|b| {
            let icon = crate::git_info::branch_icon();
            if b.is_empty() {
                format!("{icon} detached")
            } else {
                format!("{icon} {b}")
            }
        });
        if let Some(git_text) = git_text {
            let git_style = Style::default()
                .fg(theme.text_primary)
                .bg(theme.bg_base)
                .add_modifier(ratatui::style::Modifier::DIM);
            path_offset += git_text.width() as u16;
            parts.push(Span::styled(git_text, git_style));
            path_offset += 1;
            parts.push(Span::styled(" ", Style::default().bg(theme.bg_base)));
        }
        let show_worktree_label = self.is_worktree
            || self.session.is_worktree
            || lazy_git.as_ref().is_some_and(|i| i.is_worktree);
        if show_worktree_label {
            let label_style = Style::default().fg(theme.accent_user).bg(theme.bg_base);
            path_offset += "worktree ".width() as u16;
            parts.push(Span::styled("worktree ", label_style));
        }
        if let Some(profile) = echo_agent_sandbox::profile_name() {
            let sandbox_text = format!("sandbox:{profile} ");
            let sandbox_style = Style::default().fg(theme.warning).bg(theme.bg_base);
            path_offset += sandbox_text.width() as u16;
            parts.push(Span::styled(sandbox_text, sandbox_style));
        }
        let path_width = short.width() as u16;
        let path_style = if self.hit_cwd.hovered {
            Style::default().fg(theme.text_primary).bg(theme.bg_base)
        } else {
            cwd_style
        };
        parts.push(Span::styled(short, path_style));
        let main_repo_display = self
            .main_repo
            .clone()
            .or_else(|| lazy_git.as_ref().and_then(|i| i.main_repo.clone()));
        if let Some(main_repo) = main_repo_display {
            parts.push(Span::styled(
                format!(" (worktree of {main_repo})"),
                cwd_style,
            ));
        }
        let cwd_line = Line::from(parts);
        let max_cwd_width = areas
            .values()
            .map(|r| r.x)
            .min()
            .map(|min_x| min_x.saturating_sub(layout.status_bar.x).saturating_sub(1))
            .unwrap_or(layout.status_bar.width);
        let upgrade_cta =
            crate::views::announcements::promo_cta(banner_announcements, hidden_announcement_ids);
        let upgrade_reserve = upgrade_cta.map_or(0u16, |(_, label, _)| {
            1 + crate::views::announcements::upgrade_cta_reserve(label, None)
        });
        let cwd_line = truncate_line(
            cwd_line,
            max_cwd_width.saturating_sub(upgrade_reserve) as usize,
        );
        let cwd_width = cwd_line.width() as u16;
        buf.set_line_safe(
            layout.status_bar.x,
            layout.status_bar.y,
            &cwd_line,
            cwd_width,
        );
        let path_x = layout.status_bar.x + path_offset;
        let visible_path_width = path_width.min(cwd_width.saturating_sub(path_offset));
        self.hit_cwd.rect = (visible_path_width > 0).then_some(Rect {
            x: path_x,
            y: layout.status_bar.y,
            width: visible_path_width,
            height: 1,
        });
        let mut upgrade_cta_rect = None;
        if let Some((_owner, label, _url)) = upgrade_cta {
            let avail = max_cwd_width.saturating_sub(cwd_width);
            if avail > 1 {
                let cta_x = layout.status_bar.x + cwd_width;
                buf.set_span(
                    cta_x,
                    layout.status_bar.y,
                    &Span::styled(" ", Style::default().bg(theme.bg_base)),
                    1,
                );
                upgrade_cta_rect = crate::views::announcements::render_cta_button(
                    buf,
                    &theme,
                    cta_x + 1,
                    layout.status_bar.y,
                    avail - 1,
                    label,
                    None,
                    self.hit_upgrade_cta.hovered,
                );
            }
        }
        let dropdown_open = self.prompt.any_dropdown_open();
        self.hit_upgrade_cta
            .set_unless_dropdown(upgrade_cta_rect, dropdown_open);
        let mut inline_edit_cursor: Option<(u16, u16)> = None;
        let sticky_gap_row: Option<u16>;
        {
            self.sync_pending_user_input_marks();
            self.scrollback.set_cwd(Some(self.session.cwd.clone()));
            let inline_edit_dim_from =
                self.sync_inline_edit_layout(layout.scrollback_content.width);
            self.scrollback.prepare_layout(
                layout.scrollback_content.width,
                layout.scrollback_content.height,
            );
            let rewind_dim_from = self.rewind_dim_from_entry().or(inline_edit_dim_from);
            let sb_focused = self.active_pane == ActivePane::Scrollback && !overlay_focused;
            let search_highlight = if search_active {
                self.scrollback_search
                    .as_ref()
                    .and_then(|s| s.highlight_regex())
            } else {
                None
            };
            self.ensure_media_link_paths();
            let sb_rendered = crate::scrollback::ScrollbackPane::new()
                .active(sb_focused)
                .with_mouse_pos(self.last_mouse_pos)
                .with_dim_from(rewind_dim_from)
                .with_hovered_entry(self.hovered_entry)
                .with_search_highlight(search_highlight)
                .with_media_paths(self.media_link_paths.clone())
                .render_with_scratch_and_selection_boundaries(
                    layout.scrollback_content,
                    buf,
                    &self.scrollback,
                    scratch,
                );
            let sb_output = sb_rendered.output;
            sticky_gap_row = sb_output.sticky_gap_row;
            self.update_scrollback_selection_state(
                sb_output.selection_model.clone(),
                sb_rendered.selection_boundaries,
            );
            self.reclamp_drag_head_post_render(false);
            if self.inline_edit.is_some() {
                let cursor = self.render_inline_edit(buf, layout.scrollback_content);
                if self.rewind_state.is_none() {
                    inline_edit_cursor = cursor;
                }
            }
            if search_reserved_rows > 0
                && let Some(search) = self.scrollback_search.as_ref()
            {
                let reserved_top = layout.scrollback.y + layout.scrollback.height;
                let bar_y = reserved_top + (search_reserved_rows - 1);
                if search_reserved_rows >= 2 {
                    crate::views::picker::render_divider(
                        buf,
                        layout.scrollback.x,
                        reserved_top,
                        layout.scrollback.width,
                        &theme,
                        None,
                    );
                }
                let query = search.query();
                let counter = match search.current_index() {
                    Some(i) => Some(format!("{}/{}", i + 1, search.match_count())),
                    None if search.has_error() => Some("bad pattern".to_string()),
                    None if !query.is_empty() => Some("no matches".to_string()),
                    None => None,
                };
                let counter_width = counter
                    .as_deref()
                    .map_or(0, |text| UnicodeWidthStr::width(text) as u16);
                let search_layout =
                    crate::views::picker::search_bar_layout(layout.scrollback.width, counter_width);
                let leading_query;
                let (rendered_query, viewport) = if search.is_composing() {
                    (
                        query,
                        Some(search.query_viewport(search_layout.input_width())),
                    )
                } else {
                    leading_query =
                        crate::render::line_utils::truncate_str(query, search_layout.input_width());
                    (leading_query.as_str(), None)
                };
                crate::views::picker::render_search_bar_with_viewport(
                    buf,
                    layout.scrollback.x,
                    bar_y,
                    search_layout,
                    &theme,
                    rendered_query,
                    search.is_composing(),
                    !search.is_composing(),
                    None,
                    viewport.unwrap_or(echo_agent_ratatui_textarea::SingleLineViewport {
                        visible_byte_range: 0..rendered_query.len(),
                        cursor_display_column: 0,
                    }),
                );
                if let Some(counter) = counter
                    && search_layout.trailing_width() > 0
                {
                    let w = UnicodeWidthStr::width(counter.as_str()) as u16;
                    if layout.scrollback.width > w {
                        buf.set_string(
                            layout.scrollback.x + layout.scrollback.width - w,
                            bar_y,
                            &counter,
                            Style::default().fg(theme.gray),
                        );
                    }
                }
            }
            self.last_link_overlay = sb_output.link_overlay;
            scrollback_inline_media = sb_output.inline_media;
            scrollback_diagram_affordances = sb_output.diagram_affordances;
            if self.visible_link_map.is_stale(self.scrollback.generation()) {
                let citation_links =
                    collect_citation_links(&self.scrollback, &sb_output.selection_model);
                self.visible_link_map.rebuild(
                    self.scrollback.generation(),
                    &self.last_link_overlay,
                    citation_links,
                );
                self.scrollback_visible_link_count = self.visible_link_map.len();
            } else {
                self.visible_link_map
                    .truncate(self.scrollback_visible_link_count);
            }
            let sb_link_n = self.scrollback_visible_link_count;
            self.paint_link_highlights(buf, link_active_style, 0..sb_link_n);
            agent::render_entry_hover(
                buf,
                layout.scrollback,
                &self.scrollback,
                self.hovered_entry,
                &theme,
            );
            if let Some(ref drag) = self.drag_selection
                && drag.anchor.entry_idx != BTW_OVERLAY_ENTRY_IDX
            {
                render_active_selection_overlay(
                    &self.last_scrollback_selection_model,
                    drag,
                    self.table_geometry_for_selection(drag.anchor.entry_idx, drag.anchor.range_id),
                    buf,
                );
            } else if let Some(ref block_drag) = self.block_drag_selection {
                render_block_drag_overlay(&self.last_scrollback_selection_model, block_drag, buf);
            } else if let Some(ref sel) = self.persistent_text_selection
                && sel.entry_idx != BTW_OVERLAY_ENTRY_IDX
            {
                render_persistent_selection_overlay(
                    &self.last_scrollback_selection_model,
                    sel,
                    self.table_geometry_for_selection(sel.entry_idx, sel.range_id),
                    buf,
                );
            }
            agent::render_hook_hover_popup(
                buf,
                layout.scrollback,
                &self.scrollback,
                self.hovered_entry,
                self.last_mouse_pos,
                &theme,
            );
            let any_drag_active =
                self.drag_selection.is_some() || self.block_drag_selection.is_some();
            if !any_drag_active
                && !overlay_focused
                && let Some(ref selection_box) = sb_output.selection_box
            {
                selection_box.render(buf);
                self.render_selection_buttons(
                    buf,
                    selection_box,
                    sb_output.selected_entry_area,
                    &theme,
                );
            } else {
                self.hit_sb_copy.clear();
                self.hit_sb_view.clear();
            }
            let rail_shown = self.timeline_rail.is_some();
            if !rail_shown {
                agent::render_scrollbar(
                    buf,
                    layout.scrollback,
                    layout.scrollbar_x,
                    scrollbar_cfg,
                    sb_output.scroll_info,
                    self.scrollback.is_follow_mode(),
                    &theme,
                );
            }
            if !rail_shown && scrollbar_cfg.enabled && sb_output.scroll_info.is_some() {
                self.hit_scrollbar.set(Some(Rect {
                    x: layout.scrollbar_x,
                    y: layout.scrollback.y,
                    width: 1,
                    height: layout.scrollback.height,
                }));
            } else {
                self.hit_scrollbar.clear();
            }
            if let Some(ref rail) = self.timeline_rail {
                crate::views::timeline::render_rail(buf, rail, self.timeline_hover, &theme);
                if let Some(crate::views::timeline::TimelineHit::Tick(turn_idx)) =
                    self.timeline_hover
                {
                    let preview = self
                        .timeline_hover_preview
                        .as_ref()
                        .filter(|(idx, _)| *idx == turn_idx)
                        .map(|(_, text)| text.as_str());
                    if let Some(preview) = preview {
                        crate::views::timeline::render_tick_hover_popup(
                            buf,
                            rail,
                            layout.scrollback,
                            turn_idx,
                            preview,
                            &theme,
                        );
                    }
                }
            }
        }
        let mut follow_indicator_y: Option<u16> = None;
        let mut response_top_indicator_y: Option<u16> = None;
        if self.block_viewer.is_none() && !search_active {
            use crate::appearance::FollowIndicator;
            let gap_y = layout.scrollback.y + layout.scrollback.height;
            let gap_x = layout.scrollback.x;
            let gap_w = layout.scrollback.width;
            let mut content_line_y: Option<u16> = None;
            if appearance.scrollback.display.line_under_last_entry && !self.scrollback.is_empty() {
                let (scroll_offset, _, total_height) = self.scrollback.scroll_info();
                let content_end_screen = u16::try_from(
                    layout.scrollback.y as usize + total_height.saturating_sub(scroll_offset),
                )
                .unwrap_or(u16::MAX);
                if content_end_screen <= gap_y && content_end_screen >= layout.scrollback.y {
                    let line_y = content_end_screen;
                    content_line_y = Some(line_y);
                    let line_x = gap_x + 3;
                    let line_end = (gap_x + gap_w).saturating_sub(2);
                    let line_style = ratatui::style::Style::default().fg(theme.bg_light);
                    for x in line_x..line_end {
                        if let Some(cell) = buf.cell_mut((x, line_y)) {
                            cell.set_symbol("╌");
                            cell.set_style(line_style);
                        }
                    }
                }
            }
            if appearance.scrollback.scroll.follow_indicator != FollowIndicator::None {
                if !self.scrollback.is_follow_mode()
                    && self.scrollback.has_content_below()
                    && content_line_y.is_none()
                {
                    follow_indicator_y = Some(gap_y);
                }
                if self.scrollback.has_response_top_above() {
                    response_top_indicator_y = sticky_gap_row.map(|row| layout.scrollback.y + row);
                }
            }
        }
        let indicator_center_x = layout.scrollback.x + layout.scrollback.width / 2;
        draw_scroll_arrow(
            buf,
            &theme,
            indicator_center_x,
            follow_indicator_y,
            "▼",
            &mut self.hit_follow_indicator,
        );
        draw_scroll_arrow(
            buf,
            &theme,
            indicator_center_x,
            response_top_indicator_y,
            "▲",
            &mut self.hit_response_top_indicator,
        );
        if let Some(msg) = self.active_toast_message() {
            let sb = layout.scrollback;
            if let Some(toast_text) = fit_toast_text(msg, sb.width) {
                let w = toast_text.chars().count() as u16;
                if sb.height > 0 {
                    let x = sb.right().saturating_sub(w + 1);
                    let y = sb.bottom().saturating_sub(1);
                    for (i, ch) in toast_text.chars().enumerate() {
                        if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
                            cell.set_char(ch);
                            cell.fg = theme.accent_user;
                            cell.bg = theme.bg_base;
                            cell.modifier = ratatui::prelude::Modifier::BOLD;
                        }
                    }
                    self.frame_occluder_rects.push(Rect {
                        x,
                        y,
                        width: w,
                        height: 1,
                    });
                }
            }
        }
        if tasks_height > 0 {
            let bg_focused = self.active_pane == ActivePane::Tasks && !overlay_focused;
            self.tasks.render(
                layout.tasks,
                buf,
                bg_focused,
                layout_cfg,
                &self.session.bg_tasks,
                &self.subagent_sessions,
                &self.session.scheduled_tasks,
            );
            let close_rect = agent::render_todo_chrome(
                buf,
                layout.tasks,
                layout_cfg,
                bg_focused,
                false,
                self.hit_bg_close.hovered,
                &theme,
            )
            .and_then(|sel| sel.close_button_rect());
            self.hit_bg_close.set(close_rect);
        }
        if catalog_height > 0 {
            let cat_focused = self.active_pane == ActivePane::Catalog && !overlay_focused;
            self.catalog
                .render(layout.catalog, buf, cat_focused, layout_cfg);
            let close_rect = agent::render_todo_chrome(
                buf,
                layout.catalog,
                layout_cfg,
                cat_focused,
                false,
                self.hit_catalog_close.hovered,
                &theme,
            )
            .and_then(|sel| sel.close_button_rect());
            self.hit_catalog_close.set(close_rect);
        } else {
            self.hit_catalog_close.clear();
        }
        if todo_height > 0 {
            let todo_focused = self.active_pane == ActivePane::Todo && !overlay_focused;
            self.todo.render(layout.todo, buf, todo_focused, layout_cfg);
            let close_rect = agent::render_todo_chrome(
                buf,
                layout.todo,
                layout_cfg,
                todo_focused,
                false,
                self.hit_todo_close.hovered,
                &theme,
            )
            .and_then(|sel| sel.close_button_rect());
            self.hit_todo_close.set(close_rect);
        } else {
            self.hit_todo_close.clear();
        }
        if queue_height > 0 {
            let queue_focused = self.active_pane == ActivePane::Queue && !overlay_focused;
            self.queue.render(
                layout.queue,
                buf,
                queue_focused,
                layout_cfg,
                Some(layout.scrollback),
                self.session.state.is_turn_running(),
            );
            let close_rect = agent::render_todo_chrome_with_close_label(
                buf,
                layout.queue,
                layout_cfg,
                queue_focused,
                false,
                self.hit_queue_close.hovered,
                &theme,
                Some(crate::glyphs::ballot_x_button()),
            )
            .and_then(|sel| sel.close_button_rect());
            self.hit_queue_close.set(close_rect);
        } else {
            self.hit_queue_close.clear();
        }
        self.last_btw_selection_model = ResolvedSelectionModel::default();
        self.last_btw_area = Rect::default();
        if btw_height > 0
            && let Some(ref btw) = self.btw_state
        {
            let tick = self.scrollback.animation_tick();
            let mut btw_links = crate::render::osc8::LinkOverlay::new();
            crate::views::btw_overlay::render_btw_panel(
                buf,
                btw,
                layout.btw,
                tick,
                self.btw_focused && self.active_pane == AgentPane::Prompt,
                Some(&mut self.hit_btw_close),
                &mut self.last_btw_selection_model,
                Some(&mut btw_links),
                &self.media_link_paths,
                self.scrollback.cwd(),
            );
            self.last_btw_area = layout.btw;
            if !btw_links.is_empty() {
                self.last_link_overlay.extend_from(&btw_links);
                self.visible_link_map.append_from_overlay(&btw_links);
            }
            let sb_n = self.scrollback_visible_link_count;
            let total_links = self.visible_link_map.len();
            self.paint_link_highlights(buf, link_active_style, sb_n..total_links);
            self.reclamp_drag_head_post_render(true);
            if let Some(ref drag) = self.drag_selection
                && drag.anchor.entry_idx == BTW_OVERLAY_ENTRY_IDX
            {
                render_active_selection_overlay(&self.last_btw_selection_model, drag, None, buf);
            } else if let Some(ref sel) = self.persistent_text_selection
                && sel.entry_idx == BTW_OVERLAY_ENTRY_IDX
            {
                render_persistent_selection_overlay(&self.last_btw_selection_model, sel, None, buf);
            }
        } else {
            self.hit_btw_close.clear();
        }
        if let Some(idx) = self.highlighted_link_idx {
            if self.visible_link_map.is_empty() {
                self.highlighted_link_idx = None;
            } else if idx >= self.visible_link_map.links().len() {
                self.highlighted_link_idx = Some(self.visible_link_map.links().len() - 1);
            }
        }
        if let Some(idx) = self.hovered_link_idx
            && (self.visible_link_map.is_empty() || idx >= self.visible_link_map.links().len())
        {
            self.hovered_link_idx = None;
        }
        if turn_status_height > 0 {
            let pad_left = HorizontalLayout::ACCENT + layout_cfg.block_pad_left.saturating_sub(1);
            let turn_area = Rect {
                x: layout.turn_status.x + pad_left,
                y: layout.turn_status.y,
                width: layout.turn_status.width.saturating_sub(pad_left),
                height: layout.turn_status.height,
            };
            let tick = self.scrollback.animation_tick();
            let activity = self.resolve_turn_activity();
            if crate::acp::tracker::is_phase_transition(
                self.last_activity.as_ref(),
                activity.as_ref(),
            ) {
                if let Some(prev) = &self.last_activity {
                    let phase_ms = self
                        .activity_started_at
                        .map(|t| t.elapsed().as_millis() as u64)
                        .unwrap_or(0);
                    let prev_label = prev.as_label();
                    let next_label = activity.as_ref().map(|a| a.as_label()).unwrap_or("idle");
                    let sid = self.session.session_id.as_ref().map(|s| s.0.as_ref());
                    crate::unified_log::debug(
                        "turn.phase_transition",
                        sid,
                        Some(serde_json::json!({
                            "from": prev_label,
                            "to": next_label,
                            "phase_elapsed_ms": phase_ms,
                        })),
                    );
                }
                self.activity_started_at = Some(Instant::now());
            }
            if activity != self.last_activity {
                self.last_activity = activity.clone();
            }
            self.hit_plan_approval_status.clear();
            if let Some(ref pav) = self.plan_approval_view {
                let diamond_color = crate::views::turn_status::pending_diamond_color(
                    &theme,
                    theme.accent_plan,
                    tick,
                );
                let text_style = if self.hit_plan_approval_status.hovered {
                    Style::default()
                        .fg(theme.text_primary)
                        .add_modifier(ratatui::style::Modifier::UNDERLINED)
                } else {
                    Style::default().fg(theme.gray)
                };
                let status_label =
                    crate::views::plan_approval_view::plan_approval_status_label(pav.has_plan);
                let spans = vec![
                    Span::styled(
                        format!("{} ", crate::glyphs::diamond_filled()),
                        Style::default().fg(diamond_color),
                    ),
                    Span::styled(status_label, text_style),
                ];
                buf.set_line_safe(
                    turn_area.x,
                    turn_area.y,
                    &Line::from(spans),
                    turn_area.width,
                );
                let item_width: u16 = 2u16.saturating_add(status_label.len() as u16);
                self.hit_plan_approval_status.rect = Some(Rect::new(
                    turn_area.x,
                    turn_area.y,
                    item_width.min(turn_area.width),
                    1,
                ));
                self.hit_cancel_button.rect = None;
                self.hit_bg_button.rect = None;
                self.hit_watching_cue.rect = None;
            } else {
                let has_running_execute = !self.is_subagent_view
                    && wake_display_state.is_none()
                    && self
                        .session
                        .tracker
                        .running_execute_tool_call_id()
                        .is_some();
                let is_pending_user_input = matches!(
                    self.blocking_card(),
                    Some(
                        BlockingCard::Permission
                            | BlockingCard::Question
                            | BlockingCard::McpElicitation
                    )
                );
                let goal_verifying = self
                    .goal_state
                    .as_ref()
                    .is_some_and(|g| g.verifying_completion);
                let held_queue = self.held_queue_count();
                let held_queue_top_sendable = self.held_queue_top_sendable();
                let turn_output = turn_status::render_turn_status(
                    buf,
                    turn_area,
                    turn_status::TurnStatusArgs {
                        state: wake_display_state.unwrap_or(&self.session.state),
                        activity: &activity,
                        turn_elapsed: self.turn_elapsed(),
                        activity_started_at: self.activity_started_at,
                        tick,
                        drain_blocked,
                        buttons: Some(turn_status::MouseButtons {
                            cancel_hovered: self.hit_cancel_button.hovered,
                            bg_hovered: self.hit_bg_button.hovered,
                            watching_hovered: self.hit_watching_cue.hovered,
                        }),
                        has_running_execute,
                        total_tokens: self.context_state.as_ref().map(|c| c.used),
                        mcp_init_progress: self.mcp_init_progress.as_ref(),
                        is_bash_turn: self.bash_turn,
                        is_pending_user_input,
                        goal_verifying,
                        watchers,
                        parked,
                        flat_background: false,
                        held_queue,
                        held_queue_top_sendable,
                    },
                );
                self.hit_cancel_button
                    .set_unless_dropdown(turn_output.cancel_button, dropdown_open);
                self.hit_bg_button
                    .set_unless_dropdown(turn_output.bg_button, dropdown_open);
                self.hit_watching_cue
                    .set_unless_dropdown(turn_output.watching_cue, dropdown_open);
            }
        } else {
            self.hit_cancel_button.clear();
            self.hit_bg_button.clear();
            self.hit_watching_cue.clear();
            self.hit_plan_approval_status.clear();
        }
        let privacy_banner_owns_slot =
            privacy_banner && layout.banner.height >= crate::views::privacy_banner::MIN_HEIGHT;
        if !privacy_banner_owns_slot {
            self.privacy_banner.clear_hits();
        }
        if privacy_banner_owns_slot {
            self.hit_announcement_hide.clear();
            self.hit_announcement_cta.clear();
            let rects = crate::views::privacy_banner::render(layout.banner, buf, &theme, mouse_pos);
            self.privacy_banner
                .hit_opt_in
                .set_unless_dropdown(Some(rects.opt_in), dropdown_open);
            self.privacy_banner
                .hit_opt_out
                .set_unless_dropdown(Some(rects.opt_out), dropdown_open);
            self.privacy_banner
                .hit_terms
                .set_unless_dropdown(Some(rects.terms), dropdown_open);
            self.privacy_banner
                .hit_policy
                .set_unless_dropdown(Some(rects.policy), dropdown_open);
        } else if let Some((ref msg, remaining)) = self.mode_switch_banner {
            self.hit_announcement_hide.clear();
            self.hit_announcement_cta.clear();
            if layout.banner.height > 0 && layout.banner.width > 4 {
                let bg = theme.bg_base;
                for col in 0..layout.banner.width {
                    if let Some(cell) = buf.cell_mut((layout.banner.x + col, layout.banner.y)) {
                        cell.set_char(' ');
                        cell.fg = bg;
                        cell.bg = bg;
                    }
                }
                let opacity = if remaining > MODE_BANNER_FADE_TICKS {
                    1.0
                } else {
                    remaining as f32 / MODE_BANNER_FADE_TICKS as f32
                };
                let base_fg = theme.text_secondary;
                let fg = crate::render::color::blend_color(theme.bg_base, base_fg, opacity)
                    .unwrap_or(base_fg);
                let text = format!("  {}", msg);
                let maxw = layout.banner.width.saturating_sub(2) as usize;
                let display: String = if text.len() > maxw {
                    text.chars()
                        .take(maxw.saturating_sub(1))
                        .collect::<String>()
                        + "…"
                } else {
                    text
                };
                let x = layout.banner.x;
                let y = layout.banner.y;
                for (i, ch) in display.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
                        cell.set_char(ch);
                        cell.fg = fg;
                        cell.bg = bg;
                    }
                }
            }
        } else {
            let announcement_banner_owns_slot =
                self.session_banner_active && layout.banner.height > 0;
            let banner_hits = crate::views::announcements::render_banner(
                layout.banner,
                buf,
                banner_announcements,
                hidden_announcement_ids,
                self.hit_announcement_hide.hovered,
                self.hit_announcement_cta.hovered,
                self.permission_queue.is_empty(),
            );
            self.hit_announcement_hide
                .set_unless_dropdown(banner_hits.hide, dropdown_open);
            self.hit_announcement_cta
                .set_unless_dropdown(banner_hits.cta, dropdown_open);
            if !announcement_banner_owns_slot
                && banner_height > 0
                && let Some(tip_text) = tip
            {
                crate::tips::render::render_tip(
                    layout.banner,
                    buf,
                    tip_text,
                    crate::tips::render::HINT_INSET,
                );
            }
            if !announcement_banner_owns_slot
                && tip_row_visible
                && let Some(line) = self.ephemeral_tip.line()
            {
                crate::tips::render::render_ephemeral_tip(layout.banner, buf, line);
            }
        }
        self.draw_plugin_cta(buf, layout.plugin_cta, &theme);
        if voice_listening && layout.voice_recording.height > 0 && layout.voice_recording.width > 0
        {
            let rec_area = layout.voice_recording;
            let bg = theme.bg_base;
            for col in 0..rec_area.width {
                if let Some(cell) = buf.cell_mut((rec_area.x + col, rec_area.y)) {
                    cell.set_char(' ');
                    cell.fg = bg;
                    cell.bg = bg;
                }
            }
            let content_x = rec_area.x + layout_cfg.block_pad_left;
            let (filled, brightness) = record_dot_pulse();
            let dot = crate::glyphs::record_dot(filled);
            let dot_color = crate::render::color::blend_color(bg, theme.accent_error, brightness)
                .unwrap_or(theme.accent_error);
            buf.set_string(
                content_x,
                rec_area.y,
                dot,
                Style::default().fg(dot_color).bg(bg),
            );
            buf.set_string(
                content_x + 2,
                rec_area.y,
                "Recording",
                Style::default().fg(theme.accent_error).bg(bg),
            );
            let stop_str = "[stop]";
            let stop_w = unicode_width::UnicodeWidthStr::width(stop_str) as u16;
            let stop_x = rec_area.x
                + rec_area
                    .width
                    .saturating_sub(layout_cfg.block_pad_right + stop_w);
            let stop_fg = if self.hit_voice_stop_button.hovered {
                theme.accent_error
            } else {
                theme.gray
            };
            buf.set_string(
                stop_x,
                rec_area.y,
                stop_str,
                Style::default().fg(stop_fg).bg(bg),
            );
            self.hit_voice_stop_button.rect = Some(Rect::new(stop_x, rec_area.y, stop_w, 1));
        } else {
            self.hit_voice_stop_button.clear();
        }
        self.follow_up_chips = match self.follow_ups.as_ref() {
            Some(fu) => agent::render_follow_ups(
                layout.follow_ups,
                buf,
                &theme,
                &fu.suggestions,
                self.hovered_follow_up_chip,
            ),
            None => {
                self.hovered_follow_up_chip = None;
                Vec::new()
            }
        };
        let editing_label;
        let commenting_label;
        let theme = Theme::current();
        let mut mode_flags_vec: Vec<PromptFlag> = Vec::new();
        let approval_is_commenting = self
            .plan_approval_view
            .as_ref()
            .is_some_and(|pav| pav.focus == PlanApprovalFocus::Commenting);
        if effective_plan || casual_commenting {
            let commenting_range: Option<&std::ops::Range<usize>> = if approval_is_commenting {
                self.plan_approval_view
                    .as_ref()
                    .and_then(|pav| pav.commenting_range.as_ref())
            } else if casual_commenting {
                self.casual_commenting_range.as_ref()
            } else {
                None
            };
            let plan_label: &str = if approval_is_commenting || casual_commenting {
                commenting_label = match commenting_range {
                    Some(r) if r.len() == 1 => format!("commenting L{}", r.start),
                    Some(r) => format!("commenting L{}-{}", r.start, r.end - 1),
                    None => "commenting".to_string(),
                };
                commenting_label.as_str()
            } else if self.plan_approval_view.is_some() {
                "plan approval"
            } else {
                "plan"
            };
            mode_flags_vec.push(PromptFlag {
                text: plan_label,
                color: Some(theme.accent_plan),
                bold: false,
            });
        }
        if self.session.is_yolo() && !effective_plan {
            mode_flags_vec.push(PromptFlag {
                text: "always-approve",
                color: None,
                bold: false,
            });
        }
        if self.auto_flag_visible(effective_plan) {
            mode_flags_vec.push(PromptFlag {
                text: "auto",
                color: Some(theme.accent_system),
                bold: false,
            });
        }
        let mode_flags: &[PromptFlag] = &mode_flags_vec;
        let multiline = self.multiline_mode;
        let warning = self.credit_balance.as_ref().and_then(|bal| {
            crate::views::credit_bar::usage_warning_for_session(
                bal,
                self.auto_topup.as_ref(),
                self.billing_surface_visible,
                self.chat_kind,
            )
        });
        let usage_warning_text: Option<String> = warning.as_ref().map(|(t, _)| t.clone());
        let usage_warning = usage_warning_text.as_deref();
        let usage_warning_critical = warning.is_some_and(|(_, critical)| critical);
        let model_label = match self.session.models.reasoning_effort {
            Some(eff) => format!("{model_id} ({eff})"),
            None => model_id,
        };
        let info = match &self.prompt_mode {
            PromptMode::Normal => PromptInfo {
                model_name: &model_label,
                flags: mode_flags,
                multiline,
                usage_warning,
                usage_warning_critical,
            },
            PromptMode::EditingQueued { id, .. } => {
                let pos = self.session.queue_position(*id).map(|i| i + 1).unwrap_or(1);
                editing_label = format!("editing queued #{pos}");
                PromptInfo {
                    model_name: &editing_label,
                    flags: mode_flags,
                    multiline,
                    usage_warning,
                    usage_warning_critical,
                }
            }
        };
        let info = if let Some(label) = self.prompt_input_mode.prompt_info_override() {
            PromptInfo {
                model_name: label,
                flags: &[],
                multiline: false,
                usage_warning,
                usage_warning_critical,
            }
        } else {
            info
        };
        let mut prompt_cursor_pos: Option<(u16, u16)> = None;
        let mut prompt_post_flush: Option<crate::terminal::overlay::PostFlush> = None;
        if permission_view_h > 0 {
            let perm_area = layout.prompt;
            if let Some(perm) = self.permission_queue.front() {
                let followup_text = self.prompt.text();
                let render_result = crate::views::permission_view::render_permission_view(
                    buf,
                    perm_area,
                    perm,
                    followup_text,
                    self.permission_pattern_edit.as_ref(),
                    self.hovered_permission_item,
                    &theme,
                    prompt_focused,
                );
                if let Some(ref iarea) = render_result.inline_prompt {
                    let row_bg = theme.bg_visual;
                    let remaining_h = (perm_area.y + perm_area.height).saturating_sub(iarea.y);
                    let perm_followup_style = PromptStyle {
                        focused: true,
                        show_prefix: false,
                        vpad_top: 0,
                        chrome: false,
                        chrome_pad_left: 0,
                        chrome_pad_right: 0,
                        bg: PromptBg::Panel(row_bg),
                        accent_color_override: None,
                        border_color_override: None,
                        prefix_override: None,
                        placeholder_when_focused: false,
                        placeholder_override: None,
                        compact: false,
                        show_accent_line: false,
                        show_borders: false,
                        title: None,
                        image_preview: true,
                    };
                    let prompt_h = remaining_h.saturating_sub(1).max(1);
                    let prompt_draw_area = Rect {
                        x: iarea.text_x,
                        y: iarea.y,
                        width: iarea.text_w,
                        height: prompt_h,
                    };
                    let prompt_result_inner = self.prompt.draw(
                        buf,
                        prompt_draw_area,
                        None,
                        &perm_followup_style,
                        None,
                        None,
                    );
                    if let Some(pos) = prompt_result_inner.cursor_pos {
                        prompt_cursor_pos = Some(pos);
                    }
                    let accent_color = theme.accent_user;
                    let track_x = prompt_draw_area.x + prompt_draw_area.width.saturating_sub(1);
                    let accent_end = iarea.y.saturating_add(prompt_h);
                    for extra_y in (iarea.y + 1)..accent_end {
                        buf.set_style(
                            Rect {
                                x: layout.prompt.x + 1,
                                y: extra_y,
                                width: track_x.saturating_sub(layout.prompt.x + 1),
                                height: 1,
                            },
                            Style::default().bg(row_bg),
                        );
                        if let Some(cell) = buf.cell_mut((layout.prompt.x, extra_y)) {
                            cell.set_symbol(crate::glyphs::accent_bar());
                            cell.set_style(Style::default().fg(accent_color).bg(row_bg));
                        }
                    }
                    let track_bg = Style::default().bg(theme.bg_dark);
                    for track_y in iarea.y..accent_end {
                        if let Some(cell) = buf.cell_mut((track_x, track_y))
                            && cell.symbol() == " "
                        {
                            cell.set_style(track_bg);
                        }
                    }
                }
            }
        } else if question_view_h > 0 {
            let question_accent = if effective_plan {
                theme.accent_plan
            } else {
                theme.accent_user
            };
            let is_input_mode = self
                .question_view
                .as_ref()
                .map(|qv| qv.focus == crate::views::question_view::QuestionFocus::InputMode)
                .unwrap_or(false);
            let inline_prompt_h: u16 = if is_input_mode {
                question_prompt_body_h.max(1)
            } else {
                0
            };
            let question_area = Rect {
                x: layout.prompt.x,
                y: layout.prompt.y,
                width: layout.prompt.width,
                height: layout
                    .prompt
                    .height
                    .saturating_sub(inline_prompt_h)
                    .saturating_sub(question_footer_h),
            };
            if let Some(ref mut qv) = self.question_view {
                let content_w = inner_width.saturating_sub(QUESTION_VIEW_HPAD) as usize;
                if let Some(question) = qv.questions.get(qv.active_tab) {
                    let visible_options_h = crate::views::question_view::visible_options_height(
                        question,
                        question_area.height,
                        content_w,
                        qv.focused_preview(),
                        qv.fullscreen,
                        qv.cached_desc_cap,
                        qv.cached_preview_cap,
                    );
                    qv.clamp_scroll(visible_options_h, content_w);
                }
            }
            if let Some(ref qv) = self.question_view {
                let render_result = crate::views::question_view::render_question_view(
                    buf,
                    question_area,
                    qv,
                    self.hovered_question_item,
                    &theme,
                    prompt_focused,
                );
                self.question_scroll_region =
                    Some((render_result.options_start_y, render_result.options_end_y));
            }
            let mut painted_prompt_h = inline_prompt_h;
            if is_input_mode && feedback_pane {
                let box_y = question_area.y + question_area.height;
                let below_card = (layout.prompt.y + layout.prompt.height).saturating_sub(box_y);
                let box_h = inline_prompt_h
                    .min(below_card.saturating_sub(question_footer_h))
                    .max(below_card.min(1));
                let input_area = Rect {
                    x: layout.prompt.x + 3,
                    y: box_y,
                    width: feedback_input::width(layout.prompt.width),
                    height: box_h,
                };
                buf.set_style(
                    Rect {
                        x: layout.prompt.x + 1,
                        y: box_y,
                        width: layout.prompt.width.saturating_sub(1),
                        height: box_h,
                    },
                    Style::default().bg(theme.bg_light),
                );
                let outlined = box_h >= feedback_input::MIN_HEIGHT;
                let style = if outlined {
                    feedback_input::style(&theme)
                } else {
                    feedback_input::flat_style(&theme)
                };
                let result = self.prompt.draw(
                    buf,
                    input_area,
                    Some(layout.scrollback),
                    &style,
                    outlined.then_some(&PromptInfo::default()),
                    None,
                );
                prompt_cursor_pos = result.cursor_pos;
                self.inline_prompt_area = Some(input_area);
                painted_prompt_h = box_h;
            } else if is_input_mode && inline_prompt_h > 0 {
                let row_y = question_area.y + question_area.height;
                let content_x = layout.prompt.x + 3;
                let content_w = layout.prompt.width.saturating_sub(3);
                let row_bg = theme.bg_visual;
                buf.set_style(
                    Rect {
                        x: content_x,
                        y: row_y,
                        width: content_w,
                        height: 1,
                    },
                    Style::default().bg(row_bg),
                );
                let prefix_w = self
                    .question_view
                    .as_ref()
                    .and_then(|qv| qv.questions.get(qv.active_tab))
                    .map(crate::views::question_view::option_prefix_w)
                    .unwrap_or(6) as u16;
                let num_style = Style::default().fg(theme.accent_user).bg(row_bg);
                let marker_style = Style::default()
                    .fg(theme.text_primary)
                    .bg(row_bg)
                    .add_modifier(ratatui::style::Modifier::BOLD);
                let prompt_ind = Style::default().fg(theme.accent_user).bg(row_bg);
                buf.set_span_safe(content_x, row_y, &Span::styled("z ", num_style), 2);
                let is_multi = self
                    .question_view
                    .as_ref()
                    .and_then(|qv| qv.questions.get(qv.active_tab))
                    .and_then(|q| q.multi_select)
                    .unwrap_or(false);
                let freeform_sel = self
                    .question_view
                    .as_ref()
                    .and_then(|qv| {
                        qv.per_question_freeform_selected
                            .get(qv.active_tab)
                            .copied()
                    })
                    .unwrap_or(false);
                let (marker_text, actual_marker_style) = if is_multi {
                    if freeform_sel {
                        ("[x] ".to_string(), marker_style)
                    } else {
                        (
                            "[ ] ".to_string(),
                            Style::default().fg(theme.gray).bg(row_bg),
                        )
                    }
                } else if freeform_sel {
                    (format!("({}) ", crate::glyphs::filled_dot()), marker_style)
                } else {
                    (
                        "(\u{25cb}) ".to_string(),
                        Style::default().fg(theme.gray).bg(row_bg),
                    )
                };
                buf.set_span_safe(
                    content_x + 2,
                    row_y,
                    &Span::styled(marker_text, actual_marker_style),
                    4,
                );
                buf.set_span_safe(
                    content_x + prefix_w,
                    row_y,
                    &Span::styled(crate::glyphs::prompt_arrow(), prompt_ind),
                    2,
                );
                let text_x = content_x + prefix_w + 2;
                let text_w = content_w.saturating_sub(prefix_w + 2);
                let text_style = PromptStyle {
                    show_prefix: false,
                    ..question_input_style.clone()
                };
                let prompt_draw_area = Rect {
                    x: text_x,
                    y: row_y,
                    width: text_w,
                    height: inline_prompt_h,
                };
                let prompt_result_inner = self.prompt.draw(
                    buf,
                    prompt_draw_area,
                    Some(layout.scrollback),
                    &text_style,
                    None,
                    None,
                );
                prompt_cursor_pos = prompt_result_inner.cursor_pos;
                self.inline_prompt_area = Some(Rect {
                    x: layout.prompt.x,
                    y: row_y,
                    width: layout.prompt.width,
                    height: inline_prompt_h,
                });
                for y in row_y..row_y.saturating_add(inline_prompt_h) {
                    if y > row_y {
                        buf.set_style(
                            Rect {
                                x: content_x,
                                y,
                                width: prefix_w + 2,
                                height: 1,
                            },
                            Style::default().bg(row_bg),
                        );
                    }
                    for col in (layout.prompt.x + 1)..(layout.prompt.x + 3) {
                        if let Some(cell) = buf.cell_mut((col, y)) {
                            cell.set_char(' ');
                            cell.set_style(Style::default().bg(row_bg));
                        }
                    }
                    if let Some(cell) = buf.cell_mut((layout.prompt.x, y)) {
                        cell.set_symbol(crate::glyphs::accent_bar());
                        cell.set_style(Style::default().fg(question_accent).bg(row_bg));
                    }
                }
            }
            if !is_input_mode {
                self.inline_prompt_area = None;
            }
            if let Some(ref qv) = self.question_view {
                let footer_y = question_area.y + question_area.height + painted_prompt_h + 1;
                let footer_x = layout.prompt.x;
                let footer_w = layout.prompt.width;
                self.question_nav_buttons.clear();
                if footer_y < layout.prompt.y + layout.prompt.height && footer_w > 10 {
                    use ratatui::style::Modifier;
                    let footer_bg = theme.bg_light;
                    let gap_above = footer_y.saturating_sub(1);
                    if gap_above >= question_area.y + question_area.height + painted_prompt_h {
                        buf.set_style(
                            Rect {
                                x: footer_x,
                                y: gap_above,
                                width: footer_w,
                                height: 1,
                            },
                            Style::default().bg(footer_bg),
                        );
                    }
                    let gap_below = footer_y + 1;
                    if gap_below < layout.prompt.y + layout.prompt.height {
                        buf.set_style(
                            Rect {
                                x: footer_x,
                                y: gap_below,
                                width: footer_w,
                                height: 1,
                            },
                            Style::default().bg(footer_bg),
                        );
                    }
                    let footer_rect = Rect {
                        x: footer_x,
                        y: footer_y,
                        width: footer_w,
                        height: 1,
                    };
                    buf.set_style(footer_rect, Style::default().bg(footer_bg));
                    let content_x = layout.prompt.x + 3;
                    let hint_style = Style::default()
                        .fg(theme.gray)
                        .bg(footer_bg)
                        .add_modifier(Modifier::BOLD);
                    let hint_key = Style::default()
                        .fg(question_accent)
                        .bg(footer_bg)
                        .add_modifier(Modifier::BOLD);
                    let left_spans =
                        Self::question_footer_hints(qv, feedback_pane, hint_style, hint_key);
                    let left_line = Line::from(left_spans);
                    let avail_w = footer_w.saturating_sub(3);
                    buf.set_line_safe(content_x, footer_y, &left_line, avail_w);
                    let is_last = qv.active_tab >= qv.questions.len().saturating_sub(1);
                    let enter_label = if feedback_pane || qv.is_feedback_trace() {
                        "send"
                    } else if qv.is_on_freeform_row() {
                        "edit"
                    } else if is_last {
                        "submit"
                    } else {
                        "select"
                    };
                    let btn_key = "Enter";
                    let btn_bg = theme.bg_base;
                    let bkey_style = Style::default()
                        .fg(question_accent)
                        .bg(btn_bg)
                        .add_modifier(Modifier::BOLD);
                    let blabel_style = Style::default().fg(theme.gray).bg(btn_bg);
                    let bpad_style = Style::default().bg(btn_bg);
                    let bw = (1 + btn_key.len() + 1 + enter_label.len() + 1) as u16;
                    let btn_x = footer_x + footer_w.saturating_sub(3).saturating_sub(bw);
                    if btn_x > content_x {
                        buf.set_span_safe(btn_x, footer_y, &Span::styled(" ", bpad_style), 1);
                        buf.set_span_safe(
                            btn_x + 1,
                            footer_y,
                            &Span::styled(btn_key, bkey_style),
                            btn_key.len() as u16,
                        );
                        let cx = btn_x + 1 + btn_key.len() as u16;
                        buf.set_span_safe(cx, footer_y, &Span::styled(":", blabel_style), 1);
                        buf.set_span_safe(
                            cx + 1,
                            footer_y,
                            &Span::styled(enter_label, blabel_style),
                            enter_label.len() as u16,
                        );
                        buf.set_span_safe(
                            cx + 1 + enter_label.len() as u16,
                            footer_y,
                            &Span::styled(" ", bpad_style),
                            1,
                        );
                        let btn_rect = Rect {
                            x: btn_x,
                            y: footer_y,
                            width: bw,
                            height: 1,
                        };
                        self.question_nav_buttons.push(('\n', btn_rect));
                    }
                }
            }
            let accent_style = Style::default().fg(question_accent).bg(theme.bg_light);
            for y in layout.prompt.y..layout.prompt.y + layout.prompt.height {
                if let Some(cell) = buf.cell_mut((layout.prompt.x, y)) {
                    cell.set_symbol(crate::glyphs::accent_bar());
                    cell.set_style(accent_style);
                }
            }
            if let Some(ref qv) = self.question_view {
                let inner_scrollbar_x = layout.prompt.x + layout.prompt.width - 1;
                let scroll_region = self
                    .question_scroll_region
                    .unwrap_or((question_area.y, question_area.y + question_area.height));
                let sb_rect = crate::views::question_view::render_question_scrollbar(
                    buf,
                    inner_scrollbar_x,
                    qv,
                    &theme,
                    scroll_region,
                );
                self.hit_question_scrollbar.set(sb_rect);
            } else {
                self.hit_question_scrollbar.clear();
            }
        } else if elicitation_view_h > 0 {
            self.elicit_hits.clear();
            if let Some(ev) = self.elicitation_view.as_mut() {
                let elicit_area = Rect {
                    x: layout.prompt.x,
                    y: layout.prompt.y,
                    width: layout.prompt.width,
                    height: layout.prompt.height,
                };
                crate::views::elicitation_view::render_elicitation_view(
                    buf,
                    elicit_area,
                    ev,
                    &theme,
                    prompt_focused,
                    Some(&mut self.elicit_hits),
                );
            }
        } else if rewind_view_h > 0 {
            if let Some(ref rw) = self.rewind_state {
                crate::views::rewind::render_rewind_overlay(
                    buf,
                    layout.prompt,
                    &rw.phase,
                    prompt_focused,
                );
            }
        } else if jump_view_h > 0 {
            if let Some(ref js) = self.jump_state {
                crate::views::jump::render_jump_overlay(buf, layout.prompt, js, prompt_focused);
            }
        } else if cancel_turn_view_h > 0 {
            let buttons = &mut self.cancel_turn_buttons;
            if let Some(ctv) = self.cancel_turn_view.as_ref() {
                modal::render_cancel_turn_panel(buf, layout.prompt, ctv, prompt_focused, buttons);
            } else {
                buttons.clear();
            }
        } else {
            let collapsed = !prompt_focused && appearance.prompt.collapse_unfocused;
            let saved_scroll = if collapsed {
                let s = self.prompt.scroll();
                let ovr = self.prompt.textarea.scroll_override();
                self.prompt.textarea.set_scroll_override(Some(0));
                Some((s, ovr))
            } else {
                None
            };
            let voice_overlay = if voice_available && (voice_listening || voice_interim.is_some()) {
                Some(crate::views::prompt_widget::VoicePromptOverlay {
                    interim: voice_interim,
                    color: theme.accent_running,
                })
            } else {
                None
            };
            let prompt_result_inner = self.prompt.draw(
                buf,
                layout.prompt,
                Some(layout.scrollback),
                &prompt_style,
                Some(&info),
                voice_overlay,
            );
            if let Some((s, ovr)) = saved_scroll {
                self.prompt.textarea.set_scroll_override(ovr);
                self.prompt.set_scroll(s);
            }
            prompt_cursor_pos = prompt_result_inner.cursor_pos;
            if let Some(escapes) = prompt_result_inner.post_flush_escapes {
                prompt_post_flush = Some(escapes.into());
            }
        }
        if self.prompt.file_search_visible() {
            use crate::views::file_search::dropdown::{MAX_DROPDOWN_ROWS, render_dropdown};
            let item_count = self.prompt.file_search.result_count();
            let item_rows = (item_count as u16).min(MAX_DROPDOWN_ROWS);
            if item_rows > 0 {
                self.prompt.file_search.ensure_visible(item_rows as usize);
                let bottom_border_y = if let Some(ipa) = self.inline_prompt_area {
                    ipa.y.saturating_sub(1)
                } else {
                    layout.prompt.y.saturating_sub(1)
                };
                let panel_height = item_rows + 2;
                let top_border_y = bottom_border_y.saturating_sub(panel_height - 1);
                let panel_x = area.x + layout_cfg.eff_hpad_left(compact);
                let panel_width = area.width.saturating_sub(
                    layout_cfg.eff_hpad_left(compact) + layout_cfg.eff_hpad_right(compact),
                );
                if top_border_y < bottom_border_y && panel_width > 4 {
                    let panel_area = Rect {
                        x: panel_x,
                        y: top_border_y,
                        width: panel_width,
                        height: panel_height,
                    };
                    ratatui::widgets::Clear.render(panel_area, buf);
                    self.frame_occluder_rects.push(panel_area);
                    buf.set_style(
                        panel_area,
                        Style::default().fg(theme.text_primary).bg(theme.bg_light),
                    );
                    let border_style = Style::default().fg(theme.bg_highlight).bg(theme.bg_base);
                    let border_line = Line::styled("─".repeat(panel_width as usize), border_style);
                    buf.set_line_safe(panel_x, top_border_y, &border_line, panel_width);
                    buf.set_line_safe(panel_x, bottom_border_y, &border_line, panel_width);
                    {
                        let (k, n) = (
                            self.prompt.file_search.result_count(),
                            self.prompt.file_search.total_items(),
                        );
                        let hint = if k >= 1000 {
                            format!("1k+/{n}")
                        } else {
                            format!("{k}/{n}")
                        };
                        let hint_w = hint.len() as u16;
                        if hint_w + 2 <= panel_width {
                            let hint_x = panel_x + panel_width - hint_w - 1;
                            let hint_line = Line::styled(
                                hint,
                                Style::default().fg(theme.gray).bg(theme.bg_base),
                            );
                            buf.set_line_safe(hint_x, top_border_y, &hint_line, hint_w);
                        }
                    }
                    let items_x = layout.prompt.x + dropdown_content_inset();
                    let items_width = dropdown_items_width(layout.prompt);
                    let items_area = Rect {
                        x: items_x,
                        y: top_border_y + 1,
                        width: items_width,
                        height: item_rows,
                    };
                    render_dropdown(buf, items_area, &self.prompt.file_search, &theme);
                    self.dropdown_items_area = Some(items_area);
                } else {
                    self.dropdown_items_area = None;
                }
            } else {
                self.dropdown_items_area = None;
            }
        } else {
            self.dropdown_items_area = None;
        }
        if !self.prompt.file_search_visible() && self.prompt.slash_open() {
            use crate::views::slash_dropdown::{
                desired_item_rows, render_dropdown as render_slash,
            };
            let snap = self.prompt.slash_snapshot();
            let item_count = snap.matches.len();
            let items_width = dropdown_items_width(layout.prompt);
            let item_rows = desired_item_rows(&snap.matches, items_width);
            if item_rows > 0 {
                if let Some(chrome) = render_dropdown_chrome(
                    buf,
                    item_count,
                    item_rows,
                    self.inline_prompt_area,
                    layout.prompt,
                    area,
                    layout_cfg,
                    compact,
                    false,
                    &theme,
                ) {
                    let hovered = self.prompt.slash_hovered();
                    self.slash_dropdown_hit =
                        render_slash(buf, chrome.items, &snap, hovered, &theme);
                    self.slash_dropdown_items_area = Some(chrome.items);
                    self.frame_occluder_rects.push(chrome.panel);
                } else {
                    self.slash_dropdown_items_area = None;
                    self.slash_dropdown_hit = Default::default();
                }
            } else {
                self.slash_dropdown_items_area = None;
                self.slash_dropdown_hit = Default::default();
            }
        } else {
            self.slash_dropdown_items_area = None;
            self.slash_dropdown_hit = Default::default();
        }
        if !self.prompt.slash_open()
            && !self.prompt.file_search_visible()
            && self.prompt.completion_dropdown_open()
        {
            use crate::views::completion_dropdown::{
                MAX_VISIBLE_ROWS, render_dropdown as render_completions,
            };
            let dd = &self.prompt.suggestions.dropdown;
            let item_count = dd.items.len();
            let item_rows = (item_count as u16).min(MAX_VISIBLE_ROWS);
            if item_rows > 0 {
                if let Some(chrome) = render_dropdown_chrome(
                    buf,
                    item_count,
                    item_rows,
                    self.inline_prompt_area,
                    layout.prompt,
                    area,
                    layout_cfg,
                    compact,
                    false,
                    &theme,
                ) {
                    render_completions(buf, chrome.items, dd, &theme);
                    self.completion_dropdown_items_area = Some(chrome.items);
                    self.frame_occluder_rects.push(chrome.panel);
                } else {
                    self.completion_dropdown_items_area = None;
                }
            } else {
                self.completion_dropdown_items_area = None;
            }
        } else {
            self.completion_dropdown_items_area = None;
        }
        if self.prompt.history_search.is_active() {
            use crate::render::scrollbar::render_scrollbar_styled;
            use ratatui::style::Modifier;
            let max_rows: u16 = 8;
            let result_count = self.prompt.history_search.result_count();
            let item_rows = (result_count as u16).min(max_rows).max(1);
            let panel_height = item_rows + 2;
            let bottom_border_y = layout.prompt.y.saturating_sub(1);
            let top_border_y = bottom_border_y.saturating_sub(panel_height - 1);
            let panel_x = area.x + layout_cfg.eff_hpad_left(compact);
            let panel_width = area.width.saturating_sub(
                layout_cfg.eff_hpad_left(compact) + layout_cfg.eff_hpad_right(compact),
            );
            if panel_height >= 3 && top_border_y < bottom_border_y && panel_width > 4 {
                let panel_area = Rect {
                    x: panel_x,
                    y: top_border_y,
                    width: panel_width,
                    height: panel_height,
                };
                ratatui::widgets::Clear.render(panel_area, buf);
                self.frame_occluder_rects.push(panel_area);
                buf.set_style(
                    panel_area,
                    Style::default().fg(theme.text_primary).bg(theme.bg_light),
                );
                let border_style = Style::default().fg(theme.bg_highlight).bg(theme.bg_base);
                let border_line =
                    Line::styled("\u{2500}".repeat(panel_width as usize), border_style);
                buf.set_line_safe(panel_x, top_border_y, &border_line, panel_width);
                buf.set_line_safe(panel_x, bottom_border_y, &border_line, panel_width);
                {
                    let hint = format!("{result_count}");
                    let hint_w = hint.len() as u16;
                    if hint_w + 2 <= panel_width {
                        let hint_x = panel_x + panel_width - hint_w - 1;
                        buf.set_line_safe(
                            hint_x,
                            top_border_y,
                            &Line::styled(hint, Style::default().fg(theme.gray).bg(theme.bg_base)),
                            hint_w,
                        );
                    }
                    let label = " history ";
                    let label_w = label.len() as u16;
                    if label_w + 2 <= panel_width {
                        buf.set_line_safe(
                            panel_x + 1,
                            top_border_y,
                            &Line::styled(label, Style::default().fg(theme.gray).bg(theme.bg_base)),
                            label_w,
                        );
                    }
                }
                let items_x = layout.prompt.x + dropdown_content_inset();
                let items_width = dropdown_items_width(layout.prompt);
                let items_area_rect = Rect {
                    x: items_x,
                    y: top_border_y + 1,
                    width: items_width,
                    height: item_rows,
                };
                self.history_dropdown_area = Some(items_area_rect);
                let hover_bg = theme.bg_hover;
                if result_count == 0 {
                    let msg_y = top_border_y + 1;
                    if msg_y < bottom_border_y {
                        let message = if self.prompt_history_loading() {
                            "  Loading..."
                        } else {
                            "  no matching history"
                        };
                        buf.set_string_safe(
                            items_x,
                            msg_y,
                            message,
                            Style::default().fg(theme.gray).bg(theme.bg_light),
                        );
                    }
                } else {
                    let selected = self.prompt.history_search.selected;
                    let visible_rows = item_rows as usize;
                    let scroll_offset = if selected >= visible_rows {
                        selected - (visible_rows - 1)
                    } else {
                        0
                    };
                    let visible_end = result_count.min(scroll_offset + visible_rows);
                    let needs_scrollbar = result_count > visible_rows;
                    let text_width = if needs_scrollbar {
                        items_width.saturating_sub(2)
                    } else {
                        items_width
                    };
                    for (vi, ri) in (scroll_offset..visible_end).enumerate() {
                        let row_y = top_border_y + 1 + vi as u16;
                        if row_y >= bottom_border_y {
                            break;
                        }
                        let result = match self.prompt.history_search.result_at(ri) {
                            Some(r) => r,
                            None => continue,
                        };
                        let is_selected = ri == selected;
                        let is_hovered =
                            self.prompt.history_search.hovered() == Some(ri) && !is_selected;
                        let row_bg = if is_selected {
                            theme.bg_visual
                        } else if is_hovered {
                            hover_bg
                        } else {
                            theme.bg_light
                        };
                        let bold = if is_selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        };
                        let fill_width = if needs_scrollbar {
                            items_width.saturating_sub(1)
                        } else {
                            items_width
                        };
                        for col in items_x..items_x + fill_width {
                            if let Some(cell) = buf.cell_mut((col, row_y)) {
                                cell.set_char(' ');
                                cell.set_style(Style::default().bg(row_bg));
                            }
                        }
                        let prefix_w: u16 = crate::glyphs::PROMPT_ARROW_WIDTH;
                        let prefix = if is_selected {
                            crate::glyphs::prompt_arrow()
                        } else {
                            "  "
                        };
                        let pfx_style = Style::default()
                            .fg(theme.text_primary)
                            .bg(row_bg)
                            .add_modifier(bold);
                        for (i, ch) in prefix.chars().enumerate() {
                            let px = items_x + i as u16;
                            if px < items_x + text_width
                                && let Some(cell) = buf.cell_mut((px, row_y))
                            {
                                cell.set_char(ch);
                                cell.set_style(if is_selected {
                                    pfx_style
                                } else {
                                    Style::default().bg(row_bg)
                                });
                            }
                        }
                        let match_style = Style::default()
                            .fg(theme.accent_user)
                            .bg(row_bg)
                            .add_modifier(bold);
                        let normal_style = Style::default()
                            .fg(theme.text_primary)
                            .bg(row_bg)
                            .add_modifier(bold);
                        let mut indices = &result.indices[..];
                        let mut col = items_x + prefix_w;
                        let max_col = items_x + prefix_w + text_width.saturating_sub(prefix_w);
                        let preview: String = result
                            .text
                            .chars()
                            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                            .collect();
                        for (ci, ch) in preview.chars().enumerate() {
                            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                            if col + cw > max_col {
                                if col > items_x + prefix_w
                                    && let Some(cell) = buf.cell_mut((col.saturating_sub(1), row_y))
                                {
                                    cell.set_char('\u{2026}');
                                }
                                break;
                            }
                            let is_match = indices.first() == Some(&(ci as u32));
                            if is_match {
                                indices = &indices[1..];
                            }
                            let st = if is_match { match_style } else { normal_style };
                            if let Some(cell) = buf.cell_mut((col, row_y)) {
                                cell.set_symbol(&ch.to_string());
                                cell.set_style(st);
                            }
                            col += cw;
                        }
                    }
                    if needs_scrollbar {
                        let sb_area = Rect {
                            x: items_x + items_width - 1,
                            y: top_border_y + 1,
                            width: 1,
                            height: item_rows,
                        };
                        render_scrollbar_styled(
                            buf,
                            Some(sb_area),
                            result_count as u16,
                            item_rows,
                            scroll_offset as u16,
                            Style::default().bg(theme.bg_dark),
                            Style::default().fg(theme.gray_dim).bg(theme.bg_dark),
                        );
                    }
                }
            }
        } else {
            self.history_dropdown_area = None;
        }
        if self.active_modal.is_some() {
            self.draw_active_modal(area, buf, theme, compact);
            self.pane_areas = layout.pane_areas();
            return (None, crate::terminal::overlay::clear().map(Into::into));
        }
        match self.shortcuts_bar_content(registry, esc_owned_before_agent) {
            ShortcutsBarContent::Hidden => {}
            ShortcutsBarContent::Surface(hints) => {
                ShortcutsBar::new(&hints)
                    .with_pending(pending_hint)
                    .render(layout.shortcuts, buf);
            }
            ShortcutsBarContent::Pane(hints) => {
                let help_hint = registry.find(ActionId::ShortcutsHelp).map(|def| {
                    let mut hint = def.hint();
                    if in_dashboard_overlay
                        && def.default_key == key!('x', CONTROL)
                        && let Some(alt) = def.alt_keys.first()
                    {
                        hint.keys = vec![*alt];
                    }
                    hint
                });
                ShortcutsBar::new(&hints)
                    .compact(5, help_hint)
                    .with_pending(pending_hint)
                    .render(layout.shortcuts, buf);
            }
        }
        let line_viewer_toast = self.active_toast_message().map(|s| s.to_string());
        let is_plan_viewer = self.is_plan_viewer();
        let has_plan_comments = !self.plan_comments.is_empty();
        let casual_commenting = self.is_casual_commenting();
        if self.line_viewer.is_some() {
            use crate::views::file_search::line_viewer::render_line_viewer;
            use crate::views::shortcuts_bar::HintItem;
            let plan_prompt_focused = self
                .plan_approval_view
                .as_ref()
                .is_some_and(|p| p.focus != PlanApprovalFocus::Preview);
            let overlay_bottom = if layout.turn_status.height > 0 {
                layout.turn_status.y
            } else if layout.voice_recording.height > 0 {
                layout.voice_recording.y
            } else {
                layout.prompt.y
            };
            let overlay_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: overlay_bottom.saturating_sub(area.y),
            };
            let approval_comment_count = self
                .plan_approval_view
                .as_ref()
                .map(|pav| pav.comments.len())
                .unwrap_or(0);
            let effective_comment_count = if approval_comment_count > 0 {
                approval_comment_count
            } else {
                self.plan_comments.len()
            };
            let mermaid_placements = self
                .line_viewer
                .as_mut()
                .map(|viewer| {
                    if let Some(ref pav) = self.plan_approval_view {
                        viewer.plan_mut().active_commenting_range = pav.commenting_range.clone();
                    } else {
                        viewer.plan_mut().active_commenting_range =
                            self.casual_commenting_range.clone();
                    }
                    render_line_viewer(
                        buf,
                        overlay_area,
                        viewer,
                        &self.session.cwd,
                        &theme,
                        effective_comment_count,
                    );
                    viewer
                        .last_popup_area
                        .map(|area| viewer.diagram_affordance_placements(area))
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            self.inline_media_hits = super::InlineMediaHitAreas::default();
            self.paint_diagram_affordances(buf, mermaid_placements, &theme);
            let Some(viewer) = self.line_viewer.as_mut() else {
                return (prompt_cursor_pos, prompt_post_flush);
            };
            let toast_area = viewer
                .last_popup_area
                .or(viewer.last_modal_area)
                .unwrap_or(overlay_area);
            if let Some(ref msg) = line_viewer_toast
                && toast_area.height > 0
                && let Some(toast_text) = fit_toast_text(msg, toast_area.width.saturating_sub(1))
            {
                let w = toast_text.chars().count() as u16;
                let tx = toast_area.right().saturating_sub(w + 1);
                let ty = toast_area.bottom().saturating_sub(1);
                for (i, ch) in toast_text.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((tx + i as u16, ty)) {
                        cell.set_char(ch);
                        cell.fg = theme.accent_user;
                        cell.bg = theme.bg_base;
                        cell.modifier = ratatui::prelude::Modifier::BOLD;
                    }
                }
            }
            let in_plan_approval = self.plan_approval_view.is_some();
            let on_comment = in_plan_approval
                && viewer
                    .list_state
                    .selected_index()
                    .and_then(|vi| {
                        let pi = viewer.list_state.to_physical(vi);
                        viewer.lines.get(pi)
                    })
                    .is_some_and(|item| item.comment_id().is_some());
            let approval_has_comments = in_plan_approval
                && self
                    .plan_approval_view
                    .as_ref()
                    .is_some_and(|pav| !pav.comments.is_empty());
            let viewer_hints = if in_plan_approval && on_comment {
                let mut h = vec![
                    HintItem::new(key!(Enter), "edit"),
                    HintItem::new(key!('x'), "delete"),
                ];
                if approval_has_comments {
                    h.push(HintItem::new(key!('s'), "send"));
                } else {
                    h.push(HintItem::new(key!('a'), "approve"));
                }
                h.push(HintItem::new(key!('y'), "copy plan"));
                h.push(HintItem::new(key!('q'), "quit plan"));
                h.push(HintItem::new(key!(Tab), "prompt"));
                h
            } else if in_plan_approval {
                let mut h = vec![
                    HintItem::new(key!('c'), "comment"),
                    HintItem::new(key!('y'), "copy plan"),
                ];
                if approval_has_comments {
                    h.push(HintItem::new(key!('s'), "send"));
                } else {
                    h.push(HintItem::new(key!('a'), "approve"));
                }
                h.push(HintItem::new(key!('q'), "quit plan"));
                if self.vim_mode {
                    h.push(HintItem::paired(key!('j'), key!('k'), "nav"));
                }
                h.push(HintItem::new(key!('v'), "select"));
                h.push(HintItem::new(key!(Tab), "prompt"));
                h
            } else if is_plan_viewer {
                let on_casual_comment = viewer
                    .list_state
                    .selected_index()
                    .and_then(|vi| {
                        let pi = viewer.list_state.to_physical(vi);
                        viewer.lines.get(pi)
                    })
                    .is_some_and(|item| item.comment_id().is_some());
                let mut h = if on_casual_comment {
                    vec![
                        HintItem::new(key!(Enter), "edit"),
                        HintItem::new(key!('x'), "delete"),
                        HintItem::new(key!('y'), "copy plan"),
                    ]
                } else {
                    vec![
                        HintItem::new(key!('c'), "comment"),
                        HintItem::new(key!('y'), "copy plan"),
                    ]
                };
                if has_plan_comments {
                    h.push(HintItem::new(key!('s'), "send"));
                }
                if self.vim_mode {
                    h.push(HintItem::paired(key!('j'), key!('k'), "nav"));
                }
                h.push(HintItem::new(key!('v'), "select"));
                h.push(HintItem::new(key!('f', CONTROL), "fullscreen"));
                h.push(HintItem::new(key!('/'), "search"));
                h.push(HintItem::new(key!(Esc), "close"));
                h
            } else {
                let mut h = vec![HintItem::new(key!(Enter), "confirm")];
                if self.vim_mode {
                    h.push(HintItem::paired(key!('j'), key!('k'), "nav"));
                }
                h.push(HintItem::new(key!('v'), "select"));
                h.push(HintItem::new(key!('x'), "clear"));
                if self.vim_mode {
                    h.push(HintItem::new(key!('y'), "copy"));
                    h.push(HintItem::new(key!('Y'), "filename"));
                }
                h.push(HintItem::new(key!(':'), "goto"));
                h.push(HintItem::new(key!('/'), "search"));
                h.push(HintItem::new(key!(Esc), "cancel"));
                h
            };
            let input_bar_active = viewer.list_state.input_mode().is_some();
            if !(plan_prompt_focused || casual_commenting || viewer.fullscreen && input_bar_active)
            {
                ShortcutsBar::new(&viewer_hints).render(layout.shortcuts, buf);
            }
            self.pane_areas = layout.pane_areas();
            let viewer_cursor = if plan_prompt_focused || self.is_casual_commenting() {
                prompt_cursor_pos
            } else {
                None
            };
            return (viewer_cursor, prompt_post_flush);
        }
        if let Some(ref mut viewer) = self.image_viewer {
            use crate::terminal::image::{GraphicsProtocol, detect_graphics_protocol};
            use crate::views::file_search::line_viewer::dim_area;
            use crate::views::shortcuts_bar::HintItem;
            let overlay_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: layout.shortcuts.y.saturating_sub(area.y),
            };
            let popup_width = ((overlay_area.width as u32 * 90) / 100)
                .max(28)
                .min(overlay_area.width as u32) as u16;
            let popup_height = ((overlay_area.height as u32 * 90) / 100)
                .max(8)
                .min(overlay_area.height as u32) as u16;
            let popup_x = overlay_area.x + (overlay_area.width.saturating_sub(popup_width)) / 2;
            let popup_y = overlay_area.y + (overlay_area.height.saturating_sub(popup_height)) / 2;
            let popup_rect = Rect::new(popup_x, popup_y, popup_width, popup_height);
            let mut image_escape_emitted = false;
            if popup_rect.width >= 10 && popup_rect.height >= 5 {
                dim_area(buf, overlay_area, theme.bg_base, 0.5);
                ratatui::widgets::Clear.render(popup_rect, buf);
                buf.set_style(
                    popup_rect,
                    Style::default().fg(theme.text_primary).bg(theme.bg_base),
                );
                let block = ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(theme.gray_dim))
                    .style(Style::default().bg(theme.bg_base));
                block.render(popup_rect, buf);
                let inner_cols = popup_rect.width.saturating_sub(2);
                let inner_rows = popup_rect.height.saturating_sub(2);
                let title_style = Style::default()
                    .fg(theme.text_primary)
                    .bg(theme.bg_base)
                    .add_modifier(ratatui::style::Modifier::BOLD);
                let dim_style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
                let border_style = Style::default().fg(theme.gray_dim).bg(theme.bg_base);
                let title_spans: Vec<ratatui::text::Span> = if viewer.loading {
                    let name = viewer.title.as_deref().unwrap_or("Loading...");
                    vec![
                        ratatui::text::Span::styled("\u{2500} ", border_style),
                        ratatui::text::Span::styled(name.to_owned(), title_style),
                        ratatui::text::Span::styled(" \u{2500}", border_style),
                    ]
                } else {
                    let name = viewer.title.as_deref().unwrap_or(&viewer.mime_type);
                    let dims = format!(" ({}\u{00d7}{})", viewer.image_width, viewer.image_height);
                    vec![
                        ratatui::text::Span::styled("\u{2500} ", border_style),
                        ratatui::text::Span::styled(name.to_owned(), title_style),
                        ratatui::text::Span::styled(dims, dim_style),
                        ratatui::text::Span::styled(" \u{2500}", border_style),
                    ]
                };
                viewer.modal_state.popup_area = Some(popup_rect);
                crate::views::modal_window::render_close_button(
                    buf,
                    popup_rect,
                    &mut viewer.modal_state,
                    &theme,
                );
                let close_width = viewer
                    .modal_state
                    .close_button_rect
                    .map_or(0, |r| r.width + 1);
                let tx = popup_rect.x + 2;
                let max_title_width = popup_rect.width.saturating_sub(2 + close_width + 1);
                {
                    use unicode_width::UnicodeWidthStr;
                    let mut cursor: u16 = 0;
                    for span in &title_spans {
                        let w = span.content.width() as u16;
                        if cursor + w > max_title_width {
                            break;
                        }
                        buf.set_span_safe(tx + cursor, popup_rect.y, span, w);
                        cursor += w;
                    }
                }
                if viewer.loading {
                    if inner_cols > 0 && inner_rows > 0 {
                        use crate::views::turn_status::SPINNER_DIVISOR;
                        use unicode_width::UnicodeWidthStr;
                        let tick = self.scrollback.animation_tick();
                        let frames = crate::glyphs::braille_spinner_frames();
                        let frame = frames[(tick / SPINNER_DIVISOR) as usize % frames.len()];
                        let loading = format!("{} Loading...", frame);
                        let lw = loading.width() as u16;
                        let lx = popup_rect.x + 1 + inner_cols.saturating_sub(lw) / 2;
                        let ly = popup_rect.y + 1 + inner_rows / 2;
                        buf.set_span_safe(
                            lx,
                            ly,
                            &ratatui::text::Span::styled(
                                loading,
                                Style::default().fg(theme.gray_dim).bg(theme.bg_base),
                            ),
                            lw,
                        );
                    }
                } else {
                    let rendered_pixel = crate::terminal::overlay::static_centered(
                        &viewer.display_bytes,
                        viewer.image_width,
                        viewer.image_height,
                        popup_rect,
                        viewer.overlay_owner_id,
                    )
                    .is_some_and(|esc| {
                        prompt_post_flush = Some(esc.into());
                        image_escape_emitted = true;
                        true
                    });
                    if inner_cols > 0 && inner_rows > 0 {
                        let inner_rect = ratatui::layout::Rect::new(
                            popup_rect.x + 1,
                            popup_rect.y + 1,
                            inner_cols,
                            inner_rows,
                        );
                        if !rendered_pixel {
                            let meta_lines = vec![
                                ratatui::text::Line::from(""),
                                ratatui::text::Line::from(format!(
                                    "  {}x{} {}",
                                    viewer.image_width, viewer.image_height, viewer.mime_type,
                                )),
                                ratatui::text::Line::from(""),
                                ratatui::text::Line::from("  Press Esc to close"),
                            ];
                            ratatui::widgets::Paragraph::new(meta_lines)
                                .style(Style::default().fg(theme.gray_dim).bg(theme.bg_base))
                                .render(inner_rect, buf);
                        } else {
                            let loading = "Loading...";
                            let lw = loading.len() as u16;
                            let lx = inner_rect.x + inner_cols.saturating_sub(lw) / 2;
                            let ly = inner_rect.y + inner_rows / 2;
                            buf.set_span_safe(
                                lx,
                                ly,
                                &ratatui::text::Span::styled(
                                    loading,
                                    Style::default().fg(theme.gray_dim).bg(theme.bg_base),
                                ),
                                lw,
                            );
                        }
                    }
                }
            }
            if !image_escape_emitted && detect_graphics_protocol() == GraphicsProtocol::Kitty {
                let clear = crate::terminal::overlay::clear_kitty();
                prompt_post_flush = Some(clear.into());
            }
            let hints = vec![HintItem::new(key!(Esc), "close")];
            ShortcutsBar::new(&hints).render(layout.shortcuts, buf);
            self.pane_areas = layout.pane_areas();
            return (None, prompt_post_flush);
        }
        if let Some(ref viewer) = self.video_viewer {
            use crate::terminal::image::{GraphicsProtocol, detect_graphics_protocol};
            use crate::views::shortcuts_bar::HintItem;
            let overlay_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: layout.shortcuts.y.saturating_sub(area.y),
            };
            let mut video_escape_emitted = false;
            if let Some(popup_rect) = crate::render::video_overlay::render_video_overlay(
                buf,
                overlay_area,
                viewer,
                theme.bg_base,
                theme.text_primary,
                theme.gray_dim,
            ) {
                if let Some(esc) = crate::terminal::overlay::volatile_centered(
                    viewer.current_frame_data(),
                    viewer.video_width,
                    viewer.video_height,
                    popup_rect,
                ) {
                    prompt_post_flush = Some(esc.into());
                    video_escape_emitted = true;
                }
                if !video_escape_emitted && detect_graphics_protocol() == GraphicsProtocol::Kitty {
                    let clear = crate::terminal::overlay::clear_kitty();
                    prompt_post_flush = Some(clear.into());
                }
            }
            let play_label = if viewer.playing { "pause" } else { "play" };
            let hints = vec![
                HintItem::new(key!(Esc), "close"),
                HintItem::new(key!(' '), play_label),
                HintItem::new(key!(Left), "back"),
                HintItem::new(key!(Right), "fwd"),
            ];
            ShortcutsBar::new(&hints).render(layout.shortcuts, buf);
            self.pane_areas = layout.pane_areas();
            return (None, prompt_post_flush);
        }
        if let Some(gboom) = self.gboom.as_mut() {
            use crate::terminal::image::{GraphicsProtocol, detect_graphics_protocol};
            use crate::views::shortcuts_bar::HintItem;
            let overlay_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: layout.shortcuts.y.saturating_sub(area.y),
            };
            let mut gboom_escape_emitted = false;
            let hud = gboom.hud();
            if let Some(popup_rect) = crate::render::gboom_overlay::render_gboom_overlay(
                buf,
                overlay_area,
                &hud,
                theme.bg_base,
                theme.text_primary,
                theme.gray_dim,
            ) {
                gboom.set_mouse_region(
                    popup_rect.x,
                    popup_rect.y,
                    popup_rect.width,
                    popup_rect.height,
                );
                let inner_cols = popup_rect.width.saturating_sub(2);
                let inner_rows = popup_rect.height.saturating_sub(2);
                if inner_cols >= 10 && inner_rows >= 4 {
                    let (px_w, px_h) =
                        crate::gboom::GboomState::frame_size_for_cells(inner_cols, inner_rows);
                    if let Some(png) = gboom.frame_png(px_w, px_h)
                        && let Some(esc) = crate::terminal::overlay::volatile_image(
                            png,
                            inner_cols,
                            inner_rows,
                            popup_rect.x + 1,
                            popup_rect.y + 1,
                        )
                    {
                        prompt_post_flush = Some(esc.into());
                        gboom_escape_emitted = true;
                    }
                }
            } else {
                gboom.clear_mouse_region();
            }
            if !gboom_escape_emitted && detect_graphics_protocol() == GraphicsProtocol::Kitty {
                let clear = crate::terminal::overlay::clear_kitty();
                prompt_post_flush = Some(clear.into());
            }
            let hints = vec![
                HintItem::new(key!(Esc), "quit"),
                HintItem::new(key!(' '), "fire"),
            ];
            ShortcutsBar::new(&hints).render(layout.shortcuts, buf);
            self.pane_areas = layout.pane_areas();
            return (None, prompt_post_flush);
        }
        let block_viewer_toast = self.active_toast_message().map(|s| s.to_string());
        if let Some(ref mut viewer) = self.block_viewer {
            use ratatui::style::Modifier;
            use ratatui::widgets::{Block as RBlock, BorderType, Borders, Clear, Widget};
            let overlay_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: layout.shortcuts.y.saturating_sub(area.y),
            };
            let entry = if viewer.kind == crate::views::block_viewer::ViewerKind::PlainText {
                Some(crate::scrollback::entry::ScrollbackEntry::new(
                    crate::scrollback::block::RenderBlock::system(String::new()),
                ))
            } else {
                self.scrollback.get_by_id(viewer.entry_id).cloned()
            };
            let Some(entry) = entry else {
                self.block_viewer = None;
                self.pane_areas = layout.pane_areas();
                return (prompt_cursor_pos, prompt_post_flush);
            };
            let popup_w = ((overlay_area.width as f32 * 0.95) as u16)
                .max(60)
                .min(overlay_area.width);
            let popup_h = ((overlay_area.height as f32 * 0.92) as u16)
                .max(12)
                .min(overlay_area.height.saturating_sub(2));
            let h_pad = 2u16;
            let inner_w = popup_w.saturating_sub(2);
            let content_w = inner_w.saturating_sub(h_pad * 2);
            if popup_h.saturating_sub(2) < 3 || inner_w < 10 {
                self.pane_areas = layout.pane_areas();
                return (prompt_cursor_pos, prompt_post_flush);
            }
            let popup_x = overlay_area.x + (overlay_area.width.saturating_sub(popup_w)) / 2;
            let popup_y = overlay_area.y + (overlay_area.height.saturating_sub(popup_h)) / 2;
            let popup_area = Rect::new(popup_x, popup_y, popup_w, popup_h);
            let base_style = Style::default().fg(theme.text_primary).bg(theme.bg_base);
            let border = RBlock::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.gray_dim))
                .style(base_style);
            let inner = border.inner(popup_area);
            if inner.height < 3 || inner.width < 10 {
                self.pane_areas = layout.pane_areas();
                return (prompt_cursor_pos, prompt_post_flush);
            }
            crate::views::file_search::line_viewer::dim_area(buf, overlay_area, theme.bg_base, 0.5);
            Clear.render(popup_area, buf);
            buf.set_style(popup_area, base_style);
            border.render(popup_area, buf);
            let close_chars: &[char] = &['[', 'x', ']'];
            let close_w_px = close_chars.len() as u16;
            let close_x = inner.x + inner.width.saturating_sub(close_w_px + 1);
            let close_y = inner.y;
            let close_rect = Rect::new(close_x, close_y, close_w_px, 1);
            for (i, &ch) in close_chars.iter().enumerate() {
                if let Some(cell) = buf.cell_mut((close_x + i as u16, close_y)) {
                    cell.set_char(ch);
                    let style = if viewer.modal.close_hovered {
                        Style::default()
                            .fg(theme.text_primary)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.gray_dim)
                    };
                    cell.set_style(style);
                }
            }
            viewer.modal.close_button_rect = Some(close_rect);
            viewer.modal.popup_area = Some(popup_area);
            let preamble_ctx = crate::scrollback::types::BlockContext {
                mode: crate::scrollback::types::DisplayMode::Expanded,
                is_running: entry.is_running,
                width: content_w,
                raw: entry.raw,
                max_lines: None,
                appearance: appearance.clone(),
                is_selected: false,
                cwd: Some(self.session.cwd.clone()),
            };
            let preamble = entry.block.preamble(&preamble_ctx);
            let mut prepend_lines: Vec<ratatui::text::Line<'static>> = preamble
                .as_ref()
                .map(|text| {
                    text.lines
                        .iter()
                        .flat_map(|line| {
                            crate::render::wrapping::wrap_header_flush(
                                line.clone(),
                                content_w as usize,
                                0,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !prepend_lines.is_empty() {
                prepend_lines.push(ratatui::text::Line::from(""));
            }
            let content_x = inner.x + h_pad;
            let content_top = inner.y + 1;
            let content_height = inner.height.saturating_sub(1);
            let content_area = Rect {
                x: content_x,
                y: content_top,
                width: content_w,
                height: content_height,
            };
            viewer.render_content(content_area, buf, &entry, true, &prepend_lines);
            viewer.render_text_drag_overlay(buf);
            let has_input_bar =
                viewer.list_state.input_mode().is_some() || viewer.list_state.matcher().is_some();
            let in_visual = viewer.list_state.visual_mode;
            if (has_input_bar || in_visual) && content_area.height > 2 {
                let div_y = content_area.y + content_area.height - 2;
                for x in inner.x..inner.x + inner.width {
                    if let Some(cell) = buf.cell_mut((x, div_y)) {
                        cell.reset();
                        cell.set_char('\u{2500}');
                        cell.fg = theme.gray_dim;
                        cell.bg = theme.bg_base;
                    }
                }
                if in_visual && !has_input_bar {
                    let status_y = div_y + 1;
                    for x in inner.x..inner.x + inner.width {
                        if let Some(cell) = buf.cell_mut((x, status_y)) {
                            cell.reset();
                            cell.set_char(' ');
                            cell.fg = theme.text_secondary;
                            cell.bg = theme.bg_base;
                        }
                    }
                    let n = viewer.list_state.copy_range().map(|r| r.len()).unwrap_or(1);
                    let s = if n == 1 { "" } else { "s" };
                    let status = format!("Selected: {n} line{s}");
                    let status_style = Style::default().fg(theme.text_secondary).bg(theme.bg_base);
                    buf.set_string(content_x, status_y, &status, status_style);
                }
            }
            if let Some(ref msg) = block_viewer_toast
                && popup_area.height > 2
                && let Some(toast_text) = fit_toast_text(msg, popup_area.width.saturating_sub(1))
            {
                let w = toast_text.chars().count() as u16;
                let tx = popup_area.right().saturating_sub(w + 2);
                let ty = popup_area.bottom().saturating_sub(2);
                for (i, ch) in toast_text.chars().enumerate() {
                    if let Some(cell) = buf.cell_mut((tx + i as u16, ty)) {
                        cell.set_char(ch);
                        cell.fg = theme.accent_user;
                        cell.bg = theme.bg_base;
                        cell.modifier = ratatui::prelude::Modifier::BOLD;
                    }
                }
            }
            let hints = viewer.shortcuts_hints();
            ShortcutsBar::new(&hints).render(layout.shortcuts, buf);
            self.pane_areas = layout.pane_areas();
            return (prompt_cursor_pos, prompt_post_flush);
        }
        if let Some(ref mut modal_state) = self.agents_modal {
            let overlay_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: layout.shortcuts.y.saturating_sub(area.y).saturating_sub(1),
            };
            let compact = self.scrollback.appearance().prompt.compact;
            let theme = Theme::current();
            crate::views::agents_modal::render_agents_modal(
                buf,
                overlay_area,
                modal_state,
                compact,
                &theme,
            );
            if let Some(ref mut detail) = self.persona_detail {
                crate::views::persona_detail::render_persona_detail(
                    buf,
                    overlay_area,
                    detail,
                    &theme,
                    compact,
                );
            }
            self.pane_areas = layout.pane_areas();
            return (None, crate::terminal::overlay::clear().map(Into::into));
        }
        if let Some(ref mut modal_state) = self.extensions_modal {
            use crate::views::extensions_modal::render_extensions_modal;
            use crate::views::shortcuts_bar::HintItem;
            let is_fullscreen = matches!(
                modal_state.picker_state.mode,
                crate::views::picker::PickerMode::FullScreen
            );
            let overlay_area = if is_fullscreen {
                area
            } else {
                Rect {
                    x: area.x,
                    y: area.y,
                    width: area.width,
                    height: layout.shortcuts.y.saturating_sub(area.y).saturating_sub(1),
                }
            };
            let compact = self.scrollback.appearance().prompt.compact;
            let tick = self.scrollback.animation_tick();
            render_extensions_modal(
                buf,
                overlay_area,
                modal_state,
                Some(layout.shortcuts),
                compact,
                tick,
            );
            if modal_state.input.is_some() {
                let hints = vec![
                    HintItem::new(key!(Enter), "submit"),
                    HintItem::new(key!(Esc), "cancel"),
                ];
                ShortcutsBar::new(&hints).render(layout.shortcuts, buf);
            } else if modal_state.pending_action.is_some() {
                let hints = vec![HintItem::new(key!(Esc), "dismiss")];
                ShortcutsBar::new(&hints).render(layout.shortcuts, buf);
            } else if modal_state.picker_state.search_active {
                let hints = vec![
                    HintItem::new(key!(Esc), "clear search"),
                    HintItem::new(key!(Enter), "keep filter"),
                ];
                ShortcutsBar::new(&hints).render(layout.shortcuts, buf);
            }
            self.pane_areas = layout.pane_areas();
            return (None, crate::terminal::overlay::clear().map(Into::into));
        }
        let dropdown_active = self.slash_dropdown_items_area.is_some()
            || self.dropdown_items_area.is_some()
            || self.completion_dropdown_items_area.is_some()
            || self.history_dropdown_area.is_some();
        let placements = if dropdown_active {
            &[][..]
        } else {
            &scrollback_inline_media[..]
        };
        self.inline_media_hits = InlineMediaHitAreas::default();
        if !placements.is_empty() {
            let mut all_escapes = String::new();
            let mut this_frame_ids: HashSet<u32> = HashSet::new();
            for placement in placements {
                let path = &placement.info.path;
                if let Some(fp_rect) = placement.filepath_screen_rect {
                    self.inline_media_hits
                        .filepath_areas
                        .push((fp_rect, path.clone()));
                }
                if let Some(open_rect) = placement.open_button_screen_rect {
                    self.inline_media_hits
                        .open_buttons
                        .push((open_rect, path.clone()));
                    continue;
                }
                if !crate::terminal::image::scrollback_inline_overlay_active() {
                    continue;
                }
                if !self.inline_media_cache.contains_key(path) && placement.screen_rect.height >= 1
                {
                    let rect = placement.screen_rect;
                    let center_x = |len: usize| rect.x + rect.width.saturating_sub(len as u16) / 2;
                    if placement.info.is_video && !crate::inline_media_ffmpeg::ffmpeg_available() {
                        use crate::inline_media_ffmpeg::{FFMPEG_HINT_TEXT, ffmpeg_install_cmd};
                        let warn = Style::default().fg(theme.warning);
                        buf.set_string_safe(
                            center_x(FFMPEG_HINT_TEXT.len()),
                            rect.y,
                            FFMPEG_HINT_TEXT,
                            warn,
                        );
                        if let Some(cmd) = ffmpeg_install_cmd() {
                            let dim = Style::default().fg(theme.gray_dim);
                            buf.set_string_safe(center_x(cmd.len()), rect.y + 1, cmd, dim);
                        }
                    } else {
                        let spinner_frames = crate::glyphs::braille_spinner_frames();
                        let tick = self.scrollback.current_tick() as usize;
                        let spinner = spinner_frames[tick % spinner_frames.len()];
                        let label = format!("{spinner} Loading...");
                        let cy = rect.y + rect.height / 2;
                        buf.set_string_safe(
                            center_x(label.len()),
                            cy,
                            &label,
                            Style::default().fg(theme.gray_dim),
                        );
                    }
                }
                if let Some(esc) = self.build_inline_media_escapes(placement) {
                    all_escapes.push_str(&esc);
                    if let Some(&id) = self.inline_media_ids.get(path) {
                        this_frame_ids.insert(id);
                    }
                    let rect = placement.screen_rect;
                    if !placement.has_button_row {
                        continue;
                    }
                    let button_y = rect.y + rect.height + 1;
                    let sb_area = self.pane_areas.scrollback;
                    let button_visible =
                        button_y >= sb_area.y && button_y < sb_area.y + sb_area.height;
                    if placement.info.is_video {
                        self.inline_media_hits
                            .video_play_areas
                            .push((rect, path.clone()));
                        if button_visible {
                            let is_playing = matches!(
                                self.inline_video,
                                Some(ref vid) if vid.path == *path && !vid.finished
                            );
                            let play_label: String = if is_playing {
                                let vid = self.inline_video.as_ref().unwrap();
                                let dur_s = vid.frames.len() as f64 / vid.fps;
                                let pos_s = vid.current_frame as f64 / vid.fps;
                                format!(
                                    "{}:{:02} / {}:{:02}",
                                    pos_s as u32 / 60,
                                    pos_s as u32 % 60,
                                    dur_s as u32 / 60,
                                    dur_s as u32 % 60,
                                )
                            } else {
                                "[Play]".to_string()
                            };
                            let open_label = "[Open]";
                            let gap = 3u16;
                            let total = play_label.len() as u16 + gap + open_label.len() as u16;
                            let start_x = rect.x + rect.width.saturating_sub(total) / 2;
                            buf.set_string_safe(
                                start_x,
                                button_y,
                                &play_label,
                                Style::default().fg(theme.gray),
                            );
                            if !is_playing {
                                self.inline_media_hits.play_buttons.push((
                                    Rect {
                                        x: start_x,
                                        y: button_y,
                                        width: play_label.len() as u16,
                                        height: 1,
                                    },
                                    path.clone(),
                                ));
                            }
                            let open_x = start_x + play_label.len() as u16 + gap;
                            buf.set_string_safe(
                                open_x,
                                button_y,
                                open_label,
                                Style::default().fg(theme.gray),
                            );
                            self.inline_media_hits.open_buttons.push((
                                Rect {
                                    x: open_x,
                                    y: button_y,
                                    width: open_label.len() as u16,
                                    height: 1,
                                },
                                path.clone(),
                            ));
                        }
                    } else {
                        self.inline_media_hits
                            .media_areas
                            .push((rect, path.clone()));
                        if button_visible {
                            let open_label = "[Open]";
                            let copy_label = "[Copy]";
                            let gap = 3u16;
                            let total = open_label.len() as u16 + gap + copy_label.len() as u16;
                            let start_x = rect.x + rect.width.saturating_sub(total) / 2;
                            buf.set_string_safe(
                                start_x,
                                button_y,
                                open_label,
                                Style::default().fg(theme.gray),
                            );
                            self.inline_media_hits.open_buttons.push((
                                Rect {
                                    x: start_x,
                                    y: button_y,
                                    width: open_label.len() as u16,
                                    height: 1,
                                },
                                path.clone(),
                            ));
                            let copy_x = start_x + open_label.len() as u16 + gap;
                            buf.set_string_safe(
                                copy_x,
                                button_y,
                                copy_label,
                                Style::default().fg(theme.gray),
                            );
                            self.inline_media_hits.copy_image_buttons.push((
                                Rect {
                                    x: copy_x,
                                    y: button_y,
                                    width: copy_label.len() as u16,
                                    height: 1,
                                },
                                path.clone(),
                            ));
                        }
                    }
                }
            }
            for &old_id in &self.last_placed_ids {
                if !this_frame_ids.contains(&old_id) {
                    all_escapes.push_str(&crate::terminal::image::clear_kitty_image(old_id));
                    self.inline_media_ids.retain(|_, &mut v| v != old_id);
                    self.inline_media_iterm_emitted
                        .retain(|p, _| self.inline_media_ids.contains_key(p));
                }
            }
            self.last_placed_ids = this_frame_ids;
            if !all_escapes.is_empty() {
                self.inline_media_active = true;
                match prompt_post_flush.as_mut() {
                    Some(existing) => existing.append_plain(&all_escapes),
                    None => {
                        prompt_post_flush =
                            Some(crate::terminal::overlay::PostFlush::plain(all_escapes));
                    }
                }
            } else if self.inline_media_active {
                self.inline_media_active = false;
                let mut clear_esc = String::new();
                for &id in self.inline_media_ids.values() {
                    clear_esc.push_str(&crate::terminal::image::clear_kitty_image(id));
                }
                self.inline_media_ids.clear();
                self.inline_media_iterm_emitted.clear();
                self.last_placed_ids.clear();
                if !clear_esc.is_empty() {
                    match prompt_post_flush.as_mut() {
                        Some(existing) => existing.append_plain(&clear_esc),
                        None => {
                            prompt_post_flush =
                                Some(crate::terminal::overlay::PostFlush::plain(clear_esc));
                        }
                    }
                }
            }
        } else if self.inline_media_active {
            self.inline_media_active = false;
            self.stop_inline_playback();
            let mut clear_esc = String::new();
            for &id in self.inline_media_ids.values() {
                clear_esc.push_str(&crate::terminal::image::clear_kitty_image(id));
            }
            self.inline_media_iterm_emitted.clear();
            self.inline_media_ids.clear();
            self.last_placed_ids.clear();
            if !clear_esc.is_empty() {
                match prompt_post_flush.as_mut() {
                    Some(existing) => existing.append_plain(&clear_esc),
                    None => {
                        prompt_post_flush =
                            Some(crate::terminal::overlay::PostFlush::plain(clear_esc));
                    }
                }
            }
        }
        if !dropdown_active {
            self.paint_diagram_affordances(buf, scrollback_diagram_affordances, &theme);
        }
        if !self.inline_media_active
            && prompt_post_flush.is_none()
            && crate::terminal::image::detect_graphics_protocol()
                != crate::terminal::image::GraphicsProtocol::None
        {
            let clear = crate::terminal::overlay::clear_kitty();
            prompt_post_flush = Some(clear.into());
        }
        if self.show_goal_detail
            && let Some(ref goal) = self.goal_state
        {
            let todos = self.todo.todos();
            let overlay_rect = crate::views::goal_detail::goal_detail_area(area, goal, todos);
            let tick = self.tasks.tick_count() as usize;
            let active_subagent_tokens: u64 = self
                .subagent_sessions
                .values()
                .filter(|s| !s.finished && s.workflow_run_id.is_none())
                .filter_map(|s| s.tokens_used)
                .sum();
            let close_rect = crate::views::goal_detail::render_goal_detail(
                buf,
                overlay_rect,
                goal,
                todos,
                tick,
                self.context_state.as_ref().map(|c| c.used),
                active_subagent_tokens,
                self.hit_goal_close.hovered,
            );
            self.hit_goal_close.rect = close_rect;
            self.frame_occluder_rects.push(overlay_rect);
        }
        if self.show_workflows {
            let runs = self.workflow_runs_newest_first();
            let mut view = self.workflows_view.clone();
            view.normalize(&runs);
            let tick = self.tasks.tick_count() as usize;
            let live: crate::views::workflows::WorkflowAgentLiveMap = self
                .subagent_sessions
                .iter()
                .filter(|(_, info)| info.workflow_run_id.is_some())
                .map(|(id, info)| {
                    (
                        id.clone(),
                        crate::views::workflows::WorkflowAgentLiveStatus {
                            activity: info.activity_label.clone(),
                            context_tokens: info.tokens_used,
                            context_window_tokens: info.context_window_tokens,
                            elapsed_ms: Some(info.display_elapsed().as_millis() as u64),
                        },
                    )
                })
                .collect();
            let popup =
                crate::views::workflows::render_workflows(buf, area, &runs, &mut view, tick, &live);
            self.workflows_view = view;
            if let Some(popup) = popup {
                self.frame_occluder_rects.push(popup);
            }
        }
        self.pane_areas = layout.pane_areas();
        {
            let route = crate::hyperlink_route::hyperlink_route();
            if route.emit_osc8 {
                let occluders = &self.frame_occluder_rects;
                let emit_id = route.emit_id;
                *link_spans_out = self
                    .last_link_overlay
                    .links()
                    .iter()
                    .filter(|link| {
                        !occluders.iter().any(|r| {
                            link.screen_row >= r.y
                                && link.screen_row < r.y.saturating_add(r.height)
                                && link.col_start < r.x.saturating_add(r.width)
                                && r.x < link.col_end
                        })
                    })
                    .filter_map(|link| {
                        crate::render::osc8::resolve_link_target_with_presentation(
                            &link.target,
                            link.presentation,
                        )
                        .and_then(|resolved| resolved.osc8_url)
                        .map(|url| echo_agent_ratatui_inline::LinkSpan {
                            row: link.screen_row,
                            col_start: link.col_start,
                            col_end: link.col_end,
                            url,
                            id: if emit_id { link.id } else { None },
                        })
                    })
                    .collect();
                self.push_promo_cta_link_span(
                    link_spans_out,
                    banner_announcements,
                    hidden_announcement_ids,
                );
                self.push_upgrade_cta_link_span(
                    link_spans_out,
                    banner_announcements,
                    hidden_announcement_ids,
                );
                self.push_status_line_link_spans(
                    link_spans_out,
                    std::mem::take(&mut status_line_link_spans),
                );
            }
        }
        let on_link = self.hovered_link_idx.is_some();
        if supports_osc22() && on_link != self.last_pointer_on_link {
            self.last_pointer_on_link = on_link;
            use crossterm::Command;
            let mut seq = String::new();
            if on_link {
                let _ = crate::terminal::SetPointerCursor.write_ansi(&mut seq);
            } else {
                let _ = crate::terminal::SetDefaultCursor.write_ansi(&mut seq);
            }
            match prompt_post_flush {
                Some(ref mut existing) => existing.append_plain(&seq),
                None => {
                    prompt_post_flush = Some(crate::terminal::overlay::PostFlush::plain(seq));
                }
            }
        }
        let cursor = if self.inline_edit.is_some() {
            inline_edit_cursor
        } else {
            prompt_cursor_pos
        };
        (cursor, prompt_post_flush)
    }
}
/// Draw one ▼/▲ scroll-indicator arrow centered on row `y`, or clear its
/// hit area when hidden (`y: None`). The unconditional set-or-clear is the
/// point: a hit rect must never outlive the frame that painted its arrow,
/// or an invisible click target keeps firing under whatever covers it
/// (e.g. an open block viewer).
fn draw_scroll_arrow(
    buf: &mut Buffer,
    theme: &Theme,
    center_x: u16,
    y: Option<u16>,
    symbol: &str,
    hit: &mut super::HitArea,
) {
    let Some(y) = y else {
        hit.clear();
        return;
    };
    let style = Style::default().fg(if hit.hovered {
        theme.gray_bright
    } else {
        theme.gray
    });
    if let Some(cell) = buf.cell_mut((center_x, y)) {
        cell.set_symbol(symbol);
        cell.set_style(style);
    }
    hit.set(Some(Rect::new(center_x.saturating_sub(1), y, 3, 1)));
}
/// Pad `msg` for the toast slot, truncating with a trailing ellipsis when it
/// cannot fit in `avail_width` columns (long clipboard toasts embed backup
/// file paths — dropping the whole toast would hide the copy feedback
/// entirely). Returns `None` only when the slot is too narrow for any text.
fn fit_toast_text(msg: &str, avail_width: u16) -> Option<String> {
    let max_msg_chars = (avail_width as usize).saturating_sub(4);
    if max_msg_chars == 0 {
        return None;
    }
    let msg_chars = msg.chars().count();
    if msg_chars <= max_msg_chars {
        return Some(format!(" {msg} "));
    }
    let truncated: String = msg.chars().take(max_msg_chars.saturating_sub(1)).collect();
    Some(format!(" {}… ", truncated.trim_end()))
}
#[cfg(test)]
mod toast_fit_tests {
    use super::fit_toast_text;
    #[test]
    fn short_message_is_padded_untouched() {
        assert_eq!(fit_toast_text("Copied!", 40).as_deref(), Some(" Copied! "));
    }
    #[test]
    fn long_message_truncates_with_ellipsis_instead_of_vanishing() {
        let msg = "Copied via OSC 52 — also saved to /tmp/echo-agent-0/last-copy.txt. If paste fails, hold Shift (or Fn) and drag to select & copy natively.";
        let fitted = fit_toast_text(msg, 60).expect("must render truncated");
        assert!(fitted.chars().count() <= 58);
        assert!(fitted.ends_with("… "));
        assert!(fitted.contains("also saved to"));
    }
    #[test]
    fn zero_width_slot_yields_none() {
        assert_eq!(fit_toast_text("Copied!", 4), None);
        assert_eq!(fit_toast_text("Copied!", 0), None);
    }
}
#[cfg(test)]
mod selection_state_tests {
    use super::super::test_fixtures::make_agent;
    use super::*;
    use crate::scrollback::text_selection::ResolvedSelectableLine;
    use crate::scrollback::types::SelectionBoundary;
    use std::sync::Arc;
    #[test]
    fn frame_reset_clears_model_and_companion_together() {
        let mut model = ResolvedSelectionModel::default();
        let line = ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx: 0,
            screen_y: 0,
            screen_x: 0,
            selectable_cols: 0..3,
            text: "foo".to_string(),
            painted_region: None,
            joiner_to_previous: None,
        };
        let mut boundaries = ResolvedSelectionBoundaries::default();
        boundaries.push(
            &line,
            Arc::new(SelectionBoundary::new("   ".to_string(), String::new())),
        );
        model.push_line(line);
        let mut agent = make_agent();
        agent.update_scrollback_selection_state(model, boundaries);
        agent.clear_scrollback_selection_state();
        assert!(agent.last_scrollback_selection_model.ranges.is_empty());
        assert!(agent.last_scrollback_selection_boundaries.is_empty());
    }
}
#[cfg(test)]
mod voice_recording_overlay_tests {
    use super::super::test_fixtures::make_agent;
    use super::super::test_fixtures::make_plan_approval_view_state;
    use super::AgentView;
    use crate::actions::ActionRegistry;
    use crate::app::bundle::BundleState;
    use crate::scrollback::render::ScratchBuffer;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    /// Agent with the plan-approval view (and its line-viewer overlay) open.
    fn plan_approval_agent() -> AgentView {
        let mut agent = make_agent();
        agent.plan_approval_view = Some(make_plan_approval_view_state());
        agent.reopen_plan_approval();
        assert!(agent.line_viewer.is_some(), "approval must open the viewer");
        agent
    }
    /// Render `agent` with the given voice state and return the buffer text.
    fn render_text(agent: &mut AgentView, listening: bool) -> String {
        let reg = ActionRegistry::defaults();
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        let mut scratch = ScratchBuffer::new();
        agent.draw(
            area,
            &mut buf,
            &reg,
            &mut scratch,
            None,
            false,
            crate::app::agent_view::BannerSlotParams::none(),
            &BundleState::default(),
            false,
            false,
            &mut Vec::new(),
            super::AppRenderParams {
                voice_available: listening,
                voice_listening: listening,
                ..Default::default()
            },
        );
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }
    /// The plan approval's line-viewer overlay used to paint over the
    /// `voice_recording` row, leaving a live mic (Ctrl+Space / F8 still work
    /// there) with no visible "Recording" indicator. The overlay must stop
    /// above the record indicator row.
    #[test]
    fn recording_row_visible_while_plan_approval_open() {
        let mut agent = plan_approval_agent();
        let text = render_text(&mut agent, true);
        assert!(
            text.contains("Recording"),
            "record indicator must stay visible under the plan approval viewer:\n{text}"
        );
    }
    /// While voice is idle no indicator row exists, so the overlay keeps
    /// reaching the prompt as before.
    #[test]
    fn no_recording_row_when_not_listening_in_plan_approval() {
        let mut agent = plan_approval_agent();
        let text = render_text(&mut agent, false);
        assert!(
            !text.contains("Recording"),
            "no record indicator when voice is idle:\n{text}"
        );
    }
}
#[cfg(test)]
mod overlay_cycle_hint_tests {
    use super::super::test_fixtures::make_agent;
    use crate::actions::ActionRegistry;
    use crate::app::bundle::BundleState;
    use crate::scrollback::render::ScratchBuffer;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    fn draw_overlay_footer(can_cycle: bool) -> String {
        let mut agent = make_agent();
        let reg = ActionRegistry::defaults();
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        let mut scratch = ScratchBuffer::new();
        agent.draw(
            area,
            &mut buf,
            &reg,
            &mut scratch,
            None,
            false,
            crate::app::agent_view::BannerSlotParams::none(),
            &BundleState::default(),
            true,
            can_cycle,
            &mut Vec::new(),
            super::AppRenderParams::default(),
        );
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn prev_next_hint_shown_when_overlay_can_cycle() {
        let text = draw_overlay_footer(true);
        assert!(
            text.contains("prev/next agent"),
            "footer must advertise cycle keys when can_cycle:\n{text}"
        );
    }
    #[test]
    fn prev_next_hint_hidden_when_overlay_cannot_cycle() {
        let text = draw_overlay_footer(false);
        assert!(
            !text.contains("prev/next agent"),
            "footer must not advertise cycle keys for a single agent:\n{text}"
        );
        assert!(
            text.contains("dashboard"),
            "back-to-dashboard hint still shown:\n{text}"
        );
    }
}
#[cfg(test)]
mod overlay_post_flush_tests {
    use super::super::test_fixtures::make_agent;
    use crate::actions::ActionRegistry;
    use crate::app::bundle::BundleState;
    use crate::scrollback::render::ScratchBuffer;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    fn draw(agent: &mut super::AgentView) -> Option<crate::terminal::overlay::PostFlush> {
        let area = Rect::new(0, 0, 100, 40);
        let mut buf = Buffer::empty(area);
        let mut scratch = ScratchBuffer::new();
        agent
            .draw(
                area,
                &mut buf,
                &ActionRegistry::defaults(),
                &mut scratch,
                None,
                false,
                crate::app::agent_view::BannerSlotParams::none(),
                &BundleState::default(),
                false,
                false,
                &mut Vec::new(),
                super::AppRenderParams::default(),
            )
            .1
    }
    fn seed_static_owner(owner_id: u64) {
        let _ = crate::terminal::overlay::static_image(&png(), 20, 10, 0, 0, owner_id)
            .unwrap()
            .commit();
    }
    fn png() -> [u8; 8] {
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
    }
    #[test]
    fn fullscreen_subagent_propagates_child_clear_to_emitter() {
        let _guard = crate::terminal::image::set_protocol_for_test(
            crate::terminal::image::GraphicsProtocol::Kitty,
        );
        crate::terminal::overlay::reset_owner();
        seed_static_owner(41);
        let mut parent = make_agent();
        parent
            .subagent_views
            .insert("child".into(), Box::new(make_agent()));
        parent.active_subagent = Some("child".into());
        let post_flush = draw(&mut parent).expect("child clear propagates");
        assert!(post_flush.as_str().contains("a=d"));
        let before_emit = crate::terminal::overlay::static_image(&png(), 20, 10, 0, 0, 41).unwrap();
        assert!(!before_emit.as_str().contains("a=t"));
        post_flush.write_to(&mut Vec::new()).unwrap();
        let after_emit = crate::terminal::overlay::static_image(&png(), 20, 10, 0, 0, 41).unwrap();
        assert!(after_emit.as_str().contains("a=t"));
    }
    #[test]
    fn active_modal_returns_clear_without_committing_discarded_state() {
        let _guard = crate::terminal::image::set_protocol_for_test(
            crate::terminal::image::GraphicsProtocol::Kitty,
        );
        crate::terminal::overlay::reset_owner();
        seed_static_owner(42);
        let mut agent = make_agent();
        agent.active_modal = Some(crate::views::modal::ActiveModal::CommandPalette {
            entries: Vec::new(),
            state: crate::views::picker::PickerState::default(),
            window: crate::views::modal_window::ModalWindowState::new(),
        });
        let post_flush = draw(&mut agent).expect("modal clear returned");
        assert!(post_flush.as_str().contains("a=d"));
        let before_emit = crate::terminal::overlay::static_image(&png(), 20, 10, 0, 0, 42).unwrap();
        assert!(!before_emit.as_str().contains("a=t"));
        post_flush.write_to(&mut Vec::new()).unwrap();
        let after_emit = crate::terminal::overlay::static_image(&png(), 20, 10, 0, 0, 42).unwrap();
        assert!(after_emit.as_str().contains("a=t"));
    }
}
#[cfg(test)]
mod permission_hint_tests {
    use super::super::test_fixtures::{make_agent, make_followup_permission_state};
    use crate::views::permission_view::PermissionFocus;
    use agent_client_protocol as acp;
    use ratatui::layout::Rect;
    use std::sync::Arc;
    const ALL_FOCUSES: [PermissionFocus; 3] = [
        PermissionFocus::Options,
        PermissionFocus::FollowupInput,
        PermissionFocus::PatternEdit,
    ];
    /// The footer Ctrl-F hint must track the key exactly: shown in every
    /// focus mode when something is collapsible (the key fires before the
    /// focus match), absent for protected-edit warning descriptions.
    #[test]
    fn ctrl_f_hint_follows_the_key_in_every_focus_mode() {
        let mut agent = make_agent();
        agent.pane_areas.prompt = Rect::new(0, 20, 80, 10);
        let mut perm = make_followup_permission_state();
        perm.bash_command_raw = Some(
            (0..12)
                .map(|i| format!("echo line{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        for focus in ALL_FOCUSES {
            perm.focus = focus;
            let hints = agent.permission_shortcut_hints(&perm);
            assert!(
                hints.iter().any(|h| h.label == "expand"),
                "missing Ctrl-F hint in {focus:?}"
            );
        }
        perm.args_expanded = true;
        assert!(
            agent
                .permission_shortcut_hints(&perm)
                .iter()
                .any(|h| h.label == "collapse")
        );
        perm.args_expanded = false;
        perm.bash_command_raw = None;
        perm.description = vec!["Warning: this file is protected".into()];
        perm.options = vec![acp::PermissionOption::new(
            acp::PermissionOptionId::new(Arc::from(
                echo_agent_workspace::permission::ALLOW_EDITS_SESSION_OPTION_ID,
            )),
            "Allow all edits this session".to_owned(),
            acp::PermissionOptionKind::AllowAlways,
        )];
        for focus in ALL_FOCUSES {
            perm.focus = focus;
            let hints = agent.permission_shortcut_hints(&perm);
            assert!(
                !hints
                    .iter()
                    .any(|h| h.label == "expand" || h.label == "collapse"),
                "protected-edit must not advertise Ctrl-F in {focus:?}"
            );
        }
        perm.options.clear();
        perm.description = vec!["{".into(), "  \"k\": 1".into(), "}".into()];
        for focus in ALL_FOCUSES {
            perm.focus = focus;
            let hints = agent.permission_shortcut_hints(&perm);
            assert!(
                hints.iter().any(|h| h.label == "expand"),
                "missing Ctrl-F hint for scope-less MCP args in {focus:?}"
            );
        }
    }
}
#[cfg(test)]
mod feedback_input_tests {
    use super::super::test_fixtures::make_agent;
    use super::AgentView;
    use crate::actions::ActionRegistry;
    use crate::app::bundle::BundleState;
    use crate::scrollback::render::ScratchBuffer;
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState, feedback_input};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::Question;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    /// Agent with the bare `/feedback` pane open and focused for typing.
    fn feedback_agent() -> AgentView {
        let mut agent = make_agent();
        let stashed = agent.prompt.stash();
        let mut state = QuestionViewState::new(
            "feedback-test".into(),
            vec![Question {
                question: crate::app::dispatch::FEEDBACK_QUESTION_LABEL.into(),
                options: vec![],
                multi_select: Some(false),
                id: None,
            }],
            stashed,
        )
        .with_local_kind(LocalQuestionKind::Feedback);
        let freeform = state.activate_freeform_input();
        agent.prompt.set_text_preserving(&freeform);
        agent.question_view = Some(state);
        agent
    }
    fn render_text(agent: &mut AgentView) -> String {
        render_text_sized(agent, 100, 40)
    }
    fn render_text_sized(agent: &mut AgentView, width: u16, height: u16) -> String {
        let reg = ActionRegistry::defaults();
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let mut scratch = ScratchBuffer::new();
        agent.draw(
            area,
            &mut buf,
            &reg,
            &mut scratch,
            None,
            false,
            crate::app::agent_view::BannerSlotParams::none(),
            &BundleState::default(),
            false,
            false,
            &mut Vec::new(),
            super::AppRenderParams::default(),
        );
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }
    /// The feedback pane keeps the question card, and puts a multi-line report area where the options and the one-line freeform row would be.
    #[test]
    fn feedback_pane_renders_report_area_in_the_card() {
        let mut agent = feedback_agent();
        let screen = render_text(&mut agent);
        assert!(
            screen.contains(crate::app::dispatch::FEEDBACK_QUESTION_LABEL),
            "label missing\n{screen}"
        );
        assert!(
            screen.contains(feedback_input::PLACEHOLDER),
            "placeholder must stay visible in the empty focused input\n{screen}"
        );
        assert!(
            !screen.contains("(\u{25cb})"),
            "the freeform radio row has no place in the feedback pane\n{screen}"
        );
        assert!(
            !screen.contains("navigate"),
            "there is nothing to navigate in the feedback pane\n{screen}"
        );
        assert!(
            screen.contains("Enter:send"),
            "footer must offer the send action\n{screen}"
        );
        let top = screen
            .lines()
            .position(|l| l.contains('\u{256d}'))
            .expect("report box needs a top rule");
        let bottom = screen
            .lines()
            .position(|l| l.contains('\u{2570}'))
            .expect("report box needs a bottom rule");
        let box_rows = (bottom - top + 1) as u16;
        assert_eq!(
            box_rows,
            feedback_input::HEIGHT,
            "report box should be {} rows, got {box_rows}\n{screen}",
            feedback_input::HEIGHT
        );
        let sides = screen
            .lines()
            .skip(top + 1)
            .take(bottom - top - 1)
            .filter(|l| l.matches('\u{2502}').count() >= 2)
            .count();
        assert_eq!(
            sides,
            bottom - top - 1,
            "every text row of the box needs both side rules\n{screen}"
        );
    }
    #[test]
    fn feedback_trace_question_renders_options() {
        let mut agent = feedback_agent();
        {
            let qv = agent.question_view.as_mut().expect("pane");
            qv.begin_feedback_trace_stage("clipboard is broken over ssh".into(), vec![]);
        }
        let screen = render_text(&mut agent);
        for fragment in ["Opt-in to provide your trace", "retain and train"] {
            assert!(
                screen.contains(fragment),
                "trace question label missing ({fragment})\n{screen}"
            );
        }
        for option in ["Opt in", "Opt out this time"] {
            assert!(screen.contains(option), "{option} missing\n{screen}");
        }
        let selected_row = screen
            .lines()
            .find(|l| l.contains("Opt in") && !l.contains("Opt out"))
            .expect("first option row");
        assert!(
            selected_row.contains('\u{25cf}'),
            "turning trace upload on must render preselected\n{screen}"
        );
        assert!(
            screen.contains("Enter:send"),
            "footer must offer send\n{screen}"
        );
        assert!(
            !screen.contains("dismiss"),
            "the trace card cannot be dismissed without sending; the bar must \
             not promise it\n{screen}"
        );
    }
    /// A shrunk box must not paint over the shortcuts bar or leave the user typing into a pane that renders nothing.
    #[test]
    fn feedback_report_box_shrinks_on_a_short_terminal() {
        for height in [10u16, 11, 12, 13, 14, 16, 18, 20] {
            let mut agent = feedback_agent();
            let screen = render_text_sized(&mut agent, 100, height);
            let rows: Vec<&str> = screen.lines().collect();
            let card_visible = screen.contains(crate::glyphs::accent_bar());
            if card_visible {
                assert!(
                    screen.contains(feedback_input::PLACEHOLDER)
                        || screen.contains(crate::glyphs::prompt_arrow()),
                    "card renders without its report input at height {height}\n{screen}"
                );
            }
            let Some(bottom) = rows.iter().position(|l| l.contains('\u{2570}')) else {
                assert!(
                    !screen.contains('\u{256d}'),
                    "a top rule without a bottom rule means a clipped box (height {height})\n{screen}"
                );
                continue;
            };
            let top = rows
                .iter()
                .position(|l| l.contains('\u{256d}'))
                .expect("a bottom rule implies a top rule");
            assert!(top < bottom, "box rules out of order (height {height})");
            assert!(
                bottom + 1 < rows.len(),
                "box must leave room below the panel (height {height})\n{screen}"
            );
        }
    }
}
#[cfg(test)]
mod status_line_draw_tests {
    use super::super::test_fixtures::make_agent;
    use super::AgentView;
    use crate::actions::ActionRegistry;
    use crate::app::bundle::BundleState;
    use crate::scrollback::render::ScratchBuffer;
    use crate::views::question_view::{LocalQuestionKind, QuestionViewState};
    use crate::views::status_line::{SanitizedText, StatusLineDisplay, StatusLineFrame};
    use echo_agent_tools::implementations::echo_agent_build::ask_user_question::Question;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    fn draw_script(output: &str, rows: u16) -> Buffer {
        draw_script_for(&mut make_agent(), output, rows)
    }
    fn draw_script_for(agent: &mut AgentView, output: &str, rows: u16) -> Buffer {
        let area = Rect::new(0, 0, 80, rows);
        let mut buf = Buffer::empty(area);
        let mut scratch = ScratchBuffer::new();
        let _ = agent.draw(
            area,
            &mut buf,
            &ActionRegistry::defaults(),
            &mut scratch,
            None,
            false,
            crate::app::agent_view::BannerSlotParams::none(),
            &BundleState::default(),
            false,
            false,
            &mut Vec::new(),
            super::AppRenderParams {
                status_line: StatusLineFrame::On {
                    display: std::sync::Arc::new(StatusLineDisplay::Text(SanitizedText::new(
                        output,
                    ))),
                    padding: 0,
                },
                ..Default::default()
            },
        );
        buf
    }
    fn find(buf: &Buffer, text: &str) -> Option<(u16, u16)> {
        let area = *buf.area();
        let want: Vec<String> = text.chars().map(String::from).collect();
        (area.y..area.bottom()).find_map(|y| {
            (area.x..area.right().saturating_sub(want.len() as u16 - 1))
                .find(|x| {
                    want.iter()
                        .enumerate()
                        .all(|(i, c)| buf[(x + i as u16, y)].symbol() == c.as_str())
                })
                .map(|x| (x, y))
        })
    }
    #[test]
    fn script_background_survives_the_pane_fill() {
        let buf = draw_script("\x1b[41mRED\x1b[0m", 30);
        let (x, y) = find(&buf, "RED").expect("the script row is on screen");
        assert_eq!(buf[(x, y)].bg, Color::Red);
    }
    /// Agent with the bare `/feedback` pane open and focused, which asks for
    /// most of the screen.
    fn question_agent() -> AgentView {
        let mut agent = make_agent();
        let stashed = agent.prompt.stash();
        let mut state = QuestionViewState::new(
            "status_line-test".into(),
            vec![Question {
                question: crate::app::dispatch::FEEDBACK_QUESTION_LABEL.into(),
                options: vec![],
                multi_select: Some(false),
                id: None,
            }],
            stashed,
        )
        .with_local_kind(LocalQuestionKind::Feedback);
        let freeform = state.activate_freeform_input();
        agent.prompt.set_text_preserving(&freeform);
        agent.question_view = Some(state);
        agent
    }
    fn dump(buf: &Buffer) -> String {
        let area = *buf.area();
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }
    const FIVE_ROW_SCRIPT: &str = "row-1\nrow-2\nrow-3\nrow-4\nrow-5";
    #[test]
    fn short_terminal_leaves_the_row_four_of_its_five_rows() {
        let buf = draw_script(FIVE_ROW_SCRIPT, 16);
        let screen = dump(&buf);
        assert!(
            find(&buf, "row-4").is_some(),
            "four rows are left over at height 16\n{screen}"
        );
        assert!(
            find(&buf, "row-5").is_none(),
            "a fifth row could only come out of the prompt\n{screen}"
        );
        assert!(
            find(&buf, "\u{2570}").is_some(),
            "the prompt keeps its bottom rule\n{screen}"
        );
        assert!(
            // The label, not the binding: a terminal that cannot send `Ctrl+.`
            // draws `Ctrl+x` for the same cheatsheet.
            find(&buf, ":shortcuts").is_some(),
            "the shortcuts bar keeps its row\n{screen}"
        );
    }
    const ONE_ROW_SCRIPT: &str = "solo-row";
    #[test]
    fn fullscreen_question_panel_leaves_the_row_its_rows() {
        let buf = draw_script_for(&mut question_agent(), FIVE_ROW_SCRIPT, 24);
        let screen = dump(&buf);
        let present: Vec<u16> = (1..=5)
            .filter_map(|i| find(&buf, &format!("row-{i}")).map(|(_, y)| y))
            .collect();
        assert_eq!(
            present.len(),
            5,
            "the panel shrinks by the rows the script asks for, got rows at {present:?}\n{screen}"
        );
        assert!(
            find(&buf, "Esc:dismiss").is_some(),
            "the shortcuts bar keeps its row\n{screen}"
        );
    }
    #[test]
    fn fullscreen_question_panel_leaves_a_one_row_script_its_row() {
        let buf = draw_script_for(&mut question_agent(), ONE_ROW_SCRIPT, 24);
        let screen = dump(&buf);
        let row_y = find(&buf, ONE_ROW_SCRIPT)
            .map(|(_, y)| y)
            .unwrap_or_else(|| panic!("the panel must leave the single row on screen\n{screen}"));
        let bar_y = find(&buf, "Esc:dismiss")
            .map(|(_, y)| y)
            .unwrap_or_else(|| panic!("the shortcuts bar keeps its row\n{screen}"));
        assert_eq!(
            bar_y,
            row_y + 1,
            "the shortcuts bar sits directly under the row\n{screen}"
        );
    }
    #[test]
    fn row_clamped_away_by_the_prompt_keeps_the_exported_size() {
        let mut agent = make_agent();
        draw_script_for(&mut agent, ONE_ROW_SCRIPT, 20);
        let painted = agent.last_status_line_size;
        assert_eq!(
            painted,
            Some(crate::views::status_line::RowSize { cols: 76, lines: 1 }),
            "the row fills the inner width of an 80-column area"
        );
        agent.prompt.set_text_preserving(&"line\n".repeat(20));
        let buf = draw_script_for(&mut agent, ONE_ROW_SCRIPT, 20);
        assert!(
            find(&buf, ONE_ROW_SCRIPT).is_none(),
            "a prompt at its cap leaves no row to paint\n{}",
            dump(&buf)
        );
        assert_eq!(
            agent.last_status_line_size, painted,
            "a frame with no row must not export a width the script would read as the 80-column fallback"
        );
    }
}
