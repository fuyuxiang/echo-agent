//! Workspace-ops RPC requests.

use serde::{Deserialize, Serialize};

use crate::types::{FuzzySearchArgs, GitDiffArgs, GitStatusOpts, HunkAction, RipgrepArgs};

/// Top-level workspace-ops RPC.
///
/// All variants share a single streaming RPC; the per-variant chunk
/// contract is documented on `OpsChunk` (most are unary, ripgrep and
/// fuzzy_search are streaming).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WorkspaceOpsRequest {
    // ------------------------------------------------------------------
    // VCS
    // ------------------------------------------------------------------
    /// Read git status.
    GitStatus(GitStatusOpts),
    /// Read a git diff.
    GitDiff(GitDiffArgs),
    /// Read git branch info.
    GitBranchInfo,
    /// Read git repository metadata.
    GitMetadata,

    // ------------------------------------------------------------------
    // Hunks
    // ------------------------------------------------------------------
    /// List all currently-tracked hunks.
    ListHunks,
    /// Apply an action (accept / reject / revert) to a hunk.
    ActOnHunk(HunkAction),

    // ------------------------------------------------------------------
    // Search
    // ------------------------------------------------------------------
    /// Run ripgrep. Streams `OpsChunk::RipgrepHit`s, terminated by
    /// `OpsChunk::RipgrepDone`.
    Ripgrep(RipgrepArgs),
    /// Fuzzy file search. Streams `OpsChunk::FuzzyMatch`es.
    FuzzySearch(FuzzySearchArgs),

    // ------------------------------------------------------------------
    // Discovery / config
    // ------------------------------------------------------------------
    /// Discover skills from the configured search paths.
    DiscoverSkills,
    /// Discover plugins from the configured search paths.
    DiscoverPlugins,
    /// Load the project config.
    LoadProjectConfig,
    /// Load the active permission policy.
    LoadPermissions,
    /// Load `.envrc` (and similar) into a flat env map.
    LoadEnvrc,

    // ------------------------------------------------------------------
    // @file provider
    // ------------------------------------------------------------------
    /// Resolve a batch of `@`-references to absolute file paths.
    ResolveFileRefs(Vec<String>),

    // ------------------------------------------------------------------
    // Memory
    // ------------------------------------------------------------------
    /// Query the memory store.
    MemorySearch {
        /// Free-form query string.
        query: String,
        /// Maximum number of chunks to return.
        ///
        /// `u32` is intentional: `usize` is host-dependent and would
        /// arbitrarily codegen to `uint64` over the wire; `u32` covers
        /// any plausible "max chunks" value with room to spare.
        limit: u32,
    },
    /// Append content to the memory store.
    MemoryWrite(String),

    // ------------------------------------------------------------------
    // Marketplace
    // ------------------------------------------------------------------
    /// Install a plugin from the marketplace.
    InstallPlugin(String),
    /// Force a refresh of the plugin discovery cache.
    RefreshPlugins,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::HunkId;

    fn samples() -> Vec<WorkspaceOpsRequest> {
        vec![
            WorkspaceOpsRequest::GitStatus(GitStatusOpts::default()),
            WorkspaceOpsRequest::GitDiff(GitDiffArgs::default()),
            WorkspaceOpsRequest::GitBranchInfo,
            WorkspaceOpsRequest::GitMetadata,
            WorkspaceOpsRequest::ListHunks,
            WorkspaceOpsRequest::ActOnHunk(HunkAction::Accept {
                hunk_id: HunkId::new("h1"),
            }),
            WorkspaceOpsRequest::Ripgrep(RipgrepArgs {
                pattern: "TODO".into(),
                ..Default::default()
            }),
            WorkspaceOpsRequest::FuzzySearch(FuzzySearchArgs {
                query: "main".into(),
                ..Default::default()
            }),
            WorkspaceOpsRequest::DiscoverSkills,
            WorkspaceOpsRequest::DiscoverPlugins,
            WorkspaceOpsRequest::LoadProjectConfig,
            WorkspaceOpsRequest::LoadPermissions,
            WorkspaceOpsRequest::LoadEnvrc,
            WorkspaceOpsRequest::ResolveFileRefs(vec!["@README.md".into()]),
            WorkspaceOpsRequest::MemorySearch {
                query: "auth".into(),
                limit: 5,
            },
            WorkspaceOpsRequest::MemoryWrite("note".into()),
            WorkspaceOpsRequest::InstallPlugin("https://github.com/example/plugin".into()),
            WorkspaceOpsRequest::RefreshPlugins,
        ]
    }

    #[test]
    fn every_variant_round_trips() {
        for req in samples() {
            let json = serde_json::to_string(&req).unwrap();
            let back: WorkspaceOpsRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(req, back);
        }
    }
}
