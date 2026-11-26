//! Shell policy for sampler image strips: which strips may rewrite stored
//! history, when, and what the user is told.
//!
//! - Only `ServerRejected` with unambiguous blame (exactly one unique URL
//!   in the rejected request) may touch history; the server's verdict
//!   names the request, not an image.
//! - The rewrite is deferred until that request's `Completed` proves the
//!   strip helped; `Failed` drops the buffer.
//! - The write is backup-gated and disk-acknowledged ([`StripOutcome`]);
//!   only `Applied` claims the stored conversation changed.
//! - Scope: `chat_history.jsonl` only. A rebuild replaying `updates.jsonl`
//!   (e.g. a remote pull) restores the image and pays one more strip cycle.

use echo_agent_chat_state::StripOutcome;
use echo_agent_sampler::{RequestId, StripReason};

use crate::extensions::notification::SessionUpdate as EchoAgentSessionUpdate;
use crate::session::acp_session::SessionActor;

impl SessionActor {
    /// Handle `SamplingEvent::ImagesStripped`: buffer a persistable strip
    /// for [`Self::apply_pending_image_strip`], or notify immediately for a
    /// request-local one.
    pub(crate) async fn handle_images_stripped(
        &self,
        request_id: RequestId,
        stripped_urls: Vec<std::sync::Arc<str>>,
        reason: StripReason,
    ) {
        let stripped = stripped_urls.len();
        // Blame is judged on unique URLs: the same image attached twice is
        // still one suspect. Distinct images are ambiguous: request-local.
        let mut unique = stripped_urls;
        unique.sort();
        unique.dedup();
        let persist_deferred = reason == StripReason::ServerRejected && unique.len() == 1;
        if persist_deferred {
            *self.pending_image_strip.lock() = Some((request_id.clone(), unique));
        }
        echo_agent_telemetry::unified_log::warn(
            "shell.turn.images_stripped",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "sampler_request_id": request_id.as_str(),
                "stripped": stripped,
                "reason": reason.as_str(),
                "persist_deferred": persist_deferred,
            })),
        );
        if !persist_deferred {
            // Request-local only: tell the user now, on the same channel as
            // load-time image drops, rendered as a system scrollback note.
            self.send_echo_agent_notification(EchoAgentSessionUpdate::ImageDropped {
                notes: vec![format!(
                    "This request failed over its images (or was too large); \
                     {stripped} image(s) were left out of the retry."
                )],
            })
            .await;
        }
    }

    /// On `Completed`: the stripped retry succeeded, so the buffered strip
    /// is now blamed with evidence: persist it and tell the user once the
    /// disk write is acknowledged.
    pub(crate) async fn apply_pending_image_strip(&self, request_id: &RequestId) {
        let urls = {
            let mut pending = self.pending_image_strip.lock();
            match pending.take() {
                Some((rid, urls)) if &rid == request_id => Some(urls),
                other => {
                    *pending = other;
                    None
                }
            }
        };
        let Some(urls) = urls else { return };
        let outcome = self.chat_state_handle.strip_conversation_images(urls).await;
        let (outcome_label, persisted) = match outcome {
            StripOutcome::Applied { stripped } => ("applied", stripped),
            StripOutcome::NoMatch => ("no_match", 0),
            StripOutcome::WriteFailed { .. } => ("write_failed", 0),
            StripOutcome::ActorUnavailable => ("actor_unavailable", 0),
        };
        echo_agent_telemetry::unified_log::warn(
            "shell.turn.images_strip_persisted",
            Some(self.session_info.id.0.as_ref()),
            Some(serde_json::json!({
                "sampler_request_id": request_id.as_str(),
                "outcome": outcome_label,
                "persisted": persisted,
            })),
        );
        // Every outcome answered without the user's image, so every outcome
        // says so; only `Applied` may also claim the stored conversation
        // changed (a failed or missed write leaves the image on disk).
        let notes = match outcome {
            StripOutcome::Applied { .. } => vec![
                "The server could not process an image, so it was removed from \
                 the conversation. Re-attach it if it is still needed."
                    .to_string(),
            ],
            StripOutcome::NoMatch
            | StripOutcome::WriteFailed { .. }
            | StripOutcome::ActorUnavailable => vec![
                "The server could not process an image, so it was left out of \
                 this request."
                    .to_string(),
            ],
        };
        self.send_echo_agent_notification(EchoAgentSessionUpdate::ImageDropped { notes })
            .await;
    }

    /// On `Failed`: the stripped retry did not rescue the turn, so the
    /// buffered strip proves nothing, so drop it. Stored history keeps its
    /// images; the next turn starts fresh.
    pub(crate) fn drop_pending_image_strip(&self, request_id: &RequestId) {
        let mut pending = self.pending_image_strip.lock();
        if pending.as_ref().is_some_and(|(rid, _)| rid == request_id) {
            tracing::debug!(
                sampler_request_id = request_id.as_str(),
                "dropping buffered image strip: the stripped retry did not complete"
            );
            *pending = None;
        }
    }
}
