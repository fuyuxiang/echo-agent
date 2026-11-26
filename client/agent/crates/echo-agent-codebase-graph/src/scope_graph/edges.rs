//! Edge types for the ScopeGraph.

use serde::{Deserialize, Serialize};

/// Describes the relation between two nodes in the ScopeGraph.
#[derive(Serialize, Deserialize, PartialEq, Eq, Copy, Clone, Debug)]
pub enum EdgeKind {
    /// The edge weight from a nested scope to its parent scope.
    ScopeToScope,

    /// The edge weight from a definition to its definition scope.
    DefToScope,

    /// The edge weight from an import to its definition scope.
    ImportToScope,

    /// The edge weight from a reference to its definition.
    RefToDef,

    /// The edge weight from a reference to its import.
    RefToImport,
}
