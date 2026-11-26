//! Backend-agnostic trait for tool search/discovery.
//!
//! `ToolSearchIndex` is defined in `echo-agent-tools` to keep the tool crate
//! backend-agnostic. The concrete implementation lives in `echo-agent-runtime`
//! (which has access to `McpState` and `FinalizedToolset`).
//!
//! Same pattern as `MemoryBackend` for `memory_search`.

use std::sync::Arc;

/// A single tool search result.
#[derive(Debug, Clone)]
pub struct ToolSearchResult {
    /// Canonical tool name (e.g., `"linear__save_issue"` or a managed gateway `{connector_id}__{tool_id}`).
    pub tool_name: String,
    /// MCP server name, managed gateway connector name, or source/group name.
    pub server_name: String,
    /// Tool description.
    pub description: String,
    /// BM25 relevance score.
    pub score: f32,
    /// Parameter names from the tool's input schema.
    pub parameters: Vec<String>,
    /// Full JSON Schema for the tool's input — included so the model can
    /// construct `use_tool` calls with the correct argument structure.
    pub input_schema: serde_json::Value,
}

/// Result of a composite search — results + index metadata from a single
/// consistent snapshot.
#[derive(Debug, Clone)]
pub struct SearchSnapshot {
    pub results: Vec<ToolSearchResult>,
    pub total_hidden_tools: usize,
    /// `true` when the index reflects all available tools. `false` when the
    /// index source is still warming up (results may be incomplete).
    pub is_ready: bool,
}

/// A summary of an MCP server available for tool search.
#[derive(Debug, Clone)]
pub struct ServerSummary {
    /// Server name (e.g., `"linear"`, `"slack"`).
    pub name: String,
    /// Optional one-line description of the server's capabilities.
    pub description: Option<String>,
    /// Number of tools this server provides.
    pub tool_count: usize,
    /// Unqualified tool names, sorted alphabetically.
    pub tool_names: Vec<String>,
}

/// Backend-agnostic interface for searching tools by keyword.
///
/// Implementations must be `Send + Sync` to be stored as `Arc<dyn ToolSearchIndex>`
/// in `Resources`. No MCP-specific concepts — the concrete implementation
/// in `echo-agent-runtime` maps `mcp_initialized` to `is_ready`.
pub trait ToolSearchIndex: Send + Sync {
    /// Search and return results + metadata from a single consistent snapshot.
    fn search_snapshot(&self, query: &str, limit: usize) -> SearchSnapshot;

    /// List the unique MCP servers in the index with their tool counts.
    ///
    /// Used to build the system-reminder listing connected servers, so
    /// the model knows which integrations are available.
    fn list_server_summaries(&self) -> Vec<ServerSummary>;
}

/// Resource wrapper for injecting a `ToolSearchIndex` into `Resources`.
///
/// Same pattern as `MemoryBackend` — stored as an ephemeral resource (not
/// serialized), injected by `echo-agent-runtime` after MCP initialization.
#[derive(Clone)]
pub struct ToolIndex(pub Arc<dyn ToolSearchIndex>);

impl std::fmt::Debug for ToolIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolIndex").finish()
    }
}
