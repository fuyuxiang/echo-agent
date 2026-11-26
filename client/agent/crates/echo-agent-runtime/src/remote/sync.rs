//! Writeback push: async queue that flushes session updates to the backend.
//!
//! `RemoteSync` runs a background tokio task that buffers ACP notifications
//! and flushes them to the backend via [`BackendClient::save_session_data()`].
//!
//! ## Backpressure
//!
//! When the buffer exceeds [`MAX_PENDING`], the task attempts an emergency
//! flush. If that also fails (network down), the oldest messages are dropped
//! to prevent unbounded memory growth.
//!
//! ## Drop behavior
//!
//! When `RemoteSync` is dropped, the sender half of the channel closes and
//! the background task exits. **Pending buffered messages are lost.** This
//! is acceptable because the local JSONL files are the source of truth —
//! writeback is best-effort.

use crate::remote::BackendClient;
use crate::session::export::{ExportedMessage, ExportedMetadata};
use agent_client_protocol as acp;
use echo_agent_telemetry::id::agent_id;
use tokio::sync::mpsc;

/// Max buffered notifications before triggering an emergency flush.
/// Sized to keep memory under ~50MB even with large notifications.
const MAX_PENDING: usize = 512;

/// How many oldest messages to drop when an emergency flush fails.
/// Dropping a batch (not one-by-one) avoids repeated failed flushes.
const DROP_BATCH_SIZE: usize = 64;

enum SyncMsg {
    Queue(Box<acp::SessionNotification>),
    Flush,
    SetTitle {
        title: String,
        is_manual: bool,
    },
    /// Drop cached title + manual flag so later flushes cannot re-advertise
    /// a pin the local summary no longer has.
    ClearTitle,
    SetModelId(String),
}

#[derive(Clone)]
pub struct RemoteSync {
    tx: mpsc::UnboundedSender<SyncMsg>,
}

impl RemoteSync {
    #[cfg(test)]
    pub(crate) fn test_observer() -> (Self, mpsc::UnboundedReceiver<acp::SessionNotification>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (observed_tx, observed_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if let SyncMsg::Queue(notification) = message {
                    let _ = observed_tx.send(*notification);
                }
            }
        });
        (Self { tx }, observed_rx)
    }

    /// Metadata is included on every flush to keep the backend session row current.
    pub(crate) fn new(
        session_id: String,
        metadata: ExportedMetadata,
        client: BackendClient,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(sync_task(session_id, metadata, client, rx));
        Self { tx }
    }

    pub fn queue(&self, notification: acp::SessionNotification) {
        let _ = self.tx.send(SyncMsg::Queue(Box::new(notification)));
    }

    pub fn flush(&self) {
        let _ = self.tx.send(SyncMsg::Flush);
    }

    pub fn set_title(&self, title: String) {
        let _ = self.tx.send(SyncMsg::SetTitle {
            title,
            is_manual: false,
        });
    }

    pub fn set_manual_title(&self, title: String) {
        let _ = self.tx.send(SyncMsg::SetTitle {
            title,
            is_manual: true,
        });
    }

    pub fn clear_title(&self) {
        let _ = self.tx.send(SyncMsg::ClearTitle);
    }

    pub(crate) fn set_model_id(&self, model_id: String) {
        let _ = self.tx.send(SyncMsg::SetModelId(model_id));
    }
}

async fn do_flush(
    client: &BackendClient,
    session_id: &str,
    metadata: &ExportedMetadata,
    pending: &mut Vec<acp::SessionNotification>,
) -> bool {
    if pending.is_empty() {
        return true;
    }

    let messages: Vec<ExportedMessage> = pending
        .iter()
        .map(ExportedMessage::from_notification)
        .collect();

    match client
        .save_session_data(session_id, &messages, Some(metadata))
        .await
    {
        Ok(()) => {
            tracing::debug!(count = pending.len(), "Writeback: synced");
            pending.clear();

            // Link session to agent so the relay can route requests to it.
            if let Err(e) = client
                .upsert_session(session_id, metadata, &agent_id())
                .await
            {
                tracing::warn!(error = %e, "Writeback: failed to upsert session");
            }

            true
        }
        Err(e) => {
            tracing::warn!(error = %e, pending = pending.len(), "Writeback: flush failed");
            false
        }
    }
}

async fn sync_task(
    session_id: String,
    mut metadata: ExportedMetadata,
    client: BackendClient,
    mut rx: mpsc::UnboundedReceiver<SyncMsg>,
) {
    let mut pending: Vec<acp::SessionNotification> = Vec::new();

    while let Some(msg) = rx.recv().await {
        match msg {
            SyncMsg::Queue(n) => {
                if pending.len() >= MAX_PENDING {
                    tracing::warn!(
                        pending = pending.len(),
                        "Writeback: buffer full, attempting emergency flush"
                    );

                    metadata.updated_at = Some(chrono::Utc::now().to_rfc3339());
                    if !do_flush(&client, &session_id, &metadata, &mut pending).await {
                        let dropped = pending.drain(0..DROP_BATCH_SIZE.min(pending.len())).count();
                        tracing::error!(
                            dropped = dropped,
                            "Writeback: emergency flush failed, dropping oldest messages"
                        );
                    }
                }
                pending.push(*n);
            }
            SyncMsg::Flush => {
                metadata.updated_at = Some(chrono::Utc::now().to_rfc3339());
                do_flush(&client, &session_id, &metadata, &mut pending).await;
            }
            SyncMsg::SetTitle { title, is_manual } => {
                metadata.title = Some(title);
                metadata.title_is_manual = is_manual.then_some(true);
                metadata.updated_at = Some(chrono::Utc::now().to_rfc3339());
                if let Err(e) = client
                    .save_session_data(&session_id, &[], Some(&metadata))
                    .await
                {
                    tracing::warn!(?e, "Writeback: failed to sync title to backend");
                } else if let Err(e) = client
                    .upsert_session(&session_id, &metadata, &agent_id())
                    .await
                {
                    // save_session_data does not write the session-row title
                    // (backend upsert title=None). Without this, list/--resume
                    // keep the pre-rename row until the next message flush.
                    tracing::warn!(error = %e, "Writeback: failed to upsert session title");
                }
            }
            SyncMsg::ClearTitle => {
                // Empty string, not `None`: an omitted field leaves the
                // backend's prior pin in place.
                metadata.title = Some(String::new());
                metadata.title_is_manual = Some(false);
                metadata.updated_at = Some(chrono::Utc::now().to_rfc3339());
                if let Err(e) = client
                    .save_session_data(&session_id, &[], Some(&metadata))
                    .await
                {
                    tracing::warn!(?e, "Writeback: failed to clear title on backend");
                } else if let Err(e) = client
                    .upsert_session(&session_id, &metadata, &agent_id())
                    .await
                {
                    // Same row-title gap as SetTitle: save_session_data does
                    // not clear the session-row pin (backend title=None).
                    tracing::warn!(error = %e, "Writeback: failed to upsert cleared session title");
                }
            }
            SyncMsg::SetModelId(id) => {
                metadata.model_id = Some(id);
                metadata.updated_at = Some(chrono::Utc::now().to_rfc3339());
                if let Err(e) = client
                    .save_session_data(&session_id, &[], Some(&metadata))
                    .await
                {
                    tracing::warn!(?e, "Writeback: failed to sync model_id to backend");
                }
            }
        }
    }
}
