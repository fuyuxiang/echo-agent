//! Handle to communicate with HunkTrackerActor.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::commands::HunkTrackerCommand;
use crate::types::{
    FileContentEntry, FileHunkData, Hunk, HunkAction, HunkActionError, HunkId, HunkSourceFilter,
    HunkTrackerSnapshot, HunkTurnDelta, SessionSummary, TrackingMode,
};

/// Handle to communicate with HunkTrackerActor.
/// This is cheap to clone and can be shared across tasks.
#[derive(Clone)]
pub struct HunkTrackerHandle {
    cmd_tx: mpsc::UnboundedSender<HunkTrackerCommand>,
}

impl HunkTrackerHandle {
    /// Create a new handle with the given command sender.
    pub(crate) fn new(cmd_tx: mpsc::UnboundedSender<HunkTrackerCommand>) -> Self {
        Self { cmd_tx }
    }

    /// Create a no-op handle that discards all commands.
    /// Useful for tests and situations where hunk tracking is not needed.
    pub fn noop() -> Self {
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        // The receiver is dropped immediately, so all sends will return Err
        // but since we use `let _ = send(...)` everywhere, this is fine.
        Self { cmd_tx }
    }

    /// Whether the backing actor has exited (its command receiver was
    /// dropped), observable even while handle clones are alive.
    pub fn is_closed(&self) -> bool {
        self.cmd_tx.is_closed()
    }

    /// Record that an agent tool wrote to a file.
    /// This is fire-and-forget - doesn't wait for processing.
    ///
    /// `previous_content` is the file content before this write (if known).
    /// It is used as a fallback baseline when the file doesn't exist in git HEAD
    /// (e.g., in worktrees created from dirty state).
    pub fn record_agent_write(
        &self,
        path: PathBuf,
        content: String,
        prompt_index: usize,
        previous_content: Option<String>,
    ) {
        let _ = self.cmd_tx.send(HunkTrackerCommand::RecordAgentWrite {
            path,
            content,
            prompt_index,
            previous_content,
        });
    }

    /// Re-root the actor after a session cwd remount.
    ///
    /// Waits until in-memory hunk keys have been rewritten so a later
    /// `record_agent_write` under the new cwd cannot be overwritten by
    /// the remount rekey.
    pub async fn set_working_dir(&self, working_dir: PathBuf) {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(HunkTrackerCommand::SetWorkingDir {
                working_dir,
                reply: reply_tx,
            })
            .is_err()
        {
            return;
        }
        let _ = reply_rx.await;
    }

    /// Notify of file change from fs_notify.
    pub fn handle_file_change(&self, path: PathBuf) {
        let _ = self
            .cmd_tx
            .send(HunkTrackerCommand::HandleFileChange { path });
    }

    /// Notify of file deletion from fs_notify.
    pub fn handle_file_deleted(&self, path: PathBuf) {
        let _ = self
            .cmd_tx
            .send(HunkTrackerCommand::HandleFileDeleted { path });
    }

    /// Refresh git dirty cache.
    pub fn refresh_git_dirty_cache(&self) {
        let _ = self.cmd_tx.send(HunkTrackerCommand::RefreshGitDirtyCache);
    }

    /// Reset baseline for a file (after commit).
    pub fn reset_baseline(&self, path: PathBuf) {
        let _ = self.cmd_tx.send(HunkTrackerCommand::ResetBaseline { path });
    }

    /// Set tracking mode.
    pub fn set_mode(&self, mode: TrackingMode) {
        let _ = self.cmd_tx.send(HunkTrackerCommand::SetMode { mode });
    }

    /// Apply action (accept/reject) to a specific hunk.
    pub async fn hunk_action(
        &self,
        hunk_id: HunkId,
        action: HunkAction,
    ) -> Result<(), HunkActionError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::HunkAction {
            hunk_id: hunk_id.clone(),
            action,
            reply: reply_tx,
        });
        reply_rx
            .await
            .unwrap_or(Err(HunkActionError::HunkNotFound(hunk_id)))
    }

    /// Apply action (accept/reject) to all hunks for a file.
    pub async fn file_action(
        &self,
        path: PathBuf,
        action: HunkAction,
    ) -> Result<Vec<HunkId>, HunkActionError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::FileAction {
            path,
            action,
            reply: reply_tx,
        });
        reply_rx.await.unwrap_or_else(|_| Ok(vec![]))
    }

    /// Apply action (accept/reject) to all hunks.
    pub async fn all_action(&self, action: HunkAction) -> Result<Vec<HunkId>, HunkActionError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::AllAction {
            action,
            reply: reply_tx,
        });
        reply_rx.await.unwrap_or_else(|_| Ok(vec![]))
    }

    /// Apply action (accept/reject) to all hunks for a specific turn.
    pub async fn turn_action(
        &self,
        prompt_index: usize,
        action: HunkAction,
    ) -> Result<Vec<HunkId>, HunkActionError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::TurnAction {
            prompt_index,
            action,
            reply: reply_tx,
        });
        reply_rx.await.unwrap_or_else(|_| Ok(vec![]))
    }

    /// Get all hunks.
    pub async fn get_all_hunks(&self) -> Vec<Arc<Hunk>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(HunkTrackerCommand::GetAllHunks { reply: reply_tx });
        reply_rx.await.unwrap_or_default()
    }

    /// Get hunks for a specific path.
    pub async fn get_hunks_for_path(&self, path: PathBuf) -> Vec<Arc<Hunk>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::GetHunksForPath {
            path,
            reply: reply_tx,
        });
        reply_rx.await.unwrap_or_default()
    }

    /// Get hunks + file content for a specific path (for diff rendering).
    pub async fn get_file_hunk_data(&self, path: PathBuf) -> FileHunkData {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::GetFileHunkData {
            path,
            reply: reply_tx,
        });
        reply_rx.await.unwrap_or_default()
    }

    /// Get hunks by source.
    pub async fn get_hunks_by_source(&self, source: HunkSourceFilter) -> Vec<Arc<Hunk>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::GetHunksBySource {
            source,
            reply: reply_tx,
        });
        reply_rx.await.unwrap_or_default()
    }

    /// Get a specific hunk by ID.
    pub async fn get_hunk(&self, hunk_id: HunkId) -> Option<Arc<Hunk>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::GetHunk {
            hunk_id,
            reply: reply_tx,
        });
        reply_rx.await.ok().flatten()
    }

    /// Get all tracked file paths (agent + external), regardless of hunk state.
    ///
    /// Returns every path the hunk tracker knows about — agent writes,
    /// fs_notify-detected external edits, and git-dirty files (in `AllDirty`
    /// mode). Entries persist after the user accepts/rejects every hunk.
    ///
    /// Use this for worktree replication where ALL changes matter, not just
    /// agent-attributed ones (the agent may have created files via terminal
    /// commands like `echo`, `cp`, `mv`, etc.).
    pub async fn get_all_tracked_paths(&self) -> Vec<PathBuf> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(HunkTrackerCommand::GetAllTrackedPaths { reply: reply_tx });
        reply_rx.await.unwrap_or_default()
    }

    /// Get staged file paths (absolute) — files with HEAD→index changes in
    /// git. In AllDirty mode this is repo-wide; in AgentOnly mode the
    /// underlying scan is scoped to tracked paths, so only their staged
    /// state is reported.
    pub async fn get_staged_files(&self) -> HashSet<PathBuf> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(HunkTrackerCommand::GetStagedFiles { reply: reply_tx });
        reply_rx.await.unwrap_or_default()
    }

    /// Get baseline, current content, agent flag, and staged flag for every
    /// tracked file in a single call. Pure in-memory — no git I/O.
    pub async fn get_all_file_contents(&self) -> Vec<FileContentEntry> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(HunkTrackerCommand::GetAllFileContents { reply: reply_tx });
        reply_rx.await.unwrap_or_default()
    }

    /// Check if path is an agent file.
    pub async fn is_agent_file(&self, path: PathBuf) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::IsAgentFile {
            path,
            reply: reply_tx,
        });
        reply_rx.await.unwrap_or(false)
    }

    /// Get complete session summary (stats + pending turns).
    pub async fn get_session_summary(&self) -> SessionSummary {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(HunkTrackerCommand::GetSessionSummary { reply: reply_tx });
        reply_rx.await.unwrap_or_default()
    }

    /// Get pending hunks for a specific turn.
    pub async fn get_turn_hunks(&self, prompt_index: usize) -> Vec<Arc<Hunk>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::GetTurnHunks {
            prompt_index,
            reply: reply_tx,
        });
        reply_rx.await.unwrap_or_default()
    }

    /// Reset session stats (e.g., after commit).
    pub fn reset_stats(&self) {
        let _ = self.cmd_tx.send(HunkTrackerCommand::ResetStats);
    }

    /// Refresh all baselines from the current git HEAD and re-read current
    /// content from disk. Call this after a git HEAD/index change to
    /// reconcile stale baselines and fix phantom "file deleted" hunks.
    pub fn refresh_all_baselines(&self) {
        let _ = self.cmd_tx.send(HunkTrackerCommand::RefreshAllBaselines);
    }

    /// Take a snapshot of all hunk tracker state for preservation across
    /// session kill/reload cycles (e.g., fork sync-back).
    ///
    /// Returns `None` if the actor has been shut down.
    pub async fn snapshot_state(&self) -> Option<HunkTrackerSnapshot> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self
            .cmd_tx
            .send(HunkTrackerCommand::SnapshotState { reply: reply_tx });
        reply_rx.await.ok()
    }

    /// Incremental single-turn delta for `prompt_index`: snapshots of the files
    /// touched that turn plus its hunk-id set. Per-prompt counterpart to
    /// [`snapshot_state`](Self::snapshot_state). `None` if the actor is shut down.
    pub async fn snapshot_turn_delta(&self, prompt_index: usize) -> Option<HunkTurnDelta> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(HunkTrackerCommand::SnapshotTurnDelta {
            prompt_index,
            reply: reply_tx,
        });
        reply_rx.await.ok()
    }

    /// Restore a previously snapshotted state. Replaces all current file
    /// states, turn index, and session stats in the actor.
    ///
    /// This is fire-and-forget — doesn't wait for processing.
    pub fn restore_state(&self, snapshot: HunkTrackerSnapshot) {
        let _ = self.cmd_tx.send(HunkTrackerCommand::RestoreState(snapshot));
    }
}
