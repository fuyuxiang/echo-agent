//! Shell-side dispatch on [`CompactionMode`]. Split into two methods so the
//! write isn't hidden behind a text-producing name:
//! - [`SessionActor::persist_compaction_segment`] — the write (offload modes).
//! - [`SessionActor::transcript_hint`] — the summary pointer text (no writes).
//!
//! Layering (do NOT collapse): mode decision + hint text in [`CompactionMode`],
//! markdown render in `echo-agent-compaction-transcript`, disk I/O in `StorageAdapter`.
//!
//! To add a mode: add the variant + hint to [`CompactionMode`], extend the match
//! in both methods, and add a `StorageAdapter` writer for any new artifact.
use super::SessionActor;
use crate::extensions::notification::CompactionSegmentFile;
use crate::session::persistence::PersistenceMsg;
use echo_agent_chat_state::CompactionMode;
use echo_agent_chat_state::compaction_utils::format_compact_summary;
use echo_agent_compaction_transcript::COMPACTION_DIR;
use echo_agent_sampling_types::ConversationItem;
impl SessionActor {
    /// Persist the per-segment store (`Segments` only; no-op for `Summary`
    /// and `Transcript`). Queues a write on the persistence channel;
    /// storage assigns the index and renders the markdown.
    ///
    /// Returns `true` iff a `CompactionSegmentFile` was queued. Does not wait on disk.
    pub(crate) fn persist_compaction_segment(
        &self,
        simplified_messages: &[ConversationItem],
        summary: &str,
    ) -> bool {
        let Some(detail) = self.compaction.compaction_mode.segment_detail() else {
            return false;
        };
        let cleaned_summary = format_compact_summary(summary);
        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        self.notifications
            .persistence_tx
            .send(PersistenceMsg::CompactionSegment(CompactionSegmentFile {
                items: simplified_messages.to_vec(),
                summary: cleaned_summary,
                detail,
                timestamp,
            }))
            .is_ok()
    }
    /// Pointer text appended to the summary — where pre-compaction history lives
    /// (`updates.jsonl` for `Transcript`, the `compaction/` store otherwise). No
    /// writes; pair with [`SessionActor::persist_compaction_segment`].
    pub(crate) fn transcript_hint(&self) -> Option<String> {
        let mode = self.compaction.compaction_mode;
        let location = match mode {
            CompactionMode::Summary => None,
            CompactionMode::Transcript => self.get_transcript_path(),
            CompactionMode::Segments(_) => Some(
                crate::session::persistence::session_dir(&self.session_info)
                    .join(COMPACTION_DIR)
                    .to_string_lossy()
                    .into_owned(),
            ),
        };
        mode.transcript_hint(location.as_deref())
    }
}
