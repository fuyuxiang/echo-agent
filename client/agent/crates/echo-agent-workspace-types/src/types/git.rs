//! Minimal serializable git/VCS shapes referenced from `WorkspaceOpsRequest`
//! and `OpsChunk`.
//!
//! TODO(workspace): align with the canonical git types in
//! `echo_agent_runtime::session::git` and `echo_agent_runtime::extensions::git`
//! when the VCS subsystem moves into the workspace crate.

use serde::{Deserialize, Serialize};

/// VCS kind.
///
/// TODO(workspace): align with `echo_agent_runtime::session::git::VcsKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VcsKind {
    /// Pure git repository (`.git/` directory).
    #[default]
    Git,
    /// Jujutsu repository colocated with git.
    Jj,
}

/// Options controlling a `GitStatus` request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusOpts {
    /// Include untracked files in the status.
    #[serde(default)]
    pub include_untracked: bool,
    /// Include ignored files in the status.
    #[serde(default)]
    pub include_ignored: bool,
}

/// Status snapshot returned by `OpsChunk::GitStatus`.
///
/// TODO(workspace): align with `GitStatusData` in
/// `echo_agent_runtime::session::git`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    /// Current branch (if HEAD is on one).
    #[serde(default)]
    pub branch: String,
    /// Commit at HEAD.
    #[serde(default)]
    pub head_commit: String,
    /// Repository root (absolute path as a string).
    #[serde(default)]
    pub root: String,
    /// Files with staged changes (relative to repo root).
    #[serde(default)]
    pub staged: Vec<String>,
    /// Files with unstaged changes.
    #[serde(default)]
    pub unstaged: Vec<String>,
    /// Untracked files (only populated when [`GitStatusOpts::include_untracked`] is set).
    #[serde(default)]
    pub untracked: Vec<String>,
    /// Whether the working tree is clean (no staged/unstaged changes).
    #[serde(default)]
    pub clean: bool,
    /// Detected VCS kind.
    #[serde(default)]
    pub vcs: VcsKind,
}

/// Arguments for a `GitDiff` request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiffArgs {
    /// Optional revision range (e.g. `"main..HEAD"`).
    #[serde(default)]
    pub range: Option<String>,
    /// Optional path filter.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Whether to include staged changes only.
    #[serde(default)]
    pub staged: bool,
}

/// Diff returned by `OpsChunk::GitDiff`.
///
/// TODO(workspace): align with `GitDiffsData` in
/// `echo_agent_runtime::session::git`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiff {
    /// Unified-diff text.
    #[serde(default)]
    pub patch: String,
    /// Files touched by the diff.
    #[serde(default)]
    pub files: Vec<String>,
}

/// Branch information returned by `OpsChunk::GitBranchInfo`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchInfo {
    /// Current branch (None for detached HEAD).
    #[serde(default)]
    pub current: Option<String>,
    /// All local branches in deterministic order.
    #[serde(default)]
    pub local: Vec<String>,
    /// Upstream branch tracked by the current branch (if any).
    #[serde(default)]
    pub upstream: Option<String>,
}

/// Repository metadata returned by `OpsChunk::GitMetadata`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitMetadata {
    /// Origin URL (e.g. `git@github.com:org/repo.git`).
    #[serde(default)]
    pub origin_url: Option<String>,
    /// Repository root (absolute path as a string).
    #[serde(default)]
    pub root: String,
    /// Default branch as known to the remote (e.g. `main`).
    #[serde(default)]
    pub default_branch: Option<String>,
    /// VCS kind.
    #[serde(default)]
    pub vcs: VcsKind,
}
