//! HunkTrackerActor - runs in a dedicated tokio task and owns all state.
//!
//! This module is organized into submodules by responsibility:
//! - `state`: Internal state types (GitRepoState, FileHunkState)
//! - `git`: Git operations (refresh_git_dirty_cache, read_baseline)
//! - `mutations`: File change handlers (record_agent_write, handle_file_change, etc.)
//! - `actions`: Hunk actions (apply_hunk_action, apply_file_action, etc.)
//! - `queries`: Read-only queries (get_all_hunks, get_hunks_for_path, etc.)
//! - `hunks`: Hunk recomputation and diff events
//! - `file_utils`: Safe file reading with binary/UTF-8 detection

mod actions;
mod file_utils;
mod git;
mod hunks;
mod mutations;
mod queries;
pub(crate) mod state;

pub use mutations::{REFRESH_SCAN_LOG_PREFIX, REFRESH_SKIP_LOG_PREFIX};

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::commands::HunkTrackerCommand;
use crate::events::HunkEvent;
use crate::handle::HunkTrackerHandle;
use crate::types::{
    FileContentEntry, FileContentView, FileHunkStateSnapshot, HunkId, HunkTrackerSnapshot,
    HunkTurnDelta, SessionStats, TrackingMode,
};

use state::{FileHunkState, GitRepoState, RepoSyncState};

/// Coalesced action for a single path within a batch.
#[derive(Debug, PartialEq)]
pub(crate) enum CoalescedPathAction {
    Changed,
    Deleted,
    DeletedThenChanged,
}

/// A batch of coalesced commands drained from the channel.
#[derive(Debug)]
pub(crate) struct CoalescedBatch {
    pub(crate) file_actions: HashMap<PathBuf, CoalescedPathAction>,
    pub(crate) refresh_all: bool,
    pub(crate) refresh_dirty: bool,
    pub(crate) other_commands: Vec<HunkTrackerCommand>,
    pub(crate) command_count: usize,
}

impl CoalescedBatch {
    pub(crate) fn new() -> Self {
        Self {
            file_actions: HashMap::new(),
            refresh_all: false,
            refresh_dirty: false,
            other_commands: Vec::new(),
            command_count: 0,
        }
    }

    pub(crate) fn add(&mut self, cmd: HunkTrackerCommand) {
        self.command_count += 1;
        match cmd {
            HunkTrackerCommand::HandleFileChange { path } => {
                self.file_actions
                    .entry(path)
                    .and_modify(|action| {
                        if let CoalescedPathAction::Deleted = action {
                            *action = CoalescedPathAction::DeletedThenChanged;
                        }
                    })
                    .or_insert(CoalescedPathAction::Changed);
            }
            HunkTrackerCommand::HandleFileDeleted { path } => {
                self.file_actions
                    .entry(path)
                    .and_modify(|action| *action = CoalescedPathAction::Deleted)
                    .or_insert(CoalescedPathAction::Deleted);
            }
            HunkTrackerCommand::RefreshAllBaselines => {
                self.refresh_all = true;
            }
            HunkTrackerCommand::RefreshGitDirtyCache => {
                self.refresh_dirty = true;
            }
            _ => {
                self.other_commands.push(cmd);
            }
        }
    }
}

/// The actor that owns all hunk tracking state.
/// Runs in a dedicated tokio task and processes commands sequentially.
pub struct HunkTrackerActor {
    /// Session ID for this hunk tracker instance
    #[allow(dead_code)]
    session_id: String,

    /// Working directory (repo root)
    working_dir: PathBuf,

    /// Unified map for all tracked files.
    /// Key: absolute path
    /// Value: file state including is_agent_file flag
    file_states: HashMap<PathBuf, FileHunkState>,

    /// Secondary index: prompt_index -> set of hunk IDs for that turn.
    /// Enables O(1) lookup for `get_hunks_for_turn`.
    turn_index: HashMap<usize, HashSet<HunkId>>,

    /// Cached set of git dirty file paths (refreshed periodically).
    /// Repo-wide in AllDirty; in AgentOnly the refresh scan is scoped to
    /// tracked paths, so only their state is cached.
    git_dirty_cache: HashSet<PathBuf>,

    /// Cached set of git staged file paths (HEAD→index changes, refreshed
    /// with — and scoped like — the dirty cache).
    git_staged_cache: HashSet<PathBuf>,

    /// Cached git repository discovery state
    git_repo_state: GitRepoState,

    /// Cached git HEAD/index state for baseline refreshes
    repo_sync_state: RepoSyncState,

    /// Channel to receive commands
    cmd_rx: mpsc::UnboundedReceiver<HunkTrackerCommand>,

    /// Channel to send hunk events to clients
    event_tx: mpsc::UnboundedSender<HunkEvent>,

    /// Current tracking mode
    mode: TrackingMode,

    /// Session-level stats for accepted/rejected hunks.
    /// Reset when all baselines are reset (e.g., after commit).
    session_stats: SessionStats,

    // Cancellation token which can cancel the ongoing loop
    cancellation_token: tokio_util::sync::CancellationToken,
}

impl HunkTrackerActor {
    /// Send an event to subscribers, logging if the channel is closed.
    fn send_event(&self, event: HunkEvent) {
        if self.event_tx.send(event).is_err() {
            debug!("Event channel closed, event dropped");
        }
    }

    /// Construct an actor with its initial state. The single construction
    /// site shared by [`spawn`](Self::spawn) and tests, so tests exercise
    /// exactly the state production starts from.
    fn new(
        session_id: String,
        working_dir: PathBuf,
        cmd_rx: mpsc::UnboundedReceiver<HunkTrackerCommand>,
        event_tx: mpsc::UnboundedSender<HunkEvent>,
        mode: TrackingMode,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Self {
        HunkTrackerActor {
            session_id,
            working_dir,
            file_states: HashMap::new(),
            turn_index: HashMap::new(),
            git_dirty_cache: HashSet::new(),
            git_staged_cache: HashSet::new(),
            git_repo_state: GitRepoState::Unknown,
            repo_sync_state: RepoSyncState::default(),
            cmd_rx,
            event_tx,
            mode,
            session_stats: SessionStats::default(),
            cancellation_token,
        }
    }

    /// Spawn the actor and return a handle to communicate with it.
    ///
    /// If `mode` is `AllDirty`, the actor automatically loads all uncommitted
    /// git changes at startup.
    pub fn spawn(
        session_id: String,
        working_dir: PathBuf,
        event_tx: mpsc::UnboundedSender<HunkEvent>,
        mode: TrackingMode,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> HunkTrackerHandle {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let actor = Self::new(
            session_id,
            working_dir,
            cmd_rx,
            event_tx,
            mode,
            cancellation_token,
        );

        // Spawn the actor task
        tokio::spawn(actor.run());

        HunkTrackerHandle::new(cmd_tx)
    }

    /// Returns true if a command can be coalesced into a batch.
    pub(crate) fn is_coalescable(cmd: &HunkTrackerCommand) -> bool {
        matches!(
            cmd,
            HunkTrackerCommand::HandleFileChange { .. }
                | HunkTrackerCommand::HandleFileDeleted { .. }
                | HunkTrackerCommand::RefreshAllBaselines
                | HunkTrackerCommand::RefreshGitDirtyCache
        )
    }

    /// Drain all queued commands from the channel and coalesce them with `first`.
    fn drain_and_coalesce(&mut self, first: HunkTrackerCommand) -> CoalescedBatch {
        let mut batch = CoalescedBatch::new();
        batch.add(first);

        while let Ok(cmd) = self.cmd_rx.try_recv() {
            batch.add(cmd);
        }

        batch
    }

    /// Process a coalesced batch of commands. Returns false if actor should shut down.
    async fn handle_coalesced_batch(&mut self, batch: CoalescedBatch) -> bool {
        let file_count = batch.file_actions.len();
        let other_count = batch.other_commands.len();
        let action_count =
            file_count + batch.refresh_all as usize + batch.refresh_dirty as usize + other_count;

        if batch.command_count > 1 {
            debug!(
                commands = batch.command_count,
                actions = action_count,
                files = file_count,
                refresh_all = batch.refresh_all,
                refresh_dirty = batch.refresh_dirty,
                other = other_count,
                "coalesced batch",
            );
        }

        if batch.refresh_dirty && !batch.refresh_all {
            self.refresh_git_dirty_cache(None).await;
        }

        if batch.refresh_all {
            let mut already_processed = std::collections::HashSet::new();
            for (path, action) in batch.file_actions {
                match action {
                    CoalescedPathAction::Changed | CoalescedPathAction::DeletedThenChanged => {
                        if !self.file_states.contains_key(&path) {
                            self.handle_file_change(path.clone()).await;
                            already_processed.insert(path);
                        }
                    }
                    CoalescedPathAction::Deleted => {
                        if self.file_states.contains_key(&path) {
                            self.handle_file_deleted(path).await;
                        }
                    }
                }
            }
            self.refresh_all_baselines_except(&already_processed).await;
        } else {
            let mut changed_paths = Vec::new();
            for (path, action) in batch.file_actions {
                match action {
                    CoalescedPathAction::Changed | CoalescedPathAction::DeletedThenChanged => {
                        changed_paths.push(path);
                    }
                    CoalescedPathAction::Deleted => {
                        self.handle_file_deleted(path).await;
                    }
                }
            }
            self.handle_file_changes_batch(changed_paths).await;
        }

        for cmd in batch.other_commands {
            if !self.handle_command(cmd).await {
                return false;
            }
        }
        true
    }

    /// Main actor loop - processes commands until shutdown or cancellation
    async fn run(mut self) {
        // If AllDirty mode, load all uncommitted git changes at startup
        if self.mode == TrackingMode::AllDirty {
            self.refresh_git_dirty_cache(None).await;
        }

        loop {
            tokio::select! {
                biased;
                _ = self.cancellation_token.cancelled() => {
                    break;
                }
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        break;
                    };

                    if Self::is_coalescable(&cmd) {
                        let batch = self.drain_and_coalesce(cmd);
                        if !self.handle_coalesced_batch(batch).await {
                            break;
                        }
                    } else if !self.handle_command(cmd).await {
                        break;
                    }
                }
            }
        }
    }

    /// Handle a command. Returns false if actor should shut down.
    async fn handle_command(&mut self, cmd: HunkTrackerCommand) -> bool {
        debug!("HunkTrackerAction:: {:?}", cmd);
        match cmd {
            HunkTrackerCommand::RecordAgentWrite {
                path,
                content,
                prompt_index,
                previous_content,
            } => {
                self.record_agent_write(path, content, prompt_index, previous_content)
                    .await;
            }
            HunkTrackerCommand::HandleFileChange { path } => {
                self.handle_file_change(path).await;
            }
            HunkTrackerCommand::HandleFileDeleted { path } => {
                self.handle_file_deleted(path).await;
            }
            HunkTrackerCommand::RefreshGitDirtyCache => {
                // Public command contract: always a full-worktree scan.
                self.refresh_git_dirty_cache(None).await;
            }
            HunkTrackerCommand::ResetBaseline { path } => {
                self.reset_baseline(&path);
            }
            HunkTrackerCommand::SetMode { mode } => {
                self.set_mode(mode).await;
            }
            HunkTrackerCommand::SetWorkingDir { working_dir, reply } => {
                self.rekey_paths_for_working_dir(&working_dir);
                self.working_dir = working_dir;
                self.git_repo_state = GitRepoState::Unknown;
                self.repo_sync_state = RepoSyncState::default();
                let _ = reply.send(());
            }
            HunkTrackerCommand::HunkAction {
                hunk_id,
                action,
                reply,
            } => {
                let result = self.apply_hunk_action(&hunk_id, action).await;
                if reply.send(result).is_err() {
                    debug!("HunkAction reply channel dropped");
                }
            }
            HunkTrackerCommand::FileAction {
                path,
                action,
                reply,
            } => {
                let affected = self.apply_file_action(&path, action).await;
                if reply.send(affected).is_err() {
                    debug!("FileAction reply channel dropped");
                }
            }
            HunkTrackerCommand::AllAction { action, reply } => {
                let affected = self.apply_all_action(action).await;
                if reply.send(affected).is_err() {
                    debug!("AllAction reply channel dropped");
                }
            }
            HunkTrackerCommand::TurnAction {
                prompt_index,
                action,
                reply,
            } => {
                let affected = self.apply_turn_action(prompt_index, action).await;
                if reply.send(affected).is_err() {
                    debug!("TurnAction reply channel dropped");
                }
            }
            HunkTrackerCommand::GetAllHunks { reply } => {
                let hunks = self.get_all_hunks();
                if reply.send(hunks).is_err() {
                    debug!("GetAllHunks reply channel dropped");
                }
            }
            HunkTrackerCommand::GetHunksForPath { path, reply } => {
                let hunks = self.get_hunks_for_path(&path);
                if reply.send(hunks).is_err() {
                    debug!("GetHunksForPath reply channel dropped");
                }
            }
            HunkTrackerCommand::GetFileHunkData { path, reply } => {
                let data = self.get_file_hunk_data(&path);
                if reply.send(data).is_err() {
                    debug!("GetFileHunkData reply channel dropped");
                }
            }
            HunkTrackerCommand::GetHunksBySource { source, reply } => {
                let hunks = self.get_hunks_by_source(source);
                if reply.send(hunks).is_err() {
                    debug!("GetHunksBySource reply channel dropped");
                }
            }
            HunkTrackerCommand::GetHunk { hunk_id, reply } => {
                let hunk = self.get_hunk(&hunk_id);
                if reply.send(hunk).is_err() {
                    debug!("GetHunk reply channel dropped");
                }
            }
            HunkTrackerCommand::IsAgentFile { path, reply } => {
                let is_agent = self
                    .file_states
                    .get(&path)
                    .map(|s| s.is_agent_file)
                    .unwrap_or(false);
                if reply.send(is_agent).is_err() {
                    debug!("IsAgentFile reply channel dropped");
                }
            }
            HunkTrackerCommand::GetAllTrackedPaths { reply } => {
                let paths = self.get_all_tracked_paths();
                if reply.send(paths).is_err() {
                    debug!("GetAllTrackedPaths reply channel dropped");
                }
            }
            HunkTrackerCommand::GetStagedFiles { reply } => {
                let staged: HashSet<PathBuf> = self
                    .git_staged_cache
                    .iter()
                    .map(|rel| self.working_dir.join(rel))
                    .collect();
                if reply.send(staged).is_err() {
                    debug!("GetStagedFiles reply channel dropped");
                }
            }
            HunkTrackerCommand::GetAllFileContents { reply } => {
                let entries: Vec<FileContentEntry> = self
                    .file_states
                    .iter()
                    .map(|(abs_path, state)| {
                        let rel = abs_path.strip_prefix(&self.working_dir).unwrap_or(abs_path);
                        let staged = self.git_staged_cache.contains(rel);
                        FileContentEntry {
                            path: abs_path.clone(),
                            baseline: FileContentView::from_content_state(&state.baseline),
                            current: FileContentView::from_content_state(&state.current_content),
                            is_agent_file: state.is_agent_file,
                            staged,
                        }
                    })
                    .collect();
                if reply.send(entries).is_err() {
                    debug!("GetAllFileContents reply channel dropped");
                }
            }
            HunkTrackerCommand::GetSessionSummary { reply } => {
                let summary = self.compute_session_summary();
                if reply.send(summary).is_err() {
                    debug!("GetSessionSummary reply channel dropped");
                }
            }
            HunkTrackerCommand::GetTurnHunks {
                prompt_index,
                reply,
            } => {
                let hunks = self.get_hunks_for_turn(prompt_index);
                if reply.send(hunks).is_err() {
                    debug!("GetTurnHunks reply channel dropped");
                }
            }
            HunkTrackerCommand::ResetStats => {
                self.session_stats = SessionStats::default();
            }
            HunkTrackerCommand::RefreshAllBaselines => {
                self.refresh_all_baselines().await;
            }
            HunkTrackerCommand::SnapshotState { reply } => {
                let snapshot = self.take_snapshot();
                if reply.send(snapshot).is_err() {
                    debug!("SnapshotState reply channel dropped");
                }
            }
            HunkTrackerCommand::SnapshotTurnDelta {
                prompt_index,
                reply,
            } => {
                let delta = self.take_turn_delta(prompt_index);
                if reply.send(delta).is_err() {
                    debug!("SnapshotTurnDelta reply channel dropped");
                }
            }
            HunkTrackerCommand::RestoreState(snapshot) => {
                self.restore_snapshot(snapshot);
            }
        }
        true
    }

    /// Remount rebases in-memory hunk keys onto the new guest root so
    /// accept/reject still target the live tree instead of the pre-virt cwd.
    fn rekey_paths_for_working_dir(&mut self, new_cwd: &Path) {
        if new_cwd == self.working_dir {
            return;
        }
        let old = self.working_dir.clone();
        let rewrite = |path: PathBuf| {
            // Virt remount always moves into a subdirectory of `old`. Paths
            // already under the new guest root must not be re-prefixed.
            if path.starts_with(new_cwd) {
                return path;
            }
            crate::types::rewrite_single_path(&path, &old, &old, new_cwd).unwrap_or(path)
        };
        let rekey_state = |mut state: FileHunkState, new_path: &Path| {
            state.hunks = state
                .hunks
                .into_iter()
                .map(|hunk| {
                    let mut owned = (*hunk).clone();
                    owned.path = new_path.to_path_buf();
                    Arc::new(owned)
                })
                .collect();
            state
        };
        // Insert already-guest keys first so a collision with a rewritten
        // `/workspace/foo` keeps the `/workspace/<conv>/foo` baseline.
        let pairs: Vec<_> = self.file_states.drain().collect();
        let (already, rewritten): (Vec<_>, Vec<_>) = pairs
            .into_iter()
            .partition(|(path, _)| path.starts_with(new_cwd));
        let mut next = HashMap::new();
        for (path, state) in already {
            let new_path = rewrite(path);
            next.insert(new_path.clone(), rekey_state(state, &new_path));
        }
        for (path, state) in rewritten {
            let new_path = rewrite(path);
            let state = rekey_state(state, &new_path);
            match next.entry(new_path.clone()) {
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(state);
                }
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    warn!(
                        path = %new_path.display(),
                        "hunk rekey collision; merging remounted file state"
                    );
                    let dest = o.get_mut();
                    dest.hunks.extend(state.hunks);
                    dest.is_agent_file |= state.is_agent_file;
                    dest.baseline_accepted |= state.baseline_accepted;
                }
            }
        }
        self.file_states = next;
        self.git_dirty_cache = self.git_dirty_cache.drain().map(rewrite).collect();
        self.git_staged_cache = self.git_staged_cache.drain().map(rewrite).collect();
    }

    /// Snapshot one file's state (full FileContentState incl. Binary/TooLarge).
    /// Shared by [`take_snapshot`](Self::take_snapshot) and
    /// [`take_turn_delta`](Self::take_turn_delta) so they can't diverge.
    fn snapshot_file_state(state: &FileHunkState) -> FileHunkStateSnapshot {
        FileHunkStateSnapshot {
            baseline: state.baseline.clone(),
            current_content: state.current_content.clone(),
            hunks: state.hunks.iter().map(|h| (**h).clone()).collect(),
            is_agent_file: state.is_agent_file,
            baseline_accepted: state.baseline_accepted,
        }
    }

    /// Take a snapshot of all hunk tracker state for preservation across
    /// session kill/reload cycles.
    /// Preserves the full FileContentState (including Binary/TooLarge) for correctness
    /// in fork and cross-session sync flows.
    fn take_snapshot(&self) -> HunkTrackerSnapshot {
        let file_states = self
            .file_states
            .iter()
            .map(|(path, state)| (path.clone(), Self::snapshot_file_state(state)))
            .collect();

        HunkTrackerSnapshot {
            file_states,
            turn_index: self.turn_index.clone(),
            session_stats: self.session_stats.clone(),
        }
    }

    /// Incremental single-turn delta for `prompt_index`: snapshots of the files
    /// owning this turn's hunks plus the hunk-id set. Unlike
    /// [`take_snapshot`](Self::take_snapshot), never copies the whole tracker.
    fn take_turn_delta(&self, prompt_index: usize) -> HunkTurnDelta {
        let hunk_ids = self
            .turn_index
            .get(&prompt_index)
            .cloned()
            .unwrap_or_default();
        let file_states = self
            .file_states
            .iter()
            .filter(|(_, state)| state.hunks.iter().any(|h| hunk_ids.contains(&h.id)))
            .map(|(path, state)| (path.clone(), Self::snapshot_file_state(state)))
            .collect();
        HunkTurnDelta {
            prompt_index,
            file_states,
            hunk_ids,
        }
    }

    /// Restore a previously snapshotted state, replacing all current file
    /// states, turn index, and session stats.
    /// Preserves the full FileContentState (including Binary/TooLarge).
    fn restore_snapshot(&mut self, snapshot: HunkTrackerSnapshot) {
        self.file_states = snapshot
            .file_states
            .into_iter()
            .map(|(path, snap)| {
                // Preserve full FileContentState as-is
                let state = FileHunkState {
                    baseline: snap.baseline,
                    current_content: snap.current_content,
                    hunks: snap.hunks.into_iter().map(std::sync::Arc::new).collect(),
                    is_agent_file: snap.is_agent_file,
                    baseline_accepted: snap.baseline_accepted,
                };
                (path, state)
            })
            .collect();
        self.turn_index = snapshot.turn_index;
        self.session_stats = snapshot.session_stats;

        // TODO: Re-emit HunkEvent::FileAdded / HunkEvent::HunkAdded for
        // all restored files and hunks so that connected clients (TUI, VSCode
        // extension) see the restored state without requiring a manual refresh.
        // Alternative: emit a single HunkEvent::StateRestored { file_count }
        // that clients use as a signal to do a full refresh.

        debug!(
            files = self.file_states.len(),
            turns = self.turn_index.len(),
            "Hunk tracker state restored from snapshot"
        );
    }
}
