//! `NotificationSender` — transport layer for session notifications.
//!
//! Owns the gateway handle, gateway-enabled gate, and persistence
//! channel needed to emit notifications.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::{mpsc, watch};

use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;

use crate::session::persistence::PersistenceMsg;

/// Transport layer for delivering session notifications to the client
/// and persistence layer.
pub(crate) struct NotificationSender {
    /// Gateway handle for forwarding notifications to the client.
    pub gateway: GatewaySender,
    /// When false, notifications are persisted but NOT forwarded to the
    /// client. Opened by `MvpAgent::load_session` when the client
    /// explicitly loads the session.
    pub gateway_enabled: Arc<AtomicBool>,
    /// Persistence channel for writing updates to disk.
    pub persistence_tx: mpsc::UnboundedSender<PersistenceMsg>,
    pub disk_full: watch::Receiver<bool>,
}

impl NotificationSender {
    pub(crate) fn is_disk_full(&self) -> bool {
        *self.disk_full.borrow()
    }
}

#[cfg(test)]
pub(crate) fn idle_disk_full_rx() -> watch::Receiver<bool> {
    watch::channel(false).1
}
