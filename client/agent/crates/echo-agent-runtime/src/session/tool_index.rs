//! Concrete `ToolSearchIndex` implementation using BM25.
//!
//! Builds a BM25 index over registered MCP tools and searches it.
//! The index is rebuilt on each search call (sub-millisecond for tens
//! to low hundreds of tools).

use std::sync::Arc;

use std::sync::Mutex;

use bm25::{Language, SearchEngineBuilder};
use echo_agent_tools::types::tool_index::{
    SearchSnapshot, ServerSummary, ToolSearchIndex, ToolSearchResult,
};

use super::mcp_servers::MCP_TOOL_NAME_DELIMITER;

/// Split a compound identifier into component words.
///
/// Handles `__` (qualified-name delimiter), `_` (snake_case),
/// `-` (kebab-case), and camelCase / PascalCase boundaries.
///
/// Only allocates when the input actually contains a split point.
fn split_identifier(s: &str) -> Vec<&str> {
    let mut words: Vec<&str> = Vec::new();
    for part in s
        .split("__")
        .flat_map(|p| p.split('_'))
        .flat_map(|p| p.split('-'))
    {
        if part.is_empty() {
            continue;
        }
        // Split on camelCase boundaries: a lowercase→uppercase transition.
        let bytes = part.as_bytes();
        let mut start = 0;
        for i in 1..bytes.len() {
            if bytes[i - 1].is_ascii_lowercase() && bytes[i].is_ascii_uppercase() {
                words.push(&part[start..i]);
                start = i;
            }
        }
        words.push(&part[start..]);
    }
    words
}

/// Normalize a search query by expanding compound identifiers.
///
/// If the query contains `__`, `_`, `-`, or camelCase, the split
/// components are appended so BM25 can match on individual parts.
/// Plain natural-language queries pass through unchanged.
fn normalize_query(query: &str) -> String {
    let needs_split = query.contains("__")
        || query.contains('_')
        || query.contains('-')
        || query
            .as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
    if !needs_split {
        return query.to_owned();
    }
    let extra: Vec<&str> = query
        .split_whitespace()
        .flat_map(split_identifier)
        .collect();
    if extra.is_empty() {
        return query.to_owned();
    }
    format!("{query} {}", extra.join(" "))
}

/// Metadata for a single MCP tool, used to build BM25 documents.
#[derive(Debug, Clone)]
pub(crate) struct ToolMetadata {
    /// Canonical name (e.g., `"linear__save_issue"` or a managed gateway `{connector_id}__{tool_id}`).
    pub qualified_name: String,
    /// Server, source, or grouping name (e.g., `"linear"`).
    pub server_name: String,
    /// Search/display tool name.
    pub tool_name: String,
    /// Tool description.
    pub description: String,
    /// Parameter names from input schema.
    pub parameters: Vec<String>,
    /// Full JSON Schema for the tool's input parameters.
    pub input_schema: serde_json::Value,
}

impl ToolMetadata {
    /// Build the BM25 document text for this tool.
    ///
    /// Composed of: `{server_name} {tool_name} {description} {parameter_names}`
    /// plus decomposed identifier components (snake_case, camelCase, kebab-case).
    /// Exact canonical-name lookup is handled before BM25 ranking.
    fn to_document(&self) -> String {
        let params = self.parameters.join(" ");
        let doc = format!(
            "{} {} {} {}",
            self.server_name, self.tool_name, self.description, params
        );
        // Decompose identifiers into component words so BM25 can match
        // individual parts. e.g. "SearchDashboards" → "Search Dashboards",
        // "grafana-ai" → "grafana ai".
        let extra: String = [self.server_name.as_str(), self.tool_name.as_str()]
            .iter()
            .flat_map(|s| split_identifier(s))
            .chain(self.parameters.iter().flat_map(|p| split_identifier(p)))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{doc} {extra}")
    }
}

/// Per-server metadata (name + optional description from the MCP
/// initialize handshake's `instructions` field). Tool count is derived
/// from `ToolMetadataSnapshot::tools` at read time, so disabled tools
/// (which are unregistered from the bridge before the snapshot is
/// rebuilt) are excluded by construction.
#[derive(Debug, Clone)]
pub(crate) struct ServerMetadata {
    pub name: String,
    pub description: Option<String>,
}

/// A snapshot of MCP tool metadata, shared between the session and the search index.
///
/// Updated when MCP tools are registered or re-initialized.
#[derive(Debug, Clone, Default)]
pub(crate) struct ToolMetadataSnapshot {
    pub tools: Vec<ToolMetadata>,
    pub servers: Vec<ServerMetadata>,
    pub mcp_initialized: bool,
}

/// Concrete `ToolSearchIndex` implementation backed by BM25.
///
/// Holds a shared snapshot of MCP tool metadata behind a `std::sync::Mutex`.
/// Using a sync mutex (not TokioMutex) because:
/// - The lock is held only to clone the snapshot (fast, no I/O)
/// - `search_snapshot()` is a sync trait method called from async context
/// - `TokioMutex::blocking_lock()` panics on single-threaded runtimes
pub(crate) struct Bm25ToolSearchIndex {
    snapshot: Arc<Mutex<ToolMetadataSnapshot>>,
}

impl Bm25ToolSearchIndex {
    pub(crate) fn new(snapshot: Arc<Mutex<ToolMetadataSnapshot>>) -> Self {
        Self { snapshot }
    }
}

impl ToolSearchIndex for Bm25ToolSearchIndex {
    fn search_snapshot(&self, query: &str, limit: usize) -> SearchSnapshot {
        let snapshot = self.snapshot.lock().unwrap().clone();

        let is_ready = snapshot.mcp_initialized;
        let total_hidden_tools = snapshot.tools.len();

        if snapshot.tools.is_empty() {
            return SearchSnapshot {
                results: Vec::new(),
                total_hidden_tools,
                is_ready,
            };
        }

        // Fast path: exact match on qualified name or bare tool name.
        // When the model already knows the tool name (e.g. "grafana-ai__SearchDashboards"
        // or "SearchDashboards"), skip BM25 entirely and return the match directly.
        let query_lower = query.trim().to_lowercase();
        if let Some(exact) = snapshot.tools.iter().find(|t| {
            t.qualified_name.to_lowercase() == query_lower
                || t.tool_name.to_lowercase() == query_lower
        }) {
            return SearchSnapshot {
                results: vec![ToolSearchResult {
                    tool_name: exact.qualified_name.clone(),
                    server_name: exact.server_name.clone(),
                    description: exact.description.clone(),
                    score: 1.0,
                    parameters: exact.parameters.clone(),
                    input_schema: exact.input_schema.clone(),
                }],
                total_hidden_tools,
                is_ready,
            };
        }

        let documents: Vec<String> = snapshot.tools.iter().map(|t| t.to_document()).collect();

        let search_engine =
            SearchEngineBuilder::<u32>::with_corpus(Language::English, documents).build();

        let normalized = normalize_query(query);
        let bm25_results = search_engine.search(&normalized, limit);

        let results = bm25_results
            .into_iter()
            .filter_map(|sr| {
                let meta = snapshot.tools.get(sr.document.id as usize)?;
                Some(ToolSearchResult {
                    tool_name: meta.qualified_name.clone(),
                    server_name: meta.server_name.clone(),
                    description: meta.description.clone(),
                    score: sr.score,
                    parameters: meta.parameters.clone(),
                    input_schema: meta.input_schema.clone(),
                })
            })
            .collect();

        SearchSnapshot {
            results,
            total_hidden_tools,
            is_ready,
        }
    }

    fn list_server_summaries(&self) -> Vec<ServerSummary> {
        let (tools_snapshot, servers) = {
            let snapshot = self.snapshot.lock().unwrap();
            let tools: Vec<(String, String)> = snapshot
                .tools
                .iter()
                .map(|t| (t.server_name.clone(), t.tool_name.clone()))
                .collect();
            let srvs: Vec<ServerMetadata> = snapshot.servers.clone();
            (tools, srvs)
        };

        let mut map: std::collections::BTreeMap<String, (usize, Option<String>, Vec<String>)> =
            std::collections::BTreeMap::new();
        for (server, tool) in tools_snapshot {
            let (count, _desc, names) = map.entry(server).or_insert((0, None, Vec::new()));
            *count += 1;
            names.push(tool);
        }
        for s in servers {
            let (_count, desc, _names) = map.entry(s.name).or_insert((0, None, Vec::new()));
            *desc = s.description;
        }

        map.into_iter()
            .map(|(name, (tool_count, description, mut tool_names))| {
                tool_names.sort_unstable();
                ServerSummary {
                    name,
                    description,
                    tool_count,
                    tool_names,
                }
            })
            .collect()
    }
}

/// Extract parameter names from a JSON Schema `properties` object.
pub(crate) fn extract_parameter_names(schema: &serde_json::Value) -> Vec<String> {
    schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

/// Split a qualified MCP tool name (`"server__tool"`) into `(server, tool)`.
pub(crate) fn split_qualified_name(qualified: &str) -> (&str, &str) {
    qualified
        .split_once(MCP_TOOL_NAME_DELIMITER)
        .unwrap_or(("", qualified))
}

#[cfg(test)]
#[path = "tool_index_tests.rs"]
mod tests;
