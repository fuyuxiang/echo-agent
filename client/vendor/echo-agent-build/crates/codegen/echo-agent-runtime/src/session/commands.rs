//! Session actor command enum and associated public types.
//!
//! `SessionCommand` defines the message protocol used to drive a session
//! actor. It was extracted from `acp_session.rs` to keep the actor
//! implementation focused on behaviour.
use super::acp_types::*;
use super::plan_mode::PromptMode;
use crate::extensions::notification::SessionNotification;
use crate::session::signals::TurnDeltaSnapshot;
use agent_client_protocol as acp;
use tokio::sync::oneshot;
/// Structured context for a cancelled turn, replacing stringly-typed JSON.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CancellationContext {
    pub tool_name: Option<String>,
    pub reason: Option<String>,
    pub hook_name: Option<String>,
    /// What triggered the cancel (e.g. `"send_now"`, `"esc"`, `"mouse"`);
    /// surfaced as `cancelTrigger` on the turn-end `_meta`.
    pub trigger: Option<String>,
}
/// Failure surface of a `/btw` side question. Kept typed until the ACP
/// boundary so `handle_btw` can route model errors through the canonical
/// [`map_sampling_err_to_acp`](crate::sampling::error::map_sampling_err_to_acp)
/// (typed rate-limit / auth codes) instead of a flattened string.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SideQuestionError {
    #[error("side question model call failed: {0}")]
    Sampling(#[from] echo_agent_sampling_types::SamplingError),
    #[error("failed to prepare client: {0}")]
    PrepareClient(String),
    #[error("No response from model")]
    EmptyResponse,
}
/// Prompt completion kind returned to the ACP layer.
#[derive(Debug, Clone)]
pub enum PromptCompletionKind {
    Completed,
    /// Silent EndTurn after stationarity/true-noop thrash. Distinct from
    /// Completed so goal continuation is not re-queued under an active goal.
    StationarityEnded,
    Cancelled {
        category: Option<echo_agent_session_events::types::CancellationCategory>,
        context: Option<CancellationContext>,
    },
    MaxTurnsReached {
        limit: usize,
    },
    Rewound,
    /// A queued prompt was removed (or cleared) from the server-authoritative
    /// queue before it ever ran. Used to resolve the still-pending
    /// `session/prompt` RPC of the client that submitted it WITHOUT triggering
    /// any turn-completion side effects: the prompt never started a turn, so the
    /// `prompt_complete` broadcast (which carries no `promptId` and would tell
    /// every attached leader-mode client the *running* turn ended) and the
    /// roster `Idle` delta (which would flip the dashboard off `Working` while
    /// the real turn is still in flight) must be skipped. See
    /// `MvpAgent::prompt`'s short-circuit and `respond_removed_prompt`.
    RemovedFromQueue,
}
/// `_meta.cancellationCategory` of a hook-denied cancel; the pager matches it
/// to render the blocked-by-a-hook marker.
pub const HOOK_DENIED_CATEGORY: &str = "HookDenied";
/// `_meta.cancellationCategory` of a max-turns end; headless matches it to
/// drive the max-turns exit code.
pub const MAX_TURNS_REACHED_CATEGORY: &str = "max_turns_reached";
/// `_meta.cancellationCategory` of a stationarity end.
pub const ACTION_STATIONARITY_CATEGORY: &str = "action_stationarity";
/// `_meta.cancellationCategory` wire name of a cancel category — an explicit
/// match so a variant rename cannot silently change the wire. Deliberately a
/// second vocabulary next to the serde snake_case of the events.jsonl /
/// after-turn rails: `_meta` shipped PascalCase and clients match it.
pub fn meta_category_str(
    category: echo_agent_session_events::types::CancellationCategory,
) -> &'static str {
    use echo_agent_session_events::types::CancellationCategory;
    match category {
        CancellationCategory::HookDenied => HOOK_DENIED_CATEGORY,
        CancellationCategory::PermissionRejected => "PermissionRejected",
        CancellationCategory::PermissionCancelled => "PermissionCancelled",
        CancellationCategory::MidTurnAbort => "MidTurnAbort",
    }
}
impl PromptCompletionKind {
    /// The completion's `_meta.cancellationCategory`, shared by every terminal
    /// rail (`PromptResponse` `_meta`, legacy `prompt_complete`, durable
    /// `TurnCompleted`) so the wires never disagree.
    pub fn cancellation_category_meta(&self) -> Option<String> {
        match self {
            Self::Cancelled { category, .. } => {
                category.map(|cat| meta_category_str(cat).to_string())
            }
            Self::MaxTurnsReached { .. } => Some(MAX_TURNS_REACHED_CATEGORY.to_string()),
            Self::StationarityEnded => Some(ACTION_STATIONARITY_CATEGORY.to_string()),
            Self::Completed | Self::Rewound | Self::RemovedFromQueue => None,
        }
    }
}
/// Successful prompt/turn payload returned to the ACP layer and trace uploaders.
#[derive(Debug, Clone)]
pub struct PromptTurnOk {
    pub stop_reason: acp::StopReason,
    pub total_tokens: u64,
    pub turn_snapshot: Option<TurnDeltaSnapshot>,
    pub completion_kind: PromptCompletionKind,
    /// Schema-validated `--json-schema` output, delivered to the client in the
    /// prompt-response `_meta`. `None` unless a schema was requested;
    /// `Some(Err)` carries a parse/validation error message.
    pub structured_output: Option<Result<serde_json::Value, String>>,
    pub usage: Option<crate::extensions::notification::PromptUsage>,
    pub tool_overrides: Option<echo_agent_sampling_types::ToolOverrides>,
}
/// Result of a prompt turn, containing the stop reason, accumulated token count,
/// and an optional turn-end signals snapshot (for trace metadata enrichment).
pub(crate) type PromptTurnResult = Result<PromptTurnOk, acp::Error>;
/// Convenience: successful end-of-turn result.
pub(crate) fn ok_end_turn(tokens: u64, snapshot: Option<TurnDeltaSnapshot>) -> PromptTurnResult {
    Ok(PromptTurnOk {
        stop_reason: acp::StopReason::EndTurn,
        total_tokens: tokens,
        turn_snapshot: snapshot,
        completion_kind: PromptCompletionKind::Completed,
        structured_output: None,
        usage: None,
        tool_overrides: None,
    })
}
/// Pre-parsed prompt metadata sent back to the caller after `parse_prompt`.
pub struct ParsedPromptInfo {
    /// Post-truncation text (what the model sees).
    pub text: String,
    /// Pre-truncation text, only `Some` when truncated.
    pub full_text: Option<String>,
    /// Local disk path embedded in truncated message, only `Some` when truncated.
    pub local_path: Option<std::path::PathBuf>,
}
/// Priority levels for notification drain timing.
///
/// Ordering: `Next < Later` (derived from declaration order).
/// `Next` = more urgent, eligible for mid-turn drain (future enhancement).
/// `Later` = deferred to end-of-turn or idle drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NotificationPriority {
    /// Drain mid-turn (between tool calls). For urgent monitor events.
    Next,
    /// Drain only at end-of-turn or when idle. Used for bash task completions.
    Later,
}
#[derive(Debug, Clone)]
pub enum NotificationSource {
    MonitorEvent { task_id: String },
    MonitorCompleted { task_id: String },
    BashTaskCompleted { task_id: String },
}
impl NotificationSource {
    pub fn task_id(&self) -> &str {
        match self {
            Self::MonitorEvent { task_id }
            | Self::MonitorCompleted { task_id }
            | Self::BashTaskCompleted { task_id } => task_id,
        }
    }
}
#[derive(Debug)]
pub struct TaskWakeFallback {
    pub prompt_id: String,
    pub prompt_blocks: Vec<acp::ContentBlock>,
    pub source: NotificationSource,
}
#[derive(Debug)]
pub struct TaskWakeAdmission {
    pub respond_to: oneshot::Sender<bool>,
    pub fallback: TaskWakeFallback,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownKind {
    /// Running work survives (idle unload, process quiesce, subagent teardown).
    Graceful,
    CancelRunningTurn,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelTrigger {
    Esc,
    CtrlC,
    SendNow,
    Shutdown,
    SessionClose,
    SessionDelete,
    Client(String),
}
/// What a cancel means for the session, derived from its trigger by [`CancelTrigger::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelKind {
    StopGesture,
    Replace,
    Teardown,
}
impl CancelTrigger {
    /// Parse a client's `_meta.cancelTrigger`. Internal spellings land in
    /// [`Self::Client`], so a client-supplied string never maps to an
    /// internal trigger.
    pub fn from_client(s: &str) -> Self {
        match s {
            "esc" => Self::Esc,
            "ctrl_c" => Self::CtrlC,
            other => Self::Client(other.to_string()),
        }
    }
    /// The one place a trigger is classified, so a new variant is a single decision.
    /// `Client(_)` lands on `StopGesture`, so an unrecognized wire name fails closed.
    pub fn kind(&self) -> CancelKind {
        match self {
            Self::Esc | Self::CtrlC | Self::Client(_) => CancelKind::StopGesture,
            Self::SendNow => CancelKind::Replace,
            Self::Shutdown | Self::SessionClose | Self::SessionDelete => CancelKind::Teardown,
        }
    }
    pub fn as_str(&self) -> &str {
        match self {
            Self::Esc => "esc",
            Self::CtrlC => "ctrl_c",
            Self::SendNow => "send_now",
            Self::Shutdown => "shutdown",
            Self::SessionClose => "session_close",
            Self::SessionDelete => "session_delete",
            Self::Client(s) => s,
        }
    }
}
/// What a cancel does to the in-memory conversation history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CancelHistoryDisposition {
    /// Leave the cancelled turn's user message in history.
    #[default]
    Keep,
    /// Pop the named front if the no-output window is still open. `None` is a legacy client.
    RewindIfNoOutput { prompt_id: Option<String> },
}
#[derive(Debug, Clone, Default)]
pub struct CancelOptions {
    pub cancel_subagents: bool,
    pub kill_background_tasks: bool,
    pub history: CancelHistoryDisposition,
    pub trigger: Option<CancelTrigger>,
    /// Drives the cancel-rate metric, and marks an untriggered cancel as the user's.
    pub user_initiated: bool,
    /// Optional acknowledgement for clients that must know the session actor has
    /// actually applied the cancellation before presenting a stable stopped state.
    /// Internal fire-and-forget cancellation paths leave this unset.
    pub acknowledged: Option<std::sync::Arc<tokio::sync::Notify>>,
}
pub enum SessionCommand {
    Initialize {
        system_prompt: String,
    },
    /// Non-destructive system-prompt sync on session attach: swaps only the
    /// leading `System` message, keeping user/assistant turns. Backed by the
    /// atomic `ChatStateCommand::ReplaceSystemHead` (see its doc for the
    /// serialization guarantees); no-op when the live head already matches.
    ReplaceSystemPrompt {
        system_prompt: String,
    },
    /// Push a fresh status-line snapshot. Sent when a client attaches to a
    /// resident session, which the transient `SessionStatus` notification would
    /// otherwise never reach.
    EmitStatusSnapshot,
    /// Resume hook: after a session is restored with
    /// `awaiting_plan_approval == true`, re-issue the `exit_plan_mode`
    /// reverse-request so the client re-shows approval chrome over a real live
    /// waiter. Fire-and-forget; the actor spawns the round-trip + decision.
    RestorePlanApproval,
    /// A `/rename` landed for this resident session. `manual: true` (a user
    /// title) freezes the auto title refresh and aborts any in-flight one;
    /// `manual: false` (`/rename --auto`) reopens it so the whole-conversation
    /// refresh can re-title.
    TitleRenamed {
        manual: bool,
    },
    GetToolOverrides {
        respond_to: oneshot::Sender<Option<echo_agent_sampling_types::ToolOverrides>>,
    },
    /// Establish the per-turn tool-overrides state before the first prompt runs. Sent once by
    /// `handle_subagent_request` ahead of the child's first `Prompt`, so a spawned subagent's
    /// inherited cutoff is applied and published (for its own subagents to read) before any turn.
    SetToolOverrides {
        overrides: echo_agent_sampling_types::ToolOverrides,
    },
    Prompt {
        prompt_id: String,
        prompt_blocks: Vec<acp::ContentBlock>,
        /// Prompt mode parsed from request `_meta.mode`.
        prompt_mode: PromptMode,
        #[allow(private_interfaces)]
        artifact_upload_ctx: Option<crate::upload::manifest::ArtifactUploadContext>,
        /// Optional client identifier from the prompt request meta (overrides session-level one)
        client_identifier: Option<String>,
        /// Optional screen mode from the prompt request meta (`_meta.screenMode`,
        /// pager-only: `fullscreen` | `inline` | `minimal` | `headless`).
        /// Telemetry-only; `None` for other clients and synthetic prompts.
        screen_mode: Option<String>,
        /// Skip `<user_query>` wrapping and large-prompt truncation.
        verbatim: bool,
        /// W3C traceparent from the caller's OTEL span context, used to link
        /// `session.handle_prompt` back to `agent.prompt` across the channel hop.
        traceparent: Option<String>,
        json_schema: Option<serde_json::Value>,
        /// Cancel-and-send: cancel the running turn and run this prompt next.
        /// Also derived server-side during an interruptible wait (see
        /// [`SessionActor::queue_input`]).
        send_now: bool,
        /// Actor-authoritative admission and deferred fallback for terminal task wakes.
        admission: Option<TaskWakeAdmission>,
        tool_overrides_update: Option<echo_agent_sampling_types::ToolOverridesUpdate>,
        respond_to: oneshot::Sender<PromptTurnResult>,
        /// Optional oneshot fired after the user message has been appended to
        /// chat history and a persistence flush barrier has completed, before
        /// LLM inference begins. Used by callers that need to ensure
        /// `chat_history.jsonl` includes the prompt before trace snapshots or
        /// `session/load`.
        persist_ack: Option<oneshot::Sender<()>>,
        /// Pre-parsed prompt content blocks from `parse_prompt`, sent back to the
        /// caller so it can use the fully-rendered prompt for metadata.json without
        /// re-parsing. The session sends on this channel right after parsing.
        parsed_prompt_tx: Option<oneshot::Sender<ParsedPromptInfo>>,
    },
    SessionMode {
        session_mode: acp::SessionModeId,
        responds_to: oneshot::Sender<Result<(), acp::Error>>,
    },
    SetSessionModel {
        sampling_config: echo_agent_sampler::SamplerConfig,
        use_concise: bool,
        /// Models declare differing `model_family`s → compact (lossy) at switch end.
        is_family_switch: bool,
        /// When `false`, skip the system prompt rewrite (concise/default swap).
        /// Set to `false` for forked sessions so mid-session model switches
        /// cannot contaminate the inherited prompt configuration.
        apply_prompt_override: bool,
        /// When `true`, suppress the system prompt rewrite even though
        /// `apply_prompt_override` may be `true`. Set by the model-switch
        /// orchestrator immediately after a successful
        /// `RebuildAgentForDefinition` so the fresh harness's prompt
        /// (already installed by the rebuild handler) is not clobbered by
        /// the concise/default swap below.
        skip_prompt_rewrite: bool,
        /// Re-resolved auto-compact threshold for the new model. Computed
        /// by `MvpAgent` against the new model id so per-model remote settings
        /// and per-model user TOML overrides target the right model after a
        /// `/model` switch. The session actor stores this on
        /// `compaction.threshold_percent` (which is `Cell<u8>` so it can
        /// update without `&mut self`).
        auto_compact_threshold_percent: u8,
        responds_to: oneshot::Sender<Result<acp::ModelId, acp::Error>>,
    },
    /// Zero-turn harness rebuild: build a brand-new `Agent` from the
    /// session's `AgentRebuildSpec` and the new `AgentDefinition`,
    /// re-register MCP tools, swap the live `Agent`, rewrite the
    /// system message in the conversation, persist the new prompt
    /// artifacts, and update `active_agent_type`.
    ///
    /// Triggered by `MvpAgent::set_session_model` when the new model's
    /// `agent_type` differs from the session's current one and no user
    /// message has been sent yet (`turn_count == 0`).
    RebuildAgentForDefinition {
        definition: echo_agent_core::AgentDefinition,
        responds_to: oneshot::Sender<Result<(), acp::Error>>,
    },
    /// Override the model name and optionally inject extra HTTP headers
    /// into the session's sampling config.
    ///
    /// Unlike `SetSessionModel` (which requires a fully resolved `ModelEntry`
    /// and does NOT update `primaryModelId` in signals — the resolved model
    /// is already tracked via inference responses), this command also calls
    /// `set_primary_model()` so that signals report the override model
    /// rather than the agent-level default (e.g. `echo-agent-4.5`).
    ///
    /// Keeps the existing base_url, api_key, and other config — only changes
    /// the `model` field sent in the `x-echo-agent-model-override` header and merges
    /// any additional headers (e.g. `x-openrouter-api-key` for BYOK).
    ///
    /// Used to set model IDs (e.g. opaque third-party routing names) that are
    /// routing hints for the backend and don't need to exist in the
    /// agent's local model registry.
    OverrideModelName {
        model_name: String,
        extra_headers: indexmap::IndexMap<String, String>,
        /// Override the context window size for the new model. Without this,
        /// forked sessions inherit the source session's context window, causing
        /// auto-compact and context-usage signals to use the wrong threshold.
        context_window: Option<std::num::NonZeroU64>,
    },
    GetCurrentModel {
        responds_to: oneshot::Sender<String>,
    },
    GetCurrentPromptMode {
        responds_to: oneshot::Sender<PromptMode>,
    },
    GetModelMetadata {
        responds_to: oneshot::Sender<echo_agent_chat_state::ModelMetadata>,
    },
    /// Snapshot for `/session-info`.
    GetSessionInfo {
        responds_to: oneshot::Sender<SessionInfoData>,
    },
    /// Compacts the current session, saving on the context window
    CompactSession {
        /// Optional user-provided context to guide the compaction
        user_context: Option<String>,
        respond_to: oneshot::Sender<acp::Result<()>>,
    },
    /// Reload plugin hooks and registry mid-session.
    ReloadPlugins {
        registry: Option<std::sync::Arc<echo_agent_core::plugins::PluginRegistry>>,
    },
    /// Re-discover the session's own project hooks (`.echo-agent/hooks`,
    /// `.cursor/hooks.json`, …) mid-session, re-evaluating folder trust. Used by
    /// the interactive folder-trust grant so a granted folder's repo-local hooks
    /// start without a session restart (plugin-contributed hooks are handled by
    /// `ReloadPlugins`; this covers the non-plugin project hook registry).
    ReloadHooks,
    /// Re-discover skills from disk and update the session's skill baseline.
    RefreshSkillBaseline,
    /// Trigger an on-demand memory flush for this session.
    ///
    /// Calls `run_memory_flush("user_requested", None)` on the session actor.
    /// Returns an error if memory is not enabled for this session, or
    /// `Ok(true/false)` indicating whether a flush actually ran (false if
    /// another flush was already in progress).
    FlushMemory {
        respond_to: oneshot::Sender<acp::Result<bool>>,
    },
    /// Run manual long-term consolidation and report whether it wrote memory.
    DreamMemory {
        respond_to: oneshot::Sender<acp::Result<bool>>,
    },
    /// Auto-approve all permission prompts when `enabled`.
    SetYoloMode {
        enabled: bool,
    },
    /// Set auto permission mode (LLM classifier for non-fast-path tools).
    SetAutoMode {
        enabled: bool,
    },
    /// Atomically apply a mutually-exclusive permission mode and report the
    /// state actually accepted by feature gates and managed policy.
    SetPermissionMode {
        yolo_mode: bool,
        auto_mode: bool,
        respond_to: oneshot::Sender<(bool, bool)>,
    },
    ResetPermissionState,
    Rewind {
        request: RewindRequest,
        respond_to: oneshot::Sender<anyhow::Result<RewindResponse>>,
    },
    /// Out-of-band history repair (`echo.agent/session/repair`): fix tool-pairing
    /// violations (orphaned/displaced `ToolResult`s, duplicates, unanswered
    /// calls) that would otherwise 400 on every request. `dry_run` only
    /// reports. Refused while a turn is in flight.
    RepairHistory {
        dry_run: bool,
        respond_to: oneshot::Sender<
            anyhow::Result<echo_agent_chat_state::compaction_utils::HistoryRepairReport>,
        >,
    },
    GetRewindPoints {
        respond_to: oneshot::Sender<RewindPointsResponse>,
    },
    /// Local file-snapshot counts keyed by `prompt_index`, read straight from
    /// the file-state tracker (independent of the chat-state prompt index,
    /// which is empty in bridge mode). The bridge joins these onto the
    /// server's rewind points so `num_file_snapshots`/`has_file_changes` match
    /// what local-mode rewind reports.
    GetRewindFileCounts {
        respond_to: oneshot::Sender<std::collections::HashMap<usize, usize>>,
    },
    /// Reconcile the file-state rewind tracker after a bridge-mode
    /// `ConversationOnly` rewind that already committed server-side. Runs the
    /// same tracker bookkeeping `handle_rewind` does for `ConversationOnly`
    /// (merge the discarded prompts' file effects into the prior rewind point +
    /// persist), without reverting files or rewinding the conversation — both
    /// live server-side in bridge mode. Without it, bridge `ConversationOnly`
    /// commits leave orphaned local rewind points. Fire-and-forget (no ack):
    /// the server rewind has already committed, and the local truncation in
    /// `handle_rewind` is itself fire-and-forget, so the bridge does not block
    /// its response on the merge.
    ReconcileRewindTracker {
        target_prompt_index: usize,
    },
    /// EchoAgent extension session notification - client-side events to store in persistence
    EchoAgentSessionNotification {
        notification: SessionNotification,
    },
    /// Apply subagent usage into parent ledgers. Acks `()` once chat-state
    /// applied (prompt-attributed or session-only). Drop the oneshot on failure
    /// so the child treats the fold as not landed.
    RecordSubagentUsage {
        by_model: Vec<(String, echo_agent_chat_state::UsageTotals)>,
        parent_prompt_id: Option<String>,
        /// Nested subagent bill may under-count.
        incomplete: bool,
        respond_to: oneshot::Sender<()>,
    },
    /// Sticky incomplete for a parent prompt (or the live pin when `None`). Acks when marked.
    MarkSubagentUsageNotApplied {
        parent_prompt_id: Option<String>,
        respond_to: oneshot::Sender<()>,
    },
    /// Shared error-path usage attach (same policy as durable TurnCompleted).
    ErrorPathUsageFallback {
        prompt_id: Option<String>,
        respond_to: oneshot::Sender<Option<crate::extensions::notification::PromptUsage>>,
    },
    /// Persist the monotonic telemetry turn counter ("next trace turn") for the session.
    SetNextTraceTurn {
        next_trace_turn: u64,
        request_id: Option<String>,
    },
    /// Flush pending writes and copy the current session directory contents to memory.
    /// The caller can then tar.gz + upload to GCS (or similar).
    CopyFile {
        respond_to: oneshot::Sender<anyhow::Result<crate::session::persistence::SessionStateCopy>>,
    },
    /// Flush the replay buffer and persistence, then signal completion.
    /// Used during reconnect to ensure all buffered content is persisted before replay.
    FlushComplete {
        respond_to: oneshot::Sender<std::io::Result<()>>,
    },
    /// Update MCP servers for an existing session (used during reconnect or
    /// mid-session via the `echo.agent/session/update_mcp_servers` extension method).
    /// This replaces the current MCP server configuration and triggers re-initialization.
    ///
    /// The caller is notified via `respond_to` once MCP re-initialization
    /// completes (or immediately if configs are unchanged).
    UpdateMcpServers {
        mcp_servers: Vec<acp::McpServer>,
        respond_to: oneshot::Sender<Result<(), acp::Error>>,
    },
    /// Re-apply per-attachment policy (MCP init strategy, delivery tools)
    /// from a resident `session/load` whose request carried explicit
    /// `startupHints`. Spawn-time structural hints are NOT touched. Sent
    /// fire-and-forget alongside `UpdateMcpServers` on the reconnect rail.
    UpdateAttachPolicy {
        startup_hints: Box<crate::session::StartupHints>,
    },
    /// Toggle an MCP server on/off within the session actor's event loop.
    /// Atomic read-modify-write avoids TOCTOU races with background config
    /// refreshes that can change `mcp_state.configs` between a snapshot read
    /// and an `UpdateMcpServers` command.
    ToggleMcpServer {
        server_name: String,
        enabled: bool,
        /// Whether this live toggle should also update the user's durable
        /// enabled/disabled state. Internal optional capabilities set false.
        persist_enabled_state: bool,
        /// Fully-formed server config to add when re-enabling. Built by the
        /// caller via `merge_managed_mcp_servers` (with OAuth headers injected).
        /// `None` when disabling.
        server_config: Option<acp::McpServer>,
        respond_to: oneshot::Sender<Result<(), acp::Error>>,
    },
    /// Toggle a single MCP tool on/off within a server. The server stays connected;
    /// only the tool's registration in ToolBridge is affected.
    ToggleMcpTool {
        server_name: String,
        tool_name: String,
        enabled: bool,
        is_managed_gateway: bool,
        respond_to: oneshot::Sender<Result<(), acp::Error>>,
    },
    /// Read MCP status: which servers are configured, which clients are healthy, what tools.
    GetMcpStatus {
        respond_to: oneshot::Sender<crate::extensions::mcp::McpStatusSnapshot>,
    },
    GetManagedGatewayDisabledTools {
        respond_to:
            oneshot::Sender<std::collections::HashMap<String, std::collections::HashSet<String>>>,
    },
    /// Snapshot the session's live MCP client pool for subagent inheritance.
    SnapshotMcpPool {
        respond_to: oneshot::Sender<Option<crate::session::mcp_servers::SharedMcpPool>>,
    },
    /// Snapshot the session's client-registered hooks so a subagent inherits the same
    /// PreToolUse gate and observe hooks over the parent's connection.
    SnapshotClientHooks {
        respond_to: oneshot::Sender<crate::extensions::hooks::ClientHooks>,
    },
    /// Snapshot the session's resolved tool schema (same list the parent's own turn
    /// sends) so a verbatim-fork child can present a byte-identical tool prefix.
    SnapshotToolDefinitions {
        respond_to: oneshot::Sender<Vec<echo_agent_sampling_types::ToolSpec>>,
    },
    SnapshotTurnAuthority {
        respond_to: oneshot::Sender<crate::session::InheritedTurnAuthority>,
    },
    /// Replace the session's client-registered hooks. Sent on `load_session` reconnect to a
    /// live actor so a client can re-register (or clear) its hooks without a fresh session.
    SetClientHooks {
        hooks: crate::extensions::hooks::ClientHooks,
    },
    /// Client-driven MCP tool call outside the LLM loop.
    CallMcpTool {
        server_name: String,
        server_url: Option<String>,
        tool_name: String,
        arguments: serde_json::Value,
        respond_to: oneshot::Sender<Result<crate::extensions::mcp::McpCallResponse, String>>,
    },
    ReadMcpResource {
        server_name: String,
        uri: String,
        respond_to:
            oneshot::Sender<Result<crate::extensions::mcp::McpReadResourceResponse, String>>,
    },
    McpAuthStatus {
        respond_to: oneshot::Sender<Vec<crate::extensions::mcp::McpAuthStatusEntry>>,
    },
    McpAuthTrigger {
        server_name: String,
        respond_to: oneshot::Sender<Result<(), String>>,
    },
    RetryAuthRequiredServers {
        respond_to: oneshot::Sender<()>,
    },
    RefreshMcpSearchIndex,
    /// Move a foreground bash command to background by tool_call_id.
    /// Unblocks the agent loop so it can continue with the next action.
    BackgroundForegroundCommand {
        tool_call_id: String,
        respond_to: oneshot::Sender<bool>,
    },
    /// Kill a background task by task_id.
    /// Routes through the ToolBridge's TerminalBackend (lock-free, Arc-shared).
    KillBackgroundTask {
        task_id: String,
        source: echo_agent_tools::types::KillSource,
        respond_to: oneshot::Sender<Result<echo_agent_tools::types::KillOutcome, String>>,
    },
    DeleteScheduledTask {
        task_id: String,
        respond_to: oneshot::Sender<Result<bool, String>>,
    },
    /// List all background tasks.
    /// Routes through the ToolBridge's TerminalBackend.
    ListTasks {
        respond_to: oneshot::Sender<Option<Vec<echo_agent_tools::types::TaskSnapshot>>>,
    },
    /// Query whether the session has work in flight: a running turn
    /// (`running_task.is_some()`) **or** queued inputs
    /// (`pending_inputs` non-empty). Used by the leader's idle-unload decision
    /// on client disconnect (the no-evict keystone) to avoid unloading a
    /// session that still has pending work.
    IsBusy {
        respond_to: oneshot::Sender<bool>,
    },
    GetHooksList {
        respond_to: oneshot::Sender<echo_agent_hooks_plugins_types::HooksListResponse>,
    },
    /// Execute a hooks management action from the pager modal.
    HooksAction {
        action: echo_agent_hooks_plugins_types::HooksAction,
        respond_to: oneshot::Sender<echo_agent_hooks_plugins_types::ActionOutcome>,
    },
    /// Broadcast a plugin updates notification to the session.
    NotifyPluginUpdates {
        updates: Vec<(String, String, String)>,
    },
    /// Execute a plugins management action from the pager modal.
    PluginsAction {
        action: echo_agent_hooks_plugins_types::PluginsAction,
        respond_to: oneshot::Sender<echo_agent_hooks_plugins_types::ActionOutcome>,
    },
    /// This session's plugin registry, as served by `echo.agent/plugins/list`.
    PluginsList {
        respond_to:
            oneshot::Sender<Option<std::sync::Arc<echo_agent_core::plugins::PluginRegistry>>>,
    },
    /// Inject a notification (monitor event or bash task completion) into
    /// the session's notification queue. Notifications are idle-gated and
    /// batched by `maybe_drain_notifications`.
    InjectNotification {
        prompt_id: String,
        prompt_blocks: Vec<acp::ContentBlock>,
        priority: NotificationPriority,
        source: NotificationSource,
    },
    /// Drop queued / mid-turn-buffered `MonitorEvent` notifications for a
    /// task. Used when natural monitor exit already auto-woke via
    /// `TaskCompleted` so stdout + terminal pipeline events do not start a
    /// second `NotificationDrain` turn for the same completion.
    DropMonitorNotifications {
        task_id: String,
    },
    /// Dispatch a compat `Notification` hook (e.g. `task_complete`
    /// from the notification bridge, which does not go through `send_echo_agent_notification`).
    DispatchNotificationHook {
        notification_type: String,
        message: Option<String>,
        title: Option<String>,
        level: Option<String>,
    },
    /// Record background-task ids reparented from a harness-internal
    /// verifier/planner subagent's surviving dev server on subagent exit. The
    /// handler inserts them into `goal_turn_task_ids` whenever the goal harness
    /// is enabled (not gated on the racy `Active` status), so their late
    /// auto-wake completions are suppressed by `maybe_drain_notifications` even
    /// when a final verification round has already flipped the goal to Blocked.
    RecordGoalTurnTaskIds {
        task_ids: Vec<String>,
    },
    /// Remove a queued (not-yet-running) prompt from the authoritative prompt
    /// queue. Versioned + idempotent: a stale `expected_version`
    /// or an already-drained `id` is a no-op (the actor just re-broadcasts the
    /// current queue so the client reconciles). When `owner` is `Some`, the
    /// removal only applies if the item's attribution matches (edit authority:
    /// a client edits its own items).
    RemoveQueuedPrompt {
        id: String,
        expected_version: u64,
        owner: Option<String>,
    },
    /// Reorder the queued (not-yet-running) prompts to match `ordered_ids`.
    /// Ids not present in the live queue are ignored; queued items missing
    /// from `ordered_ids` keep their relative order at the back. The actor
    /// re-broadcasts the resulting queue. Idempotent.
    ReorderQueue {
        ordered_ids: Vec<String>,
    },
    /// Clear queued (not-yet-running) prompts. When `owner` is `Some`, only
    /// that client's items are cleared. The running turn is never touched.
    ClearQueue {
        owner: Option<String>,
    },
    /// Replace the text of a queued (not-yet-running) prompt in place
    /// (server-side LWW). Last write wins via the actor's
    /// serialized mailbox; the rebroadcast of `echo.agent/queue/changed` is the
    /// truth signal for every attached client. The original `owner`
    /// attribution is preserved; `editor` is recorded as the most recent
    /// editor (for future "alice edited this" UX). A missing id, or an id
    /// that names the currently-running turn, is a benign no-op.
    EditQueuedPrompt {
        id: String,
        new_text: String,
        editor: Option<String>,
    },
    /// Hold a queued prompt while a client edits it in the composer: skip it as
    /// a combine follower **and** block promote while it is the queue front.
    /// Released via [`Self::ReleaseEdit`], or cleared by edit / remove / interject.
    HoldEdit {
        id: String,
    },
    /// Release a previous [`Self::HoldEdit`]. Re-kicks the promoter so a
    /// previously held front can start when the session is idle.
    ReleaseEdit {
        id: String,
    },
    /// Atomically interject a queued (not-yet-running) prompt into the running
    /// turn: the actor removes it from `pending_inputs` and pushes
    /// its text into `pending_interjections` in a single mailbox op, so the
    /// in-flight turn merges it at the next safe point and the prompt can never
    /// both interject *and* later run as its own turn. Versioned + idempotent
    /// like [`RemoveQueuedPrompt`]. A benign no-op (the prompt stays queued and
    /// runs normally) when no turn is running, the id names the running turn, is
    /// stale/already-drained, or `owner` doesn't match. The rebroadcast of
    /// `echo.agent/queue/changed` is the truth signal for every attached client.
    InterjectQueuedPrompt {
        id: String,
        expected_version: u64,
        owner: Option<String>,
        /// Optional replacement text (client-edited row). When `Some`, it is
        /// interjected INSTEAD of the stored queue text, under the same single
        /// version check — edit + interject is one atomic op (a stale version
        /// no-ops the whole thing, edited text included).
        new_text: Option<String>,
    },
    Cancel(CancelOptions),
    Shutdown(ShutdownKind),
    /// Graceful shutdown whose final persistence barrier is acknowledged after
    /// SessionEnd. The actor stays alive on a write failure so the host can
    /// retain its Runtime and retry without rerunning teardown hooks.
    ShutdownChecked {
        completion: tokio::sync::watch::Sender<Option<Result<(), String>>>,
    },
    /// Force-trigger a feedback request notification for local client testing.
    /// Bypasses all heuristics, sampling, and cooldown checks.
    TriggerTestFeedback {
        tier: crate::session::feedback::FeedbackTier,
        mode: crate::session::feedback::FeedbackMode,
        respond_to: oneshot::Sender<anyhow::Result<acp::ExtResponse>>,
    },
    /// Persist a local feedback entry via the persistence actor. This ensures
    /// feedback.jsonl is written through the same channel as other session
    /// files and is included in GCS CopyFile snapshots.
    PersistFeedback(Box<crate::session::persistence::LocalFeedbackEntry>),
    AdvertiseCommands,
    GetWorkflowCatalogState {
        respond_to: oneshot::Sender<(bool, bool)>,
    },
    ListAvailableCommands {
        respond_to: oneshot::Sender<crate::session::slash_commands::ListCommandsResponse>,
    },
    /// Re-discover skills from disk, update the SkillManager baseline,
    /// and re-advertise slash commands to the client.
    ReloadSkills,
    /// Acknowledge only after the actor applied its refreshed skill baseline.
    ReloadSkillsChecked {
        respond_to: oneshot::Sender<usize>,
    },
    /// Dispatch session_start hook using the actor's loaded HookRegistry.
    DispatchSessionStartHook {
        /// "new" for brand new sessions, "load" for sessions loaded from disk.
        source: String,
    },
    /// Retrieve session context for enriching a feedback Slack notification.
    GetFeedbackContext {
        turn_number: Option<i64>,
        responds_to: oneshot::Sender<FeedbackContext>,
    },
    /// Retrieve the session's active agent type.
    ///
    /// Returns the name of the `AgentDefinition` that was used to initialize
    /// this session (or the most recent one applied via `handle_session_mode`).
    /// Used by `mvp_agent.set_session_model` to check whether a model's
    /// `agent_type` is compatible with the current session before switching.
    GetActiveAgent {
        responds_to: oneshot::Sender<Option<String>>,
    },
    /// Ask a side question without interrupting the current turn.
    /// The session snapshots the conversation context, makes a single
    /// tool-free model call, and returns the response text.
    SideQuestion {
        question: String,
        respond_to: oneshot::Sender<Result<String, SideQuestionError>>,
    },
    /// Generate a session recap (a short "where was I" summary) and broadcast
    /// it to clients via `SessionUpdate::SessionRecap`.
    ///
    /// Fire-and-forget: the session snapshots the conversation, makes a single
    /// tool-free model call, and emits the result for display only. It never
    /// mutates the conversation, so unlike `SideQuestion` it needs no reply
    /// channel — the answer travels back as a notification.
    Recap {
        /// `true` when triggered automatically on return-from-away,
        /// `false` for an explicit `/recap`.
        auto: bool,
    },
    /// Request an AI-generated shell command suggestion.
    ///
    /// The session actor builds a minimal prompt from `prefix` + `cwd`,
    /// calls the sampler with low temperature / max_tokens, and returns
    /// the suggested completion via `respond_to`.
    AISuggest {
        prefix: String,
        cwd: String,
        model_override: Option<String>,
        respond_to: oneshot::Sender<Option<String>>,
    },
    /// Predict the user's likely next prompt (tab autocomplete ghost text).
    ///
    /// Fired by the client after a turn completes. The session builds a
    /// compact text-only transcript of the recent conversation, makes one
    /// tool-free model call (default `echo-agent-build-0.1` when available via
    /// `model_override`, else the session model), sanitizes the output, and
    /// returns the predicted prompt via `respond_to`. Best-effort: any
    /// failure returns `None`.
    SuggestPrompt {
        model_override: Option<String>,
        respond_to: oneshot::Sender<Option<String>>,
    },
    /// Rewrite a raw memory note into well-structured markdown via a one-shot
    /// LLM call. The session uses `prepare_chat_completion()` with
    /// `echo-agent-build` model, low temperature, and capped output tokens.
    RewriteMemoryNote {
        raw_text: String,
        context_summary: String,
        respond_to: oneshot::Sender<Result<String, String>>,
    },
    /// Inject a user message into the active turn without canceling it.
    /// The text is queued in `pending_interjections` and drained at the
    /// next safe point in `process_conversation_turn`.  Fire-and-forget:
    /// no response channel needed since the command just pushes to a Mutex.
    Interject {
        text: String,
        expected_prompt_id: Option<String>,
        /// Client-minted id echoed back on the broadcast
        /// `echo.agent/session/interjection` so the originating pager can dedup its
        /// optimistic local block. `None` from older clients.
        id: Option<String>,
        /// Pasted images riding along with the interjection. Empty from
        /// text-only / older clients.
        images: Vec<acp::ImageContent>,
    },
    /// Trigger a model turn so the model can print a visible goal progress
    /// summary.  The goal orchestrator injects a system reminder into context
    /// (via `push_parent_reminder`) *before* sending this command.  The session
    /// actor queues a short synthetic prompt instructing the model to summarize
    /// the reminder, then calls `maybe_start_running_task`.  Fire-and-forget.
    GoalSummaryTurn {
        /// Short instruction appended as a verbatim user message.
        prompt_text: String,
    },
    WorkflowCompletionTurn {
        run_id: String,
        revision: u64,
    },
    /// Take turn messages from the chat state actor (proxied from mvp_agent).
    TakeTurnMessages {
        respond_to: oneshot::Sender<Option<echo_agent_chat_state::TurnCapture>>,
    },
    /// Drain the sealed harness trace turns (goal planner + verifier panels)
    /// from the chat state actor (proxied from mvp_agent). Routed through the
    /// session actor — like `TakeTurnMessages` — so the drain is ordered ahead
    /// of any subsequent turn's harness recording. Each `Vec` is one turn's
    /// synthetic `task` pairs, uploaded as its own sibling `turn_{N}` artifact.
    TakeHarnessTraceTurns {
        respond_to:
            oneshot::Sender<Vec<Vec<echo_agent_sampling_types::conversation::ConversationItem>>>,
    },
    /// Take and clear the session actor's out-of-band streaming-turn capture.
    ///
    /// Returns `Some(...)` when the model streamed reasoning or text in the
    /// current turn but the canonical assistant response never reached
    /// `chat_state` (user cancel mid-stream, sampler terminal failure such as
    /// `MaxTokensTruncation`). The consumer uploads it as
    /// `streaming_partial.json` for trace inspection; `chat_state` is never
    /// mutated by this command.
    ///
    /// `prompt_id` is passed so the handler can detect a race where a queued
    /// turn's `StreamStarted` reset the live slot to a different prompt
    /// between cancel and take. On mismatch the handler emits a
    /// `tracing::warn!` tripwire and returns `None`; there is no stash, so
    /// the tail-race data is dropped rather than misattributed.
    TakeStreamingCapture {
        prompt_id: String,
        #[allow(private_interfaces)]
        respond_to: oneshot::Sender<Option<crate::session::acp_session::StreamingTurnCapture>>,
    },
    /// Persist the current git HEAD commit and branch to summary.json.
    ///
    /// Sent at the end of each prompt turn so `--restore-code` sees the latest
    /// HEAD even when the `GitHeadChanged` filesystem watcher misses events.
    PersistGitHead {
        commit: Option<String>,
        branch: Option<String>,
    },
}
#[cfg(test)]
mod cancellation_category_meta_tests {
    use super::PromptCompletionKind;
    use echo_agent_session_events::types::CancellationCategory;
    /// Pins every `_meta.cancellationCategory` wire name: shipped clients
    /// string-match these, so a rename is a wire break the compiler can't see.
    #[test]
    fn pins_every_wire_name() {
        let cancelled = |category| PromptCompletionKind::Cancelled {
            category,
            context: None,
        };
        for (kind, expected) in [
            (
                cancelled(Some(CancellationCategory::HookDenied)),
                Some("HookDenied"),
            ),
            (
                cancelled(Some(CancellationCategory::MidTurnAbort)),
                Some("MidTurnAbort"),
            ),
            (
                cancelled(Some(CancellationCategory::PermissionRejected)),
                Some("PermissionRejected"),
            ),
            (
                cancelled(Some(CancellationCategory::PermissionCancelled)),
                Some("PermissionCancelled"),
            ),
            (cancelled(None), None),
            (
                PromptCompletionKind::MaxTurnsReached { limit: 1 },
                Some("max_turns_reached"),
            ),
            (
                PromptCompletionKind::StationarityEnded,
                Some("action_stationarity"),
            ),
            (PromptCompletionKind::Completed, None),
            (PromptCompletionKind::Rewound, None),
            (PromptCompletionKind::RemovedFromQueue, None),
        ] {
            assert_eq!(
                kind.cancellation_category_meta().as_deref(),
                expected,
                "{kind:?}"
            );
        }
    }
}
#[cfg(test)]
mod cancel_trigger_tests {
    use super::{CancelKind, CancelTrigger};
    #[test]
    fn classifies_every_trigger() {
        for (trigger, expected) in [
            (CancelTrigger::Esc, CancelKind::StopGesture),
            (CancelTrigger::CtrlC, CancelKind::StopGesture),
            (CancelTrigger::from_client("mouse"), CancelKind::StopGesture),
            (
                CancelTrigger::from_client("some_future_gesture"),
                CancelKind::StopGesture,
            ),
            (CancelTrigger::SendNow, CancelKind::Replace),
            (CancelTrigger::Shutdown, CancelKind::Teardown),
            (CancelTrigger::SessionClose, CancelKind::Teardown),
            (CancelTrigger::SessionDelete, CancelKind::Teardown),
        ] {
            assert_eq!(trigger.kind(), expected, "{trigger:?}");
        }
    }
}
