//! Shared transport types used by workspace communication layers.
//!
//! The `WorkspaceChannel` trait and `MpscChannel` in-process implementation
//! have been removed. Sessions now use `WorkspaceHandle` directly (local mode)
//! or `ToolHarness` RPC calls (proxy mode). These shared types remain for
//! backward compatibility with code that references them.

use serde_json::Value;

/// Context passed alongside transport calls (session routing, tracing, etc.).
#[derive(Debug, Clone, Default)]
pub struct TransportContext {
    pub session_id: Option<String>,
}

/// Transport-level error (distinct from [`WorkspaceError`]).
pub type TransportError = anyhow::Error;
pub type TransportCallResult = Value;
pub type TransportNotification = Value;
