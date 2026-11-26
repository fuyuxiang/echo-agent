//! Hook events delivered from the harness to tools.

use serde::{Deserialize, Serialize};

/// Internally-tagged hook payload. New variants land alongside `Custom`,
/// which keeps unknown future kinds round-trippable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HookEvent {
    /// Cancel an in-flight call. The owning `tool_call_id` travels in the
    /// enclosing `hook` frame.
    Cancel,
    Pause,
    Resume,
    /// Broadcast to every tool server bound to the session.
    SessionEnded,
    /// Forward-compatible escape hatch.
    Custom {
        kind: String,
        payload: serde_json::Value,
    },
}
