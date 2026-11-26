//! Public event types — the wire contract for `echo-agent-fsnotify`.
//!
//! Pure data: no I/O, no tokio, no intra-crate deps. Safe to lift into a
//! sibling `-types` crate for WASM/no-tokio consumers.
//!
//! All variants are `#[non_exhaustive]`; add additively. The workspace
//! translator (in `echo-agent-workspace`) maps these to
//! `WorkspaceEvent`s and enriches `GitOperationCompleted { head_changed:
//! true }` with `commit + branch + vcs` via a git shell-out — that I/O
//! belongs at the workspace layer, not on the OS-watcher hot path.

use std::path::PathBuf;

/// One semantic event from the local workspace. Causal order on the
/// source's broadcast channel. `FilesChanged` paths share a single `kind`
/// (per-debounce-window grouping); per-event causality would need
/// `Vec<{path, kind}>`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FsEvent {
    /// Workspace file changes; all paths share `kind`. Paths under
    /// `git_dir` are excluded (metadata surfaces as `GitMetaChanged`,
    /// `.lock` files are dropped).
    FilesChanged {
        paths: Vec<PathBuf>,
        kind: FsEventKind,
    },

    /// A git metadata file changed (HEAD, index, refs/, FETCH_HEAD).
    GitMetaChanged { kind: GitMetaKind },

    /// VCS lock activity observed: `index.lock`/`gc.pid`/`.sl` `wlock` is
    /// present, or an event for one arrived with the file already gone (fast
    /// ops complete inside one debounce batch). State is in flux until the
    /// matching `GitOperationCompleted` arrives.
    GitOperationStarted,

    /// The lock has been gone for [`crate::SETTLE_MS`]: rapid lock cycles
    /// (rebase/squash picks) merge into one operation, so one pair is emitted
    /// per burst, not per cycle. `head_changed` reports whether `.git/HEAD`
    /// differs from its value when the operation's *first* lock appeared.
    GitOperationCompleted { head_changed: bool },
}

/// Aligned with `echo_agent_workspace_types::FsEventKind` (identity map at
/// the workspace boundary). `notify::EventKind::{Access, Any, Other}` are
/// filtered upstream and never surface here.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FsEventKind {
    Created,
    #[default]
    Modified,
    Removed,
    Renamed,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GitMetaKind {
    /// `.git/HEAD` (branch switch, commit, rebase step).
    HeadChanged,
    /// `.git/index` (`git add`, `git reset`, `git commit`).
    IndexChanged,
    /// `.git/refs/*` or `.git/packed-refs` (ref updates).
    RefsChanged,
    /// `.git/FETCH_HEAD` (fetch / pull).
    FetchHeadChanged,
}
