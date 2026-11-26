//! Wire-friendly tool-call output.
//!
//! Tool servers may emit `Text`, `Json`, or `Mcp` directly depending
//! on the shape needed.

use serde::{Deserialize, Serialize};

/// Stable wire representation of a tool-call output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ToolOutputWire {
    /// Pre-formatted prompt text — the in-process default via
    /// `ToolOutput::to_prompt_format`.
    Text(String),
    /// Opaque JSON escape hatch — mirrors `ToolOutput::Dynamic`.
    Json(serde_json::Value),
    /// MCP-style structured blocks.
    Mcp { blocks: Vec<McpBlock> },
}

/// One block in [`ToolOutputWire::Mcp`]'s `blocks` list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpBlock {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        /// Base64-encoded payload.
        data: String,
    },
    Resource {
        uri: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}
