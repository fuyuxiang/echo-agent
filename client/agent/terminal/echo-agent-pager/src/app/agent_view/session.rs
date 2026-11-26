//! Session lifecycle: bind/reload/replay bookkeeping, turn activity
//! resolution, context/credit updates, and app-scoped gates.
#[cfg(test)]
use super::test_agent_view;
use super::{
    ActivePane, AgentView, InlineMediaHitAreas, InputMode, PaneAreas, PluginCtaState,
    PromptInputMode, PromptMode, REWOUND_PROMPT_ID_CAP, ReplayRebuiltState,
    SELF_ORIGINATED_PROMPT_CAP, SessionReload,
};
use crate::app::agent::AgentSession;
use crate::app::app_view::InputOutcome;
use crate::app::cancel_latency::{CancelLatency, CancelOrigin, TurnEnd};
use crate::scrollback::state::ScrollbackState;
use crate::scrollback::text_selection::ResolvedSelectionModel;
use crate::views::prompt_widget::PromptWidget;
use crate::views::queue_pane::QueuePane;
use crate::views::subagent_catalog_pane::SubagentCatalogPane;
use crate::views::tasks_pane::TasksPane;
use crate::views::todo_pane::TodoPane;
use echo_agent_telemetry::events::{CancellationCompleted, CancellationScope};
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;
impl AgentView {
    /// Live mutation of the turn-summary display field. Always bumps
    /// [`Self::last_turn_summary_gen`] so a concurrent disk hydrate that
    /// captured an older generation cannot overwrite this write.
    pub(crate) fn set_last_turn_summary(&mut self, summary: Option<String>) {
        self.last_turn_summary = summary;
        self.last_turn_summary_gen = self.last_turn_summary_gen.wrapping_add(1);
    }
    /// Bind this view to a root session id, resetting the per-session
    /// reconnect cursor and both dedup highwaters (ACP + EchoAgent) when the id
    /// actually changes — all three are meaningless against another session's
    /// event-id history (a stale cursor relies on exact-match failure for
    /// safety; a stale highwater could dedup-drop the new session's events
    /// outright).
    pub(crate) fn bind_session_id(&mut self, session_id: agent_client_protocol::SessionId) {
        if self.session.session_id.as_ref() != Some(&session_id) {
            self.session_binding_epoch = self.session_binding_epoch.wrapping_add(1);
            self.last_seen_event_id = None;
            self.last_seen_event_seq = None;
            self.last_applied_event_seq = None;
            self.last_applied_echo_agent_event_seq = None;
            self.deferred_subagent_finishes.clear();
            self.clear_minimal_btw_lifecycle();
        }
        self.session.session_id = Some(session_id);
    }
    /// Advance the reconnect cursor forward-only. Stores the raw id and its
    /// parsed sequence together so later compares need not re-parse the string.
    ///
    /// A later lower-ID apply (out-of-order lifecycle) must not regress the
    /// cursor and re-deliver an already-applied tail on reconnect. When the
    /// incoming id has no parseable sequence the cursor still advances, matching
    /// the pre-existing "unknown seq always applies" rule, but the known
    /// highwater counter is retained so later numeric ids stay gated.
    pub(crate) fn advance_last_seen_event_id(&mut self, event_id: String, event_seq: Option<u64>) {
        let new_seq = event_seq.or_else(|| crate::acp::meta::event_id_counter(&event_id));
        let cur_seq = self.last_seen_event_seq.or_else(|| {
            self.last_seen_event_id
                .as_deref()
                .and_then(crate::acp::meta::event_id_counter)
        });
        let should_advance = match (new_seq, cur_seq) {
            (Some(new), Some(cur)) => new > cur,
            _ => true,
        };
        if should_advance {
            self.last_seen_event_id = Some(event_id);
            self.last_seen_event_seq = new_seq.or(cur_seq);
        }
    }
    /// Unbind this view from its current session identity.
    pub(crate) fn unbind_session_id(&mut self) {
        if self.session.session_id.take().is_some() {
            self.session_binding_epoch = self.session_binding_epoch.wrapping_add(1);
            self.deferred_subagent_finishes.clear();
            self.clear_minimal_btw_lifecycle();
        }
    }
    /// Record a prompt id this client originated (sent to the agent as the turn
    /// driver). Used by the ACP gate to keep `attached_as_viewer` per-turn
    /// accurate. Bounded FIFO; a no-op for ids already tracked.
    pub fn note_self_originated_prompt(&mut self, prompt_id: &str) {
        if self.is_self_originated_prompt(prompt_id) {
            return;
        }
        self.self_originated_prompt_ids
            .push_back(prompt_id.to_string());
        while self.self_originated_prompt_ids.len() > SELF_ORIGINATED_PROMPT_CAP {
            self.self_originated_prompt_ids.pop_front();
        }
    }
    /// Whether `prompt_id` is a turn THIS client originated (vs. one another
    /// client drives, or a server-initiated turn).
    pub fn is_self_originated_prompt(&self, prompt_id: &str) -> bool {
        self.self_originated_prompt_ids
            .iter()
            .any(|p| p == prompt_id)
    }
    pub(crate) fn note_rewound_prompt(&mut self, prompt_id: &str) {
        if self.rewound_prompt_ids.iter().any(|p| p == prompt_id) {
            return;
        }
        self.rewound_prompt_ids.push_back(prompt_id.to_string());
        while self.rewound_prompt_ids.len() > REWOUND_PROMPT_ID_CAP {
            self.rewound_prompt_ids.pop_front();
        }
    }
    pub(crate) fn is_rewound_prompt(&self, prompt_id: &str) -> bool {
        self.rewound_prompt_ids.iter().any(|p| p == prompt_id)
    }
    /// Create a new agent view with default UI state.
    ///
    /// The prompt widget is initialized with the session's working directory.
    pub fn new(session: AgentSession, scrollback: ScrollbackState) -> Self {
        let prompt = PromptWidget::new_with_cwd(&session.cwd);
        let mut view = Self {
            session,
            session_binding_epoch: 0,
            scrollback,
            prompt,
            tip_typing_dismissed: false,
            todo: TodoPane::new(),
            tasks: TasksPane::new(),
            catalog: SubagentCatalogPane::new(),
            queue: QueuePane::new(),
            shared_queue: Vec::new(),
            attached_as_viewer: false,
            self_originated_prompt_ids: VecDeque::new(),
            rewound_prompt_ids: VecDeque::new(),
            last_applied_event_seq: None,
            last_applied_echo_agent_event_seq: None,
            last_seen_event_id: None,
            last_seen_event_seq: None,
            deferred_subagent_finishes: HashMap::new(),
            session_reload: None,
            unexpected_replay_drops: 0,
            late_replay_until: None,
            replayed_terminal_prompts: HashSet::new(),
            failed_wake_marker_for: None,
            running_wake_turn: None,
            finished_wake_prompts: HashSet::new(),
            active_pane: ActivePane::Prompt,
            prompt_mode: PromptMode::Normal,
            prompt_input_mode: PromptInputMode::Normal,
            multiline_mode: false,
            vim_mode: crate::appearance::cache::load_vim_mode(),
            input_mode: InputMode::Vim,
            bash_turn: false,
            cron_task_id: None,
            stashed_prompt: None,
            prompt_stash: None,
            draft_consumed: false,
            credit_limit_stashed_prompt: None,
            reauth_stashed_prompt: None,
            active_modal: None,
            modal_buttons: Vec::new(),
            modal_hovered_key: None,
            context_state: None,
            status_context: None,
            last_status_line_size: None,
            chat_kind: false,
            conversation_entry: false,
            app_chat_mode: false,
            #[cfg(feature = "local-workspace")]
            workspace_mode: crate::views::welcome::WelcomeWorkspaceMode::Sandbox,
            #[cfg(feature = "local-workspace")]
            workspace_mode_cli_locked: false,
            credit_balance: None,
            auto_topup: None,
            goal_state: None,
            workflow_blocks: std::collections::HashMap::new(),
            workflow_runs: Vec::new(),
            workflow_run_revisions: std::collections::HashMap::new(),
            cleared_workflow_runs: std::collections::HashSet::new(),
            show_workflows: false,
            workflows_view: crate::views::workflows::WorkflowsViewState::default(),
            pending_stop_hooks: None,
            last_cleared_goal_id: None,
            show_goal_detail: false,
            turn_start_ms: None,
            turn_start_ms_prompt: None,
            turn_started_at: None,
            first_activity_logged_for: None,
            turn_paused_duration: std::time::Duration::ZERO,
            turn_paused_wall: std::time::Duration::ZERO,
            self_interjection_ids: std::collections::HashSet::new(),
            last_active_at: Some(Instant::now()),
            current_branch: None,
            is_worktree: false,
            main_repo: None,
            worktree_label: None,
            activity_started_at: None,
            last_activity: None,
            pane_areas: PaneAreas::default(),
            hovered_entry: None,
            pending_text_drag: None,
            drag_selection: None,
            pending_block_drag: None,
            block_drag_selection: None,
            deferred_text_press: None,
            persistent_text_selection: None,
            table_selection_geometry: None,
            selection_created_at: None,
            last_drag_mouse: None,
            drag_autoscroll: None,
            left_mouse_down: false,
            plan_prompt_mouse_drag: false,
            last_scrollback_selection_model: ResolvedSelectionModel::default(),
            last_scrollback_selection_boundaries: Default::default(),
            last_link_overlay: Default::default(),
            frame_occluder_rects: Vec::new(),
            visible_link_map: Default::default(),
            scrollback_visible_link_count: 0,
            highlighted_link_idx: None,
            hovered_link_idx: None,
            last_pointer_on_link: false,
            last_btw_selection_model: ResolvedSelectionModel::default(),
            last_btw_area: Rect::default(),
            pending_scrollback_click: None,
            pending_link_click: None,
            media_link_paths: Vec::new(),
            media_link_paths_gen: None,
            last_mouse_pos: (0, 0),
            last_mouse_moved_at: None,
            last_click: None,
            last_text_click: None,
            last_clipboard_toast_at: None,
            last_context_click_at: None,
            hovered_prompt: false,
            hit_context: Default::default(),
            hit_credits: Default::default(),
            hit_todo_close: Default::default(),
            hit_bg_close: Default::default(),
            hit_subagent_close: Default::default(),
            hit_catalog_close: Default::default(),
            hit_bg_status: Default::default(),
            hit_goal_status: Default::default(),
            hit_goal_close: Default::default(),
            hit_bg_button: Default::default(),
            last_bg_click: None,
            hit_queue_close: Default::default(),
            hit_plan_button: Default::default(),
            hit_plan_approval_status: Default::default(),
            hit_follow_indicator: Default::default(),
            hit_response_top_indicator: Default::default(),
            hit_cwd: Default::default(),
            hit_cancel_button: Default::default(),
            hit_watching_cue: Default::default(),
            watching_cue_toast_shown: false,
            hit_announcement_hide: Default::default(),
            hit_announcement_cta: Default::default(),
            privacy_banner: Default::default(),
            hit_upgrade_cta: Default::default(),
            hit_voice_stop_button: Default::default(),
            hit_scrollbar: Default::default(),
            scrollbar_dragging: false,
            dropdown_items_area: None,
            slash_dropdown_items_area: None,
            slash_dropdown_hit: Default::default(),
            completion_dropdown_items_area: None,
            history_dropdown_area: None,
            last_prompt_click_ms: None,
            line_viewer: None,
            image_viewer: None,
            image_load_rx: None,
            video_viewer: None,
            gboom: None,
            inline_media_cache: std::collections::HashMap::new(),
            inline_media_load_failed: std::collections::HashMap::new(),
            inline_media_ids: std::collections::HashMap::new(),
            inline_media_iterm_emitted: std::collections::HashMap::new(),
            next_inline_media_id: 2,
            inline_video: None,
            video_load_rx: None,
            mermaid: None,
            edit_hl: None,
            inline_media_active: false,
            last_placed_ids: HashSet::new(),
            last_terminal_size: (0, 0),
            terminal_size_stale: false,
            inline_media_hits: InlineMediaHitAreas::default(),
            extensions_modal: None,
            agents_modal: None,
            persona_detail: None,
            btw_state: None,
            minimal_btw_lifecycle: None,
            btw_focused: false,
            hit_btw_close: Default::default(),
            toast: None,
            ephemeral_tip: Default::default(),
            word_select_tip_prompt_snapshot: None,
            last_word_select_probe: None,
            sticky_toast: None,
            mode_switch_banner: None,
            session_banner_active: false,
            pinned_upgrade_cta_live: false,
            block_viewer: None,
            scrollback_search: None,
            hit_sb_copy: Default::default(),
            hit_sb_view: Default::default(),
            question_view: None,
            elicitation_view: None,
            pending_elicitation: None,
            elicit_hits: Vec::new(),
            hit_question_scrollbar: Default::default(),
            hovered_question_item: None,
            question_scrollbar_dragging: false,
            last_question_click: None,
            inline_prompt_area: None,
            question_nav_buttons: Vec::new(),
            hovered_question_button: None,
            question_scroll_region: None,
            plan_mode_active: false,
            plan_mode_pending: None,
            deferred_session_mode: None,
            pending_extensions_fetch: false,
            in_dashboard_overlay: false,
            overlay_can_cycle: false,
            mcp_init_progress: None,
            acp_synced_generation: 0,
            hovered_permission_item: None,
            last_permission_click: None,
            permission_queue: VecDeque::new(),
            next_perm_req_id: 0,
            permission_stashed_prompt: None,
            plan_freeform_prefill_deferred: false,
            permission_stashed_pane: None,
            permission_pattern_edit: None,
            plan_approval_view: None,
            latest_inline_plan_content: None,
            plan_comments: Vec::new(),
            plan_next_comment_id: 0,
            casual_commenting_range: None,
            casual_editing_comment_id: None,
            casual_stashed_prompt: None,
            cancel_turn_view: None,
            cancel_turn_buttons: Vec::new(),
            cancel_subagents_preference: None,
            cancel_trigger_hint: None,
            rewind_state: None,
            rewind_points: None,
            inline_edit: None,
            pending_inline_resubmit: None,
            jump_state: None,
            timeline_rail: None,
            timeline_hover: None,
            timeline_hover_preview: None,
            session_agent_name: None,
            subagent_sessions: HashMap::new(),
            subagent_views: HashMap::new(),
            active_subagent: None,
            is_subagent_view: false,
            hit_subagent_frame_close: Default::default(),
            sharing_enabled: false,
            scheduler_background_loops: None,
            billing_surface_visible: false,
            usage_command_visible: true,
            input_log: crate::input_log::InputRingBuffer::new(),
            esc_pressed_at: None,
            rewind_suppress_deadline: None,
            pending_first_prompt: None,
            pending_fork_banner: None,
            loading_placeholder_id: None,
            pending_recap_entry: None,
            display_name: None,
            generated_session_title: None,
            title_unpin_committed: false,
            last_turn_summary: None,
            last_turn_summary_gen: 0,
            pending_effects: Vec::new(),
            paste_probe_in_flight: 0,
            deferred_send: None,
            pending_turn_end_reconcile: None,
            pending_cancel_resend: None,
            cancel_latency: None,
            expect_send_now_cancel: None,
            front_message_committed: true,
            optimistic_queue_ids: std::collections::HashSet::new(),
            send_now_awaiting_confirm: None,
            send_now_painted_blocks: std::collections::HashMap::new(),
            follow_without_jump_prompt_id: None,
            plugin_cta: PluginCtaState::default(),
            follow_ups: None,
            follow_up_shown_prompt_id: None,
            follow_up_chips: Vec::new(),
            hovered_follow_up_chip: None,
            follow_up_seen: HashMap::new(),
            follow_up_next_gen: 0,
            follow_up_pending: HashMap::new(),
            follow_up_pending_order: VecDeque::new(),
            pending_adoption_updates: Vec::new(),
        };
        let mode = if crate::appearance::cache::load_simple_mode() {
            InputMode::Simple
        } else {
            InputMode::Vim
        };
        view.set_input_mode(mode);
        view
    }
    /// Establish read-only child identity before a view is stored or opened.
    pub(crate) fn mark_as_subagent_view(&mut self) {
        self.is_subagent_view = true;
    }
    /// Register a child view and establish its read-only subagent identity.
    pub(crate) fn insert_subagent_view(
        &mut self,
        child_sid: String,
        mut child_view: Box<AgentView>,
    ) {
        child_view.mark_as_subagent_view();
        self.subagent_views.insert(child_sid, child_view);
    }
    /// Called at every turn-termination site; clears the wall anchor so a turn
    /// that reuses a prompt id cannot wall-max against a prior attempt.
    pub(crate) fn mark_turn_finished(&mut self, end: TurnEnd) {
        let now = Instant::now();
        self.turn_started_at = None;
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_paused_wall = std::time::Duration::ZERO;
        self.turn_start_ms = None;
        self.turn_start_ms_prompt = None;
        self.last_active_at = Some(now);
        if let Some(event) = self.settle_cancel(end, now) {
            echo_agent_telemetry::session_ctx::log_event(event);
        }
    }
    /// Cancel the running work and arm its latency anchor in one place, so the
    /// action and the `CancellationScope` it measures cannot drift apart.
    pub(crate) fn cancel_and_arm(&mut self, scope: CancellationScope, origin: CancelOrigin) {
        let now = Instant::now();
        match scope {
            CancellationScope::Turn => self.session.cancel_turn(&mut self.scrollback),
            CancellationScope::Compaction => self.session.cancel_compact_command(),
        }
        if origin == CancelOrigin::UserGesture && !self.is_subagent_view {
            self.cancel_latency
                .get_or_insert_with(|| CancelLatency::new(now, scope));
        }
    }
    /// Settle a pending user-cancel anchor into a `CancellationCompleted`.
    /// The anchor is consumed on both ends, so emission is once-by-construction.
    pub(crate) fn settle_cancel(
        &mut self,
        end: TurnEnd,
        now: Instant,
    ) -> Option<CancellationCompleted> {
        let pending = self.cancel_latency.take();
        match end {
            TurnEnd::Completed => pending.map(|p| CancellationCompleted {
                latency_ms: now.saturating_duration_since(p.requested_at).as_millis() as u64,
                scope: p.scope,
            }),
            TurnEnd::Aborted => None,
        }
    }
    /// Absorb a closing/replaced question view's open span into the turn's
    /// pause totals, on both clocks — a close site that updated only the
    /// `Instant` pause would resurface suspend time as worked time in
    /// [`honest_turn_elapsed`].
    pub(crate) fn record_question_pause(
        &mut self,
        qv: &crate::views::question_view::QuestionViewState,
    ) {
        self.turn_paused_duration += qv.opened_at.elapsed();
        self.turn_paused_wall +=
            wall_since_ms(qv.opened_at_wall_ms, chrono::Utc::now().timestamp_millis());
    }
    /// Invalidate and clear a minimal `/btw` lifecycle at a session boundary.
    pub(crate) fn clear_minimal_btw_lifecycle(&mut self) {
        crate::minimal_api::clear_minimal_btw(self);
    }
    /// Accept leftover `isReplay` after `loading_replay` clears. Long enough
    /// for FIFO drain of a foreign ACP head after the Unrelated firehose timeout.
    pub(crate) const LATE_REPLAY_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
    pub(crate) fn arm_late_replay_grace(&mut self) {
        self.late_replay_until = Some(std::time::Instant::now() + Self::LATE_REPLAY_GRACE);
    }
    /// Whether a replayed (`isReplay`) update should be applied right now: a `session/load`
    /// replay window is open, or the post-load grace for a late replay tail is still running
    /// (see `late_replay_until`). Anything else is a misrouted replay against a live transcript.
    pub(crate) fn accepts_replayed_update(&self) -> bool {
        self.session.loading_replay
            || self
                .late_replay_until
                .is_some_and(|deadline| std::time::Instant::now() < deadline)
    }
    /// Enter a `session/load` replay window: the fields coupled to that
    /// transition (incl. `cancel_latency`) reset together so no site drifts.
    pub(crate) fn begin_replay_window(&mut self) {
        self.clear_minimal_btw_lifecycle();
        self.session.loading_replay = true;
        self.replayed_terminal_prompts.clear();
        self.unexpected_replay_drops = 0;
        self.late_replay_until = None;
        self.running_wake_turn = None;
        self.finished_wake_prompts.clear();
        self.pending_cancel_resend = None;
        self.cancel_latency = None;
        self.pending_stop_hooks = None;
        self.clear_send_now_expectation();
        self.front_message_committed = true;
        self.optimistic_queue_ids.clear();
        self.send_now_awaiting_confirm = None;
        self.send_now_painted_blocks.clear();
        self.workflow_blocks.clear();
        self.workflow_run_revisions.clear();
        self.cleared_workflow_runs.clear();
        self.workflow_runs.clear();
    }
    /// Swap every replay-rebuilt field for a fresh value and return the old
    /// state. Reset together so stale revision gates cannot suppress the
    /// replayed updates.
    pub(crate) fn take_replay_rebuilt_state(&mut self) -> ReplayRebuiltState {
        let fresh = self.scrollback.fresh_continuation();
        ReplayRebuiltState {
            scrollback: std::mem::replace(&mut self.scrollback, fresh),
            tracker: std::mem::replace(
                &mut self.session.tracker,
                crate::acp::tracker::AcpUpdateTracker::new(),
            ),
            todo: std::mem::take(&mut self.todo),
            workflow_blocks: std::mem::take(&mut self.workflow_blocks),
            workflow_runs: std::mem::take(&mut self.workflow_runs),
            workflow_run_revisions: std::mem::take(&mut self.workflow_run_revisions),
            cleared_workflow_runs: std::mem::take(&mut self.cleared_workflow_runs),
        }
    }
    /// Put a taken [`ReplayRebuiltState`] back: the counterpart of
    /// [`Self::take_replay_rebuilt_state`] for callers whose rebuild failed
    /// and who would otherwise leave a bare view where content used to be.
    /// Used by the subagent restore path and the reload failure outcome.
    pub(crate) fn restore_replay_rebuilt_state(&mut self, mut taken: ReplayRebuiltState) {
        taken.scrollback.raise_id_floor(self.scrollback.id_floor());
        taken
            .scrollback
            .raise_invalidation_floor(self.scrollback.invalidation_generations());
        self.scrollback = taken.scrollback;
        self.session.tracker = taken.tracker;
        self.todo = taken.todo;
        self.workflow_blocks = taken.workflow_blocks;
        self.workflow_runs = taken.workflow_runs;
        self.workflow_run_revisions = taken.workflow_run_revisions;
        self.cleared_workflow_runs = taken.cleared_workflow_runs;
    }
    /// Open a reconnect reload window: stash the current transcript/tracker
    /// and point the live fields at fresh state for the incoming
    /// `session/load` replay. The transcript is NOT cleared — it stays
    /// recoverable until [`finish_session_reload`](Self::finish_session_reload)
    /// decides the outcome.
    pub(crate) fn begin_session_reload(&mut self, generation: u64) {
        self.dismiss_jump_picker();
        if let Some(prev) = self.session_reload.take() {
            tracing::warn!(
                generation,
                prev_generation = prev.generation,
                "session reload superseded without finalize; restoring previous stash first"
            );
            if self.apply_reload_outcome(prev, false) {
                crate::memory_release::release_retained_memory("reload-supersede");
            }
        }
        while self.scrollback.in_batch() {
            self.scrollback.end_batch();
        }
        if let Some(pid) = self.loading_placeholder_id.take() {
            self.scrollback.remove_entry(pid);
        }
        if let Some(rid) = self.pending_recap_entry.take() {
            self.scrollback.remove_entry(rid);
        }
        self.session.model_switch_pending = false;
        self.pending_adoption_updates.clear();
        let stash = self.take_replay_rebuilt_state();
        self.session_reload = Some(SessionReload {
            generation,
            stash,
            last_seen_event_id: self.last_seen_event_id.clone(),
            last_seen_event_seq: self.last_seen_event_seq,
            last_applied_event_seq: self.last_applied_event_seq,
            last_applied_echo_agent_event_seq: self.last_applied_echo_agent_event_seq,
            saw_replay: false,
            saw_todo_update: false,
            replayed_expiry_notices: Vec::new(),
        });
        self.loading_placeholder_id = Some(self.scrollback.push_block(
            crate::scrollback::block::RenderBlock::system("Reloading session after reconnect..."),
        ));
        self.scrollback.begin_batch();
        self.begin_replay_window();
    }
    /// Record that an `isReplay` update applied while a reload window is open.
    /// No-op otherwise.
    pub(crate) fn mark_reload_replay_seen(&mut self) {
        if let Some(reload) = self.session_reload.as_mut() {
            reload.saw_replay = true;
        }
    }
    /// Record a staged expiry notice for the keep-stash finalize dedupe. No-op outside a
    /// reconnect reload window (a fresh `session/load` has no stash to duplicate against).
    pub(crate) fn note_replayed_expiry_notice(
        &mut self,
        entry_id: crate::scrollback::entry::EntryId,
    ) {
        if let Some(reload) = self.session_reload.as_mut() {
            reload.replayed_expiry_notices.push(entry_id);
        }
    }
    /// Record that a Plan update applied while a reload window is open.
    /// No-op otherwise.
    pub(crate) fn mark_reload_todo_update(&mut self) {
        if let Some(reload) = self.session_reload.as_mut() {
            reload.saw_todo_update = true;
        }
    }
    /// Start a locally-tracked turn: enter TurnRunning with the turn-scoped
    /// bookkeeping every real turn start must apply, so no caller can miss
    /// it. Deliberately NOT used by server-initiated synthetic turns
    /// (auto-wake / actor runs): they never call `start_turn`.
    pub(crate) fn start_turn_boundary(&mut self, starting_prompt_id: Option<&str>) {
        if self
            .expect_send_now_cancel
            .as_deref()
            .is_some_and(|id| Some(id) != starting_prompt_id)
        {
            self.expect_send_now_cancel = None;
        }
        self.front_message_committed = false;
        self.pending_cancel_resend = None;
        self.cancel_latency = None;
        self.session.start_turn(&mut self.scrollback);
    }
    /// Adopt the in-flight turn another client is driving, conveyed by the
    /// `session/load` response meta (`echo.agent/runningPromptId`): enter
    /// TurnRunning and match subsequent live deltas. No user-prompt block is
    /// pushed — the turn's prompt and prior chunks arrived via the replay.
    pub(crate) fn adopt_running_prompt(&mut self, prompt_id: String) {
        self.start_turn_boundary(Some(&prompt_id));
        self.session.tracker.clear_user_echo_skip();
        self.front_message_committed = true;
        self.session.current_prompt_id = Some(prompt_id.clone());
        self.turn_started_at = Some(Instant::now());
        self.scrollback.enable_follow_with_preserve();
        self.flush_pending_follow_ups(&prompt_id);
    }
    /// Finalize any open reload window as FAILED, regardless of generation.
    ///
    /// For load initiations that take over the agent (fork/worktree/restore
    /// binding a new session): the stash belongs to the superseded
    /// pre-reconnect state, and an open window would corrupt the incoming
    /// load's batch/replay bookkeeping — and defer its results. The window's
    /// pending re-init completion later no-ops (generation gone).
    pub(crate) fn abort_session_reload(&mut self) {
        if let Some(reload) = self.session_reload.take()
            && self.apply_reload_outcome(reload, false)
        {
            crate::memory_release::release_retained_memory("reload-abort");
        }
    }
    /// Finalize the reload window opened for `generation`.
    ///
    /// Returns `false` (untouched state) when no window with that generation
    /// is open — the agent was never reloading, or a newer reconnect already
    /// superseded it.
    pub(crate) fn finish_session_reload(&mut self, generation: u64, success: bool) -> bool {
        match self.session_reload.take() {
            Some(reload) if reload.generation == generation => {
                if self.apply_reload_outcome(reload, success) {
                    crate::memory_release::release_retained_memory("reload-finalize");
                }
                true
            }
            Some(other) => {
                tracing::warn!(
                    generation,
                    open_generation = other.generation,
                    "ignoring session reload finalize for a superseded generation"
                );
                self.session_reload = Some(other);
                false
            }
            None => false,
        }
    }
    /// Whether a running prompt reported on a `session/load` (resume /
    /// reconnect) is adoptable by THIS agent: the pure synthetic-turn guard
    /// ([`acp_handler::should_adopt_running_prompt`]) AND not terminal-in-replay.
    /// A turn whose durable `TurnCompleted` already arrived in this load's replay
    /// (recorded in [`Self::replayed_terminal_prompts`]) has ended; adopting it
    /// would re-strand the viewer on "Waiting…".
    ///
    /// [`acp_handler::should_adopt_running_prompt`]: crate::app::acp_handler::should_adopt_running_prompt
    pub(crate) fn should_adopt_running_prompt(&self, prompt_id: &str) -> bool {
        crate::app::acp_handler::should_adopt_running_prompt(prompt_id)
            && !self.replayed_terminal_prompts.contains(prompt_id)
            && !self.is_rewound_prompt(prompt_id)
    }
    /// Wake turn in flight (streaming or cancelling) while the pane is idle.
    pub(crate) fn wake_turn_active(&self) -> bool {
        self.session.state.is_idle() && self.running_wake_turn.is_some()
    }
    /// Wake cancel sent and still waiting on its terminal. Pane stays idle.
    pub(crate) fn wake_turn_cancelling(&self) -> bool {
        self.session.state.is_idle()
            && self
                .running_wake_turn
                .as_ref()
                .is_some_and(|wake| wake.cancel_sent)
    }
    /// Single setter for [`RunningWakeTurn`]. No-op unless the pane is idle
    /// and not replaying; keeps an in-flight cancel marker for the same id.
    pub(crate) fn note_streaming_wake_turn(&mut self, prompt_id: &str) {
        if !self.session.state.is_idle() || self.session.loading_replay {
            return;
        }
        if self.finished_wake_prompts.contains(prompt_id) {
            return;
        }
        if self
            .running_wake_turn
            .as_ref()
            .is_some_and(|wake| wake.prompt_id == prompt_id)
        {
            return;
        }
        self.running_wake_turn = Some(super::RunningWakeTurn {
            prompt_id: prompt_id.to_string(),
            cancel_sent: false,
        });
    }
    /// Local turn, running `/compact`, or streaming wake not yet asked to stop.
    pub(crate) fn stoppable_activity_running(&self) -> bool {
        self.session.state.is_turn_running()
            || self.session.state.is_compact_running()
            || (self.wake_turn_active() && !self.wake_turn_cancelling())
    }
    /// Local or wake cancel still in flight.
    pub(crate) fn any_cancel_pending(&self) -> bool {
        self.session.state.is_cancelling() || self.wake_turn_cancelling()
    }
    /// Mark the wake cancel sent. No-op without a wake turn.
    pub(crate) fn mark_wake_cancel_sent(&mut self) {
        if let Some(wake) = self.running_wake_turn.as_mut() {
            wake.cancel_sent = true;
        }
    }
    /// Overlay stop: stamp the dashboard trigger if something stoppable is running.
    pub(crate) fn arm_dashboard_stop(&mut self) -> bool {
        if self.stoppable_activity_running() {
            self.cancel_trigger_hint = Some(crate::app::actions::CancelTrigger::DashboardStop);
            true
        } else {
            false
        }
    }
    /// Status-row chrome for a wake turn, or `None` when a local turn owns it.
    pub(crate) fn wake_display_state(&self) -> Option<&'static crate::app::agent::AgentState> {
        if !self.session.state.is_idle() {
            return None;
        }
        self.running_wake_turn.as_ref().map(|wake| {
            if wake.cancel_sent {
                &crate::app::agent::AgentState::TurnCancelling
            } else {
                &crate::app::agent::AgentState::TurnRunning
            }
        })
    }
    /// Finalize a reconnect-reload window and, iff the running prompt is
    /// adoptable, adopt it. Returns whether the window finalized.
    ///
    /// Adoption is gated by [`Self::should_adopt_running_prompt`] and ordered
    /// AFTER finalize so the finalize side effect (force-idle + window resolve)
    /// always runs even when adoption is skipped for a synthetic / non-adoptable
    /// / terminal-in-replay running id. The reconnect loop in `event_loop.rs`
    /// calls this per agent.
    pub(crate) fn finalize_reload_and_maybe_adopt(
        &mut self,
        generation: u64,
        ok: bool,
        running_prompt_id: Option<String>,
    ) -> bool {
        let finalized = self.finish_session_reload(generation, ok);
        if finalized
            && let Some(pid) = running_prompt_id
            && self.should_adopt_running_prompt(&pid)
        {
            self.adopt_running_prompt(pid);
        }
        finalized
    }
    /// Resolve a closed window per the [`SessionReload`] outcome trichotomy.
    ///
    /// Returns whether a heavy transient was dropped — the stashed pre-reload
    /// scrollback (success + full replay) or the staged partial replay
    /// (failure). The success+cursor branch *reuses* the stash and moves the
    /// tail entries into it: nothing multi-MB drops, so callers must NOT
    /// purge for it (a full-arena purge there would madvise away warm pages
    /// on the most common reconnect outcome, once per open tab).
    #[must_use = "purge retained memory iff a heavy transient dropped"]
    fn apply_reload_outcome(&mut self, reload: SessionReload, success: bool) -> bool {
        if let Some(pid) = self.loading_placeholder_id.take() {
            self.scrollback.remove_entry(pid);
        }
        let dropped_heavy;
        if success && reload.saw_replay {
            self.scrollback.end_batch();
            dropped_heavy = true;
        } else if success {
            let stash = reload.stash;
            let mut tail = std::mem::replace(&mut self.scrollback, stash.scrollback);
            let mut dedupe_budget: HashMap<String, usize> = HashMap::new();
            for entry_id in &reload.replayed_expiry_notices {
                let staged_text = (0..tail.len()).find_map(|i| {
                    let entry = tail.get(i)?;
                    if entry.id != *entry_id {
                        return None;
                    }
                    match &entry.block {
                        crate::scrollback::block::RenderBlock::System(block) => {
                            Some(block.text.clone())
                        }
                        _ => None,
                    }
                });
                let Some(staged_text) = staged_text else {
                    continue;
                };
                let budget = dedupe_budget.entry(staged_text.clone()).or_insert_with(|| {
                    (0..self.scrollback.len())
                        .filter(|i| {
                            matches!(
                                self.scrollback.get(*i).map(|e| &e.block),
                                Some(crate::scrollback::block::RenderBlock::System(block))
                                    if block.text == staged_text
                            )
                        })
                        .count()
                });
                if *budget > 0 {
                    *budget -= 1;
                    tail.remove_entry(*entry_id);
                }
            }
            self.scrollback.append_entries_from(tail);
            self.workflow_blocks.extend(stash.workflow_blocks);
            {
                let mut live_by_id: HashMap<String, _> = std::mem::take(&mut self.workflow_runs)
                    .into_iter()
                    .map(|run| (run.run_id.clone(), run))
                    .collect();
                let mut merged = Vec::with_capacity(stash.workflow_runs.len() + live_by_id.len());
                for run in stash.workflow_runs {
                    if let Some(live) = live_by_id.remove(&run.run_id) {
                        merged.push(live);
                    } else {
                        merged.push(run);
                    }
                }
                let mut live_only: Vec<_> = live_by_id.into_values().collect();
                live_only.sort_by_key(|run| run.received_at);
                merged.extend(live_only);
                self.cleared_workflow_runs
                    .extend(stash.cleared_workflow_runs);
                merged.retain(|run| !self.cleared_workflow_runs.contains(&run.run_id));
                self.workflow_runs = merged;
            }
            for (run_id, rev) in stash.workflow_run_revisions {
                self.workflow_run_revisions
                    .entry(run_id)
                    .and_modify(|live| *live = (*live).max(rev))
                    .or_insert(rev);
            }
            if !reload.saw_todo_update {
                self.todo = stash.todo;
            }
            dropped_heavy = false;
        } else {
            self.restore_replay_rebuilt_state(reload.stash);
            self.last_seen_event_id = reload.last_seen_event_id;
            self.last_seen_event_seq = reload.last_seen_event_seq;
            self.last_applied_event_seq = reload.last_applied_event_seq;
            self.last_applied_echo_agent_event_seq = reload.last_applied_echo_agent_event_seq;
            dropped_heavy = true;
        }
        self.session.loading_replay = false;
        if success {
            self.arm_late_replay_grace();
        } else {
            self.late_replay_until = None;
        }
        self.session.prompt_history_loading = false;
        self.session.tracker.clear_user_echo_skip();
        self.session.finish_turn(&mut self.scrollback);
        self.scrollback.finish_all_running();
        if let Some(id) = self.pending_recap_entry.take() {
            self.scrollback.remove_entry(id);
        }
        self.mark_turn_finished(TurnEnd::Aborted);
        self.activity_started_at = None;
        self.last_activity = None;
        self.reset_follow_ups_for_reload();
        dropped_heavy
    }
    /// Effective turn elapsed time, excluding time spent in question views
    /// (accumulated pauses plus the currently open one, on both clocks).
    pub fn turn_elapsed(&self) -> Option<std::time::Duration> {
        let instant_elapsed = self.turn_started_at?.elapsed();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut instant_paused = self.turn_paused_duration;
        let mut wall_paused = self.turn_paused_wall;
        if let Some(qv) = &self.question_view {
            instant_paused += qv.opened_at.elapsed();
            wall_paused += wall_since_ms(qv.opened_at_wall_ms, now_ms);
        }
        Some(honest_turn_elapsed(TurnElapsedParams {
            instant_elapsed,
            instant_paused,
            wall_anchor_ms: self.turn_start_ms,
            wall_paused,
            anchor_prompt: self.turn_start_ms_prompt.as_deref(),
            current_prompt: self.session.current_prompt_id.as_deref(),
            now_ms,
        }))
    }
    /// Turn activity for the status spinner, with the implicit "no activity"
    /// gap during a running inference turn resolved into an explicit
    /// [`WaitingReason`] so the spinner names *what* we're waiting on.
    ///
    /// The tracker already returns `Waiting(TaskOutput/TasksComplete/Sleep)`,
    /// and `Waiting(Subagent)` for a foreground `task` call from the moment it's
    /// issued. This fills in the remaining gap: if no tracker activity but a
    /// foreground subagent is registered as running, it's still `Subagent`
    /// (covers any window where the task tool call has cleared but the child is
    /// live); otherwise the model itself (`Model`). Bash turns keep `None` so
    /// the status line renders its own "Running…".
    ///
    /// For `Waiting(TaskOutput { task_ids, .. })`, also resolves a display
    /// `subject` from live bg-task / subagent state (description preferred,
    /// else command) so the spinner can read `{description}…`.
    pub(crate) fn resolve_turn_activity(&self) -> Option<crate::acp::tracker::TurnActivity> {
        self.resolve_turn_activity_unenriched()
            .map(|activity| self.enrich_waiting_activity(activity))
    }
    /// Wait detection without display enrichment — for predicates that need
    /// the wait's identity and must not churn with view-resolved display
    /// state; [`Self::resolve_turn_activity`] adds the display subject on top.
    pub(crate) fn resolve_turn_activity_unenriched(
        &self,
    ) -> Option<crate::acp::tracker::TurnActivity> {
        use crate::acp::tracker::{TurnActivity, WaitingReason};
        use crate::app::agent::AgentState;
        if let Some(activity) = self.session.turn_activity() {
            return Some(activity);
        }
        if !matches!(self.session.state, AgentState::TurnRunning) {
            return None;
        }
        if self.bash_turn {
            return None;
        }
        let reason = if self.has_running_foreground_subagent() {
            WaitingReason::subagent()
        } else {
            WaitingReason::Model
        };
        Some(TurnActivity::Waiting(reason))
    }
    /// Fill in a `TaskOutput` / `Subagent` wait's display subject.
    fn enrich_waiting_activity(
        &self,
        activity: crate::acp::tracker::TurnActivity,
    ) -> crate::acp::tracker::TurnActivity {
        use crate::acp::tracker::{TurnActivity, WaitingReason};
        match activity {
            TurnActivity::Waiting(WaitingReason::TaskOutput {
                task_ids, waits, ..
            }) => {
                let subject = self.subject_for_wait_tasks(&task_ids);
                TurnActivity::Waiting(WaitingReason::TaskOutput {
                    task_ids,
                    subject,
                    waits,
                })
            }
            TurnActivity::Waiting(WaitingReason::Subagent { .. }) => {
                TurnActivity::Waiting(WaitingReason::Subagent {
                    display: self.subagent_wait_subject(),
                })
            }
            other => other,
        }
    }
    /// Best user-facing name for the tasks being waited on.
    ///
    /// Uses the first resolvable subject. Multi-id waits always reflect the
    /// full `task_ids` length (`"first + N more"` with `N = task_ids.len()-1`)
    /// so partial resolution still reads as multi-task. Unknown ids → `None`
    /// (spinner falls back to the generic label).
    fn subject_for_wait_tasks(&self, task_ids: &[String]) -> Option<String> {
        use crate::acp::tracker::{MAX_ACTIVITY_SUBJECT_CHARS, clamp_activity_subject};
        if task_ids.is_empty() {
            return None;
        }
        let first = task_ids
            .iter()
            .find_map(|id| self.lookup_task_subject(id))?;
        if task_ids.len() == 1 {
            let first = clamp_activity_subject(&first);
            return (!first.is_empty()).then_some(first);
        }
        let n = task_ids.len() - 1;
        let suffix = format!(" + {n} more");
        let budget = MAX_ACTIVITY_SUBJECT_CHARS
            .saturating_sub(suffix.chars().count())
            .max(8);
        let base: String = first
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or(first.trim())
            .chars()
            .take(budget)
            .collect();
        if base.is_empty() {
            None
        } else {
            Some(format!("{base}{suffix}"))
        }
    }
    /// Resolve one task id to a display subject (description preferred, else
    /// a *short* command / subagent description).
    ///
    /// Long bare commands are intentionally not used as subjects — the spinner
    /// falls back to the generic `"Waiting on task output…"` instead of
    /// stuffing a wall of shell into the status line. Descriptions are kept
    /// but clamped by the caller via [`clamp_activity_subject`].
    fn lookup_task_subject(&self, task_id: &str) -> Option<String> {
        use crate::acp::tracker::MAX_ACTIVITY_SUBJECT_CHARS;
        fn first_nonempty_line(s: &str) -> &str {
            s.lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or(s)
        }
        if let Some(task) = self.session.bg_tasks.get(task_id) {
            if let Some(desc) = task
                .description
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(first_nonempty_line(desc).to_string());
            }
            let cmd = first_nonempty_line(task.command.trim());
            if !cmd.is_empty() && cmd.chars().count() <= MAX_ACTIVITY_SUBJECT_CHARS {
                return Some(cmd.to_string());
            }
        }
        if let Some(info) = self.subagent_sessions.get(task_id) {
            let desc = info.description.trim();
            if !desc.is_empty() {
                return Some(first_nonempty_line(desc).to_string());
            }
        }
        self.subagent_sessions
            .values()
            .find(|info| info.subagent_id.as_ref() == task_id)
            .and_then(|info| {
                let desc = info.description.trim();
                if desc.is_empty() {
                    None
                } else {
                    Some(first_nonempty_line(desc).to_string())
                }
            })
    }
    /// Whether a foreground subagent (`task`/`spawn_subagent`, not
    /// `run_in_background`) is currently running. The parent turn is blocked on
    /// it, so the spinner should read as a subagent wait.
    fn has_running_foreground_subagent(&self) -> bool {
        self.running_foreground_subagents().next().is_some()
    }
    /// The one predicate shared by the wait gate and its subject.
    fn running_foreground_subagents(
        &self,
    ) -> impl Iterator<Item = &crate::app::subagent::SubagentInfo> {
        self.subagent_sessions
            .values()
            .filter(|s| s.is_running() && !s.is_background && s.workflow_run_id.is_none())
    }
    /// Display subject for a foreground-subagent wait; `None` when no running
    /// child has a description.
    fn subagent_wait_subject(&self) -> Option<String> {
        use crate::acp::tracker::{MAX_ACTIVITY_SUBJECT_CHARS, clamp_activity_subject};
        let mut running: Vec<_> = self.running_foreground_subagents().collect();
        running.sort_by_key(|info| info.started_at);
        let description = running.iter().find_map(|info| {
            let (_, desc) = crate::app::subagent::parse_tag_prefix(info.description.trim());
            let desc = clamp_activity_subject(desc);
            (!desc.is_empty()).then_some(desc)
        })?;
        if running.len() > 1 {
            let n = running.len();
            return Some(budgeted_subject(
                &format!("{n} subagents: "),
                &description,
                &format!(" +{}", n - 1),
            ));
        }
        let activity = running
            .first()
            .and_then(|info| info.activity_label.as_deref())
            .map(|label| label.trim_end_matches('…').trim())
            .filter(|label| !label.is_empty());
        match activity {
            Some(activity) => {
                const PREFIX: &str = "Subagent (";
                const SUFFIX_HEAD: &str = "): ";
                const SUBAGENT_AFFIX_CHARS: usize = PREFIX.len() + SUFFIX_HEAD.len();
                const ACTIVITY_FLOOR: usize = 8;
                let desc_claim = description
                    .chars()
                    .count()
                    .min(MAX_ACTIVITY_SUBJECT_CHARS - SUBAGENT_AFFIX_CHARS - ACTIVITY_FLOOR);
                let activity: String = activity
                    .chars()
                    .take(MAX_ACTIVITY_SUBJECT_CHARS - SUBAGENT_AFFIX_CHARS - desc_claim)
                    .collect();
                Some(budgeted_subject(
                    PREFIX,
                    &description,
                    &format!("{SUFFIX_HEAD}{activity}"),
                ))
            }
            None => Some(budgeted_subject("Subagent: ", &description, "")),
        }
    }
    /// Update context state with a full snapshot from live callers.
    ///
    /// No-op for gateway/chat-kind sessions — local GetSessionInfo / sampler
    /// breakdowns must not populate the context bar (remote owns context).
    pub fn apply_full_context_info(&mut self, next: echo_agent_runtime::session::ContextInfo) {
        if self.chat_kind {
            self.context_state = None;
            return;
        }
        self.context_state = Some(next);
    }
    /// Update context state from a streaming notification carrying only
    /// `used` and `total` fields.
    ///
    /// No-op for gateway/chat-kind sessions (same policy as
    /// [`Self::apply_full_context_info`]).
    pub fn apply_context_used(&mut self, used: u64, total: u64) {
        if self.chat_kind {
            self.context_state = None;
            return;
        }
        let total = if total > 0 {
            total
        } else {
            self.context_state.as_ref().map(|s| s.total).unwrap_or(0)
        };
        match self.context_state.as_mut() {
            Some(snap) => {
                snap.used = used;
                if total > 0 {
                    snap.total = total;
                }
                snap.usage_pct = echo_agent_token_estimation::usage_percentage_u8(used, snap.total);
                snap.free_tokens = echo_agent_token_estimation::free_tokens(snap.total, used);
            }
            None => {
                self.context_state = Some(
                    echo_agent_runtime::session::ContextInfo::from_notification(used, total),
                );
            }
        }
    }
    /// Apply Build coding-credit balance only for non-chat agents.
    /// Gateway/chat-kind sessions keep credits unset so bars/warnings stay off.
    pub fn apply_credit_balance(
        &mut self,
        balance: Option<crate::views::credit_bar::CreditBalance>,
        auto_topup: Option<crate::views::credit_bar::AutoTopupInfo>,
    ) {
        if self.chat_kind {
            self.credit_balance = None;
            self.auto_topup = None;
            return;
        }
        self.credit_balance = balance;
        self.auto_topup = auto_topup;
    }
    /// Record a key event to the input flight recorder.
    ///
    /// Zero heap allocations — stores raw `Copy` types in the ring buffer.
    /// Formatting into strings happens only during dump (`snapshot_entries`).
    pub(crate) fn record_input(
        &mut self,
        key: &crossterm::event::KeyEvent,
        outcome: &InputOutcome,
    ) {
        use crate::input_log::{ActivePaneSnapshot, OutcomeSnapshot, RawInputEntry};
        use std::time::{SystemTime, UNIX_EPOCH};
        let delta = std::mem::take(&mut self.prompt.last_input_delta);
        let pane = match self.active_pane {
            ActivePane::Scrollback => ActivePaneSnapshot::Scrollback,
            ActivePane::Todo => ActivePaneSnapshot::Todo,
            ActivePane::Queue => ActivePaneSnapshot::Queue,
            ActivePane::Prompt => ActivePaneSnapshot::Prompt,
            ActivePane::Tasks => ActivePaneSnapshot::Tasks,
            ActivePane::Catalog => ActivePaneSnapshot::Catalog,
        };
        let outcome_snap = match outcome {
            InputOutcome::Changed | InputOutcome::ArmPending { .. } => OutcomeSnapshot::Changed,
            InputOutcome::Unchanged => OutcomeSnapshot::Unchanged,
            InputOutcome::Action(_)
            | InputOutcome::ActionThenForward(_)
            | InputOutcome::ActionPair(_, _) => OutcomeSnapshot::Action,
        };
        self.input_log.push(RawInputEntry {
            wall_ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            key_code: key.code,
            key_modifiers: key.modifiers,
            key_kind: key.kind,
            active_pane: pane,
            outcome: outcome_snap,
            cursor_before: delta.cursor_before,
            cursor_after: delta.cursor_after,
            text_len_before: delta.text_len_before,
            text_len_after: delta.text_len_after,
            sel_before: delta.had_selection_before,
            sel_after: delta.had_selection_after,
            textarea_changed: delta.textarea_changed,
        });
    }
    /// Set the sharing-enabled flag on this view and propagate it to the
    /// slash-command registry so the `/share` entry stays hidden/visible in
    /// lockstep with `AgentView::sharing_enabled`. Use this instead of
    /// mutating `sharing_enabled` directly when a new agent is created or a
    /// session is loaded, so the field and registry can't drift.
    pub fn set_sharing_enabled(&mut self, enabled: bool) {
        self.sharing_enabled = enabled;
        self.prompt
            .slash_controller
            .registry_mut()
            .set_share_visible(enabled);
    }
    /// Set [`Self::billing_surface_visible`] (see the field doc) and mirror it
    /// into this agent's slash controller, so the two can't drift.
    pub fn set_billing_surface_visible(&mut self, visible: bool) {
        self.billing_surface_visible = visible;
        self.prompt
            .slash_controller
            .set_billing_surface_visible(visible);
    }
    pub fn set_usage_command_visible(&mut self, visible: bool) {
        self.usage_command_visible = visible;
        self.prompt
            .slash_controller
            .set_usage_command_visible(visible);
    }
    /// Replace the restricted slash-command deny list in this agent's
    /// registry (e.g. `/usage` denied on the free / X Basic tiers). Deny
    /// wins over every `set_*_visible` gate.
    pub fn set_restricted_commands(&mut self, names: &[String]) {
        self.prompt.set_restricted_commands(names);
    }
    /// Show or hide the `/dashboard` slash command in this agent's registry.
    /// Driven by the dashboard feature flag
    /// (`crate::views::dashboard::dashboard_enabled()`) at agent-creation
    /// time — independent of leader mode.
    pub fn set_dashboard_visible(&mut self, visible: bool) {
        self.prompt
            .slash_controller
            .registry_mut()
            .set_dashboard_visible(visible);
    }
    /// Offer `/announcements` when session announcements (critical or promo) exist.
    pub fn set_has_session_announcements(&mut self, has: bool) {
        self.prompt
            .slash_controller
            .set_has_session_announcements(has);
    }
    /// One place for the app-scoped gates a new/adopted session inherits so the session-creation sites cannot drift.
    pub(crate) fn apply_app_scoped_gates(
        &mut self,
        sharing_enabled: bool,
        billing_surface_visible: bool,
        usage_command_visible: bool,
        chat_mode: bool,
        screen_mode: crate::app::ScreenMode,
        announcements: &[echo_agent_announcements::RemoteAnnouncement],
        restricted_commands: &[String],
    ) {
        self.set_sharing_enabled(sharing_enabled);
        self.set_billing_surface_visible(billing_surface_visible);
        self.set_usage_command_visible(usage_command_visible);
        self.app_chat_mode = chat_mode;
        self.prompt.set_screen_mode(screen_mode);
        self.set_dashboard_visible(crate::views::dashboard::dashboard_enabled());
        self.set_has_session_announcements(crate::views::announcements::has_session_announcements(
            announcements,
        ));
        self.set_restricted_commands(restricted_commands);
    }
    /// ACP `kind` for `echo.agent/session/rename`: the lane this session opened on.
    pub(crate) fn rename_kind(&self) -> echo_agent_runtime::session::unified_list::SessionKind {
        if self.conversation_entry {
            echo_agent_runtime::session::unified_list::SessionKind::Chat
        } else {
            echo_agent_runtime::session::unified_list::SessionKind::Build
        }
    }
    /// Show or hide the `/recap` slash command in this agent's registry.
    pub fn set_session_recap_available(&mut self, available: bool) {
        self.prompt.set_recap_visible(available);
    }
    /// Show or hide the `/voice` slash command in this agent's registry,
    /// gated on the runtime voice gate (GA default on; kill switch may hide).
    pub fn set_voice_mode_available(&mut self, available: bool) {
        self.prompt.set_voice_visible(available);
    }
}
/// Inputs for [`honest_turn_elapsed`]: the turn span and pause total measured
/// on each clock, plus the wire anchor's provenance. `now_ms` is injected so
/// tests control the wall clock.
struct TurnElapsedParams<'a> {
    instant_elapsed: std::time::Duration,
    instant_paused: std::time::Duration,
    /// `turnStartMs` wire anchor (UTC ms) and the prompt id it was stamped
    /// for; the anchor counts only when that id matches the running prompt
    /// (interleaved deltas can re-stamp it with another prompt's anchor).
    wall_anchor_ms: Option<i64>,
    wall_paused: std::time::Duration,
    anchor_prompt: Option<&'a str>,
    current_prompt: Option<&'a str>,
    now_ms: i64,
}
/// Turn elapsed for [`AgentView::turn_elapsed`], honest across OS suspends
/// (`Instant` pauses while the machine sleeps; the wall clock keeps
/// counting). Each span is netted against pauses measured on its own clock,
/// and the larger net wins; the tests below enumerate the guard cases.
fn honest_turn_elapsed(params: TurnElapsedParams<'_>) -> std::time::Duration {
    let instant_net = params.instant_elapsed.saturating_sub(params.instant_paused);
    let (Some(start_ms), Some(anchor_prompt), Some(current_prompt)) = (
        params.wall_anchor_ms,
        params.anchor_prompt,
        params.current_prompt,
    ) else {
        return instant_net;
    };
    if anchor_prompt != current_prompt {
        return instant_net;
    }
    let wall_net = wall_since_ms(start_ms, params.now_ms).saturating_sub(params.wall_paused);
    instant_net.max(wall_net)
}
/// Wall-clock span since `start_ms`, clamped to zero when `start_ms`
/// postdates `now_ms` (skew) so a wall span can never go negative.
fn wall_since_ms(start_ms: i64, now_ms: i64) -> std::time::Duration {
    std::time::Duration::from_millis(u64::try_from(now_ms.saturating_sub(start_ms)).unwrap_or(0))
}
const SUBJECT_DESC_FLOOR: usize = 8;
/// `{prefix}{description}{suffix}` with the description cut to the leftover
/// budget; a cut description ends with `…` inside that budget. Callers size
/// `prefix` + `suffix` so the composed subject stays within
/// `MAX_ACTIVITY_SUBJECT_CHARS` (debug-asserted on the result).
fn budgeted_subject(prefix: &str, description: &str, suffix: &str) -> String {
    use crate::acp::tracker::MAX_ACTIVITY_SUBJECT_CHARS;
    let budget = MAX_ACTIVITY_SUBJECT_CHARS
        .saturating_sub(prefix.chars().count() + suffix.chars().count())
        .max(SUBJECT_DESC_FLOOR);
    let description: String = if description.chars().count() <= budget {
        description.to_string()
    } else {
        let head: String = description.chars().take(budget - 1).collect();
        format!("{head}…")
    };
    let subject = format!("{prefix}{description}{suffix}");
    debug_assert!(
        subject.chars().count() <= MAX_ACTIVITY_SUBJECT_CHARS,
        "over-budget subject {subject:?}"
    );
    subject
}
#[cfg(test)]
mod honest_turn_elapsed_tests {
    use super::*;
    use std::time::Duration;
    const NOW_MS: i64 = 1_700_000_000_000;
    const MIN: u64 = 60;
    const HOUR: u64 = 3_600;
    /// Valid same-prompt anchor context with zero spans; tests override the
    /// fields under test via struct-update syntax.
    fn base() -> TurnElapsedParams<'static> {
        TurnElapsedParams {
            instant_elapsed: Duration::ZERO,
            instant_paused: Duration::ZERO,
            wall_anchor_ms: None,
            wall_paused: Duration::ZERO,
            anchor_prompt: Some("p1"),
            current_prompt: Some("p1"),
            now_ms: NOW_MS,
        }
    }
    #[test]
    fn no_wall_anchor_keeps_instant_net() {
        assert_eq!(
            honest_turn_elapsed(TurnElapsedParams {
                instant_elapsed: Duration::from_secs(5 * MIN),
                instant_paused: Duration::from_secs(MIN),
                ..base()
            }),
            Duration::from_secs(4 * MIN)
        );
    }
    #[test]
    fn suspend_outside_questions_defers_to_wall_net() {
        assert_eq!(
            honest_turn_elapsed(TurnElapsedParams {
                instant_elapsed: Duration::from_secs(4 * MIN),
                wall_anchor_ms: Some(NOW_MS - 2 * HOUR as i64 * 1_000),
                ..base()
            }),
            Duration::from_secs(2 * HOUR)
        );
    }
    #[test]
    fn suspend_while_question_open_is_not_worked_time() {
        assert_eq!(
            honest_turn_elapsed(TurnElapsedParams {
                instant_elapsed: Duration::from_secs(10 * MIN),
                instant_paused: Duration::from_secs(5 * MIN),
                wall_anchor_ms: Some(NOW_MS - (2 * HOUR as i64 + 10 * MIN as i64) * 1_000),
                wall_paused: Duration::from_secs(2 * HOUR + 5 * MIN),
                ..base()
            }),
            Duration::from_secs(5 * MIN)
        );
    }
    #[test]
    fn instant_net_bounds_below_after_backward_wall_jump() {
        assert_eq!(
            honest_turn_elapsed(TurnElapsedParams {
                instant_elapsed: Duration::from_secs(5 * MIN),
                wall_anchor_ms: Some(NOW_MS - 1_000),
                ..base()
            }),
            Duration::from_secs(5 * MIN)
        );
    }
    #[test]
    fn foreign_prompt_anchor_falls_back_to_instant_net() {
        assert_eq!(
            honest_turn_elapsed(TurnElapsedParams {
                instant_elapsed: Duration::from_secs(10 * MIN),
                instant_paused: Duration::from_secs(4 * MIN),
                wall_anchor_ms: Some(NOW_MS - 2 * HOUR as i64 * 1_000),
                anchor_prompt: Some("p-other"),
                ..base()
            }),
            Duration::from_secs(6 * MIN)
        );
    }
    #[test]
    fn missing_current_prompt_ignores_anchor() {
        assert_eq!(
            honest_turn_elapsed(TurnElapsedParams {
                instant_elapsed: Duration::from_secs(MIN),
                wall_anchor_ms: Some(NOW_MS - 2 * HOUR as i64 * 1_000),
                current_prompt: None,
                ..base()
            }),
            Duration::from_secs(MIN)
        );
    }
    #[test]
    fn future_wall_anchor_is_ignored() {
        assert_eq!(
            honest_turn_elapsed(TurnElapsedParams {
                instant_elapsed: Duration::from_secs(MIN),
                wall_anchor_ms: Some(NOW_MS + 60_000),
                ..base()
            }),
            Duration::from_secs(MIN)
        );
    }
    #[test]
    fn turn_elapsed_reflects_wall_span_for_current_prompt() {
        let mut view = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        view.turn_started_at = Some(Instant::now());
        view.turn_start_ms = Some(chrono::Utc::now().timestamp_millis() - 60_000);
        view.turn_start_ms_prompt = Some("p1".to_string());
        view.session.current_prompt_id = Some("p1".to_string());
        assert!(view.turn_elapsed().unwrap() >= Duration::from_secs(59));
    }
    #[test]
    fn turn_elapsed_nets_wall_pauses_against_wall_span() {
        let mut view = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        view.turn_started_at = Some(Instant::now());
        view.turn_start_ms = Some(chrono::Utc::now().timestamp_millis() - 60_000);
        view.turn_start_ms_prompt = Some("p1".to_string());
        view.session.current_prompt_id = Some("p1".to_string());
        view.turn_paused_wall = Duration::from_secs(45);
        let elapsed = view.turn_elapsed().unwrap();
        assert!(elapsed >= Duration::from_secs(14) && elapsed <= Duration::from_secs(16));
    }
}
#[cfg(test)]
mod advance_last_seen_event_id_tests {
    use super::*;
    #[test]
    fn unparseable_id_preserves_known_highwater_seq() {
        let mut view = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        view.advance_last_seen_event_id("sess-1-7".into(), Some(7));
        assert_eq!(view.last_seen_event_id.as_deref(), Some("sess-1-7"));
        assert_eq!(view.last_seen_event_seq, Some(7));
        view.advance_last_seen_event_id("sess-1-opaque".into(), None);
        assert_eq!(view.last_seen_event_id.as_deref(), Some("sess-1-opaque"));
        assert_eq!(
            view.last_seen_event_seq,
            Some(7),
            "known highwater must survive an unparseable id"
        );
        view.advance_last_seen_event_id("sess-1-3".into(), Some(3));
        assert_eq!(view.last_seen_event_id.as_deref(), Some("sess-1-opaque"));
        assert_eq!(view.last_seen_event_seq, Some(7));
        view.advance_last_seen_event_id("sess-1-9".into(), Some(9));
        assert_eq!(view.last_seen_event_id.as_deref(), Some("sess-1-9"));
        assert_eq!(view.last_seen_event_seq, Some(9));
    }
}
#[cfg(test)]
mod resolve_turn_activity_tests {
    use super::*;
    use crate::acp::tracker::{TurnActivity, WaitingReason};
    use crate::app::agent::AgentState;
    fn running_view() -> AgentView {
        let mut view = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        view.session.state = AgentState::TurnRunning;
        view
    }
    #[test]
    fn idle_turn_has_no_activity() {
        let view = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        assert_eq!(view.resolve_turn_activity(), None);
    }
    #[test]
    fn running_with_no_stream_waits_on_model() {
        let view = running_view();
        assert_eq!(
            view.resolve_turn_activity(),
            Some(TurnActivity::Waiting(WaitingReason::Model))
        );
    }
    #[test]
    fn bash_turn_stays_none() {
        let mut view = running_view();
        view.bash_turn = true;
        assert_eq!(view.resolve_turn_activity(), None);
    }
    #[test]
    fn real_activity_passes_through() {
        let mut view = running_view();
        view.session
            .set_compaction_activity(Some(TurnActivity::AutoCompacting));
        assert_eq!(
            view.resolve_turn_activity(),
            Some(TurnActivity::AutoCompacting)
        );
    }
    fn running_child(description: &str) -> crate::app::subagent::SubagentInfo {
        let mut info = crate::app::agent_view::test_fixtures::running_subagent_info("child");
        info.description = std::sync::Arc::from(description);
        info
    }
    #[test]
    fn subagent_wait_names_single_child() {
        let mut view = running_view();
        view.subagent_sessions
            .insert("child-1".into(), running_child("scan src/"));
        let activity = view.resolve_turn_activity().expect("waiting activity");
        assert_eq!(activity.as_label(), "waiting_subagent");
        let TurnActivity::Waiting(reason) = activity else {
            panic!("expected waiting activity");
        };
        assert_eq!(reason.label(), "Subagent: scan src/…");
    }
    #[test]
    fn subagent_wait_strips_description_tag_prefix() {
        let mut view = running_view();
        view.subagent_sessions
            .insert("child-1".into(), running_child("[reviewer] check lints"));
        let Some(TurnActivity::Waiting(reason)) = view.resolve_turn_activity() else {
            panic!("expected waiting activity");
        };
        assert_eq!(reason.label(), "Subagent: check lints…");
        let mut earlier = running_child("[explore] scan src/");
        earlier.started_at = std::time::Instant::now() - std::time::Duration::from_secs(5);
        view.subagent_sessions.insert("child-0".into(), earlier);
        let Some(TurnActivity::Waiting(reason)) = view.resolve_turn_activity() else {
            panic!("expected waiting activity");
        };
        assert_eq!(reason.label(), "2 subagents: scan src/ +1…");
    }
    #[test]
    fn subagent_wait_composes_child_activity() {
        let mut view = running_view();
        let mut info = running_child("fix flaky test");
        info.activity_label = Some("Writing subagent prompt…".into());
        view.subagent_sessions.insert("child-1".into(), info);
        let Some(TurnActivity::Waiting(reason)) = view.resolve_turn_activity() else {
            panic!("expected waiting activity");
        };
        assert_eq!(reason.label(), "Subagent (fix flaky test): Writing subag…");
    }
    #[test]
    fn subagent_wait_long_description_keeps_activity_visible() {
        let mut view = running_view();
        let mut info = running_child("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH");
        info.activity_label = Some("Running: cargo test".into());
        view.subagent_sessions.insert("child-1".into(), info);
        let Some(TurnActivity::Waiting(reason)) = view.resolve_turn_activity() else {
            panic!("expected waiting activity");
        };
        assert_eq!(reason.label(), "Subagent (abcdefghijklmnopqr…): Running:…");
    }
    /// QA case: long description + long activity. The description gets first
    /// claim on the budget (inner ellipsis when cut) and the activity keeps
    /// at least its first 8 chars.
    #[test]
    fn subagent_wait_long_desc_and_activity_gives_description_priority() {
        let mut view = running_view();
        let mut info = running_child("summarize scratchpad findings into notes");
        info.activity_label = Some("Waiting for response…".into());
        view.subagent_sessions.insert("child-1".into(), info);
        let Some(TurnActivity::Waiting(reason)) = view.resolve_turn_activity() else {
            panic!("expected waiting activity");
        };
        let label = reason.label();
        assert_eq!(label, "Subagent (summarize scratchp…): Waiting…");
        assert!(label.chars().count() <= 41, "label too long: {label:?}");
    }
    #[test]
    fn subagent_wait_multi_child_truncated_description_gets_inner_ellipsis() {
        let mut view = running_view();
        let mut earlier = running_child("audit every dashboard panel for drift");
        earlier.started_at = std::time::Instant::now() - std::time::Duration::from_secs(5);
        view.subagent_sessions.insert("child-1".into(), earlier);
        view.subagent_sessions
            .insert("child-2".into(), running_child("fix tests"));
        let Some(TurnActivity::Waiting(reason)) = view.resolve_turn_activity() else {
            panic!("expected waiting activity");
        };
        assert_eq!(reason.label(), "2 subagents: audit every dashboard p… +1…");
    }
    #[test]
    fn subagent_wait_counts_parallel_children() {
        let mut view = running_view();
        let mut earlier = running_child("scan src/");
        earlier.started_at = std::time::Instant::now() - std::time::Duration::from_secs(5);
        view.subagent_sessions.insert("child-1".into(), earlier);
        view.subagent_sessions
            .insert("child-2".into(), running_child("fix tests"));
        let Some(TurnActivity::Waiting(reason)) = view.resolve_turn_activity() else {
            panic!("expected waiting activity");
        };
        assert_eq!(reason.label(), "2 subagents: scan src/ +1…");
    }
    #[test]
    fn unenriched_wait_matches_variant_without_subject() {
        use crate::acp::tracker::WaitingReason;
        let mut view = running_view();
        view.subagent_sessions
            .insert("child-1".into(), running_child("scan src/"));
        assert_eq!(
            view.resolve_turn_activity_unenriched(),
            Some(TurnActivity::Waiting(WaitingReason::subagent()))
        );
        assert!(view.is_waiting_on_subagent());
        let Some(TurnActivity::Waiting(WaitingReason::Subagent { display })) =
            view.resolve_turn_activity()
        else {
            panic!("expected subagent wait");
        };
        assert_eq!(display.as_deref(), Some("Subagent: scan src/"));
    }
    #[test]
    fn subagent_wait_labels_bounded_for_adversarial_inputs() {
        use crate::acp::tracker::{MAX_ACTIVITY_SUBJECT_CHARS, WaitingReason};
        let long_desc = "x".repeat(500);
        let descriptions = [
            "",
            "d",
            long_desc.as_str(),
            "line one\nline two\nline three",
            "[tag]",
        ];
        let activities = [
            None,
            Some("Run".to_string()),
            Some("a".repeat(40)),
            Some("b".repeat(50)),
        ];
        let mut cases = 0;
        for n in [1usize, 3] {
            for desc in descriptions {
                for activity in &activities {
                    cases += 1;
                    let mut view = running_view();
                    for i in 0..n {
                        let mut info = running_child(desc);
                        info.started_at = std::time::Instant::now()
                            - std::time::Duration::from_secs((n - i) as u64);
                        if i == 0 {
                            info.activity_label = activity.clone();
                        }
                        view.subagent_sessions.insert(format!("child-{i}"), info);
                    }
                    let Some(TurnActivity::Waiting(reason)) = view.resolve_turn_activity() else {
                        panic!("expected waiting activity");
                    };
                    if let WaitingReason::Subagent {
                        display: Some(display),
                    } = &reason
                    {
                        assert!(
                            display.chars().count() <= MAX_ACTIVITY_SUBJECT_CHARS,
                            "unbounded display {display:?} (desc {} chars, activity {activity:?}, n {n})",
                            desc.len(),
                        );
                    }
                    let label = reason.label();
                    assert!(
                        label.chars().count() <= MAX_ACTIVITY_SUBJECT_CHARS + 1,
                        "label too long: {label:?}"
                    );
                    if n == 1
                        && let Some(activity) = activity
                        && matches!(&reason, WaitingReason::Subagent { display: Some(_) })
                    {
                        let head: String = activity.chars().take(8).collect();
                        assert!(
                            label.contains(&head),
                            "activity head {head:?} missing from {label:?}"
                        );
                    }
                }
            }
        }
        assert_eq!(cases, 40);
    }
    #[test]
    fn subagent_wait_falls_back_without_description() {
        let mut view = running_view();
        view.subagent_sessions
            .insert("child-1".into(), running_child("  "));
        let Some(TurnActivity::Waiting(reason)) = view.resolve_turn_activity() else {
            panic!("expected waiting activity");
        };
        assert_eq!(reason.label(), "Waiting on subagent…");
    }
    #[test]
    fn tracker_subagent_wait_is_enriched() {
        use crate::acp::meta::NotificationMeta;
        use agent_client_protocol as acp;
        use std::sync::Arc;
        let mut view = running_view();
        view.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(acp::ToolCallId::new(Arc::from("task-tc-1")), "task")
                    .kind(acp::ToolKind::Other)
                    .status(acp::ToolCallStatus::Pending)
                    .content(vec![])
                    .locations(vec![]),
            ),
            &NotificationMeta::default(),
            &mut view.scrollback,
        );
        view.subagent_sessions
            .insert("child-1".into(), running_child("scan src/"));
        let Some(TurnActivity::Waiting(reason)) = view.resolve_turn_activity() else {
            panic!("expected waiting activity");
        };
        assert_eq!(reason.label(), "Subagent: scan src/…");
    }
    /// When waiting on task output, the spinner subject is the bg task's
    /// description (preferred over the raw command).
    #[test]
    fn task_output_wait_uses_bg_task_description() {
        use crate::acp::meta::NotificationMeta;
        use crate::app::agent::{BgTaskState, BgTaskStatus};
        use agent_client_protocol as acp;
        use std::sync::Arc;
        use std::time::SystemTime;
        let mut view = running_view();
        view.session.bg_tasks.insert(
            "bg-1".into(),
            BgTaskState {
                task_id: "bg-1".into(),
                tool_call_id: "tc-1".into(),
                command: "cargo test --release".into(),
                description: Some("run release tests".into()),
                cwd: String::new(),
                output_file: String::new(),
                status: BgTaskStatus::Running,
                start_time: SystemTime::now(),
                end_time: None,
                exit_code: None,
                signal: None,
                stdout: String::new(),
                stdout_line_count: 0,
                truncated: false,
                pending_kill: false,
                kill_requested_at: None,
                scrollback_entry_id: None,
                is_monitor: false,
                restored_from_replay: false,
            },
        );
        let meta = NotificationMeta::default();
        view.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(
                    acp::ToolCallId::new(Arc::from("wait-1")),
                    "get_command_or_subagent_output",
                )
                .kind(acp::ToolKind::Other)
                .status(acp::ToolCallStatus::Pending)
                .content(vec![])
                .locations(vec![]),
            ),
            &meta,
            &mut view.scrollback,
        );
        view.session.handle_update(
            acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                acp::ToolCallId::new(Arc::from("wait-1")),
                acp::ToolCallUpdateFields::new().raw_input(Some(serde_json::json!({
                    "task_ids": ["bg-1"],
                    "timeout_ms": 30_000,
                }))),
            )),
            &meta,
            &mut view.scrollback,
        );
        let activity = view.resolve_turn_activity();
        assert_eq!(
            activity,
            Some(TurnActivity::Waiting(WaitingReason::TaskOutput {
                task_ids: vec!["bg-1".into()],
                subject: Some("run release tests".into()),
                waits: true,
            }))
        );
        assert_eq!(activity.as_ref().unwrap().as_label(), "waiting_task_output");
        let TurnActivity::Waiting(reason) = activity.unwrap() else {
            panic!("expected waiting activity");
        };
        assert_eq!(reason.label(), "run release tests…");
    }
    /// Without a description, a short command is used as the subject.
    #[test]
    fn task_output_wait_falls_back_to_short_command() {
        use crate::acp::meta::NotificationMeta;
        use crate::app::agent::{BgTaskState, BgTaskStatus};
        use agent_client_protocol as acp;
        use std::sync::Arc;
        use std::time::SystemTime;
        let mut view = running_view();
        view.session.bg_tasks.insert(
            "bg-2".into(),
            BgTaskState {
                task_id: "bg-2".into(),
                tool_call_id: "tc-2".into(),
                command: "sleep 30".into(),
                description: None,
                cwd: String::new(),
                output_file: String::new(),
                status: BgTaskStatus::Running,
                start_time: SystemTime::now(),
                end_time: None,
                exit_code: None,
                signal: None,
                stdout: String::new(),
                stdout_line_count: 0,
                truncated: false,
                pending_kill: false,
                kill_requested_at: None,
                scrollback_entry_id: None,
                is_monitor: false,
                restored_from_replay: false,
            },
        );
        let meta = NotificationMeta::default();
        view.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(acp::ToolCallId::new(Arc::from("wait-2")), "get_task_output")
                    .kind(acp::ToolKind::Other)
                    .status(acp::ToolCallStatus::Pending)
                    .content(vec![])
                    .raw_input(Some(serde_json::json!({
                        "task_ids": ["bg-2"],
                        "timeout_ms": 5_000,
                    })))
                    .locations(vec![]),
            ),
            &meta,
            &mut view.scrollback,
        );
        let activity = view.resolve_turn_activity().expect("activity");
        let TurnActivity::Waiting(reason) = activity else {
            panic!("expected waiting: {activity:?}");
        };
        assert_eq!(reason.label(), "sleep 30…");
    }
    /// Multi-id waits use full task_ids.len() for "+ N more", not just resolved count.
    #[test]
    fn task_output_wait_multi_id_uses_full_task_count() {
        use crate::acp::meta::NotificationMeta;
        use crate::app::agent::{BgTaskState, BgTaskStatus};
        use agent_client_protocol as acp;
        use std::sync::Arc;
        use std::time::SystemTime;
        let mut view = running_view();
        view.session.bg_tasks.insert(
            "bg-a".into(),
            BgTaskState {
                task_id: "bg-a".into(),
                tool_call_id: "tc-a".into(),
                command: "echo a".into(),
                description: Some("alpha task".into()),
                cwd: String::new(),
                output_file: String::new(),
                status: BgTaskStatus::Running,
                start_time: SystemTime::now(),
                end_time: None,
                exit_code: None,
                signal: None,
                stdout: String::new(),
                stdout_line_count: 0,
                truncated: false,
                pending_kill: false,
                kill_requested_at: None,
                scrollback_entry_id: None,
                is_monitor: false,
                restored_from_replay: false,
            },
        );
        let meta = NotificationMeta::default();
        view.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(
                    acp::ToolCallId::new(Arc::from("wait-multi")),
                    "get_task_output",
                )
                .kind(acp::ToolKind::Other)
                .status(acp::ToolCallStatus::Pending)
                .content(vec![])
                .raw_input(Some(serde_json::json!({
                    "task_ids": ["bg-a", "missing-b", "missing-c"],
                    "timeout_ms": 5_000,
                })))
                .locations(vec![]),
            ),
            &meta,
            &mut view.scrollback,
        );
        let activity = view.resolve_turn_activity().expect("activity");
        let TurnActivity::Waiting(reason) = activity else {
            panic!("expected waiting: {activity:?}");
        };
        assert_eq!(
            reason.label(),
            "alpha task + 2 more…",
            "N more is based on full task_ids length, not resolved count"
        );
    }
    /// Long first subjects still keep the multi-task suffix after clamping.
    #[test]
    fn task_output_wait_multi_id_preserves_suffix_when_first_is_long() {
        use crate::acp::meta::NotificationMeta;
        use crate::acp::tracker::MAX_ACTIVITY_SUBJECT_CHARS;
        use crate::app::agent::{BgTaskState, BgTaskStatus};
        use agent_client_protocol as acp;
        use std::sync::Arc;
        use std::time::SystemTime;
        let long_desc = "L".repeat(80);
        let mut view = running_view();
        view.session.bg_tasks.insert(
            "bg-long".into(),
            BgTaskState {
                task_id: "bg-long".into(),
                tool_call_id: "tc-long".into(),
                command: "echo long".into(),
                description: Some(long_desc),
                cwd: String::new(),
                output_file: String::new(),
                status: BgTaskStatus::Running,
                start_time: SystemTime::now(),
                end_time: None,
                exit_code: None,
                signal: None,
                stdout: String::new(),
                stdout_line_count: 0,
                truncated: false,
                pending_kill: false,
                kill_requested_at: None,
                scrollback_entry_id: None,
                is_monitor: false,
                restored_from_replay: false,
            },
        );
        let meta = NotificationMeta::default();
        view.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(
                    acp::ToolCallId::new(Arc::from("wait-long-multi")),
                    "get_task_output",
                )
                .kind(acp::ToolKind::Other)
                .status(acp::ToolCallStatus::Pending)
                .content(vec![])
                .raw_input(Some(serde_json::json!({
                    "task_ids": ["bg-long", "missing-b"],
                    "timeout_ms": 5_000,
                })))
                .locations(vec![]),
            ),
            &meta,
            &mut view.scrollback,
        );
        let activity = view.resolve_turn_activity().expect("activity");
        let TurnActivity::Waiting(reason) = activity else {
            panic!("expected waiting: {activity:?}");
        };
        let label = reason.label();
        assert!(
            label.contains(" + 1 more"),
            "multi-task suffix must survive clamp: {label}"
        );
        assert!(label.ends_with('…'));
        let body = label.strip_suffix('…').unwrap();
        assert!(
            body.chars().count() <= MAX_ACTIVITY_SUBJECT_CHARS + 20,
            "unexpectedly long body: {body}"
        );
    }
    /// get_task_output often passes subagent_id, not the child_session_id map key.
    #[test]
    fn task_output_wait_resolves_subagent_by_subagent_id() {
        use crate::acp::meta::NotificationMeta;
        use crate::app::subagent::SubagentInfo;
        use agent_client_protocol as acp;
        use std::sync::Arc;
        use std::time::Instant;
        let mut view = running_view();
        let now = Instant::now();
        view.subagent_sessions.insert(
            "child-session-xyz".into(),
            SubagentInfo {
                subagent_id: Arc::from("sub-id-42"),
                child_session_id: Arc::from("child-session-xyz"),
                description: Arc::from("explore the auth module"),
                subagent_type: Arc::from("explore"),
                persona: None,
                role: None,
                model: None,
                context_source: None,
                resumed_from: None,
                capability_mode: None,
                workflow_run_id: None,
                context_normalized: false,
                parent_prompt_id: None,
                started_at: now,
                last_progress_at: now,
                finished: false,
                status: None,
                error: None,
                duration_ms: None,
                tool_calls: None,
                turns: None,
                turn_count: None,
                tool_call_count: None,
                tokens_used: None,
                context_window_tokens: None,
                context_usage_pct: None,
                tools_used: vec![],
                error_count: None,
                activity_label: None,
                is_background: true,
                pending_kill: false,
                kill_requested_at: None,
                scrollback_entry_id: None,
                prompt: None,
                child_cwd: None,
                worktree_path: None,
                transcript: Default::default(),
            },
        );
        let meta = NotificationMeta::default();
        view.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(
                    acp::ToolCallId::new(Arc::from("wait-sub")),
                    "get_command_or_subagent_output",
                )
                .kind(acp::ToolKind::Other)
                .status(acp::ToolCallStatus::Pending)
                .content(vec![])
                .raw_input(Some(serde_json::json!({
                    "task_ids": ["sub-id-42"],
                    "timeout_ms": 10_000,
                })))
                .locations(vec![]),
            ),
            &meta,
            &mut view.scrollback,
        );
        let activity = view.resolve_turn_activity().expect("activity");
        let TurnActivity::Waiting(reason) = activity else {
            panic!("expected waiting: {activity:?}");
        };
        assert_eq!(reason.label(), "explore the auth module…");
    }
    /// Long bare commands are not used as subjects — keep the original label.
    #[test]
    fn task_output_wait_long_command_keeps_generic_label() {
        use crate::acp::meta::NotificationMeta;
        use crate::app::agent::{BgTaskState, BgTaskStatus};
        use agent_client_protocol as acp;
        use std::sync::Arc;
        use std::time::SystemTime;
        let long_cmd = "cargo test --release --workspace --all-features -- --nocapture".to_string();
        assert!(
            long_cmd.chars().count() > 40,
            "fixture must exceed the short-command threshold"
        );
        let mut view = running_view();
        view.session.bg_tasks.insert(
            "bg-3".into(),
            BgTaskState {
                task_id: "bg-3".into(),
                tool_call_id: "tc-3".into(),
                command: long_cmd,
                description: None,
                cwd: String::new(),
                output_file: String::new(),
                status: BgTaskStatus::Running,
                start_time: SystemTime::now(),
                end_time: None,
                exit_code: None,
                signal: None,
                stdout: String::new(),
                stdout_line_count: 0,
                truncated: false,
                pending_kill: false,
                kill_requested_at: None,
                scrollback_entry_id: None,
                is_monitor: false,
                restored_from_replay: false,
            },
        );
        let meta = NotificationMeta::default();
        view.session.handle_update(
            acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(acp::ToolCallId::new(Arc::from("wait-3")), "get_task_output")
                    .kind(acp::ToolKind::Other)
                    .status(acp::ToolCallStatus::Pending)
                    .content(vec![])
                    .raw_input(Some(serde_json::json!({
                        "task_ids": ["bg-3"],
                        "timeout_ms": 5_000,
                    })))
                    .locations(vec![]),
            ),
            &meta,
            &mut view.scrollback,
        );
        let activity = view.resolve_turn_activity().expect("activity");
        let TurnActivity::Waiting(reason) = activity else {
            panic!("expected waiting: {activity:?}");
        };
        assert_eq!(
            reason.label(),
            "Waiting on task output…",
            "long command without description must not become the spinner subject"
        );
        assert_eq!(
            reason,
            WaitingReason::TaskOutput {
                task_ids: vec!["bg-3".into()],
                subject: None,
                waits: true,
            }
        );
    }
}
#[cfg(test)]
mod status_window_tests {
    use super::super::test_agent_view;
    #[test]
    fn start_turn_boundary_enters_turn_running() {
        let mut agent = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        agent.pending_cancel_resend = Some(crate::app::agent_view::PendingCancelResend {
            prompt_id: Some("old".into()),
            sent_at: std::time::Instant::now(),
            attempts: 1,
            confirmed: false,
            cancel_subagents: true,
            trigger: crate::app::actions::CancelTrigger::Esc,
        });
        agent.start_turn_boundary(None);
        assert!(agent.session.state.is_turn_running());
        assert!(agent.pending_cancel_resend.is_none());
    }
    #[test]
    fn adopt_running_prompt_marks_front_committed() {
        let mut agent = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        agent.start_turn_boundary(Some("p-local"));
        assert!(!agent.front_message_committed);
        agent.adopt_running_prompt("p-run".into());
        assert!(agent.front_message_committed);
        assert!(agent.expects_send_now_cancel());
    }
    #[test]
    fn session_rebind_and_replay_invalidate_minimal_btw() {
        let mut agent = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        let old_request = crate::minimal_api::start_minimal_btw(&mut agent, "old question".into());
        agent.bind_session_id(agent_client_protocol::SessionId::new("s2"));
        assert!(agent.btw_state.is_none());
        assert!(agent.minimal_btw_lifecycle.is_none());
        assert!(!crate::minimal_api::finish_minimal_btw(
            &mut agent,
            old_request,
            Ok("old answer".into())
        ));
        assert!(agent.btw_state.is_none());
        let replay_request =
            crate::minimal_api::start_minimal_btw(&mut agent, "pre-replay question".into());
        agent.begin_replay_window();
        assert!(agent.btw_state.is_none());
        assert!(agent.minimal_btw_lifecycle.is_none());
        assert!(!crate::minimal_api::finish_minimal_btw(
            &mut agent,
            replay_request,
            Ok("pre-replay answer".into())
        ));
        assert!(agent.btw_state.is_none());
    }
}
#[cfg(test)]
mod reconnect_workflow_maps_tests {
    use super::super::test_agent_view;
    use crate::views::workflows::WorkflowRunSnapshot;
    fn wf_snapshot(run_id: &str, status: &str) -> WorkflowRunSnapshot {
        WorkflowRunSnapshot {
            run_id: run_id.to_string(),
            name: "deep-research".to_string(),
            objective: "obj".to_string(),
            status: status.to_string(),
            management_available: true,
            builtin: false,
            phases: Vec::new(),
            current_phase: None,
            agents: Vec::new(),
            agent_budget: None,
            agents_used: 0,
            agents_reserved: 0,
            agents_remaining: None,
            agent_usage_incomplete: false,
            active_agents: 0,
            elapsed_ms: 1_000,
            received_at: std::time::Instant::now(),
            pause_message: None,
            result_summary: None,
        }
    }
    #[test]
    fn cursor_reconnect_restores_stashed_workflow_run_maps() {
        let mut agent = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        agent.workflow_runs.push(wf_snapshot("wf-1", "active"));
        agent.workflow_run_revisions.insert("wf-1".to_string(), 4);
        agent.cleared_workflow_runs.insert("wf-old".to_string());
        agent.begin_session_reload(1);
        assert!(
            agent.workflow_runs.is_empty()
                && agent.workflow_run_revisions.is_empty()
                && agent.cleared_workflow_runs.is_empty(),
            "staging starts empty for all three maps"
        );
        assert!(agent.finish_session_reload(1, true));
        assert_eq!(
            agent.workflow_runs.len(),
            1,
            "run list must be restored from the stash on cursor reconnect"
        );
        assert_eq!(agent.workflow_runs[0].run_id, "wf-1");
        assert_eq!(agent.workflow_runs[0].status, "active");
        assert_eq!(
            agent.workflow_run_revisions.get("wf-1").copied(),
            Some(4),
            "revision highwater must survive so stale re-deliveries still dedupe"
        );
        assert!(
            agent.cleared_workflow_runs.contains("wf-old"),
            "clear tombstones must survive cursor reconnect"
        );
    }
    #[test]
    fn cursor_reconnect_prefers_live_workflow_maps_over_stash() {
        let mut agent = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        agent.workflow_runs.push(wf_snapshot("wf-1", "active"));
        agent
            .workflow_runs
            .push(wf_snapshot("wf-stash-only", "active"));
        agent.workflow_run_revisions.insert("wf-1".to_string(), 3);
        agent
            .workflow_run_revisions
            .insert("wf-stash-only".to_string(), 1);
        agent.cleared_workflow_runs.insert("wf-old".to_string());
        agent.begin_session_reload(1);
        agent.workflow_runs.push(wf_snapshot("wf-1", "complete"));
        agent
            .workflow_runs
            .push(wf_snapshot("wf-live-only", "active"));
        agent.workflow_run_revisions.insert("wf-1".to_string(), 5);
        agent
            .workflow_run_revisions
            .insert("wf-live-only".to_string(), 2);
        agent.cleared_workflow_runs.insert("wf-new".to_string());
        assert!(agent.finish_session_reload(1, true));
        let by_id: std::collections::HashMap<_, _> = agent
            .workflow_runs
            .iter()
            .map(|r| (r.run_id.as_str(), r.status.as_str()))
            .collect();
        assert_eq!(
            by_id.get("wf-1").copied(),
            Some("complete"),
            "live staging snapshot wins for a shared run_id"
        );
        assert_eq!(
            by_id.get("wf-stash-only").copied(),
            Some("active"),
            "stash-only runs are restored"
        );
        assert_eq!(
            by_id.get("wf-live-only").copied(),
            Some("active"),
            "live-only runs are kept"
        );
        assert_eq!(
            agent.workflow_run_revisions.get("wf-1").copied(),
            Some(5),
            "max revision per run_id"
        );
        assert_eq!(
            agent.workflow_run_revisions.get("wf-stash-only").copied(),
            Some(1)
        );
        assert_eq!(
            agent.workflow_run_revisions.get("wf-live-only").copied(),
            Some(2)
        );
        assert!(agent.cleared_workflow_runs.contains("wf-old"));
        assert!(agent.cleared_workflow_runs.contains("wf-new"));
    }
    #[test]
    fn cursor_reconnect_does_not_resurrect_cleared_runs() {
        let mut agent = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        agent.workflow_runs.push(wf_snapshot("wf-1", "active"));
        agent.workflow_runs.push(wf_snapshot("wf-keep", "active"));
        agent
            .workflow_runs
            .push(wf_snapshot("wf-stash-survivor", "active"));
        agent.workflow_run_revisions.insert("wf-1".to_string(), 2);
        agent
            .workflow_run_revisions
            .insert("wf-keep".to_string(), 1);
        agent
            .workflow_run_revisions
            .insert("wf-stash-survivor".to_string(), 1);
        agent.begin_session_reload(1);
        agent.workflow_runs.push(wf_snapshot("wf-keep", "complete"));
        agent.cleared_workflow_runs.insert("wf-1".to_string());
        assert!(agent.finish_session_reload(1, true));
        assert!(
            agent.workflow_runs.iter().all(|r| r.run_id != "wf-1"),
            "cleared-during-window runs must not reappear from the stash"
        );
        assert!(agent.cleared_workflow_runs.contains("wf-1"));
        assert_eq!(
            agent
                .workflow_runs
                .iter()
                .find(|r| r.run_id == "wf-stash-survivor")
                .map(|r| r.status.as_str()),
            Some("active"),
            "a stash-only run not cleared during the window must be restored by the merge"
        );
        assert_eq!(
            agent
                .workflow_runs
                .iter()
                .find(|r| r.run_id == "wf-keep")
                .map(|r| r.status.as_str()),
            Some("complete")
        );
    }
}
