//! Node types for the ScopeGraph.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::types::Range;

/// A symbol extracted from the code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    /// The kind/type of symbol (e.g., "function", "class", etc.)
    pub kind: Arc<str>,
    /// The range where the symbol appears.
    pub range: Range,
}

impl Symbol {
    /// Create a new symbol.
    pub fn new(kind: Arc<str>, range: Range) -> Self {
        Self { kind, range }
    }
}

/// An opaque identifier for every symbol in a language.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SymbolId {
    /// Index into the namespace array.
    pub namespace_idx: usize,
    /// Index within the namespace.
    pub symbol_idx: usize,
}

impl SymbolId {
    /// Create a new symbol ID.
    pub fn new(namespace_idx: usize, symbol_idx: usize) -> Self {
        Self {
            namespace_idx,
            symbol_idx,
        }
    }

    /// Get the symbol name from the namespaces.
    pub fn name<'a>(&self, namespaces: &'a [Vec<String>]) -> Option<&'a str> {
        namespaces
            .get(self.namespace_idx)
            .and_then(|ns| ns.get(self.symbol_idx))
            .map(|s| s.as_str())
    }
}

/// A local scope in the source code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Hash, Eq)]
pub struct LocalScope {
    /// The range of this scope.
    pub range: Range,
}

impl LocalScope {
    /// Create a new local scope.
    pub fn new(range: Range) -> Self {
        Self { range }
    }
}

/// A local definition in the source code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct LocalDef {
    /// The range of the identifier being defined.
    pub range: Range,
    /// Optional symbol ID for type-aware resolution.
    pub symbol_id: Option<SymbolId>,
    /// The scope where this definition is visible.
    pub scope: LocalScope,
}

impl LocalDef {
    /// Create a new local definition.
    pub fn new(range: Range, symbol_id: Option<SymbolId>, scope: LocalScope) -> Self {
        Self {
            range,
            symbol_id,
            scope,
        }
    }

    /// Get the name of this definition from source bytes.
    pub fn name<'a>(&self, src: &'a [u8]) -> &'a [u8] {
        &src[self.range.start_byte()..self.range.end_byte()]
    }

    /// Get the scope range.
    pub fn scope_range(&self) -> &Range {
        &self.scope.range
    }
}

/// A local import in the source code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct LocalImport {
    /// The range of the import identifier.
    pub range: Range,
}

impl LocalImport {
    /// Create a new local import.
    pub fn new(range: Range) -> Self {
        Self { range }
    }

    /// Get the name of this import from source bytes.
    pub fn name<'a>(&self, src: &'a [u8]) -> &'a [u8] {
        &src[self.range.start_byte()..self.range.end_byte()]
    }
}

/// A reference to a symbol in the source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    /// The range of the reference.
    pub range: Range,
    /// Optional symbol ID for type-aware resolution.
    pub symbol_id: Option<SymbolId>,
}

impl Reference {
    /// Create a new reference.
    pub fn new(range: Range, symbol_id: Option<SymbolId>) -> Self {
        Self { range, symbol_id }
    }

    /// Get the name of this reference from source bytes.
    pub fn name<'a>(&self, src: &'a [u8]) -> &'a [u8] {
        &src[self.range.start_byte()..self.range.end_byte()]
    }
}

/// The type of a node in the ScopeGraph.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum NodeKind {
    /// A scope node.
    Scope(LocalScope),

    /// A definition node.
    Def(LocalDef),

    /// An import node.
    Import(LocalImport),

    /// A reference node.
    Ref(Reference),
}

impl NodeKind {
    /// Construct a scope node from a range.
    pub fn scope(range: Range) -> Self {
        Self::Scope(LocalScope::new(range))
    }

    /// Produce the range spanned by this node.
    pub fn range(&self) -> Range {
        match self {
            Self::Scope(l) => l.range,
            // For definitions, return the scope range to capture the full context
            Self::Def(d) => d.scope.range,
            Self::Ref(r) => r.range,
            Self::Import(i) => i.range,
        }
    }

    /// Get the identifier range (the actual symbol location).
    pub fn identifier_range(&self) -> Range {
        match self {
            Self::Scope(l) => l.range,
            Self::Def(d) => d.range,
            Self::Ref(r) => r.range,
            Self::Import(i) => i.range,
        }
    }
}
