//! Query commands for the HunkTrackerActor.
//!
//! These methods provide read-only access to hunk state.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::diff::generate_hunk_patch;
use crate::types::{
    FileContentView, FileHunkData, Hunk, HunkId, HunkSourceFilter, SessionSummary, TurnSummary,
};

use super::HunkTrackerActor;

impl HunkTrackerActor {
    /// Get all hunks.
    pub(super) fn get_all_hunks(&self) -> Vec<Arc<Hunk>> {
        self.file_states
            .values()
            .flat_map(|state| state.hunks.clone())
            .collect()
    }

    /// Get hunks for a specific path.
    pub(super) fn get_hunks_for_path(&self, path: &Path) -> Vec<Arc<Hunk>> {
        self.file_states
            .get(path)
            .map(|state| state.hunks.clone())
            .unwrap_or_default()
    }

    /// Get hunks + file content for a specific path (for diff rendering).
    /// Each hunk includes its own patch fragment with context lines.
    /// Returns explicit content status (Full/Binary/TooLarge/Missing) for both
    /// baseline and current content, plus legacy Option<String> fields for
    /// backward compatibility.
    pub(super) fn get_file_hunk_data(&self, path: &Path) -> FileHunkData {
        self.file_states
            .get(path)
            .map(|state| {
                // Extract text content for patching (only Full states)
                let baseline_text = state.baseline.as_str();
                let current_text = state.current_content.as_str();

                // Generate patch for each hunk
                let hunks_with_patches: Vec<Arc<Hunk>> = state
                    .hunks
                    .iter()
                    .map(|hunk| {
                        // Generate patch if we have both baseline and current content
                        let patch = match (baseline_text, current_text) {
                            (Some(baseline), Some(current)) => {
                                Some(generate_hunk_patch(baseline, current, hunk))
                            }
                            // New file: diff from empty
                            (None, Some(current)) => Some(generate_hunk_patch("", current, hunk)),
                            // Deleted file: diff to empty
                            (Some(baseline), None) => Some(generate_hunk_patch(baseline, "", hunk)),
                            (None, None) => None,
                        };

                        // Clone the hunk and add the patch
                        let mut hunk_with_patch = (**hunk).clone();
                        hunk_with_patch.patch = patch;
                        Arc::new(hunk_with_patch)
                    })
                    .collect();

                // Convert FileContentState to FileContentView (explicit status)
                let baseline = FileContentView::from_content_state(&state.baseline);
                let current = FileContentView::from_content_state(&state.current_content);

                // Legacy fields for backward compatibility (populated from views)
                let baseline_content = baseline.content.clone();
                let current_content = current.content.clone();

                FileHunkData {
                    hunks: hunks_with_patches,
                    baseline,
                    current,
                    baseline_content,
                    current_content,
                }
            })
            .unwrap_or_default()
    }

    /// Get all tracked file paths, regardless of source or remaining hunks.
    ///
    /// Returns every key in `file_states` — agent files, external edits, and
    /// fs_notify-detected changes alike.  Entries persist after the user
    /// accepts/rejects every hunk, making this suitable for file-discovery
    /// when replicating a worktree's changes back to the root repo.
    pub(super) fn get_all_tracked_paths(&self) -> Vec<PathBuf> {
        self.file_states.keys().cloned().collect()
    }

    /// Get hunks filtered by source.
    pub(super) fn get_hunks_by_source(&self, source: HunkSourceFilter) -> Vec<Arc<Hunk>> {
        self.file_states
            .values()
            .flat_map(|state| &state.hunks)
            .filter(|hunk| match source {
                HunkSourceFilter::Agent => hunk.source.is_agent_edit(),
                HunkSourceFilter::External => hunk.source.is_external(),
            })
            .cloned()
            .collect()
    }

    /// Get a specific hunk by ID.
    pub(super) fn get_hunk(&self, hunk_id: &HunkId) -> Option<Arc<Hunk>> {
        self.file_states
            .values()
            .flat_map(|state| &state.hunks)
            .find(|h| h.id == *hunk_id)
            .cloned()
    }

    /// Get hunks for a specific turn/prompt_index using the turn_index for O(1) lookup.
    pub(super) fn get_hunks_for_turn(&self, prompt_index: usize) -> Vec<Arc<Hunk>> {
        let Some(hunk_ids) = self.turn_index.get(&prompt_index) else {
            return vec![];
        };

        hunk_ids.iter().filter_map(|id| self.get_hunk(id)).collect()
    }

    /// Compute a complete session summary with stats and pending hunks grouped by turn.
    pub(super) fn compute_session_summary(&self) -> SessionSummary {
        let mut files_modified: FxHashSet<PathBuf> = FxHashSet::default();
        let mut files_with_pending: FxHashSet<PathBuf> = FxHashSet::default();

        // Temporary struct to accumulate per-turn data
        #[derive(Default)]
        struct TurnData {
            files: FxHashSet<PathBuf>,
            pending: Vec<Arc<Hunk>>,
            lines_added: usize,
            lines_removed: usize,
        }

        let mut by_prompt: FxHashMap<usize, TurnData> = FxHashMap::default();

        let mut unattributed_pending = 0;

        // Collect agent-attributed hunks for turn summaries and totals.
        // Track unattributed hunks separately (external edits, missing prompt_index).
        for (path, state) in &self.file_states {
            let mut has_agent_hunks = false;

            for hunk in &state.hunks {
                if let Some(prompt_index) = hunk.source.prompt_index() {
                    has_agent_hunks = true;
                    let entry = by_prompt.entry(prompt_index).or_default();
                    entry.files.insert(path.clone());
                    entry.pending.push(hunk.clone());
                    entry.lines_added += hunk.line_info.new_count;
                    entry.lines_removed += hunk.line_info.old_count;
                } else {
                    unattributed_pending += 1;
                }
            }

            if has_agent_hunks {
                files_modified.insert(path.clone());
                files_with_pending.insert(path.clone());
            }
        }

        // Build turn summaries (pending only)
        let mut turns: Vec<TurnSummary> = Vec::new();
        for (prompt_index, data) in by_prompt {
            turns.push(TurnSummary {
                prompt_index,
                files: data.files.into_iter().collect(),
                pending_hunks: data.pending,
                lines_added: data.lines_added,
                lines_removed: data.lines_removed,
            });
        }

        // Sort turns by prompt_index
        turns.sort_by_key(|t| t.prompt_index);

        // Compute pending totals from agent-attributed hunks only.
        let pending_hunks: usize = turns.iter().map(|t| t.pending_hunks.len()).sum();
        let pending_lines_added: usize = turns.iter().map(|t| t.lines_added).sum();
        let pending_lines_removed: usize = turns.iter().map(|t| t.lines_removed).sum();

        SessionSummary {
            stats: self.session_stats.clone(),
            turns,
            files_modified: files_modified.len(),
            files_with_pending: files_with_pending.len(),
            pending_hunks,
            pending_lines_added,
            pending_lines_removed,
            unattributed_pending,
        }
    }
}
