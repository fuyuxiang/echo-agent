//! Mutation commands for the HunkTrackerActor.
//!
//! These methods handle file changes and state mutations.
//!
//! ## Path Convention
//!
//! All paths in the hunk tracker are stored as **absolute paths**. This provides:
//! - Unambiguous file identification
//! - No need for working_dir context when processing paths
//! - Simpler path handling across the codebase
//!
//! Callers should pass absolute paths to `record_agent_write`, `handle_file_change`,
//! and `handle_file_deleted`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::events::{HunkEvent, HunkRemovalReason};
use crate::types::{HunkSource, TrackingMode};

use super::HunkTrackerActor;
use super::file_utils::{classify_string, missing_content, read_file_bounded};
use super::state::{FileContentState, FileHunkState};

/// Log-line prefix emitted only when [`HunkTrackerActor::refresh_all_baselines`]
/// runs a real scan. Test scan counters match on it; keep it the single source
/// of truth for the string.
pub const REFRESH_SCAN_LOG_PREFIX: &str = "refresh_all_baselines: completed in";

/// Log-line prefix for the unchanged-git-state skip path of
/// [`HunkTrackerActor::refresh_all_baselines`] (no scan ran).
pub const REFRESH_SKIP_LOG_PREFIX: &str = "refresh_all_baselines: git state unchanged";

/// Strip a single trailing newline (`\r\n` or `\n`) for equality comparison.
///
/// Git-stored content typically has exactly one trailing newline appended.
/// We strip only one to avoid falsely treating files with meaningful trailing
/// whitespace as clean. Bare `\r` (classic Mac) is intentionally out of scope.
fn strip_single_trailing_newline(content: &str) -> &str {
    content
        .strip_suffix("\r\n")
        .or_else(|| content.strip_suffix('\n'))
        .unwrap_or(content)
}

impl HunkTrackerActor {
    /// Record that an agent tool wrote to a file.
    ///
    /// # Arguments
    /// * `path` - Absolute path to the file
    /// * `content` - New file content
    /// * `prompt_index` - The prompt/turn index when this write occurred
    /// * `previous_content` - Content of the file before this write (if known).
    ///   Used as a fallback baseline when the file doesn't exist in git HEAD
    ///   (e.g., in worktrees created from dirty state where uncommitted files
    ///   were copied but aren't tracked by git).
    pub(super) async fn record_agent_write(
        &mut self,
        path: PathBuf,
        content: String,
        prompt_index: usize,
        previous_content: Option<String>,
    ) {
        let source = HunkSource::AgentEdit { prompt_index };

        // Classify current content into FileContentState (single classification, cloned for file_states)
        let current_state = classify_string(content.clone());
        let current_state_for_hunks = current_state.clone(); // Used by recompute_hunks below

        // Binary or TooLarge content: still track as an agent file (so
        // `get_all_tracked_paths` reports it for worktree replication)
        // but skip hunk computation — we don't diff these.
        if !current_state.is_diffable() {
            if !self.file_states.contains_key(&path) {
                // For new files, establish baseline from git or previous_content
                // Preserve previous_content when supplied (don't throw away available baseline)
                let baseline = if let Some(prev) = previous_content {
                    classify_string(prev)
                } else {
                    // Try git baseline, but don't block on it for large/binary writes
                    self.read_baseline(&path).await
                };
                self.file_states.insert(
                    path.clone(),
                    FileHunkState {
                        baseline,
                        current_content: current_state,
                        hunks: vec![],
                        is_agent_file: true,
                        baseline_accepted: false,
                    },
                );
                self.send_event(HunkEvent::FileAdded {
                    path,
                    is_agent_file: true,
                });
            } else if let Some(state) = self.file_states.get_mut(&path) {
                state.is_agent_file = true;
                state.current_content = current_state;
                // Clear hunks since content is not diffable
                state.hunks.clear();
            }
            return;
        }

        // Ensure file is tracked as an agent file
        let is_new_file = !self.file_states.contains_key(&path);

        if is_new_file {
            // First time seeing this file - establish baseline
            // For agent writes, path is already absolute
            let baseline = match self.read_baseline(&path).await {
                FileContentState::Missing => {
                    // File doesn't exist in git HEAD
                    // Use previous_content as fallback if available
                    previous_content
                        .map(classify_string)
                        .unwrap_or(missing_content())
                }
                other => other,
            };
            self.file_states.insert(
                path.clone(),
                FileHunkState {
                    baseline,
                    current_content: current_state,
                    hunks: vec![],
                    is_agent_file: true,
                    baseline_accepted: false,
                },
            );

            // Emit FileAdded event
            self.send_event(HunkEvent::FileAdded {
                path: path.clone(),
                is_agent_file: true,
            });
        } else {
            // Mark as agent file if not already
            if let Some(state) = self.file_states.get_mut(&path) {
                state.is_agent_file = true;
            }
        }

        // Recompute hunks with pre-classified content (no re-classification in recompute_hunks)
        self.recompute_hunks(&path, Some(current_state_for_hunks), source);
    }

    /// Handle a file change notification from fs_notify.
    pub(super) async fn handle_file_change(&mut self, path: PathBuf) {
        self.process_file_change(path, None).await;
    }

    /// Handle multiple file change notifications as a batch.
    ///
    /// Reads all needed baselines in a single call instead of per-file.
    pub(super) async fn handle_file_changes_batch(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }

        // Collect paths that need baseline reads:
        // - Untracked paths (need initial baseline)
        // - Tracked with baseline_accepted (need fresh baseline to detect restoration)
        let baseline_paths: Vec<PathBuf> = paths
            .iter()
            .filter(|p| {
                let tracked = self.file_states.contains_key(*p);
                if self.mode == TrackingMode::AgentOnly && !tracked {
                    return false;
                }
                !tracked
                    || self
                        .file_states
                        .get(*p)
                        .is_some_and(|s| s.baseline_accepted)
            })
            .cloned()
            .collect();

        let mut baselines = if baseline_paths.is_empty() {
            HashMap::new()
        } else {
            self.read_baselines_batch(&baseline_paths).await
        };

        for path in paths {
            let baseline = baselines.remove(&path);
            self.process_file_change(path, baseline).await;
        }
    }

    /// Shared implementation for processing a single file change.
    ///
    /// When `preloaded_baseline` is `Some`, uses the provided baseline.
    /// When `None`, reads the baseline from git on demand.
    async fn process_file_change(
        &mut self,
        path: PathBuf,
        mut preloaded_baseline: Option<FileContentState>,
    ) {
        let is_tracked = self.file_states.contains_key(&path);

        if self.mode == TrackingMode::AgentOnly && !is_tracked {
            return;
        }

        let current_state = read_file_bounded(&path).await;

        if !current_state.is_diffable() && !is_tracked {
            let baseline = if self.mode == TrackingMode::AllDirty {
                match preloaded_baseline.take() {
                    Some(b) => b,
                    None => self.read_baseline(&path).await,
                }
            } else {
                missing_content()
            };

            // No git baseline + not in dirty cache, gitignored; skip.
            // But allow directory paths through; they are legitimate fsnotify
            // entries used to discover files inside new directories
            // (inotify recursive-watch race recovery).
            if self.mode == TrackingMode::AllDirty
                && matches!(baseline, FileContentState::Missing)
                && !path.is_dir()
            {
                let rel = path.strip_prefix(&self.working_dir).unwrap_or(&path);
                if !self.git_dirty_cache.contains(rel) {
                    return;
                }
            }

            let has_diffable_baseline = baseline.is_diffable();
            self.file_states.insert(
                path.clone(),
                FileHunkState {
                    baseline,
                    current_content: current_state,
                    hunks: vec![],
                    is_agent_file: false,
                    baseline_accepted: false,
                },
            );
            self.send_event(HunkEvent::FileAdded {
                path: path.clone(),
                is_agent_file: false,
            });
            // If the baseline exists in HEAD but the file is missing/non-diffable
            // on disk, compute a deletion hunk (e.g., staged deletion after soft reset).
            if has_diffable_baseline {
                self.recompute_hunks(&path, None, HunkSource::External);
            }
            return;
        }

        if !is_tracked {
            let baseline = match preloaded_baseline.take() {
                Some(b) => b,
                None => self.read_baseline(&path).await,
            };

            // No git baseline + not in dirty cache → gitignored; skip.
            // But allow directory paths through (see comment above).
            if self.mode == TrackingMode::AllDirty
                && matches!(baseline, FileContentState::Missing)
                && !path.is_dir()
            {
                let rel = path.strip_prefix(&self.working_dir).unwrap_or(&path);
                if !self.git_dirty_cache.contains(rel) {
                    return;
                }
            }
            self.file_states.insert(
                path.clone(),
                FileHunkState {
                    baseline,
                    current_content: missing_content(), // Will be set by recompute_hunks
                    hunks: vec![],
                    is_agent_file: false,
                    baseline_accepted: false,
                },
            );
            self.send_event(HunkEvent::FileAdded {
                path: path.clone(),
                is_agent_file: false,
            });
        } else if self
            .file_states
            .get(&path)
            .is_some_and(|s| s.baseline_accepted)
        {
            // Existing file with accepted baseline — check if content was
            // restored to git HEAD (e.g., `git restore .`). If so, reset
            // baseline so the file appears clean.
            let git_head_state = match preloaded_baseline.take() {
                Some(b) => b,
                None => self.read_baseline(&path).await,
            };

            let content_matches_head = match (&current_state, &git_head_state) {
                (FileContentState::Full(current), FileContentState::Full(head)) => {
                    strip_single_trailing_newline(current) == strip_single_trailing_newline(head)
                }
                (FileContentState::Missing, FileContentState::Missing) => true,
                (FileContentState::Binary { .. }, FileContentState::Binary { .. }) => true,
                (FileContentState::TooLarge { .. }, FileContentState::TooLarge { .. }) => true,
                (FileContentState::LfsPointer { .. }, FileContentState::LfsPointer { .. }) => true,
                (FileContentState::Symlink, FileContentState::Symlink) => true,
                // Symlink on disk vs Full(target) in HEAD (or vice versa):
                // git stores symlinks as plain text blobs, so the types
                // differ even when the file is unchanged. Consult dirty cache.
                (FileContentState::Symlink, FileContentState::Full(_))
                | (FileContentState::Full(_), FileContentState::Symlink) => {
                    let rel = path.strip_prefix(&self.working_dir).unwrap_or(&path);
                    !self.git_dirty_cache.contains(rel)
                }
                _ => false,
            };

            if content_matches_head {
                if let Some(state) = self.file_states.get_mut(&path) {
                    state.baseline = git_head_state.clone();
                    state.current_content = git_head_state;
                    state.baseline_accepted = false;
                    state.hunks.clear();
                }
                return;
            }
        }

        let source = if self.file_states.get(&path).is_some_and(|s| s.is_agent_file) {
            HunkSource::ExternalEditOnAgentFile
        } else {
            HunkSource::External
        };

        self.recompute_hunks(&path, Some(current_state), source);
    }

    /// Handle a file deletion notification from fs_notify.
    ///
    /// Some git operations (e.g., `git restore .`) emit Remove events for
    /// files that are immediately re-created with different content. To
    /// avoid treating these as true deletions, we check whether the file
    /// still exists on disk before marking it as deleted.
    ///
    /// # Arguments
    /// * `path` - Absolute path to the deleted file
    pub(super) async fn handle_file_deleted(&mut self, path: PathBuf) {
        if !self.file_states.contains_key(&path) {
            // File not tracked by hunk tracker.
            // In AgentOnly mode, skip untracked files (same as handle_file_change).
            if self.mode == TrackingMode::AgentOnly {
                return;
            }

            // If the file still exists on disk, this is not a real deletion.
            if path.exists() {
                return;
            }

            // Check if the file exists in git HEAD. If so, this is a meaningful
            // deletion that should produce a deletion hunk (e.g., user ran
            // `rm foo.txt` on a committed file).
            let baseline = self.read_baseline(&path).await;
            if matches!(baseline, FileContentState::Missing) {
                return; // Not in HEAD either, nothing to track
            }

            // Seed file_states with baseline and Missing current content.
            self.file_states.insert(
                path.clone(),
                FileHunkState {
                    baseline,
                    current_content: missing_content(),
                    hunks: vec![],
                    is_agent_file: false,
                    baseline_accepted: false,
                },
            );
            self.send_event(HunkEvent::FileAdded {
                path: path.clone(),
                is_agent_file: false,
            });

            // Recompute hunks: (Full baseline, Missing current) -> file_deleted hunk
            self.recompute_hunks(&path, None, HunkSource::External);
            return;
        }

        // File IS tracked — existing logic below.

        // If the file still exists on disk, this was a replace (e.g.,
        // `git restore`), not a true deletion. Delegate to
        // handle_file_change which handles baseline refresh correctly.
        if path.exists() {
            self.handle_file_change(path).await;
            return;
        }

        let source = if self
            .file_states
            .get(&path)
            .map(|s| s.is_agent_file)
            .unwrap_or(false)
        {
            // User deleted a file the agent has touched
            HunkSource::ExternalEditOnAgentFile
        } else {
            HunkSource::External
        };

        // Set current_content to Missing (deleted) and recompute hunks.
        // Don't remove from file_states — the baseline still exists in HEAD.
        self.recompute_hunks(&path, None, source);
    }

    /// Reset baseline for a file (typically after commit).
    pub(super) fn reset_baseline(&mut self, path: &Path) {
        if let Some(state) = self.file_states.get_mut(path) {
            // Update baseline to current content
            state.baseline = state.current_content.clone();
            state.baseline_accepted = false;
            // Clear hunks since baseline == current
            let old_hunks = std::mem::take(&mut state.hunks);

            // Remove from turn_index and emit removed events for all hunks
            for hunk in old_hunks {
                if let Some(prompt_index) = hunk.source.prompt_index()
                    && let Some(set) = self.turn_index.get_mut(&prompt_index)
                {
                    set.remove(&hunk.id);
                }
                self.send_event(HunkEvent::HunkRemoved {
                    path: path.to_path_buf(),
                    hunk_id: hunk.id.clone(),
                    reason: HunkRemovalReason::Superseded,
                });
            }

            self.send_event(HunkEvent::BaselineUpdated {
                path: path.to_path_buf(),
            });
        }
    }

    /// Refresh all baselines from the current git HEAD and re-read current
    /// content from disk for every tracked file.
    ///
    /// This is called after a git HEAD/index change to reconcile stale
    /// state. For each tracked file:
    /// - Re-read baseline from the new HEAD
    /// - Re-read current content from disk
    /// - Recompute hunks
    /// - Drop files that are now clean (baseline == current, not agent files)
    pub(super) async fn refresh_all_baselines(&mut self) {
        self.refresh_all_baselines_except(&HashSet::new()).await;
    }

    /// Same as `refresh_all_baselines` but skips paths in `skip`. Returns
    /// immediately when AgentOnly has nothing tracked (no per-file work to do).
    pub(super) async fn refresh_all_baselines_except(&mut self, skip: &HashSet<PathBuf>) {
        // AgentOnly with nothing tracked has no work: it never auto-discovers,
        // and the dirty/staged caches are read only for tracked files. Skipping
        // avoids a full-worktree gix scan per git change. AllDirty must still
        // scan — that is how it discovers newly-dirty files.
        if self.mode == TrackingMode::AgentOnly && self.file_states.is_empty() {
            return;
        }

        let start = std::time::Instant::now();

        if let Some(repo_sync_state) = self.read_repo_sync_state().await {
            if repo_sync_state == self.repo_sync_state {
                debug!("{REFRESH_SKIP_LOG_PREFIX}, skipping refresh");
                return;
            }
            self.repo_sync_state = repo_sync_state;
        }

        // Refresh git dirty/staged caches BEFORE the main loop so the
        // is_clean check can consult them for non-diffable files (LFS,
        // binary, tooLarge). In AllDirty mode this also picks up newly
        // dirty files on the new branch, so it must scan the full worktree.
        // AgentOnly never auto-discovers and only consults the caches for
        // tracked paths, so the scan is scoped to them. Paths that can't be
        // made working-dir-relative are dropped: the caches are keyed
        // working-dir-relative, so such paths could never match a cache
        // entry anyway.
        let scope: Option<Vec<PathBuf>> = match self.mode {
            TrackingMode::AllDirty => None,
            TrackingMode::AgentOnly => Some(
                self.file_states
                    .keys()
                    .filter_map(|p| p.strip_prefix(&self.working_dir).ok())
                    .map(Path::to_path_buf)
                    .collect(),
            ),
        };
        match scope {
            // Every tracked path fell outside working_dir: nothing in scope
            // can ever hit the caches, and an empty pathspec list would mean
            // a FULL worktree scan in gix — the inversion of the intent — so
            // skip the scan. Clear the caches rather than keep them: their
            // entries predate the HEAD/index move that brought us here, and
            // the repo_sync_state committed above would short-circuit every
            // later refresh into serving those stale entries (get_staged_files,
            // staged flags). Consistent-empty matches the scope: the caches
            // describe nothing we track.
            Some(rels) if rels.is_empty() => {
                self.git_dirty_cache.clear();
                self.git_staged_cache.clear();
            }
            scope => self.refresh_git_dirty_cache(scope).await,
        }

        let paths: Vec<PathBuf> = self
            .file_states
            .keys()
            .filter(|p| !skip.contains(*p))
            .cloned()
            .collect();

        // Batch all git baseline reads into a single spawn_blocking call
        // to avoid per-file repo open / HEAD resolve overhead.
        let baselines = if paths.is_empty() {
            HashMap::new()
        } else {
            self.read_baselines_batch(&paths).await
        };

        const PARALLEL_READ_LIMIT: usize = 64;
        let mut current_contents: HashMap<PathBuf, FileContentState> = {
            let mut results = HashMap::with_capacity(paths.len());
            for chunk in paths.chunks(PARALLEL_READ_LIMIT) {
                let mut join_set = tokio::task::JoinSet::new();
                for path in chunk {
                    let p = path.clone();
                    join_set.spawn(async move {
                        let content = read_file_bounded(&p).await;
                        (p, content)
                    });
                }
                while let Some(result) = join_set.join_next().await {
                    match result {
                        Ok((path, content)) => {
                            results.insert(path, content);
                        }
                        Err(e) => debug!("parallel read task failed: {e}"),
                    }
                }
            }
            results
        };

        for path in paths {
            let new_baseline = baselines.get(&path).cloned().unwrap_or(missing_content());

            let new_current = current_contents.remove(&path).unwrap_or(missing_content());

            let Some(state) = self.file_states.get_mut(&path) else {
                continue;
            };

            let is_agent_file = state.is_agent_file;

            // Update baseline and current content (move ownership, no clones)
            state.baseline = new_baseline;
            state.current_content = new_current;
            state.baseline_accepted = false;

            // Check if file is now clean (baseline == current).
            // For Full states, compare text (ignoring trailing newline).
            // For non-diffable states (Binary/TooLarge/LFS): consult the git
            // dirty cache (refreshed above) — if git says the file is clean,
            // drop it from tracking to avoid phantom entries.
            // For Missing state: clean only if file doesn't exist.
            let is_clean = match (&state.baseline, &state.current_content) {
                (FileContentState::Full(b), FileContentState::Full(c)) => {
                    strip_single_trailing_newline(b) == strip_single_trailing_newline(c)
                }
                (FileContentState::Binary { .. }, FileContentState::Binary { .. })
                | (FileContentState::TooLarge { .. }, FileContentState::TooLarge { .. })
                | (FileContentState::LfsPointer { .. }, FileContentState::LfsPointer { .. })
                | (FileContentState::Symlink, FileContentState::Symlink) => {
                    // Non-diffable states with matching types: consult git dirty cache.
                    // The dirty cache was refreshed above so it reflects current HEAD.
                    let rel = path.strip_prefix(&self.working_dir).unwrap_or(&path);
                    !self.git_dirty_cache.contains(rel)
                }
                // LFS pointer baseline with different current content type (e.g. the
                // normal smudge case: baseline=pointer, current=binary). These are
                // NOT diffable but may be clean per git status. Consult dirty cache.
                (FileContentState::LfsPointer { .. }, _)
                | (_, FileContentState::LfsPointer { .. }) => {
                    let rel = path.strip_prefix(&self.working_dir).unwrap_or(&path);
                    !self.git_dirty_cache.contains(rel)
                }
                // Symlink on disk vs Full(target) in HEAD (or vice versa):
                // git stores symlinks as plain text blobs, so the types
                // differ even when the file is unchanged. Consult dirty cache.
                (FileContentState::Symlink, FileContentState::Full(_))
                | (FileContentState::Full(_), FileContentState::Symlink) => {
                    let rel = path.strip_prefix(&self.working_dir).unwrap_or(&path);
                    !self.git_dirty_cache.contains(rel)
                }
                (FileContentState::Missing, FileContentState::Missing) => !path.exists(),
                // Not in git HEAD + not in dirty cache → gitignored; clean.
                (FileContentState::Missing, _) => {
                    let rel = path.strip_prefix(&self.working_dir).unwrap_or(&path);
                    !self.git_dirty_cache.contains(rel)
                }
                _ => false,
            };

            if is_clean && !is_agent_file {
                // File is clean and not an agent file — stop tracking it
                let old_state = self.file_states.remove(&path).unwrap();

                // Clean up turn_index and emit removal events
                for hunk in &old_state.hunks {
                    if let Some(prompt_index) = hunk.source.prompt_index()
                        && let Some(set) = self.turn_index.get_mut(&prompt_index)
                    {
                        set.remove(&hunk.id);
                    }
                    self.send_event(HunkEvent::HunkRemoved {
                        path: path.clone(),
                        hunk_id: hunk.id.clone(),
                        reason: HunkRemovalReason::Superseded,
                    });
                }
                self.send_event(HunkEvent::FileRemoved { path: path.clone() });

                debug!("refresh_all_baselines: dropped clean file {:?}", path);
            } else if is_clean && is_agent_file {
                // Agent file is clean — clear hunks but keep tracking
                let old_hunks = std::mem::take(&mut state.hunks);
                for hunk in &old_hunks {
                    if let Some(prompt_index) = hunk.source.prompt_index()
                        && let Some(set) = self.turn_index.get_mut(&prompt_index)
                    {
                        set.remove(&hunk.id);
                    }
                    self.send_event(HunkEvent::HunkRemoved {
                        path: path.clone(),
                        hunk_id: hunk.id.clone(),
                        reason: HunkRemovalReason::Superseded,
                    });
                }
                self.send_event(HunkEvent::BaselineUpdated { path: path.clone() });

                debug!("refresh_all_baselines: agent file {:?} is now clean", path);
            } else {
                // File still has diffs — recompute hunks
                // Determine source: preserve agent attribution if it's an agent file
                let source = if is_agent_file {
                    HunkSource::ExternalEditOnAgentFile
                } else {
                    HunkSource::External
                };

                // Pass current_content directly (already FileContentState, no re-classification)
                let current = self
                    .file_states
                    .get(&path)
                    .map(|s| s.current_content.clone());
                self.recompute_hunks(&path, current, source);

                self.send_event(HunkEvent::BaselineUpdated { path: path.clone() });

                debug!("refresh_all_baselines: recomputed hunks for {:?}", path);
            }
        }

        debug!("{REFRESH_SCAN_LOG_PREFIX} {:?}", start.elapsed());
    }

    /// Set tracking mode.
    pub(super) async fn set_mode(&mut self, mode: TrackingMode) {
        let old_mode = self.mode;
        self.mode = mode;

        if old_mode == TrackingMode::AgentOnly && mode == TrackingMode::AllDirty {
            // Switching to AllDirty - refresh git cache and track dirty files
            self.refresh_git_dirty_cache(None).await;
        } else if old_mode == TrackingMode::AllDirty && mode == TrackingMode::AgentOnly {
            // Switching to AgentOnly - remove non-agent files
            let non_agent_paths: Vec<PathBuf> = self
                .file_states
                .iter()
                .filter(|(_, state)| !state.is_agent_file)
                .map(|(path, _)| path.clone())
                .collect();

            for path in non_agent_paths {
                if let Some(state) = self.file_states.remove(&path) {
                    // Remove from turn_index and emit removed events for all hunks
                    for hunk in state.hunks {
                        if let Some(prompt_index) = hunk.source.prompt_index()
                            && let Some(set) = self.turn_index.get_mut(&prompt_index)
                        {
                            set.remove(&hunk.id);
                        }
                        self.send_event(HunkEvent::HunkRemoved {
                            path: path.clone(),
                            hunk_id: hunk.id.clone(),
                            reason: HunkRemovalReason::Superseded,
                        });
                    }
                    self.send_event(HunkEvent::FileRemoved { path });
                }
            }
        }
    }
}
