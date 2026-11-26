//! `@file` provider shapes referenced from `OpsChunk::ResolvedFiles`.
//!
//! TODO(workspace): align with the canonical resolution result types
//! used by the `@file` provider in `echo-agent-runtime`.

use serde::{Deserialize, Serialize};

/// A reference (input) to be resolved by the `@file` provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReference {
    /// The raw reference token (e.g. `"@docs/AGENTS.md"`).
    pub raw: String,
    /// Optional already-resolved absolute path (as a string).
    #[serde(default)]
    pub absolute_path: Option<String>,
}

/// A resolved file returned in `OpsChunk::ResolvedFiles`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedFile {
    /// Original reference text from the user.
    pub reference: String,
    /// Absolute path (as a string).
    #[serde(default)]
    pub path: String,
    /// Whether the path was resolved successfully.
    #[serde(default)]
    pub resolved: bool,
    /// Optional preview content (first N bytes).
    #[serde(default)]
    pub preview: Option<String>,
    /// Reason the reference failed to resolve, if any.
    #[serde(default)]
    pub error: Option<String>,
}
