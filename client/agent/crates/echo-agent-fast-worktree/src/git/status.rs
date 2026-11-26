//! Git status helpers (compute dirty paths).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use dashmap::DashSet;
use gix::bstr::BString;
use gix::status::index_worktree::Item;
use gix_status::index_as_worktree::{Change, EntryStatus};

use crate::copy::DirtyFilesReport;

/// Result of scanning for modified files, including both paths and counts by category.
pub(crate) struct ModifiedFilesResult {
    /// Set of all relative paths that are modified/untracked/deleted.
    pub paths: DashSet<PathBuf>,
    /// Categorized counts of dirty files.
    pub report: DirtyFilesReport,
}

/// Get modified files from the source repository.
///
/// This uses `gix`'s `index_worktree_iter` which compares the **index** to the
/// **worktree**. It reports which files have been modified/added/deleted relative
/// to what's staged, but does **not** expose the two-column staged-vs-worktree
/// status (`XY` in porcelain output). For full `XY` semantics (needed by
/// `sync::WorktreeSync`), see the CLI-based parser in `sync.rs`.
///
/// This is a blocking operation.
pub(crate) fn get_modified_files(source: &Path) -> Result<ModifiedFilesResult> {
    let repo = gix::discover(source).context("failed to discover git repository")?;
    let modified: DashSet<PathBuf> = DashSet::new();

    // Guard against empty index files — gix-index panics when the file
    // is 0 bytes because it tries to slice the trailing hash from an
    // empty mmap (integer underflow in the slice range).
    let index_path = repo.git_dir().join("index");
    if index_path.metadata().map_or(true, |m| m.len() == 0) {
        tracing::debug!(
            path = %source.display(),
            "index file is empty or missing, returning empty modified set"
        );
        return Ok(ModifiedFilesResult {
            paths: modified,
            report: DirtyFilesReport {
                modified_files: 0,
                untracked_files: 0,
                deleted_files: 0,
            },
        });
    }

    let mut modified_count = 0u64;
    let mut untracked_count = 0u64;
    let mut deleted_count = 0u64;

    // Cap produce workers: gix-features spawn-EAGAIN aborts under panic=abort.
    let status =
        echo_agent_gix_status::with_budgeted_thread_limit(repo.status(gix::progress::Discard)?);
    let iter = status.into_index_worktree_iter(Vec::<BString>::new())?;

    for item_result in iter {
        let item = item_result?;

        let path = match &item {
            Item::Modification {
                rela_path, status, ..
            } => {
                // Check if it's a deletion (file exists in index but not in worktree)
                match status {
                    EntryStatus::Change(Change::Removed) => deleted_count += 1,
                    _ => modified_count += 1,
                }
                rela_path.to_string()
            }
            Item::DirectoryContents { entry, .. } => {
                // DirectoryContents = untracked files from directory walk
                untracked_count += 1;
                entry.rela_path.to_string()
            }
            Item::Rewrite { dirwalk_entry, .. } => {
                // Rewrite = file was renamed (tracked as modified)
                modified_count += 1;
                dirwalk_entry.rela_path.to_string()
            }
        };

        modified.insert(PathBuf::from(path));
    }

    tracing::info!(
        count = modified.len(),
        modified = modified_count,
        untracked = untracked_count,
        deleted = deleted_count,
        "found modified files"
    );

    Ok(ModifiedFilesResult {
        paths: modified,
        report: DirtyFilesReport {
            modified_files: modified_count,
            untracked_files: untracked_count,
            deleted_files: deleted_count,
        },
    })
}
