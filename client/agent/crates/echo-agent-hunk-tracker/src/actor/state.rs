//! Internal state types for the HunkTrackerActor.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use crate::types::Hunk;

/// Maximum size (in bytes) of file text content to retain in memory.
/// Files larger than this are stored as TooLarge.
/// This is aligned with the diff limit to ensure consistent behavior.
pub(crate) const MAX_TRACKED_TEXT_BYTES: usize = 1024 * 1024; // 1 MB

/// Explicit state of file content storage.
/// Replaces Option<String> for baseline/current_content to avoid unbounded memory.
/// Files exceeding MAX_TRACKED_TEXT_BYTES or containing binary content are
/// stored with metadata only (no text retained).
///
/// `Serialize`/`Deserialize` back the disk-persisted rewind checkpoint store; the
/// default externally-tagged representation round-trips every variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileContentState {
    /// File does not exist at this reference point.
    Missing,
    /// File content is not valid UTF-8 (binary).
    /// byte_len is optional because we may not know the size.
    Binary { byte_len: Option<usize> },
    /// File exceeds MAX_TRACKED_TEXT_BYTES; content not retained.
    TooLarge { byte_len: usize },
    /// File content is a Git LFS pointer (small text stub that references
    /// the real object in the LFS store). Not diffable because the working
    /// copy holds the smudged (real) content while the git blob holds only
    /// the pointer — comparing them produces a phantom diff.
    LfsPointer { byte_len: usize },
    /// Path is a symbolic link. Not diffable because the hunk tracker
    /// follows symlinks when reading content, producing a phantom diff
    /// against the git-stored symlink target string.
    Symlink,
    /// Full text content retained (within limit).
    Full(String),
}

impl FileContentState {
    /// Returns the text content if Full, None otherwise.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            FileContentState::Full(s) => Some(s),
            _ => None,
        }
    }

    /// Returns true if content can be used for diffing.
    pub(crate) fn is_diffable(&self) -> bool {
        matches!(self, FileContentState::Full(_))
    }

    /// Returns byte length if known.
    #[allow(dead_code)]
    pub(crate) fn byte_len(&self) -> Option<usize> {
        match self {
            FileContentState::Missing => Some(0),
            FileContentState::Binary { byte_len } => *byte_len,
            FileContentState::TooLarge { byte_len } => Some(*byte_len),
            FileContentState::LfsPointer { byte_len } => Some(*byte_len),
            FileContentState::Symlink => None,
            FileContentState::Full(s) => Some(s.len()),
        }
    }
}

/// Cached state for git repository discovery.
/// Avoids repeated filesystem walks to find the repo root.
///
/// When a repo is discovered, we cache a `gix::ThreadSafeRepository` handle
/// so that subsequent operations can call `.to_thread_local()` (a cheap
/// `Arc` clone + thread-local wrapper) instead of re-opening the repo via
/// `gix::open()` on every `spawn_blocking` call.
#[derive(Clone)]
pub(crate) enum GitRepoState {
    /// Haven't attempted discovery yet
    Unknown,
    /// Discovered that working_dir is not inside a git repository
    NotARepo,
    /// Successfully discovered the git repository
    Discovered {
        /// Cached thread-safe repo handle. `.to_thread_local()` is cheap.
        repo: Arc<gix::ThreadSafeRepository>,
        /// Prefix to convert working_dir-relative paths to repo-relative paths
        /// (working_dir relative to repo_root, e.g., "subdir/nested" if working_dir is repo_root/subdir/nested)
        prefix: PathBuf,
    },
}

impl std::fmt::Debug for GitRepoState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => write!(f, "Unknown"),
            Self::NotARepo => write!(f, "NotARepo"),
            Self::Discovered { prefix, .. } => f
                .debug_struct("Discovered")
                .field("prefix", prefix)
                .finish_non_exhaustive(),
        }
    }
}

/// Git state used to decide when to refresh baselines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RepoSyncState {
    /// Last observed HEAD commit id.
    pub head_oid: Option<String>,
    /// Last observed .git/index modification time.
    pub index_mtime: Option<SystemTime>,
}

/// Internal state for a single tracked file.
pub(crate) struct FileHunkState {
    /// Content at git HEAD or session start (baseline for diffing).
    /// FileContentState::Missing means file didn't exist at baseline (new file).
    /// FileContentState::TooLarge/Binary means content not retained (metadata only).
    pub baseline: FileContentState,

    /// Last known content (from agent write or disk read).
    /// Used to detect external edits by comparing to disk content.
    /// FileContentState::Missing means file doesn't exist currently.
    pub current_content: FileContentState,

    /// Active hunks for this file (computed from baseline vs current).
    /// Wrapped in Arc for cheap cloning when returning from queries.
    /// Hunks only exist for Full text states on both sides.
    pub hunks: Vec<Arc<Hunk>>,

    /// True if agent has written to this file.
    /// Determines if file stays tracked in AgentOnly mode.
    pub is_agent_file: bool,

    /// True if the baseline has been patched by an accept action (diverged
    /// from git HEAD).  Used by `handle_file_change` to decide whether to
    /// re-read the baseline from git HEAD: if this flag is set and the new
    /// file content matches git HEAD, the baseline is refreshed and the flag
    /// cleared.  This handles `git restore .` without undoing accepts when
    /// the user makes a normal (non-restore) edit.
    pub baseline_accepted: bool,
}
