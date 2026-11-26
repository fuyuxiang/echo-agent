//! External OTEL schema v1: event names, attribute keys, typed records, and
//! the per-event mapping functions wired through the `telemetry_event!`
//! macro's `external = …` arm.
//!
//! `ExternalRecord` is a **closed, typed structure** — attribute keys are the
//! [`ExternalKey`] enum, not strings — so the compiler enumerates every
//! attribute that can possibly reach the wire. Three independent mechanisms
//! must be defeated to leak a new attribute: this enum, the pinned
//! [`EXTERNAL_ALLOWED_KEYS`] test, and the export-time validators in
//! [`super::redact`]. Telemetry-owner review gates this file (CODEOWNERS).

use crate::events;

/// Wire schema version, exported as resource attr `echo_agent_code.schema.version`.
/// Additive changes (new events/attrs) do not bump it; renames/removals do.
pub const SCHEMA_VERSION: &str = "v1";

/// Meter/logger instrumentation scope name.
pub const SCOPE_NAME: &str = "ai.echoagent.echo_agent_code";

// ─────────────────────────────────────────────────────────────────────────────
// Event names
// ─────────────────────────────────────────────────────────────────────────────

/// External event names (`event.name` on the OTLP log record). Closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumCount)]
pub enum ExternalEventName {
    SessionStart,
    SessionEnd,
    UserPrompt,
    TurnCompleted,
    ApiRequest,
    ApiError,
    ToolResult,
    ToolDecision,
    McpServerConnection,
    PermissionModeChanged,
    SkillActivated,
    PluginLoaded,
    Compaction,
    Subagent,
    Auth,
    InternalError,
    ModelSwitched,
    ContextualTip,
}

impl ExternalEventName {
    /// Stable wire name. These are a public schema commitment (documented in
    /// the monitoring-usage page); renames require a `SCHEMA_VERSION` bump.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "echo_agent_code.session_start",
            Self::SessionEnd => "echo_agent_code.session_end",
            Self::UserPrompt => "echo_agent_code.user_prompt",
            Self::TurnCompleted => "echo_agent_code.turn_completed",
            Self::ApiRequest => "echo_agent_code.api_request",
            Self::ApiError => "echo_agent_code.api_error",
            Self::ToolResult => "echo_agent_code.tool_result",
            Self::ToolDecision => "echo_agent_code.tool_decision",
            Self::McpServerConnection => "echo_agent_code.mcp_server_connection",
            Self::PermissionModeChanged => "echo_agent_code.permission_mode_changed",
            Self::SkillActivated => "echo_agent_code.skill_activated",
            Self::PluginLoaded => "echo_agent_code.plugin_loaded",
            Self::Compaction => "echo_agent_code.compaction",
            Self::Subagent => "echo_agent_code.subagent",
            Self::Auth => "echo_agent_code.auth",
            Self::InternalError => "echo_agent_code.internal_error",
            Self::ModelSwitched => "echo_agent_code.model_switched",
            Self::ContextualTip => "echo_agent_code.contextual_tip",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Attribute keys
// ─────────────────────────────────────────────────────────────────────────────

/// Every attribute key the external stream can attach to a log record. You
/// cannot attach an attribute the schema doesn't name. Adding a variant trips
/// the [`EXTERNAL_ALLOWED_KEYS`] pin test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumCount)]
pub enum ExternalKey {
    // Context / correlation (injected by emit.rs)
    SessionId,
    TurnNumber,
    PromptId,
    EventSequence,
    // Identity (injected per-record from the identity snapshot)
    UserId,
    OrganizationId,
    TeamId,
    DeploymentId,
    // Session lifecycle
    Model,
    PermissionMode,
    McpServerCount,
    PluginCount,
    SkillCount,
    HookCount,
    MemoryEnabled,
    IsGitRepo,
    ClientIdentifier,
    DurationSecs,
    TurnCount,
    ToolCallCount,
    CompactionCount,
    // Prompt / turn
    PromptLength,
    Prompt,
    ScreenMode,
    Outcome,
    DurationMs,
    ErrorCategory,
    CancellationCategory,
    // Model API
    StopReason,
    InputTokens,
    OutputTokens,
    ReasoningTokens,
    CacheReadTokens,
    StatusCode,
    // Tools
    ToolName,
    Success,
    FileExtension,
    ToolParameters,
    FilePath,
    Decision,
    AccessKind,
    Source,
    // MCP
    Status,
    TransportType,
    ToolCount,
    ErrorType,
    McpServerName,
    // Permission mode
    ToMode,
    Trigger,
    // Skills / plugins
    SkillSource,
    SkillName,
    InstallKind,
    PluginScope,
    PluginName,
    PluginVersion,
    // Compaction
    CompactionTrigger,
    CompactionOutcome,
    TokensBefore,
    TokensAfter,
    // Subagents
    Phase,
    SubagentType,
    // Auth
    AuthMethod,
    // Model switching
    FromModel,
    ToModel,
    ErrorCode,
    // Contextual tips
    Tip,
    Action,
}

impl ExternalKey {
    /// Stable wire name for this key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionId => "session.id",
            Self::TurnNumber => "turn_number",
            Self::PromptId => "prompt.id",
            Self::EventSequence => "event.sequence",
            Self::UserId => "user.id",
            Self::OrganizationId => "organization.id",
            Self::TeamId => "team.id",
            Self::DeploymentId => "deployment.id",
            Self::Model => "model",
            Self::PermissionMode => "permission_mode",
            Self::McpServerCount => "mcp_server_count",
            Self::PluginCount => "plugin_count",
            Self::SkillCount => "skill_count",
            Self::HookCount => "hook_count",
            Self::MemoryEnabled => "memory_enabled",
            Self::IsGitRepo => "is_git_repo",
            Self::ClientIdentifier => "client_identifier",
            Self::DurationSecs => "duration_secs",
            Self::TurnCount => "turn_count",
            Self::ToolCallCount => "tool_call_count",
            Self::CompactionCount => "compaction_count",
            Self::PromptLength => "prompt_length",
            Self::Prompt => "prompt",
            Self::ScreenMode => "screen_mode",
            Self::Outcome => "outcome",
            Self::DurationMs => "duration_ms",
            Self::ErrorCategory => "error_category",
            Self::CancellationCategory => "cancellation_category",
            Self::StopReason => "stop_reason",
            Self::InputTokens => "input_tokens",
            Self::OutputTokens => "output_tokens",
            Self::ReasoningTokens => "reasoning_tokens",
            Self::CacheReadTokens => "cache_read_tokens",
            Self::StatusCode => "status_code",
            Self::ToolName => "tool_name",
            Self::Success => "success",
            Self::FileExtension => "file_extension",
            Self::ToolParameters => "tool_parameters",
            Self::FilePath => "file_path",
            Self::Decision => "decision",
            Self::AccessKind => "access_kind",
            Self::Source => "source",
            Self::Status => "status",
            Self::TransportType => "transport_type",
            Self::ToolCount => "tool_count",
            Self::ErrorType => "error_type",
            Self::McpServerName => "mcp_server.name",
            Self::ToMode => "to_mode",
            Self::Trigger => "trigger",
            Self::SkillSource => "skill_source",
            Self::SkillName => "skill.name",
            Self::InstallKind => "install_kind",
            Self::PluginScope => "plugin_scope",
            Self::PluginName => "plugin_name",
            Self::PluginVersion => "plugin_version",
            Self::CompactionTrigger => "compaction_trigger",
            Self::CompactionOutcome => "compaction_outcome",
            Self::TokensBefore => "tokens_before",
            Self::TokensAfter => "tokens_after",
            Self::Phase => "phase",
            Self::SubagentType => "subagent_type",
            Self::AuthMethod => "auth_method",
            Self::FromModel => "from_model",
            Self::ToModel => "to_model",
            Self::ErrorCode => "error_code",
            Self::Tip => "tip",
            Self::Action => "action",
        }
    }
}

/// Every [`ExternalKey`] variant, for allowlist construction. The
/// `EnumCount` assertion below keeps it complete.
pub(crate) const ALL_KEYS: &[ExternalKey] = &[
    ExternalKey::SessionId,
    ExternalKey::TurnNumber,
    ExternalKey::PromptId,
    ExternalKey::EventSequence,
    ExternalKey::UserId,
    ExternalKey::OrganizationId,
    ExternalKey::TeamId,
    ExternalKey::DeploymentId,
    ExternalKey::Model,
    ExternalKey::PermissionMode,
    ExternalKey::McpServerCount,
    ExternalKey::PluginCount,
    ExternalKey::SkillCount,
    ExternalKey::HookCount,
    ExternalKey::MemoryEnabled,
    ExternalKey::IsGitRepo,
    ExternalKey::ClientIdentifier,
    ExternalKey::DurationSecs,
    ExternalKey::TurnCount,
    ExternalKey::ToolCallCount,
    ExternalKey::CompactionCount,
    ExternalKey::PromptLength,
    ExternalKey::Prompt,
    ExternalKey::ScreenMode,
    ExternalKey::Outcome,
    ExternalKey::DurationMs,
    ExternalKey::ErrorCategory,
    ExternalKey::CancellationCategory,
    ExternalKey::StopReason,
    ExternalKey::InputTokens,
    ExternalKey::OutputTokens,
    ExternalKey::ReasoningTokens,
    ExternalKey::CacheReadTokens,
    ExternalKey::StatusCode,
    ExternalKey::ToolName,
    ExternalKey::Success,
    ExternalKey::FileExtension,
    ExternalKey::ToolParameters,
    ExternalKey::FilePath,
    ExternalKey::Decision,
    ExternalKey::AccessKind,
    ExternalKey::Source,
    ExternalKey::Status,
    ExternalKey::TransportType,
    ExternalKey::ToolCount,
    ExternalKey::ErrorType,
    ExternalKey::McpServerName,
    ExternalKey::ToMode,
    ExternalKey::Trigger,
    ExternalKey::SkillSource,
    ExternalKey::SkillName,
    ExternalKey::InstallKind,
    ExternalKey::PluginScope,
    ExternalKey::PluginName,
    ExternalKey::PluginVersion,
    ExternalKey::CompactionTrigger,
    ExternalKey::CompactionOutcome,
    ExternalKey::TokensBefore,
    ExternalKey::TokensAfter,
    ExternalKey::Phase,
    ExternalKey::SubagentType,
    ExternalKey::AuthMethod,
    ExternalKey::FromModel,
    ExternalKey::ToModel,
    ExternalKey::ErrorCode,
    ExternalKey::Tip,
    ExternalKey::Action,
];

/// Compile-time completeness guard: a new `ExternalKey` variant that is not
/// listed in `ALL_KEYS` fails this assertion, so the allowlist can never
/// silently miss a key.
const _: () = assert!(ALL_KEYS.len() == <ExternalKey as strum::EnumCount>::COUNT);

/// The runtime allowlist the export-time validators enforce: exactly the wire
/// names of every [`ExternalKey`]. Pinned by an independent literal copy in
/// the test module (mirroring `otel_layer::redact::allowlist_contents_are_pinned`).
pub(crate) fn external_allowed_keys() -> &'static std::collections::HashSet<&'static str> {
    static SET: std::sync::LazyLock<std::collections::HashSet<&'static str>> =
        std::sync::LazyLock::new(|| ALL_KEYS.iter().map(|k| k.as_str()).collect());
    &SET
}

/// Keys allowed only when the matching content gate was on at init.
pub(crate) fn gate_for_key(key: &str) -> Option<Gate> {
    match key {
        "prompt" => Some(Gate::UserPrompts),
        "tool_parameters" | "file_path" | "skill.name" | "plugin_name" | "plugin_version" => {
            Some(Gate::ToolDetails)
        }
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Record structure
// ─────────────────────────────────────────────────────────────────────────────

/// Attribute value. No nested/array variants: the external schema is flat.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    Str(String),
    I64(i64),
    Bool(bool),
}

impl From<&str> for AttrValue {
    fn from(v: &str) -> Self {
        Self::Str(v.to_owned())
    }
}
impl From<String> for AttrValue {
    fn from(v: String) -> Self {
        Self::Str(v)
    }
}
impl From<i64> for AttrValue {
    fn from(v: i64) -> Self {
        Self::I64(v)
    }
}
impl From<u32> for AttrValue {
    fn from(v: u32) -> Self {
        Self::I64(v as i64)
    }
}
impl From<u64> for AttrValue {
    fn from(v: u64) -> Self {
        Self::I64(v as i64)
    }
}
impl From<usize> for AttrValue {
    fn from(v: usize) -> Self {
        Self::I64(v as i64)
    }
}
impl From<bool> for AttrValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

/// Content gate guarding a [`GatedAttr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// `OTEL_LOG_USER_PROMPTS`.
    UserPrompts,
    /// `OTEL_LOG_TOOL_DETAILS`.
    ToolDetails,
}

/// An attribute emitted only when its gate is on. When a gated attr shares a
/// key with a default attr (e.g. verbatim vs. sanitized `tool_name`), the
/// gated value **replaces** the default at emit time.
#[derive(Debug, Clone, PartialEq)]
pub struct GatedAttr {
    pub key: ExternalKey,
    pub gate: Gate,
    pub value: AttrValue,
}

/// Typed metric increment derived at mapping time. `emit.rs` converts these
/// into instrument `add()` calls with identity/cardinality attributes.
#[derive(Debug, Clone, PartialEq)]
pub enum MetricIncrement {
    /// `echo_agent_code.session.count`.
    SessionCount,
    /// `echo_agent_code.token.usage` — `token_type` ∈ `input|output|reasoning|cache_read`.
    TokenUsage {
        token_type: &'static str,
        model: String,
        count: u64,
    },
    /// `echo_agent_code.turn.count`.
    TurnCount {
        outcome: &'static str,
        model: String,
    },
    /// `echo_agent_code.tool.decision`.
    ToolDecision {
        tool_name: String,
        decision: &'static str,
        access_kind: &'static str,
        permission_mode: &'static str,
    },
    /// `echo_agent_code.tool.usage`.
    ToolUsage {
        tool_name: String,
        outcome: &'static str,
    },
    /// `echo_agent_code.error.count`.
    ErrorCount {
        error_category: String,
        model: String,
    },
    /// `echo_agent_code.startup.timeout` (`stuck_in` = phase label).
    StartupTimeout { stuck_in: String, auth_mode: String },
    /// `echo_agent_code.startup.phase_duration` (ms; `phase` = phase label).
    StartupPhaseDuration {
        phase: String,
        duration_ms: u64,
        outcome: String,
        auth_mode: String,
    },
    /// `echo_agent_code.startup.total` (ms from process start to a usable session).
    StartupTotal {
        duration_ms: u64,
        outcome: String,
        auth_mode: String,
    },
}

/// Curated external representation of one telemetry event: an optional log
/// record plus derived metric increments.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExternalRecord {
    /// Log-record event name; `None` for metric-only mappings (`SessionNew`).
    pub event: Option<ExternalEventName>,
    /// Attributes always emitted while the stream is active.
    pub attrs: Vec<(ExternalKey, AttrValue)>,
    /// Attributes emitted only when the matching gate is on.
    pub gated: Vec<GatedAttr>,
    /// Metric increments derived from this event.
    pub metrics: Vec<MetricIncrement>,
}

impl ExternalRecord {
    fn event(event: ExternalEventName) -> Self {
        Self {
            event: Some(event),
            ..Default::default()
        }
    }

    fn attr(mut self, key: ExternalKey, value: impl Into<AttrValue>) -> Self {
        self.attrs.push((key, value.into()));
        self
    }

    fn attr_opt(mut self, key: ExternalKey, value: Option<impl Into<AttrValue>>) -> Self {
        if let Some(v) = value {
            self.attrs.push((key, v.into()));
        }
        self
    }

    fn gated(mut self, key: ExternalKey, gate: Gate, value: impl Into<AttrValue>) -> Self {
        self.gated.push(GatedAttr {
            key,
            gate,
            value: value.into(),
        });
        self
    }

    fn metric(mut self, m: MetricIncrement) -> Self {
        self.metrics.push(m);
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Metric instrument schema (names / units / pinned attr keys)
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) const METRIC_SESSION_COUNT: &str = "echo_agent_code.session.count";
pub(crate) const METRIC_TOKEN_USAGE: &str = "echo_agent_code.token.usage";
pub(crate) const METRIC_TURN_COUNT: &str = "echo_agent_code.turn.count";
pub(crate) const METRIC_TOOL_DECISION: &str = "echo_agent_code.tool.decision";
pub(crate) const METRIC_TOOL_USAGE: &str = "echo_agent_code.tool.usage";
pub(crate) const METRIC_ERROR_COUNT: &str = "echo_agent_code.error.count";
pub(crate) const METRIC_STARTUP_PHASE_DURATION: &str = "echo_agent_code.startup.phase_duration";
pub(crate) const METRIC_STARTUP_TIMEOUT: &str = "echo_agent_code.startup.timeout";
pub(crate) const METRIC_STARTUP_TOTAL: &str = "echo_agent_code.startup.total";

/// Every attribute key that may appear on a metric data point: the
/// instrument-specific keys plus the per-increment identity/cardinality keys.
/// `prompt.id` is deliberately absent (events only — unbounded cardinality).
/// Enforced fail-closed by `ValidatingMetricExporter` (drops the export on
/// violation) and pinned by test.
pub(crate) const METRIC_ALLOWED_ATTR_KEYS: &[&str] = &[
    "type",
    "model",
    "outcome",
    "tool_name",
    "decision",
    "access_kind",
    "permission_mode",
    "error_category",
    "phase",
    "stuck_in",
    "auth_mode",
    "session.id",
    "app.version",
    "user.id",
    "organization.id",
    "team.id",
    "deployment.id",
];

// ─────────────────────────────────────────────────────────────────────────────
// Sanitizers
// ─────────────────────────────────────────────────────────────────────────────

/// First-party `client_identifier` allowlist (pinned by test). The underlying
/// field is externally controlled free text from ACP client metadata, so it
/// must never pass verbatim — unknown values collapse to `"other"`.
pub(crate) const KNOWN_CLIENT_IDENTIFIERS: &[&str] = &[
    "echo-agent-pager",
    "echo-agent-tui",
    "echo-agent-shell",
    "echo-agent-web",
    "echo-agent-desktop",
    "echo-agent-code-extension",
    "echo-agent-agent-sdk",
    "nebula",
    "zed",
];

pub(crate) fn sanitize_client_identifier(raw: &str) -> &'static str {
    KNOWN_CLIENT_IDENTIFIERS
        .iter()
        .find(|known| **known == raw)
        .copied()
        .unwrap_or("other")
}

/// `screen_mode` allowlist (pinned by test). Like `client_identifier`, the
/// underlying value is externally controlled free text from ACP prompt
/// metadata (`_meta.screenMode`), so it must never pass verbatim — unknown
/// values collapse to `"other"`.
pub(crate) const KNOWN_SCREEN_MODES: &[&str] = &["fullscreen", "inline", "minimal", "headless"];

pub(crate) fn sanitize_screen_mode(raw: &str) -> &'static str {
    KNOWN_SCREEN_MODES
        .iter()
        .find(|known| **known == raw)
        .copied()
        .unwrap_or("other")
}

/// Built-in tool names (closed enum surface in `echo-agent-tools`) that pass
/// verbatim through `tool_name` sanitization. Pinned by test; everything else
/// collapses (common MCP-name handling; fail-closed for the rest).
pub(crate) const BUILTIN_TOOL_NAMES: &[&str] = &[
    "read_file",
    "write",
    "search_replace",
    "edit_notebook",
    "delete_file",
    "run_terminal_cmd",
    "run_terminal_command",
    "get_task_output",
    "get_command_or_subagent_output",
    "get_terminal_command_output",
    "kill_task",
    "kill_command_or_subagent",
    "kill_terminal_command",
    "wait_commands_or_subagents",
    "grep",
    "glob",
    "list_dir",
    "skill",
    "ask_user_question",
    "enter_plan_mode",
    "exit_plan_mode",
    "todo_write",
    "task",
    "spawn_subagent",
    "web_search",
    "web_fetch",
    "lsp",
    "image_gen",
    "image_edit",
    "video_gen",
    "image_to_video",
    "reference_to_video",
    "monitor",
    "scheduler_create",
    "scheduler_delete",
    "scheduler_list",
    "search_tool",
    "use_tool",
    "memory_search",
    "memory_get",
    "update_goal",
];

/// Default `tool_name` reduction: built-ins pass verbatim, MCP-qualified
/// names (`server__tool`) collapse to `"mcp_tool"`, anything else collapses
/// to `"custom_tool"` (fail-closed — never export an unknown free-text name).
/// The verbatim name rides the `ToolDetails` gate.
pub(crate) fn sanitize_tool_name(raw: &str) -> &'static str {
    if let Some(known) = BUILTIN_TOOL_NAMES.iter().find(|n| **n == raw) {
        return known;
    }
    if raw.contains("__") {
        return "mcp_tool";
    }
    "custom_tool"
}

/// File-extension reduction for the always-on `file_extension` attribute:
/// extension only, lowercase, capped at
/// [`super::truncate::MAX_FILE_EXTENSION_LEN`] chars. Anything else about the
/// path is details-gated.
pub(crate) fn file_extension(path: &str) -> Option<String> {
    let ext = std::path::Path::new(path).extension()?.to_str()?;
    let ext: String = ext
        .chars()
        .take(super::truncate::MAX_FILE_EXTENSION_LEN)
        .collect::<String>()
        .to_ascii_lowercase();
    (!ext.is_empty()).then_some(ext)
}

// ─────────────────────────────────────────────────────────────────────────────
// Enum label helpers (stable snake_case, matching the serde representations)
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn outcome_label(o: events::Outcome) -> &'static str {
    match o {
        events::Outcome::Completed => "completed",
        events::Outcome::Cancelled => "cancelled",
        events::Outcome::Error => "error",
    }
}

pub(crate) fn access_kind_label(k: events::AccessKind) -> &'static str {
    match k {
        events::AccessKind::Read => "read",
        events::AccessKind::Edit => "edit",
        events::AccessKind::Bash => "bash",
        events::AccessKind::Grep => "grep",
        events::AccessKind::Mcp => "mcp",
        events::AccessKind::Web => "web",
    }
}

pub(crate) fn permission_mode_label(m: crate::enums::PermissionMode) -> &'static str {
    match m {
        crate::enums::PermissionMode::Ask => "ask",
        crate::enums::PermissionMode::AlwaysApprove => "always_approve",
        crate::enums::PermissionMode::Auto => "auto",
    }
}

fn tool_outcome_label(o: &echo_agent_session_events::types::ToolOutcome) -> &'static str {
    use echo_agent_session_events::types::ToolOutcome;
    match o {
        ToolOutcome::Success => "success",
        ToolOutcome::Error => "error",
        ToolOutcome::PermissionRejected => "permission_rejected",
        ToolOutcome::PermissionCancelled => "permission_cancelled",
        ToolOutcome::Followup => "followup",
        ToolOutcome::HookDenied => "hook_denied",
        ToolOutcome::InvalidTool => "invalid_tool",
        ToolOutcome::Cancelled => "cancelled",
    }
}

fn mcp_transport_label(t: events::McpTransport) -> &'static str {
    match t {
        events::McpTransport::Stdio => "stdio",
        events::McpTransport::Sse => "sse",
        events::McpTransport::Http => "http",
    }
}

fn plan_trigger_label(t: events::PlanModeTrigger) -> &'static str {
    match t {
        events::PlanModeTrigger::User => "user",
        events::PlanModeTrigger::Tool => "tool",
    }
}

fn contextual_tip_kind_label(t: events::ContextualTipKind) -> &'static str {
    match t {
        events::ContextualTipKind::Undo => "undo",
        events::ContextualTipKind::PlanMode => "plan_mode",
        events::ContextualTipKind::ImageInput => "image_input",
        events::ContextualTipKind::SendNow => "send_now",
        events::ContextualTipKind::SmallScreen => "small_screen",
        events::ContextualTipKind::WordSelect => "word_select",
        events::ContextualTipKind::SshWrap => "ssh_wrap",
    }
}

fn contextual_tip_action_label(a: events::ContextualTipAction) -> &'static str {
    match a {
        events::ContextualTipAction::Shown => "shown",
        events::ContextualTipAction::Accepted => "accepted",
    }
}

fn yolo_trigger_label(t: events::YoloTrigger) -> &'static str {
    match t {
        events::YoloTrigger::SlashCommand => "slash_command",
        events::YoloTrigger::ClientMeta => "client_meta",
        events::YoloTrigger::Pager => "pager",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Mapping functions (`telemetry_event!(…, external = …)` targets)
// ─────────────────────────────────────────────────────────────────────────────

/// `SessionHarness` → `echo_agent_code.session_start`. Emitted from a spawn outside
/// `TELEMETRY_CTX`, so `session.id` is mapped from the struct's own field.
pub fn map_session_start(ev: &events::SessionHarness) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::SessionStart)
            .attr(ExternalKey::SessionId, ev.session_id.as_str())
            .attr(ExternalKey::Model, ev.model_id.as_str())
            .attr(
                ExternalKey::PermissionMode,
                permission_mode_label(ev.permission_mode),
            )
            .attr(ExternalKey::McpServerCount, ev.mcp_server_names.len())
            .attr(ExternalKey::PluginCount, ev.plugin_names.len())
            .attr(ExternalKey::SkillCount, ev.skill_names.len())
            .attr(ExternalKey::HookCount, ev.hook_names.len())
            .attr(ExternalKey::MemoryEnabled, ev.memory_enabled)
            .attr(ExternalKey::IsGitRepo, ev.is_git_repo)
            .attr_opt(
                ExternalKey::ClientIdentifier,
                ev.client_identifier
                    .as_deref()
                    .map(sanitize_client_identifier),
            ),
    )
}

/// `SessionNew` → `echo_agent_code.session.count` (metric only; the flagship
/// `session_start` log record comes from the richer `SessionHarness`).
pub fn map_session_new(ev: &events::SessionNew) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::default()
            .attr(ExternalKey::SessionId, ev.session_id.as_str())
            .metric(MetricIncrement::SessionCount),
    )
}

/// `SessionEnded` → `echo_agent_code.session_end`.
pub fn map_session_end(ev: &events::SessionEnded) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::SessionEnd)
            .attr(ExternalKey::DurationSecs, ev.duration_secs)
            .attr(ExternalKey::TurnCount, ev.turn_count)
            .attr(ExternalKey::ToolCallCount, ev.tool_call_count)
            .attr(ExternalKey::CompactionCount, ev.compaction_count)
            .attr(ExternalKey::Model, ev.model_id.as_str()),
    )
}

/// `PromptSubmitted` → `echo_agent_code.user_prompt`. Prompt text rides the
/// `UserPrompts` gate (60 KB cap applied at emit time).
pub fn map_user_prompt(ev: &events::PromptSubmitted) -> Option<ExternalRecord> {
    let mut rec = ExternalRecord::event(ExternalEventName::UserPrompt)
        .attr(ExternalKey::PromptLength, ev.prompt_length)
        .attr(ExternalKey::Model, ev.model_id.as_str())
        .attr_opt(
            ExternalKey::ScreenMode,
            ev.screen_mode.as_deref().map(sanitize_screen_mode),
        );
    if let Some(text) = ev.prompt_text.as_deref() {
        rec = rec.gated(ExternalKey::Prompt, Gate::UserPrompts, text);
    }
    Some(rec)
}

/// `TurnCompleted` → `echo_agent_code.turn_completed` + `turn.count`
/// (+ `error.count` on error outcomes).
pub fn map_turn_completed(ev: &events::TurnCompleted) -> Option<ExternalRecord> {
    let outcome = outcome_label(ev.outcome);
    let mut rec = ExternalRecord::event(ExternalEventName::TurnCompleted)
        .attr(ExternalKey::Outcome, outcome)
        .attr(ExternalKey::DurationMs, ev.duration_ms)
        .attr(ExternalKey::ToolCallCount, ev.tool_call_count)
        .attr(ExternalKey::Model, ev.model_id.as_str())
        .attr_opt(ExternalKey::ErrorCategory, ev.error_category.as_deref())
        .attr_opt(
            ExternalKey::CancellationCategory,
            ev.cancellation_category.as_deref(),
        )
        .metric(MetricIncrement::TurnCount {
            outcome,
            model: ev.model_id.clone(),
        });
    if matches!(ev.outcome, events::Outcome::Error) {
        rec = rec.metric(MetricIncrement::ErrorCount {
            error_category: ev
                .error_category
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
            model: ev.model_id.clone(),
        });
    }
    Some(rec)
}

/// `ModelResponseReceived` → `echo_agent_code.api_request` + `token.usage`.
pub fn map_api_request(ev: &events::ModelResponseReceived) -> Option<ExternalRecord> {
    let mut rec = ExternalRecord::event(ExternalEventName::ApiRequest)
        .attr(ExternalKey::Model, ev.model_id.as_str())
        .attr(ExternalKey::DurationMs, ev.duration_ms)
        .attr_opt(ExternalKey::StopReason, ev.stop_reason.as_deref())
        .attr_opt(ExternalKey::InputTokens, ev.prompt_tokens)
        .attr_opt(ExternalKey::OutputTokens, ev.completion_tokens)
        .attr_opt(ExternalKey::ReasoningTokens, ev.reasoning_tokens)
        .attr_opt(ExternalKey::CacheReadTokens, ev.cached_prompt_tokens);
    for (token_type, count) in [
        ("input", ev.prompt_tokens),
        ("output", ev.completion_tokens),
        ("reasoning", ev.reasoning_tokens),
        ("cache_read", ev.cached_prompt_tokens),
    ] {
        if let Some(count) = count.filter(|c| *c > 0) {
            rec = rec.metric(MetricIncrement::TokenUsage {
                token_type,
                model: ev.model_id.clone(),
                count: count as u64,
            });
        }
    }
    Some(rec)
}

/// `RateLimitHit` → `echo_agent_code.api_error` (`error_category = rate_limit`).
/// No `error.count` increment: a rate-limited turn (retries exhausted) also
/// ends in `TurnCompleted{outcome: Error}`, which is the single increment
/// source — incrementing here too would double-count the failure.
pub fn map_rate_limit_hit(ev: &events::RateLimitHit) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::ApiError)
            .attr(ExternalKey::ErrorCategory, "rate_limit")
            .attr(ExternalKey::Model, ev.model_id.as_str()),
    )
}

/// `ApiError` → `echo_agent_code.api_error`. Category/class enums only — no
/// message text. Deliberately **no** `error.count` increment: `ApiError` is
/// emitted alongside `TurnCompleted{outcome: Error}` for the same failure,
/// and the metric's increment sources are exactly `TurnCompleted{Error}` +
/// `RateLimitHit` (per the design's metric table) — adding one here would
/// double-count every failed turn.
pub fn map_api_error(ev: &events::ApiError) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::ApiError)
            .attr(ExternalKey::ErrorCategory, ev.error_category.as_str())
            .attr(ExternalKey::Model, ev.model_id.as_str())
            .attr_opt(ExternalKey::StatusCode, ev.status_code.map(|c| c as i64))
            .attr_opt(ExternalKey::DurationMs, ev.duration_ms),
    )
}

/// `ToolCallCompleted` → `echo_agent_code.tool_result` + `tool.usage`.
pub fn map_tool_result(ev: &events::ToolCallCompleted) -> Option<ExternalRecord> {
    use echo_agent_session_events::types::ToolOutcome;
    let sanitized = sanitize_tool_name(&ev.tool_name);
    let outcome = tool_outcome_label(&ev.outcome);
    let mut rec = ExternalRecord::event(ExternalEventName::ToolResult)
        .attr(ExternalKey::ToolName, sanitized)
        .attr(ExternalKey::Outcome, outcome)
        .attr(
            ExternalKey::Success,
            matches!(ev.outcome, ToolOutcome::Success),
        )
        .attr(ExternalKey::DurationMs, ev.duration_ms)
        .gated(
            ExternalKey::ToolName,
            Gate::ToolDetails,
            ev.tool_name.as_str(),
        )
        .metric(MetricIncrement::ToolUsage {
            tool_name: sanitized.to_owned(),
            outcome,
        });
    if let Some(path) = ev.file_path.as_deref() {
        rec = rec
            .attr_opt(ExternalKey::FileExtension, file_extension(path))
            .gated(ExternalKey::FilePath, Gate::ToolDetails, path);
    }
    if let Some(params) = ev.parameters.as_ref() {
        rec = rec.gated(
            ExternalKey::ToolParameters,
            Gate::ToolDetails,
            super::truncate::reduce_tool_input(params),
        );
    }
    Some(rec)
}

/// `PermissionDecisionPayload` → `echo_agent_code.tool_decision` + `tool.decision`.
pub fn map_tool_decision(ev: &events::PermissionDecisionPayload) -> Option<ExternalRecord> {
    let sanitized = sanitize_tool_name(&ev.tool_name);
    let decision = ev.decision.as_str();
    let access_kind = access_kind_label(ev.access_kind);
    let permission_mode = permission_mode_label(ev.permission_mode);
    Some(
        ExternalRecord::event(ExternalEventName::ToolDecision)
            .attr(ExternalKey::ToolName, sanitized)
            .attr(ExternalKey::Decision, decision)
            .attr(ExternalKey::AccessKind, access_kind)
            .attr(ExternalKey::PermissionMode, permission_mode)
            .attr_opt(ExternalKey::Source, ev.source.as_deref())
            .gated(
                ExternalKey::ToolName,
                Gate::ToolDetails,
                ev.tool_name.as_str(),
            )
            .metric(MetricIncrement::ToolDecision {
                tool_name: sanitized.to_owned(),
                decision,
                access_kind,
                permission_mode,
            }),
    )
}

/// `McpServerConnected` → `echo_agent_code.mcp_server_connection` (`status=connected`).
/// Server name collapses to `"mcp_server"` by default (name is details-gated).
pub fn map_mcp_server_connected(ev: &events::McpServerConnected) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::McpServerConnection)
            .attr(ExternalKey::Status, "connected")
            .attr(
                ExternalKey::TransportType,
                mcp_transport_label(ev.transport),
            )
            .attr(ExternalKey::DurationMs, ev.duration_ms)
            .attr(ExternalKey::ToolCount, ev.tool_count)
            .attr(ExternalKey::McpServerName, "mcp_server")
            .gated(
                ExternalKey::McpServerName,
                Gate::ToolDetails,
                ev.server_name.as_str(),
            ),
    )
}

/// `McpServerFailed` → `echo_agent_code.mcp_server_connection` (`status=failed`).
pub fn map_mcp_server_failed(ev: &events::McpServerFailed) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::McpServerConnection)
            .attr(ExternalKey::Status, "failed")
            .attr(ExternalKey::ErrorType, ev.error_type.as_str())
            .attr(ExternalKey::DurationMs, ev.duration_ms)
            .attr(ExternalKey::McpServerName, "mcp_server")
            .gated(
                ExternalKey::McpServerName,
                Gate::ToolDetails,
                ev.server_name.as_str(),
            ),
    )
}

/// `PlanModeToggled` → `echo_agent_code.permission_mode_changed`.
pub fn map_plan_mode_toggled(ev: &events::PlanModeToggled) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::PermissionModeChanged)
            .attr(
                ExternalKey::ToMode,
                if ev.enabled { "plan" } else { "default" },
            )
            .attr(ExternalKey::Trigger, plan_trigger_label(ev.trigger)),
    )
}

/// `ContextualTip` → `echo_agent_code.contextual_tip`. Labels only (no user
/// content), so nothing here is gated.
pub fn map_contextual_tip(ev: &events::ContextualTip) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::ContextualTip)
            .attr(ExternalKey::Tip, contextual_tip_kind_label(ev.tip))
            .attr(ExternalKey::Action, contextual_tip_action_label(ev.action)),
    )
}

/// `YoloToggled` → `echo_agent_code.permission_mode_changed`.
pub fn map_yolo_toggled(ev: &events::YoloToggled) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::PermissionModeChanged)
            .attr(
                ExternalKey::ToMode,
                if ev.enabled {
                    "bypass_permissions"
                } else {
                    "default"
                },
            )
            .attr(ExternalKey::Trigger, yolo_trigger_label(ev.trigger)),
    )
}

/// `SkillDispatched` → `echo_agent_code.skill_activated`. Skill names are details-gated; source and trigger export by default.
pub fn map_skill_activated(ev: &events::SkillDispatched) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::SkillActivated)
            .attr(
                ExternalKey::SkillSource,
                if ev.plugin_source.is_some() {
                    "plugin"
                } else {
                    "local"
                },
            )
            .attr(ExternalKey::Trigger, <&'static str>::from(ev.trigger))
            .gated(
                ExternalKey::SkillName,
                Gate::ToolDetails,
                ev.skill_name.as_str(),
            ),
    )
}

/// `PluginInstalled` → `echo_agent_code.plugin_loaded`.
pub fn map_plugin_installed(ev: &events::PluginInstalled) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::PluginLoaded)
            .attr(ExternalKey::InstallKind, ev.install_kind.as_str())
            .attr(ExternalKey::Success, ev.success)
            .attr_opt(ExternalKey::ErrorCategory, ev.error_category.as_deref()),
    )
}

/// `PluginUsed` → `echo_agent_code.plugin_loaded` (plugin name details-gated).
pub fn map_plugin_used(ev: &events::PluginUsed) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::PluginLoaded)
            .attr(ExternalKey::Success, ev.success)
            .gated(
                ExternalKey::PluginName,
                Gate::ToolDetails,
                ev.plugin_name.as_str(),
            ),
    )
}

/// `CompactionCompleted` → `echo_agent_code.compaction`.
pub fn map_compaction(ev: &events::CompactionCompleted) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::Compaction)
            .attr(ExternalKey::DurationMs, ev.duration_ms)
            .attr(ExternalKey::TokensBefore, ev.tokens_before)
            .attr(ExternalKey::TokensAfter, ev.tokens_after)
            .attr_opt(ExternalKey::Model, ev.model_id.as_deref()),
    )
}

/// `CompactionTriggered` → `echo_agent_code.compaction` trigger attrs ride on the
/// completion event instead (one event per compaction); not mapped.
/// `SubagentLaunched` → `echo_agent_code.subagent` (`phase=launched`).
pub fn map_subagent_launched(ev: &events::SubagentLaunched) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::Subagent)
            .attr(ExternalKey::Phase, "launched")
            .attr(ExternalKey::SubagentType, ev.subagent_type.as_str()),
    )
}

/// `SubagentCompleted` → `echo_agent_code.subagent` (`phase=completed`).
pub fn map_subagent_completed(ev: &events::SubagentCompleted) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::Subagent)
            .attr(ExternalKey::Phase, "completed")
            .attr(ExternalKey::Outcome, outcome_label(ev.outcome))
            .attr(ExternalKey::DurationMs, ev.duration_ms),
    )
}

/// `Login` → `echo_agent_code.auth`.
pub fn map_auth(ev: &events::Login) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::Auth)
            .attr(ExternalKey::AuthMethod, ev.auth_method.as_str()),
    )
}

/// `InternalError` → `echo_agent_code.internal_error`. Error class only — no
/// message, no location (user decision, RQ5).
pub fn map_internal_error(ev: &events::InternalError) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::InternalError)
            .attr(ExternalKey::ErrorType, ev.error_type.as_str()),
    )
}

/// `AgentConnect` → phase histogram + timeout counter (no external log event).
pub fn map_agent_connect(ev: &events::AgentConnect) -> Option<ExternalRecord> {
    let mut rec = ExternalRecord::default();
    for (phase, duration_ms) in &ev.phase_durations_ms {
        rec = rec.metric(MetricIncrement::StartupPhaseDuration {
            phase: phase.clone(),
            duration_ms: *duration_ms,
            outcome: ev.outcome.label().to_string(),
            auth_mode: ev.auth_mode.label().to_string(),
        });
    }
    if ev.outcome == crate::startup::StartupOutcome::Timeout {
        let stuck = ev.stuck_in.clone().unwrap_or_else(|| "unknown".to_owned());
        rec = rec.metric(MetricIncrement::StartupTimeout {
            stuck_in: stuck,
            auth_mode: ev.auth_mode.label().to_string(),
        });
    }
    Some(rec)
}

/// `StartupCompleted` → the total histogram (no external log event).
pub fn map_startup_completed(ev: &events::StartupCompleted) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::default().metric(MetricIncrement::StartupTotal {
            duration_ms: ev.total_ms,
            outcome: ev.outcome.label().to_string(),
            auth_mode: ev.auth_mode.label().to_string(),
        }),
    )
}

/// `ModelSwitched` → `echo_agent_code.model_switched`.
pub fn map_model_switched(ev: &events::ModelSwitched) -> Option<ExternalRecord> {
    Some(
        ExternalRecord::event(ExternalEventName::ModelSwitched)
            .attr(ExternalKey::SessionId, ev.session_id.as_str())
            .attr(ExternalKey::FromModel, ev.previous_model_id.as_str())
            .attr(ExternalKey::ToModel, ev.new_model_id.as_str())
            .attr(ExternalKey::Success, ev.success)
            .attr_opt(ExternalKey::ErrorCode, ev.error_code.as_deref()),
    )
}
