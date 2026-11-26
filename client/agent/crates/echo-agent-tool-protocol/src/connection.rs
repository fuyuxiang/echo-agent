//! Connection-shape and tool-definition-mode enums.

use serde::{Deserialize, Serialize};

/// Role of a WebSocket connection. The computer hub uses this to decide
/// which methods are valid on a given socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Harness,
    ToolServer,
}

/// How the computer hub exposes the registered tool set to the model.
///
/// `Concise` carries a configurable meta-tool pair so callers can choose
/// the model-facing names of the search/invoke meta-tools per session.
///
/// Wire form is adjacently tagged on `mode`: `Full` serialises as
/// `{"mode": "full"}` (an object, not a bare string), and `Concise` as
/// `{"mode": "concise", "meta_search": "...", "meta_call": "..."}`.
///
/// `Copy` is intentionally NOT derived: `Concise`'s [`crate::ToolId`]
/// fields wrap heap strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToolDefinitionMode {
    /// Every `ToolDescription` is sent to the model directly.
    Full,
    /// Only the meta-tool pair is sent; everything else is discoverable
    /// through the search meta-tool.
    Concise {
        /// Model-facing name of the search/discovery meta-tool.
        meta_search: crate::ToolId,
        /// Model-facing name of the call/invoke meta-tool.
        meta_call: crate::ToolId,
    },
}
