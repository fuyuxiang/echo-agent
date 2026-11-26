//! Turn-boundary fan-out for the workspace.
//!
//! [`WorkspaceHandle::on_turn_boundary`] is the single internal entry point for
//! turn/prompt boundaries.
//!
//! A rewind checkpoint is keyed by `prompt_index` and bundles per-domain state
//! (filesystem [`RewindPoint`], optional hunk delta, optional git HEAD/index);
//! restore reverts all enabled domains together.
use crate::handle::WorkspaceHandle;
use crate::session::WorkspaceSession;
use crate::session::file_state::{FileRewindResponse, RewindPoint, rewind_files};
use crate::session::git;
use echo_agent_hunk_tracker::{HunkId, HunkTrackerSnapshot, HunkTurnDelta};
use echo_agent_tool_protocol::turn_hook::TurnHookOutcome;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
/// A turn/prompt boundary routed through [`WorkspaceHandle::on_turn_boundary`].
///
/// `prompt_index` selects the origin and keeps the two effect sets disjoint:
/// - `None` — a turn hook (`on_before_turn`/`on_after_turn`).
/// - `Some(idx)` — a rewind RPC arm (`begin_prompt`/`end_prompt`).
pub(crate) enum TurnBoundary {
    /// Turn start.
    Start {
        prompt_index: Option<usize>,
        turn_number: u64,
    },
    /// Turn end.
    End {
        prompt_index: Option<usize>,
        turn_number: u64,
        duration_ms: u64,
        outcome: TurnHookOutcome,
        written: Vec<String>,
    },
}
impl TurnBoundary {
    pub(crate) fn turn_start(turn_number: u64) -> Self {
        Self::Start {
            prompt_index: None,
            turn_number,
        }
    }
    /// Turn-hook end (from `on_after_turn`): activity + upload + rootfs snapshot.
    pub(crate) fn turn_end(
        turn_number: u64,
        duration_ms: u64,
        outcome: TurnHookOutcome,
        written: Vec<String>,
    ) -> Self {
        Self::End {
            prompt_index: None,
            turn_number,
            duration_ms,
            outcome,
            written,
        }
    }
    /// Rewind begin (from the `begin_prompt` RPC arm): FS-rewind only.
    ///
    /// The RPC carries no turn metadata; `turn ≈ prompt`, so `turn_number`
    /// mirrors `prompt_index` and the dispatcher ignores it on this path.
    pub(crate) fn rewind_begin(prompt_index: usize) -> Self {
        Self::Start {
            prompt_index: Some(prompt_index),
            turn_number: prompt_index as u64,
        }
    }
    /// Rewind finalize (from the `end_prompt` RPC arm): FS-rewind only.
    ///
    /// As with [`Self::rewind_begin`], the turn-hook fields are inert on this
    /// path and are never read by the dispatcher.
    pub(crate) fn rewind_finalize(prompt_index: usize) -> Self {
        Self::End {
            prompt_index: Some(prompt_index),
            turn_number: prompt_index as u64,
            duration_ms: 0,
            outcome: TurnHookOutcome::Completed,
            written: Vec::new(),
        }
    }
}
/// A per-prompt rewind checkpoint: the FS rewind point bundled with the
/// incremental hunk-tracker delta for the same `prompt_index`. Assembled on
/// demand by [`WorkspaceSession::get_checkpoint`]; serialized to disk by the
/// [`CheckpointStore`](crate::session::checkpoint_store::CheckpointStore).
///
/// Optional domain fields use `#[serde(default)]` so the schema stays additive:
/// a blob written before a later field existed still deserializes (field `None`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewindCheckpoint {
    /// The prompt this checkpoint belongs to.
    pub prompt_index: usize,
    /// Filesystem before/after snapshots for the prompt.
    pub fs: RewindPoint,
    /// Incremental hunk delta. `None` when capture was off or the turn touched
    /// no tracked files.
    #[serde(default)]
    pub hunks: Option<HunkTurnDelta>,
}
/// Resolve `workspace_rewind_hunks` from `ECHO_AGENT_WORKSPACE_REWIND_HUNKS` (default off).
pub(crate) fn rewind_hunks_enabled() -> bool {
    echo_agent_config::env_bool("ECHO_AGENT_WORKSPACE_REWIND_HUNKS").unwrap_or(false)
}
/// Resolve `workspace_rewind_durable` from `ECHO_AGENT_WORKSPACE_REWIND_DURABLE`
/// (default off). Off ⇒ the legacy in-memory-only path with no disk I/O.
pub(crate) fn rewind_durable_enabled() -> bool {
    echo_agent_config::env_bool("ECHO_AGENT_WORKSPACE_REWIND_DURABLE").unwrap_or(false)
}
impl WorkspaceSession {
    /// Capture the hunk delta for `prompt_index` into the in-memory store.
    /// Last-write-wins, so repeated finalizes (e.g. `ForceContinue`) are
    /// idempotent. Empty deltas are skipped to avoid a stale `turn_index` entry.
    /// Returns `true` when a delta was stored, so callers count a hunk capture.
    pub(crate) async fn capture_hunk_delta(&self, prompt_index: usize) -> bool {
        let Some(delta) = self.hunk_tracker.snapshot_turn_delta(prompt_index).await else {
            return false;
        };
        if delta.file_states.is_empty() {
            return false;
        }
        self.hunk_checkpoints
            .lock()
            .await
            .insert(prompt_index, delta);
        true
    }
    /// Record the metrics shared by both finalize paths: FS capture + finalize
    /// counters and FS capture duration, plus the git-domain capture when git
    /// state was recorded at begin. Git is captured at begin (outcome unknown),
    /// so it's *counted* here where `outcome` is known. Hunk is recorded by the
    /// caller on whichever finalize path captured it.
    async fn record_finalize_metrics(
        &self,
        prompt_index: usize,
        outcome: TurnHookOutcome,
        fs_capture_seconds: f64,
    ) {
        crate::handle::record_fs_finalize(outcome, fs_capture_seconds);
        if git::git_rewind_enabled() && self.git_checkpoints().contains(prompt_index).await {
            crate::handle::record_rewind_capture(crate::handle::RewindDomain::Git, outcome);
        }
    }
    /// Re-seed the hunk tracker to the **start** of `target_prompt_index` by
    /// composing stored deltas for prompts `< target` (ascending; last write per
    /// path wins) and dropping deltas `>= target`, mirroring the FS rewind.
    ///
    /// No-op when the store is empty, or holds only deltas `>= target` with
    /// `target > 0` (flag enabled mid-session; start-of-target can't be rebuilt,
    /// so uncaptured live hunks aren't wiped). `target == 0` re-seeds to empty.
    /// Composed `turn_index` ids are pruned to those surviving in the snapshots.
    pub(crate) async fn restore_hunk_checkpoints(&self, target_prompt_index: usize) {
        let mut store = self.hunk_checkpoints.lock().await;
        if store.is_empty() {
            return;
        }
        let mut prompts: Vec<usize> = store
            .keys()
            .copied()
            .filter(|&idx| idx < target_prompt_index)
            .collect();
        if prompts.is_empty() && target_prompt_index > 0 {
            return;
        }
        prompts.sort_unstable();
        let mut file_states: HashMap<
            std::path::PathBuf,
            echo_agent_hunk_tracker::FileHunkStateSnapshot,
        > = HashMap::new();
        let mut turn_index: HashMap<usize, HashSet<HunkId>> = HashMap::new();
        for idx in prompts {
            let delta = &store[&idx];
            for (path, snap) in &delta.file_states {
                file_states.insert(path.clone(), snap.clone());
            }
            turn_index.insert(idx, delta.hunk_ids.clone());
        }
        let present: HashSet<HunkId> = file_states
            .values()
            .flat_map(|state| state.hunks.iter().map(|h| h.id.clone()))
            .collect();
        turn_index.retain(|_, ids| {
            ids.retain(|id| present.contains(id));
            !ids.is_empty()
        });
        let session_stats = self.hunk_tracker.get_session_summary().await.stats;
        self.hunk_tracker.restore_state(HunkTrackerSnapshot {
            file_states,
            turn_index,
            session_stats,
        });
        store.retain(|&idx, _| idx < target_prompt_index);
    }
    /// Assemble the [`RewindCheckpoint`] for `prompt_index` from the live FS
    /// rewind point and stored hunk delta. `None` when no FS rewind point exists.
    pub async fn get_checkpoint(&self, prompt_index: usize) -> Option<RewindCheckpoint> {
        let fs = self
            .file_state_tracker
            .get_rewind_point(prompt_index)
            .await?;
        let hunks = self
            .hunk_checkpoints
            .lock()
            .await
            .get(&prompt_index)
            .cloned();
        Some(RewindCheckpoint {
            prompt_index,
            fs,
            hunks,
        })
    }
    /// Assemble the checkpoint for `prompt_index` and write it through the durable
    /// [`CheckpointStore`](crate::session::checkpoint_store::CheckpointStore).
    /// Last-write-wins (idempotent across repeated finalizes); no-op when no
    /// checkpoint exists. Mirror only — restore stays in-process.
    pub(crate) async fn persist_checkpoint(&self, prompt_index: usize) {
        let Some(checkpoint) = self.get_checkpoint(prompt_index).await else {
            return;
        };
        self.checkpoint_store.persist(checkpoint).await;
    }
    /// Drop persisted checkpoints `>= target_prompt_index` (cache + disk),
    /// mirroring the FS/hunk truncation so the on-disk mirror stays consistent.
    pub(crate) async fn truncate_checkpoint_store(&self, target_prompt_index: usize) {
        self.checkpoint_store
            .truncate_from(target_prompt_index)
            .await;
    }
}
impl WorkspaceHandle {
    /// Single fan-out for turn/prompt boundaries.
    ///
    /// Keyed on `prompt_index`: turn hooks (`None`) drive activity; rewind RPC
    /// arms (`Some`) drive rewind capture (FS, plus git/hunks when their flags
    /// are on). `workspace_rewind_all_outcomes` also finalizes the open FS
    /// checkpoint on non-`Completed` turn-ends (gap #2).
    ///
    /// Turn-hook ends return the after-turn enqueue handle for the ack path.
    pub(crate) async fn on_turn_boundary(
        &self,
        session_id: &str,
        boundary: TurnBoundary,
    ) -> Option<tokio::task::JoinHandle<echo_agent_file_utils::queue::EnqueueOutcome>> {
        match boundary {
            TurnBoundary::Start {
                prompt_index: Some(idx),
                ..
            } => {
                if let Some(session) = self.session(session_id) {
                    session.file_state_tracker().begin_prompt(idx).await;
                    if git::git_rewind_enabled() {
                        let started = std::time::Instant::now();
                        if let Some(state) = git::capture_git_state(session.cwd()).await {
                            session.git_checkpoints().record(idx, state).await;
                            crate::handle::observe_rewind_capture_duration(
                                crate::handle::RewindDomain::Git,
                                started.elapsed().as_secs_f64(),
                            );
                        }
                    }
                }
                None
            }
            TurnBoundary::Start {
                prompt_index: None,
                turn_number,
            } => {
                self.shared
                    .activity_tracker
                    .turn_started(session_id, turn_number);
                None
            }
            TurnBoundary::End {
                prompt_index: Some(idx),
                outcome,
                ..
            } => {
                if let Some(session) = self.session(session_id) {
                    let tracker = session.file_state_tracker();
                    let finalized = tracker.current_prompt_index().await == Some(idx);
                    let fs_started = std::time::Instant::now();
                    tracker.end_prompt(session.async_fs(), idx).await;
                    let fs_elapsed = fs_started.elapsed().as_secs_f64();
                    let mut hunk_captured = false;
                    let mut hunk_capture_secs = 0.0_f64;
                    if rewind_hunks_enabled() {
                        let hunk_started = std::time::Instant::now();
                        hunk_captured = session.capture_hunk_delta(idx).await;
                        hunk_capture_secs = hunk_started.elapsed().as_secs_f64();
                    }
                    if rewind_durable_enabled() {
                        session.persist_checkpoint(idx).await;
                    }
                    if finalized {
                        session
                            .record_finalize_metrics(idx, outcome, fs_elapsed)
                            .await;
                        if hunk_captured {
                            crate::handle::observe_rewind_capture_duration(
                                crate::handle::RewindDomain::Hunk,
                                hunk_capture_secs,
                            );
                            crate::handle::record_rewind_capture(
                                crate::handle::RewindDomain::Hunk,
                                outcome,
                            );
                        }
                    }
                }
                None
            }
            TurnBoundary::End {
                prompt_index: None,
                turn_number,
                duration_ms,
                outcome,
                written,
            } => {
                self.shared
                    .activity_tracker
                    .turn_completed(session_id, turn_number, duration_ms);
                if let Some(session) = self.session(session_id) {
                    let roots = match crate::workspace_ops::materialized_git_roots(self).await {
                        Ok(r) if !r.is_empty() => r,
                        _ => vec![session.cwd().to_path_buf()],
                    };
                    for root in roots {
                        let on_conv_branch = git::get_branch(&root)
                            .await
                            .is_some_and(|b| b.starts_with("conv/"));
                        if on_conv_branch {
                            git::commit_turn_if_dirty(&root, turn_number).await;
                        }
                    }
                }
                let handle = {
                    let _ = written;
                    None
                };
                if self.shared.workspace_rewind_all_outcomes
                    && outcome != TurnHookOutcome::Completed
                    && let Some(session) = self.session(session_id)
                {
                    let tracker = session.file_state_tracker();
                    if let Some(idx) = tracker.current_prompt_index().await {
                        let fs_started = std::time::Instant::now();
                        tracker.end_prompt(session.async_fs(), idx).await;
                        let fs_elapsed = fs_started.elapsed().as_secs_f64();
                        let mut hunk_captured = false;
                        let mut hunk_capture_secs = 0.0_f64;
                        if rewind_hunks_enabled() {
                            let hunk_started = std::time::Instant::now();
                            hunk_captured = session.capture_hunk_delta(idx).await;
                            hunk_capture_secs = hunk_started.elapsed().as_secs_f64();
                        }
                        if rewind_durable_enabled() {
                            session.persist_checkpoint(idx).await;
                        }
                        session
                            .record_finalize_metrics(idx, outcome, fs_elapsed)
                            .await;
                        if hunk_captured {
                            crate::handle::observe_rewind_capture_duration(
                                crate::handle::RewindDomain::Hunk,
                                hunk_capture_secs,
                            );
                            crate::handle::record_rewind_capture(
                                crate::handle::RewindDomain::Hunk,
                                outcome,
                            );
                        }
                        crate::handle::record_non_completed_finalize_canary(outcome);
                    }
                }
                handle
            }
        }
    }
    /// User-initiated restore (`workspace.rewind_to` RPC): restore every enabled
    /// domain to before `target_prompt_index`. Ordering matters: git's soft
    /// restore (stash + `reset --soft` + unstage, behind `workspace_rewind_git`)
    /// runs first so its stash-or-abort guard sees live state; then the FS is
    /// reverted; then — only on FS success — git paths are re-staged against the
    /// reverted tree and git/hunk checkpoints `>= target` are dropped. A failed
    /// `rewind_files` keeps all domains for retry. Missing session yields a failed
    /// response, not a panic.
    pub(crate) async fn rewind_to(
        &self,
        session_id: &str,
        target_prompt_index: usize,
    ) -> FileRewindResponse {
        let Some(session) = self.session(session_id) else {
            return FileRewindResponse {
                success: false,
                target_prompt_index,
                reverted_files: Vec::new(),
                clean_files: Vec::new(),
                conflicts: Vec::new(),
                error: Some(format!("workspace session not found: {session_id}")),
            };
        };
        let git_restore = self
            .restore_git_checkpoint(session_id, target_prompt_index)
            .await;
        if let Some((git_outcome, _)) = git_restore.as_ref() {
            if !git_outcome.restored {
                crate::handle::record_rewind_restore(crate::handle::RewindDomain::Git, false);
                tracing::warn!(
                    session_id,
                    target_prompt_index,
                    reason = ?git_outcome.aborted_reason,
                    stash_ref = ?git_outcome.stash_ref,
                    "rewind_to: git domain not restored; filesystem still reverted (partial rewind)"
                );
            } else if let Some(stash_ref) = &git_outcome.stash_ref {
                tracing::info!(
                    session_id,
                    stash_ref = %stash_ref,
                    "rewind_to: git domain restored; pre-rewind changes saved to a stash"
                );
            }
        }
        let response = rewind_files(
            session.file_state_tracker(),
            session.async_fs(),
            target_prompt_index,
        )
        .await;
        crate::handle::record_rewind_restore(crate::handle::RewindDomain::Fs, response.success);
        if response.success {
            if let Some((git_outcome, git_state)) = git_restore.as_ref()
                && git_outcome.restored
            {
                let restaged = git::restage_git_paths(session.cwd(), git_state, session_id).await;
                crate::handle::record_rewind_restore(
                    crate::handle::RewindDomain::Git,
                    git_outcome.index_reset && restaged,
                );
                session
                    .git_checkpoints()
                    .truncate_from(target_prompt_index)
                    .await;
            }
            if git::git_rewind_enabled() && git_restore.is_none() {
                tracing::warn!(
                    session_id,
                    target_prompt_index,
                    "rewind_to: git domain not rewound (no git checkpoint at or before \
                     target); HEAD left untouched and may not match the reverted tree \
                     (partial rewind); git checkpoints retained to stay coherent with HEAD"
                );
            }
            if rewind_hunks_enabled() {
                session.restore_hunk_checkpoints(target_prompt_index).await;
                crate::handle::record_rewind_restore(crate::handle::RewindDomain::Hunk, true);
            }
        }
        if response.success && rewind_durable_enabled() {
            session.truncate_checkpoint_store(target_prompt_index).await;
        }
        response
    }
    /// Soft-restore the git domain at `target_prompt_index` (phase 1 only: stash +
    /// `reset --soft` + unstage; turn-local commits preserved). Returns the
    /// [`GitRestoreOutcome`](git::GitRestoreOutcome) and captured
    /// [`GitStateRef`](git::GitStateRef) so the caller can, after the FS revert
    /// succeeds, re-stage paths and drop checkpoints `>= target` — deferred so a
    /// failed FS revert retains them for retry.
    ///
    /// Checkpoint selection prefers the state captured exactly at the target, but
    /// falls back to the nearest earlier checkpoint (greatest index `<= target`)
    /// when none was captured at the target itself — e.g. git-rewind was enabled
    /// mid-session so the target predates capture, or capture was skipped for that
    /// prompt. This lands HEAD on the closest known-good git state instead of
    /// leaving it post-turn. `None` when disabled, no session, or no git state
    /// captured at or before the target (caller then degrades explicitly).
    pub(crate) async fn restore_git_checkpoint(
        &self,
        session_id: &str,
        target_prompt_index: usize,
    ) -> Option<(git::GitRestoreOutcome, git::GitStateRef)> {
        if !git::git_rewind_enabled() {
            return None;
        }
        let session = self.session(session_id)?;
        let store = session.git_checkpoints();
        let (checkpoint_index, state) = match store.get_at_or_before(target_prompt_index).await {
            Some(found) => found,
            None => {
                tracing::debug!(
                    session_id,
                    target_prompt_index,
                    "restore_git_checkpoint: no git state captured at or before target; skipping"
                );
                return None;
            }
        };
        if checkpoint_index != target_prompt_index {
            tracing::info!(
                session_id,
                target_prompt_index,
                checkpoint_index,
                "restore_git_checkpoint: no checkpoint at target; soft-restoring to the \
                 nearest earlier git checkpoint (closest known-good HEAD)"
            );
        }
        let outcome = git::soft_restore_git_state(session.cwd(), &state, session_id).await;
        Some((outcome, state))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::tests::{make_handle, make_handle_with_rewind_all_outcomes};
    #[tokio::test]
    async fn start_with_prompt_index_begins_and_end_finalizes_fs_rewind() {
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        let cwd = session.cwd().to_path_buf();
        handle
            .on_turn_boundary("main", TurnBoundary::rewind_begin(3))
            .await;
        let tracker = session.file_state_tracker();
        assert_eq!(tracker.current_prompt_index().await, Some(3));
        assert!(
            tracker.get_rewind_point(3).await.is_some(),
            "begin should create a rewind point for the prompt"
        );
        tracker
            .add_before_snapshot_for_prompt(3, &cwd.join("a.txt"), &cwd, Some("v1".to_owned()))
            .await;
        handle
            .on_turn_boundary("main", TurnBoundary::rewind_finalize(3))
            .await;
        assert_eq!(
            tracker.current_prompt_index().await,
            None,
            "finalize (end_prompt) clears the current prompt index"
        );
        let point = tracker
            .get_rewind_point(3)
            .await
            .expect("rewind point still present after finalize");
        assert!(
            !point.after_snapshots.is_empty(),
            "finalize should capture after-snapshots for touched files"
        );
    }
    #[tokio::test]
    async fn turn_hook_start_does_not_touch_fs_rewind() {
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        handle
            .on_turn_boundary("main", TurnBoundary::turn_start(7))
            .await;
        let tracker = session.file_state_tracker();
        assert!(
            tracker.get_rewind_points().await.is_empty(),
            "turn-hook boundary must not create rewind points"
        );
        assert_eq!(tracker.current_prompt_index().await, None);
    }
    /// Open a checkpoint and record one touched file so finalize has an after-snapshot.
    async fn open_checkpoint_with_touched_file(handle: &WorkspaceHandle, prompt_index: usize) {
        let session = handle.session("main").expect("main session exists");
        let cwd = session.cwd().to_path_buf();
        handle
            .on_turn_boundary("main", TurnBoundary::rewind_begin(prompt_index))
            .await;
        session
            .file_state_tracker()
            .add_before_snapshot_for_prompt(
                prompt_index,
                &cwd.join("a.txt"),
                &cwd,
                Some("v1".to_owned()),
            )
            .await;
    }
    #[tokio::test]
    async fn non_completed_turn_end_finalizes_open_fs_rewind_when_flag_on() {
        let handle = make_handle_with_rewind_all_outcomes(true);
        open_checkpoint_with_touched_file(&handle, 5).await;
        let session = handle.session("main").expect("main session exists");
        let tracker = session.file_state_tracker();
        assert_eq!(tracker.current_prompt_index().await, Some(5));
        handle
            .on_turn_boundary(
                "main",
                TurnBoundary::turn_end(5, 0, TurnHookOutcome::Error, Vec::new()),
            )
            .await;
        assert_eq!(
            tracker.current_prompt_index().await,
            None,
            "non-Completed turn-end should finalize (clear) the open prompt"
        );
        let point = tracker
            .get_rewind_point(5)
            .await
            .expect("rewind point present");
        assert!(
            !point.after_snapshots.is_empty(),
            "finalize should capture after-snapshots for touched files"
        );
    }
    #[tokio::test]
    async fn non_completed_turn_end_does_not_finalize_when_flag_off() {
        let handle = make_handle();
        open_checkpoint_with_touched_file(&handle, 5).await;
        let session = handle.session("main").expect("main session exists");
        let tracker = session.file_state_tracker();
        handle
            .on_turn_boundary(
                "main",
                TurnBoundary::turn_end(5, 0, TurnHookOutcome::Error, Vec::new()),
            )
            .await;
        assert_eq!(
            tracker.current_prompt_index().await,
            Some(5),
            "with the flag off the turn-hook path must leave the checkpoint open"
        );
        let point = tracker
            .get_rewind_point(5)
            .await
            .expect("rewind point present");
        assert!(
            point.after_snapshots.is_empty(),
            "no after-snapshots should be captured when the flag is off"
        );
    }
    #[tokio::test]
    async fn completed_turn_end_hook_does_not_finalize_even_when_flag_on() {
        let handle = make_handle_with_rewind_all_outcomes(true);
        open_checkpoint_with_touched_file(&handle, 5).await;
        let session = handle.session("main").expect("main session exists");
        let tracker = session.file_state_tracker();
        handle
            .on_turn_boundary(
                "main",
                TurnBoundary::turn_end(5, 0, TurnHookOutcome::Completed, Vec::new()),
            )
            .await;
        assert_eq!(
            tracker.current_prompt_index().await,
            Some(5),
            "Completed turn-ends are finalized by the RPC path, not the hook path"
        );
    }
    /// The non-Completed canary advances when a non-`Completed` turn-end finalizes
    /// an open FS checkpoint (flag on). Counters are monotonic, so `after > before` is robust.
    #[tokio::test]
    async fn canary_counts_non_completed_finalize() {
        let label = crate::handle::rewind_outcome_label(TurnHookOutcome::Error);
        let before = crate::handle::REWIND_NON_COMPLETED_FINALIZE_TOTAL
            .with_label_values(&[label])
            .get();
        let handle = make_handle_with_rewind_all_outcomes(true);
        open_checkpoint_with_touched_file(&handle, 11).await;
        handle
            .on_turn_boundary(
                "main",
                TurnBoundary::turn_end(11, 0, TurnHookOutcome::Error, Vec::new()),
            )
            .await;
        let after = crate::handle::REWIND_NON_COMPLETED_FINALIZE_TOTAL
            .with_label_values(&[label])
            .get();
        assert!(
            after > before,
            "canary must advance on a non-Completed finalize (before={before}, after={after})"
        );
    }
    /// A `Completed` turn-end never feeds the canary: the `completed` label must
    /// stay zero, guarding the `outcome != Completed` gate.
    #[tokio::test]
    async fn canary_never_labeled_completed() {
        let handle = make_handle_with_rewind_all_outcomes(true);
        open_checkpoint_with_touched_file(&handle, 12).await;
        handle
            .on_turn_boundary(
                "main",
                TurnBoundary::turn_end(12, 0, TurnHookOutcome::Completed, Vec::new()),
            )
            .await;
        assert_eq!(
            crate::handle::REWIND_NON_COMPLETED_FINALIZE_TOTAL
                .with_label_values(&[crate::handle::rewind_outcome_label(
                    TurnHookOutcome::Completed
                )])
                .get(),
            0,
            "Completed turns must never increment the non-Completed finalize canary"
        );
    }
    /// Capture two turns, rewind: earlier turn survives; target-and-later are
    /// dropped and truncated.
    #[tokio::test]
    async fn capture_then_restore_round_trips_turn_delta() {
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        let cwd = session.cwd().to_path_buf();
        let hunks = session.hunk_tracker();
        hunks.record_agent_write(cwd.join("a.rs"), "fn a() {}\n".to_owned(), 0, None);
        hunks.record_agent_write(cwd.join("b.rs"), "fn b() {}\n".to_owned(), 1, None);
        session.capture_hunk_delta(0).await;
        session.capture_hunk_delta(1).await;
        assert!(!hunks.get_turn_hunks(0).await.is_empty());
        assert!(!hunks.get_turn_hunks(1).await.is_empty());
        session.restore_hunk_checkpoints(1).await;
        assert!(
            hunks.get_turn_hunks(1).await.is_empty(),
            "rewind must drop the rewound turn's hunks"
        );
        assert!(
            !hunks.get_turn_hunks(0).await.is_empty(),
            "rewind must keep turns before the target"
        );
        let tracked = hunks.get_all_tracked_paths().await;
        assert!(
            tracked.contains(&cwd.join("a.rs")),
            "kept turn's file remains"
        );
        assert!(
            !tracked.contains(&cwd.join("b.rs")),
            "rewound turn's file is gone"
        );
        let store = session.hunk_checkpoints.lock().await;
        assert!(store.contains_key(&0), "kept delta remains in the store");
        assert!(!store.contains_key(&1), "rewound delta is dropped");
    }
    /// `get_checkpoint` assembles both domains: FS rewind point + stored hunk delta.
    #[tokio::test]
    async fn get_checkpoint_bundles_fs_point_and_hunk_delta() {
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        let cwd = session.cwd().to_path_buf();
        let tracker = session.file_state_tracker();
        tracker.begin_prompt(0).await;
        tracker
            .add_before_snapshot_for_prompt(0, &cwd.join("a.rs"), &cwd, Some("v0".to_owned()))
            .await;
        session.hunk_tracker().record_agent_write(
            cwd.join("a.rs"),
            "fn a() {}\n".to_owned(),
            0,
            None,
        );
        session.capture_hunk_delta(0).await;
        let checkpoint = session.get_checkpoint(0).await.expect("checkpoint exists");
        assert_eq!(checkpoint.prompt_index, 0);
        assert!(
            !checkpoint.fs.file_snapshots.is_empty(),
            "fs side carries the before-snapshot"
        );
        let delta = checkpoint.hunks.expect("hunk delta captured");
        assert!(delta.file_states.contains_key(&cwd.join("a.rs")));
        assert!(session.get_checkpoint(5).await.is_none());
    }
    /// Flag off (default): finalize never captures a hunk delta.
    #[tokio::test]
    async fn turn_end_with_flag_off_skips_hunk_capture() {
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        let cwd = session.cwd().to_path_buf();
        session.hunk_tracker().record_agent_write(
            cwd.join("a.rs"),
            "fn a() {}\n".to_owned(),
            0,
            None,
        );
        assert!(
            !rewind_hunks_enabled(),
            "flag must default off for the legacy path"
        );
        handle
            .on_turn_boundary("main", TurnBoundary::rewind_finalize(0))
            .await;
        assert!(
            session.hunk_checkpoints.lock().await.is_empty(),
            "flag-off finalize must not store hunk deltas (legacy default)"
        );
    }
    /// Mid-session enable with no deltas before a non-zero target: restore must
    /// not wipe live hunks.
    #[tokio::test]
    async fn restore_with_no_deltas_before_target_does_not_wipe() {
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        let cwd = session.cwd().to_path_buf();
        let hunks = session.hunk_tracker();
        hunks.record_agent_write(cwd.join("early.rs"), "fn e() {}\n".to_owned(), 1, None);
        hunks.record_agent_write(cwd.join("late.rs"), "fn l() {}\n".to_owned(), 5, None);
        session.capture_hunk_delta(5).await;
        session.restore_hunk_checkpoints(3).await;
        let tracked = hunks.get_all_tracked_paths().await;
        assert!(
            tracked.contains(&cwd.join("early.rs")),
            "uncaptured live hunks must not be wiped when nothing is captured before the target"
        );
        assert!(
            session.hunk_checkpoints.lock().await.contains_key(&5),
            "the store is left untouched when restore cannot reconstruct"
        );
    }
    /// Rewind to 0: re-seed to the empty start-of-session state.
    #[tokio::test]
    async fn restore_to_zero_clears_all_hunk_state() {
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        let cwd = session.cwd().to_path_buf();
        let hunks = session.hunk_tracker();
        hunks.record_agent_write(cwd.join("a.rs"), "fn a() {}\n".to_owned(), 0, None);
        session.capture_hunk_delta(0).await;
        assert!(!hunks.get_all_tracked_paths().await.is_empty());
        session.restore_hunk_checkpoints(0).await;
        assert!(
            hunks.get_all_tracked_paths().await.is_empty(),
            "rewind to 0 reconstructs the empty start-of-session hunk state"
        );
        assert!(
            session.hunk_checkpoints.lock().await.is_empty(),
            "rewind to 0 truncates the entire delta store"
        );
    }
    /// `workspace_rewind_git` default (OFF): capture must not touch git.
    #[tokio::test]
    async fn git_capture_is_noop_when_flag_disabled_by_default() {
        assert!(
            !git::git_rewind_enabled(),
            "workspace_rewind_git must default OFF"
        );
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        handle
            .on_turn_boundary("main", TurnBoundary::rewind_begin(2))
            .await;
        assert!(
            session.git_checkpoints().get(2).await.is_none(),
            "git state must not be captured while workspace_rewind_git is disabled"
        );
        assert!(
            session
                .file_state_tracker()
                .get_rewind_point(2)
                .await
                .is_some(),
            "FS-rewind capture must be unaffected by the git flag"
        );
    }
    /// A fully-populated checkpoint (FS + hunk delta) round-trips through JSON:
    /// `Value` is stable across encode→decode→encode and both payloads survive.
    /// `Hunk.selected` is `#[serde(skip)]` (transient), so it decodes to default.
    #[tokio::test]
    async fn rewind_checkpoint_round_trips_through_json() {
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        let cwd = session.cwd().to_path_buf();
        let tracker = session.file_state_tracker();
        tracker.begin_prompt(0).await;
        tracker
            .add_before_snapshot_for_prompt(0, &cwd.join("a.rs"), &cwd, Some("v0".to_owned()))
            .await;
        tracker.end_prompt(session.async_fs(), 0).await;
        session.hunk_tracker().record_agent_write(
            cwd.join("a.rs"),
            "fn a() {}\n".to_owned(),
            0,
            None,
        );
        session.capture_hunk_delta(0).await;
        let original = session.get_checkpoint(0).await.expect("checkpoint exists");
        assert!(
            original.hunks.is_some(),
            "precondition: hunk delta captured, so serde covers all populated domains"
        );
        let encoded = serde_json::to_value(&original).expect("serialize");
        let decoded: RewindCheckpoint =
            serde_json::from_value(encoded.clone()).expect("deserialize");
        let re_encoded = serde_json::to_value(&decoded).expect("re-serialize");
        assert_eq!(
            encoded, re_encoded,
            "RewindCheckpoint must round-trip through JSON unchanged"
        );
        assert_eq!(decoded.prompt_index, 0);
        assert_eq!(
            decoded.fs.file_snapshots.len(),
            1,
            "fs before-snapshot survives the round-trip"
        );
        let delta = decoded.hunks.expect("hunk delta survives round-trip");
        assert_eq!(delta.prompt_index, 0);
        let snapshot = delta
            .file_states
            .get(&cwd.join("a.rs"))
            .expect("hunk delta's touched file survives");
        let hunk = snapshot.hunks.first().expect("the file's hunk survives");
        assert!(
            hunk.new_text.contains("fn a()"),
            "hunk new_text payload survives the round-trip, got {:?}",
            hunk.new_text
        );
        assert!(!hunk.selected, "transient `selected` is not persisted");
    }
    /// Optional domain fields are additive: a checkpoint round-trips with
    /// `hunks: None`, and JSON omitting the optional field still deserializes.
    #[tokio::test]
    async fn checkpoint_serde_tolerates_missing_optional_fields() {
        let cp = RewindCheckpoint {
            prompt_index: 2,
            fs: RewindPoint::new(2),
            hunks: None,
        };
        let encoded = serde_json::to_value(&cp).expect("serialize");
        let decoded: RewindCheckpoint = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded.prompt_index, 2);
        assert!(decoded.hunks.is_none());
        let json = r#"{
            "prompt_index": 2,
            "fs": {
                "prompt_index": 2,
                "created_at": "2024-01-01T00:00:00Z",
                "file_snapshots": {},
                "after_snapshots": {}
            }
        }"#;
        let from_legacy: RewindCheckpoint = serde_json::from_str(json).expect("deserialize legacy");
        assert_eq!(from_legacy.prompt_index, 2);
        assert!(
            from_legacy.hunks.is_none(),
            "a missing optional field defaults to None"
        );
    }
    /// Durable flag off (default): finalize does no disk I/O — the on-disk store
    /// directory is never created.
    #[tokio::test]
    async fn turn_end_with_durable_flag_off_writes_nothing_to_disk() {
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        let cwd = session.cwd().to_path_buf();
        assert!(
            !rewind_durable_enabled(),
            "durable flag must default off for the legacy path"
        );
        handle
            .on_turn_boundary("main", TurnBoundary::rewind_begin(0))
            .await;
        session
            .file_state_tracker()
            .add_before_snapshot_for_prompt(0, &cwd.join("a.txt"), &cwd, Some("v".to_owned()))
            .await;
        handle
            .on_turn_boundary("main", TurnBoundary::rewind_finalize(0))
            .await;
        let store_root = cwd.join(".echo-agent").join("rewind-checkpoints");
        assert!(
            !store_root.exists(),
            "flag-off finalize must not touch disk (legacy default)"
        );
    }
    /// `persist_checkpoint` writes the assembled checkpoint through the session's
    /// durable store (called directly to bypass the flag); retrievable with both domains.
    #[tokio::test]
    async fn persist_checkpoint_serializes_through_session_store() {
        let handle = make_handle();
        let session = handle.session("main").expect("main session exists");
        let cwd = session.cwd().to_path_buf();
        let tracker = session.file_state_tracker();
        tracker.begin_prompt(0).await;
        tracker
            .add_before_snapshot_for_prompt(0, &cwd.join("a.rs"), &cwd, Some("v0".to_owned()))
            .await;
        tracker.end_prompt(session.async_fs(), 0).await;
        session.hunk_tracker().record_agent_write(
            cwd.join("a.rs"),
            "fn a() {}\n".to_owned(),
            0,
            None,
        );
        session.capture_hunk_delta(0).await;
        session.persist_checkpoint(0).await;
        let store_root = cwd.join(".echo-agent").join("rewind-checkpoints");
        assert!(store_root.exists(), "persist creates the durable store dir");
        let stored = session
            .checkpoint_store
            .get(0)
            .await
            .expect("checkpoint persisted");
        assert_eq!(stored.prompt_index, 0);
        assert!(
            stored.hunks.is_some(),
            "hunk delta is bundled into the blob"
        );
        assert_eq!(stored.fs.file_snapshots.len(), 1);
    }
    /// Full rewind lifecycle against a disabled (`noop()`) hunk tracker — the
    /// operating mode when the user sets `hunk_tracker_mode = off`. Capture
    /// must report "nothing stored" and restore must be a clean no-op, while
    /// the FS-rewind half keeps working (hunks simply absent from the blob).
    #[tokio::test]
    async fn rewind_lifecycle_is_clean_noop_with_disabled_hunk_tracker() {
        let handle = make_handle();
        handle.drop_session("main", "main").expect("drop main");
        let session = handle
            .create_session_with_tracker_and_viewer_ctx(
                "main",
                handle.root_cwd().unwrap(),
                echo_agent_hunk_tracker::HunkTrackerHandle::noop(),
                None,
                crate::capability::CapabilityMode::All,
                None,
                false,
            )
            .expect("create session with noop tracker");
        let cwd = session.cwd().to_path_buf();
        let file = cwd.join("a.rs");
        let tracker = session.file_state_tracker();
        tracker.begin_prompt(0).await;
        tracker
            .add_before_snapshot_for_prompt(0, &file, &cwd, Some("v0\n".to_owned()))
            .await;
        session
            .async_fs()
            .write_file(&file, b"v1\n")
            .await
            .expect("write v1");
        tracker.end_prompt(session.async_fs(), 0).await;
        session
            .hunk_tracker()
            .record_agent_write(file.clone(), "v1\n".to_owned(), 0, None);
        assert!(
            !session.capture_hunk_delta(0).await,
            "capture_hunk_delta must return false under a disabled hunk tracker"
        );
        session.restore_hunk_checkpoints(0).await;
        session.persist_checkpoint(0).await;
        let stored = session
            .checkpoint_store
            .get(0)
            .await
            .expect("FS checkpoint persisted even with the tracker disabled");
        assert_eq!(stored.prompt_index, 0);
        assert!(
            stored.hunks.is_none(),
            "no hunk delta is bundled when the tracker is disabled"
        );
        assert_eq!(stored.fs.file_snapshots.len(), 1);
        let resp = handle.rewind_to("main", 0).await;
        assert!(resp.success, "FS rewind must succeed: {:?}", resp.error);
        assert!(
            resp.reverted_files.iter().any(|f| f.ends_with("a.rs")),
            "a.rs must be among the reverted files: {:?}",
            resp.reverted_files
        );
        let restored = session
            .async_fs()
            .try_read_to_string(&file)
            .await
            .expect("read a.rs");
        assert_eq!(
            restored.as_deref(),
            Some("v0\n"),
            "file content must revert to its pre-prompt state"
        );
    }
}
