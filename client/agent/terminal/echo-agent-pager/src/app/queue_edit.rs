//! Queued-prompt editing (`PromptMode::EditingQueued`) state machine.
//!
//! Extracted from `agent_view.rs` as a sibling `impl AgentView` block (same
//! pattern as echo-agent-runtime's `compaction.rs`): entry from the queue pane,
//! editing-mode key intercepts, the dirty-edit focus lock, and the
//! exit/cleanup paths.
//!
//! Stash invariant: `stashed_prompt` is set exactly once on entry
//! (`enter_queue_edit`) and `take()`n exactly once on exit —
//! `exit_editing_mode` is the sole restore owner: every exit (including
//! lost-row cancel) restores the draft exactly once.
//!
//! A dirty pane switch is blocked and never arms the undrawn `EditConfirm`
//! modal, which would otherwise capture all input invisibly.

use crossterm::event::{KeyCode, KeyEvent};

use crate::key;
use crate::views::modal::{ActiveModal, EditConfirmResult, ModalConfirmation};
use crate::views::queue_pane::QueueRowRef;

use super::actions::Action;
use super::agent_view::{AgentPane, AgentView, PromptInputMode};
use super::app_view::InputOutcome;

/// Toast for an edit attempted on an optimistic queue row whose enqueue RPC has not confirmed.
/// Shared by the keyboard and mouse edit paths, which both funnel through `enter_queue_edit`.
pub(in crate::app) const STILL_QUEUEING_TOAST: &str = "Still queueing, try again in a moment";

/// State of the prompt widget's editing context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptMode {
    /// Normal mode: typing a new prompt to send.
    Normal,
    /// Editing a queued prompt.
    EditingQueued {
        /// Stable selection ID of the prompt being edited. For local rows
        /// this is the `QueuedPrompt.id` monotonic counter; for server rows
        /// this is the synthesized `QueuedPromptEntry.id` (a hash of
        /// `server_id`) so [`AgentView::queue`] selection works uniformly.
        id: u64,
        /// Snapshot of the original text (for dirty detection).
        original: String,
        /// When `Some`, this is a server-authoritative shared-queue row and
        /// `server_id` is the agent's stable `prompt_id`. On save we route
        /// the change through `Action::QueueEditShared` (and rely on the
        /// `echo.agent/queue/changed` rebroadcast for the visual result) instead
        /// of mutating the local `pending_prompts` mirror. `None` is the
        /// pre-existing local-origin path.
        server_id: Option<String>,
        /// Kind snapshot for the interject guard's vanished-row fallback.
        kind: crate::app::agent::QueueEntryKind,
    },
}

impl AgentView {
    /// Editing-mode key intercepts for the prompt pane.
    ///
    /// Bare Enter saves, Esc (or Ctrl-C on empty) cancels. Shift/Alt+Enter
    /// inserts a newline (same as the normal composer) and must not save.
    /// Apple Terminal Cmd/Shift/Opt+Enter is rescued inside `is_mod_enter`
    /// via CoreGraphics — not a universal Cmd+Enter binding.
    /// Interject is remappable, so it is handled via the
    /// `ActionId::InterjectPrompt` registry arm → `interject_editing_queued_intercept`,
    /// not matched as a raw key here.
    ///
    /// Returns `None` when not editing or unhandled — must fall through to the widget.
    pub(super) fn handle_editing_queued_key(&mut self, key: &KeyEvent) -> Option<InputOutcome> {
        if let PromptMode::EditingQueued { id, server_id, .. } = &self.prompt_mode {
            let (id, server_id) = (*id, server_id.clone());
            let ctrl_c_empty = key!('c', CONTROL).matches(key) && self.prompt.text().is_empty();

            // Before bare-Enter save: Shift/Alt flags, or Apple Terminal bare
            // Enter with Cmd/Shift/Opt held (CoreGraphics rescue in is_mod_enter).
            if crate::input::is_mod_enter(key) {
                self.prompt.textarea.insert_str("\n");
                return Some(InputOutcome::Changed);
            }
            if key!(Enter).matches(key) && !self.prompt.text().trim().is_empty() {
                return Some(self.save_edited_queued_row(id, server_id, true));
            }
            if key.code == KeyCode::Esc || ctrl_c_empty {
                self.exit_editing_mode();
                return Some(InputOutcome::Action(Action::DrainQueue));
            }
        }
        None
    }

    /// Dirty-edit focus lock checked by `set_active_pane`.
    ///
    /// `None` = not editing / no lock — the caller proceeds with the normal
    /// pane switch. `Some(switched)` = the lock handled the switch: `false`
    /// when blocked behind the confirm modal, `true` after a clean exit.
    pub(super) fn editing_lock_on_pane_switch(&mut self, target: AgentPane) -> Option<bool> {
        if let PromptMode::EditingQueued { ref original, .. } = self.prompt_mode
            && target != AgentPane::Prompt
        {
            let dirty = self.prompt.text() != original;
            if dirty {
                // Block the switch; never arm `EditConfirm` here (it has no draw
                // arm, so it would capture all input invisibly). Resolve with Enter/Esc.
                // Clear the overlay-focus flip toggle callers set before switching.
                match target {
                    AgentPane::Queue => self.queue.overlay.focused = false,
                    AgentPane::Todo => self.todo.overlay.focused = false,
                    AgentPane::Tasks => self.tasks.overlay.focused = false,
                    AgentPane::Catalog => self.catalog.overlay.focused = false,
                    _ => {}
                }
                self.show_toast("Editing a queued prompt: press Enter to save, Esc to discard");
                return Some(false); // blocked, no modal armed
            }
            // Clean edit — silently exit editing mode.
            self.exit_editing_mode();
            self.active_pane = target;
            return Some(true);
        }
        None
    }

    /// Resolve an `EditConfirm` modal keypress (the modal has already been
    /// `take()`n out of `active_modal` by `handle_modal_key`).
    pub(super) fn handle_edit_confirm_choice(
        &mut self,
        confirm: ModalConfirmation<EditConfirmResult>,
        pending_target: AgentPane,
        ch: char,
    ) -> InputOutcome {
        if let Some(result) = confirm.resolve(ch) {
            let was_drain_blocked = self.drain_blocked();
            match result {
                EditConfirmResult::Cancel => {
                    // Dismiss dialog, stay in editing mode.
                    // (active_modal already taken)
                }
                EditConfirmResult::Save => {
                    // Empty edit: keep the original row text — a
                    // queued prompt must never be blanked by Save.
                    if self.prompt.text().trim().is_empty() {
                        self.exit_editing_mode();
                        self.set_active_pane(pending_target, true);
                        if was_drain_blocked {
                            return InputOutcome::Action(Action::DrainQueue);
                        }
                        return InputOutcome::Changed;
                    }
                    let outcome = match self.prompt_mode.clone() {
                        PromptMode::EditingQueued { id, server_id, .. } => {
                            // Drain only when "save & send" was the
                            // advertised label (see helper doc).
                            self.save_edited_queued_row(id, server_id, was_drain_blocked)
                        }
                        // Unreachable in practice: the modal only
                        // opens from `EditingQueued`.
                        _ => {
                            self.exit_editing_mode();
                            InputOutcome::Changed
                        }
                    };
                    self.set_active_pane(pending_target, true);
                    return outcome;
                }
                EditConfirmResult::Discard => {
                    // Discard changes (revert to original), exit editing.
                    self.exit_editing_mode();
                    self.set_active_pane(pending_target, true);
                    if was_drain_blocked {
                        return InputOutcome::Action(Action::DrainQueue);
                    }
                    return InputOutcome::Changed;
                }
                EditConfirmResult::Delete => {
                    // Delete the prompt entirely from the queue.
                    // Server-origin rows route through
                    // `Action::QueueRemoveShared`; local rows mutate
                    // the mirror.
                    if let PromptMode::EditingQueued {
                        id: _,
                        server_id: Some(server_id),
                        ..
                    } = self.prompt_mode.clone()
                    {
                        let expected_version = self
                            .shared_queue
                            .iter()
                            .find(|e| e.id == server_id)
                            .map(|e| e.version)
                            .unwrap_or(0);
                        // Keep the hold until remove lands — release would re-kick promote of the row we are deleting.
                        self.exit_editing_mode_keeping_hold();
                        self.set_active_pane(pending_target, true);
                        return InputOutcome::Action(Action::QueueRemoveShared {
                            id: server_id,
                            expected_version,
                        });
                    }
                    if let PromptMode::EditingQueued { id, .. } = self.prompt_mode {
                        self.session.pending_prompts.retain(|p| p.id != id);
                    }
                    self.exit_editing_mode();
                    self.set_active_pane(pending_target, true);
                    // If drain was blocked and we deleted the front,
                    // the next prompt (if any) should now drain.
                    if was_drain_blocked {
                        return InputOutcome::Action(Action::DrainQueue);
                    }
                    return InputOutcome::Changed;
                }
            }
        } else {
            // Key didn't match any option — restore modal, keep blocking.
            self.active_modal = Some(ActiveModal::EditConfirm {
                modal: confirm,
                pending_target,
            });
        }
        InputOutcome::Changed
    }

    /// Enter editing mode for the queue row selected via
    /// `QueueEvent::EditSelected` (called from `handle_queue_key`).
    pub(super) fn enter_queue_edit(&mut self, id: u64, is_server: bool, row: Option<QueueRowRef>) {
        use crate::app::agent::QueueEntryKind;
        // Optimistic echo whose enqueue RPC has not confirmed: the shell has no row to hold
        // yet, so toast instead of silently dropping the keypress and wait for the confirming
        // `echo.agent/queue/changed` before allowing the edit. Mirrors the send-now park gate in
        // `force_interject_queue_row`: both gates enforce the same unconfirmed-row rule, so a
        // change to one likely applies to the other.
        if let Some(sid) = row.as_ref().and_then(|r| r.server_id.as_deref())
            && self.optimistic_queue_ids.contains(sid)
        {
            self.show_toast(STILL_QUEUEING_TOAST);
            return;
        }
        type QueueEditEntryData = (
            String,
            QueueEntryKind,
            Option<String>,
            Vec<crate::prompt_images::PastedImage>,
            Vec<crate::app::agent::ChipElement>,
        );

        // Resolve text + display kind from whichever mirror owns
        // the row, plus the server `prompt_id` for server-origin
        // rows. The save path in `save_edited_queued_row` / the
        // modal-confirm `Save` arm branches on `server_id`.
        let entry_data: Option<QueueEditEntryData> = if is_server {
            row.as_ref()
                .and_then(|r| r.server_id.clone())
                .and_then(|server_id| {
                    self.shared_queue
                        .iter()
                        .find(|e| e.id == server_id)
                        .map(|w| {
                            (
                                w.text.clone(),
                                crate::views::queue_pane::kind_from_wire(&w.kind),
                                Some(server_id),
                                Vec::new(),
                                Vec::new(),
                            )
                        })
                })
        } else {
            // Only local rows own image and chip state.
            self.session
                .pending_prompts
                .iter()
                .find(|p| p.id == id)
                .map(|p| {
                    (
                        p.text.clone(),
                        p.kind,
                        None,
                        p.images.clone(),
                        p.chip_elements.clone(),
                    )
                })
        };
        if let Some((text, kind, server_id, images, chip_elements)) = entry_data {
            self.stashed_prompt = if self.prompt.text().is_empty() {
                None
            } else {
                Some(self.prompt.stash())
            };
            // Load queued text and enter editing mode.
            // Set prompt_input_mode based on entry kind so the prompt
            // renders with the correct visual (yellow `!` prefix
            // for bash entries, normal for prompts/commands).
            self.prompt
                .restore(crate::views::prompt_widget::StashedPrompt::from_submission(
                    text.clone(),
                    images,
                    chip_elements,
                ));
            // `server_id: Some(_)` routes the save through
            // `Action::QueueEditShared` (server LWW); `None` is
            // the existing local-mirror mutation path.
            self.prompt_mode = PromptMode::EditingQueued {
                id,
                original: text,
                server_id: server_id.clone(),
                kind,
            };
            self.prompt_input_mode = if kind == QueueEntryKind::BashCommand {
                PromptInputMode::Bash
            } else {
                PromptInputMode::Normal
            };
            self.set_active_pane(AgentPane::Prompt, false);
            if let (Some(sid), Some(session_id)) = (server_id, self.session.session_id.clone()) {
                self.pending_effects
                    .push(crate::app::actions::Effect::QueueHoldEdit {
                        session_id,
                        id: sid,
                    });
            }
        } else {
            // The row left the mirror between selection and keypress, so there is nothing to edit.
            self.show_toast("Queued prompt is no longer in the queue");
        }
    }

    /// Save the edited composer text back to the queued row and exit edit
    /// mode. Single owner of the save invariants for the bare-Enter
    /// intercept, the idle edit-interject, and the modal Save arm.
    ///
    /// `drain`: whether a local-row save requests a queue drain. Enter-save
    /// and idle edit-interject always drain (the user just released the
    /// front edit lock); modal Save drains only when the drain was blocked
    /// on this edit — a plain save of a non-front row must not start the
    /// head prompt's turn.
    ///
    /// Text that resolves to a pager builtin leaves through `Action::RunEditedQueuedCommand`
    /// instead, ignoring `drain`: dispatch runs the command and drains once it has settled the row.
    fn save_edited_queued_row(
        &mut self,
        id: u64,
        server_id: Option<String>,
        drain: bool,
    ) -> InputOutcome {
        // A pager builtin left in the row would reach the model verbatim as a literal `/…` string:
        // the agent's resolve() reserves pager-owned names without handling them.
        // Only a `Prompt` row in normal composer mode qualifies; bash and remember rows stay text.
        if matches!(
            self.prompt_mode,
            PromptMode::EditingQueued {
                kind: crate::app::agent::QueueEntryKind::Prompt,
                ..
            }
        ) && self.prompt_input_mode == PromptInputMode::Normal
            && crate::slash::is_complete_builtin_invocation(
                self.prompt.text(),
                self.prompt.slash_controller.registry(),
            )
        {
            let text = self.prompt.text().to_string();
            // A vanished server row has no version to check, so it carries no removal and dispatch
            // just runs the command.
            let server = server_id.as_ref().and_then(|sid| {
                self.queue
                    .row_ref(id)
                    .map(|row| crate::app::actions::SharedQueueTarget {
                        id: sid.clone(),
                        expected_version: row.version,
                    })
            });
            // Release the hold: the action's `QueueRemove` is processed inside `drain_and_process`,
            // before `pending_effects` flush, so it goes out first; a remove rejected on a stale
            // version then returns the row to combine.
            self.exit_editing_mode();
            return InputOutcome::Action(Action::RunEditedQueuedCommand {
                local_id: id,
                server,
                text,
            });
        }
        match server_id {
            Some(server_id) => {
                let new_text = self.prompt.text().to_string();
                // Server-origin row: route the edit through the agent (LWW); the
                // rebroadcast updates every client's mirror, so don't mutate
                // locally. Keep the hold until the edit lands — see
                // `exit_editing_mode_keeping_hold`.
                self.exit_editing_mode_keeping_hold();
                InputOutcome::Action(Action::QueueEditShared {
                    id: server_id,
                    new_text,
                })
            }
            None => {
                let edited = self.prompt.stash();
                let (new_text, mut images, chip_elements) = edited.into_submission();
                // Local row: in-place mutation (existing behavior).
                // Recompute token ranges for the edited text — the stale
                // ranges would point at the pre-edit byte offsets.
                let skill_token_ranges = self
                    .prompt
                    .slash_controller
                    .recognized_token_ranges(&new_text, &self.session.models);
                if let Some(entry) = self.session.pending_prompts.iter_mut().find(|p| p.id == id) {
                    let retained: std::collections::HashSet<u64> = images
                        .iter()
                        .map(|image| image.preview.identity())
                        .collect();
                    for old in entry.images.drain(..) {
                        if !retained.contains(&old.preview.identity()) {
                            crate::prompt_images::cleanup_temp_file(&old);
                        }
                    }
                    entry.text = new_text;
                    entry.images = std::mem::take(&mut images);
                    entry.chip_elements = chip_elements;
                    entry.skill_token_ranges = skill_token_ranges;
                    // Clear stale wire_blocks: edited text may no longer match the original skill
                    // invocation. The prompt will be sent as plain text via the normal path.
                    // Pager builtins never get here (`is_complete_builtin_invocation` routed them
                    // to dispatch); ACP, skill, and unknown `/…` text is left for the agent's
                    // resolve(), which does not know pager builtins.
                    entry.wire_blocks = None;
                    // display_as_skill rides wire_blocks (see its field doc) — clear both
                    // together, or the drain keeps stale skill styling over the ranges.
                    entry.display_as_skill = false;
                }
                crate::prompt_images::drain_and_cleanup(&mut images);
                self.exit_editing_mode();
                if drain {
                    InputOutcome::Action(Action::DrainQueue)
                } else {
                    InputOutcome::Changed
                }
            }
        }
    }

    /// Interject-key intercept while editing a queued row, delegated from
    /// the `ActionId::InterjectPrompt` registry arm in `handle_prompt_key`.
    /// Falling through would strand `EditingQueued` with the row still
    /// queued (dirty-modal loop + blocked drain). `None` = not editing —
    /// the arm proceeds with its normal interject handling.
    pub(super) fn interject_editing_queued_intercept(&mut self) -> Option<InputOutcome> {
        if let PromptMode::EditingQueued {
            id,
            server_id,
            kind,
            ..
        } = &self.prompt_mode
        {
            let (id, server_id, kind) = (*id, server_id.clone(), *kind);
            return Some(self.interject_edited_queued(id, server_id, kind));
        }
        None
    }

    /// Interject key pressed while editing a queued row: turn running →
    /// interject the EDITED text and remove the row; idle → bare-Enter
    /// save; empty composer → no-op, stay in edit mode.
    fn interject_edited_queued(
        &mut self,
        id: u64,
        server_id: Option<String>,
        kind: crate::app::agent::QueueEntryKind,
    ) -> InputOutcome {
        let text = self.prompt.text().trim().to_string();
        if text.is_empty() {
            return InputOutcome::Changed;
        }
        if !self.session.state.is_turn_running() {
            return self.save_edited_queued_row(id, server_id, true);
        }
        // Non-prompt rows stay queued (see `queue_row_prompt_like`): save the edit.
        let row_prompt_like = self.queue_row_prompt_like(id);
        if row_prompt_like == Some(false) {
            self.show_toast("Can't send this mid-turn: it runs when the current turn ends");
            return self.save_edited_queued_row(id, server_id, true);
        }
        if row_prompt_like.is_none() && kind != crate::app::agent::QueueEntryKind::Prompt {
            self.show_toast("Queued prompt is no longer in the queue");
            return self.save_edited_queued_row(id, server_id, true);
        }
        match server_id {
            Some(server_id) => {
                // Server rows: the queue wire (`echo.agent/queue/interject` newText)
                // is text-only, so composer images can't ride along — known
                // limitation, dropped with an accurate toast.
                if !self.prompt.images.is_empty() {
                    self.prompt.images.clear();
                    self.show_toast("Images can't be attached when editing a shared queued prompt");
                }
                // new_text carries the edit — without it the agent would
                // interject the original server-side text.
                let expected_version = self.queue.row_ref(id).map(|r| r.version);
                match expected_version {
                    Some(expected_version) => {
                        // Hold until interject clears it — release would re-kick promote of original text.
                        self.exit_editing_mode_keeping_hold();
                        InputOutcome::Action(Action::QueueInterjectShared {
                            id: server_id,
                            expected_version,
                            new_text: Some(text),
                        })
                    }
                    // Row vanished from the mirror — just interject the text.
                    None => {
                        self.exit_editing_mode();
                        InputOutcome::Action(Action::Interject {
                            text,
                            images: vec![],
                        })
                    }
                }
            }
            None => {
                // Exit before row removal so auto-hide cannot re-enter or strand edit mode.
                let edited = self.prompt.stash();
                let (_, images, _) = edited.into_submission();
                self.exit_editing_mode();
                if let Some(mut row) = self.remove_local_queue_row(id) {
                    let retained: std::collections::HashSet<u64> = images
                        .iter()
                        .map(|image| image.preview.identity())
                        .collect();
                    for old in row.images.drain(..) {
                        if !retained.contains(&old.preview.identity()) {
                            crate::prompt_images::cleanup_temp_file(&old);
                        }
                    }
                }
                InputOutcome::Action(Action::Interject { text, images })
            }
        }
    }

    /// Whether the next turn is held because the user is editing the front prompt.
    ///
    /// - Local rows (`server_id: None`): idle and the edited id is
    ///   `pending_prompts` front.
    /// - Server rows (`server_id: Some(sid)`): idle and `sid` is the front of
    ///   `shared_queue` (wire excludes the running turn, so index 0 is next).
    pub(crate) fn drain_blocked(&self) -> bool {
        let PromptMode::EditingQueued { id, server_id, .. } = &self.prompt_mode else {
            return false;
        };
        if !self.session.state.is_idle() {
            return false;
        }
        match server_id {
            Some(sid) => self.shared_queue.first().is_some_and(|e| e.id == *sid),
            None => self
                .session
                .pending_prompts
                .front()
                .is_some_and(|p| p.id == *id),
        }
    }

    /// Exit a server-origin queue edit whose row vanished, restoring the pre-edit draft.
    pub(crate) fn cancel_editing_queued_for_lost_row(&mut self) {
        if !matches!(
            self.prompt_mode,
            PromptMode::EditingQueued {
                server_id: Some(_),
                ..
            }
        ) {
            return;
        }
        // Restore the pre-edit draft; keeping the orphaned edit text would look
        // "duplicated" (the row is now the running turn). A concurrent-removal edit is lost.
        self.exit_editing_mode();
        self.show_toast("Queued prompt is no longer in the queue");
    }

    /// Exit editing mode: restore stashed text, clear mode, focus queue pane.
    /// No-op unless `EditingQueued`. The default exit; releases the
    /// server-side combine hold (cancel, lost-row, modal paths).
    ///
    /// Always resets `prompt_input_mode` to `Normal` so it doesn't leak
    /// into subsequent normal prompt entry.
    pub(super) fn exit_editing_mode(&mut self) {
        self.exit_editing_mode_inner(true);
    }

    /// Exit editing without emitting `QueueReleaseEdit` — the server-row save
    /// path's `QueueEditShared` clears the hold on the shell instead. Releasing
    /// here would flush first (via `pending_effects`) and let combine merge the
    /// row on stale text before the edit lands.
    fn exit_editing_mode_keeping_hold(&mut self) {
        self.exit_editing_mode_inner(false);
    }

    fn exit_editing_mode_inner(&mut self, release_hold: bool) {
        // Idempotent: remove_local_queue_row's guard may have exited already;
        // a second take() of the spent stash would wipe the composer.
        if !matches!(self.prompt_mode, PromptMode::EditingQueued { .. }) {
            return;
        }
        if release_hold
            && let PromptMode::EditingQueued {
                server_id: Some(sid),
                ..
            } = &self.prompt_mode
            && let Some(session_id) = self.session.session_id.clone()
        {
            self.pending_effects
                .push(crate::app::actions::Effect::QueueReleaseEdit {
                    session_id,
                    id: sid.clone(),
                });
        }
        let stash = self.stashed_prompt.take().unwrap_or_default();
        self.prompt.restore(stash);
        self.finish_editing_exit();
    }

    /// Shared tail of every edit exit — stash policy stays with the callers.
    fn finish_editing_exit(&mut self) {
        self.prompt_mode = PromptMode::Normal;
        self.prompt_input_mode = PromptInputMode::Normal;
        // Editing is over, so a pending EditConfirm is meaningless — left
        // behind it would eat all input without ever rendering. Other
        // modal variants are untouched.
        if matches!(self.active_modal, Some(ActiveModal::EditConfirm { .. })) {
            self.active_modal = None;
        }
        // Return focus to queue pane (if still visible).
        // Force=true: we just cleared editing mode, no lock to check.
        if self.queue.is_visible() {
            self.set_active_pane(AgentPane::Queue, true);
        } else {
            self.set_active_pane(AgentPane::Scrollback, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol as acp;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::actions::{Action, Effect, SharedQueueTarget};
    use crate::app::agent::AgentState;
    use crate::app::agent_view::test_fixtures::{
        force_interject_key, make_running_agent, non_vscode_registry, running_agent_local_only,
        test_pasted_image,
    };
    use crate::app::agent_view::{AgentPane, AgentView, PromptMode};
    use crate::app::app_view::InputOutcome;
    use crate::scrollback::block::RenderBlock;
    use crate::views::modal::{ActiveModal, ModalConfirmation};

    fn edit_key() -> KeyEvent {
        // Default key binding for QueueEvent::EditSelected.
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)
    }

    fn delete_key() -> KeyEvent {
        // Default key binding for QueueEvent::DeleteSelected.
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
    }

    fn enter_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn enter_edit_local_row() -> AgentView {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        let ids = agent.queue.entry_ids();
        // Local row is second (server rendered first).
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        assert!(matches!(
            agent.prompt_mode,
            PromptMode::EditingQueued { .. }
        ));
        agent
    }

    /// Shift/Alt+Enter → newline in edit mode (must not save).
    /// Cmd/SUPER is not a product-wide newline chord (Apple Terminal only via CG).
    /// `/btw why` fences the ordering: mod-Enter beats the hijack in `save_edited_queued_row`.
    #[test]
    fn edit_mod_enter_inserts_newline_without_exiting() {
        for mods in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
            for text in ["line1", "/btw why"] {
                let mut agent = enter_edit_local_row();
                agent.prompt.set_text(text);
                let outcome =
                    agent.handle_prompt_key_for_test(&KeyEvent::new(KeyCode::Enter, mods));
                assert!(
                    matches!(outcome, InputOutcome::Changed),
                    "mod Enter ({mods:?}, {text:?}) must not save; got {outcome:?}"
                );
                assert!(
                    matches!(agent.prompt_mode, PromptMode::EditingQueued { .. }),
                    "must stay in edit mode for {mods:?}, {text:?}"
                );
                assert_eq!(
                    agent.prompt.text(),
                    format!("{text}\n"),
                    "mod Enter ({mods:?}) must insert a newline"
                );
                assert_eq!(
                    agent.session.pending_prompts[0].text, "local one",
                    "queue row must stay unchanged for {mods:?}, {text:?}"
                );
            }
        }
    }

    /// Bare Enter still saves (mod-enter path must not steal it).
    #[test]
    fn edit_bare_enter_still_saves() {
        let mut agent = enter_edit_local_row();
        agent.prompt.set_text("line1 EDITED");
        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        assert!(
            matches!(outcome, InputOutcome::Action(Action::DrainQueue)),
            "bare Enter must save; got {outcome:?}"
        );
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert_eq!(agent.session.pending_prompts[0].text, "line1 EDITED");
    }

    fn attach_image_to_local_row(agent: &mut AgentView) {
        let mut image = test_pasted_image();
        image.display_number = 1;
        let row = agent.session.pending_prompts.front_mut().unwrap();
        row.text = "local one [Image #1] ".into();
        row.images = vec![image];
        row.chip_elements = vec![crate::app::agent::ChipElement {
            range: 10..20,
            kind: crate::views::prompt_widget::KIND_IMAGE,
            display: None,
        }];
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            None,
            None,
            &agent.send_now_painted_blocks,
        );
    }

    /// Edit on an optimistic (unconfirmed) server row toasts and stays Normal.
    #[test]
    fn edit_on_optimistic_server_row_toasts() {
        let mut agent = make_running_agent();
        agent.optimistic_queue_ids.insert("p1".into());
        let registry = non_vscode_registry();
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);

        assert!(
            matches!(agent.prompt_mode, PromptMode::Normal),
            "an unconfirmed echo must not be editable"
        );
        assert_eq!(
            agent.toast.as_ref().map(|(message, _)| message.as_str()),
            Some(super::STILL_QUEUEING_TOAST),
        );
        assert!(
            agent.pending_effects.is_empty(),
            "no QueueHoldEdit may be emitted for a row the shell doesn't have"
        );
    }

    /// Edit on a row no longer in the mirror toasts instead of a silent drop.
    #[test]
    fn edit_on_vanished_row_toasts() {
        let mut agent = make_running_agent();
        let row = crate::views::queue_pane::QueueRowRef {
            origin: crate::views::queue_pane::QueueRowOrigin::Server,
            server_id: Some("gone".into()),
            version: 0,
        };
        agent.enter_queue_edit(999, true, Some(row));

        assert!(
            matches!(agent.prompt_mode, PromptMode::Normal),
            "a vanished row must not enter edit mode"
        );
        assert_eq!(
            agent.toast.as_ref().map(|(message, _)| message.as_str()),
            Some("Queued prompt is no longer in the queue"),
        );
    }

    /// Editing a Server-origin row enters `EditingQueued` with `server_id`
    /// populated (the new behavior replacing the "isn't supported yet" toast).
    #[test]
    fn edit_server_row_enters_editing_queued_with_server_id() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        // Server row first.
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);

        match &agent.prompt_mode {
            PromptMode::EditingQueued {
                server_id: Some(sid),
                original,
                ..
            } => {
                assert_eq!(sid, "p1");
                assert_eq!(original, "server one");
            }
            other => panic!("expected EditingQueued with server_id Some, got {other:?}"),
        }
        assert_eq!(agent.prompt.text(), "server one");
    }

    /// Idle + editing the shared-queue front blocks drain UI.
    #[test]
    fn drain_blocked_true_editing_server_front_while_idle() {
        let mut agent = make_running_agent();
        agent.session.state = AgentState::Idle;
        let registry = non_vscode_registry();
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        assert!(
            agent.drain_blocked(),
            "idle + editing shared-queue front must block drain"
        );
    }

    /// Editing a non-front shared row does not block drain.
    #[test]
    fn drain_blocked_false_editing_server_non_front() {
        let mut agent = make_running_agent();
        agent.session.state = AgentState::Idle;
        agent.shared_queue = vec![
            crate::app::prompt_queue::QueueEntryWire {
                id: "p1".into(),
                version: 1,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "front".into(),
                position: 0,
                combined_texts: None,
            },
            crate::app::prompt_queue::QueueEntryWire {
                id: "p2".into(),
                version: 1,
                owner: None,
                last_editor: None,
                kind: "prompt".into(),
                text: "back".into(),
                position: 1,
                combined_texts: None,
            },
        ];
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );
        agent.prompt_mode = PromptMode::EditingQueued {
            id: 0,
            original: "back".into(),
            server_id: Some("p2".into()),
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        assert!(
            !agent.drain_blocked(),
            "editing a non-front shared row must not block drain"
        );
    }

    /// Running turn: server front edit is not "drain blocked" (shell still holds).
    #[test]
    fn drain_blocked_false_editing_server_front_while_running() {
        let mut agent = make_running_agent();
        assert!(matches!(agent.session.state, AgentState::TurnRunning));
        let registry = non_vscode_registry();
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        assert!(
            !agent.drain_blocked(),
            "while a turn is running, drain_blocked is false (promote gate is shell-side)"
        );
    }

    /// Submitting an edit on a server-origin row dispatches
    /// `Action::QueueEditShared` and does NOT mutate the local mirror.
    #[test]
    fn submit_server_edit_routes_to_action_no_local_mutation() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);

        // Type a replacement.
        agent.prompt.set_text("server one EDITED");

        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        match outcome {
            InputOutcome::Action(Action::QueueEditShared { id, new_text }) => {
                assert_eq!(id, "p1");
                assert_eq!(new_text, "server one EDITED");
            }
            other => panic!("expected QueueEditShared, got {other:?}"),
        }
        // Local mirror untouched.
        assert_eq!(agent.shared_queue.len(), 1);
        assert_eq!(agent.shared_queue[0].text, "server one");
        // EditingQueued cleared.
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// Saving a server-row edit must not emit `QueueReleaseEdit` — see
    /// `exit_editing_mode_keeping_hold`.
    #[test]
    fn submit_server_edit_keeps_combine_hold_until_edit() {
        use crate::app::actions::Effect;
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        // Entering edit on a server row arms the hold.
        assert!(
            agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::QueueHoldEdit { .. })),
            "entering edit must emit QueueHoldEdit"
        );

        agent.prompt.set_text("server one EDITED");
        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::QueueEditShared { .. })
            ),
            "save must route to QueueEditShared"
        );
        assert!(
            !agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::QueueReleaseEdit { .. })),
            "server-row save must NOT emit QueueReleaseEdit (the edit clears the hold)"
        );
    }

    /// Cancelling (Esc) a server-row edit still releases the hold, so an
    /// abandoned edit can't pin the row out of combine.
    #[test]
    fn cancel_server_edit_releases_combine_hold() {
        use crate::app::actions::Effect;
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.pending_effects.clear();

        let _ = agent.handle_prompt_key_for_test(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::QueueReleaseEdit { .. })),
            "cancelling an edit must emit QueueReleaseEdit"
        );
    }

    #[test]
    fn shared_queue_edit_rejects_image_before_normal_save() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        let ctx = crate::app::actions::ClipboardPasteContext {
            target: crate::app::actions::ClipboardPasteTarget::AgentPrompt {
                agent_id: agent.session.id,
                images_dir: None,
                from_feedback_pane: false,
            },
            source: crate::app::actions::ClipboardPasteSource::ClipboardKey {
                text: crate::app::actions::ClipboardTextRead::Success(None),
                tip_showing: false,
            },
        };
        let completion = agent.complete_clipboard_attachment_paste(
            ctx,
            crate::app::actions::ProbedAttachment::Image(test_pasted_image()),
            None,
        );
        assert_eq!(
            completion,
            crate::app::actions::ClipboardPasteCompletion::Failed(
                crate::app::actions::ClipboardPasteFailure::AlreadyReported,
            )
        );
        assert!(agent.prompt.images.is_empty());
        assert!(!agent.prompt.text().contains("[Image #"));
        assert!(agent.toast.is_some());

        agent.prompt.set_text("server one EDITED");
        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        match outcome {
            InputOutcome::Action(Action::QueueEditShared { new_text, .. }) => {
                assert_eq!(new_text, "server one EDITED");
                assert!(!new_text.contains("[Image #"));
            }
            other => panic!("expected QueueEditShared, got {other:?}"),
        }
    }

    /// Submitting an edit on a local-origin row continues to mutate the local
    /// `pending_prompts` in place (existing behavior preserved).
    #[test]
    fn submit_local_edit_mutates_local_pending_prompts() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        // Local row is second (server rendered first).
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);

        agent.prompt.set_text("local one EDITED");

        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        match outcome {
            InputOutcome::Action(Action::DrainQueue) => {}
            other => panic!("expected DrainQueue, got {other:?}"),
        }
        assert_eq!(agent.session.pending_prompts.len(), 1);
        assert_eq!(agent.session.pending_prompts[0].text, "local one EDITED");
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// Editing a skill-injection row clears `display_as_skill` alongside
    /// `wire_blocks`, so the drained echo styles the recomputed mid-text
    /// ranges (not stale leading-token skill styling) and the wire send is
    /// plain text.
    #[test]
    fn edit_skill_row_drains_with_recomputed_ranges_not_skill_styling() {
        let mut agent = running_agent_local_only();
        agent.session.state = AgentState::Idle;
        let registry = non_vscode_registry();
        // Advertise the skill so the edited text's mid-text token is recognized.
        let models = agent.session.models.clone();
        agent.prompt.sync_acp_commands(
            &[
                acp::AvailableCommand::new("pr-workflow", "PR workflow skill").meta(
                    serde_json::json!({
                        "path": "/tmp/skills/pr-workflow/SKILL.md",
                        "scope": "local",
                    })
                    .as_object()
                    .cloned(),
                ),
            ],
            None,
            &models,
        );
        // Turn the fixture row into an InjectSkill-shaped row.
        {
            let row = agent.session.pending_prompts.front_mut().unwrap();
            row.text = "/pr-workflow ship it".into();
            row.wire_blocks = Some(vec![acp::ContentBlock::Text(acp::TextContent::new(
                "<skill>pr-workflow instructions</skill>",
            ))]);
            row.display_as_skill = true;
        }
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            None,
            None,
            &agent.send_now_painted_blocks,
        );

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("great /pr-workflow go");
        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        assert!(matches!(outcome, InputOutcome::Action(Action::DrainQueue)));

        let mut app = crate::app::app_view::tests::test_app();
        let id = agent.session.id;
        app.agents.insert(id, agent);
        let effects = crate::app::dispatch::maybe_drain_queue_and_note_peek(&mut app, id);
        let agent = app.agents.get(&id).unwrap();
        match &agent.scrollback.get(0).unwrap().block {
            RenderBlock::UserPrompt(b) => {
                assert_eq!(b.text, "great /pr-workflow go");
                assert_eq!(
                    b.skill_token_ranges,
                    vec![6..18],
                    "echo must style the recomputed mid-text token"
                );
            }
            other => panic!("expected UserPrompt, got {other:?}"),
        }
        match &effects[0] {
            Effect::SendPrompt {
                text,
                skill_token_ranges,
                ..
            } => {
                assert_eq!(text, "great /pr-workflow go");
                assert_eq!(skill_token_ranges, &vec![6..18]);
            }
            other => panic!("expected plain SendPrompt, got {other:?}"),
        }
    }

    #[test]
    fn edit_local_row_into_builtin_routes_to_run_edited_queued_command() {
        let mut agent = enter_edit_local_row();
        let row_id = agent.session.pending_prompts[0].id;
        agent.prompt.set_text("/btw what is the default");

        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        match outcome {
            InputOutcome::Action(Action::RunEditedQueuedCommand {
                local_id,
                server,
                text,
            }) => {
                assert_eq!(local_id, row_id);
                assert_eq!(server, None);
                assert_eq!(text, "/btw what is the default");
            }
            other => panic!("expected RunEditedQueuedCommand, got {other:?}"),
        }
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert_eq!(
            agent.session.pending_prompts[0].text, "local one",
            "the view must not remove the row: dispatch drops it after its guards pass"
        );
    }

    #[test]
    fn builtin_accepted_from_dropdown_mid_edit_runs_on_the_next_enter() {
        let mut agent = enter_edit_local_row();
        agent.prompt.set_text("/btw");
        agent.prompt.set_cursor(4);
        agent.prompt.refresh_slash(&agent.session.models);
        assert!(agent.prompt.slash_open(), "menu must be live in edit mode");
        // Highlight `/btw` explicitly: the ranker's order is not under test.
        let idx = agent
            .prompt
            .slash_snapshot()
            .matches
            .iter()
            .position(|row| row.display == "/btw")
            .expect("/btw in the slash menu");
        for _ in 0..idx {
            agent.prompt.slash_move_selection(1);
        }

        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "accepting the menu row must not save, got {outcome:?}"
        );
        assert!(matches!(
            agent.prompt_mode,
            PromptMode::EditingQueued { .. }
        ));
        assert!(agent.prompt.text().starts_with("/btw"));

        // Type the question so the slash state tracks the edit, as it does live.
        for ch in "why".chars() {
            let _ = agent
                .handle_prompt_key_for_test(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(agent.prompt.text(), "/btw why");
        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::RunEditedQueuedCommand { .. })
            ),
            "expected RunEditedQueuedCommand, got {outcome:?}"
        );
    }

    /// Fail-closed: only a complete builtin invocation at position 0 is hijacked. Everything else
    /// saves as text exactly as before.
    #[test]
    fn incomplete_unknown_and_mid_text_slash_edits_still_save_as_text() {
        // `/btw` alone requires args; `/nope` is unknown (agent pass-through); a mid-text token
        // is not an invocation.
        for text in ["/btw", "/nope x", "great /compact go"] {
            let mut agent = enter_edit_local_row();
            agent.prompt.set_text(text);
            let outcome = agent.handle_prompt_key_for_test(&enter_key());
            assert!(
                matches!(outcome, InputOutcome::Action(Action::DrainQueue)),
                "{text:?} must save as text; got {outcome:?}"
            );
            assert_eq!(agent.session.pending_prompts[0].text, text);
        }
    }

    /// A bash row edited into `/btw …` stays a bash command: the composer is in bash mode, so the
    /// text is a shell string, not a pager command.
    #[test]
    fn edit_local_bash_row_into_builtin_saves_as_bash_text() {
        use crate::app::agent::QueueEntryKind;
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        agent.session.pending_prompts.clear();
        agent.session.enqueue_bash_command("ls".into());
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );

        let ids = agent.queue.entry_ids();
        // The local bash row renders after the fixture's server row.
        agent.queue.list_state.select_by_id(*ids.last().unwrap());
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("/btw why");

        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        assert!(
            matches!(outcome, InputOutcome::Action(Action::DrainQueue)),
            "bash rows are never hijacked, got {outcome:?}"
        );
        let row = &agent.session.pending_prompts[0];
        assert_eq!(row.text, "/btw why");
        assert_eq!(row.kind, QueueEntryKind::BashCommand, "kind must survive");
    }

    /// Server row: the hijack carries the row's `expected_version` and never mutates the shared
    /// mirror (the rebroadcast is the source of truth).
    #[test]
    fn edit_server_row_into_builtin_carries_versioned_removal() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("/btw why");

        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        match outcome {
            InputOutcome::Action(Action::RunEditedQueuedCommand { server, text, .. }) => {
                assert_eq!(
                    server,
                    Some(SharedQueueTarget {
                        id: "p1".into(),
                        expected_version: 2,
                    })
                );
                assert_eq!(text, "/btw why");
            }
            other => panic!("expected RunEditedQueuedCommand, got {other:?}"),
        }
        assert_eq!(agent.shared_queue.len(), 1);
        assert_eq!(agent.shared_queue[0].text, "server one");
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// The hijack sends no edit, so the combine hold must be released exactly once.
    #[test]
    fn edit_server_row_into_builtin_releases_combine_hold_once() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.pending_effects.clear();
        agent.prompt.set_text("/btw why");

        let _ = agent.handle_prompt_key_for_test(&enter_key());
        assert_eq!(
            agent
                .pending_effects
                .iter()
                .filter(|e| matches!(e, Effect::QueueReleaseEdit { .. }))
                .count(),
            1,
            "an abandoned hold would pin the row out of combine forever"
        );
    }

    /// Server row dropped by a rebroadcast mid-edit: there is no version to check, so the hijack
    /// carries no removal instead of guessing one.
    #[test]
    fn edit_vanished_server_row_into_builtin_carries_no_removal() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);

        agent.shared_queue.clear();
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            None,
            None,
            &agent.send_now_painted_blocks,
        );
        agent.prompt.set_text("/btw why");

        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        match outcome {
            InputOutcome::Action(Action::RunEditedQueuedCommand { server, .. }) => {
                assert_eq!(server, None);
            }
            other => panic!("expected RunEditedQueuedCommand, got {other:?}"),
        }
    }

    #[test]
    fn cancel_editing_queued_for_lost_row_exits_when_server_origin() {
        let mut agent = make_running_agent();
        agent.stashed_prompt = Some(crate::views::prompt_widget::StashedPrompt {
            text: "pre-edit draft".into(),
            cursor: 0,
            images: Vec::new(),
            chip_elements: Vec::new(),
            image_counter: 0,
            image_undo_stash: Vec::new(),
        });
        agent.prompt.set_text("replacement typed over queued row");
        agent.prompt_mode = PromptMode::EditingQueued {
            id: 999,
            original: "server one".into(),
            server_id: Some("p1".into()),
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        agent.cancel_editing_queued_for_lost_row();
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert_eq!(agent.prompt.text(), "pre-edit draft");
        assert!(agent.stashed_prompt.is_none());
    }

    /// Idempotent: the cleanup hook is a no-op when not in `EditingQueued`
    /// with a server origin (e.g. local-origin edit, normal mode).
    #[test]
    fn cancel_editing_queued_for_lost_row_noop_for_local_or_normal() {
        let mut agent = make_running_agent();
        // Local-origin: should NOT exit (the local row hasn't disappeared).
        agent.prompt_mode = PromptMode::EditingQueued {
            id: 0,
            original: "local one".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        };
        agent.cancel_editing_queued_for_lost_row();
        assert!(matches!(
            agent.prompt_mode,
            PromptMode::EditingQueued { .. }
        ));

        // Normal mode: no-op.
        agent.prompt_mode = PromptMode::Normal;
        agent.cancel_editing_queued_for_lost_row();
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// A dirty pane switch must not arm the undrawn EditConfirm; lost-row cancel
    /// restores the pre-edit draft.
    #[test]
    fn dirty_pane_switch_blocks_without_modal_then_lost_row_restores_draft() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        agent.prompt.set_text("draft");

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("server one EDITED");

        assert!(!agent.set_active_pane(AgentPane::Scrollback, false));
        assert!(
            agent.active_modal.is_none(),
            "dirty pane switch must never arm an invisible input-eating EditConfirm"
        );
        assert!(
            matches!(agent.prompt_mode, PromptMode::EditingQueued { .. }),
            "blocked switch keeps the user in the edit"
        );

        agent.cancel_editing_queued_for_lost_row();
        assert!(agent.active_modal.is_none());
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert_eq!(agent.prompt.text(), "draft");
    }

    /// Ctrl+; (toggle_queue_pane) while dirty-editing must not brick: the switch
    /// is blocked, no modal armed, and the queue overlay is not left focused.
    #[test]
    fn toggle_queue_pane_while_dirty_editing_does_not_brick() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("server one EDITED");
        assert!(matches!(
            agent.prompt_mode,
            PromptMode::EditingQueued { .. }
        ));

        agent.toggle_queue_pane();

        assert!(
            agent.active_modal.is_none(),
            "toggling the queue mid-edit must never arm an invisible EditConfirm"
        );
        assert!(
            matches!(agent.prompt_mode, PromptMode::EditingQueued { .. }),
            "the blocked switch keeps the user in the edit"
        );
        assert!(
            !agent.queue.overlay.focused,
            "a blocked switch must not leave the queue overlay focused while input is in the prompt"
        );
    }

    /// Interject key while editing a LOCAL queued row mid-turn: the row
    /// leaves the queue, the EDITED text interjects, edit mode exits, and
    /// the stashed composer draft is restored (the pre-fix bug stranded
    /// `EditingQueued` with the row still queued).
    #[test]
    fn edit_interject_local_row_interjects_edited_text() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        agent.prompt.set_text("draft");

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("local one EDITED");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::Interject { text, .. }) => {
                assert_eq!(text, "local one EDITED");
            }
            other => panic!("expected Interject, got {other:?}"),
        }
        assert!(agent.session.pending_prompts.is_empty());
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert_eq!(agent.prompt.text(), "draft");
    }

    #[test]
    fn queue_edit_cancel_restores_draft_image_state() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        attach_image_to_local_row(&mut agent);
        agent.prompt.set_text("draft ");
        let draft_end = agent.prompt.text().len();
        agent.prompt.set_cursor(draft_end);
        agent.prompt.insert_image(test_pasted_image()).unwrap();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        assert_eq!(agent.prompt.text(), "local one [Image #1] ");
        assert_eq!(agent.prompt.images.len(), 1);
        agent.prompt.set_text("discard this edit");

        let outcome =
            agent.handle_prompt_key_for_test(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(outcome, InputOutcome::Action(Action::DrainQueue)));
        assert_eq!(agent.prompt.text(), "draft [Image #1] ");
        assert_eq!(
            agent
                .prompt
                .textarea()
                .elements()
                .iter()
                .filter(|element| element.kind == crate::views::prompt_widget::KIND_IMAGE)
                .count(),
            1
        );
        assert_eq!(
            agent.prompt.drain_images().len(),
            1,
            "restored draft image must remain sendable"
        );
        let row = agent.session.pending_prompts.front().unwrap();
        assert_eq!(row.text, "local one [Image #1] ");
        assert_eq!(row.images.len(), 1);
        assert_eq!(row.chip_elements.len(), 1);
    }

    #[test]
    fn queue_edit_save_moves_complete_state_into_send() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        attach_image_to_local_row(&mut agent);

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent
            .prompt
            .insert_image(crate::prompt_images::from_clipboard_data(
                &crate::clipboard::ImageData {
                    data: vec![1, 2, 3],
                    mime_type: "image/png".into(),
                },
            ))
            .unwrap();

        let outcome = agent.handle_prompt_key_for_test(&enter_key());
        assert!(matches!(outcome, InputOutcome::Action(Action::DrainQueue)));
        let row = agent.session.pending_prompts.front().unwrap();
        assert_eq!(row.images.len(), 2);
        assert_eq!(
            row.chip_elements
                .iter()
                .filter(|chip| chip.kind == crate::views::prompt_widget::KIND_IMAGE)
                .count(),
            2
        );

        agent.session.state = AgentState::Idle;
        // Running server turn completed: its shared-queue row is gone, so the
        // local edited row is the next turn (server-owns-next-turn gate clears).
        agent.shared_queue.clear();
        let mut app = crate::app::app_view::tests::test_app();
        let id = agent.session.id;
        app.agents.insert(id, agent);
        let effects = crate::app::dispatch::maybe_drain_queue_and_note_peek(&mut app, id);
        assert!(matches!(
            effects.as_slice(),
            [Effect::SendPromptBlocks { .. }]
        ));
        let agent = app.agents.get(&id).unwrap();
        let in_flight = agent.session.in_flight_prompt.as_ref().unwrap();
        assert_eq!(in_flight.images.len(), 2);
        assert_eq!(in_flight.chip_elements.len(), 2);
    }

    /// Lone-local-row agent with "draft" stashed, edit mode entered on the
    /// row. Interjecting empties the queue → the pane auto-hide switch runs
    /// mid-flow (the setup `make_running_agent` can't reach: its server row
    /// keeps the pane open).
    fn edit_lone_local_row() -> AgentView {
        let mut agent = running_agent_local_only();
        let registry = non_vscode_registry();
        agent.prompt.set_text("draft");
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        // Fixture breakage must fail here, not in the flows under test.
        assert!(matches!(
            agent.prompt_mode,
            PromptMode::EditingQueued { .. }
        ));
        agent
    }

    /// Edit-interject of a local bash row saves the edit and keeps it queued.
    #[test]
    fn edit_interject_local_bash_row_saves_edit_and_keeps_kind() {
        use crate::app::agent::QueueEntryKind;
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        agent.session.pending_prompts.clear();
        agent.session.enqueue_bash_command("ls".into());
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );

        let ids = agent.queue.entry_ids();
        // The local bash row renders after the fixture's server row.
        agent.queue.list_state.select_by_id(*ids.last().unwrap());
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        assert!(matches!(
            agent.prompt_mode,
            PromptMode::EditingQueued { .. }
        ));
        agent.prompt.set_text("ls -la");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        assert!(
            !matches!(outcome, InputOutcome::Action(Action::Interject { .. })),
            "bash edit-interject must not interject, got {outcome:?}"
        );
        let row = &agent.session.pending_prompts[0];
        assert_eq!(row.text, "ls -la", "edit must be saved");
        assert_eq!(row.kind, QueueEntryKind::BashCommand, "kind must survive");
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert!(agent.toast.is_some(), "guard must explain itself");
    }

    /// Interject a DIRTY edit of the last visible queue row: the auto-hide
    /// pane switch must not strand an invisible `EditConfirm` modal that
    /// silently consumes all subsequent input.
    #[test]
    fn edit_interject_lone_local_row_dirty_leaves_no_orphaned_modal() {
        let mut agent = edit_lone_local_row();
        agent.prompt.set_text("local one EDITED");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::Interject { text, .. }) => {
                assert_eq!(text, "local one EDITED");
            }
            other => panic!("expected Interject, got {other:?}"),
        }
        assert!(agent.session.pending_prompts.is_empty());
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert!(
            agent.active_modal.is_none(),
            "orphaned EditConfirm would brick input"
        );
        assert_eq!(agent.prompt.text(), "draft");
    }

    /// Interject a CLEAN (unmodified) edit of the last visible queue row:
    /// the auto-hide pane switch must not exit editing mode a second time
    /// and wipe the just-restored composer draft.
    #[test]
    fn edit_interject_lone_local_row_clean_restores_draft_once() {
        let mut agent = edit_lone_local_row();

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::Interject { text, .. }) => {
                assert_eq!(text, "local one");
            }
            other => panic!("expected Interject, got {other:?}"),
        }
        assert!(agent.session.pending_prompts.is_empty());
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert!(agent.active_modal.is_none());
        assert_eq!(
            agent.prompt.text(),
            "draft",
            "stash must be restored exactly once, not wiped by a double exit"
        );
    }

    /// Deleting the lone LOCAL row while it is being edited (dirty): the
    /// delete discards the edit — it must not leave `EditingQueued` pointing
    /// at a removed row or let the auto-hide arm the invisible modal.
    #[test]
    fn delete_edited_lone_local_row_discards_edit_without_orphaned_modal() {
        let mut agent = edit_lone_local_row();
        let registry = non_vscode_registry();
        agent.prompt.set_text("local one EDITED");

        let _ = agent.handle_queue_key(&delete_key(), &registry);

        assert!(agent.session.pending_prompts.is_empty());
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert!(
            agent.active_modal.is_none(),
            "orphaned EditConfirm would brick input"
        );
        assert_eq!(agent.prompt.text(), "draft");
    }

    /// `PromptMode` for an in-progress edit of the fixture's local row.
    fn editing_lone_local() -> PromptMode {
        PromptMode::EditingQueued {
            id: 0,
            original: "local one".into(),
            server_id: None,
            kind: crate::app::agent::QueueEntryKind::Prompt,
        }
    }

    /// `exit_editing_mode` clears a stray `EditConfirm` (unreachable in-tree
    /// after the reorder, so pin the backstop directly) and leaves every
    /// other modal variant alone.
    #[test]
    fn exit_editing_mode_clears_only_stray_edit_confirm() {
        let mut agent = make_running_agent();
        agent.prompt_mode = editing_lone_local();
        agent.active_modal = Some(ActiveModal::EditConfirm {
            modal: ModalConfirmation::edit_confirm(),
            pending_target: AgentPane::Scrollback,
        });
        agent.exit_editing_mode();
        assert!(agent.active_modal.is_none());
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));

        // Scoping: a non-EditConfirm modal survives the exit untouched.
        // Re-enter edit mode so the guarded body (not the idempotency
        // early-return) is what leaves the palette alone.
        agent.prompt_mode = editing_lone_local();
        agent.active_modal = Some(ActiveModal::CommandPalette {
            entries: crate::views::modal::default_palette_entries(
                agent.sharing_enabled,
                &agent.prompt.slash_controller,
            ),
            state: crate::views::picker::PickerState::input_active(),
            window: crate::views::modal_window::ModalWindowState::new(),
        });
        agent.exit_editing_mode();
        assert!(matches!(
            agent.active_modal,
            Some(ActiveModal::CommandPalette { .. })
        ));
    }

    /// `exit_editing_mode` outside `EditingQueued` is a no-op — the
    /// remove-then-exit caller shape must not wipe the composer by
    /// double-taking the spent stash.
    #[test]
    fn exit_editing_mode_is_noop_when_not_editing() {
        let mut agent = make_running_agent();
        agent.prompt.set_text("draft");
        agent.exit_editing_mode();
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
        assert_eq!(agent.prompt.text(), "draft");
    }

    /// Interject key while editing a SERVER queued row mid-turn routes to
    /// `Action::QueueInterjectShared` with the edited text as `new_text`
    /// and exits edit mode without touching the local mirror.
    #[test]
    fn edit_interject_server_row_routes_to_queue_interject_shared_with_new_text() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("server one EDITED");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::QueueInterjectShared {
                id,
                expected_version,
                new_text,
            }) => {
                assert_eq!(id, "p1");
                assert_eq!(expected_version, 2);
                assert_eq!(new_text.as_deref(), Some("server one EDITED"));
            }
            other => panic!("expected QueueInterjectShared with new_text, got {other:?}"),
        }
        // Shared mirror untouched — the rebroadcast is the source of truth.
        assert_eq!(agent.shared_queue.len(), 1);
        assert_eq!(agent.shared_queue[0].text, "server one");
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// Force-interject on a server row must not emit `QueueReleaseEdit` —
    /// see `exit_editing_mode_keeping_hold`.
    #[test]
    fn edit_interject_server_row_keeps_combine_hold_until_interject() {
        use crate::app::actions::Effect;
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        assert!(
            agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::QueueHoldEdit { .. })),
            "entering edit must emit QueueHoldEdit"
        );

        agent.prompt.set_text("server one EDITED");
        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        assert!(
            matches!(
                outcome,
                InputOutcome::Action(Action::QueueInterjectShared { .. })
            ),
            "interject must route to QueueInterjectShared"
        );
        assert!(
            !agent
                .pending_effects
                .iter()
                .any(|e| matches!(e, Effect::QueueReleaseEdit { .. })),
            "server-row interject must NOT emit QueueReleaseEdit (the handler clears the hold)"
        );
    }

    /// Interject key while editing and IDLE behaves like the bare-Enter save
    /// (row text updated + drain) instead of silently dropping the key.
    #[test]
    fn edit_interject_while_idle_saves_local_edit() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.session.state = AgentState::Idle;
        agent.prompt.set_text("local one EDITED");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::DrainQueue) => {}
            other => panic!("expected DrainQueue, got {other:?}"),
        }
        assert_eq!(agent.session.pending_prompts.len(), 1);
        assert_eq!(agent.session.pending_prompts[0].text, "local one EDITED");
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// Interject key with an empty composer while editing is a no-op — no
    /// empty interjection, and edit mode stays active.
    #[test]
    fn edit_interject_empty_composer_stays_in_edit_mode() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("   ");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        assert!(
            matches!(outcome, InputOutcome::Changed),
            "empty edit-interject must be a no-op, got {outcome:?}"
        );
        assert!(matches!(
            agent.prompt_mode,
            PromptMode::EditingQueued { .. }
        ));
        assert_eq!(agent.session.pending_prompts.len(), 1);
    }

    /// Modal Save with an empty composer keeps the original row text — a
    /// queued prompt must never be blanked by Save.
    #[test]
    fn edit_confirm_save_empty_preserves_original_text() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("");

        // Arm the modal directly (the pane switch no longer arms it) to test empty-Save.
        agent.active_modal = Some(ActiveModal::EditConfirm {
            modal: ModalConfirmation::edit_confirm(),
            pending_target: AgentPane::Scrollback,
        });
        let _ = agent.handle_modal_key(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        assert_eq!(agent.session.pending_prompts[0].text, "local one");
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// Modal Save on a NON-front local row while idle saves the text but must
    /// NOT dispatch `DrainQueue` — the modal advertised plain "save" (not
    /// "save & send"), and draining would start the head prompt's turn.
    #[test]
    fn edit_confirm_save_non_front_row_does_not_drain() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        agent.shared_queue.clear();
        agent.session.enqueue_prompt("local two".to_string());
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            None,
            None,
            &agent.send_now_painted_blocks,
        );
        agent.session.state = AgentState::Idle;

        // Edit the non-front row; arm the modal directly (the pane switch no longer arms it).
        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("local two EDITED");
        agent.active_modal = Some(ActiveModal::EditConfirm {
            modal: ModalConfirmation::edit_confirm(),
            pending_target: AgentPane::Scrollback,
        });

        let outcome =
            agent.handle_modal_key(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(
            !matches!(outcome, InputOutcome::Action(Action::DrainQueue)),
            "plain save must not drain the head prompt, got {outcome:?}"
        );
        assert_eq!(agent.session.pending_prompts.len(), 2);
        assert_eq!(agent.session.pending_prompts[1].text, "local two EDITED");
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// A bash row that vanished mid-edit saves via the kind snapshot, never plain-interjects.
    #[test]
    fn edit_interject_vanished_bash_row_saves_instead_of_plain_interject() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();
        agent.shared_queue[0].kind = "bash".into();
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        assert!(matches!(
            agent.prompt_input_mode,
            crate::app::agent_view::PromptInputMode::Bash
        ));

        agent.shared_queue.clear();
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            agent.session.current_prompt_id.as_deref(),
            agent.expect_send_now_cancel.as_deref(),
            &agent.send_now_painted_blocks,
        );
        agent.prompt_input_mode = crate::app::agent_view::PromptInputMode::Normal;
        agent.prompt.set_text("ls -la EDITED");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::QueueEditShared { id, new_text }) => {
                assert_eq!(id, "p1");
                assert_eq!(new_text, "ls -la EDITED");
            }
            other => panic!("expected QueueEditShared save, got {other:?}"),
        }
        assert!(agent.toast.is_some(), "guard must explain itself");
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// Server row vanished mid-edit (drained / removed by another client):
    /// edit-interject falls back to a plain interjection of the edited text.
    #[test]
    fn edit_interject_server_row_vanished_falls_back_to_plain_interject() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);

        // Row disappears from the broadcast while the user edits.
        agent.shared_queue.clear();
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            None,
            None,
            &agent.send_now_painted_blocks,
        );
        agent.prompt.set_text("server one EDITED");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::Interject { text, .. }) => {
                assert_eq!(text, "server one EDITED");
            }
            other => panic!("expected Interject fallback, got {other:?}"),
        }
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// Interject key while editing a SERVER row and IDLE saves via
    /// `Action::QueueEditShared` (same route as bare-Enter save).
    #[test]
    fn edit_interject_while_idle_saves_server_edit() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.session.state = AgentState::Idle;
        agent.prompt.set_text("server one EDITED");

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::QueueEditShared { id, new_text }) => {
                assert_eq!(id, "p1");
                assert_eq!(new_text, "server one EDITED");
            }
            other => panic!("expected QueueEditShared, got {other:?}"),
        }
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }

    /// Composer images pasted while edit-interjecting a LOCAL row ride
    /// along on the interjection instead of being dropped with a toast.
    #[test]
    fn edit_interject_local_row_carries_composer_images() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("local one EDITED");
        // Insert via the widget so the chip element exists (drain_images
        // reconciles against live elements).
        let len = agent.prompt.textarea().text().len();
        agent.prompt.textarea.set_cursor(len);
        agent
            .prompt
            .insert_image(crate::prompt_images::from_clipboard_data(
                &crate::clipboard::ImageData {
                    data: vec![1, 2, 3],
                    mime_type: "image/png".into(),
                },
            ))
            .unwrap();

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::Interject { images, .. }) => {
                assert_eq!(images.len(), 1);
            }
            other => panic!("expected Interject with images, got {other:?}"),
        }
        assert!(agent.prompt.images.is_empty());
        assert!(agent.toast.is_none(), "no drop toast expected");
    }

    /// Deciding test for the display_number-collision question: edit a LOCAL
    /// row whose stored image is `[Image #1]`,
    /// paste a NEW image mid-edit, interject — BOTH images must survive with
    /// distinct display_numbers (no silent eviction via the dedup merge).
    #[test]
    fn edit_interject_pasted_image_does_not_collide_with_row_image() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        // The local row carries a stored image numbered 1 and its placeholder.
        let mut row_img = crate::prompt_images::from_clipboard_data(&crate::clipboard::ImageData {
            data: vec![9, 9, 9],
            mime_type: "image/png".into(),
        });
        row_img.display_number = 1;
        agent.session.pending_prompts[0].text = "local one [Image #1]".into();
        agent.session.pending_prompts[0].images.push(row_img);
        agent.session.pending_prompts[0]
            .chip_elements
            .push(crate::app::agent::ChipElement {
                range: 10..20,
                kind: crate::views::prompt_widget::KIND_IMAGE,
                display: None,
            });
        agent.queue.sync_from_merged(
            &agent.session.pending_prompts,
            &agent.shared_queue,
            None,
            None,
            &agent.send_now_painted_blocks,
        );

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[1]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);

        // Paste a fresh image while editing.
        let len = agent.prompt.textarea().text().len();
        agent.prompt.textarea.set_cursor(len);
        agent
            .prompt
            .insert_image(crate::prompt_images::from_clipboard_data(
                &crate::clipboard::ImageData {
                    data: vec![1, 2, 3],
                    mime_type: "image/png".into(),
                },
            ))
            .unwrap();

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        match outcome {
            InputOutcome::Action(Action::Interject { images, .. }) => {
                assert_eq!(
                    images.len(),
                    2,
                    "row image + new paste must BOTH survive, got numbers {:?}",
                    images.iter().map(|i| i.display_number).collect::<Vec<_>>()
                );
                let mut numbers: Vec<usize> = images.iter().map(|i| i.display_number).collect();
                numbers.sort_unstable();
                numbers.dedup();
                assert_eq!(numbers.len(), 2, "display_numbers must be distinct");
            }
            other => panic!("expected Interject with images, got {other:?}"),
        }
    }

    /// Composer images on a SERVER-row edit-interject are still dropped with
    /// an accurate toast — the queue wire's `newText` is text-only.
    #[test]
    fn edit_interject_server_row_clears_images_with_toast() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("server one EDITED");
        agent
            .prompt
            .images
            .push(crate::prompt_images::from_clipboard_data(
                &crate::clipboard::ImageData {
                    data: vec![1, 2, 3],
                    mime_type: "image/png".into(),
                },
            ));

        let outcome = agent.handle_prompt_key_for_test(&force_interject_key());
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::QueueInterjectShared { .. })
        ));
        assert!(agent.prompt.images.is_empty());
        let (msg, _) = agent.toast.as_ref().expect("toast shown");
        assert!(msg.contains("can't be attached"), "got toast {msg:?}");
    }

    /// Modal Save with an empty composer on a SERVER row must not emit
    /// `QueueEditShared` (the row text would be blanked agent-side).
    #[test]
    fn edit_confirm_save_empty_server_row_skips_queue_edit() {
        let mut agent = make_running_agent();
        let registry = non_vscode_registry();

        let ids = agent.queue.entry_ids();
        agent.queue.list_state.select_by_id(ids[0]);
        let _ = agent.handle_queue_key(&edit_key(), &registry);
        agent.prompt.set_text("");

        // Arm the modal directly (the pane switch no longer arms it).
        agent.active_modal = Some(ActiveModal::EditConfirm {
            modal: ModalConfirmation::edit_confirm(),
            pending_target: AgentPane::Scrollback,
        });
        let outcome =
            agent.handle_modal_key(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        assert!(
            !matches!(
                outcome,
                InputOutcome::Action(Action::QueueEditShared { .. })
            ),
            "empty Save must not blank the server row, got {outcome:?}"
        );
        assert_eq!(agent.shared_queue[0].text, "server one");
        assert!(matches!(agent.prompt_mode, PromptMode::Normal));
    }
}
