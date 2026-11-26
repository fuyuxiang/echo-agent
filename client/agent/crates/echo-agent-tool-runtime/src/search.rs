//! Backend-agnostic tool search interface.
//!
//! `ToolSearchIndex` is a `Send + Sync` trait so concrete implementations
//! can live in different crates (BM25, OpenSearch, in-memory linear) and
//! be stored as `Arc<dyn ToolSearchIndex>` for shared access across tasks.

use std::sync::Arc;

/// A single tool search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSearchResult {
    /// Qualified tool name (e.g. `"linear__save_issue"`).
    pub tool_name: String,
    /// Origin server name (e.g. `"linear"`).
    pub server_name: String,
    /// Tool description.
    pub description: String,
    /// Backend-defined relevance score; comparable within a single
    /// snapshot but not across snapshots.
    pub score: f32,
    /// Parameter names from the tool's input schema, in declaration order.
    pub parameters: Vec<String>,
    /// Full JSON Schema for the tool's input. Included so callers can
    /// construct dispatched tool calls without a separate schema fetch.
    pub input_schema: serde_json::Value,
}

/// Snapshot of a search query — results plus index metadata captured from
/// the same point-in-time view.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchSnapshot {
    pub results: Vec<ToolSearchResult>,
    /// Number of indexed tools that did not appear in `results`.
    pub total_hidden_tools: usize,
    /// `true` when the index reflects all available tools. `false` while
    /// the index source is still warming up.
    pub is_ready: bool,
}

/// Summary of an MCP server (or other tool source) available for search.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerSummary {
    /// Server name (e.g. `"linear"`, `"slack"`).
    pub name: String,
    /// Optional short description of the server's surface area.
    pub description: Option<String>,
    /// Unqualified tool names, sorted alphabetically. Use
    /// [`Self::tool_count`] for a count without indirection.
    pub tool_names: Vec<String>,
}

impl ServerSummary {
    /// Number of tools the server exposes.
    pub fn tool_count(&self) -> usize {
        self.tool_names.len()
    }
}

/// Backend-agnostic search interface.
///
/// Implementations must be `Send + Sync` so they can be wrapped in
/// `Arc<dyn ToolSearchIndex>` and shared across concurrent tasks.
pub trait ToolSearchIndex: Send + Sync {
    /// Run a query against a single consistent index snapshot. Returning
    /// the metadata alongside the results lets the caller render an
    /// accurate "N results out of M" line without a second call.
    fn search_snapshot(&self, query: &str, limit: usize) -> SearchSnapshot;

    /// Enumerate the unique servers in the index. Used to render the
    /// system-reminder listing connected integrations.
    fn list_server_summaries(&self) -> Vec<ServerSummary>;
}

/// Resource wrapper for storing a `ToolSearchIndex` behind an `Arc` in
/// shared resource maps.
#[derive(Clone)]
pub struct ToolIndex(pub Arc<dyn ToolSearchIndex>);

impl std::fmt::Debug for ToolIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolIndex").finish()
    }
}
