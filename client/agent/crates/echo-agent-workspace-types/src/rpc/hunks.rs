//! Hunk tracker methods (`workspace.hunk_*`).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{RpcActivityClass, WorkspaceRpc};

/// Wire-safe hunk action enum. Maps to `echo_agent_hunk_tracker::types::HunkAction`
/// but carries `Serialize + Deserialize` for RPC transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HunkActionKind {
    Accept,
    Reject,
}

/// Single-hunk action payload, nested inside [`HunkSingleActionReq`]
/// (which owns the `workspace.hunk_action` wire contract).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkActionReq {
    pub hunk_id: String,
    pub action: HunkActionKind,
}

/// Single-hunk action. Wire format: `{ "action": { "hunk_id": ..., "action": ... } }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkSingleActionReq {
    pub action: HunkActionReq,
}

impl WorkspaceRpc for HunkSingleActionReq {
    const METHOD: &'static str = "workspace.hunk_action";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = HunkActionResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkFileActionReq {
    pub path: String,
    pub action: HunkActionKind,
}

impl WorkspaceRpc for HunkFileActionReq {
    const METHOD: &'static str = "workspace.hunk_file_action";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = BulkHunkActionResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkTurnActionReq {
    pub prompt_index: usize,
    pub action: HunkActionKind,
}

impl WorkspaceRpc for HunkTurnActionReq {
    const METHOD: &'static str = "workspace.hunk_turn_action";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = BulkHunkActionResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkAllActionReq {
    pub action: HunkActionKind,
}

impl WorkspaceRpc for HunkAllActionReq {
    const METHOD: &'static str = "workspace.hunk_all_action";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = BulkHunkActionResponse;
}

/// Get staged file paths.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HunkGetStagedFilesReq {}

impl WorkspaceRpc for HunkGetStagedFilesReq {
    const METHOD: &'static str = "workspace.hunk_get_staged_files";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Vec<String>;
}

/// Get per-file hunk summaries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HunkGetFileSummariesReq {}

impl WorkspaceRpc for HunkGetFileSummariesReq {
    const METHOD: &'static str = "workspace.hunk_get_file_summaries";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Vec<FileSummary>;
}

/// Response for a single-hunk action (accept/reject).
///
/// The hub handler returns `null` on success; this struct provides a typed
/// alternative for `WorkspaceOp` dispatch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HunkActionResponse {}

/// Response for bulk hunk actions (file-level, turn-level, all-hunks).
///
/// Contains the IDs of all hunks that were affected by the action.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BulkHunkActionResponse {
    pub affected: Vec<String>,
}

/// Summary of a single file's hunk-tracking state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSummary {
    pub path: String,
    pub hunk_count: usize,
    pub is_agent_file: bool,
}

// =========================================================================
// Request types whose responses reference `echo_agent_hunk_tracker` types (which pull
// in `gix`), so those responses are mirrored below as wire structs.
// =========================================================================

/// Get all tracked hunks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HunkGetAllHunksReq {}

impl WorkspaceRpc for HunkGetAllHunksReq {
    const METHOD: &'static str = "workspace.get_all_hunks";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Vec<HunkWire>;
}

/// Get every tracked file's baseline + current content.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HunkGetAllFileContentsReq {}

impl WorkspaceRpc for HunkGetAllFileContentsReq {
    const METHOD: &'static str = "workspace.hunk_get_all_file_contents";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Vec<FileContentEntryWire>;
}

/// Get the session-level hunk summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HunkGetSessionSummaryReq {}

impl WorkspaceRpc for HunkGetSessionSummaryReq {
    const METHOD: &'static str = "workspace.get_session_summary";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = SessionSummaryWire;
}

/// Get hunks filtered by path and/or source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HunkGetFilteredHunksReq {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

impl WorkspaceRpc for HunkGetFilteredHunksReq {
    const METHOD: &'static str = "workspace.hunk_get_filtered_hunks";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = FilteredHunksResponse;
}

// =========================================================================
// Wire mirrors of `echo_agent_hunk_tracker` response types
// =========================================================================

/// Wire mirror of `echo_agent_hunk_tracker::types::Hunk` (the `selected` field is
/// `#[serde(skip)]` upstream and so is omitted here).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HunkWire {
    pub id: String,
    pub path: PathBuf,
    pub line_info: HunkLineInfoWire,
    pub source: HunkSourceWire,
    pub old_text: Option<String>,
    pub new_text: String,
    pub patch: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Wire mirror of `echo_agent_hunk_tracker::types::HunkLineInfo`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HunkLineInfoWire {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
}

/// Wire mirror of `echo_agent_hunk_tracker::types::HunkSource`.
///
/// `Unknown` (`#[serde(other)]`) keeps decoding forward-tolerant: an
/// unrecognized `type` tag from a newer server decodes here instead of failing
/// the whole structured response. The server only ever produces the known
/// variants.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HunkSourceWire {
    AgentEdit {
        prompt_index: usize,
    },
    ExternalEditOnAgentFile,
    External,
    #[serde(other)]
    Unknown,
}

/// Wire mirror of `echo_agent_hunk_tracker::types::FileContentStatus`.
///
/// `Deserialize` is hand-written so an unrecognized status from a newer server
/// decodes to [`Unknown`](Self::Unknown) rather than failing the whole
/// structured response. A plain string enum cannot use `#[serde(other)]` (only
/// allowed on internally/adjacently tagged enums), hence the manual impl. The
/// server only produces the known variants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileContentStatusWire {
    #[default]
    Missing,
    Binary,
    TooLarge,
    LfsPointer,
    Symlink,
    Full,
    /// A status string this client does not know (a newer server variant).
    Unknown,
}

impl<'de> Deserialize<'de> for FileContentStatusWire {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "missing" => Self::Missing,
            "binary" => Self::Binary,
            "tooLarge" => Self::TooLarge,
            "lfsPointer" => Self::LfsPointer,
            "symlink" => Self::Symlink,
            "full" => Self::Full,
            // Forward-tolerant: an unknown status decodes here.
            _ => Self::Unknown,
        })
    }
}

/// Wire mirror of `echo_agent_hunk_tracker::types::FileContentView`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContentViewWire {
    pub status: FileContentStatusWire,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// Wire mirror of `echo_agent_hunk_tracker::types::FileContentEntry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContentEntryWire {
    pub path: PathBuf,
    pub baseline: FileContentViewWire,
    pub current: FileContentViewWire,
    pub is_agent_file: bool,
    pub staged: bool,
}

/// Wire mirror of `echo_agent_hunk_tracker::types::SessionStats`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatsWire {
    pub accepted_hunks: usize,
    pub rejected_hunks: usize,
    pub accepted_lines_added: usize,
    pub accepted_lines_removed: usize,
    pub rejected_lines_added: usize,
    pub rejected_lines_removed: usize,
}

/// Wire mirror of `echo_agent_hunk_tracker::types::TurnSummary`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSummaryWire {
    pub prompt_index: usize,
    pub files: Vec<PathBuf>,
    pub pending_hunks: Vec<HunkWire>,
    pub lines_added: usize,
    pub lines_removed: usize,
}

/// Wire mirror of `echo_agent_hunk_tracker::types::SessionSummary`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummaryWire {
    pub stats: SessionStatsWire,
    pub turns: Vec<TurnSummaryWire>,
    pub files_modified: usize,
    pub files_with_pending: usize,
    pub pending_hunks: usize,
    pub pending_lines_added: usize,
    pub pending_lines_removed: usize,
    pub unattributed_pending: usize,
}

/// Response containing a filtered set of hunks. Mirrors the server-side
/// `FilteredHunksResponse` shape (no `rename_all`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FilteredHunksResponse {
    pub hunks: Vec<HunkWire>,
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_constants() {
        assert_eq!(HunkSingleActionReq::METHOD, "workspace.hunk_action");
        assert_eq!(HunkFileActionReq::METHOD, "workspace.hunk_file_action");
        assert_eq!(HunkTurnActionReq::METHOD, "workspace.hunk_turn_action");
        assert_eq!(HunkAllActionReq::METHOD, "workspace.hunk_all_action");
        assert_eq!(
            HunkGetStagedFilesReq::METHOD,
            "workspace.hunk_get_staged_files"
        );
        assert_eq!(
            HunkGetFileSummariesReq::METHOD,
            "workspace.hunk_get_file_summaries"
        );
        assert_eq!(HunkGetAllHunksReq::METHOD, "workspace.get_all_hunks");
        assert_eq!(
            HunkGetAllFileContentsReq::METHOD,
            "workspace.hunk_get_all_file_contents"
        );
        assert_eq!(
            HunkGetSessionSummaryReq::METHOD,
            "workspace.get_session_summary"
        );
        assert_eq!(
            HunkGetFilteredHunksReq::METHOD,
            "workspace.hunk_get_filtered_hunks"
        );
    }

    #[test]
    fn hunk_wire_round_trips_server_json() {
        // A representative server-side `Hunk` serialization (camelCase, no
        // `selected` field).
        let json = serde_json::json!({
            "id": "hunk-1",
            "path": "/repo/src/main.rs",
            "lineInfo": { "oldStart": 1, "oldCount": 2, "newStart": 1, "newCount": 3 },
            // enum-level `rename_all` renames the variant (`agentEdit`) but NOT
            // struct-variant fields, so `prompt_index` stays snake_case.
            "source": { "type": "agentEdit", "prompt_index": 4 },
            "oldText": "old\n",
            "newText": "new\n",
            "patch": null,
            "createdAt": "2026-06-23T00:00:00Z"
        });
        let wire: HunkWire = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&wire).unwrap(), json);
    }

    #[test]
    fn file_content_entry_wire_omits_absent_byte_len_and_content() {
        let json = serde_json::json!({
            "path": "/repo/new.rs",
            "baseline": { "status": "missing" },
            "current": { "status": "full", "byteLen": 4, "content": "new\n" },
            "isAgentFile": true,
            "staged": false
        });
        let wire: FileContentEntryWire = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&wire).unwrap(), json);
    }

    #[test]
    fn hunk_action_kind_lowercase_wire_values() {
        assert_eq!(
            serde_json::to_value(HunkActionKind::Accept).unwrap(),
            serde_json::json!("accept")
        );
        assert_eq!(
            serde_json::to_value(HunkActionKind::Reject).unwrap(),
            serde_json::json!("reject")
        );
    }

    #[test]
    fn hunk_source_wire_unknown_type_decodes_tolerantly() {
        // An unrecognized `type` tag from a newer server decodes to Unknown
        // rather than erroring.
        let src: HunkSourceWire =
            serde_json::from_value(serde_json::json!({ "type": "futureSource" })).unwrap();
        assert!(matches!(src, HunkSourceWire::Unknown));
    }

    #[test]
    fn file_content_status_wire_unknown_decodes_tolerantly() {
        // An unrecognized status string decodes to Unknown.
        let status: FileContentStatusWire =
            serde_json::from_value(serde_json::json!("futureStatus")).unwrap();
        assert_eq!(status, FileContentStatusWire::Unknown);
        // Embedded in a FileContentEntryWire, the whole structured response still
        // decodes.
        let entry: FileContentEntryWire = serde_json::from_value(serde_json::json!({
            "path": "/x.rs",
            "baseline": { "status": "futureStatus" },
            "current": { "status": "full", "byteLen": 1, "content": "a" },
            "isAgentFile": false,
            "staged": false
        }))
        .unwrap();
        assert_eq!(entry.baseline.status, FileContentStatusWire::Unknown);
    }
}
