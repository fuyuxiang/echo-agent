//! ScopeGraph module for per-file symbol tracking.
//!
//! A ScopeGraph represents the symbols (definitions, references, imports) and their
//! relationships within a single source file.

pub mod edges;
pub mod graph;
pub mod nodes;

pub use edges::EdgeKind;
pub use graph::{
    NodeIndex, QueryVersion, ScopeGraph, ScopeGraphIndex, ScopeStack, Snippet,
    extract_symbols_fast, scope_graph_from_definitions_query,
};
pub use nodes::{LocalDef, LocalImport, LocalScope, NodeKind, Reference, Symbol, SymbolId};

use crate::languages::TSLanguageConfig;

/// Result of building a scope graph, including alias pairs.
pub struct ScopeGraphResult {
    /// The scope graph for the file.
    pub graph: ScopeGraph,
    /// Alias pairs: (alias_name, original_name).
    pub aliases: Vec<(String, String)>,
}

/// Build a ScopeGraph from tree-sitter query and source.
///
/// This is a convenience wrapper around `scope_graph_from_definitions_query`.
pub fn build_scope_graph(
    query: &tree_sitter::Query,
    root_node: tree_sitter::Node<'_>,
    src: &[u8],
    language: &TSLanguageConfig,
) -> ScopeGraphResult {
    let (graph, aliases) = scope_graph_from_definitions_query(query, root_node, src, language);
    ScopeGraphResult { graph, aliases }
}
