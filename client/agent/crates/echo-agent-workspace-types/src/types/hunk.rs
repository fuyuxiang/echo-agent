//! Minimal serializable hunk shapes.
//!
//! TODO(workspace): align with `echo_agent_hunk_tracker::Hunk` and
//! `echo_agent_hunk_tracker::HunkAction` when the hunk tracker's wire surface
//! is extracted into this crate. The fields below are a strict subset
//! sufficient for the API surface to compile.

use serde::{Deserialize, Serialize};

use crate::identity::HunkId;

/// A single tracked hunk in a file.
///
/// TODO(workspace): align with `echo_agent_hunk_tracker::types::Hunk`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// Stable identifier for this hunk.
    pub id: HunkId,
    /// Absolute file path the hunk applies to (as a string for wire ease).
    #[serde(default)]
    pub path: String,
    /// Number of lines added by this hunk.
    #[serde(default)]
    pub added: u32,
    /// Number of lines removed by this hunk.
    #[serde(default)]
    pub removed: u32,
    /// 1-indexed start line in the current file.
    #[serde(default)]
    pub start_line: u32,
    /// Optional human-readable summary.
    #[serde(default)]
    pub summary: String,
}

/// Action applied to a hunk by `WorkspaceOpsRequest::ActOnHunk`.
///
/// TODO(workspace): align with `echo_agent_hunk_tracker::types::HunkAction`.
///
/// Tagged with `tag = "type", content = "data"` (adjacent tagging) to
/// match every other wire enum in the crate. See `crate::lib` doc-comment
/// "# Wire format" for the rationale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum HunkAction {
    /// Accept the hunk -- update baseline to include this change.
    Accept {
        /// Hunk to accept.
        hunk_id: HunkId,
    },
    /// Reject the hunk -- restore baseline content for the affected lines.
    Reject {
        /// Hunk to reject.
        hunk_id: HunkId,
    },
    /// Revert a previously-accepted hunk back to the baseline.
    Revert {
        /// Hunk to revert.
        hunk_id: HunkId,
    },
}
