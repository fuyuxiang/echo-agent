//! Per-turn dashboard summary lifecycle on `SessionActor`.
//!
//! Pure prompt helpers live in [`crate::session::helpers::turn_summary`].
//! Shared sampling setup is in [`super::side_call`].

use super::*;

impl SessionActor {
    /// (Re)start the per-turn dashboard summary side-call for the turn
    /// `prompt_id` that just completed successfully, aborting any in-flight
    /// generation (its result would describe an older turn).
    ///
    /// Abort is safe mid-call: the persist + broadcast commit block in
    /// `generate_turn_summary` has no await points, so cancellation can only
    /// land before it, never inside it. Generation is also checked again
    /// immediately before commit so a task that finishes after abort cannot
    /// write a stale summary.
    pub(crate) fn restart_turn_summary(self: &Arc<Self>, prompt_id: String) {
        if !self.turn_summary_enabled || self.startup_hints.is_subagent {
            return;
        }
        // A queued follow-up promoted by `maybe_start_running_task` is
        // already running when this fires from the completion arm; a snapshot
        // taken now would contain that turn's user message. Bail — the
        // running turn's own completion re-fires.
        if self
            .current_prompt_id
            .lock()
            .expect("current_prompt_id mutex poisoned")
            .is_some()
        {
            return;
        }
        self.abort_turn_summary();
        let generation = self.turn_summary_generation.get().wrapping_add(1);
        self.turn_summary_generation.set(generation);
        let actor = self.clone();
        let task = tokio::task::spawn_local(async move {
            actor.generate_turn_summary(&prompt_id, generation).await;
            // Drop the slot only if we are still the registered task. An
            // abort-and-respawn can replace the handle before we finish.
            if actor.turn_summary_generation.get() == generation {
                *actor.turn_summary_task.borrow_mut() = None;
            }
        });
        *self.turn_summary_task.borrow_mut() = Some(task);
    }

    /// Abort an in-flight turn-summary generation. Callers: real prompt
    /// accept ([`Self::invalidate_side_calls_for_new_prompt`]), conversation
    /// rewind, and session shutdown. Not cancel: an in-flight summary
    /// describes a prior successful turn and should finish under
    /// show-until-replaced.
    pub(crate) fn abort_turn_summary(&self) {
        // Invalidate so a finishing aborted task cannot clear a later spawn
        // or pass the pre-commit generation gate.
        self.turn_summary_generation
            .set(self.turn_summary_generation.get().wrapping_add(1));
        if let Some(task) = self.turn_summary_task.borrow_mut().take() {
            task.abort();
        }
    }

    /// The turn-summary side-call body: snapshot, one tool-free model call,
    /// then persist to `summary.json` + broadcast transiently to attached
    /// clients. Display-only and best-effort — failures log and drop, the
    /// turn is already over. `generation` is the spawn-time token; if it no
    /// longer matches at commit time, this result is stale and is dropped.
    async fn generate_turn_summary(&self, prompt_id: &str, generation: u64) {
        use crate::session::helpers::turn_summary;

        let conversation = self.chat_state_handle.get_conversation().await;
        let Some(anchor) = turn_summary::last_user_anchor(&conversation) else {
            return;
        };

        let setup = match self.prepare_side_call().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "turn summary: failed to prepare sampling client");
                return;
            }
        };
        let instruction =
            turn_summary::turn_summary_instruction(self.reminder_wrapper_tag(), &anchor);
        let items = crate::session::helpers::session_recap::budget_instruction_items(
            conversation,
            instruction,
            setup.strip_reasoning,
            setup.context_window,
        );
        let request = self
            .side_call_request(
                &setup,
                items,
                format!("turn-summary-{}", uuid::Uuid::new_v4()),
                format!("echo-agent-turn-summary-{}", uuid::Uuid::new_v4()),
            )
            .await;

        let response = match setup.client.conversation_collect(request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "turn summary: model call failed");
                return;
            }
        };
        super::side_call::log_prompt_cache_usage(
            "turn_summary",
            setup.client.api_backend(),
            &response,
        );
        let summary = turn_summary::clean_turn_summary_text(&response.assistant_text());
        if summary.is_empty() {
            tracing::debug!("turn summary: model returned empty summary");
            return;
        }

        // Stale after abort / newer spawn: do not persist or broadcast.
        if self.turn_summary_generation.get() != generation {
            tracing::debug!("turn summary: discarded stale generation");
            return;
        }

        // Commit block: no await between here and the end, so an abort can
        // never leave the persisted and broadcast copies disagreeing.
        tracing::info!(chars = summary.len(), "turn summary generated");
        let _ = self
            .notifications
            .persistence_tx
            .send(PersistenceMsg::LastTurnSummary(Some((
                summary.clone(),
                prompt_id.to_string(),
            ))));
        self.send_echo_agent_notification_transient(
            crate::extensions::notification::SessionUpdate::LastTurnSummary {
                summary,
                prompt_id: Some(prompt_id.to_string()),
            },
        );
    }
}
