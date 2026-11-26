//! Relay session sharing module.
//!
//! Provides functionality for syncing TUI sessions to the relay backend
//! via WebSocket, enabling cross-machine session persistence and real-time sharing.
//!
//! # Architecture
//!
//! - Local disk remains the source of truth
//! - [`RelaySync`] streams updates to the relay in real-time
//! - Reconnection is handled by `run_relay_loop` in the agent relay module
//! - Connection state (Disconnected → Connecting → Connected) is observable via [`RelaySync::connection_state`]
//! - Disk-based sync cursor (`relay_sync.json`) tracks last synced event for offline resilience
pub mod sync;
pub mod types;

pub use sync::{ConnectionState, RelaySync, RelaySyncState, StatusCallback, SyncStatus};
pub use types::AgentType;
