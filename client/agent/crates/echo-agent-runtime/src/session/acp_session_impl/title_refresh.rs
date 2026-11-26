//! Legacy, optional title checkpoints for non-desktop clients.
//!
//! The first title comes from the fast first-prompt path
//! ([`crate::session::summary::SummaryGenerator`]); this side-call then
//! requests a title from the whole conversation at
//! [`crate::session::helpers::session_summary::TITLE_REFRESH_TURNS`], then freezes.
//! Desktop sessions disable these calls. Persistence only adopts a result when
//! no title exists, so these compatibility requests never replace an established
//! automatic or manual title.

use super::*;

use super::side_call::AuxCall;
use crate::session::helpers::{session_recap, session_summary};

/// Upper bound on the title-refresh model call so a hung backend cannot hold the
/// one-at-a-time refresh slot indefinitely.
const TITLE_REFRESH_MODEL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

fn strip_expert_context_from_conversation(conversation: &mut [ConversationItem]) {
    for item in conversation {
        let ConversationItem::User(user) = item else {
            continue;
        };
        for part in &mut user.content {
            let ContentPart::Text { text } = part else {
                continue;
            };
            let visible = session_summary::strip_expert_persona_blocks(text);
            *text = Arc::<str>::from(visible);
        }
    }
}

impl SessionActor {
    /// Spawn a title refresh after a successful turn, unless the title is frozen
    /// or one is already running. No-op for subagents and when post-turn
    /// side-calls are disabled (title refresh shares the `turn_summary_enabled`
    /// gate). At most one refresh runs at a time and it is left to finish; the
    /// checkpoint decision and freeze happen in [`Self::refresh_title`].
    pub(crate) fn maybe_refresh_title(self: &Arc<Self>) {
        if !self.title_refresh_enabled || self.startup_hints.is_subagent {
            return;
        }
        if self.next_title_refresh_idx.get() >= session_summary::TITLE_REFRESH_TURNS.len() {
            return;
        }
        // One refresh at a time: a whole-conversation title doesn't need the
        // very latest turn, and letting the in-flight call finish (rather than
        // aborting and respawning every turn) guarantees the checkpoint is
        // eventually consumed even if the model keeps failing. `is_finished`
        // (not just `is_some`) so a panicked task can't wedge the slot shut.
        if self
            .title_refresh_task
            .borrow()
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        let generation = self.title_refresh_generation.get();
        let actor = self.clone();
        let task = tokio::task::spawn_local(async move {
            actor.refresh_title(generation).await;
            if actor.title_refresh_generation.get() == generation {
                *actor.title_refresh_task.borrow_mut() = None;
            }
        });
        *self.title_refresh_task.borrow_mut() = Some(task);
    }

    /// React to a `/rename`. A manual rename freezes the auto refresh (so a
    /// racing in-flight refresh can't flip the title and no later refresh fights
    /// the user's title); `/rename --auto` reopens it so the whole-conversation
    /// refresh can re-title. Aborts any in-flight refresh either way and
    /// persists the new checkpoint so the decision survives resume.
    pub(crate) fn on_title_renamed(&self, manual: bool) {
        self.abort_title_refresh();
        let idx = if manual {
            session_summary::TITLE_REFRESH_TURNS.len()
        } else {
            0
        };
        self.next_title_refresh_idx.set(idx);
        session_summary::save_title_refresh_watermark(
            &crate::session::persistence::session_dir(&self.session_info),
            idx,
        );
    }

    /// Abort an in-flight title refresh. Bumps the generation so a task that
    /// finishes after the abort neither persists its result nor consumes a
    /// checkpoint. Called on rewind (the snapshot is now stale) and shutdown
    /// (free the actor `Arc`); a new prompt deliberately does not abort it.
    pub(crate) fn abort_title_refresh(&self) {
        self.title_refresh_generation
            .set(self.title_refresh_generation.get().wrapping_add(1));
        if let Some(task) = self.title_refresh_task.borrow_mut().take() {
            task.abort();
        }
    }

    /// If the real-user-turn count has reached the next refresh checkpoint,
    /// generate a whole-conversation title and persist it.
    ///
    /// `generation` is the spawn-time token. A completed attempt consumes the
    /// checkpoint (advancing the index, freezing once past the last one) even
    /// when generation *failed*, so a persistently failing model cannot keep
    /// spawning side-calls forever. Only a stale attempt — one whose generation
    /// was bumped by an abort (prompt / rewind / shutdown) — bails without
    /// consuming, so the newer path retries.
    async fn refresh_title(&self, generation: u64) {
        let conversation = self.chat_state_handle.get_conversation().await;
        let turns = session_recap::main_turn_count(&conversation);

        let idx = self.next_title_refresh_idx.get();
        let target_idx = session_summary::checkpoints_reached(turns);
        if target_idx <= idx {
            return;
        }

        let title = self.generate_refreshed_title(conversation).await;

        // No await past here, so an abort can only land before the commit.
        if self.title_refresh_generation.get() != generation {
            return;
        }
        self.next_title_refresh_idx.set(target_idx);
        // Persist the checkpoint (even on a failed attempt) so the freeze is
        // durable and a failing model can't keep re-attempting after resume.
        session_summary::save_title_refresh_watermark(
            &crate::session::persistence::session_dir(&self.session_info),
            target_idx,
        );
        if let Some(title) = title {
            tracing::info!(turns, chars = title.len(), "session title refreshed");
            // The persistence actor only fills an absent title (checked under
            // the summary lock), notifying clients only when the write lands.
            let _ = self
                .notifications
                .persistence_tx
                .send(PersistenceMsg::RegenerateTitle(title));
        }
    }

    /// One tool-free model call producing a cleaned whole-conversation title,
    /// or `None` on any setup/model failure or empty output.
    async fn generate_refreshed_title(
        &self,
        mut conversation: Vec<ConversationItem>,
    ) -> Option<String> {
        // The conversation keeps model-visible expert instructions for replay,
        // but title refresh is user-facing metadata. Remove that transport
        // context from real user text before sending the side call.
        strip_expert_context_from_conversation(&mut conversation);
        let setup = match self.prepare_side_call().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "title refresh: failed to prepare sampling client");
                return None;
            }
        };
        let instruction = session_summary::title_refresh_instruction(self.reminder_wrapper_tag());
        let items = session_recap::budget_instruction_items(
            conversation,
            instruction,
            setup.strip_reasoning,
            setup.context_window,
        );
        // Deliberately tool-free: unlike the turn summary, the request carries
        // no tools, so the model can't spend the call on a tool invocation that
        // would leave empty text and burn the checkpoint. (The conversation
        // prefix still rides the parent prompt-cache key.)
        let request = self.parent_cached_request(AuxCall {
            items,
            tools: Vec::new(),
            hosted_tools: Vec::new(),
            model: setup.model.clone(),
            reasoning_effort: setup.reasoning_effort,
            backend: setup.client.api_backend(),
            conv_id: format!("title-refresh-{}", uuid::Uuid::new_v4()),
            req_id: format!("echo-agent-title-refresh-{}", uuid::Uuid::new_v4()),
        });

        let response = match tokio::time::timeout(
            TITLE_REFRESH_MODEL_TIMEOUT,
            setup.client.conversation_collect(request),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "title refresh: model call failed");
                return None;
            }
            Err(_) => {
                tracing::warn!(
                    timeout_secs = TITLE_REFRESH_MODEL_TIMEOUT.as_secs(),
                    "title refresh: model call timed out"
                );
                return None;
            }
        };
        super::side_call::log_prompt_cache_usage(
            "title_refresh",
            setup.client.api_backend(),
            &response,
        );
        let title = session_summary::clean_title_text(&response.assistant_text());
        if title.is_empty() {
            tracing::debug!("title refresh: model returned empty title");
            return None;
        }
        Some(title)
    }
}

#[cfg(test)]
mod expert_context_tests {
    use super::*;

    #[test]
    fn title_refresh_conversation_uses_only_visible_user_text() {
        let mut conversation = vec![ConversationItem::user(
            "<!--EXPERT_PERSONA_BEGIN-->\nexpert\n<!--EXPERT_PERSONA_END-->\n\nuser task",
        )];
        strip_expert_context_from_conversation(&mut conversation);
        assert_eq!(conversation[0].text_content(), "user task");
    }
}
