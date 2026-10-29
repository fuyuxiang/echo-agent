//! `SessionHandle` — the `Clone + Send` proxy for interacting with a session actor.
//!
//! Callers hold a `SessionHandle` and send `SessionCommand` messages via the
//! internal channel. Extracted from `acp_session.rs` to keep the actor
//! implementation focused on behaviour.
use super::commands::SessionCommand;
use super::persistence::{LocalFeedbackEntry, PersistenceMsg};
use agent_client_protocol as acp;
use echo_agent_file_utils::queue::UploadQueue;
use echo_agent_hunk_tracker::HunkTrackerHandle;
use echo_agent_sampling_types::ReasoningEffort;
use std::collections::{HashMap, HashSet};
use tokio::sync::{mpsc, oneshot};
/// Coarse lifecycle state of a session as known to the leader/agent.
///
/// A echoagent session has no
/// terminal status field on its own — it is a resumable log on disk — so
/// "liveness" is *residency + turn-state*, not a pid. The agent's join-handle
/// supervisor tracks this per session so a panicked actor can be reaped
/// (demoted to `Dormant`) instead of lingering as a roster zombie. This is the
/// data source the roster/dashboard reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionLiveState {
    /// Resident actor, a turn is currently running.
    Working,
    /// Resident actor, no turn in flight.
    IdleResident,
    /// On disk, not resident (idle-unloaded or never loaded this run).
    Dormant,
    /// Finished and resumable (terminal marker on disk).
    Completed,
    /// Actor panicked / load failed: the `JoinHandle` ended with no terminal
    /// marker. Harmless to reap — the conversation persists and demotes to
    /// `Dormant` on the next disk scan.
    DeadFailed,
    /// A load or resume is building the actor.
    Attaching,
}
/// `_meta` key carrying [`SessionHandle::scheduler_background_loops`] on the
/// `session/new` and `session/load` responses. Defined here so the shell that
/// publishes it and the clients that read it share one spelling.
pub const SCHEDULER_BACKGROUND_LOOPS_META_KEY: &str = "echo.agent/schedulerBackgroundLoops";
/// Handle for interacting with a session actor.
/// Note: Permission event receivers are returned separately from `spawn_session_actor`
/// and should be stored/managed by the caller.
#[derive(Clone)]
pub struct SessionHandle {
    pub cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    /// Persistence channel shared with the actor (used by extension handlers).
    pub(crate) persistence_tx: mpsc::UnboundedSender<PersistenceMsg>,
    /// Current running prompt/turn id, if any.
    ///
    /// Shared with the session actor so external cancellation paths can target
    /// subagents launched by the active turn only.
    pub current_prompt_id: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Open blocking reverse-requests (permission / question / plan-approval),
    /// keyed by `tool_call_id`. Mirrors `current_prompt_id`: the same `Arc` is
    /// shared with the session actor, which inserts on issue and removes on
    /// resolve. The roster reads this synchronously to surface `NeedsInput`
    /// Never persisted.
    pub pending_interactions: crate::session::pending_interaction::PendingInteractions,
    /// Session info (id, cwd) - cached for quick access without querying persistence
    pub info: crate::session::info::Info,
    /// Resolved turn limit for this session; lets a spawned subagent inherit
    /// the parent's limit. `None` = unlimited.
    pub max_turns: Option<usize>,
    /// Configured cutoff a subagent inherits, published by the session actor. `None` when unset.
    pub resolved_tool_overrides:
        std::sync::Arc<arc_swap::ArcSwapOption<echo_agent_sampling_types::ToolOverrides>>,
    /// Handle to the hunk tracker for this session
    pub hunk_tracker_handle: HunkTrackerHandle,
    /// Actor-based chat state handle — lets callers inspect final conversation state.
    pub chat_state_handle: echo_agent_chat_state::ChatStateHandle,
    /// Handle to session signals (used for completion tracking)
    pub signals_handle: super::signals::SessionSignalsHandle,
    /// Shared gate controlling whether the session actor forwards
    /// notifications to the client via the gateway. See
    /// [`SessionActor::gateway_enabled`] for details.
    pub gateway_enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// See [`SessionActor::status_line_enabled`]. Assigned by
    /// [`Self::set_status_line_wanted`] at every attach, and when a client
    /// disconnects from a session that stays resident.
    pub status_line_enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// MCP server configs for this session (merged local + client-provided).
    /// Stored on the handle so forked sessions can inherit the parent's
    /// MCP servers without requiring a round-trip through the session actor.
    ///
    /// **Note:** This is a snapshot from `spawn_session_actor` time. If the
    /// client later sends `UpdateMcpServers`, the handle's copy is NOT updated.
    /// This is fine for forks that happen immediately after spawn, but callers
    /// that need the latest MCP state should query the session actor via command.
    pub mcp_servers: Vec<acp::McpServer>,
    /// Client-provided MCP servers after vendor `mcps` kill-switch admission
    /// (still pre-merge with disk/plugins/managed). Hot-reloads re-merge from
    /// this seed so disabled-vendor servers rejected at ingress cannot reappear
    /// merely because on-disk attribution vanished mid-session.
    pub initial_client_mcp_servers: Vec<acp::McpServer>,
    /// Stable display path for forked sessions (original project path).
    ///
    /// When set, the hunk tracker extension handler rewrites worktree paths
    /// in API responses to this path so the client UI shows the original
    /// project path, not the worktree path.
    pub display_cwd: Option<String>,
    /// Feedback manager for periodic signal sync. Exposed so callers can
    /// attach GCS upload queue stats for snapshotting into signals.
    pub feedback_manager: std::sync::Arc<crate::session::feedback_manager::FeedbackManager>,
    /// Session-scoped upload queue. Lazily initialized on the first turn that
    /// enables trace uploads. `Arc<OnceLock<_>>` ensures all `SessionHandle`
    /// clones share the same underlying queue instance.
    pub(crate) upload_queue: std::sync::Arc<std::sync::OnceLock<UploadQueue>>,
    /// Consecutive upload failures with no confirmed upload in between,
    /// driving this session's upload-failure log suppression. Shared across
    /// handle clones; per-session so one session's bucket outage cannot mute
    /// another session's first-failure log (its unified_log artifact must
    /// carry evidence of its own failures).
    pub(crate) upload_failures_since_success: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Session context captured at spawn time so callers can inherit shared runtime state.
    pub tool_context: crate::tools::ToolContext,
    /// The model this session was created with (or switched to via setModel).
    /// Per-session tracking prevents cross-client contamination in leader mode
    /// where `MvpAgent.current_model_id` is shared mutable state.
    pub model_id: acp::ModelId,
    /// Whether this session's scheduled fires run as detached background
    /// subagents. Copied from the value the spawn resolved for the session's
    /// [`AgentRebuildSpec`](crate::session::agent_rebuild::AgentRebuildSpec), so
    /// it is pinned for the session's whole life exactly like the fire side.
    /// Published to clients on the `session/new` / `session/load` response so
    /// they describe the fires this session will actually get rather than
    /// re-resolving a setting that may have flipped since spawn.
    pub scheduler_background_loops: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// YOLO (auto-approve) mode for this session.
    /// Per-session tracking prevents cross-client contamination in leader mode
    /// where one client enabling YOLO could affect another client's sessions.
    pub yolo_mode: bool,
    /// Explicit origin client metadata captured when the session was created.
    /// Used for per-session User-Agent rendering and for scoping leader-mode
    /// client behaviors like yolo broadcasts.
    pub origin_client: Option<crate::http::OriginClientInfo>,
    /// Whether the client that created this session advertised
    /// `echo.agent/codeNavigation.enabled`.  Stored per-session so that in leader
    /// mode a later `initialize()` from a different client cannot retroactively
    /// change code-nav eligibility for already-running sessions.
    pub code_nav_enabled: bool,
    /// Whether the `ask_user_question` tool is exposed for this session
    /// (`_meta.askUserQuestion` / `--no-ask-user` and the remote settings / config /
    /// env gate). Subagents deliberately do not inherit it.
    pub ask_user_question_enabled: bool,
    /// Whether this session was spawned non-interactive
    /// (`startupHints.nonInteractive`, e.g. headless `-p` / SDK). Stored
    /// per-session so subagents inherit it at spawn.
    pub non_interactive: bool,
    /// Plan mode tracker — shared with the session actor via Arc.
    /// Exposed so the `echo.agent/toggle_plan_mode` handler can toggle plan mode
    /// without going through the session command channel.
    pub plan_mode: std::sync::Arc<parking_lot::Mutex<crate::session::plan_mode::PlanModeTracker>>,
    /// Debug flag: when set to `true`, the next turn unconditionally triggers
    /// auto-compaction regardless of context window usage. Consumed (reset to
    /// `false`) atomically on use via `compare_exchange`.
    /// Set via `echo.agent/debug/arm_auto_compact`.
    pub force_compact: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub permission_handle: echo_agent_workspace::permission::PermissionHandle,
    /// The parent SessionActor's live `Auth401AttributionCallback`
    /// (if any). Exposed on the handle so
    /// `MvpAgent::build_subagent_spawn_context` can copy it into the
    /// spawn context, so subagents inherit the parent's callback
    /// rather than getting a fresh one (preserving the parent's
    /// session_id on the child's emits).
    pub attribution_callback: Option<echo_agent_sampler::SharedAttributionCallback>,
    /// The agent definition name for this session.
    pub agent_name: String,
    pub managed_mcp_proxy_base_url: String,
    pub session_default_agent_profile: Option<String>,
    /// Subagent types this agent can spawn (from Agent(t1, t2) in tools).
    pub allowed_subagent_types: Option<Vec<String>>,
    /// Hook registry for this session (snapshot from spawn time).
    pub hook_registry: Option<std::sync::Arc<echo_agent_hooks::discovery::HookRegistry>>,
    /// Typed workspace operations handle (agent sessions use local ops).
    pub workspace_ops: echo_agent_workspace::WorkspaceOps,
    /// Terminal backend for this session. Subagents inherit the parent's
    /// backend so background tasks and monitors survive the subagent's exit.
    pub terminal_backend:
        Option<std::sync::Arc<dyn echo_agent_tools::computer::types::TerminalBackend>>,
    /// Notification handle for this session's tool bridge. Subagents use
    /// this to reparent surviving tasks' notification handles on exit so
    /// events route to the parent's notification bridge.
    pub tools_notification_handle:
        Option<echo_agent_tools::notification::types::ToolNotificationHandle>,
    /// Scheduler handle for this session. Subagents inherit the parent's
    /// handle so scheduled tasks survive the subagent's exit.
    pub scheduler_handle: Option<
        echo_agent_tools::implementations::echo_agent_build::scheduler::types::SchedulerHandle,
    >,
}
impl SessionHandle {
    /// Last assistant `model_id` / `model_fingerprint` in conversation (global, not turn-scoped).
    pub(crate) async fn get_model_metadata(&self) -> echo_agent_chat_state::ModelMetadata {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::GetModelMetadata { responds_to: tx })
            .is_ok()
        {
            rx.await.unwrap_or_default()
        } else {
            echo_agent_chat_state::ModelMetadata::default()
        }
    }
    /// Move a foreground bash command to background by tool_call_id.
    /// Returns `true` if a matching foreground process was found and unblocked.
    pub(crate) async fn background_foreground_command(&self, tool_call_id: &str) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::BackgroundForegroundCommand {
                tool_call_id: tool_call_id.to_string(),
                respond_to: tx,
            })
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }
    /// Kill a background task by task_id.
    /// Routes through the session actor to the ToolBridge's TerminalBackend.
    pub(crate) async fn kill_background_task(
        &self,
        task_id: &str,
        source: echo_agent_tools::types::KillSource,
    ) -> Result<echo_agent_tools::types::KillOutcome, String> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::KillBackgroundTask {
                task_id: task_id.to_string(),
                source,
                respond_to: tx,
            })
            .is_err()
        {
            return Err("session not found".to_string());
        }
        rx.await.unwrap_or(Err("session actor died".to_string()))
    }
    pub(crate) async fn delete_scheduled_task(&self, task_id: &str) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::DeleteScheduledTask {
                task_id: task_id.to_string(),
                respond_to: tx,
            })
            .is_err()
        {
            return Err("session not found".to_string());
        }
        rx.await.unwrap_or(Err("session actor died".to_string()))
    }
    /// Returns `true` if the session has work in flight: a running turn or
    /// queued inputs (`running_task.is_some() || !pending_inputs.is_empty()`).
    ///
    /// Used by the leader's idle-unload decision on client disconnect.
    /// Falls back to `true` (conservative: keep the session resident, never
    /// unload) if the actor is unreachable.
    pub async fn is_busy(&self) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::IsBusy { respond_to: tx })
            .is_err()
        {
            return true;
        }
        rx.await.unwrap_or(true)
    }
    /// List all background tasks.
    /// Routes through the session actor to the ToolBridge's TerminalBackend.
    pub async fn list_tasks(&self) -> Option<Vec<echo_agent_tools::types::TaskSnapshot>> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::ListTasks { respond_to: tx })
            .is_err()
        {
            return None;
        }
        rx.await.unwrap_or(None)
    }
    /// Get hooks list for the pager modal.
    pub(crate) async fn get_hooks_list(
        &self,
    ) -> Option<echo_agent_hooks_plugins_types::HooksListResponse> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::GetHooksList { respond_to: tx })
            .is_err()
        {
            return None;
        }
        rx.await.ok()
    }
    /// Execute a hooks management action from the pager modal.
    pub(crate) async fn execute_hooks_action(
        &self,
        action: echo_agent_hooks_plugins_types::HooksAction,
    ) -> Option<echo_agent_hooks_plugins_types::ActionOutcome> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::HooksAction {
                action,
                respond_to: tx,
            })
            .is_err()
        {
            return None;
        }
        rx.await.ok()
    }
    /// Execute a plugins management action from the pager modal.
    pub(crate) async fn execute_plugins_action(
        &self,
        action: echo_agent_hooks_plugins_types::PluginsAction,
    ) -> Option<echo_agent_hooks_plugins_types::ActionOutcome> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::PluginsAction {
                action,
                respond_to: tx,
            })
            .is_err()
        {
            return None;
        }
        rx.await.ok()
    }
    /// This session's plugin registry, including plugins loaded via `_meta.pluginDirs`.
    pub(crate) async fn plugins_list(
        &self,
    ) -> Option<std::sync::Arc<echo_agent_core::plugins::PluginRegistry>> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::PluginsList { respond_to: tx })
            .is_err()
        {
            return None;
        }
        rx.await.ok().flatten()
    }
    /// Snapshot the session's live MCP client pool for subagent inheritance.
    pub(crate) async fn snapshot_mcp_pool(
        &self,
    ) -> Option<crate::session::mcp_servers::SharedMcpPool> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::SnapshotMcpPool { respond_to: tx })
            .ok()?;
        rx.await.ok().flatten()
    }
    /// Snapshot the session's client-registered hooks for subagent inheritance. A dead actor
    /// or dropped reply fails open to no hooks, warned since it drops the inherited deny gate.
    pub(crate) async fn snapshot_client_hooks(&self) -> crate::extensions::hooks::ClientHooks {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::SnapshotClientHooks { respond_to: tx })
            .is_err()
        {
            tracing::warn!(
                "snapshot_client_hooks: session actor gone; subagent inherits no client hooks"
            );
            return Default::default();
        }
        rx.await.unwrap_or_else(|_| {
            tracing::warn!(
                "snapshot_client_hooks: reply dropped; subagent inherits no client hooks"
            );
            Default::default()
        })
    }
    /// A missing/dead parent must never become an unrestricted child.
    pub(crate) async fn snapshot_turn_authority(
        &self,
    ) -> Result<crate::session::InheritedTurnAuthority, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::SnapshotTurnAuthority { respond_to: tx })
            .map_err(|_| "Parent authority is unavailable".to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(10), rx)
            .await
            .map_err(|_| "Parent authority snapshot timed out".to_string())?
            .map_err(|_| "Parent authority snapshot was cancelled".to_string())
    }
    /// Snapshot the session's resolved tool schema for verbatim-fork inheritance.
    /// A dead actor or dropped reply fails open to an empty list (child then builds
    /// its own toolset, same as a non-fork spawn).
    pub(crate) async fn snapshot_tool_definitions(
        &self,
    ) -> Vec<echo_agent_sampling_types::ToolSpec> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::SnapshotToolDefinitions { respond_to: tx })
            .is_err()
        {
            tracing::warn!(
                "snapshot_tool_definitions: session actor gone; fork child inherits no parent tools"
            );
            return Vec::new();
        }
        rx.await.unwrap_or_else(|_| {
            tracing::warn!(
                "snapshot_tool_definitions: reply dropped; fork child inherits no parent tools"
            );
            Vec::new()
        })
    }
    pub(crate) async fn workflow_catalog_state(&self) -> (bool, bool) {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::GetWorkflowCatalogState { respond_to: tx })
            .is_err()
        {
            return (false, false);
        }
        rx.await.unwrap_or((false, false))
    }
    pub(crate) async fn list_available_commands(
        &self,
    ) -> crate::session::slash_commands::ListCommandsResponse {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::ListAvailableCommands { respond_to: tx })
            .is_err()
        {
            return crate::session::slash_commands::ListCommandsResponse::default();
        }
        rx.await
            .unwrap_or_else(|_| crate::session::slash_commands::ListCommandsResponse::default())
    }
    /// Record whether the client now on this session draws a status row.
    ///
    /// Assigned rather than raised and lowered from separate events: a resident
    /// session outlives its clients, and the disconnect sweep hands the
    /// decision to an attach that is already in flight. An attach that only
    /// raised the flag would leave the previous client's row armed, and the
    /// session would keep building payloads nobody draws.
    pub(crate) fn set_status_line_wanted(&self, wanted: bool) {
        self.status_line_enabled
            .store(wanted, std::sync::atomic::Ordering::Relaxed);
    }
    /// Ask for a fresh status-line snapshot. Used when a client attaches: the
    /// notification is transient, so there is nothing to replay. The emitter
    /// re-reads the capability when the wake lands, so
    /// [`Self::set_status_line_wanted`] has to be stored before this is sent.
    pub(crate) fn request_status_snapshot(&self) {
        let _ = self.cmd_tx.send(SessionCommand::EmitStatusSnapshot);
    }
    /// Replace the live session's client-registered hooks (see `SessionCommand::SetClientHooks`).
    pub(crate) fn set_client_hooks(&self, hooks: crate::extensions::hooks::ClientHooks) {
        let _ = self.cmd_tx.send(SessionCommand::SetClientHooks { hooks });
    }
    pub(crate) async fn get_mcp_status(&self) -> crate::extensions::mcp::McpStatusSnapshot {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::GetMcpStatus { respond_to: tx })
            .is_err()
        {
            return Default::default();
        }
        rx.await.unwrap_or_default()
    }
    pub(crate) async fn toggle_mcp_server(
        &self,
        server_name: String,
        enabled: bool,
        server_config: Option<agent_client_protocol::McpServer>,
    ) -> Result<(), agent_client_protocol::Error> {
        self.toggle_mcp_server_with_persistence(server_name, enabled, server_config, true)
            .await
    }
    /// Attach or detach an internal capability for this live session without
    /// touching user or project configuration.
    pub(crate) async fn toggle_mcp_server_session_scoped(
        &self,
        server_name: String,
        enabled: bool,
        server_config: Option<agent_client_protocol::McpServer>,
    ) -> Result<(), agent_client_protocol::Error> {
        self.toggle_mcp_server_with_persistence(server_name, enabled, server_config, false)
            .await
    }
    async fn toggle_mcp_server_with_persistence(
        &self,
        server_name: String,
        enabled: bool,
        server_config: Option<agent_client_protocol::McpServer>,
        persist_enabled_state: bool,
    ) -> Result<(), agent_client_protocol::Error> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::ToggleMcpServer {
                server_name,
                enabled,
                persist_enabled_state,
                server_config,
                respond_to: tx,
            })
            .is_err()
        {
            return Err(agent_client_protocol::Error::internal_error().data("session closed"));
        }
        rx.await
            .map_err(|_| agent_client_protocol::Error::internal_error().data("session closed"))?
    }
    pub(crate) async fn toggle_mcp_tool(
        &self,
        server_name: String,
        tool_name: String,
        enabled: bool,
    ) -> Result<(), agent_client_protocol::Error> {
        self.toggle_mcp_tool_with_source(server_name, tool_name, enabled, false)
            .await
    }
    pub(crate) async fn toggle_managed_gateway_tool(
        &self,
        server_name: String,
        tool_name: String,
        enabled: bool,
    ) -> Result<(), agent_client_protocol::Error> {
        self.toggle_mcp_tool_with_source(server_name, tool_name, enabled, true)
            .await
    }
    async fn toggle_mcp_tool_with_source(
        &self,
        server_name: String,
        tool_name: String,
        enabled: bool,
        is_managed_gateway: bool,
    ) -> Result<(), agent_client_protocol::Error> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::ToggleMcpTool {
                server_name,
                tool_name,
                enabled,
                is_managed_gateway,
                respond_to: tx,
            })
            .is_err()
        {
            return Err(agent_client_protocol::Error::internal_error().data("session closed"));
        }
        rx.await
            .map_err(|_| agent_client_protocol::Error::internal_error().data("session closed"))?
    }
    pub(crate) async fn managed_gateway_disabled_tool_names(
        &self,
    ) -> HashMap<String, HashSet<String>> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::GetManagedGatewayDisabledTools { respond_to: tx })
            .is_err()
        {
            return HashMap::new();
        }
        rx.await.unwrap_or_default()
    }
    pub(crate) async fn retry_auth_required_servers(&self) {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::RetryAuthRequiredServers { respond_to: tx })
            .is_err()
        {
            return;
        }
        let _ = rx.await;
    }
    pub async fn call_mcp_tool(
        &self,
        server_name: String,
        server_url: Option<String>,
        tool_name: String,
        arguments: serde_json::Value,
    ) -> Result<crate::extensions::mcp::McpCallResponse, String> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::CallMcpTool {
                server_name,
                server_url,
                tool_name,
                arguments,
                respond_to: tx,
            })
            .is_err()
        {
            return Err("session closed".to_string());
        }
        rx.await
            .unwrap_or_else(|_| Err("session closed".to_string()))
    }
    pub(crate) async fn read_mcp_resource(
        &self,
        server_name: String,
        uri: String,
    ) -> Result<crate::extensions::mcp::McpReadResourceResponse, String> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::ReadMcpResource {
                server_name,
                uri,
                respond_to: tx,
            })
            .is_err()
        {
            return Err("session closed".to_string());
        }
        rx.await
            .unwrap_or_else(|_| Err("session closed".to_string()))
    }
    pub(crate) async fn mcp_auth_status(&self) -> Vec<crate::extensions::mcp::McpAuthStatusEntry> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::McpAuthStatus { respond_to: tx })
            .is_err()
        {
            return vec![];
        }
        rx.await.unwrap_or_default()
    }
    pub(crate) async fn mcp_auth_trigger(&self, server_name: String) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(SessionCommand::McpAuthTrigger {
                server_name,
                respond_to: tx,
            })
            .is_err()
        {
            return Err("session closed".to_string());
        }
        rx.await
            .unwrap_or_else(|_| Err("session closed".to_string()))
    }
    /// Emit a PluginUpdatesInstalled notification to the session.
    /// Fire-and-forget — no response expected.
    pub(crate) async fn notify_plugin_updates(&self, updates: Vec<(String, String, String)>) {
        let _ = self
            .cmd_tx
            .send(SessionCommand::NotifyPluginUpdates { updates });
    }
    /// Send a feedback entry to the persistence actor; logs on a closed channel.
    pub(crate) fn persist_feedback(&self, entry: LocalFeedbackEntry) {
        if self
            .persistence_tx
            .send(PersistenceMsg::Feedback(entry))
            .is_err()
        {
            tracing::warn!(
                session_id = %self.info.id.0,
                "feedback persistence channel closed; entry dropped",
            );
        }
    }
}
