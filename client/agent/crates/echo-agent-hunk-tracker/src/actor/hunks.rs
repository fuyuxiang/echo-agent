//! Hunk recomputation and diff event emission for the HunkTrackerActor.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use crate::diff::{
    compute_hunks, find_matching_old_hunk, hunk_moved, hunks_match_content, hunks_overlap,
};
use crate::events::{HunkEvent, HunkRemovalReason};
use crate::types::{Hunk, HunkId, HunkSource};

use super::HunkTrackerActor;
use super::file_utils::missing_content;
use super::state::FileContentState;

impl HunkTrackerActor {
    /// Recompute hunks for a file and emit events.
    /// Also updates the turn_index for O(1) prompt_index lookup.
    ///
    /// **Invariant:** `source` must reflect a single edit origin per call.
    /// The actor processes commands sequentially (one at a time via
    /// `cmd_rx.recv()`), so agent writes (`RecordAgentWrite`) and external
    /// edits (`HandleFileChange`) never share the same invocation. All
    /// `HunkContentChanged` events emitted from one call therefore share
    /// the same `trigger_source`, which is correct.
    ///
    /// Takes `Option<FileContentState>` to preserve explicit Binary/TooLarge states
    /// through recomputation.
    pub(super) fn recompute_hunks(
        &mut self,
        path: &Path,
        new_current: Option<FileContentState>,
        source: HunkSource,
    ) {
        let Some(state) = self.file_states.get_mut(path) else {
            return;
        };

        let old_hunks = std::mem::take(&mut state.hunks);

        // Update current_content with the new state (preserves Binary/TooLarge)
        state.current_content = new_current.unwrap_or_else(missing_content);

        // Compute new hunks from baseline vs current
        // Only diff Full vs Full states; clear hunks for non-diffable states
        let mut new_hunks = match (&state.baseline, &state.current_content) {
            // Both Full - normal diff
            (FileContentState::Full(baseline), FileContentState::Full(current)) => {
                compute_hunks(path, baseline, current, source)
            }
            // Baseline Full, current deleted - whole file deleted
            (FileContentState::Full(baseline), FileContentState::Missing) => {
                if baseline.is_empty() {
                    vec![]
                } else {
                    vec![Hunk::file_deleted(
                        path.to_path_buf(),
                        baseline.clone(),
                        source,
                    )]
                }
            }
            // Baseline Full, current TooLarge/Binary/LfsPointer - can't diff, clear hunks
            (FileContentState::Full(_), FileContentState::TooLarge { .. })
            | (FileContentState::Full(_), FileContentState::Binary { .. })
            | (FileContentState::Full(_), FileContentState::LfsPointer { .. }) => {
                vec![]
            }
            // No baseline (Missing), current Full - whole file added
            (FileContentState::Missing, FileContentState::Full(current)) => {
                if current.is_empty() {
                    vec![]
                } else {
                    vec![Hunk::file_created(
                        path.to_path_buf(),
                        current.clone(),
                        source,
                    )]
                }
            }
            // Baseline TooLarge/Binary/LfsPointer, current Full - can't diff baseline, clear hunks
            (FileContentState::TooLarge { .. }, FileContentState::Full(_))
            | (FileContentState::Binary { .. }, FileContentState::Full(_))
            | (FileContentState::LfsPointer { .. }, FileContentState::Full(_)) => {
                vec![]
            }
            // All other combinations - no hunks (both non-Full, or both Missing, etc.)
            _ => vec![],
        };

        // Preserve hunk IDs and sources from matching old hunks
        // Track claimed old hunk IDs to prevent duplicates when one old hunk splits into multiple
        let mut claimed_old_ids: HashSet<HunkId> = HashSet::new();

        for new_hunk in &mut new_hunks {
            if let Some(best_match) = find_matching_old_hunk(new_hunk, &old_hunks) {
                // Skip if this old hunk was already claimed by another new hunk
                if claimed_old_ids.contains(&best_match.id) {
                    continue; // new_hunk keeps its new ID
                }

                claimed_old_ids.insert(best_match.id.clone());

                // Always preserve hunk ID for continuity
                new_hunk.id = best_match.id.clone();

                // Source preservation logic:
                // - If new edit is from agent: keep new source (latest prompt_index wins)
                // - If new edit is external but old was agent: preserve agent attribution
                // - Otherwise: keep new source
                if new_hunk.source.is_external() && best_match.source.is_agent_edit() {
                    new_hunk.source = best_match.source;
                }
                // else: keep new_hunk.source as-is (agent edits always update attribution)
            }
        }

        // Wrap hunks in Arc for cheap cloning
        let arc_hunks: Vec<Arc<Hunk>> = new_hunks.iter().cloned().map(Arc::new).collect();
        state.hunks = arc_hunks.clone();

        // Update turn_index: remove old hunk IDs, add new ones
        for old_hunk in &old_hunks {
            if let Some(prompt_index) = old_hunk.source.prompt_index()
                && let Some(set) = self.turn_index.get_mut(&prompt_index)
            {
                set.remove(&old_hunk.id);
            }
        }
        for new_hunk in &arc_hunks {
            if let Some(prompt_index) = new_hunk.source.prompt_index() {
                self.turn_index
                    .entry(prompt_index)
                    .or_default()
                    .insert(new_hunk.id.clone());
            }
        }

        // Diff old hunks vs new hunks → emit Added/Removed/Moved/ContentChanged events
        self.emit_hunk_diff_events(path, &old_hunks, &arc_hunks, source);
    }

    /// Emit events for the difference between old and new hunks.
    ///
    /// `trigger_source` is the source of the edit that triggered this recomputation
    /// (before any source-preservation logic). It is forwarded to `HunkContentChanged`
    /// so that LOC tracking can attribute in-place changes to the correct author.
    fn emit_hunk_diff_events(
        &self,
        path: &Path,
        old_hunks: &[Arc<Hunk>],
        new_hunks: &[Arc<Hunk>],
        trigger_source: HunkSource,
    ) {
        // Find removed hunks (in old but no overlap with any new hunk)
        // A hunk is removed only if it doesn't overlap with any new hunk
        for old_hunk in old_hunks {
            let has_overlap = new_hunks
                .iter()
                .any(|n| hunks_overlap(old_hunk, n) || hunks_match_content(old_hunk, n));
            if !has_overlap {
                self.send_event(HunkEvent::HunkRemoved {
                    path: path.to_path_buf(),
                    hunk_id: old_hunk.id.clone(),
                    reason: HunkRemovalReason::Superseded,
                });
            }
        }

        // Find added and moved hunks
        for new_hunk in new_hunks {
            // Check for exact content match first
            let exact_match = old_hunks.iter().find(|o| hunks_match_content(o, new_hunk));

            match exact_match {
                Some(old_hunk) if hunk_moved(old_hunk, new_hunk) => {
                    self.send_event(HunkEvent::HunkMoved {
                        path: path.to_path_buf(),
                        hunk_id: old_hunk.id.clone(),
                        new_line_info: new_hunk.line_info.clone(),
                    });
                }
                Some(_) => {
                    // Same content, same position - no event needed
                }
                None => {
                    // No exact match - check if there's any overlap
                    let has_overlap = old_hunks.iter().any(|o| hunks_overlap(o, new_hunk));
                    if !has_overlap {
                        // Truly new hunk with no relation to old hunks
                        self.send_event(HunkEvent::HunkAdded {
                            path: path.to_path_buf(),
                            hunk: new_hunk.clone(),
                        });
                    } else {
                        // Hunk grew/merged/changed in place — ID was already
                        // preserved in recompute_hunks. Find the matching old
                        // hunk by ID to get previous line counts for delta
                        // computation. Fall back to the overlapping hunk if
                        // no ID match (e.g., hunk split/merge scenarios).
                        let prev = old_hunks
                            .iter()
                            .find(|o| o.id == new_hunk.id)
                            .or_else(|| old_hunks.iter().find(|o| hunks_overlap(o, new_hunk)));
                        let (prev_lines_added, prev_lines_removed) = prev
                            .map(|h| (h.line_info.new_count, h.line_info.old_count))
                            .unwrap_or((0, 0));

                        self.send_event(HunkEvent::HunkContentChanged {
                            path: path.to_path_buf(),
                            hunk: new_hunk.clone(),
                            trigger_source,
                            prev_lines_added,
                            prev_lines_removed,
                        });
                    }
                }
            }
        }
    }
}
