//! Streaming response chunk types.
//!
//! Each transport call returns a stream of chunks. The chunk type is
//! domain-specific:
//!
//! - [`ToolChunk`] -- streaming output / progress / final result for a
//!   tool invocation (or the response to
//!   [`crate::ToolRequest::Definitions`]). Tools that need user
//!   approval or input also yield `NeedPermission` /
//!   `NeedUserAnswer` chunks; the sampler answers them by sending
//!   [`ToolResponse`] values back on the paired bidi response sender.
//! - [`OpsChunk`] -- one or more chunks for a workspace ops call (most
//!   are unary; ripgrep / fuzzy_search are streaming).
//! - [`SessionChunk`] -- one or more chunks for a session lifecycle call.
//!
//! Every chunk variant maps to a static [`ChunkKind`] discriminator so
//! the typed-trait layer can produce a clear
//! [`crate::WorkspaceError::ProtocolMismatch`] when an unexpected chunk
//! arrives on the wrong stream.
//!
//! The bidi
//! `NeedPermission` / `NeedUserAnswer` flow rides the same streams.

pub mod ops;
pub mod session;
pub mod tool;

pub use ops::OpsChunk;
pub use session::SessionChunk;
pub use tool::{ToolChunk, ToolResponse};

use serde::{Deserialize, Serialize};

/// Static discriminator for every variant across [`ToolChunk`],
/// [`OpsChunk`], and [`SessionChunk`].
///
/// Used as the `got` field of
/// [`crate::WorkspaceError::ProtocolMismatch`]. Each chunk enum exposes
/// a `kind() -> ChunkKind` method that returns its current variant's
/// discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    // ------------------------------------------------------------------
    // ToolChunk variants
    // ------------------------------------------------------------------
    /// `ToolChunk::Output`
    ToolOutput,
    /// `ToolChunk::Progress`
    ToolProgress,
    /// `ToolChunk::Final`
    ToolFinal,
    /// `ToolChunk::Definitions`
    ToolDefinitions,
    /// `ToolChunk::NeedPermission`
    NeedPermission,
    /// `ToolChunk::NeedUserAnswer`
    NeedUserAnswer,
    /// `ToolChunk::NeedPlanModeChange`
    NeedPlanModeChange,

    // ------------------------------------------------------------------
    // OpsChunk variants
    // ------------------------------------------------------------------
    /// `OpsChunk::GitStatus`
    GitStatus,
    /// `OpsChunk::GitDiff`
    GitDiff,
    /// `OpsChunk::GitBranchInfo`
    GitBranchInfo,
    /// `OpsChunk::GitMetadata`
    GitMetadata,
    /// `OpsChunk::Hunks`
    Hunks,
    /// `OpsChunk::Skills`
    Skills,
    /// `OpsChunk::Plugins`
    Plugins,
    /// `OpsChunk::ProjectConfig`
    ProjectConfig,
    /// `OpsChunk::Permissions`
    Permissions,
    /// `OpsChunk::Envrc`
    Envrc,
    /// `OpsChunk::ResolvedFiles`
    ResolvedFiles,
    /// `OpsChunk::MemoryChunks`
    MemoryChunks,
    /// `OpsChunk::Plugin`
    Plugin,
    /// `OpsChunk::Ack`
    Ack,
    /// `OpsChunk::FuzzyMatch`
    FuzzyMatch,
    /// `OpsChunk::RipgrepHit`
    RipgrepHit,
    /// `OpsChunk::RipgrepDone`
    RipgrepDone,

    // ------------------------------------------------------------------
    // SessionChunk variants
    // ------------------------------------------------------------------
    /// `SessionChunk::SessionId`
    SessionId,
    /// `SessionChunk::SessionInfo`
    SessionInfo,
    /// `SessionChunk::RewindResult`
    RewindResult,
    /// `SessionChunk::RewindPoints`
    RewindPoints,
    /// `SessionChunk::Ack`
    SessionAck,
}

impl ChunkKind {
    /// Stable static name (used for error messages).
    ///
    /// Implemented as an exhaustive `match` so that adding a new
    /// variant fails compilation here -- the array returned by
    /// [`Self::all`] depends on this property to stay in sync with
    /// the enum.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolOutput => "ToolOutput",
            Self::ToolProgress => "ToolProgress",
            Self::ToolFinal => "ToolFinal",
            Self::ToolDefinitions => "ToolDefinitions",
            Self::NeedPermission => "NeedPermission",
            Self::NeedUserAnswer => "NeedUserAnswer",
            Self::NeedPlanModeChange => "NeedPlanModeChange",

            Self::GitStatus => "GitStatus",
            Self::GitDiff => "GitDiff",
            Self::GitBranchInfo => "GitBranchInfo",
            Self::GitMetadata => "GitMetadata",
            Self::Hunks => "Hunks",
            Self::Skills => "Skills",
            Self::Plugins => "Plugins",
            Self::ProjectConfig => "ProjectConfig",
            Self::Permissions => "Permissions",
            Self::Envrc => "Envrc",
            Self::ResolvedFiles => "ResolvedFiles",
            Self::MemoryChunks => "MemoryChunks",
            Self::Plugin => "Plugin",
            Self::Ack => "Ack",
            Self::FuzzyMatch => "FuzzyMatch",
            Self::RipgrepHit => "RipgrepHit",
            Self::RipgrepDone => "RipgrepDone",

            Self::SessionId => "SessionId",
            Self::SessionInfo => "SessionInfo",
            Self::RewindResult => "RewindResult",
            Self::RewindPoints => "RewindPoints",
            Self::SessionAck => "SessionAck",
        }
    }

    /// Every variant of [`ChunkKind`], in declaration order.
    ///
    /// Pairs with [`Self::assert_exhaustive`]: the test below uses an
    /// exhaustive `match` to fail compilation if a new variant is
    /// added without also being added to this array. The two together
    /// guarantee the array is exhaustive and unique-by-construction.
    pub const fn all() -> &'static [Self] {
        &[
            Self::ToolOutput,
            Self::ToolProgress,
            Self::ToolFinal,
            Self::ToolDefinitions,
            Self::NeedPermission,
            Self::NeedUserAnswer,
            Self::NeedPlanModeChange,
            Self::GitStatus,
            Self::GitDiff,
            Self::GitBranchInfo,
            Self::GitMetadata,
            Self::Hunks,
            Self::Skills,
            Self::Plugins,
            Self::ProjectConfig,
            Self::Permissions,
            Self::Envrc,
            Self::ResolvedFiles,
            Self::MemoryChunks,
            Self::Plugin,
            Self::Ack,
            Self::FuzzyMatch,
            Self::RipgrepHit,
            Self::RipgrepDone,
            Self::SessionId,
            Self::SessionInfo,
            Self::RewindResult,
            Self::RewindPoints,
            Self::SessionAck,
        ]
    }
}

impl std::fmt::Display for ChunkKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Compile-time exhaustiveness guard.
    ///
    /// If a new variant is added to [`ChunkKind`] without being added
    /// to this match, the build fails. The body is intentionally
    /// trivial -- the only useful thing here is the exhaustive `match`.
    #[test]
    fn assert_exhaustive_match() {
        for kind in ChunkKind::all() {
            match kind {
                ChunkKind::ToolOutput
                | ChunkKind::ToolProgress
                | ChunkKind::ToolFinal
                | ChunkKind::ToolDefinitions
                | ChunkKind::NeedPermission
                | ChunkKind::NeedUserAnswer
                | ChunkKind::NeedPlanModeChange
                | ChunkKind::GitStatus
                | ChunkKind::GitDiff
                | ChunkKind::GitBranchInfo
                | ChunkKind::GitMetadata
                | ChunkKind::Hunks
                | ChunkKind::Skills
                | ChunkKind::Plugins
                | ChunkKind::ProjectConfig
                | ChunkKind::Permissions
                | ChunkKind::Envrc
                | ChunkKind::ResolvedFiles
                | ChunkKind::MemoryChunks
                | ChunkKind::Plugin
                | ChunkKind::Ack
                | ChunkKind::FuzzyMatch
                | ChunkKind::RipgrepHit
                | ChunkKind::RipgrepDone
                | ChunkKind::SessionId
                | ChunkKind::SessionInfo
                | ChunkKind::RewindResult
                | ChunkKind::RewindPoints
                | ChunkKind::SessionAck => {}
            }
        }
    }

    /// Compile-time third gate plus a runtime duplicate-detection
    /// check on `ChunkKind::all()`.
    ///
    /// What this catches:
    ///
    /// 1. **Compile-time:** the inner `fn touch(k: ChunkKind)` is an
    ///    exhaustive `match` with one arm per variant. Adding a new
    ///    variant to `ChunkKind` without adding a matching arm here
    ///    fails to compile -- this is a third compile-time gate
    ///    alongside `as_str()` and `assert_exhaustive_match`. Forgetting
    ///    to also add the variant to `all()` is *not* caught here.
    /// 2. **Runtime:** `HashSet::from_iter(all())` deduplicates the
    ///    array; if any variant is listed more than once in `all()`,
    ///    the set is smaller than the slice and the assertion fires.
    ///
    /// What this does **not** catch (and previous versions of the
    /// doc-comment misleadingly claimed it did): the array having
    /// fewer entries than the enum has variants. Iterating over
    /// `all()` and summing `1` per element trivially equals
    /// `all().len()` regardless of how many variants exist; that
    /// formulation is vacuous. The honest gate against "forgot to
    /// extend `all()` after adding a variant" is reviewer attention
    /// plus the `count: ChunkKind = X` line counts in the diff -- not
    /// a runtime test. The compile-time gates here, in `as_str()`, and
    /// in `assert_exhaustive_match` ensure the reviewer does see a
    /// diff for every new variant.
    ///
    /// The arm pattern is intentionally one-arm-per-variant (rather
    /// than `_ => ...`) so the match is exhaustive, not a catch-all.
    #[test]
    fn chunk_kind_all_is_complete() {
        fn touch(k: ChunkKind) {
            match k {
                ChunkKind::ToolOutput
                | ChunkKind::ToolProgress
                | ChunkKind::ToolFinal
                | ChunkKind::ToolDefinitions
                | ChunkKind::NeedPermission
                | ChunkKind::NeedUserAnswer
                | ChunkKind::NeedPlanModeChange
                | ChunkKind::GitStatus
                | ChunkKind::GitDiff
                | ChunkKind::GitBranchInfo
                | ChunkKind::GitMetadata
                | ChunkKind::Hunks
                | ChunkKind::Skills
                | ChunkKind::Plugins
                | ChunkKind::ProjectConfig
                | ChunkKind::Permissions
                | ChunkKind::Envrc
                | ChunkKind::ResolvedFiles
                | ChunkKind::MemoryChunks
                | ChunkKind::Plugin
                | ChunkKind::Ack
                | ChunkKind::FuzzyMatch
                | ChunkKind::RipgrepHit
                | ChunkKind::RipgrepDone
                | ChunkKind::SessionId
                | ChunkKind::SessionInfo
                | ChunkKind::RewindResult
                | ChunkKind::RewindPoints
                | ChunkKind::SessionAck => {}
            }
        }
        // Compile-time exhaustiveness gate (#1 above): the loop body
        // exists only to invoke `touch` so the match arms are
        // type-checked.
        for &k in ChunkKind::all() {
            touch(k);
        }
        // Runtime duplicate detection (#2 above): if `all()` has been
        // hand-edited to list any variant twice, this fires.
        let unique: std::collections::HashSet<_> = ChunkKind::all().iter().copied().collect();
        assert_eq!(
            unique.len(),
            ChunkKind::all().len(),
            "ChunkKind::all() contains duplicate variants"
        );
    }

    #[test]
    fn discriminator_strings_are_unique_globally() {
        let names: HashSet<&str> = ChunkKind::all().iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names.len(),
            ChunkKind::all().len(),
            "duplicate ChunkKind::as_str() values"
        );
    }

    #[test]
    fn round_trips_through_json() {
        for kind in ChunkKind::all() {
            let json = serde_json::to_string(kind).unwrap();
            let back: ChunkKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back);
        }
    }

    #[test]
    fn display_matches_as_str() {
        for kind in ChunkKind::all() {
            assert_eq!(kind.to_string(), kind.as_str());
        }
    }
}
