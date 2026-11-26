//! Tool RPC requests.

use serde::{Deserialize, Serialize};

use crate::identity::{SessionId, ToolCallId};

/// Top-level tool RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ToolRequest {
    /// Execute a tool. The streaming response is a sequence of
    /// `ToolChunk::Output` / `Progress` chunks ending with exactly one
    /// `ToolChunk::Final`.
    Call(ToolCallArgs),
    /// List the registered tool definitions. The response is a single
    /// `ToolChunk::Definitions(Vec<ToolDef>)`.
    Definitions,
}

/// Arguments for `ToolRequest::Call`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallArgs {
    /// Session id the tool runs in.
    pub session: SessionId,
    /// Registered tool name (e.g. `"read_file"`).
    pub tool_name: String,
    /// JSON-encoded input arguments. The shape is tool-specific.
    #[serde(default)]
    pub input_json: String,
    /// Caller-assigned tool call id (used for cancellation and for
    /// correlating tool-stream chunks back to this call).
    pub call_id: ToolCallId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_call_variant() {
        let req = ToolRequest::Call(ToolCallArgs {
            session: SessionId::new("s1"),
            tool_name: "read_file".into(),
            input_json: r#"{"path": "/etc/hosts"}"#.into(),
            call_id: ToolCallId::new("c1"),
        });
        let json = serde_json::to_string(&req).unwrap();
        let back: ToolRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn round_trips_definitions_variant() {
        let req = ToolRequest::Definitions;
        let json = serde_json::to_string(&req).unwrap();
        let back: ToolRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }
}
