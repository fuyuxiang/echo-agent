//! Sync a pre-created worktree to match a source repo's current state.
//!
//! This module provides the [`WorktreeSync`] API used by the worktree pool
//! to bring a pre-allocated worktree up to date with the source repo's HEAD
//! and dirty working tree state. It is designed for **linked** worktrees
//! (shared object store), so `git reset --hard <commit>` always succeeds
//! regardless of how far HEAD has diverged.
//!
//! All git operations use `gix` for HEAD resolution and `git` CLI for
//! mutations (`reset`, `clean`, `status`). File copies use
//! `reflink_copy::reflink_or_copy()` for CoW efficiency.
//!
//! ## Why CLI `git status` instead of `gix::status()`?
//!
//! The existing `git::status::get_modified_files()` uses `gix` natively via
//! `repo.status()` / `index_worktree_iter`. However, it only reports
//! *worktree-vs-index* differences — it does not distinguish the **staged
//! (index) column** from the **worktree column** (the `XY` pair in porcelain
//! output). For `sync_dirty_state` we need the full `XY` semantics to
//! correctly handle cases like `XY = "D "` (staged deletion, file still on
//! disk) vs `XY = " D"` (worktree deletion). The porcelain v2 format gives
//! us both columns, rename detection with `origPath`, and unmerged entries
//! — all NUL-delimited for safe path handling.

use std::path::Path;

use anyhow::{Context, Result};
use bytes::Bytes;

use crate::copy::cow::{clone_file, replace_symlink};
use crate::git::checkout::{git_clean_fd, git_command, git_reset_hard_command};
use crate::git::discovery::get_head_commit;

/// Pre-collected dirty state from a source repository.
///
/// Holds the raw output of `git status --porcelain=v2 -z --untracked-files=all`.
/// Collect once via [`collect_source_dirty_state`], then pass to multiple
/// [`WorktreeSync::sync_from_precomputed`] calls to avoid redundant `git status`
/// invocations (each takes ~1.4s on large repos).
///
/// Uses [`Bytes`] internally so cloning is a cheap ref-count bump rather
/// than a full buffer copy — important when sharing across multiple syncs.
#[derive(Clone, Debug)]
pub struct SourceDirtyState {
    /// Raw NUL-delimited porcelain v2 output. Empty means the source is clean.
    raw: Bytes,
}

impl SourceDirtyState {
    /// Returns `true` if the source has no dirty files (nothing to sync).
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }
}

/// Collect dirty state from a source repository.
///
/// Runs `git status --porcelain=v2 -z --untracked-files=all` on the source
/// and captures the output. The result can be shared across multiple
/// [`WorktreeSync::sync_from_precomputed`] calls.
///
/// This is a **blocking** function — call from `spawn_blocking` in async contexts.
pub fn collect_source_dirty_state(source: &Path) -> Result<SourceDirtyState> {
    let output = git_command()
        .args(["status", "--porcelain=v2", "-z", "--untracked-files=all"])
        .current_dir(source)
        .output()
        .context("failed to run git status")?;

    if !output.status.success() {
        anyhow::bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(SourceDirtyState {
        raw: Bytes::from(output.stdout),
    })
}

/// Report from a sync operation.
#[derive(Clone, Debug, Default)]
pub struct SyncReport {
    /// Whether HEAD was moved (git reset --hard was needed).
    pub head_moved: bool,
    /// Number of dirty files replicated from source.
    pub dirty_files_copied: u64,
    /// Number of files deleted in destination to match source deletions.
    pub files_deleted: u64,
    /// Number of staged entries replicated via `git add`/`git rm --cached`.
    pub staged_entries: u64,
    /// Whether `git clean` was skipped (worktree known to be clean).
    pub clean_skipped: bool,
    /// Whether dirty sync was skipped because pre-computed state was empty.
    pub dirty_skipped: bool,

    // ── Per-phase timing (milliseconds) ─────────────────────────────────
    /// Time to resolve HEAD commits on source + worktree (gix).
    pub head_resolve_ms: u64,
    /// Time for `git reset --hard` (0 if HEAD didn't move).
    pub reset_hard_ms: u64,
    /// Time for `git clean -fd` (0 if skipped).
    pub clean_ms: u64,
    /// Time for dirty-state sync: `git status` + file copy/delete + staged replay.
    pub dirty_sync_ms: u64,
    /// Time for just the `git status` call inside dirty sync (subset of dirty_sync_ms).
    pub git_status_ms: u64,
    /// Time for file copy/delete operations (subset of dirty_sync_ms).
    pub file_ops_ms: u64,
    /// Time for staged change replay (subset of dirty_sync_ms).
    pub staged_replay_ms: u64,
}

/// Sync a pre-created worktree to match a source repo's current state.
///
/// Holds the source and worktree paths, allowing repeated sync operations
/// on the same pair (useful for pool replenishment).
///
/// All operations are **synchronous/blocking**. Callers should use
/// `spawn_blocking` when calling from async contexts.
pub struct WorktreeSync<'a> {
    /// Path to the source repository (the "truth").
    pub source: &'a Path,
    /// Path to the linked worktree to sync.
    pub worktree: &'a Path,
}

impl<'a> WorktreeSync<'a> {
    /// Create a new sync handle for the given source/worktree pair.
    pub fn new(source: &'a Path, worktree: &'a Path) -> Self {
        Self { source, worktree }
    }

    /// Sync the worktree to match the source's current HEAD + dirty state.
    ///
    /// Strategy:
    /// 1. Resolve source HEAD and worktree HEAD via gix.
    /// 2. If HEAD moved → `git reset --hard <source_HEAD>` (linked worktrees
    ///    share the object store, so new commits are always available).
    /// 3. `git clean -fd` to remove any leftover untracked files (skippable).
    /// 4. If `copy_dirty` is true → replicate dirty state from source.
    ///
    /// # Errors
    ///
    /// Returns an error if any git command fails (non-zero exit), or if
    /// the gix repository cannot be opened.
    pub fn sync_worktree(&self, copy_dirty: bool) -> Result<SyncReport> {
        self.sync_worktree_opts(copy_dirty, false)
    }

    /// Sync with full control over the clean step.
    ///
    /// When `skip_clean` is `true`, the `git clean` step is omitted entirely.
    /// This is safe when the worktree is **known to be clean** — e.g. a freshly
    /// created pool worktree, or one that was just released (reset+clean'd).
    /// Skipping the clean step saves ~800ms on large repos (106K files) because
    /// `git clean` walks the entire directory tree even when there's nothing to
    /// remove.
    pub fn sync_worktree_opts(&self, copy_dirty: bool, skip_clean: bool) -> Result<SyncReport> {
        use std::time::Instant;
        let mut report = SyncReport::default();

        // Phase 1: Resolve HEAD commits.
        let t = Instant::now();
        let source_head = get_head_commit(self.source).context("failed to get source HEAD")?;
        let worktree_head =
            get_head_commit(self.worktree).context("failed to get worktree HEAD")?;
        report.head_resolve_ms = t.elapsed().as_millis() as u64;

        // Phase 2: Sync committed state.
        // `git reset --hard` rebuilds the index with correct stat caches,
        // fsmonitor data, and untracked-cache extensions. We must NOT
        // overwrite this index afterwards.
        if source_head != worktree_head {
            report.head_moved = true;
            let t = Instant::now();
            git_reset_hard_command(self.worktree, Some(&source_head))?;
            report.reset_hard_ms = t.elapsed().as_millis() as u64;
        }

        // Phase 3: Clean untracked files from previous use.
        // Skipped when the worktree is known to be clean (freshly created or
        // just released). Uses `-fd` (not `-fdx`): pool worktrees never have
        // gitignored files, and skipping `-x` avoids parsing .gitignore which
        // is faster.
        if skip_clean {
            report.clean_skipped = true;
            tracing::debug!(
                worktree = %self.worktree.display(),
                "skipping git clean (worktree known to be clean)"
            );
        } else {
            let t = Instant::now();
            git_clean_fd(self.worktree, false)?;
            report.clean_ms = t.elapsed().as_millis() as u64;
        }

        // Phase 4: Replicate dirty state if requested.
        // This copies dirty files and replays staged changes via
        // `git add`/`git rm --cached` — it does NOT copy the index
        // wholesale, preserving the stat caches built by reset.
        if copy_dirty {
            let t = Instant::now();
            let dirty_report = self.sync_dirty_state_timed()?;
            report.dirty_sync_ms = t.elapsed().as_millis() as u64;
            report.dirty_files_copied = dirty_report.dirty_files_copied;
            report.files_deleted = dirty_report.files_deleted;
            report.staged_entries = dirty_report.staged_entries;
            report.git_status_ms = dirty_report.git_status_ms;
            report.file_ops_ms = dirty_report.file_ops_ms;
            report.staged_replay_ms = dirty_report.staged_replay_ms;
        }

        Ok(report)
    }

    /// Sync using a pre-collected [`SourceDirtyState`].
    ///
    /// Same as [`sync_worktree_opts`] but uses a pre-computed dirty state
    /// instead of running `git status` internally. This allows a single
    /// `git status` call to be shared across multiple worktree syncs.
    ///
    /// If `dirty_state` is `None`, dirty sync is skipped entirely
    /// (equivalent to `copy_dirty=false`).
    pub fn sync_from_precomputed(
        &self,
        dirty_state: Option<&SourceDirtyState>,
        skip_clean: bool,
    ) -> Result<SyncReport> {
        let mut report = SyncReport::default();

        let source_head = get_head_commit(self.source).context("failed to get source HEAD")?;
        let worktree_head =
            get_head_commit(self.worktree).context("failed to get worktree HEAD")?;

        if source_head != worktree_head {
            report.head_moved = true;
            git_reset_hard_command(self.worktree, Some(&source_head))?;
        }

        if skip_clean {
            report.clean_skipped = true;
        } else {
            git_clean_fd(self.worktree, false)?;
        }

        match dirty_state {
            Some(state) if !state.is_empty() => {
                let result = apply_porcelain_v2_entries(&state.raw, self.source, self.worktree)?;
                report.dirty_files_copied = result.copied;
                report.files_deleted = result.deleted;

                if !result.staged_adds.is_empty() || !result.staged_deletes.is_empty() {
                    report.staged_entries = replay_staged_changes(
                        self.worktree,
                        &result.staged_adds,
                        &result.staged_deletes,
                    )?;
                }
            }
            Some(_) => {
                // Pre-computed state is empty — source is clean, nothing to sync.
                report.dirty_skipped = true;
            }
            None => {
                // No dirty state provided — skip entirely.
                // Currently unreachable from production callers (fallback path
                // uses the old `sync_worktree_opts` instead), but kept as a
                // defensive API contract: callers can pass None to opt out.
                report.dirty_skipped = true;
            }
        }

        Ok(report)
    }

    /// Copy dirty files from source to worktree and replicate staged changes.
    ///
    /// Uses `git status --porcelain=v2 -z` for correct handling of all entry
    /// types including renames (see module-level docs for rationale).
    /// File copies use `reflink_or_copy()` for CoW.
    ///
    /// Staged changes are replicated by running `git add` / `git rm --cached`
    /// on the worktree for each entry with a non-`.` X column. This preserves
    /// the worktree's index stat caches, fsmonitor data, and untracked-cache
    /// extensions that were built by `git reset --hard`.
    ///
    /// **Does NOT copy the source index wholesale** — that would destroy all
    /// stat caches (every entry would have stale mtime/inode/dev from the
    /// source filesystem) and force `git status` to re-hash every file.
    pub fn sync_dirty_state(&self) -> Result<SyncReport> {
        self.sync_dirty_state_timed()
    }

    /// Inner implementation with per-phase timing captured in the report.
    fn sync_dirty_state_timed(&self) -> Result<SyncReport> {
        use std::time::Instant;
        let mut report = SyncReport::default();

        // Sub-phase 1: git status on source.
        let t = Instant::now();
        let output = git_command()
            .args(["status", "--porcelain=v2", "-z", "--untracked-files=all"])
            .current_dir(self.source)
            .output()
            .context("failed to run git status")?;
        report.git_status_ms = t.elapsed().as_millis() as u64;

        if !output.status.success() {
            anyhow::bail!(
                "git status failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Sub-phase 2: Parse and apply status entries (file copies + deletions).
        if !output.stdout.is_empty() {
            let t = Instant::now();
            let result = apply_porcelain_v2_entries(&output.stdout, self.source, self.worktree)?;
            report.file_ops_ms = t.elapsed().as_millis() as u64;
            report.dirty_files_copied = result.copied;
            report.files_deleted = result.deleted;

            // Sub-phase 3: Replay staged changes on the worktree's index.
            if !result.staged_adds.is_empty() || !result.staged_deletes.is_empty() {
                let t = Instant::now();
                report.staged_entries = replay_staged_changes(
                    self.worktree,
                    &result.staged_adds,
                    &result.staged_deletes,
                )?;
                report.staged_replay_ms = t.elapsed().as_millis() as u64;
            }
        }

        Ok(report)
    }
}

/// Result of parsing and applying porcelain v2 status entries.
struct ApplyResult {
    /// Number of files copied from source to worktree.
    copied: u64,
    /// Number of files deleted in worktree to match source.
    deleted: u64,
    /// Staged additions/modifications: `(mode_in_index, hash_in_index, path)`.
    ///
    /// We capture the exact blob hash from porcelain v2 (`hI` field) so that
    /// `replay_staged_changes` can use `git update-index --cacheinfo` to set
    /// the index entry to the correct blob WITHOUT re-reading the working tree.
    /// This is critical for `XY = "MM"` where the staged content differs from
    /// the working tree content.
    staged_adds: Vec<(String, String, String)>,
    /// Paths that need `git rm --cached` in the worktree (staged deletions).
    staged_deletes: Vec<String>,
}

/// Parse `git status --porcelain=v2 -z` output and apply changes to the destination.
///
/// Porcelain v2 format with `-z` uses NUL as the field/record separator.
/// Entry types:
/// - `1 <XY> ...` — ordinary changed entry (one path follows)
/// - `2 <XY> ...` — renamed/copied entry (two paths follow: path then origPath)
/// - `u <XY> ...` — unmerged entry (one path follows)
/// - `? <path>` — untracked file
/// - `! <path>` — ignored file (not shown by default)
///
/// For renames (`2` entries), we copy the destination path (the new name)
/// and delete the source path (the old name) if it was tracked.
///
/// Also collects staged entries (X != '.') so the caller can replay them
/// on the worktree's index without copying the index wholesale.
///
/// Submodule entries are skipped — see [`is_submodule_entry`].
fn apply_porcelain_v2_entries(
    stdout: &[u8],
    source: &Path,
    worktree: &Path,
) -> Result<ApplyResult> {
    let mut copied: u64 = 0;
    let mut deleted: u64 = 0;
    let mut staged_adds: Vec<(String, String, String)> = Vec::new();
    let mut staged_deletes: Vec<String> = Vec::new();

    // Split on NUL bytes
    let mut chunks = stdout.split(|&b| b == 0).peekable();

    while let Some(chunk) = chunks.next() {
        if chunk.is_empty() {
            continue;
        }

        let line = std::str::from_utf8(chunk).context("non-UTF-8 path in git status output")?;

        if line.starts_with("1 ") || line.starts_with("u ") {
            // Ordinary entry: `1 XY sub mH mI mW hH hI path`
            //                   0  1  2   3  4  5  6  7  8
            // Unmerged entry:  `u XY sub m1 m2 m3 h1 h2 h3 path`
            //                   0  1  2   3  4  5  6  7  8  9

            if is_submodule_entry(line) {
                tracing::debug!(line = %line, "skipping submodule entry in dirty sync");
                continue;
            }

            let path = extract_ordinary_path(line);
            let xy = &line[2..4];
            apply_file_change(xy, path, source, worktree, &mut copied, &mut deleted)?;

            // Track staged changes (X column != '.')
            let x = xy.as_bytes()[0];
            if x != b'.' {
                if x == b'D' {
                    staged_deletes.push(path.to_string());
                } else {
                    // Extract mode-in-index (field 4) and hash-in-index (field 7)
                    // from the porcelain v2 line for `git update-index --cacheinfo`.
                    if let Some((mode, hash)) = extract_index_mode_hash(line) {
                        staged_adds.push((mode, hash, path.to_string()));
                    }
                }
            }
        } else if line.starts_with("2 ") {
            // Rename/copy entry: `2 XY sub mH mI mW hH hI X<score> path`
            //                      0  1  2   3  4  5  6  7    8      9
            // Followed by the origPath as the next NUL-delimited chunk

            if is_submodule_entry(line) {
                tracing::debug!(line = %line, "skipping submodule rename entry in dirty sync");
                // Consume the origPath chunk so parsing stays aligned.
                let _ = chunks.next();
                continue;
            }

            let path = extract_ordinary_path(line);
            let xy = &line[2..4];

            // Copy the new file
            apply_file_change(xy, path, source, worktree, &mut copied, &mut deleted)?;

            // Extract mode/hash for the renamed file and stage it
            if let Some((mode, hash)) = extract_index_mode_hash(line) {
                staged_adds.push((mode, hash, path.to_string()));
            }

            // Consume the origPath (old name) — it's the next NUL chunk
            if let Some(orig_chunk) = chunks.next() {
                let orig_path = std::str::from_utf8(orig_chunk)
                    .context("non-UTF-8 origPath in rename entry")?;
                // Delete the old name from destination if it exists
                let old_dest = worktree.join(orig_path);
                if old_dest.exists() {
                    let _ = std::fs::remove_file(&old_dest);
                    deleted += 1;
                }
                // Stage the deletion of the old path
                staged_deletes.push(orig_path.to_string());
            }
        } else if let Some(path) = line.strip_prefix("? ") {
            // Untracked file: `? path`
            copy_file_to_worktree(source, worktree, path)?;
            copied += 1;
        }
        // Ignore `!` (ignored files) — we don't replicate those
    }

    Ok(ApplyResult {
        copied,
        deleted,
        staged_adds,
        staged_deletes,
    })
}

/// Check whether a porcelain v2 entry is a submodule.
///
/// The `sub` field (space-delimited field 2) is `S...` for submodules and
/// `N...` for regular entries. Submodules are directories on disk, so
/// `reflink_or_copy` cannot handle them. Their committed state is already
/// synced by `git reset --hard`.
fn is_submodule_entry(line: &str) -> bool {
    line.split(' ')
        .nth(2)
        .is_some_and(|sub| sub.starts_with('S'))
}

/// Extract `(mode_in_index, hash_in_index)` from a porcelain v2 line.
///
/// For ordinary entries (`1 XY sub mH mI mW hH hI path`):
///   - `mI` is field 4 (0-indexed), the mode in the index
///   - `hI` is field 7, the object hash in the index
///
/// For rename entries (`2 XY sub mH mI mW hH hI X<score> path`):
///   - Same field positions for mI and hI
///
/// Returns `None` if the line doesn't have enough fields.
fn extract_index_mode_hash(line: &str) -> Option<(String, String)> {
    let fields: Vec<&str> = line.splitn(10, ' ').collect();
    if fields.len() >= 8 {
        // fields[4] = mI (mode in index), fields[7] = hI (hash in index)
        Some((fields[4].to_string(), fields[7].to_string()))
    } else {
        None
    }
}

/// Extract the path from a porcelain v2 ordinary/rename/unmerged entry.
///
/// See `git status --help`, section "Changed Tracked Entries" for the format:
/// <https://git-scm.com/docs/git-status#_changed_tracked_entries>
///
/// Format: `1 XY sub mH mI mW hH hI path` (8 space-separated header fields)
/// Rename: `2 XY sub mH mI mW hH hI X<score> path` (9 fields)
/// Unmerged: `u XY sub m1 m2 m3 h1 h2 h3 path` (10 fields)
fn extract_ordinary_path(line: &str) -> &str {
    let prefix = if line.starts_with("2 ") {
        // Rename: 9 space-separated header fields before path
        9
    } else if line.starts_with("u ") {
        // Unmerged: 10 space-separated header fields before path
        10
    } else {
        // Ordinary: 8 space-separated header fields before path
        8
    };

    let mut spaces_seen = 0;
    for (i, c) in line.char_indices() {
        if c == ' ' {
            spaces_seen += 1;
            if spaces_seen == prefix {
                return &line[i + 1..];
            }
        }
    }

    // Fallback — shouldn't happen with valid porcelain v2 output
    line
}

/// Apply a single file change (copy or delete) based on the XY status.
///
/// In porcelain v2, `X` is the **staged** (index) status and `Y` is the
/// **worktree** status. We care about the worktree column (`Y`) for deciding
/// whether the file exists on disk in the source:
///
/// - `Y == 'D'` → file is deleted in the worktree → delete in target
/// - `X == 'D'` with `Y != 'D'` → staged for deletion (`git rm --cached`)
///   but the file **still exists** on disk → copy it
/// - anything else → file exists on disk → copy it
fn apply_file_change(
    xy: &str,
    path: &str,
    source: &Path,
    worktree: &Path,
    copied: &mut u64,
    deleted: &mut u64,
) -> Result<()> {
    let y = xy.as_bytes()[1];

    if y == b'D' {
        // Worktree deletion — file does not exist on disk in source
        let dest_file = worktree.join(path);
        if dest_file.exists() {
            std::fs::remove_file(&dest_file)
                .with_context(|| format!("failed to delete {}", dest_file.display()))?;
            *deleted += 1;
        }
    } else {
        // File exists on disk in source (modified, added, type-changed,
        // staged-deletion-but-still-on-disk, etc.) — copy it
        copy_file_to_worktree(source, worktree, path)?;
        *copied += 1;
    }

    Ok(())
}

/// Replay staged changes on the worktree's index.
///
/// For staged additions/modifications, uses `git update-index --cacheinfo`
/// with the exact `(mode, blob_hash, path)` from the source's porcelain v2
/// output. This sets the index entry to the correct blob **without reading
/// the working tree**, which is critical for `XY = "MM"` where the staged
/// content differs from the on-disk content.
///
/// For staged deletions, uses `git rm --cached` to remove the entry from
/// the index while leaving the file on disk (if present).
///
/// Returns the total number of staged entries applied.
fn replay_staged_changes(
    worktree: &Path,
    staged_adds: &[(String, String, String)],
    staged_deletes: &[String],
) -> Result<u64> {
    let mut count = 0u64;

    // Batch `git update-index --cacheinfo` for all staged adds.
    // We use `--stdin` with `-z` for NUL-delimited input to handle
    // paths with spaces/special characters safely.
    //
    // Input format for --cacheinfo via --stdin -z:
    //   <mode> SP <hex-hash> TAB <path> NUL
    if !staged_adds.is_empty() {
        use std::io::Write;
        #[allow(clippy::disallowed_methods)] // git command, waited on below
        let mut child = git_command()
            .current_dir(worktree)
            .args(["update-index", "-z", "--index-info"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("failed to spawn git update-index --index-info")?;

        {
            let stdin = child
                .stdin
                .as_mut()
                .context("no stdin for git update-index")?;
            for (mode, hash, path) in staged_adds {
                // Format: "<mode> <hash>\t<path>\0"
                write!(stdin, "{mode} {hash}\t{path}\0")
                    .context("failed to write to git update-index stdin")?;
            }
        }

        let output = child
            .wait_with_output()
            .context("git update-index --index-info failed")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                worktree = %worktree.display(),
                stderr = %stderr.trim(),
                "git update-index --index-info for staged changes failed (non-fatal)"
            );
        } else {
            count += staged_adds.len() as u64;
        }
    }

    // Batch `git rm --cached` for staged deletions.
    // This removes the entry from the index while leaving the file on disk.
    if !staged_deletes.is_empty() {
        let mut cmd = git_command();
        cmd.current_dir(worktree)
            .arg("rm")
            .arg("--cached")
            .arg("--ignore-unmatch")
            .arg("--");
        for path in staged_deletes {
            cmd.arg(path);
        }
        let output = cmd
            .output()
            .context("failed to run git rm --cached for staged deletions")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                worktree = %worktree.display(),
                stderr = %stderr.trim(),
                "git rm --cached for staged deletions failed (non-fatal)"
            );
        } else {
            count += staged_deletes.len() as u64;
        }
    }

    Ok(count)
}

/// Copy a single dirty entry from source to worktree, creating parent dirs as
/// needed.
///
/// Uses `symlink_metadata` (not `exists`) to read the entry type without
/// following the link, so a dangling symlink is recreated, not skipped.
fn copy_file_to_worktree(source: &Path, worktree: &Path, rel_path: &str) -> Result<()> {
    let src_file = source.join(rel_path);
    let dst_file = worktree.join(rel_path);

    let src_meta = match std::fs::symlink_metadata(&src_file) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Source entry disappeared between `git status` and copy — TOCTOU
            // race. Expected when files are being actively edited.
            tracing::debug!(path = %rel_path, "source file vanished before copy, skipping");
            return Ok(());
        }
        Err(e) => {
            return Err(e).with_context(|| format!("failed to stat {}", src_file.display()));
        }
    };

    if let Some(parent) = dst_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent dirs for {}", dst_file.display()))?;
    }

    if src_meta.file_type().is_symlink() {
        let target = std::fs::read_link(&src_file)
            .with_context(|| format!("failed to read symlink {}", src_file.display()))?;
        return replace_symlink(&target, &dst_file)
            .with_context(|| format!("failed to recreate symlink {}", dst_file.display()));
    }

    // Regular file: drop any existing dest first (reflink_or_copy can't
    // overwrite). symlink_metadata detects a dangling-symlink dest, too.
    if std::fs::symlink_metadata(&dst_file).is_ok() {
        let _ = std::fs::remove_file(&dst_file);
    }

    clone_file(&src_file, &dst_file).with_context(|| {
        format!(
            "failed to copy {} -> {}",
            src_file.display(),
            dst_file.display()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::index::copy_git_index;
    use echo_agent_test_utils::git::{git_commit_all, init_git_repo};
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    /// Helper: create a git worktree from a source repo
    fn create_linked_worktree(source: &Path, name: &str) -> PathBuf {
        let worktree_path = source.parent().unwrap().join(name);
        let output = Command::new("git")
            .current_dir(source)
            .args([
                "worktree",
                "add",
                "--detach",
                worktree_path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        worktree_path
    }

    #[test]
    fn test_sync_same_head_no_dirty() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "content").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(false).unwrap();
        assert!(!report.head_moved);
        assert_eq!(report.dirty_files_copied, 0);
    }

    #[test]
    fn test_sync_head_moved() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "v1").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Make a new commit in source
        std::fs::write(source.join("file.txt"), "v2").unwrap();
        git_commit_all(&source, "second");

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(false).unwrap();
        assert!(report.head_moved);

        // Worktree should have the new content
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "v2"
        );
    }

    #[test]
    fn test_sync_dirty_files() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "committed").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Make dirty changes in source
        std::fs::write(source.join("file.txt"), "modified").unwrap();
        std::fs::write(source.join("new_untracked.txt"), "untracked").unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(true).unwrap();
        assert!(report.dirty_files_copied >= 2);

        // Worktree should have the dirty files
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "modified"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("new_untracked.txt")).unwrap(),
            "untracked"
        );
    }

    #[test]
    fn test_sync_deleted_file() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("keep.txt"), "keep").unwrap();
        std::fs::write(source.join("delete_me.txt"), "bye").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Delete a tracked file in source
        std::fs::remove_file(source.join("delete_me.txt")).unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(true).unwrap();
        assert!(report.files_deleted >= 1);

        // File should be gone in worktree too
        assert!(!worktree.join("delete_me.txt").exists());
    }

    #[test]
    fn test_sync_head_moved_with_dirty() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "v1").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // New commit + dirty changes in source
        std::fs::write(source.join("file.txt"), "v2").unwrap();
        git_commit_all(&source, "second");
        std::fs::write(source.join("file.txt"), "v2-dirty").unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(true).unwrap();
        assert!(report.head_moved);
        assert!(report.dirty_files_copied >= 1);

        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "v2-dirty"
        );
    }

    #[test]
    fn test_sync_rename() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("old_name.txt"), "content").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Rename file in source (git mv)
        Command::new("git")
            .current_dir(&source)
            .args(["mv", "old_name.txt", "new_name.txt"])
            .output()
            .unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(true).unwrap();
        // Should have copied new_name.txt and (potentially) deleted old_name.txt
        assert!(report.dirty_files_copied >= 1 || report.files_deleted >= 1);

        assert!(worktree.join("new_name.txt").exists());
        assert_eq!(
            std::fs::read_to_string(worktree.join("new_name.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn test_copy_git_index() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "content").unwrap();
        git_commit_all(&source, "initial");

        // Stage a change (modify the index)
        std::fs::write(source.join("file.txt"), "staged").unwrap();
        Command::new("git")
            .current_dir(&source)
            .args(["add", "file.txt"])
            .output()
            .unwrap();

        let worktree = create_linked_worktree(&source, "wt1");

        // Copy index (via the shared git::index function)
        let copied = copy_git_index(&source, &worktree).unwrap();
        assert!(copied, "index should have been copied");

        // The worktree's index should now reflect the staged change
        let output = Command::new("git")
            .current_dir(&worktree)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .unwrap();
        let staged = String::from_utf8_lossy(&output.stdout);
        assert!(
            staged.contains("file.txt"),
            "index copy should replicate staging state"
        );
    }

    #[test]
    fn test_sync_subdirectory_creation() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "root").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Create nested untracked file in source
        std::fs::create_dir_all(source.join("deeply/nested/dir")).unwrap();
        std::fs::write(source.join("deeply/nested/dir/file.txt"), "deep").unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(true).unwrap();
        assert!(report.dirty_files_copied >= 1);

        assert_eq!(
            std::fs::read_to_string(worktree.join("deeply/nested/dir/file.txt")).unwrap(),
            "deep"
        );
    }

    #[test]
    fn test_sync_copy_dirty_false_does_not_copy_index() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "content").unwrap();
        git_commit_all(&source, "initial");

        // Stage a change in source
        std::fs::write(source.join("file.txt"), "staged-change").unwrap();
        Command::new("git")
            .current_dir(&source)
            .args(["add", "file.txt"])
            .output()
            .unwrap();

        let worktree = create_linked_worktree(&source, "wt1");

        // Sync without dirty state
        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(false).unwrap();
        assert_eq!(report.staged_entries, 0);
        assert_eq!(report.dirty_files_copied, 0);

        // Worktree should NOT have the staged change
        let output = Command::new("git")
            .current_dir(&worktree)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .unwrap();
        let staged = String::from_utf8_lossy(&output.stdout);
        assert!(
            !staged.contains("file.txt"),
            "staged changes should not be replicated when copy_dirty=false"
        );
    }

    #[test]
    fn test_sync_file_with_spaces_and_unicode() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "anchor").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Create files with spaces and unicode in source
        std::fs::write(source.join("file with spaces.txt"), "spaces").unwrap();
        std::fs::create_dir_all(source.join("dir with spaces")).unwrap();
        std::fs::write(
            source.join("dir with spaces/nested file.txt"),
            "nested spaces",
        )
        .unwrap();
        std::fs::write(source.join("日本語ファイル.txt"), "unicode").unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(true).unwrap();
        assert!(report.dirty_files_copied >= 3);

        assert_eq!(
            std::fs::read_to_string(worktree.join("file with spaces.txt")).unwrap(),
            "spaces"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("dir with spaces/nested file.txt")).unwrap(),
            "nested spaces"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("日本語ファイル.txt")).unwrap(),
            "unicode"
        );
    }

    #[test]
    fn test_sync_staged_deletion_file_still_on_disk() {
        echo_agent_test_utils::require_git!();
        // Regression test: `git rm --cached` stages a deletion but
        // leaves the file on disk. We should copy the file, not delete it.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("cached.txt"), "still here").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Stage deletion but keep on disk
        Command::new("git")
            .current_dir(&source)
            .args(["rm", "--cached", "cached.txt"])
            .output()
            .unwrap();

        // File should still exist on disk in source
        assert!(source.join("cached.txt").exists());

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(true).unwrap();

        // The file should be COPIED (it exists on disk), not deleted
        assert!(
            worktree.join("cached.txt").exists(),
            "file should be copied since it's still on disk in source"
        );
        assert!(report.dirty_files_copied >= 1);
    }

    #[test]
    fn test_is_submodule_entry() {
        // Ordinary submodule
        assert!(is_submodule_entry(
            "1 .M SC.. 160000 160000 160000 aeed4e8a def456 submodules/example-submodule"
        ));
        // Submodule with all flags set
        assert!(is_submodule_entry(
            "1 .M SCMU 160000 160000 160000 aeed4e8a def456 submodules/foo"
        ));
        // Regular file
        assert!(!is_submodule_entry(
            "1 .M N... 100644 100644 100644 abc123 def456 src/main.rs"
        ));
        // Rename entry for submodule
        assert!(is_submodule_entry(
            "2 R. SC.. 160000 160000 160000 abc123 def456 R100 submodules/new-name"
        ));
        // Unmerged submodule
        assert!(is_submodule_entry(
            "u UU SC.. 160000 160000 160000 160000 abc123 def456 ghi789 submodules/foo"
        ));
        // Untracked entry (no sub field) — should not match
        assert!(!is_submodule_entry("? some/path"));
    }

    #[test]
    fn test_apply_skips_submodule_entries() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("src");
        let worktree = temp.path().join("wt");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        std::fs::write(source.join("file.txt"), "modified").unwrap();
        // Submodule is a directory on disk — must not be passed to reflink_or_copy
        std::fs::create_dir_all(source.join("submodules/example-submodule")).unwrap();

        // Porcelain v2: regular file + submodule entry
        let status_output = b"1 .M N... 100644 100644 100644 abc123 def456 file.txt\x001 .M SC.. 160000 160000 160000 aeed4e def456 submodules/example-submodule\x00";
        let result = apply_porcelain_v2_entries(status_output, &source, &worktree).unwrap();

        assert_eq!(result.copied, 1, "only the regular file should be copied");
        assert_eq!(result.deleted, 0);
        assert!(worktree.join("file.txt").exists());
    }

    #[test]
    fn test_extract_ordinary_path_type1() {
        // Ordinary changed entry
        let line = "1 .M N... 100644 100644 100644 abc123 def456 src/main.rs";
        assert_eq!(extract_ordinary_path(line), "src/main.rs");
    }

    #[test]
    fn test_extract_ordinary_path_type2() {
        // Rename entry
        let line = "2 R. N... 100644 100644 100644 abc123 def456 R100 new_name.rs";
        assert_eq!(extract_ordinary_path(line), "new_name.rs");
    }

    #[test]
    fn test_extract_ordinary_path_unmerged() {
        // Unmerged entry
        let line = "u UU N... 100644 100644 100644 100644 abc123 def456 ghi789 conflict.rs";
        assert_eq!(extract_ordinary_path(line), "conflict.rs");
    }

    #[test]
    fn test_sync_branch_changed() {
        echo_agent_test_utils::require_git!();
        // Source checks out a different branch after worktree was created.
        // The worktree (detached) should sync to the new branch's HEAD.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "main-v1").unwrap();
        git_commit_all(&source, "initial on main");

        let worktree = create_linked_worktree(&source, "wt1");

        // Create a new branch in source and switch to it
        Command::new("git")
            .current_dir(&source)
            .args(["checkout", "-b", "feature-branch"])
            .output()
            .unwrap();
        std::fs::write(source.join("file.txt"), "feature-v1").unwrap();
        std::fs::write(source.join("feature_only.txt"), "feature file").unwrap();
        git_commit_all(&source, "commit on feature-branch");

        // Sync — worktree should jump to the feature branch's HEAD
        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(false).unwrap();
        assert!(
            report.head_moved,
            "HEAD should have moved to feature branch"
        );

        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "feature-v1"
        );
        assert!(
            worktree.join("feature_only.txt").exists(),
            "feature-only file should exist in worktree"
        );
    }

    #[test]
    fn test_sync_multiple_commits_ahead() {
        echo_agent_test_utils::require_git!();
        // Source is many commits ahead of the worktree.
        // git reset --hard should jump directly regardless of distance.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "v1").unwrap();
        git_commit_all(&source, "commit 1");

        let worktree = create_linked_worktree(&source, "wt1");

        // Make 5 more commits in source
        for i in 2..=6 {
            std::fs::write(source.join("file.txt"), format!("v{i}")).unwrap();
            std::fs::write(
                source.join(format!("added_in_commit_{i}.txt")),
                format!("new in {i}"),
            )
            .unwrap();
            git_commit_all(&source, &format!("commit {i}"));
        }

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(false).unwrap();
        assert!(report.head_moved, "HEAD should have moved 5 commits ahead");

        // Verify the latest content
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "v6"
        );
        // Verify all new files exist
        for i in 2..=6 {
            assert!(
                worktree.join(format!("added_in_commit_{i}.txt")).exists(),
                "file from commit {i} should exist"
            );
        }
    }

    #[test]
    fn test_sync_staged_and_worktree_modifications_same_file() {
        echo_agent_test_utils::require_git!();
        // A file has BOTH staged changes (in the index) AND further worktree
        // modifications on top. The sync should replicate the on-disk state
        // (worktree version) and the index (staged version).
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "original").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Stage a change
        std::fs::write(source.join("file.txt"), "staged-version").unwrap();
        Command::new("git")
            .current_dir(&source)
            .args(["add", "file.txt"])
            .output()
            .unwrap();

        // Make further worktree modifications on top of the staged change
        std::fs::write(source.join("file.txt"), "worktree-version").unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree(true).unwrap();
        assert!(report.dirty_files_copied >= 1);
        assert!(
            report.staged_entries >= 1,
            "staged change should be replicated"
        );

        // On-disk content should be the worktree version (latest on-disk state)
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "worktree-version"
        );

        // Index should have the staged version (not the worktree version)
        let output = Command::new("git")
            .current_dir(&worktree)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .unwrap();
        let staged = String::from_utf8_lossy(&output.stdout);
        assert!(
            staged.contains("file.txt"),
            "file should appear as staged in worktree's index"
        );

        // Verify the staged content is the "staged-version" (not "worktree-version")
        let output = Command::new("git")
            .current_dir(&worktree)
            .args(["show", ":file.txt"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git show :file.txt failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "staged-version",
            "index should contain the staged version, not the worktree version"
        );
    }

    // ========================================================================
    // skip_clean=true tests (pool path)
    //
    // The worktree pool calls sync_worktree_opts(copy_dirty, skip_clean=true)
    // because pool worktrees are known-clean (freshly created or just
    // released). These tests verify that commits, dirty files, and untracked
    // files are correctly replicated through that code path.
    // ========================================================================

    #[test]
    fn test_skip_clean_commit_replication() {
        echo_agent_test_utils::require_git!();
        // Pool scenario: worktree created, source gets new commits, sync
        // with skip_clean=true should still replicate them via reset --hard.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "v1").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Source advances by 3 commits after worktree creation
        std::fs::write(source.join("file.txt"), "v2").unwrap();
        git_commit_all(&source, "commit 2");
        std::fs::write(source.join("new_file.txt"), "added in commit 3").unwrap();
        git_commit_all(&source, "commit 3");
        std::fs::write(source.join("file.txt"), "v4").unwrap();
        git_commit_all(&source, "commit 4");

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree_opts(false, true).unwrap();
        assert!(
            report.head_moved,
            "HEAD should move to source's latest commit"
        );
        assert!(report.clean_skipped, "clean_skipped should be reported");

        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "v4",
            "worktree should have latest committed content"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("new_file.txt")).unwrap(),
            "added in commit 3",
            "file from intermediate commit should exist"
        );
    }

    #[test]
    fn test_skip_clean_dirty_and_untracked_replication() {
        echo_agent_test_utils::require_git!();
        // Pool scenario: worktree is clean, source has dirty tracked files
        // and new untracked files. skip_clean=true + copy_dirty=true should
        // replicate both.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("tracked.txt"), "committed").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Dirty modifications in source (no commit)
        std::fs::write(source.join("tracked.txt"), "dirty-modification").unwrap();
        std::fs::write(source.join("brand_new.txt"), "untracked content").unwrap();
        std::fs::create_dir_all(source.join("new_dir")).unwrap();
        std::fs::write(source.join("new_dir/nested.txt"), "nested untracked").unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree_opts(true, true).unwrap();
        assert!(!report.head_moved, "HEAD unchanged — no new commits");
        assert!(
            report.dirty_files_copied >= 3,
            "should copy tracked mod + 2 untracked"
        );

        assert_eq!(
            std::fs::read_to_string(worktree.join("tracked.txt")).unwrap(),
            "dirty-modification"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("brand_new.txt")).unwrap(),
            "untracked content"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("new_dir/nested.txt")).unwrap(),
            "nested untracked"
        );
    }

    #[test]
    fn test_skip_clean_commit_plus_dirty() {
        echo_agent_test_utils::require_git!();
        // Full pool scenario: source advanced by commits AND has dirty state
        // on top. skip_clean=true + copy_dirty=true.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "v1").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // New commit
        std::fs::write(source.join("file.txt"), "v2-committed").unwrap();
        std::fs::write(source.join("committed_new.txt"), "from commit").unwrap();
        git_commit_all(&source, "second");

        // Dirty state on top of the new commit
        std::fs::write(source.join("file.txt"), "v2-dirty-overlay").unwrap();
        std::fs::write(source.join("untracked.txt"), "untracked").unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree_opts(true, true).unwrap();
        assert!(report.head_moved);
        assert!(report.dirty_files_copied >= 2);

        // Committed file from second commit should exist
        assert_eq!(
            std::fs::read_to_string(worktree.join("committed_new.txt")).unwrap(),
            "from commit"
        );
        // Dirty overlay should win over committed content
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "v2-dirty-overlay"
        );
        // Untracked file should be copied
        assert_eq!(
            std::fs::read_to_string(worktree.join("untracked.txt")).unwrap(),
            "untracked"
        );
    }

    #[test]
    fn test_skip_clean_staged_changes_replicated() {
        echo_agent_test_utils::require_git!();
        // Pool scenario: source has staged (indexed) changes.
        // skip_clean=true + copy_dirty=true should replay staged entries
        // via git update-index.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "original").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Stage a modification in source
        std::fs::write(source.join("file.txt"), "staged-content").unwrap();
        Command::new("git")
            .current_dir(&source)
            .args(["add", "file.txt"])
            .output()
            .unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree_opts(true, true).unwrap();
        assert!(report.staged_entries >= 1, "should replicate staged entry");
        assert!(
            report.dirty_files_copied >= 1,
            "should copy the staged file"
        );

        // On-disk content should match source
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "staged-content"
        );

        // Index should show file as staged
        let output = Command::new("git")
            .current_dir(&worktree)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .unwrap();
        let staged = String::from_utf8_lossy(&output.stdout);
        assert!(
            staged.contains("file.txt"),
            "file should be staged in worktree index"
        );
    }

    #[test]
    fn test_skip_clean_sequential_reuse() {
        echo_agent_test_utils::require_git!();
        // Simulates pool worktree reuse: create → sync → (simulate use) →
        // source changes → sync again. Both syncs use skip_clean=true.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "v1").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // --- First sync: source advanced ---
        std::fs::write(source.join("file.txt"), "v2").unwrap();
        git_commit_all(&source, "second");

        let sync = WorktreeSync::new(&source, &worktree);
        let report1 = sync.sync_worktree_opts(false, true).unwrap();
        assert!(report1.head_moved);
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "v2"
        );

        // --- Simulate release: reset --hard + clean (what the pool does) ---
        Command::new("git")
            .current_dir(&worktree)
            .args(["reset", "--hard"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&worktree)
            .args(["clean", "-fd"])
            .output()
            .unwrap();

        // --- Second sync: source advanced again with dirty state ---
        std::fs::write(source.join("file.txt"), "v3").unwrap();
        git_commit_all(&source, "third");
        std::fs::write(source.join("file.txt"), "v3-dirty").unwrap();
        std::fs::write(source.join("second_use.txt"), "new untracked").unwrap();

        let report2 = sync.sync_worktree_opts(true, true).unwrap();
        assert!(report2.head_moved);
        assert!(report2.dirty_files_copied >= 2);

        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "v3-dirty"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("second_use.txt")).unwrap(),
            "new untracked"
        );
    }

    #[test]
    fn test_skip_clean_does_not_remove_leftover_untracked() {
        echo_agent_test_utils::require_git!();
        // Documents the skip_clean contract: if the worktree has leftover
        // untracked files from a previous use and skip_clean=true is passed,
        // those files PERSIST. This is correct for the pool because pool
        // worktrees are always cleaned before being returned to the ready
        // state.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "v1").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Simulate leftover untracked file in worktree (from previous use)
        std::fs::write(worktree.join("leftover.txt"), "stale").unwrap();

        // Sync with skip_clean=true — leftover should survive
        let sync = WorktreeSync::new(&source, &worktree);
        let _report = sync.sync_worktree_opts(false, true).unwrap();

        assert!(
            worktree.join("leftover.txt").exists(),
            "skip_clean=true should NOT remove leftover untracked files"
        );

        // Contrast: sync with skip_clean=false removes the leftover
        let _report = sync.sync_worktree_opts(false, false).unwrap();
        assert!(
            !worktree.join("leftover.txt").exists(),
            "skip_clean=false should remove leftover untracked files"
        );
    }

    #[test]
    fn test_skip_clean_deleted_tracked_file_replicated() {
        echo_agent_test_utils::require_git!();
        // Source deletes a tracked file. skip_clean=true + copy_dirty=true
        // should replicate the deletion in the worktree.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("keep.txt"), "keep").unwrap();
        std::fs::write(source.join("delete_me.txt"), "bye").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Delete tracked file in source (dirty deletion, not committed)
        std::fs::remove_file(source.join("delete_me.txt")).unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree_opts(true, true).unwrap();
        assert!(report.files_deleted >= 1);
        assert!(
            !worktree.join("delete_me.txt").exists(),
            "deleted tracked file should be removed from worktree"
        );
        assert!(
            worktree.join("keep.txt").exists(),
            "non-deleted file should still exist"
        );
    }

    #[test]
    fn test_skip_clean_branch_switch_with_dirty() {
        echo_agent_test_utils::require_git!();
        // Source switches branches and has dirty state. skip_clean=true
        // should handle the branch jump + dirty overlay.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "main-v1").unwrap();
        git_commit_all(&source, "initial on main");

        let worktree = create_linked_worktree(&source, "wt1");

        // Switch to feature branch, commit, then add dirty state
        Command::new("git")
            .current_dir(&source)
            .args(["checkout", "-b", "feature"])
            .output()
            .unwrap();
        std::fs::write(source.join("feature.txt"), "feature file").unwrap();
        git_commit_all(&source, "feature commit");
        std::fs::write(source.join("feature.txt"), "feature-dirty").unwrap();
        std::fs::write(source.join("untracked_feature.txt"), "untracked on feature").unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_worktree_opts(true, true).unwrap();
        assert!(report.head_moved, "HEAD should jump to feature branch");
        assert!(report.dirty_files_copied >= 2);

        // Committed feature file should exist with dirty overlay
        assert_eq!(
            std::fs::read_to_string(worktree.join("feature.txt")).unwrap(),
            "feature-dirty"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("untracked_feature.txt")).unwrap(),
            "untracked on feature"
        );
    }

    // ── sync_from_precomputed tests ──

    #[test]
    fn test_sync_from_precomputed_none_skips_dirty() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "committed").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Make dirty changes in source
        std::fs::write(source.join("file.txt"), "modified").unwrap();

        // sync_from_precomputed with None should skip dirty sync
        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_from_precomputed(None, true).unwrap();
        assert!(report.dirty_skipped);
        assert_eq!(report.dirty_files_copied, 0);

        // Worktree should NOT have the dirty change
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "committed"
        );
    }

    #[test]
    fn test_sync_from_precomputed_empty_skips_dirty() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "committed").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Collect dirty state from a clean source
        let dirty = super::collect_source_dirty_state(&source).unwrap();
        assert!(dirty.is_empty());

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_from_precomputed(Some(&dirty), true).unwrap();
        assert!(report.dirty_skipped);
        assert_eq!(report.dirty_files_copied, 0);
    }

    #[test]
    fn test_sync_from_precomputed_with_dirty_files() {
        echo_agent_test_utils::require_git!();
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "committed").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Make dirty changes, then collect
        std::fs::write(source.join("file.txt"), "modified").unwrap();
        std::fs::write(source.join("untracked.txt"), "new file").unwrap();

        let dirty = super::collect_source_dirty_state(&source).unwrap();
        assert!(!dirty.is_empty());

        let sync = WorktreeSync::new(&source, &worktree);
        let report = sync.sync_from_precomputed(Some(&dirty), true).unwrap();
        assert!(!report.dirty_skipped);
        assert!(report.dirty_files_copied >= 2);

        // Worktree should have the dirty changes
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "modified"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("untracked.txt")).unwrap(),
            "new file"
        );
    }

    #[test]
    fn test_sync_from_precomputed_shared_across_two_worktrees() {
        echo_agent_test_utils::require_git!();
        // The core use case: collect once, apply to two worktrees.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("file.txt"), "committed").unwrap();
        git_commit_all(&source, "initial");

        let wt_a = create_linked_worktree(&source, "wt_a");
        let wt_b = create_linked_worktree(&source, "wt_b");

        // Make dirty changes, collect once
        std::fs::write(source.join("file.txt"), "dirty").unwrap();
        let dirty = super::collect_source_dirty_state(&source).unwrap();

        // Apply to both worktrees (clone is cheap — Bytes ref-count bump)
        let sync_a = WorktreeSync::new(&source, &wt_a);
        let report_a = sync_a.sync_from_precomputed(Some(&dirty), true).unwrap();

        let sync_b = WorktreeSync::new(&source, &wt_b);
        let report_b = sync_b.sync_from_precomputed(Some(&dirty), true).unwrap();

        assert!(report_a.dirty_files_copied >= 1);
        assert!(report_b.dirty_files_copied >= 1);

        assert_eq!(
            std::fs::read_to_string(wt_a.join("file.txt")).unwrap(),
            "dirty"
        );
        assert_eq!(
            std::fs::read_to_string(wt_b.join("file.txt")).unwrap(),
            "dirty"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_sync_replicates_symlinks_not_regular_files() {
        echo_agent_test_utils::require_git!();
        // Dirty symlinks (valid and dangling) must land in the worktree as
        // symlinks, never dereferenced to regular files and never skipped.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("target.txt"), "real content").unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");

        // Dirty source fixtures: valid symlink, dangling symlink, regular file.
        std::os::unix::fs::symlink("target.txt", source.join("link.txt")).unwrap();
        std::os::unix::fs::symlink("does-not-exist", source.join("dangling.txt")).unwrap();
        std::fs::write(source.join("regular.txt"), "regular").unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        sync.sync_worktree(true).unwrap();

        let link_meta = std::fs::symlink_metadata(worktree.join("link.txt")).unwrap();
        assert!(
            link_meta.file_type().is_symlink(),
            "link.txt must remain a symlink, not become a regular file"
        );
        assert_eq!(
            std::fs::read_link(worktree.join("link.txt")).unwrap(),
            PathBuf::from("target.txt")
        );

        let dangling_meta = std::fs::symlink_metadata(worktree.join("dangling.txt")).unwrap();
        assert!(
            dangling_meta.file_type().is_symlink(),
            "dangling.txt must be recreated as a symlink"
        );
        assert_eq!(
            std::fs::read_link(worktree.join("dangling.txt")).unwrap(),
            PathBuf::from("does-not-exist")
        );

        let reg_meta = std::fs::symlink_metadata(worktree.join("regular.txt")).unwrap();
        assert!(reg_meta.file_type().is_file());
        assert_eq!(
            std::fs::read_to_string(worktree.join("regular.txt")).unwrap(),
            "regular"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_file_replaces_dangling_symlink_dest_with_regular_file() {
        // Dest holds a dangling symlink; copying a regular file over it must
        // replace it — reflink can't overwrite and `exists()` misses a broken link.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("src");
        let worktree = temp.path().join("wt");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();

        std::fs::write(source.join("file.txt"), "real content").unwrap();
        std::os::unix::fs::symlink("does-not-exist", worktree.join("file.txt")).unwrap();

        copy_file_to_worktree(&source, &worktree, "file.txt").unwrap();

        let meta = std::fs::symlink_metadata(worktree.join("file.txt")).unwrap();
        assert!(
            meta.file_type().is_file(),
            "dest must become a regular file, not stay a symlink"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("file.txt")).unwrap(),
            "real content"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_sync_updates_modified_tracked_symlink_target() {
        echo_agent_test_utils::require_git!();
        // Re-pointing a tracked symlink in source must update the worktree dest
        // to the new target while keeping it a symlink.
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        std::fs::write(source.join("target_a.txt"), "a").unwrap();
        std::fs::write(source.join("target_b.txt"), "b").unwrap();
        std::os::unix::fs::symlink("target_a.txt", source.join("link.txt")).unwrap();
        git_commit_all(&source, "initial");

        let worktree = create_linked_worktree(&source, "wt1");
        assert_eq!(
            std::fs::read_link(worktree.join("link.txt")).unwrap(),
            PathBuf::from("target_a.txt"),
            "worktree starts with the committed symlink target"
        );

        // Re-point the tracked symlink in source (modified, uncommitted).
        std::fs::remove_file(source.join("link.txt")).unwrap();
        std::os::unix::fs::symlink("target_b.txt", source.join("link.txt")).unwrap();

        let sync = WorktreeSync::new(&source, &worktree);
        sync.sync_worktree(true).unwrap();

        let meta = std::fs::symlink_metadata(worktree.join("link.txt")).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "dest must remain a symlink after the update"
        );
        assert_eq!(
            std::fs::read_link(worktree.join("link.txt")).unwrap(),
            PathBuf::from("target_b.txt"),
            "dest symlink must be updated to the new target"
        );
    }
}
