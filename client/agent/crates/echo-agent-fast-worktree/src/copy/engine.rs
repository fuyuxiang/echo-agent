//! Parallel copy engine.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use crossbeam::channel::{Sender, bounded};
use dashmap::{DashMap, DashSet};
use ignore::{WalkBuilder, WalkState};
use tokio_util::sync::CancellationToken;

use crate::copy::shard::shard_for_path;
use crate::copy::skip::build_skip_matcher;
use crate::copy::types::{
    CopyEntry, CopyEntryKind, CopyStats, ParallelCopyConfig, ParallelCopyResult,
};
use crate::copy::worker::{WorkerCtx, run_worker};

/// Copy files from source to dest using parallel workers with hash-based sharding.
///
/// Returns both stats and the set of paths that were copied (for deduplication).
/// Maximum worker threads to prevent FD exhaustion on macOS.
/// macOS default ulimit is 256. With 8 workers + 8 walker threads = 16 threads,
/// each can have ~10 FDs open (deeply nested dirs), leaving headroom for other uses.
#[cfg(target_os = "macos")]
const MAX_PARALLEL_WORKERS: usize = 8;

#[cfg(not(target_os = "macos"))]
const MAX_PARALLEL_WORKERS: usize = 32;

pub(crate) fn copy_parallel(
    source: &Path,
    dest: &Path,
    config: ParallelCopyConfig,
    cancellation_token: CancellationToken,
) -> Result<ParallelCopyResult> {
    let num_workers = if config.num_workers == 0 {
        num_cpus::get().min(MAX_PARALLEL_WORKERS)
    } else {
        config.num_workers.min(MAX_PARALLEL_WORKERS)
    };

    // Build skip patterns matcher.
    let skip_matcher = if !config.skip_patterns.is_empty() {
        Some(build_skip_matcher(&config.skip_patterns)?)
    } else {
        None
    };

    // Create bounded channels for each shard.
    let channels: Vec<_> = (0..num_workers)
        .map(|_| bounded::<CopyEntry>(config.channel_buffer))
        .collect();

    // Shared atomic counters for stats.
    let files_copied = Arc::new(AtomicU64::new(0));
    let dirs_created = Arc::new(AtomicU64::new(0));
    let symlinks_copied = Arc::new(AtomicU64::new(0));
    let files_skipped = Arc::new(AtomicU64::new(0));
    let issues: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));

    // Track successfully copied paths for deduplication.
    let copied_paths: Arc<DashSet<std::path::PathBuf>> = Arc::new(DashSet::new());

    // Collect file metadata for index updates.
    let file_metadata: Arc<DashMap<std::path::PathBuf, std::fs::Metadata>> =
        Arc::new(DashMap::new());

    // Spawn worker threads.
    let workers: Vec<_> = channels
        .iter()
        .map(|(_, rx)| {
            let rx = rx.clone();
            let ctx = WorkerCtx {
                source: source.to_path_buf(),
                dest: dest.to_path_buf(),
                files_copied: Arc::clone(&files_copied),
                dirs_created: Arc::clone(&dirs_created),
                symlinks_copied: Arc::clone(&symlinks_copied),
                issues: Arc::clone(&issues),
                copied_paths: Arc::clone(&copied_paths),
                file_metadata: Arc::clone(&file_metadata),
            };

            std::thread::spawn(move || run_worker(rx, ctx))
        })
        .collect();

    // Collect senders for the walker.
    let senders: Vec<Sender<CopyEntry>> = channels.iter().map(|(tx, _)| tx.clone()).collect();

    // Build the walker.
    // IMPORTANT: Limit walker threads to match num_workers to avoid FD exhaustion.
    // On macOS, the default FD limit (256) can easily be exceeded when:
    // - num_cpus walker threads (default) × directories open per thread
    // - Plus num_workers copy workers × files being copied
    // Deep directory trees (15+ levels) amplify this significantly.
    let mut builder = WalkBuilder::new(source);
    builder
        .hidden(false) // Include hidden files.
        .git_ignore(config.respect_gitignore)
        .git_global(false) // Never use global gitignore (~/.config/git/ignore) —
        // it contains personal preferences irrelevant to worktree creation.
        .git_exclude(false) // Never use .git/info/exclude — external tooling
        // can append broad patterns (*.min.js, *.zip) that
        // incorrectly skip git-tracked files. The `ignore` crate doesn't
        // check tracking status, so tracked files matching these patterns
        // get silently dropped during the copy.
        .threads(num_workers) // Limit walker parallelism to avoid FD exhaustion
        .filter_entry(|entry| {
            // Always skip .git directory.
            entry.file_name() != ".git"
        });

    let walker = builder.build_parallel();

    // Clone data for the walker closure.
    let source_for_walker = source.to_path_buf();
    let skip_files = config.skip_files.clone();
    let files_skipped_walker = Arc::clone(&files_skipped);
    let skip_matcher = skip_matcher.map(Arc::new);

    // Run the parallel walker.
    walker.run(|| {
        let senders = senders.clone();
        let source = source_for_walker.clone();
        let n = num_workers;
        let skip_files = skip_files.clone();
        let files_skipped = Arc::clone(&files_skipped_walker);
        let skip_matcher = skip_matcher.clone();
        let cancellation_token = cancellation_token.clone();

        Box::new(move |entry_result| {
            // Check for cancellation
            if cancellation_token.is_cancelled() {
                return WalkState::Quit;
            }

            let entry = match entry_result {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };

            // Get relative path.
            let rel_path = match entry.path().strip_prefix(&source) {
                Ok(p) => p.to_path_buf(),
                Err(_) => return WalkState::Continue,
            };

            // Skip root.
            if rel_path.as_os_str().is_empty() {
                return WalkState::Continue;
            }

            // Check if this file should be skipped (already copied or explicitly skipped).
            if let Some(ref skip) = skip_files
                && skip.contains(&rel_path)
            {
                files_skipped.fetch_add(1, Ordering::Relaxed);
                return WalkState::Continue;
            }

            // Check skip patterns.
            if let Some(ref matcher) = skip_matcher
                && matcher.is_match(&rel_path)
            {
                files_skipped.fetch_add(1, Ordering::Relaxed);
                return WalkState::Continue;
            }

            let file_type = entry.file_type();
            let is_dir = file_type.as_ref().map(|ft| ft.is_dir()).unwrap_or(false);
            let is_symlink = file_type
                .as_ref()
                .map(|ft| ft.is_symlink())
                .unwrap_or(false);

            let kind = if is_dir {
                CopyEntryKind::Dir
            } else if is_symlink {
                CopyEntryKind::Symlink
            } else {
                CopyEntryKind::File
            };

            // Compute shard and send.
            let shard = shard_for_path(&rel_path, n);
            let _ = senders[shard].send(CopyEntry { rel_path, kind });

            WalkState::Continue
        })
    });

    // Close senders to signal workers to finish.
    drop(senders);
    for (tx, _) in channels {
        drop(tx);
    }

    // Wait for all workers.
    for worker in workers {
        let _ = worker.join();
    }

    // Collect issues.
    let issues = match Arc::try_unwrap(issues) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().unwrap().clone(),
    };

    let copied_paths = match Arc::try_unwrap(copied_paths) {
        Ok(set) => set,
        Err(arc) => {
            let mut set = DashSet::new();
            set.extend(arc.iter().map(|p| p.clone()));
            set
        }
    };

    let file_metadata = match Arc::try_unwrap(file_metadata) {
        Ok(map) => map,
        Err(arc) => {
            let map = DashMap::new();
            for entry in arc.iter() {
                map.insert(entry.key().clone(), entry.value().clone());
            }
            map
        }
    };

    Ok(ParallelCopyResult {
        stats: CopyStats {
            files_copied: files_copied.load(Ordering::Relaxed),
            dirs_created: dirs_created.load(Ordering::Relaxed),
            symlinks_copied: symlinks_copied.load(Ordering::Relaxed),
            files_skipped: files_skipped.load(Ordering::Relaxed),
            issues,
        },
        copied_paths,
        file_metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copy::types::ParallelCopyConfig;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_copy_parallel_simple() {
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();

        // Create some files
        std::fs::write(src.path().join("file1.txt"), "content1").unwrap();
        std::fs::write(src.path().join("file2.txt"), "content2").unwrap();
        std::fs::create_dir(src.path().join("subdir")).unwrap();
        std::fs::write(src.path().join("subdir/file3.txt"), "content3").unwrap();

        let config = ParallelCopyConfig {
            num_workers: 2,
            channel_buffer: 64,
            respect_gitignore: false,
            ..Default::default()
        };

        let result =
            copy_parallel(src.path(), dest.path(), config, CancellationToken::new()).unwrap();

        assert_eq!(result.stats.files_copied, 3);
        assert!(dest.path().join("file1.txt").exists());
        assert!(dest.path().join("file2.txt").exists());
        assert!(dest.path().join("subdir/file3.txt").exists());
        assert_eq!(result.copied_paths.len(), 4); // 3 files + 1 dir
    }

    #[test]
    fn test_copy_parallel_with_skip() {
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();

        std::fs::write(src.path().join("keep.txt"), "keep").unwrap();
        std::fs::write(src.path().join("skip.txt"), "skip").unwrap();

        let skip = DashSet::new();
        skip.insert(PathBuf::from("skip.txt"));

        let config = ParallelCopyConfig {
            num_workers: 2,
            channel_buffer: 64,
            skip_files: Some(Arc::new(skip)),
            respect_gitignore: false,
            ..Default::default()
        };

        let result =
            copy_parallel(src.path(), dest.path(), config, CancellationToken::new()).unwrap();

        assert_eq!(result.stats.files_copied, 1);
        assert_eq!(result.stats.files_skipped, 1);
        assert!(dest.path().join("keep.txt").exists());
        assert!(!dest.path().join("skip.txt").exists());
    }

    #[test]
    fn test_copy_parallel_only_ignored() {
        echo_agent_test_utils::require_git!();
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();

        // Create a tracked file
        std::fs::write(src.path().join("tracked.txt"), "tracked").unwrap();

        // Create an "ignored" directory
        std::fs::create_dir(src.path().join("node_modules")).unwrap();
        std::fs::write(src.path().join("node_modules/pkg.txt"), "pkg").unwrap();

        // Create .gitignore
        std::fs::write(src.path().join(".gitignore"), "node_modules/").unwrap();

        // Initialize git repo
        std::process::Command::new("git")
            .current_dir(src.path())
            .args(["init"])
            .output()
            .unwrap();

        // Copy only ignored files (skip unignored paths)
        let config = ParallelCopyConfig {
            num_workers: 2,
            channel_buffer: 64,
            skip_files: Some(Arc::new(
                crate::copy::collect_unignored_paths(src.path(), 1).unwrap(),
            )),
            ..Default::default()
        };

        let _result =
            copy_parallel(src.path(), dest.path(), config, CancellationToken::new()).unwrap();

        // Should have copied node_modules but not tracked.txt
        assert!(dest.path().join("node_modules/pkg.txt").exists());
        assert!(!dest.path().join("tracked.txt").exists());
    }

    #[test]
    fn test_copy_parallel_with_cancellation_token_cancelled() {
        use tokio_util::sync::CancellationToken;

        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();

        // Create some files
        std::fs::write(src.path().join("file1.txt"), "content1").unwrap();
        std::fs::write(src.path().join("file2.txt"), "content2").unwrap();
        std::fs::write(src.path().join("file3.txt"), "content3").unwrap();

        // Create cancellation token - cancel immediately (pre-cancelled)
        let token = CancellationToken::new();
        token.cancel();

        let config = ParallelCopyConfig {
            num_workers: 2,
            channel_buffer: 64,
            respect_gitignore: false,
            ..Default::default()
        };

        // Pass the PRE-CANCELLED token (not a fresh one): the walker checks
        // cancellation first thing in every callback and quits, so nothing is
        // ever queued to the workers.
        let result = copy_parallel(src.path(), dest.path(), config, token).unwrap();

        assert_eq!(
            result.stats.files_copied, 0,
            "a pre-cancelled token must short-circuit the copy before any file is written"
        );
    }

    #[test]
    fn test_copy_parallel_cancellation_token_not_cancelled() {
        use tokio_util::sync::CancellationToken;

        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();

        // Create some files
        std::fs::write(src.path().join("file1.txt"), "content1").unwrap();
        std::fs::write(src.path().join("file2.txt"), "content2").unwrap();

        let config = ParallelCopyConfig {
            num_workers: 2,
            channel_buffer: 64,
            respect_gitignore: false,
            ..Default::default()
        };

        let result =
            copy_parallel(src.path(), dest.path(), config, CancellationToken::new()).unwrap();

        // All files should be copied
        assert_eq!(result.stats.files_copied, 2);
        assert!(dest.path().join("file1.txt").exists());
        assert!(dest.path().join("file2.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_parallel_replicates_symlink() {
        // Exercises the worker's CopyEntryKind::Symlink arm: a symlink in the
        // source tree must be replicated AS a symlink (not dereferenced).
        let src = TempDir::new().unwrap();
        let dest = TempDir::new().unwrap();

        std::fs::write(src.path().join("target.txt"), "content").unwrap();
        std::os::unix::fs::symlink("target.txt", src.path().join("link.txt")).unwrap();

        let config = ParallelCopyConfig {
            num_workers: 2,
            channel_buffer: 64,
            respect_gitignore: false,
            ..Default::default()
        };

        let result =
            copy_parallel(src.path(), dest.path(), config, CancellationToken::new()).unwrap();

        assert_eq!(result.stats.symlinks_copied, 1);
        let meta = std::fs::symlink_metadata(dest.path().join("link.txt")).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "link must be replicated as a symlink"
        );
        assert_eq!(
            std::fs::read_link(dest.path().join("link.txt")).unwrap(),
            PathBuf::from("target.txt")
        );
    }
}
