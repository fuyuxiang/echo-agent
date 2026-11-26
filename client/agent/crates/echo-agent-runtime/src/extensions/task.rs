use agent_client_protocol as acp;
use echo_agent_tools::types::{KillOutcome, KillSource, TaskSnapshot};
use serde::{Deserialize, Serialize};

use echo_agent_tools::implementations::echo_agent_build::task::types::{
    SubagentCancelOutcome, SubagentInspection, SubagentProvenance, SubagentSnapshot,
    SubagentSnapshotStatus,
};

use crate::agent::MvpAgent;
use crate::session::ExtMethodResult;

type ExtResult = Result<acp::ExtResponse, acp::Error>;

/// Wire DTO for the `echo.agent/task/kill` ext request.
///
/// `pub` (with both serde directions) so ACP clients (echo-agent-pager) build
/// the request from the same type the agent parses — keeping the wire
/// contract typed end-to-end instead of duplicated `json!` literals.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KillTaskRequest {
    pub session_id: String,
    pub task_id: String,
    /// Single-task UI `[×]` omits this (defaults to [`TaskKillSource::ClientUi`]).
    /// Bulk teardown (dashboard stop-all, session delete, headless reap)
    /// must send [`TaskKillSource::Teardown`].
    #[serde(default)]
    pub source: TaskKillSource,
}

/// Client-facing kill reason on `echo.agent/task/kill`. Older clients omit it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskKillSource {
    #[default]
    ClientUi,
    Teardown,
}

impl From<TaskKillSource> for KillSource {
    fn from(source: TaskKillSource) -> Self {
        match source {
            TaskKillSource::ClientUi => Self::ClientUi,
            TaskKillSource::Teardown => Self::Teardown,
        }
    }
}

/// Wire DTO for the `echo.agent/task/kill` ext response payload (nested under
/// `result` in the `ExtMethodResult` envelope).
///
/// `pub` (with both serde directions) so ACP clients deserialize the typed
/// outcome instead of probing raw JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KillTaskResponse {
    pub task_id: String,
    pub outcome: KillOutcome,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListTasksRequest {
    session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListTasksResponse {
    tasks: Vec<TaskSnapshot>,
}

/// Wire DTO for the `echo.agent/subagent/cancel` ext request.
///
/// `pub` (with both serde directions) so ACP clients (echo-agent-pager) build
/// the request from the same type the agent parses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelSubagentRequest {
    pub subagent_id: String,
}

/// Wire mirror of the coordinator's [`SubagentCancelOutcome`], `kind`-tagged so
/// a client can branch and read the already-finished `status`. Sent alongside
/// the legacy `cancelled` bool: a new pager prefers this, an old one ignores it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubagentCancelOutcomeDto {
    /// A live subagent was cancelled — a real `SubagentFinished` is coming.
    Cancelled,
    /// Already finished — no finish coming; `status` is the real terminal status.
    AlreadyFinished { status: String },
    /// The id is unknown (never existed / evicted) — no finish coming.
    NotFound,
    /// Unknown future `kind` (`#[serde(other)]`): lets an old client still parse
    /// and fall back to the legacy bool. Never produced by `From`.
    #[serde(other)]
    Unknown,
}

impl SubagentCancelOutcomeDto {
    /// Legacy bool for older pagers: true only when a live subagent was stopped.
    /// Already-finished / not-found → false so an old pager finalizes the row.
    fn cancelled_bool(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl From<SubagentCancelOutcome> for SubagentCancelOutcomeDto {
    fn from(outcome: SubagentCancelOutcome) -> Self {
        match outcome {
            SubagentCancelOutcome::Cancelled => Self::Cancelled,
            SubagentCancelOutcome::AlreadyFinished { status } => Self::AlreadyFinished { status },
            SubagentCancelOutcome::NotFound => Self::NotFound,
        }
    }
}

/// Wire DTO for the `echo.agent/subagent/cancel` response payload (under `result` in
/// the `ExtMethodResult` envelope). `pub` + both serde dirs so clients read it typed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelSubagentResponse {
    pub subagent_id: String,
    /// Legacy wire-compat flag for older pagers; new clients prefer `outcome`.
    pub cancelled: bool,
    /// Typed outcome; `None` only from an older shell. This shell always sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<SubagentCancelOutcomeDto>,
}

// ── Subagent list_running DTOs ────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRunningSubagentsRequest {
    session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListRunningSubagentsResponse {
    subagents: Vec<SubagentLiveSnapshotDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubagentLiveSnapshotDto {
    subagent_id: String,
    parent_session_id: String,
    child_session_id: String,
    subagent_type: String,
    description: String,
    started_at_epoch_ms: u64,
    duration_ms: u64,
    turn_count: u32,
    tool_call_count: u32,
    tokens_used: u64,
    context_window_tokens: u64,
    context_usage_pct: u8,
    tools_used: Vec<String>,
    error_count: u32,
}

impl From<SubagentInspection> for SubagentLiveSnapshotDto {
    fn from(inspection: SubagentInspection) -> Self {
        let SubagentInspection {
            snapshot,
            parent_session_id,
            child_session_id,
            ..
        } = inspection;
        let SubagentSnapshotStatus::Running {
            turn_count,
            tool_call_count,
            tokens_used,
            context_window_tokens,
            context_usage_pct,
            tools_used,
            error_count,
        } = snapshot.status
        else {
            unreachable!("list_running returns only active children");
        };
        Self {
            subagent_id: snapshot.subagent_id,
            parent_session_id,
            child_session_id,
            subagent_type: snapshot.subagent_type,
            description: snapshot.description,
            started_at_epoch_ms: snapshot.started_at_epoch_ms,
            duration_ms: snapshot.duration_ms,
            turn_count,
            tool_call_count,
            tokens_used,
            context_window_tokens,
            context_usage_pct,
            tools_used,
            error_count,
        }
    }
}

// ── Subagent get DTOs ────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetSubagentRequest {
    subagent_id: String,
    #[serde(default)]
    block: Option<bool>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GetSubagentResponse {
    snapshot: Option<SubagentSnapshotDto>,
}

/// ACP DTO for a single subagent snapshot (any status).
///
/// Extends the identity fields from `SubagentLiveSnapshotDto` with
/// status-dependent fields for completed/failed/cancelled states.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubagentSnapshotDto {
    subagent_id: String,
    parent_session_id: String,
    child_session_id: String,
    subagent_type: String,
    description: String,
    started_at_epoch_ms: u64,
    duration_ms: u64,
    status: String,
    // ── Running fields (present only when status == "running") ────
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_used: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_window_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_usage_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools_used: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_count: Option<u32>,
    // ── Completed fields ─────────────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree_path: Option<String>,
    // ── Failed / Cancelled fields ────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cancel_reason: Option<String>,
    // ── Fork/resume provenance ─────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none")]
    fork_context_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fork_parent_prompt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resumed_from: Option<String>,
}

impl SubagentSnapshotDto {
    /// Build a DTO from a resolved snapshot and its session identity.
    fn from_snapshot(
        snap: SubagentSnapshot,
        parent_session_id: String,
        child_session_id: String,
        provenance: SubagentProvenance,
    ) -> Self {
        let mut dto = SubagentSnapshotDto {
            subagent_id: snap.subagent_id,
            parent_session_id,
            child_session_id,
            subagent_type: snap.subagent_type,
            description: snap.description,
            started_at_epoch_ms: snap.started_at_epoch_ms,
            duration_ms: snap.duration_ms,
            status: String::new(),
            turn_count: None,
            tool_call_count: None,
            tokens_used: None,
            context_window_tokens: None,
            context_usage_pct: None,
            tools_used: None,
            error_count: None,
            output: None,
            tool_calls: None,
            turns: None,
            worktree_path: None,
            failure_error: None,
            cancel_reason: None,
            fork_context_source: None,
            fork_parent_prompt_id: provenance.fork_parent_prompt_id,
            resumed_from: provenance.resumed_from,
        };
        match snap.status {
            SubagentSnapshotStatus::Initializing => {
                dto.status = "initializing".into();
            }
            SubagentSnapshotStatus::Running {
                turn_count,
                tool_call_count,
                tokens_used,
                context_window_tokens,
                context_usage_pct,
                tools_used,
                error_count,
            } => {
                dto.status = "running".into();
                dto.turn_count = Some(turn_count);
                dto.tool_call_count = Some(tool_call_count);
                dto.tokens_used = Some(tokens_used);
                dto.context_window_tokens = Some(context_window_tokens);
                dto.context_usage_pct = Some(context_usage_pct);
                dto.tools_used = Some(tools_used);
                dto.error_count = Some(error_count);
            }
            SubagentSnapshotStatus::Completed {
                output,
                tool_calls,
                turns,
                worktree_path,
            } => {
                dto.status = "completed".into();
                dto.output = Some(output);
                dto.tool_calls = Some(tool_calls);
                dto.turns = Some(turns);
                dto.worktree_path = worktree_path;
            }
            SubagentSnapshotStatus::Failed { error } => {
                dto.status = "failed".into();
                dto.failure_error = Some(error);
            }
            SubagentSnapshotStatus::Cancelled { reason } => {
                dto.status = "cancelled".into();
                dto.cancel_reason = reason;
            }
        }
        dto
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn parse<T: serde::de::DeserializeOwned>(args: &acp::ExtRequest) -> Result<T, acp::Error> {
    serde_json::from_str(args.params.get())
        .map_err(|e| acp::Error::invalid_params().data(format!("invalid params: {e}")))
}

fn respond<T: Serialize>(result: Result<T, impl std::fmt::Display>) -> ExtResult {
    ExtMethodResult::from_result(result)
        .to_ext_response()
        .map_err(|e| acp::Error::internal_error().data(e.to_string()))
}

pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/task/kill" => {
            let req: KillTaskRequest = parse(args)?;
            let result = agent
                .kill_background_task(&req.session_id, &req.task_id, req.source.into())
                .await
                .map(|outcome| KillTaskResponse {
                    task_id: req.task_id,
                    outcome,
                });
            respond(result)
        }
        "echo.agent/task/list" => {
            let req: ListTasksRequest = parse(args)?;
            let result = agent
                .list_tasks(&req.session_id)
                .await
                .ok_or_else(|| "session not found or no terminal backend".to_string())
                .map(|tasks| ListTasksResponse { tasks });
            respond(result)
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

// ── Scheduler DTOs ────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteScheduledTaskRequest {
    session_id: String,
    task_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteScheduledTaskResponse {
    task_id: String,
    deleted: bool,
}

/// Handle `echo.agent/scheduler/*` extension methods.
pub(crate) async fn handle_scheduler(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/scheduler/delete" => {
            let req: DeleteScheduledTaskRequest = parse(args)?;
            let result = agent
                .delete_scheduled_task(&req.session_id, &req.task_id)
                .await
                .map(|deleted| DeleteScheduledTaskResponse {
                    task_id: req.task_id,
                    deleted,
                });
            respond(result)
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

/// Handle `echo.agent/subagent/*` extension methods.
pub(crate) async fn handle_subagent(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/subagent/cancel" => {
            let req: CancelSubagentRequest = parse(args)?;
            tracing::info!(subagent_id = %req.subagent_id, "Cancelling subagent via ext method");
            let outcome =
                SubagentCancelOutcomeDto::from(agent.cancel_subagent(&req.subagent_id).await);
            respond(Ok::<_, String>(CancelSubagentResponse {
                subagent_id: req.subagent_id,
                cancelled: outcome.cancelled_bool(),
                outcome: Some(outcome),
            }))
        }
        "echo.agent/subagent/get" => {
            let req: GetSubagentRequest = parse(args)?;
            let block = req.block.unwrap_or(false);
            let timeout_ms = req.timeout_ms.unwrap_or(30_000);

            let snapshot = agent
                .query_subagent(&req.subagent_id, block, Some(timeout_ms))
                .await;
            let inspection = agent.inspect_subagent(&req.subagent_id).await;
            let (parent_session_id, child_session_id, provenance) = inspection
                .map(|inspection| {
                    (
                        inspection.parent_session_id,
                        inspection.child_session_id,
                        SubagentProvenance {
                            fork_parent_prompt_id: inspection.fork_parent_prompt_id,
                            resumed_from: inspection.resumed_from,
                        },
                    )
                })
                .unwrap_or_default();
            respond(Ok::<_, String>(GetSubagentResponse {
                snapshot: snapshot.map(|snapshot| {
                    SubagentSnapshotDto::from_snapshot(
                        snapshot,
                        parent_session_id,
                        child_session_id,
                        provenance,
                    )
                }),
            }))
        }
        "echo.agent/subagent/list_running" => {
            let req: ListRunningSubagentsRequest = parse(args)?;
            let subagents = agent
                .list_running_subagents(&req.session_id)
                .await
                .into_iter()
                .map(SubagentLiveSnapshotDto::from)
                .collect();
            respond(Ok::<_, String>(ListRunningSubagentsResponse { subagents }))
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_scheduled_task_request_deserializes_camel_case() {
        let json = r#"{"sessionId":"sess-1","taskId":"task-42"}"#;
        let req: DeleteScheduledTaskRequest = serde_json::from_str(json).expect("should parse");
        assert_eq!(req.session_id, "sess-1");
        assert_eq!(req.task_id, "task-42");
    }

    #[test]
    fn kill_task_request_source_defaults_to_client_ui() {
        let req: KillTaskRequest =
            serde_json::from_str(r#"{"sessionId":"s","taskId":"t"}"#).expect("legacy kill");
        assert_eq!(req.source, TaskKillSource::ClientUi);
        let teardown: KillTaskRequest =
            serde_json::from_str(r#"{"sessionId":"s","taskId":"t","source":"teardown"}"#)
                .expect("teardown kill");
        assert_eq!(teardown.source, TaskKillSource::Teardown);
    }

    #[test]
    fn delete_scheduled_task_response_serializes_camel_case() {
        let resp = DeleteScheduledTaskResponse {
            task_id: "task-42".into(),
            deleted: true,
        };
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["taskId"], "task-42");
        assert_eq!(json["deleted"], true);
    }

    #[test]
    fn subagent_live_snapshot_dto_serializes_camel_case() {
        let dto = SubagentLiveSnapshotDto {
            subagent_id: "sub-1".into(),
            parent_session_id: "parent-1".into(),
            child_session_id: "child-1".into(),
            subagent_type: "explore".into(),
            description: "find files".into(),
            started_at_epoch_ms: 1_700_000_000_000,
            duration_ms: 5000,
            turn_count: 2,
            tool_call_count: 7,
            tokens_used: 30_000,
            context_window_tokens: 256_000,
            context_usage_pct: 23,
            tools_used: vec!["bash".into(), "grep".into()],
            error_count: 1,
        };
        let json = serde_json::to_value(&dto).expect("should serialize");
        assert_eq!(json["subagentId"], "sub-1");
        assert_eq!(json["parentSessionId"], "parent-1");
        assert_eq!(json["childSessionId"], "child-1");
        assert_eq!(json["subagentType"], "explore");
        assert_eq!(json["startedAtEpochMs"], 1_700_000_000_000_u64);
        assert_eq!(json["durationMs"], 5000);
        assert_eq!(json["turnCount"], 2);
        assert_eq!(json["toolCallCount"], 7);
        assert_eq!(json["tokensUsed"], 30_000);
        assert_eq!(json["contextWindowTokens"], 256_000);
        assert_eq!(json["contextUsagePct"], 23);
        assert_eq!(json["toolsUsed"], serde_json::json!(["bash", "grep"]));
        assert_eq!(json["errorCount"], 1);
    }

    #[test]
    fn from_resolved_running_subagent_maps_all_fields() {
        let resolved = SubagentInspection {
            snapshot: SubagentSnapshot {
                subagent_id: "s".into(),
                subagent_type: "plan".into(),
                description: "d".into(),
                started_at_epoch_ms: 100,
                duration_ms: 200,
                persona: None,
                status: SubagentSnapshotStatus::Running {
                    turn_count: 1,
                    tool_call_count: 3,
                    tokens_used: 500,
                    context_window_tokens: 1000,
                    context_usage_pct: 50,
                    tools_used: vec!["read_file".into()],
                    error_count: 0,
                },
            },
            parent_session_id: "p".into(),
            child_session_id: "c".into(),
            fork_parent_prompt_id: None,
            resumed_from: None,
        };
        let dto = SubagentLiveSnapshotDto::from(resolved);
        assert_eq!(dto.subagent_id, "s");
        assert_eq!(dto.parent_session_id, "p");
        assert_eq!(dto.child_session_id, "c");
        assert_eq!(dto.context_usage_pct, 50);
        assert_eq!(dto.tools_used, vec!["read_file"]);
    }

    #[test]
    fn list_running_response_serializes_with_subagents_array() {
        let resp = ListRunningSubagentsResponse { subagents: vec![] };
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["subagents"], serde_json::json!([]));
    }

    // ── SubagentSnapshotDto serialization tests ────────────────────────

    #[test]
    fn snapshot_dto_running_serializes_with_progress_fields() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-1".into(),
            subagent_type: "explore".into(),
            description: "find files".into(),
            started_at_epoch_ms: 1000,
            duration_ms: 5000,
            persona: None,
            status: SubagentSnapshotStatus::Running {
                turn_count: 3,
                tool_call_count: 12,
                tokens_used: 45_000,
                context_window_tokens: 256_000,
                context_usage_pct: 35,
                tools_used: vec!["bash".into(), "grep".into()],
                error_count: 1,
            },
        };
        let dto = SubagentSnapshotDto::from_snapshot(
            snap,
            "parent-1".into(),
            "child-1".into(),
            Default::default(),
        );
        let json = serde_json::to_value(&dto).expect("should serialize");
        assert_eq!(json["parentSessionId"], "parent-1");
        assert_eq!(json["childSessionId"], "child-1");
        assert_eq!(json["status"], "running");
        assert_eq!(json["turnCount"], 3);
        assert_eq!(json["toolCallCount"], 12);
        assert_eq!(json["tokensUsed"], 45_000);
        assert_eq!(json["contextUsagePct"], 35);
        assert_eq!(json["errorCount"], 1);
        // Completed-only fields should be absent
        assert!(json.get("output").is_none());
        assert!(json.get("failureError").is_none());
    }

    #[test]
    fn snapshot_dto_completed_serializes_with_output() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-2".into(),
            subagent_type: "general-purpose".into(),
            description: "refactor auth".into(),
            started_at_epoch_ms: 2000,
            duration_ms: 15_000,
            persona: None,
            status: SubagentSnapshotStatus::Completed {
                output: "Done, refactored 3 files.".into(),
                tool_calls: 8,
                turns: 2,
                worktree_path: None,
            },
        };
        let dto =
            SubagentSnapshotDto::from_snapshot(snap, "p".into(), "c".into(), Default::default());
        let json = serde_json::to_value(&dto).expect("should serialize");
        assert_eq!(json["status"], "completed");
        assert_eq!(json["output"], "Done, refactored 3 files.");
        assert_eq!(json["toolCalls"], 8);
        assert_eq!(json["turns"], 2);
        // Running-only fields should be absent
        assert!(json.get("turnCount").is_none());
        assert!(json.get("tokensUsed").is_none());
    }

    #[test]
    fn snapshot_dto_failed_serializes_with_error() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-3".into(),
            subagent_type: "plan".into(),
            description: "plan feature".into(),
            started_at_epoch_ms: 0,
            duration_ms: 100,
            persona: None,
            status: SubagentSnapshotStatus::Failed {
                error: "sampling error".into(),
            },
        };
        let dto =
            SubagentSnapshotDto::from_snapshot(snap, "p".into(), "c".into(), Default::default());
        let json = serde_json::to_value(&dto).expect("should serialize");
        assert_eq!(json["status"], "failed");
        assert_eq!(json["failureError"], "sampling error");
    }

    #[test]
    fn snapshot_dto_cancelled_serializes_with_reason() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-4".into(),
            subagent_type: "explore".into(),
            description: "search".into(),
            started_at_epoch_ms: 0,
            duration_ms: 50,
            persona: None,
            status: SubagentSnapshotStatus::Cancelled {
                reason: Some("user cancelled".into()),
            },
        };
        let dto =
            SubagentSnapshotDto::from_snapshot(snap, "p".into(), "c".into(), Default::default());
        let json = serde_json::to_value(&dto).expect("should serialize");
        assert_eq!(json["status"], "cancelled");
        assert_eq!(json["cancelReason"], "user cancelled");
    }

    #[test]
    fn snapshot_dto_cancelled_without_reason_omits_field() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-5".into(),
            subagent_type: "explore".into(),
            description: "search".into(),
            started_at_epoch_ms: 0,
            duration_ms: 50,
            persona: None,
            status: SubagentSnapshotStatus::Cancelled { reason: None },
        };
        let dto =
            SubagentSnapshotDto::from_snapshot(snap, "p".into(), "c".into(), Default::default());
        let json = serde_json::to_value(&dto).expect("should serialize");
        assert_eq!(json["status"], "cancelled");
        assert!(json.get("cancelReason").is_none());
    }

    #[test]
    fn get_subagent_response_null_snapshot() {
        let resp = GetSubagentResponse { snapshot: None };
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert!(json["snapshot"].is_null());
    }

    #[test]
    fn get_subagent_response_with_running_snapshot() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-run".into(),
            subagent_type: "explore".into(),
            description: "search".into(),
            started_at_epoch_ms: 1000,
            duration_ms: 5000,
            persona: None,
            status: SubagentSnapshotStatus::Running {
                turn_count: 2,
                tool_call_count: 5,
                tokens_used: 20_000,
                context_window_tokens: 256_000,
                context_usage_pct: 15,
                tools_used: vec!["bash".into()],
                error_count: 0,
            },
        };
        let resp = GetSubagentResponse {
            snapshot: Some(SubagentSnapshotDto::from_snapshot(
                snap,
                "parent-1".into(),
                "child-1".into(),
                Default::default(),
            )),
        };
        let json = serde_json::to_value(&resp).expect("should serialize");
        let s = &json["snapshot"];
        assert_eq!(s["status"], "running");
        assert_eq!(s["subagentId"], "sub-run");
        assert_eq!(s["parentSessionId"], "parent-1");
        assert_eq!(s["childSessionId"], "child-1");
        assert_eq!(s["turnCount"], 2);
        // Completed-only fields must be absent
        assert!(s.get("output").is_none());
        assert!(s.get("turns").is_none());
    }

    #[test]
    fn get_subagent_response_with_completed_snapshot() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-done".into(),
            subagent_type: "general-purpose".into(),
            description: "refactor".into(),
            started_at_epoch_ms: 0,
            duration_ms: 10_000,
            persona: None,
            status: SubagentSnapshotStatus::Completed {
                output: "Refactored 3 files.".into(),
                tool_calls: 7,
                turns: 2,
                worktree_path: None,
            },
        };
        let resp = GetSubagentResponse {
            snapshot: Some(SubagentSnapshotDto::from_snapshot(
                snap,
                "parent-2".into(),
                "child-2".into(),
                Default::default(),
            )),
        };
        let json = serde_json::to_value(&resp).expect("should serialize");
        let s = &json["snapshot"];
        assert_eq!(s["status"], "completed");
        assert_eq!(s["output"], "Refactored 3 files.");
        assert_eq!(s["toolCalls"], 7);
        assert_eq!(s["turns"], 2);
        // Running-only fields must be absent
        assert!(s.get("turnCount").is_none());
        assert!(s.get("tokensUsed").is_none());
    }

    #[test]
    fn get_subagent_request_deserializes_block_false() {
        let json = r#"{"subagentId":"sub-1","block":false}"#;
        let req: GetSubagentRequest = serde_json::from_str(json).expect("should parse");
        assert_eq!(req.subagent_id, "sub-1");
        assert_eq!(req.block, Some(false));
        assert!(req.timeout_ms.is_none());
    }

    #[test]
    fn get_subagent_request_deserializes_block_true_with_timeout() {
        let json = r#"{"subagentId":"sub-2","block":true,"timeoutMs":5000}"#;
        let req: GetSubagentRequest = serde_json::from_str(json).expect("should parse");
        assert_eq!(req.subagent_id, "sub-2");
        assert_eq!(req.block, Some(true));
        assert_eq!(req.timeout_ms, Some(5000));
    }

    #[test]
    fn get_subagent_request_defaults_block_and_timeout() {
        let json = r#"{"subagentId":"sub-3"}"#;
        let req: GetSubagentRequest = serde_json::from_str(json).expect("should parse");
        assert!(req.block.is_none());
        assert!(req.timeout_ms.is_none());
    }

    #[test]
    fn snapshot_dto_resumed_provenance_serializes() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-resumed".into(),
            subagent_type: "general-purpose".into(),
            description: "fix review".into(),
            started_at_epoch_ms: 2000,
            duration_ms: 3000,
            persona: None,
            status: SubagentSnapshotStatus::Running {
                turn_count: 1,
                tool_call_count: 2,
                tokens_used: 10_000,
                context_window_tokens: 256_000,
                context_usage_pct: 8,
                tools_used: vec![],
                error_count: 0,
            },
        };
        let provenance = SubagentProvenance {
            fork_parent_prompt_id: Some("prompt-5".into()),
            resumed_from: Some("source-agent-id".into()),
        };
        let dto = SubagentSnapshotDto::from_snapshot(
            snap,
            "parent".into(),
            "child-resumed".into(),
            provenance,
        );
        let json = serde_json::to_value(&dto).expect("should serialize");
        assert_eq!(json["resumedFrom"], "source-agent-id");
        assert_eq!(json["forkParentPromptId"], "prompt-5");
    }

    // ── echo.agent/subagent/cancel outcome wire DTO ──────────────────────────

    #[test]
    fn subagent_cancel_outcome_dto_maps_from_coordinator_outcome() {
        // Cancelled → legacy bool true (a real finish is coming).
        let dto = SubagentCancelOutcomeDto::from(SubagentCancelOutcome::Cancelled);
        assert_eq!(dto, SubagentCancelOutcomeDto::Cancelled);
        assert!(dto.cancelled_bool());

        // AlreadyFinished carries the terminal status; legacy bool false.
        let dto = SubagentCancelOutcomeDto::from(SubagentCancelOutcome::AlreadyFinished {
            status: "completed".into(),
        });
        assert_eq!(
            dto,
            SubagentCancelOutcomeDto::AlreadyFinished {
                status: "completed".into()
            }
        );
        assert!(!dto.cancelled_bool());

        // NotFound → legacy bool false.
        let dto = SubagentCancelOutcomeDto::from(SubagentCancelOutcome::NotFound);
        assert_eq!(dto, SubagentCancelOutcomeDto::NotFound);
        assert!(!dto.cancelled_bool());
    }

    #[test]
    fn cancel_subagent_response_serializes_outcome_snake_case() {
        let resp = CancelSubagentResponse {
            subagent_id: "sa-1".into(),
            cancelled: false,
            outcome: Some(SubagentCancelOutcomeDto::AlreadyFinished {
                status: "failed".into(),
            }),
        };
        let json = serde_json::to_value(&resp).expect("should serialize");
        assert_eq!(json["subagentId"], "sa-1");
        assert_eq!(json["cancelled"], false);
        assert_eq!(json["outcome"]["kind"], "already_finished");
        assert_eq!(json["outcome"]["status"], "failed");
    }

    /// Wire-compat: a payload from an older shell (no `outcome`) still
    /// deserializes, leaving `outcome` as `None` so the client falls back to
    /// the legacy `cancelled` bool.
    #[test]
    fn cancel_subagent_response_deserializes_without_outcome() {
        let resp: CancelSubagentResponse =
            serde_json::from_str(r#"{"subagentId":"sa-1","cancelled":true}"#).expect("parse");
        assert_eq!(resp.subagent_id, "sa-1");
        assert!(resp.cancelled);
        assert!(resp.outcome.is_none());
    }
}
