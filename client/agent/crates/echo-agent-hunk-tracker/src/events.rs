//! Events emitted by the HunkTrackerActor.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use crate::types::{Hunk, HunkId, HunkLineInfo, HunkSource};

/// Why a hunk was removed. Used by the LOC sink to decide whether to
/// negate the hunk's accumulated LOC contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HunkRemovalReason {
    /// User accepted the hunk — lines are kept in the file and should
    /// still count toward the author's LOC total.
    Accepted,
    /// User rejected/reverted the hunk — changes are undone, LOC should
    /// be zeroed out.
    Rejected,
    /// Hunk was replaced during recomputation (overlapping edit created
    /// a new hunk), baseline reset, or file cleanup. LOC should be
    /// zeroed out (the replacement hunk has its own records).
    Superseded,
}

/// Events emitted by the HunkTrackerActor when hunks change.
/// Sent via the update_tx channel to subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HunkEvent {
    /// A new hunk was created
    HunkAdded { path: PathBuf, hunk: Arc<Hunk> },

    /// A hunk was removed.
    HunkRemoved {
        path: PathBuf,
        hunk_id: HunkId,
        reason: HunkRemovalReason,
    },

    /// A hunk's position changed but content is the same
    HunkMoved {
        path: PathBuf,
        hunk_id: HunkId,
        new_line_info: HunkLineInfo,
    },

    /// A hunk's content changed in place (overlapping region, same hunk ID).
    /// Emitted when an edit modifies a hunk without fully removing/recreating it.
    ///
    /// `trigger_source` is the source of the *edit that triggered* this change
    /// (before source-preservation logic). This lets LOC tracking attribute
    /// the change correctly even when the hunk's own `source` field was
    /// preserved from a prior agent edit.
    ///
    /// `prev_lines_added` / `prev_lines_removed` are the line counts from the
    /// previous version of this hunk, so the LOC sink can compute the delta
    /// (new - prev) and attribute only the incremental change.
    HunkContentChanged {
        path: PathBuf,
        hunk: Arc<Hunk>,
        trigger_source: HunkSource,
        prev_lines_added: usize,
        prev_lines_removed: usize,
    },

    /// A file started being tracked
    FileAdded { path: PathBuf, is_agent_file: bool },

    /// A file stopped being tracked (all hunks gone, not an agent file)
    FileRemoved { path: PathBuf },

    /// Baseline was updated for a file (after accept or commit)
    BaselineUpdated { path: PathBuf },
}
