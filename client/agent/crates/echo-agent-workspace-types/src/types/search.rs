//! Minimal serializable search-related shapes (ripgrep + fuzzy file search).
//!
//! TODO(workspace): align with the canonical ripgrep / fuzzy types in
//! `echo_agent_runtime::file_system` when the search subsystem moves
//! into the workspace crate.

use serde::{Deserialize, Serialize};

/// Arguments for a `Ripgrep` request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RipgrepArgs {
    /// Regex pattern.
    pub pattern: String,
    /// Optional working directory (relative to workspace root).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Glob filters (positive + negative `!` prefixed patterns).
    #[serde(default)]
    pub globs: Vec<String>,
    /// Case insensitive search.
    #[serde(default)]
    pub case_insensitive: bool,
    /// Max number of matches before short-circuiting.
    #[serde(default)]
    pub max_matches: Option<u32>,
}

/// One match emitted as `OpsChunk::RipgrepHit`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentMatch {
    /// File path containing the match.
    pub path: String,
    /// 1-indexed line number.
    pub line_number: u32,
    /// Matched line text (without trailing newline).
    pub line: String,
    /// Inclusive byte offsets within `line` for the match span.
    #[serde(default)]
    pub spans: Vec<MatchSpan>,
}

/// Byte span within a `ContentMatch::line`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchSpan {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
}

/// Statistics emitted as `OpsChunk::RipgrepDone`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RipgrepStats {
    /// Total files matched.
    #[serde(default)]
    pub files_matched: u32,
    /// Total lines matched.
    #[serde(default)]
    pub lines_matched: u32,
    /// Whether the match limit was hit.
    #[serde(default)]
    pub truncated: bool,
}

/// Arguments for a `FuzzySearch` request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzySearchArgs {
    /// Query string.
    pub query: String,
    /// Optional working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Maximum results to return.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// One match emitted as `OpsChunk::FuzzyMatch`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzyMatch {
    /// File path (relative to workspace root).
    pub path: String,
    /// Match score (higher is better).
    #[serde(default)]
    pub score: i32,
    /// Optional indices of matched chars within `path`.
    #[serde(default)]
    pub matched_indices: Vec<u32>,
}
