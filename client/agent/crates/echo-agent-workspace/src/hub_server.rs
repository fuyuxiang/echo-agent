//! Workspace-side RPC handler for server-proxied workspace method calls.
//!
//! [`WorkspaceRpcHandler`] implements [`ToolServerHandler`] and dispatches
//! `workspace.*` JSON-RPC methods to [`WorkspaceHandle`]. Registered on
//! the `ToolServer` with tool_id `workspace_rpc`.
use crate::error::{WorkspaceError, WorkspaceResult};
use crate::handle::WorkspaceHandle;
use crate::hub_ids::WORKSPACE_RPC_TOOL_ID;
use crate::rpc_envelope::{RpcEnvelope, envelope_err};
use crate::workspace_ops::{RpcActivityClass, WorkspaceOp, WorkspaceRpc};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use echo_agent_computer_hub_sdk::ToolServerHandler;
use echo_agent_tool_protocol::{HookEvent, HookFrame, SessionId, ToolId, ToolServerEvictParams};
use echo_agent_tool_runtime::{
    ToolCallContext, ToolError, ToolErrorKind, ToolStream, TypedToolOutput, terminal_only,
};
use echo_agent_tool_types::ToolDescription;
use echo_agent_tools::computer::types::KillOutcome;
use echo_agent_tools::computer::types::TaskKind;
use echo_agent_tools::implementations::echo_agent_build::scheduler::interval::interval_to_human;
use echo_agent_tools::implementations::echo_agent_build::scheduler::types::{
    SchedulerCommand, SchedulerHandle,
};
use echo_agent_tools::registry::types::FinalizedToolset;
use echo_agent_tools::types::resources::Terminal;
use echo_agent_workspace_types::rpc::workspace::{
    BackgroundTaskSnapshotWire, KillTaskOutcome, ScheduledTaskSnapshotWire, TasksSnapshotResponse,
};
use prometheus::{HistogramVec, IntCounterVec, register_histogram_vec, register_int_counter_vec};
use serde_json::Value;
/// Deprecation monitor for the self-attested `caller_session_id` param:
/// `kind="param_mismatch"` — the param disagreed with the server-bound envelope
/// session (envelope trusted); `kind="envelope_absent"` — no envelope
/// session, the param was used as a compat fallback. Enforcement
/// (envelope-only identity) waits for this to be flat zero.
static WORKSPACE_RPC_CALLER_MISMATCH_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_rpc_caller_mismatch_total",
            "Mutation RPCs whose caller_session_id param was not backed by a matching \
             server-bound envelope session, by method and kind",
            &["method", "kind"]
        )
        .unwrap()
    });
/// Audit trail for the deliberate-mutation RPC surface
/// (`update_tool_config` / `drop_session` / `configure_mcp`).
static WORKSPACE_RPC_MUTATION_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_rpc_mutation_total",
            "Session-mutating workspace RPC calls, by method and outcome",
            &["method", "outcome"]
        )
        .unwrap()
    });
/// Every dispatched `workspace.*` RPC, by method and result. Unrecognized
/// methods collapse to the `unknown` label to keep cardinality bounded.
static WORKSPACE_RPC_REQUESTS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_rpc_requests_total",
            "Workspace RPC dispatches, by method and result. Donated per-sandbox \
             series inflate absolute volume — SLOs must use ratios \
             (error/total), not increase() counts.",
            &["method", "result"]
        )
        .unwrap()
    });
/// Failed `workspace.*` RPC dispatches, by method and
/// [`WorkspaceError::metric_kind`].
static WORKSPACE_RPC_ERRORS_TOTAL: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        register_int_counter_vec!(
            "echo_agent_workspace_rpc_errors_total",
            "Failed workspace RPC dispatches, by method and error kind. \
             Donated per-sandbox series inflate absolute volume — compare \
             error_kind shares or error/total ratios, not raw counts.",
            &["method", "error_kind"]
        )
        .unwrap()
    });
/// Per-method wall-clock duration of a `workspace.*` RPC dispatch.
static WORKSPACE_RPC_DURATION_SECONDS: std::sync::LazyLock<HistogramVec> =
    std::sync::LazyLock::new(|| {
        register_histogram_vec!(
            "echo_agent_workspace_rpc_duration_seconds",
            "Workspace RPC dispatch duration",
            &["method"],
            vec![
                0.001, 0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0
            ]
        )
        .unwrap()
    });
const UNKNOWN_METHOD_LABEL: &str = "unknown";
/// Zero-init this module's metric families. See [`crate::init_metrics`].
pub(crate) fn init_metrics() {
    WORKSPACE_RPC_REQUESTS_TOTAL
        .with_label_values(&[UNKNOWN_METHOD_LABEL, "error"])
        .inc_by(0);
    WORKSPACE_RPC_ERRORS_TOTAL
        .with_label_values(&[UNKNOWN_METHOD_LABEL, "unknown_method"])
        .inc_by(0);
    let _ = WORKSPACE_RPC_DURATION_SECONDS.with_label_values(&[UNKNOWN_METHOD_LABEL]);
}
/// Resolve the caller identity for a mutation RPC: the server-bound envelope
/// session is authoritative; the deprecated `caller_session_id` param is
/// only used when no envelope session exists (old call paths). Both
/// divergences are counted on [`WORKSPACE_RPC_CALLER_MISMATCH_TOTAL`].
fn resolve_mutation_caller<'a>(
    method: &'static str,
    bound_session: Option<&'a str>,
    param_caller: Option<&'a str>,
) -> WorkspaceResult<&'a str> {
    match (bound_session, param_caller) {
        (Some(envelope), Some(param)) => {
            if envelope != param {
                WORKSPACE_RPC_CALLER_MISMATCH_TOTAL
                    .with_label_values(&[method, "param_mismatch"])
                    .inc();
                tracing::warn!(
                    method,
                    envelope_session = %envelope,
                    param_caller = %param,
                    "caller_session_id param disagrees with the server-bound envelope session; \
                     trusting the envelope"
                );
            }
            Ok(envelope)
        }
        (Some(envelope), None) => Ok(envelope),
        (None, Some(param)) => {
            WORKSPACE_RPC_CALLER_MISMATCH_TOTAL
                .with_label_values(&[method, "envelope_absent"])
                .inc();
            Ok(param)
        }
        (None, None) => Err(WorkspaceError::HubError(format!(
            "{method}: missing caller identity (no bound session and no caller_session_id)"
        ))),
    }
}
/// Audit-log and count a mutation RPC on [`WORKSPACE_RPC_MUTATION_TOTAL`].
/// Failures log at WARN because that arm carries rejected cross-session
/// forgeries (`Unauthorized`), the audit trail's most interesting event.
fn record_mutation_rpc<T>(
    method: &'static str,
    caller: &str,
    target: &str,
    result: &WorkspaceResult<T>,
) {
    let outcome = match result {
        Ok(_) => "ok",
        Err(_) => "error",
    };
    WORKSPACE_RPC_MUTATION_TOTAL
        .with_label_values(&[method, outcome])
        .inc();
    match result {
        Ok(_) => tracing::info!(method, caller, target, "workspace mutation rpc"),
        Err(e) => {
            tracing::warn!(method, caller, target, error = %e, "workspace mutation rpc failed");
        }
    }
}
/// No-op notifier for RPC-driven worktree creation.
struct NoOpNotifier;
#[async_trait]
impl crate::worktree::WorktreeNotificationSender for NoOpNotifier {
    async fn send_worktree_status(&self, _progress: crate::worktree::WorktreeStatus) {}
}
/// Env escape hatch for the client-facing `workspace.client_fs_*` ops.
///
/// Default **on**; setting `WORKSPACE_CLIENT_FS_QUERIES=0` (or `false`)
/// disables the ops with a graceful `HubError` that the remote caller
/// maps to a fallback. Read per call — flipping the variable needs no
/// process restart and tests can toggle it under a lock.
fn client_fs_queries_enabled() -> bool {
    !matches!(
        std::env::var("WORKSPACE_CLIENT_FS_QUERIES").as_deref(),
        Ok("0") | Ok("false")
    )
}
/// Reject `workspace.client_fs_*` dispatch when the escape hatch is off.
fn ensure_client_fs_queries_enabled() -> WorkspaceResult<()> {
    if client_fs_queries_enabled() {
        Ok(())
    } else {
        Err(WorkspaceError::HubError(
            "client fs queries disabled on this workspace".into(),
        ))
    }
}
/// Stamp client-RPC activity for mutation-classed methods. Called before
/// param validation so a malformed call from a live client still counts.
fn note_mutation<Op: WorkspaceRpc>(ws: &WorkspaceHandle) {
    if Op::ACTIVITY == RpcActivityClass::Mutation {
        ws.activity_tracker().note_client_rpc_activity();
    }
}
/// Generic dispatch helper: deserialize params, execute, serialize result.
async fn dispatch_op<Op: WorkspaceOp>(
    params: Value,
    ws: &WorkspaceHandle,
    session_id: Option<&str>,
) -> WorkspaceResult<Value> {
    note_mutation::<Op>(ws);
    let req: Op = serde_json::from_value(params)
        .map_err(|e| WorkspaceError::HubError(format!("invalid params for {}: {e}", Op::METHOD)))?;
    let result = req.execute(ws, session_id).await?;
    serde_json::to_value(result)
        .map_err(|e| WorkspaceError::HubError(format!("{}: {e}", Op::METHOD)))
}
/// List a session's outstanding (not-completed) background terminal tasks from
/// the session toolset's `TerminalBackend` resource, mapped to the slim wire
/// DTO. Empty when the session has no terminal backend. Source of truth for the
/// `workspace.list_background_tasks` RPC (post-compaction system-reminder state).
async fn list_outstanding_background_tasks(
    toolset: &echo_agent_tools::registry::types::FinalizedToolset,
) -> Vec<echo_agent_workspace_types::rpc::workspace::BackgroundTaskSummaryWire> {
    use echo_agent_tools::computer::types::TaskKind;
    use echo_agent_tools::types::resources::Terminal;
    use echo_agent_tools::types::tool::ToolKind;
    use echo_agent_workspace_types::rpc::workspace::BackgroundTaskSummaryWire;
    let terminal = {
        let res = toolset.resources.lock().await;
        res.get::<Terminal>().map(|t| t.0.clone())
    };
    let Some(terminal) = terminal else {
        return Vec::new();
    };
    let execute_name = toolset.tool_name_for_kind(ToolKind::Execute);
    let monitor_name = toolset.tool_name_for_kind(ToolKind::Monitor);
    terminal
        .list_tasks()
        .await
        .into_iter()
        .filter(|t| !t.completed)
        .map(|t| {
            let command = t
                .display_command
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or(t.command);
            let tool_name = match t.kind {
                TaskKind::Monitor => monitor_name.clone(),
                TaskKind::Bash => execute_name.clone(),
            };
            BackgroundTaskSummaryWire {
                task_id: t.task_id,
                command,
                tool_name,
            }
        })
        .collect()
}
/// Kill a background terminal task via the session toolset's `TerminalBackend`.
/// Returns `NotFound` when the session has no terminal backend.
async fn kill_background_task(toolset: &FinalizedToolset, task_id: &str) -> KillTaskOutcome {
    let terminal = {
        let res = toolset.resources.lock().await;
        res.get::<Terminal>().map(|t| t.0.clone())
    };
    let Some(terminal) = terminal else {
        return KillTaskOutcome::NotFound;
    };
    match terminal.kill_task(task_id).await {
        KillOutcome::Killed => KillTaskOutcome::Killed,
        KillOutcome::AlreadyExited => KillTaskOutcome::AlreadyExited,
        KillOutcome::NotFound => KillTaskOutcome::NotFound,
    }
}
/// Delete a scheduled (loop) task via the session toolset's scheduler actor.
/// `Ok(false)` strictly means no such task; scheduler refusals propagate as errors so a client never treats a still-firing task as gone.
async fn delete_scheduled_task(
    toolset: &FinalizedToolset,
    task_id: &str,
) -> Result<bool, WorkspaceError> {
    let scheduler = {
        let res = toolset.resources.lock().await;
        res.get::<SchedulerHandle>().cloned()
    };
    let Some(handle) = scheduler else {
        return Ok(false);
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if handle
        .0
        .send(SchedulerCommand::Delete {
            id: task_id.to_owned(),
            reply: reply_tx,
        })
        .is_err()
    {
        return Err(WorkspaceError::HubError("scheduler actor stopped".into()));
    }
    match reply_rx.await {
        Ok(Ok(deleted)) => Ok(deleted),
        Ok(Err(e)) => Err(WorkspaceError::HubError(e.to_string())),
        Err(_) => Err(WorkspaceError::HubError("scheduler actor stopped".into())),
    }
}
/// Incomplete backgrounded terminal tasks + live scheduled tasks (client tray rebuild).
async fn tasks_snapshot(toolset: &FinalizedToolset) -> TasksSnapshotResponse {
    let (terminal, scheduler) = {
        let res = toolset.resources.lock().await;
        (
            res.get::<Terminal>().map(|t| t.0.clone()),
            res.get::<SchedulerHandle>().cloned(),
        )
    };
    let background_tasks = match terminal {
        Some(terminal) => terminal
            .list_tasks()
            .await
            .into_iter()
            .filter(|t| t.is_outstanding_background())
            .map(|t| {
                let command = t
                    .display_command
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(t.command);
                BackgroundTaskSnapshotWire {
                    task_id: t.task_id,
                    command,
                    kind: match t.kind {
                        TaskKind::Bash => "bash".to_owned(),
                        TaskKind::Monitor => "monitor".to_owned(),
                    },
                    started_at: DateTime::<Utc>::from(t.start_time).to_rfc3339(),
                    description: t.description,
                }
            })
            .collect(),
        None => Vec::new(),
    };
    let scheduled_tasks = match scheduler {
        Some(handle) => {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            let _ = handle.0.send(SchedulerCommand::List { reply: reply_tx });
            reply_rx
                .await
                .map(|snapshot| snapshot.tasks)
                .unwrap_or_default()
                .into_iter()
                .map(|t| ScheduledTaskSnapshotWire {
                    task_id: t.id.clone(),
                    prompt: t.prompt.clone(),
                    human_schedule: interval_to_human(t.interval_secs),
                    next_fire_at: t.next_fire_at().to_rfc3339(),
                    recurring: t.recurring,
                    created_at: t.created_at.to_rfc3339(),
                })
                .collect()
        }
        None => Vec::new(),
    };
    TasksSnapshotResponse {
        background_tasks,
        scheduled_tasks,
    }
}
/// List the session's TODO items (via `todo_write`) from the session toolset's
/// `State<TodoState>` resource, mapped to the slim wire DTO. Empty when the
/// session has no todo state. Source of truth for the `workspace.list_todos`
/// RPC (post-compaction system-reminder state).
async fn list_session_todos(
    toolset: &echo_agent_tools::registry::types::FinalizedToolset,
) -> Vec<echo_agent_workspace_types::rpc::workspace::TodoSummaryWire> {
    use echo_agent_tools::implementations::echo_agent_build::todo::{TodoState, TodoStatus};
    use echo_agent_tools::types::resources::State;
    use echo_agent_workspace_types::rpc::workspace::TodoSummaryWire;
    let res = toolset.resources.lock().await;
    let Some(state) = res.get::<State<TodoState>>() else {
        return Vec::new();
    };
    state
        .0
        .todo_items_with_ids()
        .map(|(id, item)| {
            let status = match item.status {
                TodoStatus::Pending => "pending",
                TodoStatus::InProgress => "in_progress",
                TodoStatus::Completed => "completed",
                TodoStatus::Cancelled => "cancelled",
            };
            TodoSummaryWire {
                id: id.to_string(),
                content: item.content.clone(),
                status: status.to_string(),
            }
        })
        .collect()
}
/// Routes JSON-RPC `workspace.*` method calls to [`WorkspaceHandle`].
pub(crate) struct WorkspaceRpcHandler {
    workspace: WorkspaceHandle,
}
impl WorkspaceRpcHandler {
    pub(crate) fn new(workspace: WorkspaceHandle) -> Self {
        Self { workspace }
    }
    /// Route a `workspace.*` method; `bound_session` is the caller's server-bound session.
    async fn dispatch(
        &self,
        method: &str,
        params: Value,
        bound_session: Option<&str>,
    ) -> WorkspaceResult<Value> {
        use crate::file_system::ContentSearchRequest;
        use crate::file_system::{
            FsDeleteFileReq, FsExistsReq, FsListReq, FsReadFileReq, FsWriteFileReq,
        };
        use crate::session::checkpoint::TurnBoundary;
        use crate::workspace_ops::*;
        use crate::worktree::{ApplyWorktreeRequest, CreateWorktreeRequest, RemoveWorktreeRequest};
        use echo_agent_workspace_types::rpc::git::{GitBranchInfoReq, GitMetadataReq};
        use echo_agent_workspace_types::rpc::presence::PresenceNoteReq;
        use echo_agent_workspace_types::rpc::search::FuzzyStatusReq;
        use echo_agent_workspace_types::rpc::skills::DiscoverPluginsReq;
        use echo_agent_workspace_types::rpc::workspace::{
            ConfigureMcpReq, DeleteScheduledTaskReq, DeleteScheduledTaskResponse, DropSessionReq,
            InstallPluginReq, KillTaskReq, KillTaskResponse, ListBackgroundTasksReq,
            ListBackgroundTasksResponse, ListTodosReq, ListTodosResponse, LoadEnvrcReq,
            LoadPermissionsReq, LoadProjectConfigReq, RefreshPluginsReq, ResolveFileReferencesReq,
            TasksSnapshotReq, ToolDefinitionsReq, UpdateToolConfigReq, WorkspaceInfo,
        };
        use echo_agent_workspace_types::rpc::worktree::WorktreeCreateSyncReq;
        tracing::debug!(method, "workspace rpc dispatch");
        let params = if params.is_null() {
            serde_json::json!({})
        } else {
            params
        };
        match method {
            <WorkspaceInfoReq as WorkspaceRpc>::METHOD => {
                let cwd = self.workspace.root_cwd()?;
                let shell = std::env::var("SHELL")
                    .ok()
                    .and_then(|s| {
                        std::path::Path::new(&s)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                    })
                    .unwrap_or_else(|| "sh".to_string());
                let info = WorkspaceInfo {
                    os: std::env::consts::OS.to_owned(),
                    shell,
                    cwd: cwd.to_string_lossy().into_owned(),
                    version: Some(echo_agent_version::VERSION.to_owned()),
                };
                serde_json::to_value(info).map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            <GitStatusReq as WorkspaceRpc>::METHOD => {
                static DEPRECATION_WARNING: std::sync::Once = std::sync::Once::new();
                DEPRECATION_WARNING.call_once(|| {
                    tracing::warn!(
                        "workspace.git_status is deprecated and will be removed in a future \
                         release. Use workspace.git_status_ext with format: \"prompt\" instead."
                    );
                });
                let cwd = self.workspace.root_cwd()?;
                let result = crate::file_system::git_status(cwd)
                    .await
                    .map_err(|e| WorkspaceError::HubError(e.to_string()))?;
                Ok(Value::String(result))
            }
            <GitBranchInfoReq as WorkspaceRpc>::METHOD => {
                let cwd = self.workspace.root_cwd()?;
                match crate::session::git::git_info(&cwd).await {
                    Ok(info) => serde_json::to_value(info)
                        .map_err(|e| WorkspaceError::HubError(e.to_string())),
                    Err(_) => Ok(Value::Null),
                }
            }
            <ToolDefinitionsReq as WorkspaceRpc>::METHOD => {
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing session_id".into()))?;
                let session = self
                    .workspace
                    .session(session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(session_id.into()))?;
                let defs = session.toolset().tool_definitions();
                serde_json::to_value(defs).map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            <ListBackgroundTasksReq as WorkspaceRpc>::METHOD => {
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing session_id".into()))?;
                let session = self
                    .workspace
                    .session(session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(session_id.into()))?;
                let toolset = session.toolset();
                let tasks = list_outstanding_background_tasks(toolset.as_ref()).await;
                serde_json::to_value(ListBackgroundTasksResponse { tasks })
                    .map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            <TasksSnapshotReq as WorkspaceRpc>::METHOD => {
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing session_id".into()))?;
                let session = self
                    .workspace
                    .session(session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(session_id.into()))?;
                let toolset = session.toolset();
                let snapshot = tasks_snapshot(toolset.as_ref()).await;
                serde_json::to_value(snapshot).map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            <KillTaskReq as WorkspaceRpc>::METHOD => {
                note_mutation::<KillTaskReq>(&self.workspace);
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing session_id".into()))?;
                let task_id = params
                    .get("task_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing task_id".into()))?
                    .to_owned();
                let session = self
                    .workspace
                    .session(session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(session_id.into()))?;
                let toolset = session.toolset();
                let outcome = kill_background_task(toolset.as_ref(), &task_id).await;
                serde_json::to_value(KillTaskResponse { task_id, outcome })
                    .map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            <DeleteScheduledTaskReq as WorkspaceRpc>::METHOD => {
                note_mutation::<DeleteScheduledTaskReq>(&self.workspace);
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing session_id".into()))?;
                let task_id = params
                    .get("task_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing task_id".into()))?
                    .to_owned();
                let session = self
                    .workspace
                    .session(session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(session_id.into()))?;
                let toolset = session.toolset();
                let deleted = delete_scheduled_task(toolset.as_ref(), &task_id).await?;
                serde_json::to_value(DeleteScheduledTaskResponse { task_id, deleted })
                    .map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            <ListTodosReq as WorkspaceRpc>::METHOD => {
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing session_id".into()))?;
                let session = self
                    .workspace
                    .session(session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(session_id.into()))?;
                let toolset = session.toolset();
                let todos = list_session_todos(toolset.as_ref()).await;
                serde_json::to_value(ListTodosResponse { todos })
                    .map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            <UpdateToolConfigReq as WorkspaceRpc>::METHOD => {
                note_mutation::<UpdateToolConfigReq>(&self.workspace);
                let caller = resolve_mutation_caller(
                    "update_tool_config",
                    bound_session,
                    params
                        .get("caller_session_id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty()),
                )?;
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing session_id".into()))?
                    .to_owned();
                let new_config = serde_json::from_value(
                    params
                        .get("new_config")
                        .cloned()
                        .ok_or_else(|| WorkspaceError::HubError("missing new_config".into()))?,
                )
                .map_err(|e| WorkspaceError::HubError(format!("invalid new_config: {e}")))?;
                let result = self
                    .workspace
                    .update_tool_config(caller, &session_id, new_config)
                    .await;
                record_mutation_rpc("update_tool_config", caller, &session_id, &result);
                result.map(|()| Value::Null)
            }
            <DropSessionReq as WorkspaceRpc>::METHOD => {
                let caller = resolve_mutation_caller(
                    "drop_session",
                    bound_session,
                    params
                        .get("caller_session_id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty()),
                )?;
                let target = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing session_id".into()))?;
                let result = self.workspace.drop_session(caller, target);
                record_mutation_rpc("drop_session", caller, target, &result);
                result.map(|()| Value::Null)
            }
            <ResolveFileReferencesReq as WorkspaceRpc>::METHOD => {
                let refs: Vec<String> = params
                    .get("refs")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let cwd = self.workspace.client_fs_base(bound_session).await?.base;
                let mut results = Vec::new();
                for ref_path in &refs {
                    let requested_path = if std::path::Path::new(ref_path).is_absolute() {
                        std::path::PathBuf::from(ref_path)
                    } else {
                        cwd.join(ref_path)
                    };
                    let full_path =
                        match self.workspace.confine_to_root(&requested_path, &cwd).await {
                            Ok((confined, _)) => confined,
                            Err(e) => {
                                results.push(serde_json::json!({
                                    "path": requested_path.to_string_lossy(),
                                    "ref": ref_path,
                                    "exists": false,
                                    "content": Value::Null,
                                    "error": e.to_string(),
                                }));
                                continue;
                            }
                        };
                    let exists = full_path.exists();
                    let content = if exists {
                        tokio::fs::read_to_string(&full_path).await.ok()
                    } else {
                        None
                    };
                    results.push(serde_json::json!({
                        "path": full_path.to_string_lossy(),
                        "ref": ref_path,
                        "exists": exists,
                        "content": content,
                    }));
                }
                Ok(Value::Array(results))
            }
            <PutFilesReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<PutFilesReq>(params, &self.workspace, bound_session).await
            }
            <GetFilesReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GetFilesReq>(params, &self.workspace, bound_session).await
            }
            <FsListReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<FsListReq>(params, &self.workspace, None).await
            }
            <FsExistsReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<FsExistsReq>(params, &self.workspace, None).await
            }
            <FsReadFileReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<FsReadFileReq>(params, &self.workspace, None).await
            }
            <FsWriteFileReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<FsWriteFileReq>(params, &self.workspace, None).await
            }
            <FsDeleteFileReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<FsDeleteFileReq>(params, &self.workspace, None).await
            }
            <ClientFsListReq as WorkspaceRpc>::METHOD => {
                ensure_client_fs_queries_enabled()?;
                dispatch_op::<ClientFsListReq>(params, &self.workspace, bound_session).await
            }
            <ClientFsStatReq as WorkspaceRpc>::METHOD => {
                ensure_client_fs_queries_enabled()?;
                dispatch_op::<ClientFsStatReq>(params, &self.workspace, bound_session).await
            }
            <ClientFsReadFileReq as WorkspaceRpc>::METHOD => {
                ensure_client_fs_queries_enabled()?;
                dispatch_op::<ClientFsReadFileReq>(params, &self.workspace, bound_session).await
            }
            <DiscoverSkillsReq as WorkspaceRpc>::METHOD => {
                let cwd = self.workspace.root_cwd()?;
                let skills =
                    crate::discovery::discover_skills(&cwd, self.workspace.shared.skills_config())
                        .await;
                Ok(Value::Array(skills))
            }
            <DiscoverAgentsMdReq as WorkspaceRpc>::METHOD => {
                let cwd = self.workspace.root_cwd()?;
                let files = crate::discovery::discover_agents_md(&cwd).await;
                Ok(Value::Array(files))
            }
            <DiscoverPluginsReq as WorkspaceRpc>::METHOD => {
                let cwd = self.workspace.root_cwd()?;
                let plugins = crate::discovery::discover_plugins(
                    &cwd,
                    self.workspace.shared.plugin_discovery_config(),
                    &crate::discovery::PluginTrustStore::load(),
                    true,
                );
                Ok(Value::Array(plugins))
            }
            <ExportGithubReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<ExportGithubReq>(params, &self.workspace, None).await
            }
            <HookRegistryReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HookRegistryReq>(params, &self.workspace, None).await
            }
            <LoadProjectConfigReq as WorkspaceRpc>::METHOD => {
                let cwd = self.workspace.root_cwd()?;
                Ok(crate::discovery::load_project_config(&cwd))
            }
            <LoadPermissionsReq as WorkspaceRpc>::METHOD => {
                let cwd = self.workspace.root_cwd()?;
                Ok(crate::discovery::load_permissions(&cwd, true).await)
            }
            <LoadEnvrcReq as WorkspaceRpc>::METHOD => {
                let cwd = self.workspace.root_cwd()?;
                let env = crate::envrc::spawn_envrc_load(cwd, true).join().await;
                serde_json::to_value(env).map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            <InstallPluginReq as WorkspaceRpc>::METHOD => {
                note_mutation::<InstallPluginReq>(&self.workspace);
                let _ = params;
                Ok(Value::Null)
            }
            <RefreshPluginsReq as WorkspaceRpc>::METHOD => {
                let cwd = self.workspace.root_cwd()?;
                let plugins = crate::discovery::discover_plugins(
                    &cwd,
                    self.workspace.shared.plugin_discovery_config(),
                    &crate::discovery::PluginTrustStore::load(),
                    true,
                );
                Ok(Value::Array(plugins))
            }
            <ConfigureMcpReq as WorkspaceRpc>::METHOD => {
                note_mutation::<ConfigureMcpReq>(&self.workspace);
                let session_id = bound_session.ok_or_else(|| {
                    WorkspaceError::HubError("configure_mcp requires a bound session".into())
                })?;
                let configs: Vec<agent_client_protocol::McpServer> = serde_json::from_value(
                    params
                        .get("mcp_servers")
                        .cloned()
                        .unwrap_or(Value::Array(vec![])),
                )
                .map_err(|e| WorkspaceError::HubError(format!("invalid mcp_servers: {e}")))?;
                let result = async {
                    if self.workspace.session(session_id).is_none() {
                        tracing::info!(
                            session_id,
                            "workspace.configure_mcp: session not found, creating on demand"
                        );
                        match self
                            .workspace
                            .create_session_with_config(
                                session_id,
                                None,
                                None,
                                crate::capability::CapabilityMode::All,
                                None,
                                true,
                            )
                        {
                            Ok(session) => {
                                self.workspace.finalize_session_setup(&session).await;
                            }
                            Err(WorkspaceError::SessionAlreadyExists(_)) => {
                                tracing::debug!(
                                    session_id,
                                    "workspace.configure_mcp: session created concurrently, using existing"
                                );
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    self.workspace.start_session_mcp_servers(session_id, configs).await
                }
                    .await;
                record_mutation_rpc("configure_mcp", "self", session_id, &result);
                serde_json::to_value(&result?)
                    .map_err(|e| WorkspaceError::HubError(format!("serialize McpStartResult: {e}")))
            }
            <GitMetadataReq as WorkspaceRpc>::METHOD => {
                let cwd = self.workspace.root_cwd()?;
                let metadata =
                    crate::session::git::resolve_persisted_session_git_metadata_sync(&cwd);
                Ok(serde_json::to_value(metadata).unwrap_or(Value::Null))
            }
            <FuzzyStatusReq as WorkspaceRpc>::METHOD => {
                let search_id = params
                    .get("search_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| WorkspaceError::HubError("missing search_id".into()))?;
                let results = self.workspace.fuzzy_get_results(search_id).await;
                match results {
                    Some(data) => serde_json::to_value(data)
                        .map_err(|e| WorkspaceError::HubError(e.to_string())),
                    None => Ok(Value::Null),
                }
            }
            "workspace.worktree_create_from_worktree"
            | <CreateWorktreeFromWorktreeSyncReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<CreateWorktreeFromWorktreeSyncReq>(params, &self.workspace, None)
                    .await
            }
            <PrepareWorktreeFromWorktreeReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<PrepareWorktreeFromWorktreeReq>(params, &self.workspace, None).await
            }
            <WorktreeCreateSyncReq as WorkspaceRpc>::METHOD => {
                note_mutation::<WorktreeCreateSyncReq>(&self.workspace);
                let req: crate::worktree::CreateWorktreeRequest = serde_json::from_value(params)
                    .map_err(|e| {
                        WorkspaceError::HubError(format!("invalid create_sync params: {e}"))
                    })?;
                let result = crate::worktree::create_worktree_streaming(&req, &NoOpNotifier).await;
                serde_json::to_value(result).map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            <ReposListReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<ReposListReq>(params, &self.workspace, None).await
            }
            <GitStatusExtReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitStatusExtReq>(params, &self.workspace, None).await
            }
            <GitFilesReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitFilesReq>(params, &self.workspace, None).await
            }
            <GitDiffReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitDiffReq>(params, &self.workspace, None).await
            }
            <GitStageReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitStageReq>(params, &self.workspace, None).await
            }
            <GitStageContentReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitStageContentReq>(params, &self.workspace, None).await
            }
            <GitUnstageReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitUnstageReq>(params, &self.workspace, None).await
            }
            <GitDiscardReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitDiscardReq>(params, &self.workspace, None).await
            }
            <GitCommitReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitCommitReq>(params, &self.workspace, None).await
            }
            <GitSyncBaseReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitSyncBaseReq>(params, &self.workspace, None).await
            }
            <GitCheckoutReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitCheckoutReq>(params, &self.workspace, None).await
            }
            <GitEnsureBindingReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitEnsureBindingReq>(params, &self.workspace, None).await
            }
            <GitMergeToMainReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitMergeToMainReq>(params, &self.workspace, None).await
            }
            <GitPushReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitPushReq>(params, &self.workspace, None).await
            }
            <GitStashReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitStashReq>(params, &self.workspace, None).await
            }
            <GitInfoReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitInfoReq>(params, &self.workspace, None).await
            }
            <GitBranchesReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitBranchesReq>(params, &self.workspace, None).await
            }
            <GitCollectChangesReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitCollectChangesReq>(params, &self.workspace, None).await
            }
            <GitResolveRootReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitResolveRootReq>(params, &self.workspace, None).await
            }
            <GitCurrentCommitReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitCurrentCommitReq>(params, &self.workspace, None).await
            }
            <DetectVcsKindReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<DetectVcsKindReq>(params, &self.workspace, None).await
            }
            <GitCheckoutCommitReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<GitCheckoutCommitReq>(params, &self.workspace, None).await
            }
            <HunkSingleActionReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HunkSingleActionReq>(params, &self.workspace, bound_session).await
            }
            <HunkFileActionReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HunkFileActionReq>(params, &self.workspace, bound_session).await
            }
            <HunkTurnActionReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HunkTurnActionReq>(params, &self.workspace, bound_session).await
            }
            <HunkAllActionReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HunkAllActionReq>(params, &self.workspace, bound_session).await
            }
            <HunkGetAllFileContentsReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HunkGetAllFileContentsReq>(params, &self.workspace, bound_session)
                    .await
            }
            <HunkGetSessionSummaryReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HunkGetSessionSummaryReq>(params, &self.workspace, bound_session)
                    .await
            }
            <HunkGetAllHunksReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HunkGetAllHunksReq>(params, &self.workspace, bound_session).await
            }
            <HunkGetStagedFilesReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HunkGetStagedFilesReq>(params, &self.workspace, bound_session).await
            }
            <HunkGetFilteredHunksReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HunkGetFilteredHunksReq>(params, &self.workspace, bound_session).await
            }
            <HunkGetFileSummariesReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<HunkGetFileSummariesReq>(params, &self.workspace, bound_session).await
            }
            <CodeGotoDefinitionReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<CodeGotoDefinitionReq>(params, &self.workspace, None).await
            }
            <CodeGotoReferencesReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<CodeGotoReferencesReq>(params, &self.workspace, None).await
            }
            <CodeFindDefinitionsReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<CodeFindDefinitionsReq>(params, &self.workspace, None).await
            }
            <CodeFindReferencesReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<CodeFindReferencesReq>(params, &self.workspace, None).await
            }
            <CodeIndexStatusReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<CodeIndexStatusReq>(params, &self.workspace, None).await
            }
            <FuzzyOpenReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<FuzzyOpenReq>(params, &self.workspace, None).await
            }
            <FuzzyChangeReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<FuzzyChangeReq>(params, &self.workspace, None).await
            }
            <FuzzyCloseReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<FuzzyCloseReq>(params, &self.workspace, None).await
            }
            <ContentSearchRequest as WorkspaceRpc>::METHOD => {
                dispatch_op::<ContentSearchRequest>(params, &self.workspace, None).await
            }
            <CreateWorktreeRequest as WorkspaceRpc>::METHOD => {
                dispatch_op::<CreateWorktreeRequest>(params, &self.workspace, None).await
            }
            <RemoveWorktreeRequest as WorkspaceRpc>::METHOD => {
                dispatch_op::<RemoveWorktreeRequest>(params, &self.workspace, None).await
            }
            <ApplyWorktreeRequest as WorkspaceRpc>::METHOD => {
                dispatch_op::<ApplyWorktreeRequest>(params, &self.workspace, None).await
            }
            <WorktreeListReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<WorktreeListReq>(params, &self.workspace, None).await
            }
            <WorktreeShowReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<WorktreeShowReq>(params, &self.workspace, None).await
            }
            <WorktreeDetachReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<WorktreeDetachReq>(params, &self.workspace, None).await
            }
            <WorktreeSalvageReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<WorktreeSalvageReq>(params, &self.workspace, None).await
            }
            <WorktreeCleanArtifactsReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<WorktreeCleanArtifactsReq>(params, &self.workspace, None).await
            }
            <WorktreeGcReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<WorktreeGcReq>(params, &self.workspace, None).await
            }
            <WorktreeDbStatsReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<WorktreeDbStatsReq>(params, &self.workspace, None).await
            }
            <WorktreeDbRebuildReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<WorktreeDbRebuildReq>(params, &self.workspace, None).await
            }
            <WorktreeDbPathReq as WorkspaceRpc>::METHOD => {
                dispatch_op::<WorktreeDbPathReq>(params, &self.workspace, None).await
            }
            <BeginPromptReq as WorkspaceRpc>::METHOD => {
                let req: BeginPromptReq = serde_json::from_value(params).map_err(|e| {
                    WorkspaceError::HubError(format!("invalid params for begin_prompt: {e}"))
                })?;
                self.workspace
                    .session(&req.session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(req.session_id.clone()))?;
                self.workspace
                    .on_turn_boundary(
                        &req.session_id,
                        TurnBoundary::rewind_begin(req.prompt_index),
                    )
                    .await;
                Ok(Value::Null)
            }
            <EndPromptReq as WorkspaceRpc>::METHOD => {
                let req: EndPromptReq = serde_json::from_value(params).map_err(|e| {
                    WorkspaceError::HubError(format!("invalid params for end_prompt: {e}"))
                })?;
                self.workspace
                    .session(&req.session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(req.session_id.clone()))?;
                self.workspace
                    .on_turn_boundary(
                        &req.session_id,
                        TurnBoundary::rewind_finalize(req.prompt_index),
                    )
                    .await;
                Ok(Value::Null)
            }
            <GetRewindPointsReq as WorkspaceRpc>::METHOD => {
                let req: GetRewindPointsReq = serde_json::from_value(params).map_err(|e| {
                    WorkspaceError::HubError(format!("invalid params for get_rewind_points: {e}"))
                })?;
                let session = self
                    .workspace
                    .session(&req.session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(req.session_id.clone()))?;
                let points = session
                    .file_state_tracker()
                    .get_rewind_points_normalized(session.cwd())
                    .await;
                serde_json::to_value(points).map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            <PresenceNoteReq as WorkspaceRpc>::METHOD => {
                let req: PresenceNoteReq = serde_json::from_value(params).map_err(|e| {
                    WorkspaceError::HubError(format!("invalid params for presence.note: {e}"))
                })?;
                self.workspace
                    .session(&req.session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(req.session_id.clone()))?;
                self.workspace
                    .activity_tracker()
                    .apply_presence_note(req.visible, req.seq);
                Ok(Value::Null)
            }
            <RewindToReq as WorkspaceRpc>::METHOD => {
                note_mutation::<RewindToReq>(&self.workspace);
                let req: RewindToReq = serde_json::from_value(params).map_err(|e| {
                    WorkspaceError::HubError(format!("invalid params for rewind_to: {e}"))
                })?;
                self.workspace
                    .session(&req.session_id)
                    .ok_or_else(|| WorkspaceError::SessionNotFound(req.session_id.clone()))?;
                let response = self
                    .workspace
                    .rewind_to(&req.session_id, req.target_prompt_index)
                    .await;
                serde_json::to_value(response).map_err(|e| WorkspaceError::HubError(e.to_string()))
            }
            _ => {
                tracing::warn!(method, "unknown workspace rpc method");
                Err(WorkspaceError::UnknownMethod(method.to_owned()))
            }
        }
    }
}
#[async_trait]
impl ToolServerHandler for WorkspaceRpcHandler {
    fn tool_id(&self) -> ToolId {
        ToolId::new(WORKSPACE_RPC_TOOL_ID).expect("constant is a valid ToolId")
    }
    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            WORKSPACE_RPC_TOOL_ID,
            "Routes workspace RPC calls to the local workspace handle.",
        )
    }
    fn input_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "method": {
                    "type": "string",
                    "description": "The workspace.* method to invoke"
                },
                "params": {
                    "type": "object",
                    "description": "Method parameters"
                }
            },
            "required": ["method"]
        }))
    }
    async fn handle_call(&self, ctx: ToolCallContext, args: Value) -> ToolStream<TypedToolOutput> {
        let tool_id = self.tool_id();
        let method = match args.get("method").and_then(Value::as_str) {
            Some(m) => m,
            None => {
                return terminal_only(Err(ToolError::new(
                    ToolErrorKind::InvalidArguments,
                    "missing required field: method",
                )));
            }
        };
        tracing::debug!("workspace rpc call from server");
        let params = args
            .get("params")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let bound_session = ctx
            .extensions
            .get::<echo_agent_tool_runtime::SessionContext>();
        let session_id = bound_session.as_deref().map(|s| s.0.as_str());
        let start = std::time::Instant::now();
        let result = self.dispatch(method, params, session_id).await;
        let error_kind = result.as_ref().err().map(WorkspaceError::metric_kind);
        let is_unknown_method = matches!(&result, Err(WorkspaceError::UnknownMethod(_)));
        let method_label = if is_unknown_method {
            UNKNOWN_METHOD_LABEL
        } else {
            method
        };
        WORKSPACE_RPC_REQUESTS_TOTAL
            .with_label_values(&[method_label, if result.is_ok() { "ok" } else { "error" }])
            .inc();
        if let Some(error_kind) = error_kind {
            WORKSPACE_RPC_ERRORS_TOTAL
                .with_label_values(&[method_label, error_kind])
                .inc();
        }
        WORKSPACE_RPC_DURATION_SECONDS
            .with_label_values(&[method_label])
            .observe(start.elapsed().as_secs_f64());
        let envelope = match result {
            Ok(value) => RpcEnvelope::ok(value),
            Err(ref e) => envelope_err(e),
        };
        let envelope =
            serde_json::to_value(envelope).expect("RpcEnvelope<Value> serialization is infallible");
        terminal_only(Ok(TypedToolOutput::from_value(tool_id, envelope)))
    }
    async fn handle_hook(&self, session_id: SessionId, frame: HookFrame) {
        match frame.event {
            HookEvent::Cancel => {
                if let Some(call_id) = &frame.call_id {
                    tracing::info!(%session_id, %call_id, "cancel hook received");
                    self.workspace
                        .cancel_tool_call(session_id.as_str(), call_id.as_str());
                } else {
                    tracing::info!(%session_id, "cancel hook received (session-wide)");
                    self.workspace.cancel_all_tool_calls(session_id.as_str());
                }
            }
            HookEvent::SessionEnded => {
                tracing::info!(%session_id, "session_ended hook received");
                self.workspace
                    .teardown_session_mcp(session_id.as_str())
                    .await;
                self.workspace.on_session_ended(session_id.as_str());
            }
            HookEvent::Custom { kind, payload } => {
                use echo_agent_tool_protocol::turn_hook::{
                    AFTER_TURN_KIND, AfterTurnPayload, BEFORE_TURN_KIND, BeforeTurnPayload,
                };
                match kind.as_str() {
                    BEFORE_TURN_KIND => {
                        match serde_json::from_value::<BeforeTurnPayload>(payload) {
                            Ok(p) => {
                                tracing::info!(
                                    session = %session_id,
                                    turn = p.turn_number,
                                    model = %p.model_id,
                                    "before_turn hook received"
                                );
                                self.workspace.on_before_turn(session_id.as_str(), &p).await;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "before_turn payload deserialization failed"
                                );
                            }
                        }
                    }
                    AFTER_TURN_KIND => match serde_json::from_value::<AfterTurnPayload>(payload) {
                        Ok(p) => {
                            tracing::info!(
                                session = %session_id,
                                turn = p.turn_number,
                                outcome = ?p.outcome,
                                duration_ms = p.duration_ms,
                                "after_turn hook received"
                            );
                            self.workspace.on_after_turn(session_id.as_str(), &p).await;
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "after_turn payload deserialization failed"
                            );
                        }
                    },
                    _ => {
                        tracing::debug!(
                            kind = %kind,
                            session = %session_id,
                            "unrecognized custom hook kind"
                        );
                    }
                }
            }
            HookEvent::Pause | HookEvent::Resume => {
                tracing::debug!(%session_id, event = ?frame.event, "hook not yet implemented");
            }
        }
    }
    async fn handle_hook_request(&self, session_id: SessionId, frame: HookFrame) -> Option<Value> {
        use echo_agent_tool_protocol::turn_hook::{self, TurnHookRequest};
        let HookEvent::Custom { kind, payload } = frame.event else {
            return None;
        };
        if kind != turn_hook::TURN_HOOK_KIND {
            return None;
        }
        let no_op = || serde_json::to_value(turn_hook::HookReply::default()).ok();
        if self.workspace.shared.activity_tracker.is_draining()
            || self.workspace.session(session_id.as_str()).is_none()
        {
            return no_op();
        }
        let request: TurnHookRequest = match serde_json::from_value(payload) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, %session_id, "invalid turn hook request");
                return no_op();
            }
        };
        let reply = self
            .workspace
            .compute_turn_injections(session_id.as_str(), &request)
            .await;
        Some(serde_json::to_value(&reply).unwrap_or(Value::Null))
    }
    /// Hub-issued `tool_server.evict`. Always tears the evicted session down
    /// (MCP bridges + activity/writer state, like the `SessionEnded` hook), then
    /// runs the global two-phase drain **only** when no other session survives —
    /// a global drain shuts down the *shared* upload queue, which must not happen
    /// while another session is live. Idempotent across fan-out and safe for an
    /// already-gone session id.
    ///
    /// Contract: the server-supplied `grace_period_ms` budgets the drain and is
    /// therefore honored only when evicting the **last** live session. For a
    /// multi-session workspace the evicted session is dropped immediately
    /// (no per-session drain) because the shared upload queue cannot be flushed
    /// or closed without affecting the survivors.
    async fn handle_evict(&self, params: ToolServerEvictParams) {
        let sid = params.session_id.as_str();
        self.workspace.teardown_session_mcp(sid).await;
        self.workspace.on_session_ended(sid);
        let (became_empty, start_drain, removed) = {
            let mut sessions = self.workspace.shared.sessions.write();
            let removed = sessions.remove(sid);
            if let Some(session) = &removed {
                session.abort_system_notify_producers();
                session.shutdown_terminal_backend();
                session.shutdown_browser_service();
                session.cancel_hunk_tracker();
            }
            let empty = sessions.is_empty();
            let already_winding_down = self.workspace.activity_tracker().is_draining();
            let start = empty && !already_winding_down;
            if start {
                self.workspace.activity_tracker().set_draining();
            }
            (empty, start, removed)
        };
        if let Some(session) = removed {
            self.workspace.invoke_unbind_hook(&session);
        }
        if !start_drain {
            if became_empty {
                tracing::info!(
                    session = %params.session_id,
                    reason = %params.reason,
                    "workspace: hub evict — already draining/shutting down; dropped session only"
                );
            } else {
                tracing::info!(
                    session = %params.session_id,
                    reason = %params.reason,
                    "workspace: hub evict — other sessions live; dropped session only"
                );
            }
            return;
        }
        let grace = std::time::Duration::from_millis(params.grace_period_ms);
        tracing::info!(
            session = %params.session_id,
            reason = %params.reason,
            grace_period_ms = params.grace_period_ms,
            "workspace: hub evict — last session; commencing two-phase drain"
        );
        let unfinished = self
            .workspace
            .two_phase_drain(grace, crate::handle::DrainReason::Evict)
            .await;
        if unfinished > 0 {
            tracing::warn!(
                session = %params.session_id,
                unfinished,
                "workspace: hub evict drain left items pending"
            );
        }
        self.workspace.activity_tracker().set_shutting_down();
    }
}
#[cfg(test)]
#[path = "hub_server_tests.rs"]
mod tests;
