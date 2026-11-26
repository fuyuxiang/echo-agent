//! Hunk Tracker extension API layer.
//!
//! Provides access to the echo-agent-hunk-tracker functionality for tracking file changes
//! with agent/external attribution.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

use super::{ExtResult, parse_params, to_ext_response};
use crate::agent::MvpAgent;
use echo_agent_hunk_tracker::{
    FileContentEntry, FileContentStatus, FileContentView, Hunk, HunkTrackerHandle,
};
use echo_agent_workspace::workspace_ops::{
    FileContentEntryWire, FileContentStatusWire, FileContentViewWire, HunkActionKind,
    HunkActionReq, HunkAllActionReq, HunkFileActionReq, HunkGetAllFileContentsReq,
    HunkGetSessionSummaryReq, HunkSingleActionReq, HunkTurnActionReq,
};

// ═══════════════════════════════════════════════════════════════════════
// Request Types
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetHunksRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    /// Filter by file path (optional)
    pub path: Option<String>,
    /// Filter by source: "agent", "external", or "all" (default: "all")
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetFilesRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HunkActionRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    pub hunk_id: String,
    pub action: String, // "accept" | "reject"
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileActionRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    pub path: String,
    pub action: String, // "accept" | "reject"
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TurnActionRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    pub prompt_index: usize,
    pub action: String, // "accept" | "reject"
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AllActionRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
    pub action: String, // "accept" | "reject"
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetSummaryRequest {
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
}

// ═══════════════════════════════════════════════════════════════════════
// Response Types
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetHunksResponse {
    pub hunks: Vec<Arc<Hunk>>,

    // === Explicit content status (new fields) ===
    /// Baseline content with explicit status - only present when requesting a specific path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<FileContentView>,
    /// Current content with explicit status - only present when requesting a specific path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<FileContentView>,

    // === Legacy fields for backward compatibility ===
    /// Baseline content (git HEAD) - legacy, use `baseline.content` instead
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_content: Option<String>,
    /// Current content (on disk) - legacy, use `current.content` instead
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_content: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSummary {
    pub path: PathBuf,
    pub is_agent_file: bool,
    pub staged: bool,
    pub hunk_count: usize,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetFilesResponse {
    pub files: Vec<FileSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetAllFileContentsResponse {
    pub files: Vec<FileContentEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActionResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_count: Option<usize>,
}

// ═══════════════════════════════════════════════════════════════════════
// Helper Functions
// ═══════════════════════════════════════════════════════════════════════

/// Bridge the workspace RPC's lean wire response type back to the
/// hunk-tracker's `FileContentEntry`: the RPC returns the wire type while the
/// shell's ACP DTOs use the hunk-tracker types.
fn file_content_entry_from_wire(w: FileContentEntryWire) -> FileContentEntry {
    FileContentEntry {
        path: w.path,
        baseline: file_content_view_from_wire(w.baseline),
        current: file_content_view_from_wire(w.current),
        is_agent_file: w.is_agent_file,
        staged: w.staged,
    }
}

fn file_content_view_from_wire(w: FileContentViewWire) -> FileContentView {
    FileContentView {
        status: file_content_status_from_wire(w.status),
        byte_len: w.byte_len,
        content: w.content,
    }
}

fn file_content_status_from_wire(w: FileContentStatusWire) -> FileContentStatus {
    match w {
        FileContentStatusWire::Missing => FileContentStatus::Missing,
        FileContentStatusWire::Binary => FileContentStatus::Binary,
        FileContentStatusWire::TooLarge => FileContentStatus::TooLarge,
        FileContentStatusWire::LfsPointer => FileContentStatus::LfsPointer,
        FileContentStatusWire::Symlink => FileContentStatus::Symlink,
        FileContentStatusWire::Full => FileContentStatus::Full,
        // A status from a newer peer this build does not know: degrade to
        // Missing (content unavailable) rather than failing.
        FileContentStatusWire::Unknown => FileContentStatus::Missing,
    }
}

/// Session context for hunk tracker operations: the tracker handle plus
/// optional path rewriting info for forked sessions.
struct HunkTrackerContext {
    handle: HunkTrackerHandle,
    /// When set, rewrite absolute worktree paths (starting with `real_cwd`)
    /// to `display_cwd` in API responses so the client UI shows the original
    /// project path, not the worktree path.
    real_cwd: String,
    display_cwd: Option<String>,
}

impl HunkTrackerContext {
    /// Rewrite a worktree path to the display path for client-facing output.
    fn display_path(&self, path: &std::path::Path) -> PathBuf {
        if let Some(ref display) = self.display_cwd {
            let real = std::path::Path::new(&self.real_cwd);
            if let Ok(suffix) = path.strip_prefix(real) {
                return PathBuf::from(display).join(suffix);
            }
        }
        path.to_path_buf()
    }

    /// Rewrite paths in a list of hunks for display.
    fn rewrite_hunks(&self, hunks: Vec<Arc<Hunk>>) -> Vec<Arc<Hunk>> {
        if self.display_cwd.is_none() {
            return hunks;
        }
        hunks
            .into_iter()
            .map(|h| {
                let new_path = self.display_path(&h.path);
                if new_path == h.path {
                    return h;
                }
                // Clone the hunk with the rewritten path
                Arc::new(Hunk {
                    path: new_path,
                    id: h.id.clone(),
                    line_info: h.line_info.clone(),
                    source: h.source,
                    old_text: h.old_text.clone(),
                    new_text: h.new_text.clone(),
                    patch: h.patch.clone(),
                    created_at: h.created_at,
                    selected: h.selected,
                })
            })
            .collect()
    }
}

/// Get the hunk tracker context for the given session.
fn get_hunk_tracker(
    agent: &MvpAgent,
    session_id: Option<&acp::SessionId>,
) -> Result<HunkTrackerContext, acp::Error> {
    let session_id = session_id.ok_or_else(|| {
        acp::Error::invalid_params().data("sessionId is required for hunk tracker operations")
    })?;

    let handle = agent.get_session_handle(session_id).ok_or_else(|| {
        acp::Error::invalid_params().data(format!("session not found: {}", session_id.0))
    })?;

    Ok(HunkTrackerContext {
        handle: handle.tool_context.hunk_tracker_handle.clone(),
        real_cwd: handle.info.cwd.clone(),
        display_cwd: handle.display_cwd.clone(),
    })
}

/// Compute file summaries from hunks.
///
/// `staged_paths` contains the absolute paths of files staged in the git index.
fn compute_file_summaries(
    hunks: &[Arc<Hunk>],
    staged_paths: &HashSet<PathBuf>,
) -> Vec<FileSummary> {
    use std::collections::HashMap;

    let mut file_map: HashMap<PathBuf, FileSummary> = HashMap::new();

    for hunk in hunks {
        let entry = file_map
            .entry(hunk.path.clone())
            .or_insert_with(|| FileSummary {
                path: hunk.path.clone(),
                is_agent_file: false,
                staged: staged_paths.contains(&hunk.path),
                hunk_count: 0,
                additions: 0,
                deletions: 0,
            });

        entry.hunk_count += 1;
        entry.additions += hunk.new_text.lines().count();
        entry.deletions += hunk
            .old_text
            .as_ref()
            .map(|t| t.lines().count())
            .unwrap_or(0);

        // Mark as agent file if any hunk is from agent
        if hunk.source.is_agent_edit() {
            entry.is_agent_file = true;
        }
    }

    let mut files: Vec<_> = file_map.into_values().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

// ═══════════════════════════════════════════════════════════════════════
// Main Handler
// ═══════════════════════════════════════════════════════════════════════

pub async fn handle(
    agent: &MvpAgent,
    ops: &echo_agent_workspace::WorkspaceOps,
    args: &acp::ExtRequest,
) -> ExtResult {
    match args.method.as_ref() {
        // ───────────────────────────────────────────────────────────────
        // Queries
        // ───────────────────────────────────────────────────────────────
        "echo.agent/hunk-tracker/get-hunks" => {
            let req = parse_params::<GetHunksRequest>(args)?;
            let ctx = get_hunk_tracker(agent, req.session_id.as_ref())?;

            // If path is specified, use get_file_hunk_data to get hunks + content together
            let (hunks, baseline, current, baseline_content, current_content) =
                if let Some(path) = req.path {
                    let data = ctx.handle.get_file_hunk_data(PathBuf::from(path)).await;
                    (
                        data.hunks,
                        Some(data.baseline),
                        Some(data.current),
                        data.baseline_content,
                        data.current_content,
                    )
                } else {
                    (ctx.handle.get_all_hunks().await, None, None, None, None)
                };

            // Filter by source if specified
            let hunks = match req.source.as_deref() {
                Some("agent") => hunks
                    .into_iter()
                    .filter(|h| h.source.is_agent_edit())
                    .collect(),
                Some("external") => hunks
                    .into_iter()
                    .filter(|h| h.source.is_external())
                    .collect(),
                _ => hunks, // "all" or unspecified
            };

            // Rewrite worktree paths to display paths for client UI
            let hunks = ctx.rewrite_hunks(hunks);

            to_ext_response(Ok(GetHunksResponse {
                hunks,
                baseline,
                current,
                baseline_content,
                current_content,
            }))
        }

        "echo.agent/hunk-tracker/get-files" => {
            let req = parse_params::<GetFilesRequest>(args)?;
            let ctx = get_hunk_tracker(agent, req.session_id.as_ref())?;

            let hunks = ctx.handle.get_all_hunks().await;
            let staged_paths = ctx.handle.get_staged_files().await;
            // Rewrite paths before computing summaries so file paths are stable
            let hunks = ctx.rewrite_hunks(hunks);
            // Rewrite staged paths for display (worktree → display path)
            let staged_paths: HashSet<PathBuf> =
                staged_paths.iter().map(|p| ctx.display_path(p)).collect();
            let files = compute_file_summaries(&hunks, &staged_paths);

            to_ext_response(Ok(GetFilesResponse { files }))
        }

        "echo.agent/hunk-tracker/get-all-file-contents" => {
            let req = parse_params::<GetFilesRequest>(args)?;

            let sid = req.session_id.as_ref().map(|s| s.0.as_ref());
            // The RPC returns the lean wire type; bridge it back to the
            // hunk-tracker type so the ACP response shape is unchanged.
            let mut files: Vec<FileContentEntry> = ops
                .dispatch(&HunkGetAllFileContentsReq {}, sid)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?
                .into_iter()
                .map(file_content_entry_from_wire)
                .collect();
            // Post-dispatch: rewrite worktree paths to display paths for client UI
            if let Some(ctx) = req
                .session_id
                .as_ref()
                .and_then(|s| get_hunk_tracker(agent, Some(s)).ok())
                && ctx.display_cwd.is_some()
            {
                for entry in &mut files {
                    entry.path = ctx.display_path(&entry.path);
                }
            }
            to_ext_response(Ok(GetAllFileContentsResponse { files }))
        }

        "echo.agent/hunk-tracker/get-summary" => {
            let req = parse_params::<GetSummaryRequest>(args)?;

            let sid = req.session_id.as_ref().map(|s| s.0.as_ref());
            let result = ops
                .dispatch(&HunkGetSessionSummaryReq {}, sid)
                .await
                .map_err(|e| acp::Error::internal_error().data(e.to_string()))?;
            to_ext_response(Ok(result))
        }

        // ───────────────────────────────────────────────────────────────
        // Single Hunk Action
        // ───────────────────────────────────────────────────────────────
        "echo.agent/hunk-tracker/hunk-action" => {
            let req = parse_params::<HunkActionRequest>(args)?;

            let action_kind = match req.action.as_str() {
                "accept" => HunkActionKind::Accept,
                "reject" => HunkActionKind::Reject,
                other => {
                    return Err(
                        acp::Error::invalid_params().data(format!("unknown action: {other}"))
                    );
                }
            };
            let op = HunkSingleActionReq {
                action: HunkActionReq {
                    hunk_id: req.hunk_id.clone(),
                    action: action_kind,
                },
            };
            let sid = req.session_id.as_ref().map(|s| s.0.as_ref());
            match ops.dispatch(&op, sid).await {
                Ok(_) => to_ext_response(Ok(ActionResponse {
                    success: true,
                    error: None,
                    affected_count: Some(1),
                })),
                Err(e) => to_ext_response(Ok(ActionResponse {
                    success: false,
                    error: Some(e.to_string()),
                    affected_count: None,
                })),
            }
        }

        // ───────────────────────────────────────────────────────────────
        // Bulk Actions
        // ───────────────────────────────────────────────────────────────
        "echo.agent/hunk-tracker/file-action" => {
            let req = parse_params::<FileActionRequest>(args)?;

            let action_kind = match req.action.as_str() {
                "accept" => HunkActionKind::Accept,
                "reject" => HunkActionKind::Reject,
                other => {
                    return Err(
                        acp::Error::invalid_params().data(format!("unknown action: {other}"))
                    );
                }
            };
            let op = HunkFileActionReq {
                path: req.path.clone(),
                action: action_kind,
            };
            let sid = req.session_id.as_ref().map(|s| s.0.as_ref());
            match ops.dispatch(&op, sid).await {
                Ok(resp) => to_ext_response(Ok(ActionResponse {
                    success: true,
                    error: None,
                    affected_count: Some(resp.affected.len()),
                })),
                Err(e) => to_ext_response(Ok(ActionResponse {
                    success: false,
                    error: Some(e.to_string()),
                    affected_count: None,
                })),
            }
        }

        "echo.agent/hunk-tracker/turn-action" => {
            let req = parse_params::<TurnActionRequest>(args)?;

            let action_kind = match req.action.as_str() {
                "accept" => HunkActionKind::Accept,
                "reject" => HunkActionKind::Reject,
                other => {
                    return Err(
                        acp::Error::invalid_params().data(format!("unknown action: {other}"))
                    );
                }
            };
            let op = HunkTurnActionReq {
                prompt_index: req.prompt_index,
                action: action_kind,
            };
            let sid = req.session_id.as_ref().map(|s| s.0.as_ref());
            match ops.dispatch(&op, sid).await {
                Ok(resp) => to_ext_response(Ok(ActionResponse {
                    success: true,
                    error: None,
                    affected_count: Some(resp.affected.len()),
                })),
                Err(e) => to_ext_response(Ok(ActionResponse {
                    success: false,
                    error: Some(e.to_string()),
                    affected_count: None,
                })),
            }
        }

        "echo.agent/hunk-tracker/all-action" => {
            let req = parse_params::<AllActionRequest>(args)?;

            let action_kind = match req.action.as_str() {
                "accept" => HunkActionKind::Accept,
                "reject" => HunkActionKind::Reject,
                other => {
                    return Err(
                        acp::Error::invalid_params().data(format!("unknown action: {other}"))
                    );
                }
            };
            let op = HunkAllActionReq {
                action: action_kind,
            };
            let sid = req.session_id.as_ref().map(|s| s.0.as_ref());
            match ops.dispatch(&op, sid).await {
                Ok(resp) => to_ext_response(Ok(ActionResponse {
                    success: true,
                    error: None,
                    affected_count: Some(resp.affected.len()),
                })),
                Err(e) => to_ext_response(Ok(ActionResponse {
                    success: false,
                    error: Some(e.to_string()),
                    affected_count: None,
                })),
            }
        }

        _ => Err(acp::Error::method_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::{GetAllFileContentsResponse, GetHunksResponse, compute_file_summaries};
    use chrono::Utc;
    use echo_agent_hunk_tracker::{
        FileContentStatus, FileContentView, Hunk, HunkId, HunkLineInfo, HunkSource,
    };
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn make_hunk(
        id: &str,
        path: &str,
        source: HunkSource,
        old_text: Option<&str>,
        new_text: &str,
    ) -> Arc<Hunk> {
        let old_count = old_text.map(|text| text.lines().count()).unwrap_or(0);
        let new_count = new_text.lines().count();
        Arc::new(Hunk {
            id: HunkId::from_string(id.to_string()),
            path: PathBuf::from(path),
            line_info: HunkLineInfo {
                old_start: 1,
                old_count,
                new_start: 1,
                new_count,
            },
            source,
            old_text: old_text.map(|text| text.to_string()),
            new_text: new_text.to_string(),
            patch: None,
            created_at: Utc::now(),
            selected: false,
        })
    }

    #[test]
    fn compute_file_summaries_aggregates_counts_and_sort_order() {
        let hunks = vec![
            make_hunk(
                "hunk-a1",
                "/repo/a.txt",
                HunkSource::AgentEdit { prompt_index: 2 },
                Some("old1\nold2"),
                "new1\nnew2\nnew3",
            ),
            make_hunk(
                "hunk-a2",
                "/repo/a.txt",
                HunkSource::ExternalEditOnAgentFile,
                None,
                "added",
            ),
            make_hunk(
                "hunk-b1",
                "/repo/b.txt",
                HunkSource::External,
                Some("gone"),
                "",
            ),
        ];

        let no_staged = HashSet::new();
        let files = compute_file_summaries(&hunks, &no_staged);

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, PathBuf::from("/repo/a.txt"));
        assert_eq!(files[1].path, PathBuf::from("/repo/b.txt"));

        assert_eq!(files[0].hunk_count, 2);
        assert_eq!(files[0].additions, 4);
        assert_eq!(files[0].deletions, 2);
        assert!(files[0].is_agent_file);

        assert_eq!(files[1].hunk_count, 1);
        assert_eq!(files[1].additions, 0);
        assert_eq!(files[1].deletions, 1);
        assert!(!files[1].is_agent_file);
    }

    #[test]
    fn compute_file_summaries_returns_empty_for_no_hunks() {
        let no_staged = HashSet::new();
        let files = compute_file_summaries(&[], &no_staged);

        assert!(files.is_empty());
    }

    #[test]
    fn compute_file_summaries_sets_agent_flag_only_for_agent_edits() {
        let hunks = vec![
            make_hunk(
                "hunk-external",
                "/repo/a.txt",
                HunkSource::ExternalEditOnAgentFile,
                Some("before"),
                "after",
            ),
            make_hunk(
                "hunk-external-2",
                "/repo/a.txt",
                HunkSource::External,
                Some("before2"),
                "after2",
            ),
        ];

        let no_staged = HashSet::new();
        let files = compute_file_summaries(&hunks, &no_staged);

        assert_eq!(files.len(), 1);
        assert!(!files[0].is_agent_file);
    }

    #[test]
    fn compute_file_summaries_counts_deletions_when_new_text_empty() {
        let hunks = vec![make_hunk(
            "hunk-delete",
            "/repo/deleted.txt",
            HunkSource::AgentEdit { prompt_index: 0 },
            Some("line1\nline2\nline3"),
            "",
        )];

        let no_staged = HashSet::new();
        let files = compute_file_summaries(&hunks, &no_staged);

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].additions, 0);
        assert_eq!(files[0].deletions, 3);
    }

    #[test]
    fn compute_file_summaries_sets_staged_flag_from_staged_paths() {
        let hunks = vec![
            make_hunk(
                "hunk-a1",
                "/repo/a.txt",
                HunkSource::AgentEdit { prompt_index: 0 },
                Some("old"),
                "new",
            ),
            make_hunk(
                "hunk-b1",
                "/repo/b.txt",
                HunkSource::External,
                Some("old"),
                "new",
            ),
        ];

        let staged = HashSet::from([PathBuf::from("/repo/a.txt")]);
        let files = compute_file_summaries(&hunks, &staged);

        assert_eq!(files.len(), 2);
        // a.txt is staged
        assert!(files[0].staged);
        // b.txt is not staged
        assert!(!files[1].staged);
    }

    // =========================================================================
    // GetHunksResponse Serialization Tests
    // =========================================================================
    // These tests verify that the ACP get-hunks response correctly serializes
    // the new explicit status fields (baseline, current) alongside legacy fields.

    /// GetHunksResponse serializes Full status with all fields
    #[test]
    fn get_hunks_response_serializes_full_status() {
        let baseline_text = "baseline content\n";
        let current_text = "current content\n";

        let response = GetHunksResponse {
            hunks: vec![],
            baseline: Some(FileContentView::full(baseline_text.to_string())),
            current: Some(FileContentView::full(current_text.to_string())),
            baseline_content: Some(baseline_text.to_string()),
            current_content: Some(current_text.to_string()),
        };

        let json = serde_json::to_value(&response).unwrap();

        // Verify baseline view fields
        let baseline = json.get("baseline").expect("baseline should be present");
        assert_eq!(baseline.get("status").unwrap().as_str().unwrap(), "full");
        assert_eq!(
            baseline.get("byteLen").unwrap().as_u64().unwrap() as usize,
            baseline_text.len()
        );
        assert_eq!(
            baseline.get("content").unwrap().as_str().unwrap(),
            baseline_text
        );

        // Verify current view fields
        let current = json.get("current").expect("current should be present");
        assert_eq!(current.get("status").unwrap().as_str().unwrap(), "full");
        assert_eq!(
            current.get("content").unwrap().as_str().unwrap(),
            current_text
        );

        // Verify legacy fields
        assert_eq!(
            json.get("baselineContent").unwrap().as_str().unwrap(),
            baseline_text
        );
        assert_eq!(
            json.get("currentContent").unwrap().as_str().unwrap(),
            current_text
        );
    }

    /// GetHunksResponse serializes Missing status
    #[test]
    fn get_hunks_response_serializes_missing_status() {
        let response = GetHunksResponse {
            hunks: vec![],
            baseline: Some(FileContentView::missing()),
            current: Some(FileContentView::full("new file\n".to_string())),
            baseline_content: None,
            current_content: Some("new file\n".to_string()),
        };

        let json = serde_json::to_value(&response).unwrap();

        // Verify baseline is Missing (no content or byteLen)
        let baseline = json.get("baseline").expect("baseline should be present");
        assert_eq!(baseline.get("status").unwrap().as_str().unwrap(), "missing");
        assert!(baseline.get("content").is_none());
        assert!(baseline.get("byteLen").is_none());

        // Legacy baseline_content should be absent (skipped when None)
        assert!(json.get("baselineContent").is_none());
    }

    /// GetHunksResponse serializes Binary status with byte_len
    #[test]
    fn get_hunks_response_serializes_binary_status() {
        let response = GetHunksResponse {
            hunks: vec![],
            baseline: Some(FileContentView::binary(Some(1024))),
            current: Some(FileContentView::binary(Some(2048))),
            baseline_content: None,
            current_content: None,
        };

        let json = serde_json::to_value(&response).unwrap();

        // Verify baseline is Binary
        let baseline = json.get("baseline").expect("baseline should be present");
        assert_eq!(baseline.get("status").unwrap().as_str().unwrap(), "binary");
        assert_eq!(baseline.get("byteLen").unwrap().as_u64().unwrap(), 1024);
        assert!(baseline.get("content").is_none());

        // Verify current is Binary
        let current = json.get("current").expect("current should be present");
        assert_eq!(current.get("status").unwrap().as_str().unwrap(), "binary");
        assert_eq!(current.get("byteLen").unwrap().as_u64().unwrap(), 2048);
    }

    /// GetHunksResponse serializes TooLarge status with byte_len
    #[test]
    fn get_hunks_response_serializes_too_large_status() {
        let response = GetHunksResponse {
            hunks: vec![],
            baseline: Some(FileContentView::too_large(5_000_000)),
            current: Some(FileContentView::too_large(10_000_000)),
            baseline_content: None,
            current_content: None,
        };

        let json = serde_json::to_value(&response).unwrap();

        // Verify baseline is TooLarge
        let baseline = json.get("baseline").expect("baseline should be present");
        assert_eq!(
            baseline.get("status").unwrap().as_str().unwrap(),
            "tooLarge"
        );
        assert_eq!(
            baseline.get("byteLen").unwrap().as_u64().unwrap(),
            5_000_000
        );
        assert!(baseline.get("content").is_none());

        // Verify current is TooLarge
        let current = json.get("current").expect("current should be present");
        assert_eq!(current.get("status").unwrap().as_str().unwrap(), "tooLarge");
        assert_eq!(
            current.get("byteLen").unwrap().as_u64().unwrap(),
            10_000_000
        );
    }

    /// GetHunksResponse omits baseline/current when None (get-all-hunks case)
    #[test]
    fn get_hunks_response_omits_none_fields() {
        let response = GetHunksResponse {
            hunks: vec![make_hunk(
                "hunk-1",
                "/repo/file.txt",
                HunkSource::AgentEdit { prompt_index: 0 },
                Some("old"),
                "new",
            )],
            baseline: None,
            current: None,
            baseline_content: None,
            current_content: None,
        };

        let json = serde_json::to_value(&response).unwrap();

        // baseline, current, baselineContent, currentContent should all be absent
        assert!(json.get("baseline").is_none());
        assert!(json.get("current").is_none());
        assert!(json.get("baselineContent").is_none());
        assert!(json.get("currentContent").is_none());

        // hunks should still be present
        assert!(json.get("hunks").is_some());
        assert_eq!(json.get("hunks").unwrap().as_array().unwrap().len(), 1);
    }

    /// FileContentView default is Missing status
    #[test]
    fn file_content_view_default_is_missing() {
        let view = FileContentView::default();

        assert_eq!(view.status, FileContentStatus::Missing);
        assert!(view.byte_len.is_none());
        assert!(view.content.is_none());
    }

    // =========================================================================
    // GetAllFileContentsResponse Serialization Tests
    // =========================================================================

    /// GetAllFileContentsResponse serializes with all fields using camelCase
    #[test]
    fn get_all_file_contents_response_serializes_correctly() {
        use echo_agent_hunk_tracker::FileContentEntry;

        let response = GetAllFileContentsResponse {
            files: vec![FileContentEntry {
                path: PathBuf::from("/repo/foo.txt"),
                baseline: FileContentView::full("old content\n".to_string()),
                current: FileContentView::full("new content\n".to_string()),
                is_agent_file: true,
                staged: false,
            }],
        };

        let json = serde_json::to_value(&response).unwrap();
        let files = json.get("files").unwrap().as_array().unwrap();
        assert_eq!(files.len(), 1);

        let f = &files[0];
        assert_eq!(f.get("path").unwrap().as_str().unwrap(), "/repo/foo.txt");
        assert!(f.get("isAgentFile").unwrap().as_bool().unwrap());
        assert!(!f.get("staged").unwrap().as_bool().unwrap());

        // Baseline
        let baseline = f.get("baseline").unwrap();
        assert_eq!(baseline.get("status").unwrap().as_str().unwrap(), "full");
        assert_eq!(
            baseline.get("content").unwrap().as_str().unwrap(),
            "old content\n"
        );

        // Current
        let current = f.get("current").unwrap();
        assert_eq!(current.get("status").unwrap().as_str().unwrap(), "full");
        assert_eq!(
            current.get("content").unwrap().as_str().unwrap(),
            "new content\n"
        );
    }

    /// GetAllFileContentsResponse handles missing baseline (new file)
    #[test]
    fn get_all_file_contents_response_missing_baseline() {
        use echo_agent_hunk_tracker::FileContentEntry;

        let response = GetAllFileContentsResponse {
            files: vec![FileContentEntry {
                path: PathBuf::from("/repo/new.txt"),
                baseline: FileContentView::missing(),
                current: FileContentView::full("brand new\n".to_string()),
                is_agent_file: true,
                staged: true,
            }],
        };

        let json = serde_json::to_value(&response).unwrap();
        let f = &json.get("files").unwrap().as_array().unwrap()[0];

        assert!(f.get("staged").unwrap().as_bool().unwrap());

        let baseline = f.get("baseline").unwrap();
        assert_eq!(baseline.get("status").unwrap().as_str().unwrap(), "missing");
        assert!(baseline.get("content").is_none());
        assert!(baseline.get("byteLen").is_none());
    }

    /// GetAllFileContentsResponse handles binary files
    #[test]
    fn get_all_file_contents_response_binary_file() {
        use echo_agent_hunk_tracker::FileContentEntry;

        let response = GetAllFileContentsResponse {
            files: vec![FileContentEntry {
                path: PathBuf::from("/repo/image.png"),
                baseline: FileContentView::binary(Some(1024)),
                current: FileContentView::binary(Some(2048)),
                is_agent_file: false,
                staged: false,
            }],
        };

        let json = serde_json::to_value(&response).unwrap();
        let f = &json.get("files").unwrap().as_array().unwrap()[0];

        let baseline = f.get("baseline").unwrap();
        assert_eq!(baseline.get("status").unwrap().as_str().unwrap(), "binary");
        assert_eq!(baseline.get("byteLen").unwrap().as_u64().unwrap(), 1024);
        assert!(baseline.get("content").is_none());

        let current = f.get("current").unwrap();
        assert_eq!(current.get("status").unwrap().as_str().unwrap(), "binary");
        assert_eq!(current.get("byteLen").unwrap().as_u64().unwrap(), 2048);
    }

    /// GetAllFileContentsResponse returns empty files array when no tracked files
    #[test]
    fn get_all_file_contents_response_empty() {
        let response = GetAllFileContentsResponse { files: vec![] };

        let json = serde_json::to_value(&response).unwrap();
        let files = json.get("files").unwrap().as_array().unwrap();
        assert!(files.is_empty());
    }

    /// GetAllFileContentsResponse with multiple files preserves all entries
    #[test]
    fn get_all_file_contents_response_multiple_files() {
        use echo_agent_hunk_tracker::FileContentEntry;

        let response = GetAllFileContentsResponse {
            files: vec![
                FileContentEntry {
                    path: PathBuf::from("/repo/agent.rs"),
                    baseline: FileContentView::full("fn main() {}\n".to_string()),
                    current: FileContentView::full("fn main() { run(); }\n".to_string()),
                    is_agent_file: true,
                    staged: true,
                },
                FileContentEntry {
                    path: PathBuf::from("/repo/readme.md"),
                    baseline: FileContentView::full("# Title\n".to_string()),
                    current: FileContentView::full("# New Title\n".to_string()),
                    is_agent_file: false,
                    staged: false,
                },
            ],
        };

        let json = serde_json::to_value(&response).unwrap();
        let files = json.get("files").unwrap().as_array().unwrap();
        assert_eq!(files.len(), 2);

        // First file: agent, staged
        assert!(files[0].get("isAgentFile").unwrap().as_bool().unwrap());
        assert!(files[0].get("staged").unwrap().as_bool().unwrap());

        // Second file: not agent, not staged
        assert!(!files[1].get("isAgentFile").unwrap().as_bool().unwrap());
        assert!(!files[1].get("staged").unwrap().as_bool().unwrap());
    }
}
