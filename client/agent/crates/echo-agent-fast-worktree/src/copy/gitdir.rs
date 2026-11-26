//! Selective CoW copy of `.git/` directory for standalone repository cloning.
//!
//! Copies essential git internal files using reflink (CoW) when supported,
//! skipping transient state, lock files, and stale worktree registrations.
//!
//! The `objects/` directory (often the largest subtree) is copied in parallel
//! using a thread pool for better throughput on SSDs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

use crate::copy::cow::clone_file;
use crate::copy::standalone::{StandaloneCopyFilter, sanitize_standalone_git_dir_keeping};

/// Statistics from copying the `.git/` directory.
#[derive(Clone, Debug, Default)]
pub(crate) struct GitDirCopyStats {
    pub files_copied: u64,
    pub dirs_created: u64,
    pub symlinks_copied: u64,
    pub entries_skipped: u64,
}

/// Top-level `.git/` entries to skip when creating a standalone copy.
///
/// These are either transient state (merge/rebase in-progress markers) or
/// linked-worktree metadata that would be stale in the copy.
const SKIP_TOP_LEVEL: &[&str] = &[
    // Linked worktree registrations — stale in a standalone copy
    "worktrees",
    // Transient HEAD-like state files
    "FETCH_HEAD",
    "ORIG_HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "AUTO_MERGE",
    "BISECT_LOG",
    // In-progress multi-step operation state
    "sequencer",
    "rebase-merge",
    "rebase-apply",
    // GC state
    "gc.log",
    // fsmonitor daemon state — a host-local Unix-domain IPC socket
    // (`fsmonitor--daemon.ipc`, which cannot be reflinked/copied) plus its
    // transient `cookies/` dir. Both are runtime state of the source repo's
    // daemon and must never be inherited by a standalone copy.
    "fsmonitor--daemon",
    "fsmonitor--daemon.ipc",
];

/// A work item for the parallel copy pool.
struct CopyWork {
    source: PathBuf,
    dest: PathBuf,
}

/// Copy `.git/` directory contents using CoW, skipping unnecessary entries.
///
/// Creates a standalone git repository's `.git/` at `dest_git` by selectively
/// copying from `source_git`. Files are copied using reflink (CoW) when the
/// filesystem supports it, falling back to regular copy otherwise.
///
/// The `objects/` subtree is copied in parallel (it's typically the largest
/// part and has no ordering dependencies). Other top-level entries are copied
/// sequentially.
///
/// Skips:
/// - Lock files (`*.lock`) at any depth
/// - Stale worktree registrations (`worktrees/`)
/// - Transient state files (`MERGE_HEAD`, `CHERRY_PICK_HEAD`, etc.)
/// - In-progress rebase/cherry-pick state (`sequencer/`, `rebase-merge/`)
/// - Extra `refs/remotes/origin/*` tips (keeps HEAD/main/master/current/@{upstream})
/// - `.git/shallow` when its grafts are not on HEAD's first-parent chain
#[cfg(test)]
pub(crate) fn copy_git_dir(source_git: &Path, dest_git: &Path) -> Result<GitDirCopyStats> {
    copy_git_dir_keeping_origin(source_git, dest_git, &HashSet::new())
}

/// Like [`copy_git_dir`], but keep extra `refs/remotes/origin/*` names through
/// CoW skip + post-copy sanitize so a later checkout can still see them.
pub(crate) fn copy_git_dir_keeping_origin(
    source_git: &Path,
    dest_git: &Path,
    extra_origin: &HashSet<String>,
) -> Result<GitDirCopyStats> {
    copy_git_dir_with_workers(source_git, dest_git, num_cpus::get(), extra_origin)
}

/// `copy_git_dir` with an explicit worker cap, so tests can force the parallel
/// branch (`max_workers >= 2`) deterministically regardless of `num_cpus`.
fn copy_git_dir_with_workers(
    source_git: &Path,
    dest_git: &Path,
    max_workers: usize,
    extra_origin: &HashSet<String>,
) -> Result<GitDirCopyStats> {
    anyhow::ensure!(
        source_git.is_dir(),
        "source .git must be a directory (not a linked worktree .git file): {}",
        source_git.display()
    );

    let files_copied = AtomicU64::new(0);
    let dirs_created = AtomicU64::new(0);
    let symlinks_copied = AtomicU64::new(0);
    let entries_skipped = AtomicU64::new(0);

    let filter = StandaloneCopyFilter::from_git_dir_keeping(source_git, extra_origin);

    // First pass: collect work items for parallel copy.
    // We collect all (source, dest) pairs, then process them in parallel.
    let mut work_items: Vec<CopyWork> = Vec::new();
    collect_work_recursive(
        source_git,
        dest_git,
        Path::new(""),
        &filter,
        &mut work_items,
        &dirs_created,
        &entries_skipped,
    )?;

    // Process file copies in parallel using scoped threads.
    let num_workers = max_workers.min(work_items.len().max(1));

    if num_workers <= 1 || work_items.len() < 64 {
        // Not enough work to justify parallelism.
        for item in &work_items {
            copy_single_entry(&item.source, &item.dest, &files_copied, &symlinks_copied)?;
        }
    } else {
        // Shard work items across threads (simple round-robin). Each thread
        // returns its first copy error; the sequential branch propagates errors
        // with `?`, so this branch must too — a failed `.git/index`/pack copy
        // would otherwise yield a silently-corrupt standalone repo.
        let chunk_size = work_items.len().div_ceil(num_workers);
        let first_error = crossbeam::scope(|scope| {
            let handles: Vec<_> = work_items
                .chunks(chunk_size)
                .map(|chunk| {
                    let files_copied = &files_copied;
                    let symlinks_copied = &symlinks_copied;
                    scope.spawn(move |_| -> Result<()> {
                        for item in chunk {
                            copy_single_entry(
                                &item.source,
                                &item.dest,
                                files_copied,
                                symlinks_copied,
                            )?;
                        }
                        Ok(())
                    })
                })
                .collect();

            // Join in spawn order so "first error" is deterministic.
            let mut first_error: Option<anyhow::Error> = None;
            for handle in handles {
                let chunk_result = match handle.join() {
                    Ok(r) => r,
                    Err(_) => Err(anyhow::anyhow!("parallel .git/ copy thread panicked")),
                };
                if let Err(e) = chunk_result
                    && first_error.is_none()
                {
                    first_error = Some(e);
                }
            }
            first_error
        })
        .map_err(|_| anyhow::anyhow!("parallel .git/ copy panicked"))?;

        if let Some(e) = first_error {
            return Err(e);
        }
    }

    sanitize_standalone_git_dir_keeping(dest_git, extra_origin)
        .context("failed to sanitize standalone .git/ after copy")?;

    let stats = GitDirCopyStats {
        files_copied: files_copied.load(Ordering::Relaxed),
        dirs_created: dirs_created.load(Ordering::Relaxed),
        symlinks_copied: symlinks_copied.load(Ordering::Relaxed),
        entries_skipped: entries_skipped.load(Ordering::Relaxed),
    };

    tracing::debug!(
        files = stats.files_copied,
        dirs = stats.dirs_created,
        symlinks = stats.symlinks_copied,
        skipped = stats.entries_skipped,
        workers = num_workers,
        "git dir copy complete"
    );

    Ok(stats)
}

/// Recursively collect work items (files/symlinks to copy), creating directories eagerly.
///
/// Directories are created immediately (they must exist before files are written),
/// but file copies are deferred to the work list for parallel processing.
fn collect_work_recursive(
    source: &Path,
    dest: &Path,
    rel: &Path,
    filter: &StandaloneCopyFilter,
    work_items: &mut Vec<CopyWork>,
    dirs_created: &AtomicU64,
    entries_skipped: &AtomicU64,
) -> Result<()> {
    std::fs::create_dir_all(dest)
        .with_context(|| format!("failed to create directory {}", dest.display()))?;
    dirs_created.fetch_add(1, Ordering::Relaxed);

    let entries = std::fs::read_dir(source)
        .with_context(|| format!("failed to read directory {}", source.display()))?;

    for entry_result in entries {
        let entry = entry_result
            .with_context(|| format!("failed to read entry in {}", source.display()))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let child_rel = rel.join(&name);

        if should_skip(&name_str, rel, filter) {
            entries_skipped.fetch_add(1, Ordering::Relaxed);
            tracing::trace!(entry = %name_str, rel = %child_rel.display(), "skipping .git/ entry");
            continue;
        }

        let source_path = entry.path();
        let dest_path = dest.join(&name);

        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to get file type for {}", source_path.display()))?;

        if file_type.is_dir() {
            collect_work_recursive(
                &source_path,
                &dest_path,
                &child_rel,
                filter,
                work_items,
                dirs_created,
                entries_skipped,
            )?;
        } else if file_type.is_file() || file_type.is_symlink() {
            // Regular file or symlink — add to work list.
            work_items.push(CopyWork {
                source: source_path,
                dest: dest_path,
            });
        } else {
            // Non-regular file (Unix socket, FIFO, device): it cannot be
            // reflinked or copied as a file, and it is transient host-local
            // state with no meaning in a copy (e.g. git's leftover
            // `fsmonitor--daemon.ipc` socket). Skip it instead of failing the
            // whole `.git/` copy.
            entries_skipped.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(entry = %name_str, rel = %child_rel.display(), "skipping non-regular .git/ entry");
        }
    }

    Ok(())
}

/// Copy a single file or symlink entry.
fn copy_single_entry(
    source_path: &Path,
    dest_path: &Path,
    files_copied: &AtomicU64,
    symlinks_copied: &AtomicU64,
) -> Result<()> {
    // Check if it's a symlink by querying symlink metadata.
    let metadata = std::fs::symlink_metadata(source_path)
        .with_context(|| format!("failed to stat {}", source_path.display()))?;

    if metadata.is_symlink() {
        // `target` is only used by the Unix symlink-recreate path. On
        // Windows we copy the link as a regular file (no native symlink),
        // so the target is never inspected.
        #[cfg(unix)]
        {
            let target = std::fs::read_link(source_path)
                .with_context(|| format!("failed to read symlink {}", source_path.display()))?;
            std::os::unix::fs::symlink(&target, dest_path).with_context(|| {
                format!(
                    "failed to create symlink {} -> {}",
                    dest_path.display(),
                    target.display()
                )
            })?;
        }
        #[cfg(not(unix))]
        {
            clone_file(source_path, dest_path).with_context(|| {
                format!(
                    "failed to copy symlink as file {} -> {}",
                    source_path.display(),
                    dest_path.display()
                )
            })?;
        }
        symlinks_copied.fetch_add(1, Ordering::Relaxed);
    } else {
        clone_file(source_path, dest_path).with_context(|| {
            format!(
                "failed to copy {} -> {}",
                source_path.display(),
                dest_path.display()
            )
        })?;
        files_copied.fetch_add(1, Ordering::Relaxed);
    }

    Ok(())
}

/// Decide whether to skip a `.git/` entry based on its name and relative path.
fn should_skip(name: &str, rel_parent: &Path, filter: &StandaloneCopyFilter) -> bool {
    // Skip lock files at any depth
    if name.ends_with(".lock") {
        return true;
    }
    let at_root = rel_parent.as_os_str().is_empty();
    // Skip known top-level entries
    if at_root && SKIP_TOP_LEVEL.contains(&name) {
        return true;
    }
    if at_root && name == "shallow" && !filter.keep_shallow {
        return true;
    }
    filter.should_skip_origin_remote(&rel_parent.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_copy_git_dir_basic() {
        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        // Create a minimal .git structure
        std::fs::create_dir_all(source_git.join("objects/pack")).unwrap();
        std::fs::create_dir_all(source_git.join("refs/heads")).unwrap();
        std::fs::write(source_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(source_git.join("config"), "[core]\n\tbare = false\n").unwrap();
        std::fs::write(source_git.join("index"), "fake index data").unwrap();
        std::fs::write(
            source_git.join("objects/pack/pack-abc.pack"),
            "fake pack data",
        )
        .unwrap();
        std::fs::write(source_git.join("refs/heads/main"), "abc123\n").unwrap();

        let stats = copy_git_dir(&source_git, &dest_git).unwrap();

        assert!(dest_git.join("HEAD").exists());
        assert!(dest_git.join("config").exists());
        assert!(dest_git.join("index").exists());
        assert!(dest_git.join("objects/pack/pack-abc.pack").exists());
        assert!(dest_git.join("refs/heads/main").exists());
        assert!(stats.files_copied >= 5);
    }

    #[test]
    fn test_copy_git_dir_skips_worktrees() {
        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        std::fs::create_dir_all(source_git.join("worktrees/wt1")).unwrap();
        std::fs::write(source_git.join("worktrees/wt1/gitdir"), "/some/path").unwrap();
        std::fs::write(source_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let stats = copy_git_dir(&source_git, &dest_git).unwrap();

        assert!(dest_git.join("HEAD").exists());
        assert!(!dest_git.join("worktrees").exists());
        assert!(stats.entries_skipped >= 1);
    }

    #[test]
    fn test_copy_git_dir_skips_lock_files() {
        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        std::fs::create_dir_all(&source_git).unwrap();
        std::fs::write(source_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(source_git.join("index.lock"), "locked").unwrap();
        std::fs::write(source_git.join("config.lock"), "locked").unwrap();

        let stats = copy_git_dir(&source_git, &dest_git).unwrap();

        assert!(dest_git.join("HEAD").exists());
        assert!(!dest_git.join("index.lock").exists());
        assert!(!dest_git.join("config.lock").exists());
        assert!(stats.entries_skipped >= 2);
    }

    #[test]
    fn test_copy_git_dir_skips_lock_files_in_subdirs() {
        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        std::fs::create_dir_all(source_git.join("refs/heads")).unwrap();
        std::fs::write(source_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(source_git.join("refs/heads/main"), "abc123\n").unwrap();
        std::fs::write(source_git.join("refs/heads/main.lock"), "locked").unwrap();

        let stats = copy_git_dir(&source_git, &dest_git).unwrap();

        assert!(dest_git.join("refs/heads/main").exists());
        assert!(!dest_git.join("refs/heads/main.lock").exists());
        assert!(stats.entries_skipped >= 1);
    }

    #[test]
    fn test_copy_git_dir_skips_transient_state() {
        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        std::fs::create_dir_all(&source_git).unwrap();
        std::fs::write(source_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(source_git.join("MERGE_HEAD"), "abc123").unwrap();
        std::fs::write(source_git.join("CHERRY_PICK_HEAD"), "def456").unwrap();
        std::fs::write(source_git.join("ORIG_HEAD"), "ghi789").unwrap();
        std::fs::write(source_git.join("FETCH_HEAD"), "jkl012").unwrap();
        std::fs::create_dir_all(source_git.join("rebase-merge")).unwrap();
        std::fs::write(source_git.join("rebase-merge/head-name"), "main").unwrap();
        std::fs::create_dir_all(source_git.join("sequencer")).unwrap();
        std::fs::write(source_git.join("sequencer/todo"), "pick abc123").unwrap();

        let stats = copy_git_dir(&source_git, &dest_git).unwrap();

        assert!(dest_git.join("HEAD").exists());
        assert!(!dest_git.join("MERGE_HEAD").exists());
        assert!(!dest_git.join("CHERRY_PICK_HEAD").exists());
        assert!(!dest_git.join("ORIG_HEAD").exists());
        assert!(!dest_git.join("FETCH_HEAD").exists());
        assert!(!dest_git.join("rebase-merge").exists());
        assert!(!dest_git.join("sequencer").exists());
        assert!(stats.entries_skipped >= 6);
    }

    #[test]
    fn test_copy_git_dir_skips_fsmonitor_daemon_state() {
        // git's fsmonitor leaves a `fsmonitor--daemon/` dir (and an `.ipc`
        // socket) of host-local runtime state. It must not be inherited by a
        // standalone copy.
        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        std::fs::create_dir_all(source_git.join("fsmonitor--daemon/cookies")).unwrap();
        std::fs::write(source_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let stats = copy_git_dir(&source_git, &dest_git).unwrap();

        assert!(dest_git.join("HEAD").exists());
        assert!(!dest_git.join("fsmonitor--daemon").exists());
        assert!(stats.entries_skipped >= 1);
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_git_dir_skips_non_regular_files() {
        // A leftover Unix-domain socket (e.g. git's `fsmonitor--daemon.ipc`)
        // cannot be reflinked or copied as a file. It must be skipped, not fail
        // the whole `.git/` copy. Uses a non-fsmonitor name so this exercises
        // the type-based skip rather than the SKIP_TOP_LEVEL name match.
        use std::os::unix::net::UnixListener;

        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        std::fs::create_dir_all(&source_git).unwrap();
        std::fs::write(source_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let _socket = UnixListener::bind(source_git.join("daemon.sock")).unwrap();

        let stats = copy_git_dir(&source_git, &dest_git).unwrap();

        assert!(dest_git.join("HEAD").exists());
        assert!(!dest_git.join("daemon.sock").exists());
        assert!(stats.entries_skipped >= 1);
    }

    #[test]
    fn test_copy_git_dir_preserves_hooks() {
        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        std::fs::create_dir_all(source_git.join("hooks")).unwrap();
        std::fs::write(source_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            source_git.join("hooks/pre-commit"),
            "#!/bin/bash\necho check",
        )
        .unwrap();

        let _stats = copy_git_dir(&source_git, &dest_git).unwrap();

        assert!(dest_git.join("hooks/pre-commit").exists());
        assert_eq!(
            std::fs::read_to_string(dest_git.join("hooks/pre-commit")).unwrap(),
            "#!/bin/bash\necho check"
        );
    }

    #[test]
    fn test_copy_git_dir_preserves_worktree_source_marker() {
        // A worktree-from-worktree (standalone) must inherit the source's
        // `echo-agent-worktree-source` marker so it still points at the ultimate
        // main repo rather than the intermediate worktree.
        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        std::fs::create_dir_all(&source_git).unwrap();
        std::fs::write(source_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(source_git.join("echo-agent-worktree-source"), "/main/repo").unwrap();

        copy_git_dir(&source_git, &dest_git).unwrap();

        assert_eq!(
            std::fs::read_to_string(dest_git.join("echo-agent-worktree-source")).unwrap(),
            "/main/repo"
        );
    }

    #[test]
    fn test_copy_git_dir_propagates_entry_copy_error() {
        // A failed entry copy must surface as an error, not a silently-corrupt
        // "success". `max_workers = 4` + >= 64 items forces the PARALLEL branch
        // deterministically (independent of num_cpus).
        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        std::fs::create_dir_all(&source_git).unwrap();
        std::fs::write(source_git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        for i in 0..128 {
            std::fs::write(source_git.join(format!("obj{i}")), "data").unwrap();
        }

        // Pre-create the dest entry for `obj0` as a DIRECTORY so the file copy
        // onto it fails (EISDIR) deterministically, even as root.
        std::fs::create_dir_all(dest_git.join("obj0")).unwrap();

        let err = copy_git_dir_with_workers(&source_git, &dest_git, 4, &HashSet::new())
            .expect_err("a failed .git/ entry copy must propagate as an error");
        // The error names the failing entry, not some unrelated setup failure.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("obj0"),
            "error should reference the failing entry, got: {chain}"
        );
    }

    #[test]
    fn test_copy_git_dir_rejects_git_file() {
        let temp = TempDir::new().unwrap();
        let source_git = temp.path().join("source/.git");
        let dest_git = temp.path().join("dest/.git");

        // Create .git as a file (linked worktree), not a directory
        std::fs::create_dir_all(temp.path().join("source")).unwrap();
        std::fs::write(&source_git, "gitdir: /some/other/path").unwrap();

        let result = copy_git_dir(&source_git, &dest_git);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be a directory")
        );
    }
}
