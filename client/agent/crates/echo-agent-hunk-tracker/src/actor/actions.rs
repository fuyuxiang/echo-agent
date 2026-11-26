//! Action commands for the HunkTrackerActor.
//!
//! These methods handle accept/reject actions on hunks.
//!
//! # Invariants
//!
//! Hunks only exist for files where both baseline and current content are
//! patchable (i.e., `FileContentState::Full` or `FileContentState::Missing`
//! for creation/deletion). Files with `Binary` or `TooLarge` content states
//! have their hunks cleared by `recompute_hunks()`.
//!
//! This means action handlers should never encounter a non-patchable state
//! when processing a hunk. The guards in these methods are defensive fallbacks
//! that silently skip non-patchable states rather than panic.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::diff::patch_lines;
use crate::events::{HunkEvent, HunkRemovalReason};
use crate::types::{Hunk, HunkAction, HunkActionError, HunkId, HunkLineInfo, HunkSource};

use super::HunkTrackerActor;
use super::state::FileContentState;

impl HunkTrackerActor {
    /// Update session stats when a hunk is accepted or rejected.
    fn update_session_stats(&mut self, line_info: &HunkLineInfo, accepted: bool) {
        let lines_added = line_info.new_count;
        let lines_removed = line_info.old_count;

        if accepted {
            self.session_stats.accepted_hunks = self.session_stats.accepted_hunks.saturating_add(1);
            self.session_stats.accepted_lines_added = self
                .session_stats
                .accepted_lines_added
                .saturating_add(lines_added);
            self.session_stats.accepted_lines_removed = self
                .session_stats
                .accepted_lines_removed
                .saturating_add(lines_removed);
        } else {
            self.session_stats.rejected_hunks = self.session_stats.rejected_hunks.saturating_add(1);
            self.session_stats.rejected_lines_added = self
                .session_stats
                .rejected_lines_added
                .saturating_add(lines_added);
            self.session_stats.rejected_lines_removed = self
                .session_stats
                .rejected_lines_removed
                .saturating_add(lines_removed);
        }
    }

    /// Remove a hunk from turn_index based on its source.
    fn remove_from_turn_index(&mut self, hunk_id: &HunkId, source: &HunkSource) {
        if let Some(prompt_index) = source.prompt_index()
            && let Some(set) = self.turn_index.get_mut(&prompt_index)
        {
            set.remove(hunk_id);
        }
    }

    /// Apply action (accept/reject) to a specific hunk.
    pub(super) async fn apply_hunk_action(
        &mut self,
        hunk_id: &HunkId,
        action: HunkAction,
    ) -> Result<(), HunkActionError> {
        // Find which file contains this hunk and capture the full hunk data
        let hunk_info = self.file_states.iter().find_map(|(path, state)| {
            state
                .hunks
                .iter()
                .find(|h| &h.id == hunk_id)
                .map(|h| (path.clone(), h.clone()))
        });

        let Some((path, hunk)) = hunk_info else {
            return Err(HunkActionError::HunkNotFound(hunk_id.clone()));
        };

        // Update session stats before removing the hunk
        let accepted = matches!(action, HunkAction::Accept);
        self.update_session_stats(&hunk.line_info, accepted);

        // Remove from turn_index
        self.remove_from_turn_index(hunk_id, &hunk.source);

        match action {
            HunkAction::Accept => {
                self.accept_hunk(&path, &hunk).await?;
            }
            HunkAction::Reject => {
                self.reject_hunk(&path, &hunk).await?;
            }
        }

        Ok(())
    }

    /// Accept a single hunk: patch baseline to include this hunk's changes.
    async fn accept_hunk(&mut self, path: &Path, hunk: &Arc<Hunk>) -> Result<(), HunkActionError> {
        // Collect data and perform mutations, then send events afterward
        let (should_recompute, current, _source) = {
            let state = self.file_states.get_mut(path);
            let Some(state) = state else {
                return Ok(());
            };

            // Handle file creation case (no baseline)
            if matches!(state.baseline, FileContentState::Missing) {
                // File was created - accepting makes current the baseline for this hunk's lines
                // For a newly created file, accept just sets baseline = current
                state.baseline = state.current_content.clone();
                state.baseline_accepted = true;
                state.hunks.retain(|h| h.id != hunk.id);
                // Don't recompute for file creation (no other hunks exist)
                (false, None, hunk.source)
            } else {
                // Patch baseline to include ONLY this hunk's changes
                if let FileContentState::Full(baseline) = &state.baseline {
                    let patched = patch_lines(
                        baseline,
                        hunk.line_info.old_start,
                        hunk.line_info.old_count,
                        &hunk.new_text,
                    );
                    state.baseline = FileContentState::Full(patched);
                }
                state.baseline_accepted = true;

                // Remove this hunk from the list
                state.hunks.retain(|h| h.id != hunk.id);

                // Pass FileContentState directly (R3/MF-4: preserve Binary/TooLarge, no String extraction)
                let current = Some(state.current_content.clone());
                let source = hunk.source;
                (true, current, source)
            }
        };

        // Send events after mutable borrow is released
        self.send_event(HunkEvent::HunkRemoved {
            path: path.to_path_buf(),
            hunk_id: hunk.id.clone(),
            reason: HunkRemovalReason::Accepted,
        });
        self.send_event(HunkEvent::BaselineUpdated {
            path: path.to_path_buf(),
        });

        // Recompute remaining hunks (positions may have shifted)
        if should_recompute {
            // Use the source of the first remaining hunk if any, else External
            let remaining_source = self
                .file_states
                .get(path)
                .and_then(|state| state.hunks.first())
                .map(|h| h.source)
                .unwrap_or(HunkSource::External);

            self.recompute_hunks(path, current, remaining_source);
        }

        Ok(())
    }

    /// Reject a single hunk: patch current content to revert this hunk's changes.
    async fn reject_hunk(&mut self, path: &Path, hunk: &Arc<Hunk>) -> Result<(), HunkActionError> {
        // First, determine what action to take and collect data
        enum RejectAction {
            RestoreDeleted { baseline: String },
            DeleteCreated,
            RevertChange { patched: String },
            NoOp,
        }

        let action = {
            let state = self.file_states.get(path);
            let Some(state) = state else {
                return Ok(());
            };

            if matches!(state.current_content, FileContentState::Missing)
                && matches!(state.baseline, FileContentState::Full(_))
            {
                // File was deleted, rejecting restores it
                if let FileContentState::Full(baseline) = &state.baseline {
                    RejectAction::RestoreDeleted {
                        baseline: baseline.clone(),
                    }
                } else {
                    RejectAction::NoOp
                }
            } else if matches!(state.baseline, FileContentState::Missing)
                && matches!(state.current_content, FileContentState::Full(_))
            {
                // File was created, rejecting deletes it
                RejectAction::DeleteCreated
            } else if let FileContentState::Full(current) = &state.current_content {
                // Normal case: patch current content to revert this hunk's changes
                let old_text = hunk.old_text.as_deref().unwrap_or("");
                let patched = patch_lines(
                    current,
                    hunk.line_info.new_start,
                    hunk.line_info.new_count,
                    old_text,
                );
                RejectAction::RevertChange { patched }
            } else {
                RejectAction::NoOp
            }
        };

        // Perform file I/O based on the action
        match &action {
            RejectAction::RestoreDeleted { baseline } => {
                tokio::fs::write(path, baseline).await.map_err(|e| {
                    HunkActionError::WriteError {
                        path: path.to_path_buf(),
                        source: e,
                    }
                })?;
            }
            RejectAction::DeleteCreated => {
                tokio::fs::remove_file(path)
                    .await
                    .map_err(|e| HunkActionError::DeleteError {
                        path: path.to_path_buf(),
                        source: e,
                    })?;
            }
            RejectAction::RevertChange { patched } => {
                tokio::fs::write(path, patched)
                    .await
                    .map_err(|e| HunkActionError::WriteError {
                        path: path.to_path_buf(),
                        source: e,
                    })?;
            }
            RejectAction::NoOp => return Ok(()),
        }

        // Update state and collect data for recompute
        let (should_recompute, current, _source) = {
            let state = self.file_states.get_mut(path);
            let Some(state) = state else {
                return Ok(());
            };

            match action {
                RejectAction::RestoreDeleted { baseline } => {
                    state.current_content = FileContentState::Full(baseline);
                    state.hunks.retain(|h| h.id != hunk.id);
                    (false, None, hunk.source)
                }
                RejectAction::DeleteCreated => {
                    state.current_content = FileContentState::Missing;
                    state.hunks.retain(|h| h.id != hunk.id);
                    (false, None, hunk.source)
                }
                RejectAction::RevertChange { patched } => {
                    let patched_state = FileContentState::Full(patched);
                    state.current_content = patched_state.clone();
                    state.hunks.retain(|h| h.id != hunk.id);
                    (true, Some(patched_state), hunk.source)
                }
                RejectAction::NoOp => return Ok(()),
            }
        };

        // Send event after mutable borrow is released
        self.send_event(HunkEvent::HunkRemoved {
            path: path.to_path_buf(),
            hunk_id: hunk.id.clone(),
            reason: HunkRemovalReason::Rejected,
        });

        // Recompute remaining hunks
        if should_recompute {
            // Use the source of the first remaining hunk if any, else External
            let remaining_source = self
                .file_states
                .get(path)
                .and_then(|state| state.hunks.first())
                .map(|h| h.source)
                .unwrap_or(HunkSource::External);

            self.recompute_hunks(path, current, remaining_source);
        }

        Ok(())
    }

    /// Apply action (accept/reject) to all hunks for a file.
    /// Uses batched processing to avoid stale hunk IDs during recomputation.
    pub(super) async fn apply_file_action(
        &mut self,
        path: &Path,
        action: HunkAction,
    ) -> Result<Vec<HunkId>, HunkActionError> {
        let Some(state) = self.file_states.get(path) else {
            return Ok(vec![]);
        };

        let hunks_to_process: Vec<Arc<Hunk>> = state.hunks.clone();

        if hunks_to_process.is_empty() {
            return Ok(vec![]);
        }

        self.apply_action_batch(&[(path.to_path_buf(), hunks_to_process)], action)
            .await
    }

    /// Apply action (accept/reject) to all hunks.
    /// Uses batched processing to avoid stale hunk IDs during recomputation.
    pub(super) async fn apply_all_action(
        &mut self,
        action: HunkAction,
    ) -> Result<Vec<HunkId>, HunkActionError> {
        // Collect all hunks grouped by file
        let files_with_hunks: Vec<(PathBuf, Vec<Arc<Hunk>>)> = self
            .file_states
            .iter()
            .filter(|(_, state)| !state.hunks.is_empty())
            .map(|(path, state)| (path.clone(), state.hunks.clone()))
            .collect();

        if files_with_hunks.is_empty() {
            return Ok(vec![]);
        }

        self.apply_action_batch(&files_with_hunks, action).await
    }

    /// Apply action (accept/reject) to all hunks for a specific turn.
    /// Uses batched processing to avoid stale hunk IDs during recomputation.
    pub(super) async fn apply_turn_action(
        &mut self,
        prompt_index: usize,
        action: HunkAction,
    ) -> Result<Vec<HunkId>, HunkActionError> {
        // Collect all hunks for this turn, grouped by file
        let mut files_with_hunks: std::collections::HashMap<PathBuf, Vec<Arc<Hunk>>> =
            std::collections::HashMap::new();

        for (path, state) in &self.file_states {
            let turn_hunks: Vec<Arc<Hunk>> = state
                .hunks
                .iter()
                .filter(|h| h.source.prompt_index() == Some(prompt_index))
                .cloned()
                .collect();

            if !turn_hunks.is_empty() {
                files_with_hunks.insert(path.clone(), turn_hunks);
            }
        }

        if files_with_hunks.is_empty() {
            return Ok(vec![]);
        }

        let files_vec: Vec<_> = files_with_hunks.into_iter().collect();
        self.apply_action_batch(&files_vec, action).await
    }

    /// Internal batched action processor.
    /// Processes all hunks for each file atomically, then recomputes once per file.
    async fn apply_action_batch(
        &mut self,
        files_with_hunks: &[(PathBuf, Vec<Arc<Hunk>>)],
        action: HunkAction,
    ) -> Result<Vec<HunkId>, HunkActionError> {
        let mut affected_hunk_ids = Vec::new();

        for (path, hunks) in files_with_hunks {
            // Process all hunks for this file in one go
            for hunk in hunks {
                // Update session stats
                let accepted = matches!(action, HunkAction::Accept);
                self.update_session_stats(&hunk.line_info, accepted);

                // Remove from turn_index
                self.remove_from_turn_index(&hunk.id, &hunk.source);

                affected_hunk_ids.push(hunk.id.clone());
            }

            // Apply all patches for this file at once
            match action {
                HunkAction::Accept => {
                    self.accept_hunks_batch(path, hunks).await?;
                }
                HunkAction::Reject => {
                    self.reject_hunks_batch(path, hunks).await?;
                }
            }
        }

        Ok(affected_hunk_ids)
    }

    /// Accept multiple hunks for a file atomically.
    /// Patches baseline incrementally, then recomputes once.
    async fn accept_hunks_batch(
        &mut self,
        path: &Path,
        hunks: &[Arc<Hunk>],
    ) -> Result<(), HunkActionError> {
        if hunks.is_empty() {
            return Ok(());
        }

        let (should_recompute, current) = {
            let Some(state) = self.file_states.get_mut(path) else {
                return Ok(());
            };

            // Special case: file creation (no baseline)
            if matches!(state.baseline, FileContentState::Missing) {
                // Accepting all hunks for a new file just sets baseline = current
                state.baseline = state.current_content.clone();
                state.baseline_accepted = true;
                let hunk_ids: Vec<HunkId> = hunks.iter().map(|h| h.id.clone()).collect();
                state.hunks.retain(|h| !hunk_ids.contains(&h.id));

                // Send events
                for hunk in hunks {
                    self.send_event(HunkEvent::HunkRemoved {
                        path: path.to_path_buf(),
                        hunk_id: hunk.id.clone(),
                        reason: HunkRemovalReason::Accepted,
                    });
                }
                self.send_event(HunkEvent::BaselineUpdated {
                    path: path.to_path_buf(),
                });

                return Ok(());
            }

            // Patch baseline for each hunk (process end-to-start to avoid shifts)
            let mut sorted_hunks = hunks.to_vec();
            sorted_hunks.sort_by_key(|h| std::cmp::Reverse(h.line_info.old_start));
            for hunk in &sorted_hunks {
                if let FileContentState::Full(baseline) = &state.baseline {
                    let patched = patch_lines(
                        baseline,
                        hunk.line_info.old_start,
                        hunk.line_info.old_count,
                        &hunk.new_text,
                    );
                    state.baseline = FileContentState::Full(patched);
                }
            }
            state.baseline_accepted = true;

            // Remove all accepted hunks
            let hunk_ids: Vec<HunkId> = hunks.iter().map(|h| h.id.clone()).collect();
            state.hunks.retain(|h| !hunk_ids.contains(&h.id));

            // Pass FileContentState directly (R3/MF-4: preserve Binary/TooLarge)
            let current = Some(state.current_content.clone());
            (true, current)
        };

        // Send events
        for hunk in hunks {
            self.send_event(HunkEvent::HunkRemoved {
                path: path.to_path_buf(),
                hunk_id: hunk.id.clone(),
                reason: HunkRemovalReason::Accepted,
            });
        }
        self.send_event(HunkEvent::BaselineUpdated {
            path: path.to_path_buf(),
        });

        // Recompute remaining hunks once
        if should_recompute {
            // Use the source of the first remaining hunk if any, else External
            let remaining_source = self
                .file_states
                .get(path)
                .and_then(|state| state.hunks.first())
                .map(|h| h.source)
                .unwrap_or(HunkSource::External);

            self.recompute_hunks(path, current, remaining_source);
        }

        Ok(())
    }

    /// Reject multiple hunks for a file atomically.
    /// Patches current content incrementally, then recomputes once.
    async fn reject_hunks_batch(
        &mut self,
        path: &Path,
        hunks: &[Arc<Hunk>],
    ) -> Result<(), HunkActionError> {
        if hunks.is_empty() {
            return Ok(());
        }

        let Some(state) = self.file_states.get(path) else {
            return Ok(());
        };

        if matches!(state.current_content, FileContentState::Missing)
            && matches!(state.baseline, FileContentState::Full(_))
        {
            // File was deleted - restore it
            if let FileContentState::Full(baseline) = &state.baseline {
                tokio::fs::write(path, baseline).await.map_err(|e| {
                    HunkActionError::WriteError {
                        path: path.to_path_buf(),
                        source: e,
                    }
                })?;
            }

            let Some(state) = self.file_states.get_mut(path) else {
                return Ok(());
            };
            state.current_content = state.baseline.clone();
            let hunk_ids: Vec<HunkId> = hunks.iter().map(|h| h.id.clone()).collect();
            state.hunks.retain(|h| !hunk_ids.contains(&h.id));

            for hunk in hunks {
                self.send_event(HunkEvent::HunkRemoved {
                    path: path.to_path_buf(),
                    hunk_id: hunk.id.clone(),
                    reason: HunkRemovalReason::Rejected,
                });
            }

            return Ok(());
        } else if matches!(state.baseline, FileContentState::Missing)
            && matches!(state.current_content, FileContentState::Full(_))
        {
            // File was created - delete it
            tokio::fs::remove_file(path)
                .await
                .map_err(|e| HunkActionError::DeleteError {
                    path: path.to_path_buf(),
                    source: e,
                })?;

            let Some(state) = self.file_states.get_mut(path) else {
                return Ok(());
            };
            state.current_content = FileContentState::Missing;
            let hunk_ids: Vec<HunkId> = hunks.iter().map(|h| h.id.clone()).collect();
            state.hunks.retain(|h| !hunk_ids.contains(&h.id));

            for hunk in hunks {
                self.send_event(HunkEvent::HunkRemoved {
                    path: path.to_path_buf(),
                    hunk_id: hunk.id.clone(),
                    reason: HunkRemovalReason::Rejected,
                });
            }

            return Ok(());
        }

        // Normal case: patch current content to revert all hunks.
        // The early returns above handle (None, Some) and (Some, None).
        // If both are non-Full, there's nothing to patch — bail out.
        let Some(state) = self.file_states.get_mut(path) else {
            return Ok(());
        };

        let current_content = match &state.current_content {
            FileContentState::Full(s) => s.clone(),
            _ => return Ok(()),
        };
        let mut current = current_content;

        // Apply patches in reverse order (from end of file to beginning)
        // to avoid line number shifts
        let mut sorted_hunks = hunks.to_vec();
        sorted_hunks.sort_by_key(|h| std::cmp::Reverse(h.line_info.new_start));

        for hunk in &sorted_hunks {
            let old_text = hunk.old_text.as_deref().unwrap_or("");
            current = patch_lines(
                &current,
                hunk.line_info.new_start,
                hunk.line_info.new_count,
                old_text,
            );
        }

        // Write patched content
        tokio::fs::write(path, &current)
            .await
            .map_err(|e| HunkActionError::WriteError {
                path: path.to_path_buf(),
                source: e,
            })?;

        let Some(state) = self.file_states.get_mut(path) else {
            return Ok(());
        };
        let current_state = FileContentState::Full(current);
        state.current_content = current_state.clone();
        let hunk_ids: Vec<HunkId> = hunks.iter().map(|h| h.id.clone()).collect();
        state.hunks.retain(|h| !hunk_ids.contains(&h.id));

        for hunk in hunks {
            self.send_event(HunkEvent::HunkRemoved {
                path: path.to_path_buf(),
                hunk_id: hunk.id.clone(),
                reason: HunkRemovalReason::Rejected,
            });
        }

        // Recompute remaining hunks
        let remaining_source = self
            .file_states
            .get(path)
            .and_then(|state| state.hunks.first())
            .map(|h| h.source)
            .unwrap_or(HunkSource::External);

        self.recompute_hunks(path, Some(current_state), remaining_source);

        Ok(())
    }
}
