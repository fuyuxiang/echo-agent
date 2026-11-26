//! Prompt-queue pane: visibility toggles, key handling, row removal, and
//! server-order reconciliation.

#[cfg(test)]
use super::test_fixtures;
use super::{AgentPane, AgentView, PromptMode, overlay_action_to_outcome};
use crate::actions::{ActionId, ActionRegistry};
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crossterm::event::KeyEvent;

impl AgentView {
    /// Remove a local queue row: fix selection, drop the entry, hide the
    /// pane if the merged view emptied. Returns the removed prompt, if any.
    pub(in crate::app) fn remove_local_queue_row(
        &mut self,
        id: u64,
    ) -> Option<crate::app::agent::QueuedPrompt> {
        let pos = self
            .session
            .pending_prompts
            .iter()
            .position(|p| p.id == id)?;
        // Deleting the row being edited discards the edit — and must exit
        // BEFORE the removal so a potential auto-hide pane switch can't hit
        // the editing lock (see queue_edit.rs ordering invariant).
        if matches!(
            self.prompt_mode,
            PromptMode::EditingQueued { id: editing_id, server_id: None, .. } if editing_id == id
        ) {
            self.exit_editing_mode();
        }
        self.queue.select_after_delete(id);
        let prompt = self.session.pending_prompts.remove(pos);
        if self.visible_queue_is_empty() {
            self.hide_queue_pane();
        }
        prompt
    }

    /// The `bool` is whether the row is server-authoritative.
    pub(in crate::app) fn resolve_queue_row(
        &self,
        id: u64,
    ) -> (bool, Option<crate::views::queue_pane::QueueRowRef>) {
        let row = self.queue.row_ref(id);
        let is_server = matches!(
            row.as_ref().map(|r| r.origin),
            Some(crate::views::queue_pane::QueueRowOrigin::Server)
        );
        (is_server, row)
    }

    /// Highlights the bottom row (last under the server-then-local merge order) and leaves the composer untouched.
    pub(super) fn try_focus_queue_from_prompt(&mut self) -> Option<InputOutcome> {
        self.sync_queue_pane();
        let id = *self.queue.entry_ids().last()?;

        if !self.set_active_pane(AgentPane::Queue, false) {
            return Some(InputOutcome::Changed);
        }

        self.queue.overlay.visible = true;
        self.queue.overlay.focused = true;
        self.queue.list_state.select_by_id(id);
        Some(InputOutcome::Changed)
    }

    /// Force-send a queued follow-up mid-turn from the prompt (empty composer).
    ///
    /// Always the **top** visible row (first under the server-then-local merge
    /// order — the next item that would drain). Bare Enter and the send-now
    /// chord share this path; queue-pane selection / mouse "Send now" keep
    /// intentional selection. Returns `None` when there is nothing to send.
    pub(super) fn try_send_now_queued_from_prompt(&mut self) -> Option<InputOutcome> {
        if !self.session.state.is_turn_running() {
            return None;
        }
        self.sync_queue_pane();
        let ids = self.queue.entry_ids();
        let id = *ids.first()?;
        let outcome = self.force_interject_queue_row(id);
        // Acting on the prompt-path send-now while its tip is up is the user
        // accepting the hint — mirrors the undo / image-input funnels so the
        // send_now `shown → accepted` conversion is measurable.
        if matches!(outcome, InputOutcome::Action(_))
            && self.ephemeral_tip.current_key() == Some(crate::tips::send_now::SEND_NOW_TIP_KEY)
        {
            echo_agent_telemetry::session_ctx::log_event(
                echo_agent_telemetry::events::ContextualTip {
                    tip: echo_agent_telemetry::events::ContextualTipKind::SendNow,
                    action: echo_agent_telemetry::events::ContextualTipAction::Accepted,
                },
            );
            self.ephemeral_tip
                .clear(crate::tips::send_now::SEND_NOW_TIP_KEY);
        }
        Some(outcome)
    }

    /// The turn is parked in a wait the shell aborts as soon as the user
    /// sends anything (blocking `get_task_output` / `wait_tasks` / `Await*`,
    /// or a blocked foreground subagent await — see
    /// [`crate::views::turn_status::is_sendable_wait`]), and the goal loop is
    /// inactive (the shell suppresses the abort during goal runs, so treating
    /// the wait as user-interruptible would lie there).
    ///
    /// Gates Enter interjecting instead of queueing and the parked queue
    /// drain. The stopped-session *rendering* additionally excludes subagent
    /// waits — see [`Self::renders_parked`].
    /// Purely view-derived — reading it has no turn-lifecycle side effects.
    pub(crate) fn is_parked_on_sendable_wait(&self) -> bool {
        crate::views::turn_status::is_sendable_wait(&self.resolve_turn_activity_unenriched())
            && !self
                .goal_state
                .as_ref()
                .is_some_and(|g| matches!(g.status, crate::app::agent::GoalDisplayStatus::Active))
    }

    /// Whether an explicit send-now dispatched right now will actually cancel
    /// the running turn shell-side. Also requires the front committed so a
    /// spared send-now does not paint under later output from that front.
    pub(crate) fn expects_send_now_cancel(&self) -> bool {
        self.session.state.is_turn_running()
            && self.front_message_committed
            && !self
                .goal_state
                .as_ref()
                .is_some_and(|g| matches!(g.status, crate::app::agent::GoalDisplayStatus::Active))
    }

    /// Arm cancel-marker + no-entry-top pin. Gate with [`Self::expects_send_now_cancel`].
    pub(crate) fn arm_send_now_expectation(&mut self, prompt_id: String) {
        self.follow_without_jump_prompt_id = Some(prompt_id.clone());
        self.expect_send_now_cancel = Some(prompt_id);
    }

    /// Clear cancel-marker + no-entry-top pin (failure / interactive cancel / reload).
    pub(crate) fn clear_send_now_expectation(&mut self) {
        self.expect_send_now_cancel = None;
        self.follow_without_jump_prompt_id = None;
    }

    /// Whether `prompt_id` names a Send Now painted block that is still awaiting
    /// its authoritative interjection notification to claim (and restyle) it in
    /// place — the active-goal Send Now flow: painted optimistically WITHOUT
    /// arming a cancel expectation, then converted to interjection styling by
    /// [`crate::app::acp_handler`]'s `handle_interjection`.
    ///
    /// Such a block must NOT be retired by the queue-echo reconcile
    /// (`queue/changed`) or the non-running `PromptResponse` (`RemovedFromQueue`)
    /// paths before its claim arrives: the row legitimately disappears from the
    /// queue the instant the shell converts the Send Now into an interjection,
    /// so those paths would otherwise drop the block and re-push the message at
    /// the scrollback end (flicker / reorder). Keeping it in place lets
    /// `handle_interjection` convert it, or turn-start adoption reuse it.
    ///
    /// Returns `false` for the armed (expects-cancel) Send Now path and for
    /// non-goal rows, so their retirement behavior is unchanged.
    pub(crate) fn is_send_now_awaiting_interjection_claim(&self, prompt_id: &str) -> bool {
        self.send_now_painted_blocks.contains_key(prompt_id)
            && self.is_self_originated_prompt(prompt_id)
            && self.expect_send_now_cancel.as_deref() != Some(prompt_id)
            && self
                .goal_state
                .as_ref()
                .is_some_and(|g| matches!(g.status, crate::app::agent::GoalDisplayStatus::Active))
    }

    /// The current wait is a foreground subagent await — sendable, but excluded
    /// from the parked look (the parent is blocked, not completed; the
    /// subagent reports its own progress).
    pub(crate) fn is_waiting_on_subagent(&self) -> bool {
        use crate::acp::tracker::{TurnActivity, WaitingReason};
        matches!(
            self.resolve_turn_activity_unenriched(),
            Some(TurnActivity::Waiting(WaitingReason::Subagent { .. }))
        )
    }

    /// Visible held rows for the "N queued" hint. 0 outside sendable waits.
    pub(crate) fn held_queue_count(&self) -> usize {
        // Goal-gated via `is_parked_on_sendable_wait` (0 during a goal — shell exempts goal turns).
        if !self.is_parked_on_sendable_wait() {
            return 0;
        }
        self.visible_held_queue_len()
    }

    /// Pane-visible held rows (excludes running + send-now echo).
    pub(crate) fn visible_held_queue_len(&self) -> usize {
        let running = self.session.current_prompt_id.as_deref();
        let send_now = self.expect_send_now_cancel.as_deref();
        let server = self
            .shared_queue
            .iter()
            .filter(|e| {
                crate::views::queue_pane::visible_held_server_row(
                    &e.id,
                    running,
                    send_now,
                    &self.send_now_painted_blocks,
                )
            })
            .count();
        server + self.session.pending_prompts.len()
    }

    /// Shell-style held occupancy (includes send-now echo; unlike pane count).
    pub(crate) fn has_held_user_queue(&self) -> bool {
        let running = self.session.current_prompt_id.as_deref();
        // An armed send-now counts as occupancy only until its own turn adopts:
        // once the armed id IS the running turn, nothing is held behind it (the
        // arm lingers only for cancel-marker suppression). Excluding the running
        // id here matches the `shared_queue` filter below.
        if self
            .expect_send_now_cancel
            .as_deref()
            .is_some_and(|arm| Some(arm) != running)
        {
            return true;
        }
        if !self.session.pending_prompts.is_empty() {
            return true;
        }
        self.shared_queue
            .iter()
            .any(|e| Some(e.id.as_str()) != running)
    }

    /// Whether bare Enter on the empty composer would actually send the TOP
    /// visible held row — the "Enter to send now" half of the inline hint.
    /// Server rows always send now; a local top row only when prompt-like
    /// (`force_interject_queue_row` refuses bash / client-expanded rows with
    /// a toast, so advertising Enter for them would over-promise).
    pub(crate) fn held_queue_top_sendable(&self) -> bool {
        let running = self.session.current_prompt_id.as_deref();
        let send_now = self.expect_send_now_cancel.as_deref();
        // Merge order: server rows render (and send) first.
        if self.shared_queue.iter().any(|e| {
            crate::views::queue_pane::visible_held_server_row(
                &e.id,
                running,
                send_now,
                &self.send_now_painted_blocks,
            )
        }) {
            return true;
        }
        self.session.pending_prompts.front().is_some_and(|p| {
            p.kind == crate::app::agent::QueueEntryKind::Prompt && p.wire_matches_display()
        })
    }

    /// Rebuild the queue pane via [`visible_held_server_row`] excludes.
    pub(crate) fn sync_queue_pane(&mut self) {
        self.queue.sync_from_merged(
            &self.session.pending_prompts,
            &self.shared_queue,
            self.session.current_prompt_id.as_deref(),
            self.expect_send_now_cancel.as_deref(),
            &self.send_now_painted_blocks,
        );
    }

    /// Whether the stopped-session look is active: the turn is parked in a
    /// sendable wait that is not a foreground subagent await. Purely
    /// view-derived — no transcript row is written for a park. Drives the
    /// idle keybar and the parked turn-status cue; flips back off (the
    /// running chrome returns) the moment the wait ends and the turn resumes.
    pub(crate) fn renders_parked(&self) -> bool {
        self.is_parked_on_sendable_wait() && !self.is_waiting_on_subagent()
    }

    /// Live counts for the turn-status watching cue; see
    /// [`crate::views::turn_status::Watchers`].
    pub(crate) fn watchers(&self) -> crate::views::turn_status::Watchers {
        let mut watchers = crate::views::turn_status::Watchers::default();
        for task in self
            .session
            .bg_tasks
            .values()
            .filter(|t| t.status == crate::app::agent::BgTaskStatus::Running)
        {
            if task.is_monitor {
                watchers.monitors += 1;
            } else {
                watchers.commands += 1;
            }
        }
        watchers.loops = self.session.scheduled_tasks.len();
        watchers.subagents = self
            .subagent_sessions
            .values()
            .filter(|s| s.is_running() && s.workflow_run_id.is_none())
            .count();
        watchers.workflows = self
            .workflow_runs
            .iter()
            .filter(|run| run.is_active())
            .count();
        watchers
    }

    /// Shared tail of every turn-end marker push
    /// (`push_turn_terminal_marker`).
    pub(crate) fn push_end_marker_block(
        &mut self,
        event: crate::scrollback::blocks::SessionEvent,
        stop_hooks: Vec<(String, Vec<crate::scrollback::blocks::tool::HookRunEntry>)>,
        prompt_id: Option<String>,
    ) {
        // The marker keeps its turn's pid for the tail-merge attribution check.
        let block = crate::scrollback::blocks::SessionEventBlock::with_stop_hooks(
            event, stop_hooks, prompt_id,
        );
        self.scrollback
            .push_block(crate::scrollback::block::RenderBlock::SessionEvent(block));
    }

    /// `Some(is_prompt_like)` for a resolvable merged-queue row; `None` when it
    /// can't be resolved. Prompt-like rows may interject: plain prompts, plus
    /// raw skill slash rows (`/find-session args`) whose wire payload IS the
    /// display text — the shell expands those at the interjection drain. Rows
    /// with a client-expanded payload (`/imagine`, `/loop`) and non-prompt
    /// kinds stay queued: interjecting them would send the display text, not
    /// the payload.
    pub(in crate::app) fn queue_row_prompt_like(&self, id: u64) -> Option<bool> {
        use crate::app::agent::QueueEntryKind;
        use crate::views::queue_pane::{QueueRowOrigin, kind_from_wire};

        if let Some(local) = self.session.pending_prompts.iter().find(|p| p.id == id) {
            return Some(local.kind == QueueEntryKind::Prompt && local.wire_matches_display());
        }
        let row = self.queue.row_ref(id)?;
        if row.origin != QueueRowOrigin::Server {
            return None;
        }
        let server_id = row.server_id?;
        let wire = self.shared_queue.iter().find(|e| e.id == server_id)?;
        Some(kind_from_wire(&wire.kind) == QueueEntryKind::Prompt)
    }

    /// Send one merged-queue row now (cancel-and-send), by selection id. The shell cancels the running turn and runs this row as the next turn.
    pub(in crate::app) fn force_interject_queue_row(&mut self, id: u64) -> InputOutcome {
        if !self.session.state.is_turn_running() {
            self.show_toast("No turn running: prompt will send when ready");
            return InputOutcome::Changed;
        }
        let (is_server, row) = self.resolve_queue_row(id);
        if is_server {
            // Server row: the agent promotes it to run next (`echo.agent/queue/interject`); any kind may send now.
            if let Some(row) = row.as_ref()
                && let Some(server_id) = row.server_id.clone()
            {
                // Still an optimistic echo: its `session/prompt` RPC is in flight, so an interject now would overtake the row shell-side and no-op.
                // Park the intent; the confirming `echo.agent/queue/changed` broadcast fires it with the row's authoritative version.
                if self.optimistic_queue_ids.contains(&server_id) {
                    self.send_now_awaiting_confirm = Some(server_id);
                    return InputOutcome::Changed;
                }
                return InputOutcome::Action(Action::QueueInterjectShared {
                    id: server_id,
                    expected_version: row.version,
                    new_text: None,
                });
            }
            return InputOutcome::Changed;
        }
        // Local rows: only plain prompts / raw skill rows can re-send (others would send display text, not payload).
        if self.queue_row_prompt_like(id) != Some(true) {
            self.show_toast("Can't send this now: it runs when the current turn ends");
            return InputOutcome::Changed;
        }
        if let Some(prompt) = self.remove_local_queue_row(id) {
            return InputOutcome::Action(Action::SendPromptNow {
                text: prompt.text,
                images: prompt.images,
            });
        }
        InputOutcome::Changed
    }

    /// Reconcile this client's optimistic queue echoes against a raw
    /// `echo.agent/queue/changed` broadcast (pre-merge entries — the mirrored
    /// snapshot re-pins unconfirmed echoes, so it can't tell confirmation
    /// apart), and resolve a parked queue-row send-now
    /// ([`Self::send_now_awaiting_confirm`]).
    ///
    /// Returns `Some((id, version))` when the parked row is now confirmed as
    /// QUEUED — the caller fires `echo.agent/queue/interject` with that
    /// authoritative version. A parked row confirmed as RUNNING clears the
    /// park with nothing to do (the natural drain won the race). A row in
    /// neither set stays parked (its RPC is still in flight).
    pub(crate) fn resolve_send_now_awaiting_confirm(
        &mut self,
        broadcast_entries: &[(String, u64)],
        running_prompt_id: Option<&str>,
    ) -> Option<(String, u64)> {
        // Confirmed ids (queued or running) leave the optimistic set.
        self.optimistic_queue_ids.retain(|id| {
            running_prompt_id != Some(id.as_str())
                && !broadcast_entries.iter().any(|(eid, _)| eid == id)
        });
        let awaiting = self.send_now_awaiting_confirm.as_deref()?;
        if running_prompt_id == Some(awaiting) {
            self.send_now_awaiting_confirm = None;
            return None;
        }
        if let Some((id, version)) = broadcast_entries.iter().find(|(eid, _)| eid == awaiting) {
            self.send_now_awaiting_confirm = None;
            return Some((id.clone(), *version));
        }
        None
    }

    /// A server-queue echo resolved without landing (RPC failed / removed /
    /// cancelled): forget it, and drop any send-now parked on it — there is
    /// no row left to promote.
    pub(crate) fn note_queue_echo_retired(&mut self, prompt_id: &str) {
        self.optimistic_queue_ids.remove(prompt_id);
        if self.send_now_awaiting_confirm.as_deref() == Some(prompt_id) {
            self.send_now_awaiting_confirm = None;
        }
        // An active-goal Send Now painted block awaiting its interjection claim
        // stays put: the echo DID land (converted into an interjection), so the
        // row's disappearance from the queue is expected — `handle_interjection`
        // will convert the block in place. Retiring here would drop and re-push
        // it at the scrollback end. Callers that must retire regardless (e.g. a
        // genuine send failure) call `retire_send_now_painted_block` directly.
        if self.is_send_now_awaiting_interjection_claim(prompt_id) {
            return;
        }
        // Retired ids never adopt — drop the painted block with the id.
        // (Re-keys route through `note_queue_echo_rekeyed` instead.)
        self.retire_send_now_painted_block(prompt_id);
    }

    /// Re-key: `old_id` is dead but the message lives on under `new_id` —
    /// move (never retire) its painted block so the new adoption reuses it.
    pub(crate) fn note_queue_echo_rekeyed(&mut self, old_id: &str, new_id: &str) {
        self.optimistic_queue_ids.remove(old_id);
        if self.send_now_awaiting_confirm.as_deref() == Some(old_id) {
            self.send_now_awaiting_confirm = None;
        }
        if let Some(entry) = self.send_now_painted_blocks.remove(old_id) {
            match self.send_now_painted_blocks.entry(new_id.to_string()) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(entry);
                }
                // Re-key collision (identical texts): remove the losing
                // block instead of orphaning it.
                std::collections::hash_map::Entry::Occupied(_) => {
                    self.scrollback.remove_entry(entry.0);
                }
            }
        }
    }

    /// Remove the optimistic block for a send-now'd prompt that will never
    /// run (send failure, removal) — a leftover would duplicate on requeue.
    pub(crate) fn retire_send_now_painted_block(&mut self, prompt_id: &str) {
        if let Some((id, _)) = self.send_now_painted_blocks.remove(prompt_id) {
            self.scrollback.remove_entry(id);
        }
    }

    /// Apply the updates buffered for `prompt_id`; other pids' entries are dropped.
    pub(crate) fn flush_pending_adoption_updates(&mut self, prompt_id: &str) {
        if self.pending_adoption_updates.is_empty() {
            return;
        }
        for (pid, update, mut meta) in std::mem::take(&mut self.pending_adoption_updates) {
            if pid == prompt_id {
                // Forward-only: the EchoAgent rail shares this cursor and may have
                // applied later events during the buffering window — assigning
                // a buffered (older) id would re-deliver those on reconnect.
                if let (Some(seq), Some(id)) = (meta.event_seq, meta.event_id.take()) {
                    self.advance_last_seen_event_id(id, Some(seq));
                }
                self.session
                    .handle_update(update, &meta, &mut self.scrollback);
            }
        }
    }

    /// Drop buffered updates for `prompt_id`; the un-advanced cursor lets a
    /// reconnect replay re-deliver them.
    pub(crate) fn discard_pending_adoption_updates(&mut self, prompt_id: &str) {
        self.pending_adoption_updates
            .retain(|(pid, _, _)| pid != prompt_id);
    }

    /// Toggle queue pane visibility (Ctrl-; shortcut).
    pub(in crate::app) fn toggle_queue_pane(&mut self) {
        self.queue.overlay.toggle();
        self.queue.on_state_change();
        if self.queue.overlay.focused {
            self.set_active_pane(AgentPane::Queue, false);
        } else if self.active_pane == AgentPane::Queue {
            self.set_active_pane(AgentPane::Scrollback, false);
        }
    }

    /// Routes through overlay structural keys, then queue actions, then navigation.
    pub(in crate::app) fn handle_queue_key(
        &mut self,
        key: &KeyEvent,
        registry: &ActionRegistry,
    ) -> InputOutcome {
        use crate::views::overlay::{handle_overlay_key, handle_overlay_nav_key};
        use crate::views::queue_pane::QueueEvent;

        // Structural keys through shared handler (Esc, Ctrl-F, etc.).
        let action = handle_overlay_key(&mut self.queue.overlay, key)
            .or_else(|| handle_overlay_nav_key(&mut self.queue.overlay, key));
        if let Some(action) = action {
            self.queue.on_state_change();
            // Overlay dismiss skips hide_queue_pane; reset edge when queue is empty.
            if !self.queue.overlay.visible && self.visible_queue_is_empty() {
                self.queue.reset_auto_show_edge();
            }
            if !self.queue.overlay.visible || !self.queue.overlay.focused {
                self.set_active_pane(AgentPane::Scrollback, false);
            }
            return overlay_action_to_outcome(action);
        }

        // Queue-specific actions (delete, edit, reorder). `x`/Delete = row delete.
        if let Some(event) = self.queue.handle_key(key, registry) {
            // Server rows route to the agent as `echo.agent/queue/*` commands (the rebroadcast is the source of truth); local rows mutate in place.
            let (is_server, row) = self.resolve_queue_row(Self::queue_event_id(&event));

            match event {
                QueueEvent::DeleteSelected { id } => {
                    if is_server {
                        // Optimistic remove; server rebroadcast is authoritative.
                        if let (Some(_sid), Some(row)) = (self.session.session_id.as_ref(), row)
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
                    // No drain kick (cf. mouse [cancel]): queue focus is unreachable mid-edit.
                    self.remove_local_queue_row(id);
                }
                QueueEvent::EditSelected { id } => {
                    // Entry into editing mode lives in `queue_edit.rs`.
                    self.enter_queue_edit(id, is_server, row);
                }
                QueueEvent::SwapUp { id } => {
                    if is_server {
                        if let Some(ordered_ids) = self.server_queue_reordered(id, true) {
                            return InputOutcome::Action(Action::QueueReorderShared {
                                ordered_ids,
                            });
                        }
                        return InputOutcome::Changed;
                    }
                    self.session.swap_prompt_up(id);
                }
                QueueEvent::SwapDown { id } => {
                    if is_server {
                        if let Some(ordered_ids) = self.server_queue_reordered(id, false) {
                            return InputOutcome::Action(Action::QueueReorderShared {
                                ordered_ids,
                            });
                        }
                        return InputOutcome::Changed;
                    }
                    self.session.swap_prompt_down(id);
                }
                QueueEvent::ForceInterject { id } => {
                    // Same InterjectPrompt chord as the prompt; surface is the queue pane (not When::PromptFocused).
                    crate::actions::log_shortcut_used(key, ActionId::InterjectPrompt, "queue");
                    return self.force_interject_queue_row(id);
                }
            }
            return InputOutcome::Changed;
        }

        // Down off the last row returns to the composer, as the history panel does past its newest entry.
        // Up stays clamped, because overshooting there would open history and rewrite the composer.
        if crate::key!(Down).matches(key)
            && self.queue.selected_id() == self.queue.entry_ids().last().copied()
        {
            self.queue.overlay.focused = false;
            self.set_active_pane(AgentPane::Prompt, false);
            return InputOutcome::Changed;
        }

        // Navigation keys (j/k, y to copy, etc.).
        if self.queue.handle_navigation_key(key) {
            InputOutcome::Changed
        } else {
            InputOutcome::Unchanged
        }
    }

    /// The selection id carried by a [`QueueEvent`].
    fn queue_event_id(event: &crate::views::queue_pane::QueueEvent) -> u64 {
        use crate::views::queue_pane::QueueEvent;
        match event {
            QueueEvent::DeleteSelected { id }
            | QueueEvent::EditSelected { id }
            | QueueEvent::SwapUp { id }
            | QueueEvent::SwapDown { id }
            | QueueEvent::ForceInterject { id } => *id,
        }
    }

    /// True when the pane would show zero rows.
    pub(in crate::app) fn visible_queue_is_empty(&self) -> bool {
        self.visible_held_queue_len() == 0
    }

    /// Hide the queue pane. Only steals focus when the queue pane was active —
    /// prompt-path send-now of the last local row must not yank the user out
    /// of the composer into scrollback.
    pub(in crate::app) fn hide_queue_pane(&mut self) {
        self.queue.overlay.visible = false;
        self.queue.overlay.focused = false;
        // External hide skips sync auto-hide; reset so next enqueue can auto-show.
        self.queue.reset_auto_show_edge();
        if self.active_pane == AgentPane::Queue {
            self.set_active_pane(AgentPane::Scrollback, false);
        }
    }

    /// Reorder payload for `echo.agent/queue/reorder`. Omit only running; include
    /// send-now in the list but do not swap past it (shell ranks missing ids last).
    fn server_queue_reordered(&self, selection_id: u64, up: bool) -> Option<Vec<String>> {
        let server_id = self.queue.row_ref(selection_id)?.server_id?;
        let running = self.session.current_prompt_id.as_deref();
        let send_now = self.expect_send_now_cancel.as_deref();
        let all_ids: Vec<String> = self
            .shared_queue
            .iter()
            .filter(|e| Some(e.id.as_str()) != running)
            .map(|e| e.id.clone())
            .collect();
        let mut swappable: Vec<String> = all_ids
            .iter()
            .filter(|id| {
                crate::views::queue_pane::visible_held_server_row(
                    id,
                    running,
                    send_now,
                    &self.send_now_painted_blocks,
                )
            })
            .cloned()
            .collect();
        let pos = swappable.iter().position(|x| x == &server_id)?;
        let swap_with = if up {
            pos.checked_sub(1)?
        } else {
            let next = pos + 1;
            if next >= swappable.len() {
                return None;
            }
            next
        };
        swappable.swap(pos, swap_with);
        let mut swap_iter = swappable.into_iter();
        let ordered: Vec<String> = all_ids
            .into_iter()
            .map(|id| {
                if crate::views::queue_pane::visible_held_server_row(
                    &id,
                    running,
                    send_now,
                    &self.send_now_painted_blocks,
                ) {
                    swap_iter
                        .next()
                        .expect("swappable count matches visible slots")
                } else {
                    id
                }
            })
            .collect();
        Some(ordered)
    }
}

#[cfg(test)]
mod queue_edit_routing_tests {
    use super::test_fixtures::{
        force_interject_key, make_running_agent, non_vscode_registry, running_agent_local_only,
        test_pasted_image, vscode_family_registry, vscode_interject_key,
    };
    use super::*;
    use crate::app::actions::Action;
    use crate::app::agent::AgentState;
    use crate::app::app_view::InputOutcome;
    use crate::app::prompt_queue::QueueEntryWire;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn delete_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
    }

    #[test]
    fn delete_routes_server_to_action_and_local_to_mutation() {
        let mut agent = make_running_agent();
        let registry = ActionRegistry::defaults();

        let ids = agent.queue.entry_ids();
        assert_eq!(ids.len(), 2);
        // Server row is rendered first (documented merge order).
        agent.queue.list_state.select_by_id(ids[0]);

        let outcome = agent.handle_queue_key(&delete_key(), &registry);
        match outcome {
            InputOutcome::Action(Action::QueueRemoveShared {
                id,
                expected_version,
            }) => {
                assert_eq!(id, "p1");
                assert_eq!(expected_version, 2);
            }
            other => panic!("expected QueueRemoveShared action, got {other:?}"),
        }
        assert_eq!(agent.session.pending_prompts.len(), 1);
        assert!(agent.shared_queue.is_empty());
        assert!(agent.queue.overlay.visible);
        assert!(agent.queue.overlay.focused);

        agent.queue.list_state.select_by_id(ids[1]);
        let outcome = agent.handle_queue_key(&delete_key(), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(agent.session.pending_prompts.is_empty());
        assert!(!agent.queue.overlay.visible);
        assert!(!agent.queue.overlay.focused);
    }

    fn server_wire(id: &str, position: usize) -> QueueEntryWire {
        QueueEntryWire {
            id: id.into(),
            version: 1,
            owner: None,
            last_editor: None,
            kind: "prompt".into(),
            text: format!("server {id}"),
            combined_texts: None,
            position,
        }
    }

    /// `visible_queue_is_empty` reflects the *merged* pane view, excluding the
    /// in-flight turn — the invariant the three pane-hide sites depend on.
    #[test]
    fn visible_queue_is_empty_reflects_merged_view_minus_running() {
        let mut agent = make_running_agent();
        // 1 non-running server row + 1 local row → not empty.
        assert!(!agent.visible_queue_is_empty());

        // Drop the server row; the lone local row keeps it non-empty.
        agent.shared_queue.clear();
        assert!(!agent.visible_queue_is_empty());

        // Both queues empty → empty.
        agent.session.pending_prompts.clear();
        assert!(agent.visible_queue_is_empty());

        // A queued (non-running) server row → not empty.
        agent.shared_queue = vec![server_wire("p1", 0)];
        agent.session.current_prompt_id = None;
        assert!(!agent.visible_queue_is_empty());

        // The only server row IS the running turn → counts as empty.
        agent.session.current_prompt_id = Some("p1".to_string());
        assert!(agent.visible_queue_is_empty());

        // Running row plus a second queued server row → not empty again.
        agent.shared_queue.push(server_wire("p2", 1));
        assert!(!agent.visible_queue_is_empty());
    }

    fn down_key() -> KeyEvent {
        KeyEvent::new(
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        )
    }

    /// Down off the last row hands focus back, so Up in and Down out is a round trip.
    #[test]
    fn down_off_the_last_row_returns_to_the_composer() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Queue;
        agent.queue.overlay.focused = true;
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(*ids.last().unwrap());

        agent.handle_queue_key(&down_key(), &ActionRegistry::defaults());

        assert_eq!(agent.active_pane, AgentPane::Prompt);
        assert!(!agent.queue.overlay.focused);
    }

    /// Above the last row Down steps the highlight and stays in the pane.
    #[test]
    fn down_above_the_last_row_stays_in_the_queue() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Queue;
        agent.queue.overlay.focused = true;
        // Two local rows: their order is the order they were queued, with no
        // server row whose visibility depends on which turn is running.
        agent.shared_queue.clear();
        agent.session.pending_prompts.clear();
        agent.session.enqueue_prompt("queued first".to_string());
        agent.session.enqueue_prompt("queued last".to_string());
        agent.sync_queue_pane();
        let ids = agent.queue.entry_ids();
        assert_eq!(ids.len(), 2, "two rows so Down has somewhere to go");
        agent.queue.list_state.select_by_id(ids[0]);

        agent.handle_queue_key(&down_key(), &ActionRegistry::defaults());

        // Only the exit rule is asserted. Stepping is the list pane's own, and it moves by
        // index, which nothing resolves until a render, so it cannot move in a headless test.
        assert_eq!(agent.active_pane, AgentPane::Queue);
        assert!(agent.queue.overlay.focused);
    }

    /// Keyboard-deleting the last *local* row while a server row remains keeps the pane open and
    /// focused (regression: it previously force-hid the pane and stranded the server rows).
    #[test]
    fn delete_last_local_row_keeps_pane_open_when_server_remains() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Queue;
        let registry = ActionRegistry::defaults();

        let ids = agent.queue.entry_ids();
        // ids[1] is the only local row.
        agent.queue.list_state.select_by_id(ids[1]);
        let outcome = agent.handle_queue_key(&delete_key(), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));

        assert!(agent.session.pending_prompts.is_empty());
        assert_eq!(agent.shared_queue.len(), 1);
        assert!(agent.queue.overlay.visible);
        assert!(agent.queue.overlay.focused);
        assert_eq!(agent.active_pane, AgentPane::Queue);
        // Through the handler: selection lands on the surviving server
        // row (ids[0]) across the merge boundary, not back at the top.
        assert_eq!(agent.queue.selected_id(), Some(ids[0]));
    }

    /// Deleting down to a truly-empty merged view hides the pane. The lone
    /// server row is the in-flight turn, so it does not count as a visible row.
    #[test]
    fn delete_to_empty_hides_pane_excluding_running_server_row() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Queue;
        // Make the lone server row the in-flight turn (excluded from the view).
        agent.session.current_prompt_id = Some("p1".to_string());
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );
        agent.queue.overlay.visible = true;
        agent.queue.overlay.focused = true;

        let registry = non_vscode_registry();
        let ids = agent.queue.entry_ids();
        // Only the local row is a visible queued row (running p1 excluded).
        assert_eq!(ids.len(), 1);
        agent.queue.list_state.select_by_id(ids[0]);
        let outcome = agent.handle_queue_key(&delete_key(), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));

        assert!(agent.session.pending_prompts.is_empty());
        assert!(agent.visible_queue_is_empty());
        assert!(!agent.queue.overlay.visible);
        assert!(!agent.queue.overlay.focused);
        assert_eq!(agent.active_pane, AgentPane::Scrollback);
    }

    /// Keyboard force-interject of the last local row keeps the pane open when
    /// a server row remains (mirrors the delete path's visibility treatment).
    #[test]
    fn force_interject_last_local_row_keeps_pane_open_when_server_remains() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Queue;
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let outcome = agent.handle_queue_key(&force_interject_key(), &registry);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "local one")
            }
            other => panic!("expected SendPromptNow action, got {other:?}"),
        }
        assert!(agent.session.pending_prompts.is_empty());
        assert_eq!(agent.shared_queue.len(), 1);
        assert!(agent.queue.overlay.visible);
        assert!(agent.queue.overlay.focused);
        assert_eq!(agent.active_pane, AgentPane::Queue);
    }

    /// Hide via the keyboard delete path (site 1) with a *literally empty*
    /// `shared_queue` and no running prompt: emptying the local queue empties
    /// the merged view → pane hides and focus returns to scrollback.
    #[test]
    fn delete_last_local_row_hides_pane_when_shared_queue_empty() {
        let mut agent = running_agent_local_only();
        let registry = ActionRegistry::defaults();
        let ids = agent.queue.entry_ids();
        assert_eq!(ids.len(), 1);
        agent.queue.list_state.select_by_id(ids[0]);
        let outcome = agent.handle_queue_key(&delete_key(), &registry);
        assert!(matches!(outcome, InputOutcome::Changed));

        assert!(agent.session.pending_prompts.is_empty());
        assert!(agent.shared_queue.is_empty());
        assert!(!agent.queue.overlay.visible);
        assert!(!agent.queue.overlay.focused);
        assert_eq!(agent.active_pane, AgentPane::Scrollback);
    }

    #[test]
    fn delete_last_then_requeue_auto_shows_pane() {
        let mut agent = running_agent_local_only();
        let registry = ActionRegistry::defaults();
        // Prime prev_len via sync (mirrors a rendered frame with one row).
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );
        assert!(agent.queue.overlay.visible);

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&delete_key(), &registry);
        assert!(!agent.queue.overlay.visible);

        agent.session.enqueue_prompt("replacement".into());
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );
        assert!(
            agent.queue.overlay.visible,
            "new queued prompt must be visible after delete+requeue"
        );
        assert_eq!(agent.queue.entry_ids().len(), 1);
    }

    /// Hide via the keyboard force-interject path (site 2): with no server rows
    /// left, interjecting the last local row empties the merged view → hide.
    #[test]
    fn force_interject_last_local_row_hides_pane_when_shared_queue_empty() {
        let mut agent = running_agent_local_only();
        let registry = non_vscode_registry();
        let ids = agent.queue.entry_ids();
        assert_eq!(ids.len(), 1);
        agent.queue.list_state.select_by_id(ids[0]);
        let outcome = agent.handle_queue_key(&force_interject_key(), &registry);
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

    /// Interjecting a Server-origin row routes to `Action::QueueInterjectShared`
    /// (the agent atomically removes it + merges it into the running turn); a
    /// Local-origin row interjects its text directly via `Action::Interject`
    /// after removing it from the client-owned queue.
    #[test]
    fn force_interject_routes_server_to_action_and_local_to_interject() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        // Stored image must ride the action (regression: silent drop).
        agent.session.pending_prompts[0]
            .images
            .push(test_pasted_image());

        let ids = agent.queue.entry_ids();
        assert_eq!(ids.len(), 2);
        // Server row first (documented merge order).
        agent.queue.list_state.select_by_id(ids[0]);
        let outcome = agent.handle_queue_key(&force_interject_key(), &registry);
        match outcome {
            InputOutcome::Action(Action::QueueInterjectShared {
                id,
                expected_version,
                new_text: None,
            }) => {
                assert_eq!(id, "p1");
                assert_eq!(expected_version, 2);
            }
            other => panic!("expected QueueInterjectShared action, got {other:?}"),
        }
        // Server interject does NOT mutate the local queue.
        assert_eq!(agent.session.pending_prompts.len(), 1);

        // The local row interjects its text (and stored images) directly.
        agent.queue.list_state.select_by_id(ids[1]);
        let outcome = agent.handle_queue_key(&force_interject_key(), &registry);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, images }) => {
                assert_eq!(text, "local one");
                assert_eq!(images.len(), 1, "row image must ride the interject");
            }
            other => panic!("expected SendPromptNow action, got {other:?}"),
        }
        // Local interject removed it from the client-owned queue.
        assert!(agent.session.pending_prompts.is_empty());
    }

    /// A running agent whose only queued row is a local bash command.
    fn running_agent_with_local_bash(command: &str) -> AgentView {
        let mut agent = running_agent_local_only();
        agent.session.pending_prompts.clear();
        agent.session.enqueue_bash_command(command.into());
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );
        agent
    }

    /// Force-sending a local bash row is a guarded no-op.
    #[test]
    fn force_interject_local_bash_row_keeps_it_queued() {
        let mut agent = running_agent_with_local_bash("ls -la");
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        assert_eq!(ids.len(), 1);
        agent.queue.list_state.select_by_id(ids[0]);
        let outcome = agent.handle_queue_key(&force_interject_key(), &registry);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "bash force-send must be a guarded no-op, got {outcome:?}"
        );
        assert_eq!(agent.session.pending_prompts.len(), 1, "row must stay");
        assert!(agent.toast.is_some(), "guard must explain itself");
    }

    /// A server bash row can send now (promoted to run as its own turn).
    #[test]
    fn force_interject_server_bash_row_promotes_via_queue_interject() {
        let mut agent = make_running_agent();
        agent.shared_queue[0].kind = "bash".into();
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let outcome = agent.handle_queue_key(&force_interject_key(), &registry);
        match outcome {
            InputOutcome::Action(Action::QueueInterjectShared { id, .. }) => {
                assert_eq!(id, "p1");
            }
            other => panic!("expected QueueInterjectShared for server bash row, got {other:?}"),
        }
    }

    /// Empty-Enter send-now must not convert a bash top row into an interjection.
    #[test]
    fn enter_empty_from_prompt_does_not_convert_bash_top_row() {
        let mut agent = running_agent_with_local_bash("git status");
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.prompt.set_text("");

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_prompt_key_for_test(&enter);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "empty Enter on a bash top row must not interject, got {outcome:?}"
        );
        assert_eq!(
            agent.session.pending_prompts.len(),
            1,
            "bash row must stay queued"
        );
    }

    /// A running agent whose only queued row is a local skill-injected row
    /// (kind Prompt + wire_blocks), mirroring the `InjectSkill` enqueue.
    fn running_agent_with_local_skill(display: &str, wire: &str) -> AgentView {
        use agent_client_protocol as acp;
        let mut agent = running_agent_local_only();
        agent.session.pending_prompts.clear();
        let id = agent.session.next_queue_id;
        agent.session.next_queue_id += 1;
        agent
            .session
            .pending_prompts
            .push_back(crate::app::agent::QueuedPrompt {
                wire_blocks: Some(vec![acp::ContentBlock::Text(acp::TextContent::new(wire))]),
                display_as_skill: true,
                ..crate::app::agent::QueuedPrompt::plain(
                    id,
                    display,
                    crate::app::agent::QueueEntryKind::Prompt,
                )
            });
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );
        agent
    }

    /// Force-sending a raw skill row (wire payload == display text, the ACP
    /// skill-command shape) interjects its slash text — the shell expands it
    /// at the interjection drain.
    #[test]
    fn force_interject_local_raw_skill_row_interjects_text() {
        let mut agent = running_agent_with_local_skill("/find-session", "/find-session");
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        assert_eq!(ids.len(), 1);
        agent.queue.list_state.select_by_id(ids[0]);
        let outcome = agent.handle_queue_key(&force_interject_key(), &registry);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "/find-session")
            }
            other => panic!("expected SendPromptNow action, got {other:?}"),
        }
        assert!(
            agent.session.pending_prompts.is_empty(),
            "row must leave the queue"
        );
    }

    /// A client-expanded row (`/imagine`-shaped: wire payload != display
    /// text) stays queued — interjecting it would send the display text,
    /// not the payload.
    #[test]
    fn force_interject_local_expanded_row_keeps_it_queued() {
        let mut agent =
            running_agent_with_local_skill("/imagine a cat", "<expanded imagine instructions>");
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let outcome = agent.handle_queue_key(&force_interject_key(), &registry);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "expanded-payload force-send must be a guarded no-op, got {outcome:?}"
        );
        assert_eq!(agent.session.pending_prompts.len(), 1, "row must stay");
        assert!(agent.toast.is_some(), "guard must explain itself");
    }

    /// The reported bug: empty-Enter send-now on a queued raw skill row
    /// (`/find-session` queued as a mid-turn follow-up) must interject it
    /// instead of toasting "Can't send this mid-turn".
    #[test]
    fn enter_empty_from_prompt_sends_raw_skill_top_row() {
        let mut agent = running_agent_with_local_skill("/find-session", "/find-session");
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.prompt.set_text("");

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_prompt_key_for_test(&enter);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "/find-session")
            }
            other => panic!("expected SendPromptNow action, got {other:?}"),
        }
        assert!(agent.session.pending_prompts.is_empty());
    }

    /// Composer interject carries pasted images on the action — no
    /// "not supported" toast, and the composer image list is drained.
    #[test]
    fn interject_key_normal_mode_carries_composer_images() {
        let mut agent = make_running_agent();
        agent.prompt.set_text("look at this");
        let len = agent.prompt.textarea().text().len();
        agent.prompt.textarea.set_cursor(len);
        agent.prompt.insert_image(test_pasted_image()).unwrap();

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { images, .. }) => {
                assert_eq!(images.len(), 1);
            }
            other => panic!("expected SendPromptNow with images, got {other:?}"),
        }
        assert!(agent.prompt.images.is_empty());
        assert!(agent.toast.is_none(), "no drop toast expected");
        assert_eq!(agent.prompt.text(), "");
    }

    /// Force-interject with no turn running is a guarded no-op (toast only) — it
    /// must never emit a server interject for an idle session.
    #[test]
    fn force_interject_noop_when_idle() {
        let mut agent = make_running_agent();
        agent.session.state = AgentState::Idle;
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let outcome = agent.handle_queue_key(&force_interject_key(), &registry);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "idle force-interject must be a no-op, got {outcome:?}"
        );
        // Nothing left the queue.
        assert_eq!(agent.shared_queue.len(), 1);
        assert_eq!(agent.session.pending_prompts.len(), 1);
    }

    /// Reordering a Server row emits `Action::QueueReorderShared` with the
    /// swapped server id order.
    #[test]
    fn swap_up_routes_server_reorder() {
        let mut agent = make_running_agent();
        // Two server rows so a swap is possible.
        agent.shared_queue = vec![
            QueueEntryWire {
                id: "p1".into(),
                version: 0,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "first".into(),
                position: 0,
                combined_texts: None,
            },
            QueueEntryWire {
                id: "p2".into(),
                version: 0,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "second".into(),
                position: 1,
                combined_texts: None,
            },
        ];
        agent.session.pending_prompts.clear();
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            None,
            None,
            &agent.send_now_painted_blocks,
        );
        agent.queue.overlay.visible = true;
        agent.queue.overlay.focused = true;
        let registry = ActionRegistry::defaults();

        // Select the second server row and swap it up.
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let outcome = agent.handle_queue_key(
            &KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT),
            &registry,
        );
        match outcome {
            InputOutcome::Action(Action::QueueReorderShared { ordered_ids }) => {
                assert_eq!(ordered_ids, vec!["p2".to_string(), "p1".to_string()]);
            }
            other => panic!("expected QueueReorderShared action, got {other:?}"),
        }
    }

    #[test]
    fn server_reorder_keeps_send_now_echo_in_ordered_ids() {
        let mut agent = make_running_agent();
        agent.session.current_prompt_id = Some("running".into());
        agent.expect_send_now_cancel = Some("send-now".into());
        agent.shared_queue = vec![
            server_wire("send-now", 0),
            server_wire("held-1", 1),
            server_wire("held-2", 2),
        ];
        agent.session.pending_prompts.clear();
        agent.sync_queue_pane();
        let ids = agent.queue.entry_ids();
        assert_eq!(ids.len(), 2, "pane hides the send-now echo");
        assert!(
            agent.server_queue_reordered(ids[0], true).is_none(),
            "SwapUp on first visible held must not demote send-now"
        );
        agent.queue.list_state.select_by_id(ids[1]);
        let ordered = agent
            .server_queue_reordered(ids[1], true)
            .expect("swap up among held rows");
        assert_eq!(
            ordered,
            vec![
                "send-now".to_string(),
                "held-2".to_string(),
                "held-1".to_string(),
            ],
            "send-now must stay front-most among queueable server rows"
        );
    }

    /// Normal-mode interject: the InterjectPrompt arm owns the composer
    /// clear — the text came from the composer, so it is cleared at the
    /// call site (dispatch never touches the composer).
    #[test]
    fn interject_key_normal_mode_clears_composer_at_handler() {
        let mut agent = make_running_agent();
        agent.prompt.set_text("hello there");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "hello there")
            }
            other => panic!("expected Interject, got {other:?}"),
        }
        assert_eq!(agent.prompt.text(), "");
    }

    /// Idle interject key: with no running turn there's nothing to interject
    /// into — the key is a no-op (does not send like Enter).
    #[test]
    fn interject_key_when_idle_is_noop() {
        let mut agent = make_running_agent();
        agent.session.state = AgentState::Idle;
        agent.prompt.set_text("hello there");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "idle interject must be a no-op, got {outcome:?}"
        );
        assert_eq!(agent.prompt.text(), "hello there");
    }

    /// Running turn but empty composer and empty queue: interject key is a no-op.
    #[test]
    fn interject_key_when_running_empty_is_noop() {
        let mut agent = make_running_agent();
        // make_running_agent seeds a local queued row — clear it so this test
        // only covers the empty-composer + empty-queue no-op.
        agent.session.pending_prompts.clear();
        agent.shared_queue.clear();
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &[],
            None,
            None,
            &Default::default(),
        );
        agent.prompt.set_text("   ");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "empty interject must be a no-op, got {outcome:?}"
        );
    }

    /// Empty composer + mid-turn queue: send-now from the *prompt* force-sends
    /// the top queued follow-up (no need to focus the queue pane) and keeps
    /// Prompt focus even when the pane hides.
    #[test]
    fn interject_key_from_prompt_force_sends_top_queued_when_empty() {
        let mut agent = running_agent_local_only();
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.prompt.set_text("");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "local one");
            }
            other => panic!("expected Interject of queued follow-up, got {other:?}"),
        }
        assert!(
            agent.session.pending_prompts.is_empty(),
            "queued row must be consumed"
        );
        assert_eq!(
            agent.active_pane,
            AgentPane::Prompt,
            "prompt-path send-now must not steal focus to scrollback"
        );
    }

    /// Bare Enter on an empty prompt mid-turn force-sends the top queued row
    /// (same path as the interject chord with an empty composer).
    #[test]
    fn enter_empty_from_prompt_force_sends_top_queued() {
        let mut agent = running_agent_local_only();
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.prompt.set_text("");

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_prompt_key_for_test(&enter);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "local one");
            }
            other => panic!("expected Interject of top queued follow-up, got {other:?}"),
        }
        assert!(
            agent.session.pending_prompts.is_empty(),
            "queued row must be consumed"
        );
        assert_eq!(
            agent.active_pane,
            AgentPane::Prompt,
            "empty-Enter send-now must not steal focus to scrollback"
        );
    }

    /// Multiline mode: empty bare Enter still send-nows (does not insert a
    /// blank line). Enter-with-text remains newline-only in multiline.
    #[test]
    fn multiline_enter_empty_from_prompt_force_sends_top_queued() {
        let mut agent = running_agent_local_only();
        agent.multiline_mode = true;
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.prompt.set_text("");

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_prompt_key_for_test(&enter);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "local one");
            }
            other => panic!("multiline empty Enter must send-now top queued row, got {other:?}"),
        }
        assert!(
            agent.session.pending_prompts.is_empty(),
            "queued row must be consumed"
        );
        assert_eq!(
            agent.prompt.text(),
            "",
            "send-now must not leave a blank line in the composer"
        );
    }

    /// Multiline + non-empty composer: bare Enter still inserts a newline
    /// (does not queue/send), even mid-turn with a queue present.
    #[test]
    fn multiline_enter_with_text_inserts_newline_not_send_now() {
        let mut agent = running_agent_local_only();
        agent.multiline_mode = true;
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.prompt.set_text("draft line");

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_prompt_key_for_test(&enter);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "multiline Enter with text must insert newline, got {outcome:?}"
        );
        assert!(
            agent.prompt.text().contains('\n'),
            "expected newline insertion, got {:?}",
            agent.prompt.text()
        );
        assert_eq!(
            agent.session.pending_prompts.len(),
            1,
            "queued follow-up must remain (text Enter is not send-now)"
        );
    }

    /// When the composer has text, that wins over a queued follow-up.
    #[test]
    fn interject_key_composer_text_wins_over_queued_follow_up() {
        let mut agent = running_agent_local_only();
        agent.active_pane = AgentPane::Prompt;
        agent.prompt.set_text("composer wins");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "composer wins");
            }
            other => panic!("expected composer Interject, got {other:?}"),
        }
        assert_eq!(
            agent.session.pending_prompts.len(),
            1,
            "queue must stay when composer text is interjected"
        );
    }

    /// Prompt-path send-now always takes the top visible row (merge order),
    /// even if a later row is selected in the queue pane.
    #[test]
    fn interject_key_from_prompt_ignores_selection_sends_top() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.prompt.set_text("");
        // Select the local row (last in merge order); top is server.
        let ids = agent.queue.entry_ids();
        assert!(ids.len() >= 2);
        agent.queue.list_state.select_by_id(*ids.last().unwrap());

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::QueueInterjectShared { id, .. }) => {
                assert_eq!(
                    id, "p1",
                    "prompt-path must send top (server), not selected local"
                );
            }
            other => panic!("expected QueueInterjectShared of top server row, got {other:?}"),
        }
    }

    /// Bare Enter empty with multi-row queue also sends the top row, not the
    /// last or selected one.
    #[test]
    fn enter_empty_from_prompt_sends_top_not_last() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        agent.prompt.set_text("");

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_prompt_key_for_test(&enter);
        match outcome {
            InputOutcome::Action(Action::QueueInterjectShared { id, .. }) => {
                assert_eq!(id, "p1", "empty Enter must send top (server) row");
            }
            other => panic!("expected QueueInterjectShared of top server row, got {other:?}"),
        }
    }

    /// Backslash continuation mid-turn must only insert the newline — it must
    /// NOT be mistaken for an empty composer and force-send a queued follow-up.
    /// `try_send()` returns `None` in both the empty and continuation cases, so
    /// the send-now path is guarded on an actually-empty composer.
    #[test]
    fn enter_backslash_continuation_does_not_force_send_queued() {
        let mut agent = running_agent_local_only();
        agent.active_pane = AgentPane::Prompt;
        agent.queue.overlay.focused = false;
        // Trailing backslash with the cursor at end (insert_str advances it).
        agent.prompt.set_text("");
        agent.prompt.textarea.insert_str("wip\\");

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let outcome = agent.handle_prompt_key_for_test(&enter);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "backslash continuation must insert a newline, not fire send-now; got {outcome:?}"
        );
        assert_eq!(
            agent.prompt.text(),
            "wip\n",
            "the backslash must be replaced with a newline (continuation applied)"
        );
        assert_eq!(
            agent.session.pending_prompts.len(),
            1,
            "queued follow-up must remain (continuation is not send-now)"
        );
    }

    /// VS Code family: Ctrl+L interjects when running + nonempty (pinned registry).
    #[test]
    fn vscode_ctrl_l_interjects_when_running_nonempty() {
        let mut agent = make_running_agent();
        agent.prompt.set_text("steer please");
        let registry = vscode_family_registry();
        let outcome =
            agent.handle_prompt_key_with_registry_for_test(&vscode_interject_key(), &registry);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "steer please")
            }
            other => panic!("expected Interject, got {other:?}"),
        }
    }

    /// VS Code family: idle Ctrl+L is a no-op (not send, not extensions).
    #[test]
    fn vscode_ctrl_l_idle_is_noop() {
        let mut agent = make_running_agent();
        agent.session.state = AgentState::Idle;
        agent.prompt.set_text("draft");
        let registry = vscode_family_registry();
        let outcome =
            agent.handle_prompt_key_with_registry_for_test(&vscode_interject_key(), &registry);
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "idle VS Ctrl+L must be a no-op, got {outcome:?}"
        );
        assert_eq!(agent.prompt.text(), "draft");
    }

    /// VS Code family queue force-interject uses Ctrl+L (not Ctrl+Enter).
    #[test]
    fn vscode_ctrl_l_force_interjects_queue_row() {
        let mut agent = make_running_agent();
        agent.active_pane = AgentPane::Queue;
        let registry = vscode_family_registry();
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let outcome = agent.handle_queue_key(&vscode_interject_key(), &registry);
        match outcome {
            InputOutcome::Action(Action::SendPromptNow { text, .. }) => {
                assert_eq!(text, "local one")
            }
            other => panic!("expected Interject, got {other:?}"),
        }
        // Ctrl+Enter must not force-interject on VS family (no alt).
        let outcome = agent.handle_queue_key(&force_interject_key(), &registry);
        assert!(
            !matches!(outcome, InputOutcome::Action(Action::Interject { .. })),
            "Ctrl+Enter must not be VS force-interject, got {outcome:?}"
        );
    }
}

#[cfg(test)]
mod watcher_tests {
    use super::super::{test_agent_view, test_fixtures};
    use crate::views::turn_status::Watchers;
    use crate::views::workflows::WorkflowRunSnapshot;

    fn workflow(run_id: &str, status: &str) -> WorkflowRunSnapshot {
        WorkflowRunSnapshot {
            run_id: run_id.to_owned(),
            name: "workflow".to_owned(),
            objective: "objective".to_owned(),
            status: status.to_owned(),
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
            elapsed_ms: 0,
            received_at: std::time::Instant::now(),
            pause_message: None,
            result_summary: None,
        }
    }

    fn insert_bg_task(
        agent: &mut crate::app::agent_view::AgentView,
        task_id: &str,
        is_monitor: bool,
    ) {
        agent.session.bg_tasks.insert(
            task_id.into(),
            crate::app::agent::BgTaskState {
                task_id: task_id.into(),
                tool_call_id: format!("call-{task_id}"),
                command: "sleep 5".into(),
                description: None,
                cwd: "/tmp".into(),
                output_file: "/tmp/out".into(),
                status: crate::app::agent::BgTaskStatus::Running,
                start_time: std::time::SystemTime::now(),
                end_time: None,
                exit_code: None,
                signal: None,
                stdout: String::new(),
                stdout_line_count: 0,
                truncated: false,
                pending_kill: false,
                kill_requested_at: None,
                scrollback_entry_id: None,
                is_monitor,
                restored_from_replay: false,
            },
        );
    }

    #[test]
    fn watchers_counts_monitors_apart_from_commands() {
        let mut agent = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        insert_bg_task(&mut agent, "bg-1", false);
        insert_bg_task(&mut agent, "mon-1", true);
        insert_bg_task(&mut agent, "done-1", false);
        agent.session.bg_tasks.get_mut("done-1").unwrap().status =
            crate::app::agent::BgTaskStatus::Done;
        assert_eq!(
            agent.watchers(),
            Watchers {
                commands: 1,
                monitors: 1,
                loops: 0,
                subagents: 0,
                workflows: 0,
            }
        );
    }

    #[test]
    fn workflow_children_coalesce_into_one_workflow_watcher() {
        let mut agent = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        agent.workflow_runs.push(workflow("wf-1", "active"));
        let mut child_a = test_fixtures::running_subagent_info("child-a");
        child_a.workflow_run_id = Some("wf-1".into());
        let mut child_b = test_fixtures::running_subagent_info("child-b");
        child_b.workflow_run_id = Some("wf-1".into());
        agent.subagent_sessions.insert("child-a".into(), child_a);
        agent.subagent_sessions.insert("child-b".into(), child_b);

        assert_eq!(
            agent.watchers(),
            Watchers {
                commands: 0,
                monitors: 0,
                loops: 0,
                subagents: 0,
                workflows: 1,
            }
        );
    }

    #[test]
    fn paused_workflows_are_not_running_watchers() {
        let mut agent = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        agent.workflow_runs.push(workflow("wf-1", "user_paused"));

        assert_eq!(agent.watchers(), Watchers::default());
    }

    #[test]
    fn standalone_subagent_and_workflow_remain_distinct_watchers() {
        let mut agent = test_agent_view(Some("s1"), std::path::PathBuf::from("/tmp"));
        agent.workflow_runs.push(workflow("wf-1", "active"));
        let mut workflow_child = test_fixtures::running_subagent_info("workflow-child");
        workflow_child.workflow_run_id = Some("wf-1".into());
        agent
            .subagent_sessions
            .insert("workflow-child".into(), workflow_child);
        agent.subagent_sessions.insert(
            "standalone-child".into(),
            test_fixtures::running_subagent_info("standalone-child"),
        );

        assert_eq!(
            agent.watchers(),
            Watchers {
                commands: 0,
                monitors: 0,
                loops: 0,
                subagents: 1,
                workflows: 1,
            }
        );
    }
}
