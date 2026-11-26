//! Core types for hunk tracking.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::{path::PathBuf, sync::Arc};
use thiserror::Error;

/// Unique identifier for a hunk.
/// Uses UUID for guaranteed uniqueness across sessions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HunkId(pub Arc<str>);

impl HunkId {
    /// Generate a new unique hunk ID
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string().into())
    }

    /// Create from existing string (for deserialization/testing)
    pub fn from_string(s: String) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for HunkId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for HunkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Show first 8 characters for display (respects char boundaries)
        let short: String = self.0.chars().take(8).collect();
        write!(f, "{}", short)
    }
}

/// Line information for a hunk (mirrors unified diff header).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HunkLineInfo {
    /// 1-indexed start line in baseline (old) file
    pub old_start: usize,
    /// Number of lines from baseline that were changed/deleted
    pub old_count: usize,
    /// 1-indexed start line in current (new) file
    pub new_start: usize,
    /// Number of lines in current that were added/modified
    pub new_count: usize,
}

impl std::fmt::Display for HunkLineInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_count, self.new_start, self.new_count
        )
    }
}

/// The source of a hunk - who made the change.
///
/// This enum distinguishes between:
/// - Changes made directly by the agent (with prompt attribution)
/// - External changes to files the agent has touched (tracked for session context)
/// - External changes to files the agent hasn't touched
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HunkSource {
    /// Change made by an agent tool at a specific prompt.
    /// The prompt_index identifies which agent turn made this change.
    AgentEdit {
        /// Prompt index when the change was made (required)
        prompt_index: usize,
    },

    /// External edit (by user) to a file the agent has previously touched.
    /// These are tracked separately so we know they're "part of agent session"
    /// but weren't written by the agent itself.
    ExternalEditOnAgentFile,

    /// External edit to a file the agent has NOT touched.
    /// Only tracked when TrackingMode::AllDirty is enabled.
    External,
}

impl HunkSource {
    /// Returns true if this was directly written by the agent
    pub fn is_agent_edit(&self) -> bool {
        matches!(self, HunkSource::AgentEdit { .. })
    }

    /// Returns true if this is any kind of agent-related source
    /// (either agent edit or user edit on agent file)
    pub fn is_agent_tracked(&self) -> bool {
        matches!(
            self,
            HunkSource::AgentEdit { .. } | HunkSource::ExternalEditOnAgentFile
        )
    }

    /// Returns true if this was an external (user) edit
    pub fn is_external(&self) -> bool {
        matches!(
            self,
            HunkSource::External | HunkSource::ExternalEditOnAgentFile
        )
    }

    /// Get prompt index if this was an agent edit
    pub fn prompt_index(&self) -> Option<usize> {
        match self {
            HunkSource::AgentEdit { prompt_index } => Some(*prompt_index),
            _ => None,
        }
    }
}

/// Error type for hunk actions (accept/reject).
#[derive(Debug, Error)]
pub enum HunkActionError {
    #[error("Hunk not found: {0}")]
    HunkNotFound(HunkId),

    #[error("Failed to write file {path}: {source}")]
    WriteError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to delete file {path}: {source}")]
    DeleteError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to read file {path}: {source}")]
    ReadError {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A single hunk representing a contiguous change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    /// Unique identifier for this hunk
    pub id: HunkId,
    /// Absolute file path
    pub path: PathBuf,
    /// Line position information
    pub line_info: HunkLineInfo,
    /// Who made this change
    pub source: HunkSource,
    /// The old text (lines removed/changed), None for new file
    pub old_text: Option<String>,
    /// The new text (lines added/changed)
    pub new_text: String,
    /// Unified diff patch fragment for this hunk (e.g., "@@ -10,3 +10,5 @@\n-old\n+new\n")
    pub patch: Option<String>,
    /// When this hunk was first detected
    pub created_at: DateTime<Utc>,
    /// Whether this hunk is selected in the UI
    #[serde(skip)]
    pub selected: bool,
}

impl Hunk {
    /// Create a new hunk for a file that was created (no baseline)
    pub fn file_created(path: PathBuf, content: String, source: HunkSource) -> Self {
        let line_count = content.lines().count().max(1);
        Self {
            id: HunkId::new(),
            path,
            line_info: HunkLineInfo {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: line_count,
            },
            source,
            old_text: None,
            new_text: content,
            patch: None,
            created_at: Utc::now(),
            selected: false,
        }
    }

    /// Create a new hunk for a file that was deleted
    pub fn file_deleted(path: PathBuf, content: String, source: HunkSource) -> Self {
        let line_count = content.lines().count().max(1);
        Self {
            id: HunkId::new(),
            path,
            line_info: HunkLineInfo {
                old_start: 1,
                old_count: line_count,
                new_start: 0,
                new_count: 0,
            },
            source,
            old_text: Some(content),
            new_text: String::new(),
            patch: None,
            created_at: Utc::now(),
            selected: false,
        }
    }

    /// Get a short summary for display
    pub fn summary(&self) -> String {
        let additions = self.new_text.lines().count();
        let deletions = self
            .old_text
            .as_ref()
            .map(|t| t.lines().count())
            .unwrap_or(0);
        format!("+{}/-{}", additions, deletions)
    }

    /// Get the display path with line number
    pub fn display_path(&self) -> String {
        format!("{}:{}", self.path.display(), self.line_info.new_start)
    }
}

/// Action to take on a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkAction {
    /// Accept the hunk - update baseline to include this change.
    /// After accept: baseline = current_content for the affected lines.
    Accept,
    /// Reject the hunk - revert file content back to baseline.
    /// After reject: file on disk is overwritten with baseline content.
    Reject,
}

/// Updates sent to clients when hunks change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HunkUpdate {
    /// A new hunk was created
    Added(Hunk),
    /// A hunk was removed (accepted, rejected, or reverted)
    Removed { hunk_id: HunkId },
    /// A hunk's position changed but content is the same
    Moved {
        hunk_id: HunkId,
        new_line_info: HunkLineInfo,
    },
}

/// Summary of a tracked file (for UI display).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSummary {
    pub path: PathBuf,
    pub hunk_count: usize,
    pub has_agent_changes: bool,
    pub has_external_changes: bool,
}

/// Filter for querying hunks.
#[derive(Debug, Clone, Default)]
pub struct HunkFilter {
    /// Filter by source
    pub source: Option<HunkSourceFilter>,
    /// Filter by path pattern (glob)
    pub path_pattern: Option<String>,
}

/// Filter for querying hunks by source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkSourceFilter {
    Agent,
    External,
}

/// How the hunk tracker should monitor files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingMode {
    /// Only track files the agent has written to
    #[default]
    AgentOnly,
    /// Track all git dirty files (agent files + external dirty files)
    AllDirty,
}

// ============================================================================
// Session Stats & Summary
// ============================================================================

/// Simple counters for session summary. Reset on baseline reset (commit).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    /// Number of hunks that have been accepted
    pub accepted_hunks: usize,
    /// Number of hunks that have been rejected
    pub rejected_hunks: usize,
    /// Lines added in accepted hunks
    pub accepted_lines_added: usize,
    /// Lines removed in accepted hunks
    pub accepted_lines_removed: usize,
    /// Lines added in rejected hunks (informational)
    pub rejected_lines_added: usize,
    /// Lines removed in rejected hunks (informational)
    pub rejected_lines_removed: usize,
}

/// Summary of pending changes for a single agent turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSummary {
    /// The prompt index this turn corresponds to
    pub prompt_index: usize,
    /// Files modified in this turn (unique paths)
    pub files: Vec<PathBuf>,
    /// Pending hunks in this turn (wrapped in Arc for cheap cloning)
    pub pending_hunks: Vec<Arc<Hunk>>,
    /// Lines added (sum of pending hunk new_count)
    pub lines_added: usize,
    /// Lines removed (sum of pending hunk old_count)
    pub lines_removed: usize,
}

/// Complete session summary: stats + pending hunks grouped by turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// Counters for accepted/rejected
    pub stats: SessionStats,
    /// Pending hunks grouped by turn
    pub turns: Vec<TurnSummary>,
    /// Total unique files with agent-attributed pending hunks
    pub files_modified: usize,
    /// Files still having agent-attributed pending hunks
    pub files_with_pending: usize,
    /// Total pending hunks (agent-attributed only)
    pub pending_hunks: usize,
    /// Pending lines added (agent-attributed only)
    pub pending_lines_added: usize,
    /// Pending lines removed (agent-attributed only)
    pub pending_lines_removed: usize,
    /// Pending hunks without prompt_index (e.g., external edits)
    pub unattributed_pending: usize,
}

// ============================================================================
// Content Status Types (for explicit API responses)
// ============================================================================

/// Status of file content - explicit discrimination for API consumers.
/// This replaces the ambiguous `Option<String>` where `None` could mean
/// missing, binary, or too large.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileContentStatus {
    /// File doesn't exist (deleted or never existed)
    #[default]
    Missing,
    /// File is binary (contains NUL bytes)
    Binary,
    /// File exceeds MAX_TRACKED_TEXT_BYTES (content not retained)
    TooLarge,
    /// File is a Git LFS pointer (raw blob is a small text stub; working
    /// copy holds the smudged content — not diffable)
    LfsPointer,
    /// Path is a symbolic link (not diffable)
    Symlink,
    /// File is diffable text (content available)
    Full,
}

/// View of file content with explicit status for API responses.
/// Combines status metadata with optional content string.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContentView {
    /// Explicit status of the content
    pub status: FileContentStatus,
    /// Size in bytes (available for Binary/TooLarge/Full states)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<usize>,
    /// Text content (only present when status is Full)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl FileContentView {
    /// Create a view for missing content
    pub fn missing() -> Self {
        Self {
            status: FileContentStatus::Missing,
            byte_len: None,
            content: None,
        }
    }

    /// Create a view for binary content
    pub fn binary(byte_len: Option<usize>) -> Self {
        Self {
            status: FileContentStatus::Binary,
            byte_len,
            content: None,
        }
    }

    /// Create a view for too-large content
    pub fn too_large(byte_len: usize) -> Self {
        Self {
            status: FileContentStatus::TooLarge,
            byte_len: Some(byte_len),
            content: None,
        }
    }

    /// Create a view for a Git LFS pointer
    pub fn lfs_pointer(byte_len: usize) -> Self {
        Self {
            status: FileContentStatus::LfsPointer,
            byte_len: Some(byte_len),
            content: None,
        }
    }

    /// Create a view for a symbolic link
    pub fn symlink() -> Self {
        Self {
            status: FileContentStatus::Symlink,
            byte_len: None,
            content: None,
        }
    }

    /// Create a view for full text content
    pub fn full(content: String) -> Self {
        let byte_len = content.len();
        Self {
            status: FileContentStatus::Full,
            byte_len: Some(byte_len),
            content: Some(content),
        }
    }

    /// Convert from internal FileContentState to API-facing FileContentView.
    /// This is the canonical conversion for query responses.
    pub fn from_content_state(state: &crate::actor::state::FileContentState) -> Self {
        use crate::actor::state::FileContentState;
        match state {
            FileContentState::Missing => Self::missing(),
            FileContentState::Binary { byte_len } => Self::binary(*byte_len),
            FileContentState::TooLarge { byte_len } => Self::too_large(*byte_len),
            FileContentState::LfsPointer { byte_len } => Self::lfs_pointer(*byte_len),
            FileContentState::Symlink => Self::symlink(),
            FileContentState::Full(content) => Self::full(content.clone()),
        }
    }
}

/// Per-file content entry returned by `GetAllFileContents`.
///
/// Contains baseline, current content, agent attribution, and staging
/// state for a single tracked file — everything a client needs to render
/// diffs without per-file round trips.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContentEntry {
    pub path: PathBuf,
    pub baseline: FileContentView,
    pub current: FileContentView,
    pub is_agent_file: bool,
    pub staged: bool,
}

/// File diff data including hunks and full file content.
/// Used to provide all data needed for diff rendering in one response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHunkData {
    /// Hunks for this file (each hunk includes its own patch fragment)
    pub hunks: Vec<Arc<Hunk>>,

    // === Explicit content status (new fields) ===
    /// Baseline content with explicit status (git HEAD)
    pub baseline: FileContentView,
    /// Current content with explicit status (on disk)
    pub current: FileContentView,

    // === Legacy fields for backward compatibility ===
    // These are populated from FileContentView for existing callers.
    // Will be deprecated once all callers migrate to baseline/current views.
    /// Baseline content (git HEAD) - legacy, use `baseline.content` instead
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_content: Option<String>,
    /// Current content (on disk) - legacy, use `current.content` instead
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_content: Option<String>,
}

// ============================================================================
// Snapshot / Restore (for cross-session sync-back)
// ============================================================================

// FileContentState is crate-internal (actor::state is pub(crate));
// imported here for snapshot serialization.
use crate::actor::state::FileContentState;

/// Snapshot of a single tracked file's hunk state.
/// Preserves the full FileContentState (including Binary/TooLarge) for correctness
/// in fork and cross-session sync flows.
///
/// `Serialize`/`Deserialize` let the rewind checkpoint store persist this to disk
/// (see [`HunkTurnDelta`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHunkStateSnapshot {
    /// Content at git HEAD or session start (baseline for diffing).
    /// FileContentState::Missing means file didn't exist at baseline (new file).
    /// FileContentState::TooLarge/Binary means content not retained (metadata only).
    pub baseline: FileContentState,
    /// Last known content (from agent write or disk read).
    /// FileContentState::Missing means file doesn't exist currently.
    pub current_content: FileContentState,
    /// Active hunks for this file.
    pub hunks: Vec<Hunk>,
    /// Whether the agent has written to this file.
    pub is_agent_file: bool,
    /// Whether the baseline has been patched by an accept action.
    pub baseline_accepted: bool,
}

/// Snapshot of all hunk tracker state.
///
/// Used to preserve pending hunks across session kill/reload cycles
/// (e.g., fork sync-back). Without this,
/// the session reload creates a fresh `HunkTrackerActor` with empty state,
/// causing all un-reviewed hunks to silently disappear — the user sees
/// their changes "auto-applied" because they're on disk but no longer
/// shown as reviewable.
#[derive(Debug, Clone)]
pub struct HunkTrackerSnapshot {
    /// All tracked files with their baselines, current content, hunks, and agent flags.
    pub file_states: HashMap<PathBuf, FileHunkStateSnapshot>,
    /// Secondary index: prompt_index → set of hunk IDs for that turn.
    pub turn_index: HashMap<usize, HashSet<HunkId>>,
    /// Session-level stats (accepted/rejected counts).
    pub session_stats: SessionStats,
}

/// Incremental, single-turn slice of hunk-tracker state, captured per
/// `prompt_index` for the rewind checkpoint store: snapshots of the turn's
/// touched files plus its hunk-id set, never a whole-tracker copy. Restore
/// composes deltas (ascending, last write per path wins) into a
/// [`HunkTrackerSnapshot`].
///
/// `Serialize`/`Deserialize` let the checkpoint store persist a delta to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkTurnDelta {
    /// The turn this delta belongs to.
    pub prompt_index: usize,
    /// Snapshots of the files touched in this turn (those owning the turn's hunks).
    pub file_states: HashMap<PathBuf, FileHunkStateSnapshot>,
    /// The hunk IDs attributed to this turn (`turn_index[prompt_index]`).
    pub hunk_ids: HashSet<HunkId>,
}

impl HunkTrackerSnapshot {
    /// Rewrite all absolute paths in the snapshot from one directory prefix
    /// to another. This is a **pure function** — no filesystem I/O.
    ///
    /// Used when transferring hunk state between sessions that operate in
    /// different directories (e.g., root cwd ↔ fork worktree). Rewrites:
    /// - `file_states` HashMap keys
    /// - `Hunk.path` field inside each file's hunks
    ///
    /// Both `old_cwd` and `canonical_old_cwd` should be provided by the
    /// caller (who canonicalizes while the directories still exist on disk).
    /// This avoids filesystem I/O inside the transform and ensures correct
    /// behavior even after worktree cleanup.
    ///
    /// Files whose paths cannot be rewritten (e.g., tracked outside the
    /// worktree) are kept at their original path with a warning log.
    pub fn rewrite_paths(
        &mut self,
        old_cwd: &std::path::Path,
        canonical_old_cwd: &std::path::Path,
        new_cwd: &std::path::Path,
    ) {
        let rewritten: HashMap<PathBuf, FileHunkStateSnapshot> = self
            .file_states
            .drain()
            .map(|(path, mut state)| {
                let new_path = rewrite_single_path(&path, old_cwd, canonical_old_cwd, new_cwd);
                let target_path = new_path.unwrap_or_else(|| {
                    tracing::warn!(
                        original_path = %path.display(),
                        old_cwd = %old_cwd.display(),
                        new_cwd = %new_cwd.display(),
                        "Cannot rewrite path: not under old_cwd, keeping original"
                    );
                    path
                });
                for hunk in &mut state.hunks {
                    hunk.path = target_path.clone();
                }
                (target_path, state)
            })
            .collect();

        self.file_states = rewritten;
    }
}

/// Rewrite a single absolute path from one directory prefix to another.
/// Pure function — no filesystem I/O.
///
/// Tries both raw and canonicalized prefix variants to handle macOS
/// symlinks (e.g., `/var` → `/private/var`) and paths stored with vs.
/// without symlink resolution.
///
/// Returns `None` if the path cannot be made relative to `old_cwd`
/// under any prefix variant.
pub(crate) fn rewrite_single_path(
    path: &std::path::Path,
    old_cwd: &std::path::Path,
    canonical_old: &std::path::Path,
    new_cwd: &std::path::Path,
) -> Option<PathBuf> {
    // Try stripping old_cwd prefix using both raw and canonical variants.
    // No canonicalize() calls here — the caller provides both variants.
    let relative = path
        .strip_prefix(canonical_old)
        .ok()
        .or_else(|| path.strip_prefix(old_cwd).ok());

    relative.map(|rel| new_cwd.join(rel))
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use chrono::Utc;

    fn make_snapshot(paths: &[&str]) -> HunkTrackerSnapshot {
        let mut file_states = HashMap::new();
        let mut turn_index: HashMap<usize, HashSet<HunkId>> = HashMap::new();

        for (i, path_str) in paths.iter().enumerate() {
            let path = PathBuf::from(path_str);
            let hunk_id = HunkId::from_string(format!("hunk-{i}"));
            let hunk = Hunk {
                id: hunk_id.clone(),
                path: path.clone(),
                line_info: HunkLineInfo {
                    old_start: 1,
                    old_count: 1,
                    new_start: 1,
                    new_count: 2,
                },
                source: HunkSource::AgentEdit { prompt_index: i },
                old_text: Some("old".to_string()),
                new_text: "new".to_string(),
                patch: None,
                created_at: Utc::now(),
                selected: false,
            };
            turn_index.entry(i).or_default().insert(hunk_id);
            file_states.insert(
                path,
                FileHunkStateSnapshot {
                    baseline: FileContentState::Full("old".to_string()),
                    current_content: FileContentState::Full("new".to_string()),
                    hunks: vec![hunk],
                    is_agent_file: true,
                    baseline_accepted: false,
                },
            );
        }

        HunkTrackerSnapshot {
            file_states,
            turn_index,
            session_stats: SessionStats::default(),
        }
    }

    #[test]
    fn rewrite_simple_prefix() {
        let mut snap = make_snapshot(&["/old/cwd/file.txt", "/old/cwd/sub/deep.rs"]);
        let old = std::path::Path::new("/old/cwd");
        let new = std::path::Path::new("/new/cwd");
        snap.rewrite_paths(old, old, new);

        let paths: Vec<_> = snap.file_states.keys().collect();
        assert!(paths.contains(&&PathBuf::from("/new/cwd/file.txt")));
        assert!(paths.contains(&&PathBuf::from("/new/cwd/sub/deep.rs")));
    }

    #[test]
    fn rewrite_updates_hunk_paths() {
        let mut snap = make_snapshot(&["/old/cwd/file.txt"]);
        let old = std::path::Path::new("/old/cwd");
        let new = std::path::Path::new("/new/cwd");
        snap.rewrite_paths(old, old, new);

        let state = snap
            .file_states
            .get(&PathBuf::from("/new/cwd/file.txt"))
            .unwrap();
        assert_eq!(state.hunks[0].path, PathBuf::from("/new/cwd/file.txt"));
    }

    #[test]
    fn rewrite_canonical_prefix_fallback() {
        // Simulate macOS: path stored as /private/var/... but old_cwd is /var/...
        let mut snap = make_snapshot(&["/private/var/folders/work/file.txt"]);
        let old_raw = std::path::Path::new("/var/folders/work");
        let old_canonical = std::path::Path::new("/private/var/folders/work");
        let new = std::path::Path::new("/new/cwd");
        snap.rewrite_paths(old_raw, old_canonical, new);

        assert!(
            snap.file_states
                .contains_key(&PathBuf::from("/new/cwd/file.txt"))
        );
    }

    #[test]
    fn rewrite_path_outside_old_cwd_kept_with_original() {
        let mut snap = make_snapshot(&["/other/dir/file.txt"]);
        let old = std::path::Path::new("/old/cwd");
        let new = std::path::Path::new("/new/cwd");
        snap.rewrite_paths(old, old, new);

        // Path outside old_cwd is kept at original path
        assert!(
            snap.file_states
                .contains_key(&PathBuf::from("/other/dir/file.txt"))
        );
    }

    #[test]
    fn rewrite_identity_is_noop() {
        let mut snap = make_snapshot(&["/same/cwd/file.txt"]);
        let cwd = std::path::Path::new("/same/cwd");
        snap.rewrite_paths(cwd, cwd, cwd);

        assert!(
            snap.file_states
                .contains_key(&PathBuf::from("/same/cwd/file.txt"))
        );
        let state = snap
            .file_states
            .get(&PathBuf::from("/same/cwd/file.txt"))
            .unwrap();
        assert_eq!(state.hunks[0].path, PathBuf::from("/same/cwd/file.txt"));
    }

    #[test]
    fn rewrite_preserves_turn_index_and_stats() {
        let mut snap = make_snapshot(&["/old/cwd/file.txt"]);
        snap.session_stats.accepted_hunks = 5;
        let old = std::path::Path::new("/old/cwd");
        let new = std::path::Path::new("/new/cwd");
        snap.rewrite_paths(old, old, new);

        // turn_index and stats are untouched
        assert!(!snap.turn_index.is_empty());
        assert_eq!(snap.session_stats.accepted_hunks, 5);
    }

    // === Snapshot preserves Binary/TooLarge (regression test) ===

    #[test]
    fn snapshot_preserves_binary_state() {
        let mut file_states = HashMap::new();
        file_states.insert(
            PathBuf::from("/test/binary.bin"),
            FileHunkStateSnapshot {
                baseline: FileContentState::Binary {
                    byte_len: Some(100),
                },
                current_content: FileContentState::Binary {
                    byte_len: Some(100),
                },
                hunks: vec![],
                is_agent_file: true,
                baseline_accepted: false,
            },
        );
        let snap = HunkTrackerSnapshot {
            file_states,
            turn_index: HashMap::new(),
            session_stats: SessionStats::default(),
        };

        // Snapshot should preserve Binary (not collapse to Missing)
        let state = snap
            .file_states
            .get(&PathBuf::from("/test/binary.bin"))
            .unwrap();
        assert!(matches!(state.baseline, FileContentState::Binary { .. }));
        assert!(matches!(
            state.current_content,
            FileContentState::Binary { .. }
        ));
    }

    #[test]
    fn snapshot_preserves_too_large_state() {
        let mut file_states = HashMap::new();
        file_states.insert(
            PathBuf::from("/test/huge.txt"),
            FileHunkStateSnapshot {
                baseline: FileContentState::TooLarge {
                    byte_len: 2_000_000,
                },
                current_content: FileContentState::TooLarge {
                    byte_len: 2_000_000,
                },
                hunks: vec![],
                is_agent_file: true,
                baseline_accepted: false,
            },
        );
        let snap = HunkTrackerSnapshot {
            file_states,
            turn_index: HashMap::new(),
            session_stats: SessionStats::default(),
        };

        // Snapshot should preserve TooLarge (not collapse to Missing)
        let state = snap
            .file_states
            .get(&PathBuf::from("/test/huge.txt"))
            .unwrap();
        assert!(matches!(state.baseline, FileContentState::TooLarge { .. }));
        assert!(matches!(
            state.current_content,
            FileContentState::TooLarge { .. }
        ));
    }
}

// ============================================================================
// FileContentView Tests (content status propagation)
// ============================================================================

#[cfg(test)]
mod content_view_tests {
    use super::*;
    use crate::actor::state::FileContentState;

    #[test]
    fn from_content_state_missing() {
        let state = FileContentState::Missing;
        let view = FileContentView::from_content_state(&state);

        assert_eq!(view.status, FileContentStatus::Missing);
        assert!(view.byte_len.is_none());
        assert!(view.content.is_none());
    }

    #[test]
    fn from_content_state_binary() {
        let state = FileContentState::Binary {
            byte_len: Some(1024),
        };
        let view = FileContentView::from_content_state(&state);

        assert_eq!(view.status, FileContentStatus::Binary);
        assert_eq!(view.byte_len, Some(1024));
        assert!(view.content.is_none());
    }

    #[test]
    fn from_content_state_binary_no_len() {
        let state = FileContentState::Binary { byte_len: None };
        let view = FileContentView::from_content_state(&state);

        assert_eq!(view.status, FileContentStatus::Binary);
        assert!(view.byte_len.is_none());
        assert!(view.content.is_none());
    }

    #[test]
    fn from_content_state_too_large() {
        let state = FileContentState::TooLarge {
            byte_len: 2_000_000,
        };
        let view = FileContentView::from_content_state(&state);

        assert_eq!(view.status, FileContentStatus::TooLarge);
        assert_eq!(view.byte_len, Some(2_000_000));
        assert!(view.content.is_none());
    }

    #[test]
    fn from_content_state_lfs_pointer() {
        let state = FileContentState::LfsPointer { byte_len: 130 };
        let view = FileContentView::from_content_state(&state);

        assert_eq!(view.status, FileContentStatus::LfsPointer);
        assert_eq!(view.byte_len, Some(130));
        assert!(view.content.is_none());
    }

    #[test]
    fn from_content_state_full() {
        let content = "hello world".to_string();
        let state = FileContentState::Full(content.clone());
        let view = FileContentView::from_content_state(&state);

        assert_eq!(view.status, FileContentStatus::Full);
        assert_eq!(view.byte_len, Some(11));
        assert_eq!(view.content, Some(content));
    }

    #[test]
    fn view_constructors() {
        // Test the convenience constructors
        let missing = FileContentView::missing();
        assert_eq!(missing.status, FileContentStatus::Missing);

        let binary = FileContentView::binary(Some(512));
        assert_eq!(binary.status, FileContentStatus::Binary);
        assert_eq!(binary.byte_len, Some(512));

        let too_large = FileContentView::too_large(5_000_000);
        assert_eq!(too_large.status, FileContentStatus::TooLarge);
        assert_eq!(too_large.byte_len, Some(5_000_000));

        let lfs = FileContentView::lfs_pointer(130);
        assert_eq!(lfs.status, FileContentStatus::LfsPointer);
        assert_eq!(lfs.byte_len, Some(130));
        assert!(lfs.content.is_none());

        let full = FileContentView::full("test".to_string());
        assert_eq!(full.status, FileContentStatus::Full);
        assert_eq!(full.byte_len, Some(4));
        assert_eq!(full.content, Some("test".to_string()));
    }

    #[test]
    fn default_view_is_missing() {
        let view = FileContentView::default();
        assert_eq!(view.status, FileContentStatus::Missing);
        assert!(view.byte_len.is_none());
        assert!(view.content.is_none());
    }

    #[test]
    fn default_status_is_missing() {
        let status = FileContentStatus::default();
        assert_eq!(status, FileContentStatus::Missing);
    }
}
