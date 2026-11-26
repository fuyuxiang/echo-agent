//! The main ScopeGraph structure.

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::ops::Range as OpsRange;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ahash::AHashMap as HashMap;
use ahash::AHashSet;
use petgraph::Graph;
use petgraph::{Direction, visit::EdgeRef};
use serde::{Deserialize, Serialize};
use tree_sitter::{QueryCursor, StreamingIterator};

use crate::interner::{StringId, StringInterner};
use crate::languages::{LanguageRegistry, TSLanguageConfig};
use crate::types::{FileMeta, Range};

use super::edges::EdgeKind;
use super::nodes::{LocalDef, LocalImport, LocalScope, NodeKind, Reference, Symbol, SymbolId};

/// A type alias for node indices in the graph.
pub type NodeIndex = petgraph::graph::NodeIndex<u32>;

/// A symbol with its name and range.
pub type SymbolWithRange = (Arc<str>, Range);

/// A reference with its resolved definition (if any).
/// Format: (ref_name, ref_range, Option<(def_name, def_range)>)
pub type ReferenceWithDefinition = (String, Range, Option<(String, Range)>);

/// Result of symbol extraction: (definitions, references, aliases).
/// Aliases use Arc<str> to avoid extra allocation when merging into index.
pub type ExtractedSymbols = (
    Vec<SymbolWithRange>,
    Vec<SymbolWithRange>,
    Vec<(Arc<str>, Arc<str>)>,
);

/// Version tracking for tree-sitter queries used to build an index.
///
/// This is used to detect when queries change and trigger a rebuild of the index,
/// even if file contents haven't changed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryVersion {
    /// Legacy format - index was built before query versioning was added.
    /// This triggers a rebuild since we don't know what queries were used.
    /// Default for backwards compatibility with old cached indexes.
    #[default]
    Legacy,
    /// The hash of all tree-sitter queries used to build this index.
    Version(u64),
}

impl QueryVersion {
    /// Check if a rebuild is needed based on the current query version.
    ///
    /// Returns true if:
    /// - This is a Legacy index (unknown query version)
    /// - The version doesn't match the current version
    pub fn needs_rebuild(&self, current_version: u64) -> bool {
        match self {
            QueryVersion::Legacy => true,
            QueryVersion::Version(v) => *v != current_version,
        }
    }
}

/// A graph representation of scopes and names in a single syntax tree.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ScopeGraph {
    /// The raw graph.
    pub(crate) graph: Graph<NodeKind, EdgeKind>,

    /// The root scope node index.
    root_idx: NodeIndex,

    /// String representation of the language.
    lang: String,
}

impl ScopeGraph {
    pub fn new(range: Range, lang: String) -> Self {
        let mut graph = Graph::new();
        let root_idx = graph.add_node(NodeKind::scope(range));
        Self {
            graph,
            root_idx,
            lang,
        }
    }

    pub fn is_definition(&self, node_idx: NodeIndex) -> bool {
        matches!(self.graph[node_idx], NodeKind::Def(_))
    }

    pub fn is_reference(&self, node_idx: NodeIndex) -> bool {
        matches!(self.graph[node_idx], NodeKind::Ref(_))
    }

    pub fn is_import(&self, node_idx: NodeIndex) -> bool {
        matches!(self.graph[node_idx], NodeKind::Import(_))
    }

    pub fn node_by_range(&self, start_byte: usize, end_byte: usize) -> Option<NodeIndex> {
        self.graph
            .node_indices()
            .filter(|&idx| self.is_definition(idx) || self.is_reference(idx) || self.is_import(idx))
            .find(|&idx| {
                let node = self.graph[idx].range();
                start_byte >= node.start_byte() && end_byte <= node.end_byte()
            })
    }

    pub fn tightest_node_for_range(&self, start_byte: usize, end_byte: usize) -> Option<NodeIndex> {
        let mut node_idxs = self
            .graph
            .node_indices()
            .filter(|&idx| self.is_definition(idx))
            .filter(|&idx| {
                let node = self.graph[idx].range();
                node.start_byte() >= start_byte && node.end_byte() <= end_byte
            })
            .collect::<Vec<_>>();
        node_idxs.sort_by(|a, b| {
            let first_node = self.graph[*a].range().byte_size();
            let second_node = self.graph[*b].range().byte_size();
            first_node.cmp(&second_node)
        });
        node_idxs.first().copied()
    }

    // The smallest scope that encompasses `range`. Start at `start` and narrow down if possible.
    fn scope_by_range(&self, range: Range, start: NodeIndex) -> Option<NodeIndex> {
        let target_range = self.graph[start].range();
        if target_range.contains(&range) {
            let child_scopes = self
                .graph
                .edges_directed(start, Direction::Incoming)
                .filter(|edge| *edge.weight() == EdgeKind::ScopeToScope)
                .map(|edge| edge.source())
                .collect::<Vec<_>>();
            for child_scope in child_scopes {
                if let Some(t) = self.scope_by_range(range, child_scope) {
                    return Some(t);
                }
            }
            return Some(start);
        }
        None
    }

    /// Insert a local scope into the scope-graph
    pub fn insert_local_scope(&mut self, new: LocalScope) {
        if let Some(parent_scope) = self.scope_by_range(new.range, self.root_idx) {
            let new_scope = NodeKind::Scope(new);
            let new_idx = self.graph.add_node(new_scope);
            self.graph
                .add_edge(new_idx, parent_scope, EdgeKind::ScopeToScope);
        }
    }

    /// We try to find the tightest local scope which contains this range
    pub fn find_tightest_local_scope(&self, range: &Range) -> LocalScope {
        let mut current_node = self.root_idx;
        loop {
            let mut found = false;
            for edge in self.graph.edges_directed(current_node, Direction::Incoming) {
                if let EdgeKind::ScopeToScope = edge.weight() {
                    let node = &self.graph[edge.source()];
                    if let NodeKind::Scope(scope) = node
                        && scope.range.contains(range)
                    {
                        current_node = edge.source();
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                break;
            }
        }
        if let NodeKind::Scope(scope) = &self.graph[current_node] {
            scope.clone()
        } else {
            unreachable!()
        }
    }

    /// Insert an import into the scope-graph
    pub fn insert_local_import(&mut self, new: LocalImport) {
        if let Some(defining_scope) = self.scope_by_range(new.range, self.root_idx) {
            let new_imp = NodeKind::Import(new);
            let new_idx = self.graph.add_node(new_imp);
            self.graph
                .add_edge(new_idx, defining_scope, EdgeKind::ImportToScope);
        }
    }

    /// Insert a def into the scope-graph, at the parent scope of the defining scope
    pub fn insert_hoisted_def(&mut self, new: LocalDef) {
        if let Some(defining_scope) = self.scope_by_range(new.range, self.root_idx) {
            let new_def = NodeKind::Def(new);
            let new_idx = self.graph.add_node(new_def);

            // if the parent scope exists, insert this def there, if not,
            // insert into the defining scope
            let target_scope = self.parent_scope(defining_scope).unwrap_or(defining_scope);

            self.graph
                .add_edge(new_idx, target_scope, EdgeKind::DefToScope);
        }
    }

    /// Insert a def into the scope-graph, at the root scope
    pub fn insert_global_def(&mut self, new: LocalDef) {
        let new_def = NodeKind::Def(new);
        let new_idx = self.graph.add_node(new_def);
        self.graph
            .add_edge(new_idx, self.root_idx, EdgeKind::DefToScope);
    }

    // Produce the parent scope of a given scope
    fn parent_scope(&self, start: NodeIndex) -> Option<NodeIndex> {
        if matches!(self.graph[start], NodeKind::Scope(_)) {
            return self
                .graph
                .edges_directed(start, Direction::Outgoing)
                .filter(|edge| *edge.weight() == EdgeKind::ScopeToScope)
                .map(|edge| edge.target())
                .next();
        }
        None
    }

    /// Insert a def into the scope-graph
    pub fn insert_local_def(&mut self, new: LocalDef) {
        if let Some(defining_scope) = self.scope_by_range(new.range, self.root_idx) {
            let new_def = NodeKind::Def(new);
            let new_idx = self.graph.add_node(new_def);
            self.graph
                .add_edge(new_idx, defining_scope, EdgeKind::DefToScope);
        }
    }

    fn scope_stack(&self, start: NodeIndex) -> ScopeStack<'_> {
        ScopeStack {
            scope_graph: self,
            start: Some(start),
        }
    }

    /// Insert a ref into the scope-graph
    pub fn insert_ref(&mut self, new: Reference, src: &[u8]) {
        let mut possible_defs = vec![];
        let mut possible_imports = vec![];
        if let Some(local_scope_idx) = self.scope_by_range(new.range, self.root_idx) {
            // traverse the scopes from the current-scope to the root-scope
            for scope in self.scope_stack(local_scope_idx) {
                // find candidate definitions in each scope
                for local_def in self
                    .graph
                    .edges_directed(scope, Direction::Incoming)
                    .filter(|edge| *edge.weight() == EdgeKind::DefToScope)
                    .map(|edge| edge.source())
                {
                    if let NodeKind::Def(def) = &self.graph[local_def]
                        && new.name(src) == def.name(src)
                    {
                        match (&def.symbol_id, &new.symbol_id) {
                            // both contain symbols, but they don't belong to the same namepspace
                            (Some(d), Some(r)) if d.namespace_idx != r.namespace_idx => {}

                            // in all other cases, form an edge from the ref to def.
                            // an empty symbol belongs to all namespaces:
                            // * (None, None)
                            // * (None, Some(_))
                            // * (Some(_), None)
                            // * (Some(_), Some(_)) if def.namespace == ref.namespace
                            _ => {
                                possible_defs.push(local_def);
                            }
                        };
                    }
                }

                // find candidate imports in each scope
                for local_import in self
                    .graph
                    .edges_directed(scope, Direction::Incoming)
                    .filter(|edge| *edge.weight() == EdgeKind::ImportToScope)
                    .map(|edge| edge.source())
                {
                    if let NodeKind::Import(import) = &self.graph[local_import]
                        && new.name(src) == import.name(src)
                    {
                        possible_imports.push(local_import);
                    }
                }
            }
        }

        if !possible_defs.is_empty() || !possible_imports.is_empty() {
            let new_ref = NodeKind::Ref(new);
            let ref_idx = self.graph.add_node(new_ref);
            for def_idx in possible_defs {
                self.graph.add_edge(ref_idx, def_idx, EdgeKind::RefToDef);
            }
            for imp_idx in possible_imports {
                self.graph.add_edge(ref_idx, imp_idx, EdgeKind::RefToImport);
            }
        }
    }

    /// Insert a ref into the scope-graph unconditionally (for cross-file reference tracking)
    /// Unlike insert_ref, this adds the reference even if no local definition is found.
    pub fn insert_ref_unconditional(&mut self, new: Reference) {
        let new_ref = NodeKind::Ref(new);
        self.graph.add_node(new_ref);
    }

    /// Get all definitions in this scope graph with their names
    pub fn get_definitions(&self, src: &[u8]) -> Vec<(String, Range)> {
        self.graph
            .node_indices()
            .filter_map(|idx| {
                if let NodeKind::Def(def) = &self.graph[idx] {
                    let name = String::from_utf8_lossy(def.name(src)).to_string();
                    Some((name, def.range))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get all references in this scope graph with their names
    pub fn get_references(&self, src: &[u8]) -> Vec<(String, Range)> {
        self.graph
            .node_indices()
            .filter_map(|idx| {
                if let NodeKind::Ref(reference) = &self.graph[idx] {
                    let name = String::from_utf8_lossy(reference.name(src)).to_string();
                    Some((name, reference.range))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get all references with their resolved definitions (if any)
    pub fn get_references_with_definitions(&self, src: &[u8]) -> Vec<ReferenceWithDefinition> {
        self.graph
            .node_indices()
            .filter_map(|idx| {
                if let NodeKind::Ref(reference) = &self.graph[idx] {
                    let ref_name = String::from_utf8_lossy(reference.name(src)).to_string();
                    let ref_range = reference.range;

                    // Find the definition this reference points to
                    let def_info = self
                        .graph
                        .edges_directed(idx, Direction::Outgoing)
                        .find(|edge| *edge.weight() == EdgeKind::RefToDef)
                        .and_then(|edge| {
                            if let NodeKind::Def(def) = &self.graph[edge.target()] {
                                let def_name = String::from_utf8_lossy(def.name(src)).to_string();
                                Some((def_name, def.range))
                            } else {
                                None
                            }
                        });

                    Some((ref_name, ref_range, def_info))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find definition by name - returns the range of the definition
    pub fn find_definition(&self, name: &str, src: &[u8]) -> Option<Range> {
        self.graph.node_indices().find_map(|idx| {
            if let NodeKind::Def(def) = &self.graph[idx]
                && def.name(src) == name.as_bytes()
            {
                return Some(def.range);
            }
            None
        })
    }

    /// Find all references to a given name
    pub fn find_references(&self, name: &str, src: &[u8]) -> Vec<Range> {
        self.graph
            .node_indices()
            .filter_map(|idx| {
                if let NodeKind::Ref(reference) = &self.graph[idx]
                    && reference.name(src) == name.as_bytes()
                {
                    return Some(reference.range);
                }
                None
            })
            .collect()
    }

    /// Create a minimal ScopeGraph from pre-extracted symbols.
    ///
    /// This is used for fast indexing where we already have definitions and references
    /// extracted via `extract_symbols_fast`.
    pub fn from_symbols(
        definitions: Vec<(String, Range)>,
        references: Vec<(String, Range)>,
    ) -> Self {
        // Create a graph with a root scope covering the whole file
        let root_range = if !definitions.is_empty() {
            definitions[0].1
        } else if !references.is_empty() {
            references[0].1
        } else {
            Range::default()
        };

        let mut graph = Graph::new();
        let root_idx = graph.add_node(NodeKind::scope(root_range));

        // Add definitions
        for (_name, range) in &definitions {
            let local_def = LocalDef::new(*range, None, LocalScope { range: root_range });
            let def_node = NodeKind::Def(local_def);
            let def_idx = graph.add_node(def_node);
            graph.add_edge(def_idx, root_idx, EdgeKind::DefToScope);
        }

        // Add references
        for (_name, range) in &references {
            let reference = Reference::new(*range, None);
            let ref_node = NodeKind::Ref(reference);
            graph.add_node(ref_node);
        }

        Self {
            graph,
            root_idx,
            lang: String::new(),
        }
    }
}

/// Iterator for traversing the scope stack.
pub struct ScopeStack<'a> {
    scope_graph: &'a ScopeGraph,
    start: Option<NodeIndex>,
}

impl<'a> Iterator for ScopeStack<'a> {
    type Item = NodeIndex;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(start) = self.start {
            let parent = self
                .scope_graph
                .graph
                .edges_directed(start, Direction::Outgoing)
                .find(|edge| *edge.weight() == EdgeKind::ScopeToScope)
                .map(|edge| edge.target());
            let original = start;
            self.start = parent;
            Some(original)
        } else {
            None
        }
    }
}

/// Build a ScopeGraph from file_definitions_query patterns (name.definition.*, name.reference.*)
/// This is simpler than scope_res_generic as it doesn't handle local scoping rules,
/// but it works with the existing query patterns in TSLanguageConfig.
/// Returns: (ScopeGraph, Vec<(alias_name, original_name)>)
pub fn scope_graph_from_definitions_query(
    query: &tree_sitter::Query,
    root_node: tree_sitter::Node<'_>,
    src: &[u8],
    language: &TSLanguageConfig,
) -> (ScopeGraph, Vec<(String, String)>) {
    let mut scope_graph = ScopeGraph::new(
        Range::for_tree_node(&root_node),
        language.primary_language_id().to_string(),
    );

    let mut cursor = QueryCursor::new();

    // Collect all captures first
    let mut def_captures: Vec<(Range, Option<SymbolId>)> = Vec::new();
    let mut ref_captures: Vec<(Range, Option<SymbolId>)> = Vec::new();
    let mut alias_pairs: Vec<(String, String)> = Vec::new();

    // We need to process matches to find alias pairs (where we have both @alias.original and @alias.name)
    let mut matches = cursor.matches(query, root_node, src);

    while let Some(match_) = matches.next() {
        let mut alias_original: Option<String> = None;
        let mut alias_name: Option<String> = None;

        for capture in match_.captures {
            let range = Range::for_tree_node(&capture.node);
            let capture_name = &query.capture_names()[capture.index as usize];
            let text = String::from_utf8_lossy(&src[capture.node.byte_range()]).to_string();

            let parts: Vec<_> = capture_name.split('.').collect();

            match parts.as_slice() {
                ["name", "definition", sym] => {
                    let symbol_id = language.symbol_id_of(sym);
                    def_captures.push((range, symbol_id));
                }
                ["name", "reference", sym] => {
                    let symbol_id = language.symbol_id_of(sym);
                    ref_captures.push((range, symbol_id));
                }
                ["alias", "original"] => {
                    alias_original = Some(text);
                }
                ["alias", "name"] => {
                    alias_name = Some(text);
                }
                _ => {}
            }
        }

        // If we found an alias pair in this match, record it
        if let (Some(original), Some(alias)) = (alias_original, alias_name) {
            alias_pairs.push((alias, original));
        }
    }

    // Insert definitions - they go into the global scope for simplicity
    for (range, symbol_id) in def_captures {
        let local_scope = scope_graph.find_tightest_local_scope(&range);
        let local_def = LocalDef::new(range, symbol_id, local_scope);
        scope_graph.insert_global_def(local_def);
    }

    // Insert references unconditionally for cross-file tracking
    for (range, symbol_id) in ref_captures {
        let reference = Reference::new(range, symbol_id);
        scope_graph.insert_ref_unconditional(reference);
    }

    (scope_graph, alias_pairs)
}

/// Lightweight symbol extraction for fast indexing.
///
/// Unlike scope_graph_from_definitions_query, this doesn't build a full ScopeGraph.
/// It directly extracts (name, range) tuples for definitions and references.
/// This is ~2-3x faster for indexing purposes where we don't need the full graph.
///
/// Returns: (definitions, references, aliases)
pub fn extract_symbols_fast(
    query: &tree_sitter::Query,
    root_node: tree_sitter::Node<'_>,
    src: &[u8],
    _lang_config: &TSLanguageConfig,
) -> ExtractedSymbols {
    // Pre-compute capture indices for fast lookup (avoid string comparisons in hot loop)
    let capture_names = query.capture_names();
    let mut is_def = vec![false; capture_names.len()];
    let mut is_ref = vec![false; capture_names.len()];
    let mut alias_original_idx: Option<usize> = None;
    let mut alias_name_idx: Option<usize> = None;

    for (i, name) in capture_names.iter().enumerate() {
        if name.starts_with("name.definition.") {
            is_def[i] = true;
        } else if name.starts_with("name.reference.") {
            is_ref[i] = true;
        } else if *name == "alias.original" {
            alias_original_idx = Some(i);
        } else if *name == "alias.name" {
            alias_name_idx = Some(i);
        }
    }

    // Pre-allocate with reasonable capacity
    let mut definitions: Vec<(Arc<str>, Range)> = Vec::with_capacity(64);
    let mut references: Vec<(Arc<str>, Range)> = Vec::with_capacity(256);
    let mut alias_pairs: Vec<(Arc<str>, Arc<str>)> = Vec::with_capacity(8);

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, root_node, src);

    while let Some(match_) = matches.next() {
        let mut alias_original: Option<&[u8]> = None;
        let mut alias_name: Option<&[u8]> = None;

        for capture in match_.captures {
            let idx = capture.index as usize;
            let node = capture.node;
            let byte_range = node.byte_range();

            if is_def.get(idx).copied().unwrap_or(false) {
                // Convert Cow<str> directly to Arc<str> - avoids intermediate String allocation
                let text: Arc<str> = String::from_utf8_lossy(&src[byte_range]).into();
                let range = Range::for_tree_node(&node);
                definitions.push((text, range));
            } else if is_ref.get(idx).copied().unwrap_or(false) {
                let text: Arc<str> = String::from_utf8_lossy(&src[byte_range]).into();
                let range = Range::for_tree_node(&node);
                references.push((text, range));
            } else if Some(idx) == alias_original_idx {
                alias_original = Some(&src[byte_range]);
            } else if Some(idx) == alias_name_idx {
                alias_name = Some(&src[byte_range]);
            }
        }

        // If we found an alias pair in this match, record it
        if let (Some(original), Some(alias)) = (alias_original, alias_name) {
            // Convert Cow<str> directly to Arc<str> - avoids intermediate String allocation
            let orig_arc: Arc<str> = String::from_utf8_lossy(original).into();
            let alias_arc: Arc<str> = String::from_utf8_lossy(alias).into();
            alias_pairs.push((alias_arc, orig_arc));
        }
    }

    (definitions, references, alias_pairs)
}

/// Magic bytes for the new binary format: "SGIX" (ScopeGraphIndex)
pub const SCOPE_GRAPH_INDEX_MAGIC: &[u8; 4] = b"SGIX";
/// Current version of the binary format
pub const SCOPE_GRAPH_INDEX_VERSION: u16 = 1;

/// Memory-efficient structure for cross-file symbol indexing.
///
/// Uses `StringInterner` to deduplicate all file paths and symbol names,
/// dramatically reducing memory usage (from ~1GB to ~100MB for large repos).
///
/// # Memory Efficiency
///
/// Instead of storing `Arc<str>` for each string occurrence (which creates
/// millions of allocations during deserialization), this struct stores all
/// unique strings once in a contiguous arena and uses `StringId` (u32) handles.
///
/// # Serialization
///
/// Uses a custom binary format with magic bytes "SGIX" for detection.
/// Falls back gracefully when loading legacy bincode format.
#[derive(Debug, Clone)]
pub struct ScopeGraphIndex {
    /// String interner for all paths and symbols
    pub(crate) interner: StringInterner,
    /// File path ID -> ScopeGraph for that file
    pub(crate) graphs: HashMap<StringId, ScopeGraph>,
    /// Symbol name ID -> list of (file_path_id, line_number) where it's defined.
    /// Line numbers are stored as `u32` (max ~4 billion lines) rather than
    /// `usize` to halve per-entry memory: `(StringId, u32)` = 8 bytes vs the
    /// 16 bytes that `(StringId, usize)` requires on 64-bit targets.
    pub(crate) definitions: HashMap<StringId, Vec<(StringId, u32)>>,
    /// Symbol name ID -> list of (file_path_id, line_number) where it's referenced.
    /// Same compact representation as `definitions`.
    pub(crate) references: HashMap<StringId, Vec<(StringId, u32)>>,
    /// Alias ID -> Original ID mapping
    pub(crate) aliases: HashMap<StringId, StringId>,
    /// Original ID -> set of alias IDs (reverse mapping)
    pub(crate) reverse_aliases: HashMap<StringId, AHashSet<StringId>>,
    /// File path ID -> metadata for staleness detection
    pub(crate) file_meta: HashMap<StringId, FileMeta>,
    /// Version of tree-sitter queries used to build this index
    pub(crate) query_version: QueryVersion,
    /// Reverse index: file path ID -> set of symbol IDs that have definitions in this file.
    /// Used for O(symbols_in_file) removal instead of O(all_symbols) full scan.
    pub(crate) file_to_defs: HashMap<StringId, AHashSet<StringId>>,
    /// Reverse index: file path ID -> set of symbol IDs that have references in this file.
    pub(crate) file_to_refs: HashMap<StringId, AHashSet<StringId>>,
}

impl Default for ScopeGraphIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeGraphIndex {
    pub fn new() -> Self {
        Self {
            interner: StringInterner::new(),
            graphs: HashMap::new(),
            definitions: HashMap::new(),
            references: HashMap::new(),
            aliases: HashMap::new(),
            reverse_aliases: HashMap::new(),
            file_meta: HashMap::new(),
            query_version: QueryVersion::Legacy,
            file_to_defs: HashMap::new(),
            file_to_refs: HashMap::new(),
        }
    }

    // ========================================================================
    // String interning helpers
    // ========================================================================

    /// Intern a string and return its ID.
    #[inline]
    pub fn intern(&mut self, s: &str) -> StringId {
        self.interner.intern(s)
    }

    /// Get the string for an ID.
    #[inline]
    pub fn get_str(&self, id: StringId) -> Option<&str> {
        self.interner.get(id)
    }

    /// Get the ID for a string if it exists.
    #[inline]
    pub fn get_id(&self, s: &str) -> Option<StringId> {
        self.interner.get_id(s)
    }

    // ========================================================================
    // File metadata operations
    // ========================================================================

    /// Update file metadata (size and mtime) for staleness tracking.
    pub fn update_file_meta(&mut self, path: &Path) {
        if let Ok(meta) = std::fs::metadata(path) {
            let path_id = self.intern(&path.to_string_lossy());
            self.file_meta
                .insert(path_id, FileMeta::from_metadata(&meta));
        }
    }

    /// Check if a file's content has changed based on mtime/size.
    /// Returns true if the file is stale (changed or missing).
    pub fn is_file_stale(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        match self
            .get_id(&path_str)
            .and_then(|id| self.file_meta.get(&id))
        {
            Some(cached) => cached.is_stale(path),
            None => true,
        }
    }

    // ========================================================================
    // Alias operations
    // ========================================================================

    /// Register an alias relationship: alias_name is an alias for original_name
    pub fn add_alias(&mut self, alias_name: &str, original_name: &str) {
        let alias_id = self.intern(alias_name);
        let original_id = self.intern(original_name);
        self.aliases.insert(alias_id, original_id);
        self.reverse_aliases
            .entry(original_id)
            .or_default()
            .insert(alias_id);
    }

    /// Register an alias relationship using Arc<str> (for builder compatibility)
    pub fn add_alias_arc(&mut self, alias_name: Arc<str>, original_name: Arc<str>) {
        self.add_alias(&alias_name, &original_name);
    }

    // ========================================================================
    // Symbol insertion (for builder/manager use)
    // ========================================================================

    /// Add a definition occurrence for a symbol.
    pub fn add_definition(&mut self, symbol: &str, path: &str, line: usize) {
        let path_id = self.intern(path);
        self.add_definition_with_path_id(symbol, path_id, line);
    }

    /// Add a definition occurrence with a pre-interned path id.
    ///
    /// `line` is a 1-indexed line number.  It is stored internally as `u32`.
    /// Values above `u32::MAX` (≈ 4.3 billion lines) are **saturated** to
    /// `u32::MAX` rather than wrapping or panicking — no real source file can
    /// have that many lines.
    pub fn add_definition_with_path_id(&mut self, symbol: &str, path_id: StringId, line: usize) {
        let line_u32 = line.min(u32::MAX as usize) as u32;
        let symbol_id = self.intern(symbol);
        self.definitions
            .entry(symbol_id)
            .or_default()
            .push((path_id, line_u32));
        self.file_to_defs
            .entry(path_id)
            .or_default()
            .insert(symbol_id);
    }

    /// Add a reference occurrence for a symbol.
    pub fn add_reference(&mut self, symbol: &str, path: &str, line: usize) {
        let path_id = self.intern(path);
        self.add_reference_with_path_id(symbol, path_id, line);
    }

    /// Add a reference occurrence with a pre-interned path id.
    ///
    /// Same line-number contract as [`add_definition_with_path_id`]: values
    /// above `u32::MAX` are saturated to `u32::MAX`.
    pub fn add_reference_with_path_id(&mut self, symbol: &str, path_id: StringId, line: usize) {
        let line_u32 = line.min(u32::MAX as usize) as u32;
        let symbol_id = self.intern(symbol);
        self.references
            .entry(symbol_id)
            .or_default()
            .push((path_id, line_u32));
        self.file_to_refs
            .entry(path_id)
            .or_default()
            .insert(symbol_id);
    }

    /// Store file metadata for a path.
    pub fn set_file_meta(&mut self, path: &str, meta: FileMeta) {
        let path_id = self.intern(path);
        self.file_meta.insert(path_id, meta);
    }

    /// Check if a symbol has any definitions.
    pub fn has_definition(&self, symbol: &str) -> bool {
        self.get_id(symbol)
            .is_some_and(|id| self.definitions.contains_key(&id))
    }

    /// Get file metadata for a path.
    pub fn get_file_meta(&self, path: &str) -> Option<&FileMeta> {
        self.get_id(path).and_then(|id| self.file_meta.get(&id))
    }

    /// Get all indexed file paths with their metadata.
    pub fn file_paths_with_meta(&self) -> impl Iterator<Item = (&str, &FileMeta)> {
        self.file_meta
            .iter()
            .filter_map(|(&id, meta)| self.get_str(id).map(|path| (path, meta)))
    }

    // ========================================================================
    // File operations
    // ========================================================================

    /// Add a file's scope graph to the index
    pub fn add_file(&mut self, file_path: PathBuf, graph: ScopeGraph, src: &[u8]) {
        let path_id = self.intern(&file_path.to_string_lossy());

        // Extract definitions (convert from 0-indexed to 1-indexed line numbers).
        // Saturate to u32::MAX consistent with add_definition_with_path_id.
        for (name, range) in graph.get_definitions(src) {
            let name_id = self.intern(&name);
            let line = (range.start_line() + 1).min(u32::MAX as usize) as u32;
            self.definitions
                .entry(name_id)
                .or_default()
                .push((path_id, line));
            // Maintain reverse index
            self.file_to_defs
                .entry(path_id)
                .or_default()
                .insert(name_id);
        }

        // Extract references (same saturating conversion).
        for (name, range) in graph.get_references(src) {
            let name_id = self.intern(&name);
            let line = (range.start_line() + 1).min(u32::MAX as usize) as u32;
            self.references
                .entry(name_id)
                .or_default()
                .push((path_id, line));
            // Maintain reverse index
            self.file_to_refs
                .entry(path_id)
                .or_default()
                .insert(name_id);
        }

        self.graphs.insert(path_id, graph);
    }

    /// Remove a file from the index.
    ///
    /// Uses the reverse index (`file_to_defs`/`file_to_refs`) for O(symbols_in_file)
    /// removal instead of scanning all symbols in the entire index.
    pub fn remove_file(&mut self, file_path: &Path) {
        let Some(path_id) = self.get_id(&file_path.to_string_lossy()) else {
            return;
        };

        self.graphs.remove(&path_id);
        self.file_meta.remove(&path_id);

        // Remove definitions for this file using reverse index
        if let Some(symbol_ids) = self.file_to_defs.remove(&path_id) {
            for symbol_id in symbol_ids {
                if let Some(defs) = self.definitions.get_mut(&symbol_id) {
                    defs.retain(|(p, _)| *p != path_id);
                    if defs.is_empty() {
                        self.definitions.remove(&symbol_id);
                    }
                }
            }
        }

        // Remove references for this file using reverse index
        if let Some(symbol_ids) = self.file_to_refs.remove(&path_id) {
            for symbol_id in symbol_ids {
                if let Some(refs) = self.references.get_mut(&symbol_id) {
                    refs.retain(|(p, _)| *p != path_id);
                    if refs.is_empty() {
                        self.references.remove(&symbol_id);
                    }
                }
            }
        }
    }

    /// Get all indexed file paths.
    pub fn indexed_files(&self) -> impl Iterator<Item = &str> {
        self.file_meta.keys().filter_map(|&id| self.get_str(id))
    }

    /// Rename a file in the index (update paths without reparsing).
    ///
    /// Uses the reverse indexes (`file_to_defs`/`file_to_refs`) to update only
    /// the symbols that reference this file — O(symbols_in_file) instead of
    /// O(total_symbols).
    pub fn rename_file(&mut self, from: &Path, to: &Path) {
        let Some(from_id) = self.get_id(&from.to_string_lossy()) else {
            return;
        };
        let to_id = self.intern(&to.to_string_lossy());

        if let Some(graph) = self.graphs.remove(&from_id) {
            self.graphs.insert(to_id, graph);
        }

        if let Some(meta) = self.file_meta.remove(&from_id) {
            self.file_meta.insert(to_id, meta);
        }

        // Update definitions using reverse index
        if let Some(symbol_ids) = self.file_to_defs.remove(&from_id) {
            for &sym_id in &symbol_ids {
                if let Some(locs) = self.definitions.get_mut(&sym_id) {
                    for (path, _) in locs.iter_mut() {
                        if *path == from_id {
                            *path = to_id;
                        }
                    }
                }
            }
            self.file_to_defs.insert(to_id, symbol_ids);
        }

        // Update references using reverse index
        if let Some(symbol_ids) = self.file_to_refs.remove(&from_id) {
            for &sym_id in &symbol_ids {
                if let Some(locs) = self.references.get_mut(&sym_id) {
                    for (path, _) in locs.iter_mut() {
                        if *path == from_id {
                            *path = to_id;
                        }
                    }
                }
            }
            self.file_to_refs.insert(to_id, symbol_ids);
        }
    }

    /// Check if a file is indexed.
    pub fn is_indexed(&self, path: &Path) -> bool {
        self.get_id(&path.to_string_lossy())
            .is_some_and(|id| self.graphs.contains_key(&id))
    }

    /// Get the scope graph for a file.
    pub fn get_graph(&self, path: &Path) -> Option<&ScopeGraph> {
        let path_id = self.get_id(&path.to_string_lossy())?;
        self.graphs.get(&path_id)
    }

    /// Get the number of indexed files.
    pub fn file_count(&self) -> usize {
        self.file_meta.len()
    }

    // ========================================================================
    // Query operations
    // ========================================================================

    /// Find where a symbol is defined (includes resolving aliases)
    pub fn find_definitions(&self, symbol: &str) -> Vec<(&str, usize)> {
        let mut results = Vec::new();

        if let Some(symbol_id) = self.get_id(symbol) {
            // Direct lookup
            if let Some(defs) = self.definitions.get(&symbol_id) {
                for &(path_id, line) in defs {
                    if let Some(path) = self.get_str(path_id) {
                        results.push((path, line as usize));
                    }
                }
            }

            // If symbol is an alias, also look up the original
            if let Some(&original_id) = self.aliases.get(&symbol_id)
                && let Some(defs) = self.definitions.get(&original_id)
            {
                for &(path_id, line) in defs {
                    if let Some(path) = self.get_str(path_id) {
                        results.push((path, line as usize));
                    }
                }
            }
        }

        results
    }

    /// Find where a symbol is referenced (includes all aliases)
    pub fn find_references(&self, symbol: &str) -> Vec<(&str, usize)> {
        let mut results = Vec::new();

        if let Some(symbol_id) = self.get_id(symbol) {
            // Direct lookup
            if let Some(refs) = self.references.get(&symbol_id) {
                for &(path_id, line) in refs {
                    if let Some(path) = self.get_str(path_id) {
                        results.push((path, line as usize));
                    }
                }
            }

            // If symbol is an alias, also look up references to the original
            if let Some(&original_id) = self.aliases.get(&symbol_id)
                && let Some(refs) = self.references.get(&original_id)
            {
                for &(path_id, line) in refs {
                    if let Some(path) = self.get_str(path_id) {
                        results.push((path, line as usize));
                    }
                }
            }

            // If symbol has aliases, also look up references to all aliases
            if let Some(alias_ids) = self.reverse_aliases.get(&symbol_id) {
                for &alias_id in alias_ids {
                    if let Some(refs) = self.references.get(&alias_id) {
                        for &(path_id, line) in refs {
                            if let Some(path) = self.get_str(path_id) {
                                results.push((path, line as usize));
                            }
                        }
                    }
                }
            }
        }

        results
    }

    /// Find references for a symbol, returning owned data with the matched symbol name
    pub fn find_references_with_names(&self, symbol: &str) -> Vec<(String, String, usize)> {
        let mut results = Vec::new();

        if let Some(symbol_id) = self.get_id(symbol) {
            // Direct lookup
            if let Some(refs) = self.references.get(&symbol_id) {
                for &(path_id, line) in refs {
                    if let Some(path) = self.get_str(path_id) {
                        results.push((symbol.to_string(), path.to_string(), line as usize));
                    }
                }
            }

            // If symbol is an alias, also look up references to the original
            if let Some(&original_id) = self.aliases.get(&symbol_id)
                && let (Some(original), Some(refs)) =
                    (self.get_str(original_id), self.references.get(&original_id))
            {
                for &(path_id, line) in refs {
                    if let Some(path) = self.get_str(path_id) {
                        results.push((original.to_string(), path.to_string(), line as usize));
                    }
                }
            }

            // If symbol has aliases, also look up references to all aliases
            if let Some(alias_ids) = self.reverse_aliases.get(&symbol_id) {
                for &alias_id in alias_ids {
                    if let (Some(alias), Some(refs)) =
                        (self.get_str(alias_id), self.references.get(&alias_id))
                    {
                        for &(path_id, line) in refs {
                            if let Some(path) = self.get_str(path_id) {
                                results.push((alias.to_string(), path.to_string(), line as usize));
                            }
                        }
                    }
                }
            }
        }

        results
    }

    /// Find where a symbol is defined, with smart context-aware ranking.
    pub fn find_definitions_smart(
        &self,
        symbol: &str,
        context_file: Option<&Path>,
        language_registry: Option<&LanguageRegistry>,
    ) -> Vec<(String, usize)> {
        let context_ext = context_file
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str());

        let mut results: Vec<(String, usize)> = Vec::new();
        let mut seen: HashSet<(String, usize)> = HashSet::new();

        if let Some(symbol_id) = self.get_id(symbol) {
            // Direct lookup
            if let Some(defs) = self.definitions.get(&symbol_id) {
                for &(path_id, line) in defs {
                    if let Some(path) = self.get_str(path_id) {
                        let key = (path.to_string(), line as usize);
                        if !seen.contains(&key) {
                            seen.insert(key.clone());
                            results.push(key);
                        }
                    }
                }
            }

            // If symbol is an alias, also look up the original
            if let Some(&original_id) = self.aliases.get(&symbol_id)
                && let Some(defs) = self.definitions.get(&original_id)
            {
                for &(path_id, line) in defs {
                    if let Some(path) = self.get_str(path_id) {
                        let key = (path.to_string(), line as usize);
                        if !seen.contains(&key) {
                            seen.insert(key.clone());
                            results.push(key);
                        }
                    }
                }
            }
        }

        // Sort by relevance
        if let Some(ctx_ext) = context_ext {
            results.sort_by(|a, b| {
                let a_ext = Path::new(&a.0)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let b_ext = Path::new(&b.0)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");

                let a_matches = Self::extensions_same_language(a_ext, ctx_ext, language_registry);
                let b_matches = Self::extensions_same_language(b_ext, ctx_ext, language_registry);

                match (a_matches, b_matches) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.0.cmp(&b.0),
                }
            });
        }

        results
    }

    /// Find where a symbol is referenced, with smart context-aware ranking.
    pub fn find_references_smart(
        &self,
        symbol: &str,
        context_file: Option<&Path>,
        language_registry: Option<&LanguageRegistry>,
    ) -> Vec<(String, String, usize)> {
        let context_ext = context_file
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str());

        let mut results = self.find_references_with_names(symbol);

        // Sort by relevance
        if let Some(ctx_ext) = context_ext {
            results.sort_by(|a, b| {
                let a_ext = Path::new(&a.1)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let b_ext = Path::new(&b.1)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");

                let a_matches = Self::extensions_same_language(a_ext, ctx_ext, language_registry);
                let b_matches = Self::extensions_same_language(b_ext, ctx_ext, language_registry);

                match (a_matches, b_matches) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.1.cmp(&b.1),
                }
            });
        }

        results
    }

    fn extensions_same_language(
        ext1: &str,
        ext2: &str,
        registry: Option<&LanguageRegistry>,
    ) -> bool {
        if ext1 == ext2 {
            return true;
        }
        if let Some(reg) = registry {
            return reg.extensions_same_language(ext1, ext2);
        }
        false
    }

    /// Find where a symbol is defined, filtered by file extensions
    pub fn find_definitions_by_extension(
        &self,
        symbol: &str,
        extensions: &[&str],
    ) -> Vec<(String, usize)> {
        let filter = |path: &str| -> bool {
            if extensions.is_empty() {
                return true;
            }
            Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| extensions.contains(&e))
        };

        self.find_definitions(symbol)
            .into_iter()
            .filter(|(path, _)| filter(path))
            .map(|(path, line)| (path.to_string(), line))
            .collect()
    }

    /// Find where a symbol is referenced, filtered by file extensions
    pub fn find_references_by_extension(
        &self,
        symbol: &str,
        extensions: &[&str],
    ) -> Vec<(String, String, usize)> {
        let filter = |path: &str| -> bool {
            if extensions.is_empty() {
                return true;
            }
            Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| extensions.contains(&e))
        };

        self.find_references_with_names(symbol)
            .into_iter()
            .filter(|(_, path, _)| filter(path))
            .collect()
    }

    // ========================================================================
    // Statistics and metadata
    // ========================================================================

    /// Get statistics: (files_count, total_definitions, total_references).
    ///
    /// File count is O(1) via `file_meta.len()`. Definition and reference
    /// counts are O(unique_symbols) — they iterate the top-level HashMap
    /// entries, not individual occurrences.
    pub fn stats(&self) -> (usize, usize, usize) {
        (
            self.file_meta.len(),
            self.definitions.values().map(|v| v.len()).sum(),
            self.references.values().map(|v| v.len()).sum(),
        )
    }

    /// Get alias count
    pub fn alias_count(&self) -> usize {
        self.aliases.len()
    }

    /// Get top symbols by reference count for statistics display.
    /// Returns (symbol_name, reference_count) tuples sorted by count descending.
    pub fn top_referenced_symbols(&self, limit: usize) -> Vec<(String, usize)> {
        let mut ref_counts: Vec<_> = self
            .references
            .iter()
            .filter_map(|(&symbol_id, locs)| {
                self.get_str(symbol_id)
                    .map(|name| (name.to_string(), locs.len()))
            })
            .collect();
        ref_counts.sort_by_key(|b| std::cmp::Reverse(b.1));
        ref_counts.truncate(limit);
        ref_counts
    }

    /// Set the query version hash
    pub fn set_query_version(&mut self, version: u64) {
        self.query_version = QueryVersion::Version(version);
    }

    /// Check if rebuild is needed due to query changes
    pub fn needs_query_rebuild(&self, current_version: u64) -> bool {
        self.query_version.needs_rebuild(current_version)
    }

    /// Reclaim over-allocated Vec capacity after a bulk build.
    ///
    /// This is a **supported public post-build maintenance hook**.  It is
    /// called automatically by [`IndexBuilder`] after every bulk build, so
    /// callers using `IndexBuilder` do not need to call it explicitly.
    ///
    /// It is useful when building an index manually via
    /// [`add_definition`](Self::add_definition) /
    /// [`add_reference`](Self::add_reference): after all insertions are
    /// complete, calling `compact()` trims the Vec doubling over-allocation
    /// in every symbol's location list and in the interner arena/offsets.
    ///
    /// Calling it multiple times is safe (idempotent) but wasteful; do not
    /// call it in tight incremental-update loops.
    pub fn compact(&mut self) {
        for locs in self.definitions.values_mut() {
            locs.shrink_to_fit();
        }
        for locs in self.references.values_mut() {
            locs.shrink_to_fit();
        }
        self.interner.shrink_to_fit();
    }

    // ========================================================================
    // Binary serialization (custom format with magic bytes)
    // ========================================================================

    /// Save the index to a file in binary format.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = std::io::BufWriter::new(file);
        self.write_to(&mut writer)?;
        writer.flush()
    }

    /// Load an index from a file. Returns None if format is unrecognized (legacy).
    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        let file = std::fs::File::open(path)?;
        let mut reader = std::io::BufReader::new(file);

        // Check magic bytes
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;

        if &magic != SCOPE_GRAPH_INDEX_MAGIC {
            // Not our format - return None to indicate legacy
            return Ok(None);
        }

        // Rewind and read full index
        use std::io::Seek;
        reader.seek(std::io::SeekFrom::Start(0))?;
        Self::read_from(&mut reader).map(Some)
    }

    /// Write the index to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        // Header
        w.write_all(SCOPE_GRAPH_INDEX_MAGIC)?;
        w.write_all(&SCOPE_GRAPH_INDEX_VERSION.to_le_bytes())?;

        // Write interner arena
        let arena = self.interner.arena();
        w.write_all(&(arena.len() as u32).to_le_bytes())?;
        w.write_all(arena)?;

        // Write interner offsets
        let offsets = self.interner.offsets();
        w.write_all(&(offsets.len() as u32).to_le_bytes())?;
        for &(start, len) in offsets {
            w.write_all(&start.to_le_bytes())?;
            w.write_all(&len.to_le_bytes())?;
        }

        // Write definitions
        w.write_all(&(self.definitions.len() as u32).to_le_bytes())?;
        for (&symbol_id, locations) in &self.definitions {
            w.write_all(&symbol_id.as_u32().to_le_bytes())?;
            w.write_all(&(locations.len() as u32).to_le_bytes())?;
            for &(path_id, line) in locations {
                w.write_all(&path_id.as_u32().to_le_bytes())?;
                w.write_all(&line.to_le_bytes())?;
            }
        }

        // Write references
        w.write_all(&(self.references.len() as u32).to_le_bytes())?;
        for (&symbol_id, locations) in &self.references {
            w.write_all(&symbol_id.as_u32().to_le_bytes())?;
            w.write_all(&(locations.len() as u32).to_le_bytes())?;
            for &(path_id, line) in locations {
                w.write_all(&path_id.as_u32().to_le_bytes())?;
                w.write_all(&line.to_le_bytes())?;
            }
        }

        // Write aliases
        w.write_all(&(self.aliases.len() as u32).to_le_bytes())?;
        for (&alias_id, &original_id) in &self.aliases {
            w.write_all(&alias_id.as_u32().to_le_bytes())?;
            w.write_all(&original_id.as_u32().to_le_bytes())?;
        }

        // Write file_meta
        w.write_all(&(self.file_meta.len() as u32).to_le_bytes())?;
        for (&path_id, meta) in &self.file_meta {
            w.write_all(&path_id.as_u32().to_le_bytes())?;
            w.write_all(&meta.size.to_le_bytes())?;
            w.write_all(&meta.mtime_secs.to_le_bytes())?;
            w.write_all(&meta.mtime_nanos.to_le_bytes())?;
        }

        // Write query_version
        match &self.query_version {
            QueryVersion::Legacy => w.write_all(&[0u8])?,
            QueryVersion::Version(hash) => {
                w.write_all(&[1u8])?;
                w.write_all(&hash.to_le_bytes())?;
            }
        }

        // Note: graphs are not serialized (rebuilt on demand from source)

        Ok(())
    }

    /// Read an index from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut buf2 = [0u8; 2];
        let mut buf4 = [0u8; 4];
        let mut buf8 = [0u8; 8];

        // Header
        r.read_exact(&mut buf4)?;
        if &buf4 != SCOPE_GRAPH_INDEX_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid magic"));
        }

        r.read_exact(&mut buf2)?;
        let version = u16::from_le_bytes(buf2);
        if version != SCOPE_GRAPH_INDEX_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported version: {}", version),
            ));
        }

        // Read interner arena
        r.read_exact(&mut buf4)?;
        let arena_len = u32::from_le_bytes(buf4) as usize;
        let mut arena = vec![0u8; arena_len];
        r.read_exact(&mut arena)?;

        // Read interner offsets
        r.read_exact(&mut buf4)?;
        let num_offsets = u32::from_le_bytes(buf4) as usize;
        let mut offsets = Vec::with_capacity(num_offsets);
        for _ in 0..num_offsets {
            r.read_exact(&mut buf4)?;
            let start = u32::from_le_bytes(buf4);
            r.read_exact(&mut buf2)?;
            let len = u16::from_le_bytes(buf2);
            offsets.push((start, len));
        }

        let interner = StringInterner::from_parts(arena, offsets);

        // Read definitions
        r.read_exact(&mut buf4)?;
        let num_defs = u32::from_le_bytes(buf4) as usize;
        let mut definitions = HashMap::with_capacity(num_defs);
        for _ in 0..num_defs {
            r.read_exact(&mut buf4)?;
            let symbol_id = StringId::new(u32::from_le_bytes(buf4));
            r.read_exact(&mut buf4)?;
            let num_locations = u32::from_le_bytes(buf4) as usize;
            let mut locations = Vec::with_capacity(num_locations);
            for _ in 0..num_locations {
                r.read_exact(&mut buf4)?;
                let path_id = StringId::new(u32::from_le_bytes(buf4));
                r.read_exact(&mut buf4)?;
                let line = u32::from_le_bytes(buf4);
                locations.push((path_id, line));
            }
            definitions.insert(symbol_id, locations);
        }

        // Read references
        r.read_exact(&mut buf4)?;
        let num_refs = u32::from_le_bytes(buf4) as usize;
        let mut references = HashMap::with_capacity(num_refs);
        for _ in 0..num_refs {
            r.read_exact(&mut buf4)?;
            let symbol_id = StringId::new(u32::from_le_bytes(buf4));
            r.read_exact(&mut buf4)?;
            let num_locations = u32::from_le_bytes(buf4) as usize;
            let mut locations = Vec::with_capacity(num_locations);
            for _ in 0..num_locations {
                r.read_exact(&mut buf4)?;
                let path_id = StringId::new(u32::from_le_bytes(buf4));
                r.read_exact(&mut buf4)?;
                let line = u32::from_le_bytes(buf4);
                locations.push((path_id, line));
            }
            references.insert(symbol_id, locations);
        }

        // Read aliases
        r.read_exact(&mut buf4)?;
        let num_aliases = u32::from_le_bytes(buf4) as usize;
        let mut aliases = HashMap::with_capacity(num_aliases);
        // Pre-size with num_aliases as an upper bound on unique originals,
        // avoiding reallocation as we iterate.
        let mut reverse_aliases: HashMap<StringId, AHashSet<StringId>> =
            HashMap::with_capacity(num_aliases);
        for _ in 0..num_aliases {
            r.read_exact(&mut buf4)?;
            let alias_id = StringId::new(u32::from_le_bytes(buf4));
            r.read_exact(&mut buf4)?;
            let original_id = StringId::new(u32::from_le_bytes(buf4));
            aliases.insert(alias_id, original_id);
            reverse_aliases
                .entry(original_id)
                .or_default()
                .insert(alias_id);
        }

        // Read file_meta
        r.read_exact(&mut buf4)?;
        let num_files = u32::from_le_bytes(buf4) as usize;
        let mut file_meta = HashMap::with_capacity(num_files);
        for _ in 0..num_files {
            r.read_exact(&mut buf4)?;
            let path_id = StringId::new(u32::from_le_bytes(buf4));
            r.read_exact(&mut buf8)?;
            let size = u64::from_le_bytes(buf8);
            r.read_exact(&mut buf8)?;
            let mtime_secs = i64::from_le_bytes(buf8);
            r.read_exact(&mut buf4)?;
            let mtime_nanos = u32::from_le_bytes(buf4);
            file_meta.insert(
                path_id,
                FileMeta {
                    size,
                    mtime_secs,
                    mtime_nanos,
                },
            );
        }

        // Read query_version
        let mut tag = [0u8; 1];
        let query_version = if r.read_exact(&mut tag).is_ok() {
            match tag[0] {
                0 => QueryVersion::Legacy,
                1 => {
                    r.read_exact(&mut buf8)?;
                    QueryVersion::Version(u64::from_le_bytes(buf8))
                }
                _ => QueryVersion::Legacy,
            }
        } else {
            QueryVersion::Legacy
        };

        // Rebuild reverse indexes from definitions and references.
        // Pre-sized to num_files to avoid rehashing as we fill them in.
        let mut file_to_defs: HashMap<StringId, AHashSet<StringId>> =
            HashMap::with_capacity(num_files);
        for (&symbol_id, locations) in &definitions {
            for &(path_id, _) in locations {
                file_to_defs.entry(path_id).or_default().insert(symbol_id);
            }
        }
        let mut file_to_refs: HashMap<StringId, AHashSet<StringId>> =
            HashMap::with_capacity(num_files);
        for (&symbol_id, locations) in &references {
            for &(path_id, _) in locations {
                file_to_refs.entry(path_id).or_default().insert(symbol_id);
            }
        }

        Ok(Self {
            interner,
            graphs: HashMap::new(), // Not serialized
            definitions,
            references,
            aliases,
            reverse_aliases,
            file_meta,
            query_version,
            file_to_defs,
            file_to_refs,
        })
    }
}

/// A snippet of code with its data, line range, and associated symbols.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Snippet {
    pub data: String,
    pub line_range: OpsRange<usize>,
    pub symbols: Vec<Symbol>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that normal line numbers survive the usize→u32→usize round-trip
    /// without loss, and that the u32::MAX boundary value is also preserved.
    ///
    /// This test documents the public contract of `add_definition_with_path_id`
    /// and `add_reference_with_path_id`: callers may pass any `usize` that fits
    /// in a `u32`; values at or below `u32::MAX` are stored and returned exactly.
    #[test]
    fn test_line_number_u32_roundtrip() {
        let mut index = ScopeGraphIndex::new();

        // Typical line numbers
        index.add_definition("sym", "file.rs", 1);
        index.add_definition("sym", "file.rs", 100);
        index.add_definition("sym", "file.rs", 65_535);

        // At the u32::MAX boundary — must be stored exactly, not corrupted
        index.add_definition("sym", "file.rs", u32::MAX as usize);

        let locs = index.find_definitions("sym");
        let lines: Vec<usize> = locs.iter().map(|&(_, l)| l).collect();

        assert!(lines.contains(&1));
        assert!(lines.contains(&100));
        assert!(lines.contains(&65_535));
        assert!(
            lines.contains(&(u32::MAX as usize)),
            "u32::MAX line number must survive round-trip; got {:?}",
            lines
        );
    }

    /// Verify that line numbers above `u32::MAX` are **saturated** to
    /// `u32::MAX`, not wrapped/truncated.
    ///
    /// This is the overflow-path test the public contract requires.
    #[test]
    fn test_line_number_overflow_saturates() {
        let mut index = ScopeGraphIndex::new();

        // u32::MAX + 1 would wrap to 0 with a truncating cast — must saturate
        // to u32::MAX instead.
        index.add_definition("sym", "file.rs", u32::MAX as usize + 1);
        let locs = index.find_definitions("sym");
        assert_eq!(locs.len(), 1);
        assert_eq!(
            locs[0].1,
            u32::MAX as usize,
            "line number exceeding u32::MAX must saturate to u32::MAX, not wrap"
        );

        // References use the same contract
        let mut index2 = ScopeGraphIndex::new();
        index2.add_reference("sym", "file.rs", u32::MAX as usize + 100);
        let refs = index2.find_references("sym");
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].1,
            u32::MAX as usize,
            "reference line number exceeding u32::MAX must saturate"
        );
    }

    /// Verify compact() is idempotent — calling it twice produces the same
    /// stats as calling it once.
    #[test]
    fn test_compact_is_idempotent() {
        let mut index = ScopeGraphIndex::new();
        for i in 0..20u32 {
            index.add_definition("sym", &format!("file_{}.rs", i), (i + 1) as usize);
        }

        index.compact();
        let (f1, d1, r1) = index.stats();

        index.compact(); // second call must be a no-op
        let (f2, d2, r2) = index.stats();

        assert_eq!(f1, f2);
        assert_eq!(d1, d2);
        assert_eq!(r1, r2);
    }
}
