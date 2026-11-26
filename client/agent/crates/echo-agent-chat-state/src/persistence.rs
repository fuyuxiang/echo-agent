//! Chat persistence trait and mock implementation.
//!
//! The actor owns persistence exclusively (`Box<dyn ChatPersistence>`), so the
//! trait uses `&mut self` — no locks, no atomics, no shared state.
//! The mock uses a channel to report records to the test, keeping everything
//! in the actor / message-passing paradigm.

use std::io;

use echo_agent_sampling_types::ConversationItem;
use tokio::sync::{mpsc, oneshot};

use crate::commands::{StrictAppendAck, StrictAppendError};

/// Abstraction over chat-specific persistence operations.
///
/// The actor owns this exclusively via `Box<dyn ChatPersistence>`, so all
/// methods take `&mut self` — no interior mutability needed.
///
/// The real implementation wraps an `mpsc::UnboundedSender<PersistenceMsg>`
/// (which only needs `&self` to send, but `&mut self` is still correct
/// because the actor is the sole owner).
pub trait ChatPersistence: Send + 'static {
    /// Persist a single conversation item (append to chat_history.jsonl).
    fn persist_message(&mut self, item: &ConversationItem);

    /// Persist one working-directory switch generation and report commit status.
    fn persist_working_directory_switch_and_ack(
        &mut self,
        item: &ConversationItem,
    ) -> oneshot::Receiver<Result<StrictAppendAck, StrictAppendError>>;

    /// Replace the entire chat history (compaction / rewind).
    fn replace_history(&mut self, items: &[ConversationItem]);

    /// Destructive image-strip rewrite: back up the on-disk history, then
    /// replace it, acking the DISK outcome. A failed backup gates off the
    /// rewrite so recoverability never silently evaporates; backends without
    /// a recoverable store may no-op the backup but must ack the write.
    fn replace_history_for_strip_and_ack(
        &mut self,
        items: &[ConversationItem],
    ) -> oneshot::Receiver<io::Result<()>>;

    /// Flush pending writes to disk.
    fn flush(&mut self);
}

/// Outcome of a conversation image strip, as acknowledged by the actor.
/// Typed so a dead actor can never masquerade as "stripped nothing".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripOutcome {
    /// Stripped and durably persisted; `stripped` counts the stored
    /// occurrences replaced (a URL stored twice counts twice).
    Applied { stripped: usize },
    /// No stored image matched the requested URLs; nothing changed.
    NoMatch,
    /// Stripped in memory, but the backup or disk write failed, or the
    /// acknowledgement was lost mid-flight. Treated as not persisted: the
    /// stored file may still carry the images and the next load re-poisons.
    WriteFailed { stripped: usize },
    /// The chat-state actor is gone; the strip may not have happened at all.
    ActorUnavailable,
}

// ============================================================================
// Mock (test double) — channel-based, no locks, no atomics
// ============================================================================

/// A record of a persistence call, sent over a channel to the test.
#[derive(Debug, Clone)]
pub enum PersistenceRecord {
    /// A single message was persisted.
    Message(ConversationItem),
    /// A persistence-acknowledged switch append was requested.
    AcknowledgedMessage(ConversationItem),
    /// The full history was replaced.
    ReplaceHistory(Vec<ConversationItem>),
    /// A backup-gated, disk-acknowledged strip rewrite was requested.
    ReplaceHistoryForStrip(Vec<ConversationItem>),
    /// A flush was requested.
    Flush,
}

/// Test implementation: sends every call as a [`PersistenceRecord`] over a
/// channel. The test holds the [`MockPersistenceReceiver`] to inspect what
/// the actor did. No locks, no atomics — just message passing.
pub struct MockChatPersistence {
    tx: mpsc::UnboundedSender<PersistenceRecord>,
    /// When set, strip rewrites ack an I/O error instead of success:
    /// pins the honest-failure half of the [`StripOutcome`] contract.
    fail_strip_writes: bool,
    persistence_ack_tx:
        Option<mpsc::UnboundedSender<oneshot::Sender<Result<StrictAppendAck, StrictAppendError>>>>,
    persisted_working_directory_switches: Vec<ConversationItem>,
}

/// Receiver side of the mock. Held by the test to drain and inspect records.
pub struct MockPersistenceReceiver {
    rx: mpsc::UnboundedReceiver<PersistenceRecord>,
    persistence_ack_rx: Option<
        mpsc::UnboundedReceiver<oneshot::Sender<Result<StrictAppendAck, StrictAppendError>>>,
    >,
}

impl MockChatPersistence {
    /// Create a paired (mock, receiver). Give the mock to the actor, keep the
    /// receiver in the test.
    pub fn new() -> (Self, MockPersistenceReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                fail_strip_writes: false,
                persistence_ack_tx: None,
                persisted_working_directory_switches: Vec::new(),
            },
            MockPersistenceReceiver {
                rx,
                persistence_ack_rx: None,
            },
        )
    }

    /// Create a mock whose strip rewrites fail at "disk": the ack carries
    /// an error, so callers must surface `StripOutcome::WriteFailed`.
    pub fn new_failing_strip_writes() -> (Self, MockPersistenceReceiver) {
        let (mut mock, rx) = Self::new();
        mock.fail_strip_writes = true;
        (mock, rx)
    }

    /// Create a mock whose persistence acknowledgement is test-controlled.
    pub fn new_with_manual_persistence_ack() -> (Self, MockPersistenceReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (persistence_ack_tx, persistence_ack_rx) = mpsc::unbounded_channel();
        (
            Self {
                tx,
                fail_strip_writes: false,
                persistence_ack_tx: Some(persistence_ack_tx),
                persisted_working_directory_switches: Vec::new(),
            },
            MockPersistenceReceiver {
                rx,
                persistence_ack_rx: Some(persistence_ack_rx),
            },
        )
    }
}

impl MockPersistenceReceiver {
    /// Drain all pending records from the channel.
    pub fn drain(&mut self) -> Vec<PersistenceRecord> {
        let mut records = Vec::new();
        while let Ok(record) = self.rx.try_recv() {
            records.push(record);
        }
        records
    }

    /// Receive the next manual persistence acknowledgement sender.
    pub async fn next_persistence_ack(
        &mut self,
    ) -> Option<oneshot::Sender<Result<StrictAppendAck, StrictAppendError>>> {
        match &mut self.persistence_ack_rx {
            Some(rx) => rx.recv().await,
            None => None,
        }
    }

    /// Collect all `Message` items received so far (drains the channel).
    pub fn messages(&mut self) -> Vec<ConversationItem> {
        self.drain()
            .into_iter()
            .filter_map(|r| match r {
                PersistenceRecord::Message(item) => Some(item),
                _ => None,
            })
            .collect()
    }
}

impl ChatPersistence for MockChatPersistence {
    fn persist_message(&mut self, item: &ConversationItem) {
        let _ = self.tx.send(PersistenceRecord::Message(item.clone()));
    }

    fn persist_working_directory_switch_and_ack(
        &mut self,
        item: &ConversationItem,
    ) -> oneshot::Receiver<Result<StrictAppendAck, StrictAppendError>> {
        let (reply, receiver) = oneshot::channel();
        let sent = self
            .tx
            .send(PersistenceRecord::AcknowledgedMessage(item.clone()))
            .map_err(|_| {
                StrictAppendError::NotCommitted(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "mock persistence closed",
                ))
            });
        if let Err(error) = sent {
            let _ = reply.send(Err(error));
        } else if let Some(ack_tx) = &self.persistence_ack_tx {
            let _ = ack_tx.send(reply);
        } else {
            let generation = item.working_directory_switch_generation();
            let acknowledgement = self
                .persisted_working_directory_switches
                .iter()
                .find(|persisted| persisted.working_directory_switch_generation() == generation)
                .cloned()
                .map_or(StrictAppendAck::Appended, StrictAppendAck::AlreadyPresent);
            if matches!(&acknowledgement, StrictAppendAck::Appended) {
                self.persisted_working_directory_switches.push(item.clone());
            }
            let _ = reply.send(Ok(acknowledgement));
        }
        receiver
    }

    fn replace_history(&mut self, items: &[ConversationItem]) {
        let _ = self
            .tx
            .send(PersistenceRecord::ReplaceHistory(items.to_vec()));
    }

    fn replace_history_for_strip_and_ack(
        &mut self,
        items: &[ConversationItem],
    ) -> oneshot::Receiver<io::Result<()>> {
        let (reply, receiver) = oneshot::channel();
        let _ = self
            .tx
            .send(PersistenceRecord::ReplaceHistoryForStrip(items.to_vec()));
        let ack = if self.fail_strip_writes {
            Err(io::Error::new(io::ErrorKind::StorageFull, "mock disk full"))
        } else {
            Ok(())
        };
        let _ = reply.send(ack);
        receiver
    }

    fn flush(&mut self) {
        let _ = self.tx.send(PersistenceRecord::Flush);
    }
}

// ============================================================================
// Null (noop) — for benchmarks / scenarios where persistence is unwanted
// ============================================================================

/// No-op implementation: discards everything (for benchmarks / noop scenarios).
pub struct NullChatPersistence;

impl ChatPersistence for NullChatPersistence {
    fn persist_message(&mut self, _item: &ConversationItem) {}
    fn persist_working_directory_switch_and_ack(
        &mut self,
        _item: &ConversationItem,
    ) -> oneshot::Receiver<Result<StrictAppendAck, StrictAppendError>> {
        let (reply, receiver) = oneshot::channel();
        let _ = reply.send(Ok(StrictAppendAck::Appended));
        receiver
    }
    fn replace_history(&mut self, _items: &[ConversationItem]) {}
    fn replace_history_for_strip_and_ack(
        &mut self,
        _items: &[ConversationItem],
    ) -> oneshot::Receiver<io::Result<()>> {
        let (reply, receiver) = oneshot::channel();
        let _ = reply.send(Ok(()));
        receiver
    }
    fn flush(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_persistence_records_messages() {
        let (mut mock, mut rx) = MockChatPersistence::new();
        let item = ConversationItem::system("test");
        mock.persist_message(&item);
        let records = rx.drain();
        assert_eq!(records.len(), 1);
        assert!(matches!(&records[0], PersistenceRecord::Message(_)));
    }

    #[test]
    fn mock_persistence_records_multiple_messages() {
        let (mut mock, mut rx) = MockChatPersistence::new();
        mock.persist_message(&ConversationItem::system("a"));
        mock.persist_message(&ConversationItem::user("b"));
        mock.persist_message(&ConversationItem::assistant("c"));
        assert_eq!(rx.messages().len(), 3);
    }

    #[test]
    fn mock_persistence_records_replace_history() {
        let (mut mock, mut rx) = MockChatPersistence::new();
        mock.replace_history(&[ConversationItem::system("a"), ConversationItem::system("b")]);
        let records = rx.drain();
        assert_eq!(records.len(), 1);
        match &records[0] {
            PersistenceRecord::ReplaceHistory(items) => assert_eq!(items.len(), 2),
            other => panic!("expected ReplaceHistory, got {other:?}"),
        }
    }

    #[test]
    fn mock_persistence_records_flush() {
        let (mut mock, mut rx) = MockChatPersistence::new();
        mock.flush();
        mock.flush();
        let records = rx.drain();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|r| matches!(r, PersistenceRecord::Flush))
        );
    }

    #[tokio::test]
    async fn mock_persistence_deduplicates_working_directory_generation() {
        let (mut mock, _rx) = MockChatPersistence::new();
        let first = ConversationItem::working_directory_switch("authoritative", 4);
        assert!(matches!(
            mock.persist_working_directory_switch_and_ack(&first)
                .await
                .unwrap()
                .unwrap(),
            StrictAppendAck::Appended
        ));
        assert!(matches!(
            mock.persist_working_directory_switch_and_ack(
                &ConversationItem::working_directory_switch("retry", 4),
            )
            .await
            .unwrap()
            .unwrap(),
            StrictAppendAck::AlreadyPresent(item) if item.text_content() == "authoritative"
        ));
    }

    #[test]
    fn null_persistence_does_not_panic() {
        let mut null = NullChatPersistence;
        null.persist_message(&ConversationItem::system("test"));
        null.replace_history(&[ConversationItem::user("a")]);
        null.flush();
    }
}
