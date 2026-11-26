//! WebSocket relay sync for real-time session sharing.
//!
//! Features:
//! - Connection state machine with Disconnected, Connecting, Connected states
//! - Disk-based sync cursor for offline resilience
//! - Status callbacks for TUI indicators
//! - Graceful degradation when relay is unavailable
//!
//! Reconnection is handled by `run_relay_loop` in the relay module.

use crate::agent::relay::{RelayConfig, spawn_relay_connection};
use crate::relay::types::AgentType;
use crate::tprintln;
use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Maximum pending notifications before dropping oldest.
const MAX_PENDING: usize = 256;
/// Number of messages to drop when buffer is full.
const DROP_BATCH_SIZE: usize = 64;

/// Build the share URL for a session.
/// Format: https://cloud.echo-agent.invalid/build/{sessionId}
pub(crate) fn build_share_url(session_id: &str) -> String {
    let base_url = std::env::var("ECHO_AGENT_CODE_WEB_URL")
        .unwrap_or_else(|_| "https://cloud.echo-agent.invalid".to_string());
    format!("{}/build/{}", base_url, session_id)
}

/// Connection state for the relay sync.
///
/// Note: reconnection is handled internally by `run_relay_loop` in relay.rs.
/// The sync task only observes Disconnected → Connecting → Connected transitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// Not connected to relay.
    Disconnected,
    /// Attempting connection (or reconnecting internally).
    Connecting,
    /// Successfully connected and handshake completed.
    Connected,
}

impl ConnectionState {
    /// Returns true if currently connected.
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionState::Connected)
    }

    /// Returns the status indicator for TUI display.
    pub fn status_indicator(&self) -> &'static str {
        match self {
            ConnectionState::Disconnected => "📡 ✗",
            ConnectionState::Connecting => "📡 ...",
            ConnectionState::Connected => "📡",
        }
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionState::Disconnected => write!(f, "disconnected"),
            ConnectionState::Connecting => write!(f, "connecting"),
            ConnectionState::Connected => write!(f, "connected"),
        }
    }
}

/// Sync state persisted to disk for offline resilience.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RelaySyncState {
    /// Event ID of the last successfully synced update.
    #[serde(default)]
    pub last_synced_event_id: Option<String>,
    /// Timestamp of the last successful sync (Unix epoch seconds).
    #[serde(default)]
    pub last_synced_at: Option<u64>,
    /// Session ID on the relay side (may differ from local).
    #[serde(default)]
    pub relay_session_id: Option<String>,
    /// Number of events successfully synced.
    #[serde(default)]
    pub synced_count: u64,
}

/// Status of relay sync for a session.
///
/// This status is based solely on the relay sync state file (`relay_sync.json`),
/// not on comparing against `updates.jsonl` line counts, since those two numbers
/// measure different things and can diverge (e.g., historical sessions created
/// before relay sync was enabled, filtered event types, etc.).
#[derive(Debug, Clone)]
pub struct SyncStatus {
    /// Whether relay sync state file exists (i.e., relay sync was enabled for this session).
    pub has_sync_state: bool,
    /// Number of events successfully queued to the relay channel.
    pub synced_count: u64,
    /// Event ID of the last successfully synced update.
    pub last_synced_event_id: Option<String>,
    /// Timestamp of last successful sync (Unix epoch seconds).
    pub last_synced_at: Option<u64>,
}

impl RelaySyncState {
    /// Load sync state from disk.
    pub fn load(session_dir: &std::path::Path) -> Self {
        let path = Self::state_path(session_dir);
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save sync state to disk atomically.
    ///
    /// Writes to a temporary file then renames to avoid corruption on crash.
    /// Creates the session directory if it doesn't exist.
    pub fn save(&self, session_dir: &std::path::Path) -> std::io::Result<()> {
        crate::util::echo_agent_home::create_dir_all_owner_only(session_dir)?;

        let path = Self::state_path(session_dir);
        let tmp_path = path.with_extension("json.tmp");
        let content = serde_json::to_string(self)?;
        std::fs::write(&tmp_path, content)?;
        std::fs::rename(tmp_path, path)
    }

    /// Get the path to the sync state file.
    fn state_path(session_dir: &std::path::Path) -> PathBuf {
        session_dir.join("relay_sync.json")
    }

    /// Check if sync state file exists for a session.
    pub fn exists(session_dir: &std::path::Path) -> bool {
        Self::state_path(session_dir).exists()
    }

    /// Update the cursor after a successful sync.
    pub(crate) fn update_cursor(&mut self, event_id: String) {
        self.last_synced_event_id = Some(event_id);
        self.last_synced_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        );
        self.synced_count += 1;
    }

    /// Get the sync status for a session based on its relay sync state file.
    ///
    /// This reads only the `relay_sync.json` file and does **not** compare against
    /// `updates.jsonl` line counts, since `synced_count` and line count measure
    /// different things (relay-queued events vs. all session updates) and can diverge
    /// for sessions created before relay sync was enabled.
    ///
    /// # Arguments
    /// * `session_dir` - Path to the session directory
    ///
    /// # Returns
    /// A `SyncStatus` struct with sync statistics.
    pub fn get_sync_status(session_dir: &std::path::Path) -> SyncStatus {
        let has_sync_state = Self::exists(session_dir);
        let sync_state = Self::load(session_dir);

        SyncStatus {
            has_sync_state,
            synced_count: sync_state.synced_count,
            last_synced_event_id: sync_state.last_synced_event_id,
            last_synced_at: sync_state.last_synced_at,
        }
    }
}

/// Callback for connection state changes (TUI status bar).
pub type StatusCallback = Arc<dyn Fn(ConnectionState) + Send + Sync + 'static>;

/// Messages for the relay sync task.
enum RelaySyncMsg {
    /// Queue a notification to be sent to relay.
    Queue(Box<acp::SessionNotification>),
    /// Flush all pending notifications immediately.
    Flush,
    /// Shutdown the relay sync.
    Shutdown,
}

/// Syncs session updates to the relay via WebSocket.
///
/// Provides a non-blocking API for queuing notifications. WebSocket communication
/// happens in a background task, ensuring the main session loop is never blocked.
///
/// Reconnection is handled by `run_relay_loop` in relay.rs. This struct only
/// manages the queue/flush lifecycle and connection state observation.
///
/// # Features
/// - Disk-based sync cursor for offline resilience
/// - Connection state observation via [`Self::connection_state`]
/// - Backpressure with configurable buffer limits
pub struct RelaySync {
    /// Channel to send messages to the sync task.
    tx: mpsc::UnboundedSender<RelaySyncMsg>,
    /// Session ID being synced.
    session_id: String,
    agent_type: AgentType,
    /// Current connection state (observable).
    connection_state_rx: watch::Receiver<ConnectionState>,
    /// Cancellation token to stop the sync task.
    cancel: CancellationToken,
    /// Number of pending events waiting to sync (for status display).
    pending_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl RelaySync {
    /// Create a new relay sync instance.
    ///
    /// Returns a `RelaySync` that queues notifications and syncs them to the relay
    /// in a background task. Connection state can be observed via `connection_state()`.
    ///
    /// # Arguments
    /// * `session_id` - The session ID to sync
    /// * `config` - Relay connection configuration
    /// * `agent_type` - Type of agent (TUI or headless)
    /// * `session_dir` - Directory for persisting sync state
    /// * `status_cb` - Optional callback for connection state changes
    pub fn new(
        session_id: String,
        config: RelayConfig,
        agent_type: AgentType,
        session_dir: Option<PathBuf>,
        status_cb: Option<StatusCallback>,
    ) -> RelaySync {
        let (tx, rx) = mpsc::unbounded_channel();
        let (state_tx, state_rx) = watch::channel(ConnectionState::Disconnected);
        let cancel = CancellationToken::new();
        let pending_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let session_id_task = session_id.clone();
        let cancel_task = cancel.clone();
        let pending_count_task = pending_count.clone();

        tokio::spawn(relay_sync_task(
            RelaySyncTaskConfig {
                session_id: session_id_task,
                config,
                agent_type,
                session_dir,
                status_cb,
            },
            rx,
            state_tx,
            cancel_task,
            pending_count_task,
        ));

        RelaySync {
            tx,
            session_id,
            agent_type,
            connection_state_rx: state_rx,
            cancel,
            pending_count,
        }
    }

    /// Queue a notification to be sent to relay.
    /// Skips replay notifications to prevent loops.
    pub fn queue(&self, notification: acp::SessionNotification) {
        let is_replay = notification
            .meta
            .as_ref()
            .and_then(|m| m.get("isReplay"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_replay {
            return;
        }

        self.pending_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if let Err(e) = self.tx.send(RelaySyncMsg::Queue(Box::new(notification))) {
            tracing::error!(error=%e, "failed to send relay sync msg");
            self.pending_count
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Flush all pending notifications immediately.
    pub fn flush(&self) {
        let _ = self.tx.send(RelaySyncMsg::Flush);
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Get the agent type.
    pub fn agent_type(&self) -> AgentType {
        self.agent_type
    }

    /// Get the current connection state.
    pub(crate) fn connection_state(&self) -> ConnectionState {
        *self.connection_state_rx.borrow()
    }

    /// Check if currently connected to relay.
    pub fn is_connected(&self) -> bool {
        self.connection_state().is_connected()
    }

    /// Get the number of pending events waiting to sync.
    pub fn pending_count(&self) -> usize {
        self.pending_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Subscribe to connection state changes.
    pub(crate) fn subscribe_state(&self) -> watch::Receiver<ConnectionState> {
        self.connection_state_rx.clone()
    }
}

impl Drop for RelaySync {
    fn drop(&mut self) {
        // Signal shutdown
        let _ = self.tx.send(RelaySyncMsg::Shutdown);
        self.cancel.cancel();
    }
}

/// Configuration for the relay sync background task.
struct RelaySyncTaskConfig {
    session_id: String,
    config: RelayConfig,
    agent_type: AgentType,
    session_dir: Option<PathBuf>,
    status_cb: Option<StatusCallback>,
}

/// Main relay sync task.
///
/// Reconnection is handled by `run_relay_loop` in relay.rs, so this task
/// is a single flat loop that processes local queue/flush messages and
/// incoming messages from the relay. The `Connected` state is only set
/// after a successful initialize handshake with the relay.
async fn relay_sync_task(
    cfg: RelaySyncTaskConfig,
    mut rx: mpsc::UnboundedReceiver<RelaySyncMsg>,
    state_tx: watch::Sender<ConnectionState>,
    cancel: CancellationToken,
    pending_count: Arc<std::sync::atomic::AtomicUsize>,
) {
    let session_id = cfg.session_id;
    let mut pending: Vec<acp::SessionNotification> = Vec::new();

    // Load sync state for offline resilience
    let mut sync_state = cfg
        .session_dir
        .as_ref()
        .map(|dir| RelaySyncState::load(dir))
        .unwrap_or_default();

    // Helper to update state and notify callback
    let update_state = |state: ConnectionState| {
        let _ = state_tx.send(state);
        if let Some(cb) = &cfg.status_cb {
            cb(state);
        }
    };

    // Set initial state to Connecting
    update_state(ConnectionState::Connecting);

    // Create channels for WebSocket communication
    let (from_relay_tx, mut from_relay_rx) = mpsc::unbounded_channel::<String>();

    // Spawn the relay connection (reconnection is handled internally by run_relay_loop)
    let (to_relay_tx, _relay_handle) =
        spawn_relay_connection(cfg.config, from_relay_tx, cancel.clone());

    // Track if we've completed initialization.
    // Flush is deferred until the initialize handshake completes (see gate below).
    let mut initialized = false;

    // Main message loop — flat, no outer reconnection loop.
    // Uses random polling (no `biased;`); the `initialized` gate prevents
    // premature flushes regardless of which branch fires first.
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!(session_id = %session_id, "RelaySync: cancel received");
                break;
            }

            // Handle messages from relay (initialize handshake, confirmations, etc.)
            msg = from_relay_rx.recv() => {
                match msg {
                    Some(relay_msg) => {
                        if let Err(e) = handle_relay_message(
                            &session_id,
                            &to_relay_tx,
                            &relay_msg,
                            &mut initialized,
                            cfg.agent_type,
                            &update_state,
                        ) {
                            tracing::warn!(error = %e, "RelaySync: error handling relay message");
                        }
                    }
                    None => {
                        tracing::debug!(session_id = %session_id, "RelaySync: relay channel closed");
                        break;
                    }
                }
            }

            // Handle messages from the local session (queue/flush)
            msg = rx.recv() => {
                match msg {
                    Some(RelaySyncMsg::Queue(notification)) => {
                        // Backpressure: drop oldest if buffer full
                        if pending.len() >= MAX_PENDING {
                            let dropped = pending.drain(0..DROP_BATCH_SIZE).count();
                            pending_count.fetch_sub(dropped, std::sync::atomic::Ordering::Relaxed);
                            tracing::warn!(
                                session_id = %session_id,
                                dropped,
                                "RelaySync: buffer full, dropping oldest"
                            );
                        }
                        pending.push(*notification);
                    }
                    Some(RelaySyncMsg::Flush) => {
                        // Don't flush until the initialize handshake is complete;
                        // the relay may reject or drop messages before then.
                        if !initialized {
                            tracing::debug!(
                                session_id = %session_id,
                                pending = pending.len(),
                                "RelaySync: flush deferred, waiting for initialize handshake"
                            );
                            continue;
                        }

                        // Send pending notifications, preserving unsent items on failure.
                        // We iterate by index so that on send failure the remaining
                        // items stay in `pending` instead of being consumed by drain.
                        let mut sent_count = 0;
                        while sent_count < pending.len() {
                            let notification = &pending[sent_count];
                            // Generate event ID for sync tracking
                            // Use consistent {sessionId}-{counter} format to match agent event IDs
                            let event_id = resolve_event_id(notification);

                            let json_rpc = json!({
                                "jsonrpc": "2.0",
                                "method": "session/update",
                                "params": {
                                    "sessionId": notification.session_id,
                                    "update": notification.update,
                                    "_meta": {
                                        "eventId": event_id,
                                    }
                                }
                            });

                            if to_relay_tx.send(json_rpc.to_string()).is_ok() {
                                sent_count += 1;
                                // Note: cursor tracks channel enqueue, not delivery.
                                // Events may be lost if the connection drops between
                                // enqueue and socket write.
                                sync_state.update_cursor(event_id);
                            } else {
                                tracing::debug!(
                                    session_id = %session_id,
                                    unsent = pending.len() - sent_count,
                                    "RelaySync: send failed, retaining unsent items"
                                );
                                break;
                            }
                        }

                        // Remove only the successfully sent items
                        if sent_count > 0 {
                            pending.drain(0..sent_count);
                        }

                        // Update pending count
                        pending_count.fetch_sub(sent_count, std::sync::atomic::Ordering::Relaxed);

                        // Persist sync state synchronously to guarantee write ordering.
                        // The file is small (~100 bytes) and flushes are infrequent.
                        if sent_count > 0
                            && let Some(dir) = &cfg.session_dir
                            && let Err(e) = sync_state.save(dir)
                        {
                            tracing::warn!(error = %e, "Failed to save relay sync state");
                        }
                    }
                    Some(RelaySyncMsg::Shutdown) => {
                        tracing::debug!(session_id = %session_id, "RelaySync: shutdown requested");
                        break;
                    }
                    None => {
                        tracing::debug!(session_id = %session_id, "RelaySync: channel closed");
                        break;
                    }
                }
            }
        }
    }

    update_state(ConnectionState::Disconnected);
    tracing::debug!(session_id = %session_id, "RelaySync task ended");
}

/// Resolve the event ID for a notification being flushed to the relay.
///
/// Preserves the `eventId` from the notification's meta if present; otherwise
/// generates a consistent `{sessionId}-{counter}` ID via the global event_id
/// counter. This ensures event IDs are always monotonically increasing and
/// comparable by the relay, avoiding gaps caused by random UUIDs.
fn resolve_event_id(notification: &acp::SessionNotification) -> String {
    notification
        .meta
        .as_ref()
        .and_then(|m| m.get("eventId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| crate::util::event_id::generate_event_id(&notification.session_id.0))
}

/// Handle an incoming message from the relay.
fn handle_relay_message(
    session_id: &str,
    to_relay_tx: &mpsc::UnboundedSender<String>,
    msg: &str,
    initialized: &mut bool,
    agent_type: AgentType,
    update_state: &dyn Fn(ConnectionState),
) -> Result<(), String> {
    let json: serde_json::Value =
        serde_json::from_str(msg).map_err(|e| format!("Failed to parse message: {}", e))?;

    let method = json.get("method").and_then(|m| m.as_str());
    let id = json.get("id");

    match method {
        Some("initialize") => {
            // Relay is sending us an initialize request - respond with our metadata
            if let Some(request_id) = id {
                tracing::debug!(session_id = %session_id, "RelaySync: received initialize request from relay");

                // Get hostname
                let hostname = gethostname::gethostname()
                    .into_string()
                    .unwrap_or_else(|_| "unknown".to_string());

                let cwd = std::env::current_dir()
                    .ok()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "/".to_string());

                let response = json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "protocolVersion": "1",
                        "serverCapabilities": {},
                        "_meta": {
                            "agentType": agent_type,
                            "agentId": format!("{}-{}", agent_type, session_id),
                            "sessionId": session_id,
                            "hostname": hostname,
                            "currentWorkingDirectory": cwd,
                        }
                    }
                });

                if to_relay_tx.send(response.to_string()).is_err() {
                    return Err("Failed to send initialize response".to_string());
                }

                tracing::info!(
                    session_id = %session_id,
                    agent_type = %agent_type,
                    "RelaySync: sent initialize response"
                );
                *initialized = true;

                // Transition to Connected only after successful handshake
                update_state(ConnectionState::Connected);

                // Register session with relay via the WebSocket connection.
                let upsert = json!({
                    "jsonrpc": "2.0",
                    "method": "_echo.agent/session/upsert",
                    "params": {
                        "sessionId": session_id,
                        "cwd": cwd,
                    }
                });
                if to_relay_tx.send(upsert.to_string()).is_err() {
                    tracing::warn!(session_id = %session_id, "RelaySync: failed to send session/upsert");
                }

                // Display share URL after successful handshake
                let share_url = build_share_url(session_id);
                tprintln!("📡 Session syncing to relay. View at: {}", share_url);
            }
        }
        Some("_echo.agent/relay/initialized") => {
            tracing::debug!(session_id = %session_id, "RelaySync: relay confirmed TUI sync mode");
        }
        Some(other) => {
            tracing::debug!(
                session_id = %session_id,
                method = other,
                "RelaySync: received unhandled method"
            );
        }
        None => {
            tracing::trace!(session_id = %session_id, "RelaySync: received message without method");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== Connection State Machine Tests =====

    #[test]
    fn test_connection_state_is_connected() {
        assert!(!ConnectionState::Disconnected.is_connected());
        assert!(!ConnectionState::Connecting.is_connected());
        assert!(ConnectionState::Connected.is_connected());
    }

    #[test]
    fn test_connection_state_status_indicator() {
        assert_eq!(ConnectionState::Disconnected.status_indicator(), "📡 ✗");
        assert_eq!(ConnectionState::Connecting.status_indicator(), "📡 ...");
        assert_eq!(ConnectionState::Connected.status_indicator(), "📡");
    }

    #[test]
    fn test_connection_state_display() {
        assert_eq!(ConnectionState::Disconnected.to_string(), "disconnected");
        assert_eq!(ConnectionState::Connecting.to_string(), "connecting");
        assert_eq!(ConnectionState::Connected.to_string(), "connected");
    }

    // ===== Backoff Logic Tests =====
    // These test the backoff helper which may be used in future reconnection strategies.

    const INITIAL_BACKOFF_MS: u64 = 1000;
    const BACKOFF_MULTIPLIER: f64 = 2.0;
    const MAX_BACKOFF_MS: u64 = 30_000;
    const JITTER_PERCENT: f64 = 0.20;

    fn calculate_backoff_with_jitter(attempt: u32) -> u64 {
        let base_delay = INITIAL_BACKOFF_MS as f64 * BACKOFF_MULTIPLIER.powi(attempt as i32);
        let capped_delay = base_delay.min(MAX_BACKOFF_MS as f64);
        let random_byte = uuid::Uuid::new_v4().as_bytes()[0] as f64 / 255.0;
        let jitter_range = capped_delay * JITTER_PERCENT;
        let jitter = (random_byte * 2.0 - 1.0) * jitter_range;
        (capped_delay + jitter).max(INITIAL_BACKOFF_MS as f64) as u64
    }

    #[test]
    fn test_backoff_initial_delay() {
        let delay = calculate_backoff_with_jitter(0);
        assert!(
            delay >= (INITIAL_BACKOFF_MS as f64 * 0.8) as u64,
            "delay {} should be >= {}",
            delay,
            (INITIAL_BACKOFF_MS as f64 * 0.8) as u64
        );
        assert!(
            delay <= (INITIAL_BACKOFF_MS as f64 * 1.2) as u64,
            "delay {} should be <= {}",
            delay,
            (INITIAL_BACKOFF_MS as f64 * 1.2) as u64
        );
    }

    #[test]
    fn test_backoff_exponential_growth() {
        let delay_0 = INITIAL_BACKOFF_MS;
        let delay_1 = (delay_0 as f64 * BACKOFF_MULTIPLIER) as u64;
        let delay_2 = (delay_1 as f64 * BACKOFF_MULTIPLIER) as u64;

        for _ in 0..10 {
            let actual_0 = calculate_backoff_with_jitter(0);
            let actual_1 = calculate_backoff_with_jitter(1);
            let actual_2 = calculate_backoff_with_jitter(2);

            assert!(actual_0 >= (delay_0 as f64 * 0.8) as u64);
            assert!(actual_1 >= (delay_1 as f64 * 0.8) as u64);
            assert!(actual_2 >= (delay_2 as f64 * 0.8) as u64);
        }
    }

    #[test]
    fn test_backoff_max_cap() {
        let delay = calculate_backoff_with_jitter(100);
        assert!(delay <= (MAX_BACKOFF_MS as f64 * 1.2) as u64);
        assert!(delay >= (MAX_BACKOFF_MS as f64 * 0.8) as u64);
    }

    #[test]
    fn test_backoff_jitter_variation() {
        let delays: Vec<u64> = (0..20).map(|_| calculate_backoff_with_jitter(3)).collect();
        let unique_delays: std::collections::HashSet<_> = delays.iter().collect();
        assert!(
            unique_delays.len() > 1,
            "Expected jitter to produce variation, got {:?}",
            delays
        );
    }

    // ===== Disk-Based Sync Cursor Tests =====

    #[test]
    fn test_relay_sync_state_default() {
        let state = RelaySyncState::default();
        assert!(state.last_synced_event_id.is_none());
        assert!(state.last_synced_at.is_none());
        assert!(state.relay_session_id.is_none());
        assert_eq!(state.synced_count, 0);
    }

    #[test]
    fn test_relay_sync_state_update_cursor() {
        let mut state = RelaySyncState::default();
        state.update_cursor("event-123".to_string());

        assert_eq!(state.last_synced_event_id, Some("event-123".to_string()));
        assert!(state.last_synced_at.is_some());
        assert_eq!(state.synced_count, 1);

        // Update again
        state.update_cursor("event-456".to_string());
        assert_eq!(state.last_synced_event_id, Some("event-456".to_string()));
        assert_eq!(state.synced_count, 2);
    }

    #[test]
    fn test_relay_sync_state_serialization() {
        let mut state = RelaySyncState::default();
        state.update_cursor("event-abc".to_string());
        state.relay_session_id = Some("session-xyz".to_string());

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: RelaySyncState = serde_json::from_str(&json).unwrap();

        assert_eq!(
            deserialized.last_synced_event_id,
            Some("event-abc".to_string())
        );
        assert_eq!(
            deserialized.relay_session_id,
            Some("session-xyz".to_string())
        );
        assert_eq!(deserialized.synced_count, 1);
    }

    #[test]
    fn test_relay_sync_state_load_save() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dir_path = temp_dir.path();

        // Save state
        let mut state = RelaySyncState::default();
        state.update_cursor("test-event".to_string());
        state.relay_session_id = Some("test-session".to_string());
        state.save(dir_path).unwrap();

        // Load state
        let loaded = RelaySyncState::load(dir_path);
        assert_eq!(loaded.last_synced_event_id, Some("test-event".to_string()));
        assert_eq!(loaded.relay_session_id, Some("test-session".to_string()));
        assert_eq!(loaded.synced_count, 1);
    }

    #[test]
    #[cfg(unix)]
    fn test_relay_sync_state_save_creates_owner_only_dir() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session_dir = temp_dir.path().join("session-abc");

        RelaySyncState::default().save(&session_dir).unwrap();

        assert_eq!(crate::test_support::unix_mode(&session_dir), 0o700);
    }

    #[test]
    fn test_relay_sync_state_load_missing_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dir_path = temp_dir.path().join("nonexistent");

        // Should return default when file doesn't exist
        let state = RelaySyncState::load(&dir_path);
        assert!(state.last_synced_event_id.is_none());
        assert_eq!(state.synced_count, 0);
    }

    // ===== Existing Tests (Updated) =====

    #[test]
    fn test_relay_sync_msg_queue() {
        let notification = acp::SessionNotification::new(
            acp::SessionId::new("test"),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("test".to_string()),
            ))),
        );
        let _ = RelaySyncMsg::Queue(Box::new(notification));
        let _ = RelaySyncMsg::Flush;
        let _ = RelaySyncMsg::Shutdown;
    }

    #[test]
    fn test_agent_type_serializes_correctly() {
        let json = serde_json::json!({
            "_meta": {
                "agentType": AgentType::Tui,
            }
        });
        assert_eq!(json["_meta"]["agentType"].as_str(), Some("tui"));
    }

    #[test]
    fn test_build_share_url_default() {
        let url = build_share_url("test-session-123");
        assert_eq!(
            url,
            "https://cloud.echo-agent.invalid/build/test-session-123"
        );
    }

    #[test]
    fn test_build_share_url_with_uuid() {
        let url = build_share_url("01937d8a-1234-7abc-9def-0123456789ab");
        assert_eq!(
            url,
            "https://cloud.echo-agent.invalid/build/01937d8a-1234-7abc-9def-0123456789ab"
        );
    }

    // ===== Sync Status Tests =====

    #[test]
    fn test_sync_status_no_sync_state() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session_dir = temp_dir.path();

        // Session exists with updates, but no relay_sync.json
        std::fs::write(
            session_dir.join("updates.jsonl"),
            "{\"method\":\"session/update\"}\n{\"method\":\"session/update\"}\n",
        )
        .unwrap();

        let status = RelaySyncState::get_sync_status(session_dir);

        assert!(!status.has_sync_state);
        assert_eq!(status.synced_count, 0);
        assert!(status.last_synced_event_id.is_none());
        assert!(status.last_synced_at.is_none());
    }

    #[test]
    fn test_sync_status_with_sync_state() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session_dir = temp_dir.path();

        // Create sync state showing 3 synced events
        let sync_state = RelaySyncState {
            relay_session_id: None,
            synced_count: 3,
            last_synced_event_id: Some("event-3".to_string()),
            last_synced_at: Some(1700000000),
        };
        sync_state.save(session_dir).unwrap();

        let status = RelaySyncState::get_sync_status(session_dir);

        assert!(status.has_sync_state);
        assert_eq!(status.synced_count, 3);
        assert_eq!(status.last_synced_event_id, Some("event-3".to_string()));
        assert_eq!(status.last_synced_at, Some(1700000000));
    }

    #[test]
    fn test_sync_status_empty_session() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session_dir = temp_dir.path();

        // No updates file, no sync state
        let status = RelaySyncState::get_sync_status(session_dir);

        assert!(!status.has_sync_state);
        assert_eq!(status.synced_count, 0);
        assert!(status.last_synced_event_id.is_none());
        assert!(status.last_synced_at.is_none());
    }

    #[test]
    fn test_relay_sync_state_exists() {
        let temp_dir = tempfile::tempdir().unwrap();
        let session_dir = temp_dir.path();

        // Initially doesn't exist
        assert!(!RelaySyncState::exists(session_dir));

        // Create sync state
        let sync_state = RelaySyncState::default();
        sync_state.save(session_dir).unwrap();

        // Now exists
        assert!(RelaySyncState::exists(session_dir));
    }

    // ===== resolve_event_id Tests =====

    /// Helper to build a minimal SessionNotification for testing.
    fn make_notification(
        session_id: &str,
        meta: Option<serde_json::Value>,
    ) -> acp::SessionNotification {
        let mut n = acp::SessionNotification::new(
            acp::SessionId::new(session_id),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("test".to_string()),
            ))),
        );
        if let Some(m) = meta.and_then(|v| v.as_object().cloned()) {
            n = n.meta(m);
        }
        n
    }

    #[test]
    fn test_resolve_event_id_uses_meta_when_present() {
        let notification = make_notification(
            "sess-123",
            Some(serde_json::json!({ "eventId": "sess-123-42" })),
        );
        assert_eq!(resolve_event_id(&notification), "sess-123-42");
    }

    #[test]
    fn test_resolve_event_id_generates_format_when_meta_missing() {
        let notification = make_notification("sess-abc", None);
        let id = resolve_event_id(&notification);
        // Must match {sessionId}-{counter} format, NOT a UUID
        assert!(id.starts_with("sess-abc-"));
        let counter_part = id.strip_prefix("sess-abc-").unwrap();
        counter_part
            .parse::<u64>()
            .expect("counter suffix should be numeric");
    }

    #[test]
    fn test_resolve_event_id_generates_format_when_event_id_key_missing() {
        // meta exists but doesn't contain "eventId"
        let notification =
            make_notification("sess-xyz", Some(serde_json::json!({ "other": "value" })));
        let id = resolve_event_id(&notification);
        assert!(id.starts_with("sess-xyz-"));
        let counter_part = id.strip_prefix("sess-xyz-").unwrap();
        counter_part
            .parse::<u64>()
            .expect("counter suffix should be numeric");
    }

    #[test]
    fn test_resolve_event_id_monotonically_increasing() {
        let ids: Vec<String> = (0..10)
            .map(|_| {
                let n = make_notification("s", None);
                resolve_event_id(&n)
            })
            .collect();

        let counters: Vec<u64> = ids
            .iter()
            .map(|id| id.rsplit('-').next().unwrap().parse::<u64>().unwrap())
            .collect();

        // Verify strictly increasing (the global counter is shared across
        // parallel tests, so gaps are expected – only monotonicity matters).
        for window in counters.windows(2) {
            assert!(
                window[1] > window[0],
                "counters not monotonically increasing: {} -> {} (ids: {:?})",
                window[0],
                window[1],
                ids
            );
        }
    }

    // ===== handle_relay_message Tests =====

    #[test]
    fn test_handle_relay_message_initialize_handshake() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let mut initialized = false;

        let update_state = |_state: ConnectionState| {
            // The actual state transition is verified via `initialized`.
        };

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        })
        .to_string();

        let result = handle_relay_message(
            "test-session",
            &tx,
            &msg,
            &mut initialized,
            AgentType::Tui,
            &update_state,
        );

        assert!(result.is_ok(), "handle_relay_message failed: {:?}", result);
        assert!(initialized, "initialized should be true after handshake");

        // Verify a response was sent to the channel
        let response_str = rx.try_recv().expect("should have sent a response");
        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("response should be valid JSON");

        // Verify response structure
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], "1");
        assert_eq!(response["result"]["_meta"]["sessionId"], "test-session");
        assert_eq!(response["result"]["_meta"]["agentType"], "tui");
    }

    #[test]
    fn test_handle_relay_message_unknown_method() {
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let mut initialized = false;

        let update_state = |_: ConnectionState| {};

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "some/unknown"
        })
        .to_string();

        let result = handle_relay_message(
            "test-session",
            &tx,
            &msg,
            &mut initialized,
            AgentType::Tui,
            &update_state,
        );

        assert!(result.is_ok());
        assert!(
            !initialized,
            "initialized should remain false for unknown method"
        );
    }

    #[test]
    fn test_handle_relay_message_invalid_json() {
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let mut initialized = false;

        let update_state = |_: ConnectionState| {};

        let result = handle_relay_message(
            "test-session",
            &tx,
            "not valid json",
            &mut initialized,
            AgentType::Tui,
            &update_state,
        );

        assert!(result.is_err(), "should return error for invalid JSON");
        assert!(
            !initialized,
            "initialized should remain false on parse error"
        );
    }

    #[test]
    fn test_handle_relay_message_initialize_without_id() {
        // initialize request without an id field should be ignored (no response sent)
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let mut initialized = false;

        let update_state = |_: ConnectionState| {};

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {}
        })
        .to_string();

        let result = handle_relay_message(
            "test-session",
            &tx,
            &msg,
            &mut initialized,
            AgentType::Agent,
            &update_state,
        );

        assert!(result.is_ok());
        assert!(
            !initialized,
            "initialized should remain false without request id"
        );
        assert!(
            rx.try_recv().is_err(),
            "should not send response without request id"
        );
    }
}
