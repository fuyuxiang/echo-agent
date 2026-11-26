//! Git operations for the HunkTrackerActor.
//!
//! Uses `gix` (pure-Rust) instead of `git2` (libgit2 C bindings) to avoid
//! global lock contention in libiconv on macOS when multiple sessions run
//! parallel git operations.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gix::bstr::BString;

use crate::types::TrackingMode;

use super::HunkTrackerActor;
use super::file_utils::{classify_bytes, missing_content};
use super::state::{FileContentState, GitRepoState, RepoSyncState};

/// Open or discover a gix repository depending on cached state.
///
/// When a `ThreadSafeRepository` is already cached (`Discovered`), this calls
/// `.to_thread_local()` which is a cheap `Arc` clone — no config parsing, no
/// HEAD resolution, no filesystem discovery. On first call (`Unknown`), it
/// runs `gix::discover()` and converts the result to a `ThreadSafeRepository`
/// for caching.
///
/// Returns `(repo, prefix, discovered)` where `discovered` is `Some` only when
/// this was the first discovery attempt (so the caller can cache the result).
#[allow(clippy::type_complexity)]
fn open_or_discover(
    cached_state: &GitRepoState,
    working_dir: &Path,
) -> Option<(
    gix::Repository,
    PathBuf,
    Option<Result<(Arc<gix::ThreadSafeRepository>, PathBuf), ()>>,
)> {
    match cached_state {
        GitRepoState::Discovered { repo, prefix } => {
            let thread_local = repo.to_thread_local();
            Some((thread_local, prefix.clone(), None))
        }
        GitRepoState::Unknown => {
            let repo = gix::discover(working_dir).ok()?;
            let repo_root = repo.workdir()?.to_path_buf();

            // Canonicalize both paths to handle symlinks (e.g., /var -> /private/var on macOS)
            let canonical_working_dir =
                dunce::canonicalize(working_dir).unwrap_or_else(|_| working_dir.to_path_buf());
            let canonical_repo_root = dunce::canonicalize(&repo_root).unwrap_or(repo_root);

            let prefix = canonical_working_dir
                .strip_prefix(&canonical_repo_root)
                .ok()?
                .to_path_buf();

            // Convert to ThreadSafeRepository for caching, then get a
            // thread-local handle for this call.
            let sync_repo = Arc::new(repo.into_sync());
            let thread_local = sync_repo.to_thread_local();

            let discovered = Some(Ok((sync_repo, prefix.clone())));
            Some((thread_local, prefix, discovered))
        }
        GitRepoState::NotARepo => None,
    }
}

/// Canonicalize a path, falling back to canonicalizing the parent directory
/// when the file itself doesn't exist (e.g., deleted files on macOS where
/// `/var` -> `/private/var`).
fn canonicalize_or_parent(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| {
        path.parent()
            .and_then(|p| dunce::canonicalize(p).ok())
            .and_then(|cp| path.file_name().map(|f| cp.join(f)))
            .unwrap_or_else(|| path.to_path_buf())
    })
}

impl HunkTrackerActor {
    /// Refresh git dirty cache and staged cache by querying git status.
    /// Uses the combined status iterator to get both index→worktree (dirty)
    /// and HEAD→index (staged) changes in a single pass.
    /// In AllDirty mode, this also starts tracking all dirty files.
    ///
    /// `scope` limits the scan to the given working-dir-relative paths:
    /// pathspecs prune the untracked dirwalk and the index-entry walk and
    /// filter the tree-index diff — but that diff still materializes the
    /// full HEAD-tree index per call (gix limitation), an O(repo) floor.
    /// `None` — and, by gix semantics, an empty list — scans the full
    /// worktree, so callers wanting "scan nothing" must skip the call.
    pub(super) async fn refresh_git_dirty_cache(&mut self, scope: Option<Vec<PathBuf>>) {
        // Early return if we already know this isn't a git repo
        if matches!(self.git_repo_state, GitRepoState::NotARepo) {
            return;
        }

        // Clone state needed by the blocking task
        let working_dir = self.working_dir.clone();
        let cached_state = self.git_repo_state.clone();

        // Result includes dirty files, staged files, and optionally newly-discovered repo info
        struct GitResult {
            dirty_files: HashSet<PathBuf>,
            staged_files: HashSet<PathBuf>,
            /// If we did discovery, include the result so actor can cache it
            discovered: Option<Result<(Arc<gix::ThreadSafeRepository>, PathBuf), ()>>,
        }

        let task_result = tokio::task::spawn_blocking(move || {
            let Some((repo, prefix, discovered)) = open_or_discover(&cached_state, &working_dir)
            else {
                return GitResult {
                    dirty_files: HashSet::new(),
                    staged_files: HashSet::new(),
                    discovered: if matches!(cached_state, GitRepoState::Unknown) {
                        Some(Err(()))
                    } else {
                        None
                    },
                };
            };

            // Guard against empty index files — gix-index panics when the file
            // is 0 bytes because it tries to slice the trailing hash from an
            // empty mmap (integer underflow in the slice range).
            let index_path = repo.git_dir().join("index");
            if index_path.metadata().map_or(true, |m| m.len() == 0) {
                tracing::debug!("index file is empty or missing, skipping git status");
                return GitResult {
                    dirty_files: HashSet::new(),
                    staged_files: HashSet::new(),
                    discovered,
                };
            }

            let mut dirty_files = HashSet::new();
            let mut staged_files = HashSet::new();

            // Cap produce workers: gix-features spawn-EAGAIN aborts under panic=abort.
            let status = match repo.status(gix::progress::Discard) {
                Ok(s) => echo_agent_gix_status::with_budgeted_thread_limit(s)
                    .untracked_files(gix::status::UntrackedFiles::Files),
                Err(_) => {
                    // Git status failed - return empty but keep cache
                    return GitResult {
                        dirty_files: HashSet::new(),
                        staged_files: HashSet::new(),
                        discovered,
                    };
                }
            };

            // `:(top)` anchors each pathspec to the repo root — without it gix
            // prepends a process-cwd-derived prefix (`Repository::prefix`),
            // which is unrelated to this actor's working_dir. `literal` stops
            // path bytes from being interpreted as globs. `into_bstr` is
            // byte-preserving on unix; on Windows it requires UTF-8 (a panic
            // there aborts under panic=abort; with unwind it is a JoinError and
            // we keep previous caches). Separators must be `/` for gix paths.
            let pathspecs: Vec<BString> = scope
                .map(|rels| {
                    rels.iter()
                        .map(|rel| {
                            let repo_rel = gix::path::to_unix_separators_on_windows(
                                gix::path::into_bstr(prefix.join(rel)),
                            );
                            let mut spec = BString::from(":(top,literal)");
                            spec.extend_from_slice(&repo_rel);
                            spec
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Use the combined iterator which yields both:
            // - Item::IndexWorktree: index vs worktree changes (dirty/untracked)
            // - Item::TreeIndex: HEAD vs index changes (staged)
            let iter = match status.into_iter(pathspecs) {
                Ok(it) => it,
                Err(_) => {
                    return GitResult {
                        dirty_files: HashSet::new(),
                        staged_files: HashSet::new(),
                        discovered,
                    };
                }
            };

            for item_result in iter {
                let Ok(item) = item_result else {
                    continue;
                };

                let path_str = item.location().to_string();
                let path = PathBuf::from(&path_str);

                // Only include files under our working_dir, and make them relative to it
                if let Ok(relative_path) = path.strip_prefix(&prefix) {
                    let rel = relative_path.to_path_buf();
                    dirty_files.insert(rel.clone());

                    // TreeIndex items represent HEAD→index changes (staged)
                    if matches!(&item, gix::status::Item::TreeIndex(_)) {
                        staged_files.insert(rel);
                    }
                }
            }

            GitResult {
                dirty_files,
                staged_files,
                discovered,
            }
        })
        .await;

        let Ok(git_result) = task_result else {
            // spawn_blocking was cancelled or panicked
            return;
        };

        // Update cached repo state if we did discovery
        if let Some(discovery_result) = git_result.discovered {
            self.git_repo_state = match discovery_result {
                Ok((repo, prefix)) => GitRepoState::Discovered { repo, prefix },
                Err(()) => GitRepoState::NotARepo,
            };
        }

        // Collect paths to track (files not yet being tracked)
        // dirty_files contains relative paths, but file_states uses absolute paths
        let paths_to_track: Vec<PathBuf> = git_result
            .dirty_files
            .iter()
            .filter(|relative_path| {
                let abs_path = self.working_dir.join(relative_path);
                !self.file_states.contains_key(&abs_path)
            })
            .cloned()
            .collect();

        self.git_dirty_cache = git_result.dirty_files;
        self.git_staged_cache = git_result.staged_files;

        // In AllDirty mode, start tracking all dirty files that aren't already tracked
        if self.mode == TrackingMode::AllDirty {
            for relative_path in paths_to_track {
                // Convert relative path to absolute for handle_file_change
                let abs_path = self.working_dir.join(&relative_path);
                self.handle_file_change(abs_path).await;
            }
        }
    }

    /// Read the current git HEAD OID and index mtime.
    pub(super) async fn read_repo_sync_state(&mut self) -> Option<RepoSyncState> {
        // Early return if we already know this isn't a git repo
        if matches!(self.git_repo_state, GitRepoState::NotARepo) {
            return None;
        }

        let working_dir = self.working_dir.clone();
        let cached_state = self.git_repo_state.clone();

        struct SyncResult {
            sync_state: Option<RepoSyncState>,
            discovered: Option<Result<(Arc<gix::ThreadSafeRepository>, PathBuf), ()>>,
        }

        let task_result = tokio::task::spawn_blocking(move || {
            let Some((repo, _prefix, discovered)) = open_or_discover(&cached_state, &working_dir)
            else {
                return SyncResult {
                    sync_state: None,
                    discovered: if matches!(cached_state, GitRepoState::Unknown) {
                        Some(Err(()))
                    } else {
                        None
                    },
                };
            };

            let head_oid = repo
                .head()
                .ok()
                .and_then(|mut head| head.peel_to_commit().ok())
                .map(|commit| commit.id().to_string());

            let index_mtime = repo
                .git_dir()
                .join("index")
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok());

            SyncResult {
                sync_state: Some(RepoSyncState {
                    head_oid,
                    index_mtime,
                }),
                discovered,
            }
        })
        .await;

        match task_result {
            Ok(result) => {
                if let Some(discovery_result) = result.discovered {
                    self.git_repo_state = match discovery_result {
                        Ok((repo, prefix)) => GitRepoState::Discovered { repo, prefix },
                        Err(()) => GitRepoState::NotARepo,
                    };
                }
                result.sync_state
            }
            Err(_) => None,
        }
    }

    /// Read baseline content from git HEAD.
    ///
    /// # Arguments
    /// * `path` - Absolute path to the file
    ///
    /// Returns FileContentState::Missing if file doesn't exist in HEAD,
    /// FileContentState::Binary/TooLarge for non-text or large files,
    /// FileContentState::Full for text content within size limits.
    pub(super) async fn read_baseline(&mut self, path: &Path) -> FileContentState {
        // Early return if we already know this isn't a git repo
        if matches!(self.git_repo_state, GitRepoState::NotARepo) {
            return missing_content();
        }

        let working_dir = self.working_dir.clone();
        let abs_path = path.to_path_buf();
        let cached_state = self.git_repo_state.clone();

        // Result includes content and optionally newly-discovered repo info
        struct BaselineResult {
            content: FileContentState,
            /// If we did discovery, include the result so actor can cache it
            discovered: Option<Result<(Arc<gix::ThreadSafeRepository>, PathBuf), ()>>,
        }

        let task_result = tokio::task::spawn_blocking(move || {
            // Clone working_dir upfront for later use in path conversion
            let working_dir_for_strip = working_dir.clone();

            let Some((repo, prefix, discovered)) = open_or_discover(&cached_state, &working_dir)
            else {
                return BaselineResult {
                    content: missing_content(),
                    discovered: if matches!(cached_state, GitRepoState::Unknown) {
                        Some(Err(()))
                    } else {
                        None
                    },
                };
            };

            // Convert absolute path to working_dir-relative, then to repo-root-relative
            // Canonicalize to handle symlinks (e.g., /var -> /private/var on macOS).
            let canonical_abs_path = canonicalize_or_parent(&abs_path);
            let canonical_working_dir =
                dunce::canonicalize(&working_dir_for_strip).unwrap_or(working_dir_for_strip);

            let working_dir_relative = match canonical_abs_path.strip_prefix(&canonical_working_dir)
            {
                Ok(rel) => rel.to_path_buf(),
                Err(_) => {
                    // Path is not under working_dir - shouldn't happen but handle gracefully
                    return BaselineResult {
                        content: missing_content(),
                        discovered,
                    };
                }
            };

            let repo_relative_path = prefix.join(&working_dir_relative);

            let content = (|| {
                let head = repo.head().ok()?.peel_to_commit().ok()?;
                let tree = head.tree().ok()?;
                let entry = tree
                    .lookup_entry_by_path(repo_relative_path.to_string_lossy().as_ref())
                    .ok()??;
                // Symlinks in git have mode 120000; return Symlink before reading blob.
                if entry.mode().is_link() {
                    return Some(FileContentState::Symlink);
                }
                let object = entry.object().ok()?;
                let blob = object.try_into_blob().ok()?;

                // Classify bytes into FileContentState (handles binary, size limits)
                Some(classify_bytes(&blob.data))
            })();

            BaselineResult {
                content: content.unwrap_or_else(missing_content),
                discovered,
            }
        })
        .await;

        match task_result {
            Ok(result) => {
                // Update cached repo state if we did discovery
                if let Some(discovery_result) = result.discovered {
                    self.git_repo_state = match discovery_result {
                        Ok((repo, prefix)) => GitRepoState::Discovered { repo, prefix },
                        Err(()) => GitRepoState::NotARepo,
                    };
                }
                result.content
            }
            Err(_) => missing_content(), // spawn_blocking was cancelled or panicked
        }
    }

    /// Read baseline content from git HEAD for multiple files in a single
    /// `spawn_blocking` call. Opens the repo and resolves HEAD once, then
    /// looks up every path in the same tree, avoiding the per-file overhead
    /// of `read_baseline`.
    ///
    /// Returns a map from absolute path to its baseline content state.
    /// FileContentState::Missing for files not in HEAD,
    /// FileContentState::Binary/TooLarge for non-text or large files,
    /// FileContentState::Full for text content within size limits.
    pub(super) async fn read_baselines_batch(
        &mut self,
        paths: &[PathBuf],
    ) -> HashMap<PathBuf, FileContentState> {
        if paths.is_empty() || matches!(self.git_repo_state, GitRepoState::NotARepo) {
            return HashMap::new();
        }

        let working_dir = self.working_dir.clone();
        let cached_state = self.git_repo_state.clone();
        let paths_owned: Vec<PathBuf> = paths.to_vec();

        struct BatchResult {
            baselines: HashMap<PathBuf, FileContentState>,
            discovered: Option<Result<(Arc<gix::ThreadSafeRepository>, PathBuf), ()>>,
        }

        let task_result = tokio::task::spawn_blocking(move || {
            let Some((repo, prefix, discovered)) = open_or_discover(&cached_state, &working_dir)
            else {
                return BatchResult {
                    baselines: paths_owned
                        .iter()
                        .map(|p| (p.clone(), missing_content()))
                        .collect(),
                    discovered: if matches!(cached_state, GitRepoState::Unknown) {
                        Some(Err(()))
                    } else {
                        None
                    },
                };
            };

            let canonical_working_dir = dunce::canonicalize(&working_dir).unwrap_or(working_dir);

            // Resolve HEAD tree once for all lookups
            let tree = (|| {
                let head = repo.head().ok()?.peel_to_commit().ok()?;
                head.tree().ok()
            })();

            let baselines = paths_owned
                .iter()
                .map(|abs_path| {
                    let content = tree.as_ref().and_then(|tree| {
                        let canonical = canonicalize_or_parent(abs_path);
                        let wd_relative = canonical.strip_prefix(&canonical_working_dir).ok()?;
                        let repo_relative = prefix.join(wd_relative);
                        let entry = tree
                            .lookup_entry_by_path(repo_relative.to_string_lossy().as_ref())
                            .ok()??;
                        if entry.mode().is_link() {
                            return Some(FileContentState::Symlink);
                        }
                        let object = entry.object().ok()?;
                        let blob = object.try_into_blob().ok()?;
                        Some(classify_bytes(&blob.data))
                    });
                    (abs_path.clone(), content.unwrap_or_else(missing_content))
                })
                .collect();

            BatchResult {
                baselines,
                discovered,
            }
        })
        .await;

        match task_result {
            Ok(result) => {
                if let Some(discovery_result) = result.discovered {
                    self.git_repo_state = match discovery_result {
                        Ok((repo, prefix)) => GitRepoState::Discovered { repo, prefix },
                        Err(()) => GitRepoState::NotARepo,
                    };
                }
                result.baselines
            }
            Err(_) => HashMap::new(),
        }
    }
}
