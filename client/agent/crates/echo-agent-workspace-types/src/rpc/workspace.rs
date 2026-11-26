//! Environment/info/config/session-admin methods (`workspace.info`,
//! `workspace.load_*`, `workspace.tool_definitions`, plugin management).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RpcActivityClass, WorkspaceRpc};

/// `workspace.info`. `Response` stays the raw [`Value`] to preserve the
/// `WorkspaceOps::workspace_info()` contract; [`WorkspaceInfo`] is the
/// typed shape of that value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceInfoReq {}

impl WorkspaceRpc for WorkspaceInfoReq {
    const METHOD: &'static str = "workspace.info";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

/// `workspace.load_project_config` — project config discovered at the
/// workspace root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadProjectConfigReq {}

impl WorkspaceRpc for LoadProjectConfigReq {
    const METHOD: &'static str = "workspace.load_project_config";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

/// `workspace.load_permissions` — permission settings discovered at the
/// workspace root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadPermissionsReq {}

impl WorkspaceRpc for LoadPermissionsReq {
    const METHOD: &'static str = "workspace.load_permissions";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

/// `workspace.load_envrc` — `.envrc` environment loaded at the workspace
/// root (empty object when absent).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadEnvrcReq {}

impl WorkspaceRpc for LoadEnvrcReq {
    const METHOD: &'static str = "workspace.load_envrc";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

/// `workspace.tool_definitions` — tool definitions for a session's
/// finalized toolset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolDefinitionsReq {
    pub session_id: String,
}

impl WorkspaceRpc for ToolDefinitionsReq {
    const METHOD: &'static str = "workspace.tool_definitions";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

/// `workspace.resolve_file_references` — resolve `@file` references
/// against the workspace root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolveFileReferencesReq {
    pub refs: Vec<String>,
}

impl WorkspaceRpc for ResolveFileReferencesReq {
    const METHOD: &'static str = "workspace.resolve_file_references";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

/// `workspace.update_tool_config` — replace a session's tool config.
///
/// Rejected with the retryable [`TURN_ACTIVE`](super::envelope::TURN_ACTIVE)
/// wire code while the target session has an active turn and the new config
/// differs; retry at the turn boundary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateToolConfigReq {
    /// Deprecated: self-attested and no longer trusted. The server derives
    /// the caller from the hub-bound envelope session and only falls back to
    /// this field when no envelope session is present (old call paths).
    /// Empty means absent: skipped on serialize so typed clients that leave
    /// the default do not send a self-attested `""` (the server also
    /// filters empty to absent for old serializers).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caller_session_id: String,
    pub session_id: String,
    pub new_config: Value,
}

impl WorkspaceRpc for UpdateToolConfigReq {
    const METHOD: &'static str = "workspace.update_tool_config";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = Value;
}

/// `workspace.drop_session` — drop a workspace session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DropSessionReq {
    /// Deprecated: self-attested and no longer trusted. The server derives
    /// the caller from the hub-bound envelope session and only falls back to
    /// this field when no envelope session is present (old call paths).
    /// Empty means absent: skipped on serialize so typed clients that leave
    /// the default do not send a self-attested `""` (the server also
    /// filters empty to absent for old serializers).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caller_session_id: String,
    pub session_id: String,
}

impl WorkspaceRpc for DropSessionReq {
    const METHOD: &'static str = "workspace.drop_session";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

/// `workspace.configure_mcp` — start MCP servers for the caller's bound session.
/// `mcp_servers` stays raw JSON (the shape is the ACP `McpServer` list)
/// so this crate carries no `agent-client-protocol` dependency.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigureMcpReq {
    pub mcp_servers: Value,
}

impl WorkspaceRpc for ConfigureMcpReq {
    const METHOD: &'static str = "workspace.configure_mcp";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = Value;
}

/// `workspace.install_plugin` — no-op on the server (installation needs
/// shell-side auth + registry); always returns `null`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstallPluginReq {}

impl WorkspaceRpc for InstallPluginReq {
    const METHOD: &'static str = "workspace.install_plugin";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = Value;
}

/// `workspace.refresh_plugins` — re-discover plugins at the workspace root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RefreshPluginsReq {}

impl WorkspaceRpc for RefreshPluginsReq {
    const METHOD: &'static str = "workspace.refresh_plugins";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

/// One still-running background terminal command (a slim, dependency-free DTO
/// over `echo_agent_tools`'s `TaskSnapshot`). `tool_name`, when set, is the
/// model-facing name of the tool that created the task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTaskSummaryWire {
    pub task_id: String,
    /// The command line that was launched (prefers `display_command`).
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

/// Response of `workspace.list_background_tasks` — outstanding (not-completed)
/// background terminal tasks only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListBackgroundTasksResponse {
    pub tasks: Vec<BackgroundTaskSummaryWire>,
}

/// `workspace.list_background_tasks` — list the outstanding background terminal
/// commands for `session_id`, for post-compaction `<system-reminder>` state.
/// `WorkspaceClient` is session-agnostic, so the caller supplies the hub-bound
/// session id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListBackgroundTasksReq {
    pub session_id: String,
}

impl WorkspaceRpc for ListBackgroundTasksReq {
    const METHOD: &'static str = "workspace.list_background_tasks";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = ListBackgroundTasksResponse;
}

/// One outstanding background terminal task, with the fields client task UI
/// needs (a slim DTO over `echo_agent_tools`'s `TaskSnapshot`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTaskSnapshotWire {
    /// Background task registry id (pairs with the `task.*` push events).
    pub task_id: String,
    /// The launched command line (prefers `display_command`).
    pub command: String,
    /// `bash` | `monitor`.
    pub kind: String,
    /// RFC3339 start timestamp.
    pub started_at: String,
    /// Model-supplied label when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One live scheduled task (`/loop`), a slim DTO over the scheduler's
/// `ScheduledTask` (pairs with the `scheduled_task.*` push events).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTaskSnapshotWire {
    pub task_id: String,
    pub prompt: String,
    /// Human-readable schedule, e.g. "every 5 minutes".
    pub human_schedule: String,
    /// RFC3339 timestamp of the next fire.
    pub next_fire_at: String,
    pub recurring: bool,
    /// RFC3339 creation timestamp.
    pub created_at: String,
}

/// Response of `workspace.tasks_snapshot`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TasksSnapshotResponse {
    /// Incomplete and backgrounded only (not in-flight foreground runs).
    pub background_tasks: Vec<BackgroundTaskSnapshotWire>,
    pub scheduled_tasks: Vec<ScheduledTaskSnapshotWire>,
}

/// Point-in-time task UI rebuild on attach/reconnect.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TasksSnapshotReq {
    pub session_id: String,
}

impl WorkspaceRpc for TasksSnapshotReq {
    const METHOD: &'static str = "workspace.tasks_snapshot";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = TasksSnapshotResponse;
}

/// Outcome of `workspace.kill_task`, mirroring `echo_agent_tools::KillOutcome`
/// wire tags (`killed` / `already_exited` / `not_found`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KillTaskOutcome {
    Killed,
    AlreadyExited,
    NotFound,
}

/// Response of `workspace.kill_task`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillTaskResponse {
    pub task_id: String,
    pub outcome: KillTaskOutcome,
}

/// `workspace.kill_task` — terminate a background terminal task by id.
/// Caller supplies the hub-bound session id (same as `tasks_snapshot`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KillTaskReq {
    pub session_id: String,
    pub task_id: String,
}

impl WorkspaceRpc for KillTaskReq {
    const METHOD: &'static str = "workspace.kill_task";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = KillTaskResponse;
}

/// Response of `workspace.delete_scheduled_task`. `deleted` is false when the task id was not found (already removed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteScheduledTaskResponse {
    pub task_id: String,
    pub deleted: bool,
}

/// `workspace.delete_scheduled_task`: delete a scheduled (loop) task by id. Caller supplies the hub-bound session id (same as `tasks_snapshot`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteScheduledTaskReq {
    pub session_id: String,
    pub task_id: String,
}

impl WorkspaceRpc for DeleteScheduledTaskReq {
    const METHOD: &'static str = "workspace.delete_scheduled_task";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = DeleteScheduledTaskResponse;
}

/// One TODO list item (slim DTO over `echo_agent_tools`'s `TodoState`). `status`
/// is the snake_case tag: `pending` | `in_progress` | `completed` | `cancelled`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoSummaryWire {
    pub id: String,
    pub content: String,
    pub status: String,
}

/// Response of `workspace.list_todos` — the full TODO list for the session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListTodosResponse {
    pub todos: Vec<TodoSummaryWire>,
}

/// `workspace.list_todos` — list the session's TODO items for post-compaction
/// `<system-reminder>` state. Caller supplies the hub-bound session id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListTodosReq {
    pub session_id: String,
}

impl WorkspaceRpc for ListTodosReq {
    const METHOD: &'static str = "workspace.list_todos";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = ListTodosResponse;
}

/// Typed response of `workspace.info`.
///
/// SYNC: matches the object built by the `workspace.info` dispatch arm
/// in `echo-agent-workspace/src/hub_server.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// `std::env::consts::OS` on the server (e.g. `"linux"`).
    pub os: String,
    /// Shell basename (e.g. `"bash"`); `"sh"` when `$SHELL` is unset.
    pub shell: String,
    pub cwd: String,
    /// Server binary version (`echo_agent_version::VERSION`); `None` on
    /// servers predating the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_info_deserializes_server_shape() {
        let raw = serde_json::json!({
            "os": "linux",
            "shell": "bash",
            "cwd": "/workspace",
        });
        let info: WorkspaceInfo = serde_json::from_value(raw).unwrap();
        assert_eq!(
            info,
            WorkspaceInfo {
                os: "linux".to_owned(),
                shell: "bash".to_owned(),
                cwd: "/workspace".to_owned(),
                version: None,
            }
        );
    }

    #[test]
    fn workspace_info_round_trips_version_field() {
        let info = WorkspaceInfo {
            os: "linux".to_owned(),
            shell: "bash".to_owned(),
            cwd: "/workspace".to_owned(),
            version: Some("1.2.3".to_owned()),
        };
        let raw = serde_json::to_value(&info).unwrap();
        assert_eq!(raw["version"], "1.2.3");
        let back: WorkspaceInfo = serde_json::from_value(raw).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn workspace_info_omits_absent_optional_fields() {
        let info = WorkspaceInfo {
            os: "linux".to_owned(),
            shell: "bash".to_owned(),
            cwd: "/workspace".to_owned(),
            version: None,
        };
        let raw = serde_json::to_value(&info).unwrap();
        assert!(raw.get("version").is_none());
    }

    #[test]
    fn workspace_info_ignores_unknown_fields() {
        let raw = serde_json::json!({
            "os": "linux",
            "shell": "zsh",
            "cwd": "/workspace",
            "future_field": 42,
        });
        let info: WorkspaceInfo = serde_json::from_value(raw).unwrap();
        assert_eq!(info.shell, "zsh");
    }

    #[test]
    fn method_constant() {
        assert_eq!(WorkspaceInfoReq::METHOD, "workspace.info");
        assert_eq!(
            LoadProjectConfigReq::METHOD,
            "workspace.load_project_config"
        );
        assert_eq!(LoadPermissionsReq::METHOD, "workspace.load_permissions");
        assert_eq!(LoadEnvrcReq::METHOD, "workspace.load_envrc");
        assert_eq!(ToolDefinitionsReq::METHOD, "workspace.tool_definitions");
        assert_eq!(
            ResolveFileReferencesReq::METHOD,
            "workspace.resolve_file_references"
        );
        assert_eq!(UpdateToolConfigReq::METHOD, "workspace.update_tool_config");
        assert_eq!(DropSessionReq::METHOD, "workspace.drop_session");
        assert_eq!(ConfigureMcpReq::METHOD, "workspace.configure_mcp");
        assert_eq!(InstallPluginReq::METHOD, "workspace.install_plugin");
        assert_eq!(RefreshPluginsReq::METHOD, "workspace.refresh_plugins");
    }
}
