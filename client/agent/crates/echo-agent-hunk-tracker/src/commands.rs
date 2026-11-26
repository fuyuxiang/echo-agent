//! Commands sent to the HunkTrackerActor.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::oneshot;

use crate::types::{
    FileContentEntry, FileHunkData, Hunk, HunkAction, HunkActionError, HunkId, HunkSourceFilter,
    HunkTrackerSnapshot, HunkTurnDelta, SessionSummary, TrackingMode,
};

/// Commands sent to the HunkTrackerActor via mpsc channel.
#[derive(Debug)]
pub enum HunkTrackerCommand {
    // === Mutation Commands (fire-and-forget) ===
    /// Agent tool wrote to a file - record it and compute hunks
    RecordAgentWrite {
        path: PathBuf,
        content: String,
        prompt_index: usize,
        /// Content of the file before this write (if known).
        /// Used as a fallback baseline when the file doesn't exist in git HEAD
        /// (e.g., in worktrees created from dirty state).
        previous_content: Option<String>,
    },

    /// fs_notify detected a file change - check if we should track/update
    HandleFileChange { path: PathBuf },

    /// fs_notify detected file deletion
    HandleFileDeleted { path: PathBuf },

    /// Refresh git dirty cache (called periodically)
    RefreshGitDirtyCache,

    /// Reset baseline after commit
    ResetBaseline { path: PathBuf },

    /// Set tracking mode
    SetMode { mode: TrackingMode },

    /// Re-root the actor after a session cwd remount (path virtualization).
    /// Reply fires after `file_states` keys have been rewritten so callers
    /// can wait before recording writes under the new cwd.
    SetWorkingDir {
        working_dir: PathBuf,
        reply: oneshot::Sender<()>,
    },

    // === Action Commands (accept/reject hunks) ===
    /// Apply action (accept/reject) to a specific hunk
    HunkAction {
        hunk_id: HunkId,
        action: HunkAction,
        reply: oneshot::Sender<Result<(), HunkActionError>>,
    },

    /// Apply action (accept/reject) to all hunks for a file
    FileAction {
        path: PathBuf,
        action: HunkAction,
        reply: oneshot::Sender<Result<Vec<HunkId>, HunkActionError>>,
    },

    /// Apply action (accept/reject) to all hunks
    AllAction {
        action: HunkAction,
        reply: oneshot::Sender<Result<Vec<HunkId>, HunkActionError>>,
    },

    /// Apply action (accept/reject) to all hunks for a specific turn
    TurnAction {
        prompt_index: usize,
        action: HunkAction,
        reply: oneshot::Sender<Result<Vec<HunkId>, HunkActionError>>,
    },

    // === Query Commands (request-response via oneshot) ===
    /// Get all current hunks
    GetAllHunks {
        reply: oneshot::Sender<Vec<Arc<Hunk>>>,
    },

    /// Get hunks for a specific path
    GetHunksForPath {
        path: PathBuf,
        reply: oneshot::Sender<Vec<Arc<Hunk>>>,
    },

    /// Get hunks + file content for a specific path (for diff rendering)
    GetFileHunkData {
        path: PathBuf,
        reply: oneshot::Sender<FileHunkData>,
    },

    /// Get hunks filtered by source
    GetHunksBySource {
        source: HunkSourceFilter,
        reply: oneshot::Sender<Vec<Arc<Hunk>>>,
    },

    /// Get a specific hunk by ID
    GetHunk {
        hunk_id: HunkId,
        reply: oneshot::Sender<Option<Arc<Hunk>>>,
    },

    /// Check if a path is being tracked as an agent file
    IsAgentFile {
        path: PathBuf,
        reply: oneshot::Sender<bool>,
    },

    /// Get all tracked file paths (agent + external, regardless of hunk state)
    GetAllTrackedPaths {
        reply: oneshot::Sender<Vec<PathBuf>>,
    },

    /// Get staged file paths (HEAD→index changes from git). Repo-wide in
    /// AllDirty; scoped to tracked paths in AgentOnly.
    GetStagedFiles {
        reply: oneshot::Sender<HashSet<PathBuf>>,
    },

    /// Get baseline, current content, agent flag, and staged flag for every
    /// tracked file in a single in-memory iteration. No async I/O.
    GetAllFileContents {
        reply: oneshot::Sender<Vec<FileContentEntry>>,
    },

    // === Session Summary Commands ===
    /// Get complete session summary (stats + pending turns)
    GetSessionSummary {
        reply: oneshot::Sender<SessionSummary>,
    },

    /// Get pending hunks for a specific turn
    GetTurnHunks {
        prompt_index: usize,
        reply: oneshot::Sender<Vec<Arc<Hunk>>>,
    },

    /// Reset session stats (e.g., after commit)
    ResetStats,

    /// Refresh all baselines from the current git HEAD and re-read current
    /// content from disk. Used after a git HEAD/index change to reconcile stale state.
    RefreshAllBaselines,

    // === Snapshot / Restore Commands (for cross-session sync-back) ===
    /// Take a snapshot of all hunk tracker state for preservation across
    /// session kill/reload cycles.
    SnapshotState {
        reply: oneshot::Sender<HunkTrackerSnapshot>,
    },

    /// Incremental single-turn delta for the rewind checkpoint store.
    SnapshotTurnDelta {
        prompt_index: usize,
        reply: oneshot::Sender<HunkTurnDelta>,
    },

    /// Restore a previously snapshotted state. Replaces all current file
    /// states, turn index, and session stats.
    RestoreState(HunkTrackerSnapshot),
}
