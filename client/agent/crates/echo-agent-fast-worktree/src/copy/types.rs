//! Shared types for copy operations.

use std::fs::Metadata;
use std::path::PathBuf;
use std::sync::Arc;

use dashmap::{DashMap, DashSet};

/// A structured report about dirty (modified/untracked/deleted) files in the source worktree.
#[derive(Clone, Debug, Default)]
pub struct DirtyFilesReport {
    pub modified_files: u64,
    pub untracked_files: u64,
    pub deleted_files: u64,
}

/// Statistics from a copy operation.
#[derive(Clone, Debug, Default)]
pub struct CopyStats {
    pub files_copied: u64,
    pub dirs_created: u64,
    pub symlinks_copied: u64,
    pub files_skipped: u64,
    /// Non-fatal issues encountered while copying.
    pub issues: Vec<String>,
}

impl CopyStats {
    /// Merge another stats into this one.
    pub fn merge(&mut self, other: CopyStats) {
        self.files_copied += other.files_copied;
        self.dirs_created += other.dirs_created;
        self.symlinks_copied += other.symlinks_copied;
        self.files_skipped += other.files_skipped;
        self.issues.extend(other.issues);
    }
}

/// Kind of filesystem entry to replicate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CopyEntryKind {
    File,
    Dir,
    Symlink,
}

/// Entry to be processed by a worker.
#[derive(Debug)]
pub(crate) struct CopyEntry {
    pub(crate) rel_path: PathBuf,
    pub(crate) kind: CopyEntryKind,
}

/// Configuration for the parallel copy operation.
#[derive(Clone, Debug, Default)]
pub(crate) struct ParallelCopyConfig {
    /// Number of parallel workers (0 = num_cpus)
    pub num_workers: usize,
    /// Channel buffer size per shard
    pub channel_buffer: usize,
    /// Files to skip (relative paths)
    pub skip_files: Option<Arc<DashSet<PathBuf>>>,
    /// Whether to respect `.gitignore` rules
    pub respect_gitignore: bool,
    /// Additional patterns to skip (glob patterns)
    pub skip_patterns: Vec<String>,
}

/// Result of a parallel copy operation, including stats and the set of copied paths.
#[derive(Clone, Debug, Default)]
pub(crate) struct ParallelCopyResult {
    pub stats: CopyStats,
    /// All relative paths that were successfully copied (for deduplication in subsequent copies).
    pub copied_paths: DashSet<PathBuf>,
    /// Metadata for files that were copied (for index updates). Only regular files, not symlinks/dirs.
    pub file_metadata: DashMap<PathBuf, Metadata>,
}
