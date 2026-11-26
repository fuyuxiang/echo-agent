//! Telemetry event structs. Every struct needs a `telemetry_event!` binding.
//! `log_event` auto-injects `session_id`/`turn_number` and reserves every key in `client::RESERVED_EVENT_KEYS`.
//!
//! These structs were extracted from `echo-agent-runtime` so they can be
//! reused across binaries (TUI, sampler) without dragging the shell HTTP /
//! product-analytics client along.

use serde::Serialize;

use super::enums::PermissionMode;
pub use super::enums::PrCreationSource;

mod permission_analytics;
pub use permission_analytics::*;

/// Binds a product event name to a struct. Implement via `telemetry_event!` below.
pub trait TelemetryEvent: Serialize + Send + 'static {
    const NAME: &'static str;

    /// Curated external-OTEL representation (see [`crate::external`]).
    /// Default: not exported externally. Override via the macro's
    /// `external = …` arm — the mapping functions live together in
    /// `external/schema.rs` so the whole wire schema is one reviewable file.
    fn external_record(&self) -> Option<crate::external::schema::ExternalRecord> {
        None
    }
}

macro_rules! telemetry_event {
    ($struct:path, $name:literal) => {
        impl $crate::events::TelemetryEvent for $struct {
            const NAME: &'static str = $name;
        }
    };
    ($struct:path, $name:literal, external = $mapper:path) => {
        impl $crate::events::TelemetryEvent for $struct {
            const NAME: &'static str = $name;

            fn external_record(&self) -> Option<$crate::external::schema::ExternalRecord> {
                $mapper(self)
            }
        }
    };
}

// ─────────────────────────────────────────────────────────────────────────────
// Typed enum fields (compile-time exhaustiveness replaces String comments)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PlanModeTrigger {
    User,
    Tool,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ContextualTipKind {
    Undo,
    PlanMode,
    ImageInput,
    SendNow,
    SmallScreen,
    /// Double-click fold/nav path → tip to enable Word select in settings.
    WordSelect,
    /// SSH session without `echoagent wrap` → tip to wrap the ssh command locally.
    SshWrap,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ContextualTipAction {
    Shown,
    Accepted,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PromptSuggestionAction {
    /// A suggestion loaded and rendered as ghost text in the prompt input.
    Shown,
    /// The user accepted the ghost text (Tab / Right arrow).
    Accepted,
    /// The user explicitly dismissed the ghost text (Esc).
    Dismissed,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum YoloTrigger {
    SlashCommand,
    ClientMeta,
    Pager,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    Read,
    Edit,
    Bash,
    Grep,
    Mcp,
    Web,
}

/// Outcome of one CLI binary install/update attempt.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliUpdateOutcome {
    Success,
    Failed,
}

/// Why a CLI binary install/update failed. Smoke kinds are post-download
/// `--version` checks; other kinds cover download/activation/misc errors.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliUpdateErrorKind {
    SmokeTimeout,
    SmokeNonzero,
    SmokeSpawn,
    Download,
    Activate,
    Other,
}

/// Installer that performed the attempt. Wire values match the persisted
/// installer strings; `Other` bounds unknown persisted values.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliUpdateInstaller {
    #[serde(rename = "npm")]
    Npm,
    #[serde(rename = "gh-release")]
    GhRelease,
    #[serde(rename = "internal")]
    Internal,
    #[serde(rename = "other")]
    Other,
}

impl CliUpdateInstaller {
    /// Kept next to the wire values above so they cannot drift apart.
    pub fn from_installer_str(installer: &str) -> Self {
        match installer {
            "npm" => Self::Npm,
            "gh-release" => Self::GhRelease,
            "internal" => Self::Internal,
            _ => Self::Other,
        }
    }
}

/// What kicked off the install/update. Travels across the process boundary
/// as `--trigger=<value>`; [`CliUpdateTrigger::as_str`] and `FromStr` are
/// the one rendering (round-trip pinned with the wire values in tests).
///
/// Volume caveat: one-shot `echoagent update` resolves telemetry from disk+env
/// only, so `user_command` under-reports relative to the in-process
/// `leader_converge` — the triggers are not directly comparable.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliUpdateTrigger {
    /// A human ran `echoagent update` or accepted an update prompt.
    UserCommand,
    /// TUI/stdio launch check spawned a detached update child.
    AutoBackground,
    /// The leader daemon's hourly in-process converge.
    LeaderConverge,
}

impl CliUpdateTrigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserCommand => "user_command",
            Self::AutoBackground => "auto_background",
            Self::LeaderConverge => "leader_converge",
        }
    }
}

impl std::str::FromStr for CliUpdateTrigger {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user_command" => Ok(Self::UserCommand),
            "auto_background" => Ok(Self::AutoBackground),
            "leader_converge" => Ok(Self::LeaderConverge),
            other => Err(format!("unknown update trigger: {other}")),
        }
    }
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOutcome {
    Allow,
    Deny,
    Cancelled,
    Followup,
}

impl PermissionOutcome {
    /// Stable snake_case label matching the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Cancelled => "cancelled",
            Self::Followup => "followup",
        }
    }
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    Manual,
    Auto,
}

/// Mixpanel mode label. Detail is omitted so `segments` never includes it.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionModeLabel {
    Summary,
    Transcript,
    Segments,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TwoPassOutcome {
    /// Policy or product-exception off (cursor, subagents).
    Disabled,
    /// Armed, fell back to single-pass.
    Miss,
    /// Pass-2 summary applied.
    Used,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Cancelled,
    Error,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum HookOutcome {
    Success,
    Error,
    Blocked,
}

/// Outcome of one `PreToolUse` gate callback. Only `Denied` blocks the tool; the rest
/// (including the `TimedOut`/`TransportError`/`Malformed`/`UnknownDecision` fail-open paths) let it run.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ClientHookGateOutcome {
    Denied,
    Proceeded,
    TimedOut,
    TransportError,
    Malformed,
    UnknownDecision,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    Stdio,
    Sse,
    Http,
}

pub use super::enums::McpInitStrategy as McpStrategy;

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum McpErrorType {
    Connection,
    Auth,
    Protocol,
    Timeout,
    SpawnFailed,
    HandshakeFailed,
}

impl McpErrorType {
    /// Stable snake_case label matching the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Auth => "auth",
            Self::Protocol => "protocol",
            Self::Timeout => "timeout",
            Self::SpawnFailed => "spawn_failed",
            Self::HandshakeFailed => "handshake_failed",
        }
    }
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PlanModeState {
    Inactive,
    Pending,
    Active,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRetrievalMode {
    Disabled,
    FtsOnly,
    Hybrid,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum MemoryFlushTrigger {
    SlashCommand,
    Interval,
    PreCompaction,
    UserRequested,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Image,
    Video,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PagerCommandSource {
    Builtin,
    NonBuiltin,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum InstallKind {
    Git,
    Local,
}

impl InstallKind {
    /// Stable snake_case label matching the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Local => "local",
        }
    }
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PluginSource {
    LocalPath,
    Git,
}

// ─────────────────────────────────────────────────────────────────────────────
// Auth
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct Login {
    pub auth_method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// The login-method picker was shown. `trigger` is "startup", "logout", or
/// "mid_session".
#[derive(Serialize)]
pub struct LoginPickerShown {
    pub trigger: String,
}

/// A login method was chosen from the picker. `method` is "echoagent" or "api_key";
/// `mode` is "device", "loopback", or "api_key".
#[derive(Serialize)]
pub struct LoginMethodChosen {
    pub method: String,
    pub mode: String,
}

/// A login flow completed successfully. `method` is "echoagent" or "api_key";
/// `mode` is the resolved auth mode; `mid_session` is true for `/login`/401
/// re-auth (as opposed to the startup/logout flow).
#[derive(Serialize)]
pub struct LoginCompleted {
    pub method: String,
    pub mode: String,
    pub duration_ms: u64,
    pub mid_session: bool,
}

/// How a login attempt's HTTP request failed.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoginFailureKind {
    /// `is_connect`: a dead TCP connect *or* a TLS handshake killed
    /// mid-flight. `os_error` tells them apart.
    TransportConnect,
    /// TLS certificate rejected for an untrusted issuer (e.g. an uninstalled proxy root).
    CertificateUntrusted,
    /// TLS certificate otherwise invalid (expired, wrong hostname).
    CertificateInvalid,
    /// In-flight request cut short: reset, close, timeout, body phase.
    TransportInterrupted,
    /// Client-side request construction / redirect policy defect.
    TransportPermanent,
    Decode,
}

/// One per failed login attempt, emitted by the login funnel so a retried
/// request can't inflate the count. Failures that never reached HTTP (user
/// backed out, loopback bind, id_token validation) are not reported.
#[derive(Serialize)]
pub struct LoginFailed {
    pub error_kind: LoginFailureKind,
    /// OS code from the failure's cause chain (54/104 ECONNRESET, 10054 on
    /// Windows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_error: Option<i32>,
}

/// The user backed out of the login funnel. `stage` is "picker",
/// "api_key_entry", "loopback_paste", "device_wait", "api_key_wait", or
/// "command_wait"; `via` is "esc" or "quit".
#[derive(Serialize)]
pub struct LoginAbandoned {
    pub stage: String,
    pub via: String,
}

/// Result of persisting a user-provided API key.
#[derive(Serialize)]
pub struct ApiKeySaveResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Plan Mode
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct PlanModeToggled {
    pub enabled: bool,
    pub trigger: PlanModeTrigger,
    pub turn_in_flight: bool,
    pub was_previously_active: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Contextual tips
// ─────────────────────────────────────────────────────────────────────────────

/// One contextual-hint impression or acceptance: per tip, how often it is
/// shown vs. acted on (the `action` property drives the product-analytics funnel).
#[derive(Serialize)]
pub struct ContextualTip {
    pub tip: ContextualTipKind,
    pub action: ContextualTipAction,
}

// ─────────────────────────────────────────────────────────────────────────────
// Prompt suggestions (tab autocomplete ghost text)
// ─────────────────────────────────────────────────────────────────────────────

/// One predicted-next-prompt ghost impression or outcome. `shown` →
/// `accepted` conversion is the acceptance rate of the tab-autocomplete
/// feature; `dismissed` counts explicit Esc dismissals (a shown suggestion
/// with neither outcome was implicitly ignored). No suggestion text is
/// logged — only content-free metadata.
#[derive(Serialize)]
pub struct PromptSuggestion {
    pub action: PromptSuggestionAction,
    /// Length of the full suggestion in characters (content-free size
    /// signal: are long or short suggestions likelier to be accepted?).
    pub chars: usize,
    /// Number of whitespace-separated words in the suggestion.
    pub words: usize,
}

// ─────────────────────────────────────────────────────────────────────────────
// Permission Mode
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct YoloToggled {
    pub enabled: bool,
    pub previous_state: bool,
    pub trigger: YoloTrigger,
}

// ─────────────────────────────────────────────────────────────────────────────
// Slash Commands
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct SlashCommandUsed {
    pub command: String,
    pub args_provided: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Auto-Compact
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AutoCompactFired {
    pub tokens_before: u64,
    pub percentage: u8,
}

// ─────────────────────────────────────────────────────────────────────────────
// Compaction
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct CompactionTriggered {
    pub trigger: CompactionTrigger,
    pub tokens_used: u64,
    pub context_window: u64,
    pub percentage: u8,
    pub model_id: String,
    pub user_context_provided: bool,
    pub compaction_id: String,
    pub compaction_mode: CompactionModeLabel,
    pub two_pass_enabled: bool,
    pub is_subagent: bool,
}

#[derive(Serialize)]
pub struct CompactionCompleted {
    pub duration_ms: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub compaction_id: String,
    pub compaction_mode: CompactionModeLabel,
    pub two_pass: TwoPassOutcome,
    pub segments_written: u32,
    pub degenerate_retries: u32,
    pub input_overflow_retries: u32,
    pub is_subagent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_wait_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_compaction_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_compaction_ms: Option<u64>,
}

pub struct CompactionBeginParams {
    pub trigger: CompactionTrigger,
    pub tokens_used: u64,
    pub context_window: u64,
    pub model_id: String,
    pub user_context_provided: bool,
    pub compaction_mode: CompactionModeLabel,
    pub two_pass_enabled: bool,
    pub is_subagent: bool,
}

pub struct CompactionCompleteStats {
    pub tokens_after: u64,
    pub two_pass_used: bool,
    pub segments_written: u32,
    pub degenerate_retries: u32,
    pub input_overflow_retries: u32,
}

#[derive(Clone, Copy)]
pub struct CompactionTiming {
    pub model_wait_ms: Option<u64>,
    pub pre_compaction_ms: Option<u64>,
    pub post_compaction_ms: Option<u64>,
}

fn resolve_two_pass(enabled: bool, used: bool) -> TwoPassOutcome {
    match (enabled, used) {
        (false, _) => TwoPassOutcome::Disabled,
        (true, true) => TwoPassOutcome::Used,
        (true, false) => TwoPassOutcome::Miss,
    }
}

/// Emits `compaction_triggered` on `begin` and `compaction_completed` on
/// `complete`, correlated by a shared `compaction_id`. A scope dropped
/// without `complete` (error or cancel) emits no completion.
pub struct CompactionScope {
    pub compaction_id: String,
    pub tokens_before: u64,
    pub model_id: String,
    start: std::time::Instant,
    _active: crate::activity::ActivityGaugeGuard,
    compaction_mode: CompactionModeLabel,
    two_pass_enabled: bool,
    is_subagent: bool,
}

impl CompactionScope {
    pub fn begin(params: CompactionBeginParams) -> Self {
        let CompactionBeginParams {
            trigger,
            tokens_used,
            context_window,
            model_id,
            user_context_provided,
            compaction_mode,
            two_pass_enabled,
            is_subagent,
        } = params;
        let compaction_id = uuid::Uuid::new_v4().to_string();
        let percentage =
            echo_agent_token_estimation::usage_percentage_u8(tokens_used, context_window);
        let active = crate::activity::COMPACTIONS_ACTIVE.enter();
        debug_assert!(
            crate::activity::COMPACTIONS_ACTIVE.get() >= 1,
            "CompactionTriggered must stamp a self-inclusive count"
        );
        crate::session_ctx::log_event(CompactionTriggered {
            trigger,
            tokens_used,
            context_window,
            percentage,
            model_id: model_id.clone(),
            user_context_provided,
            compaction_id: compaction_id.clone(),
            compaction_mode,
            two_pass_enabled,
            is_subagent,
        });
        Self {
            compaction_id,
            tokens_before: tokens_used,
            model_id,
            start: std::time::Instant::now(),
            _active: active,
            compaction_mode,
            two_pass_enabled,
            is_subagent,
        }
    }

    pub fn complete(self, stats: CompactionCompleteStats, timing: CompactionTiming) {
        let two_pass = resolve_two_pass(self.two_pass_enabled, stats.two_pass_used);
        crate::session_ctx::log_event(CompactionCompleted {
            duration_ms: self.start.elapsed().as_millis() as u64,
            tokens_before: self.tokens_before,
            tokens_after: stats.tokens_after,
            model_id: Some(self.model_id),
            compaction_id: self.compaction_id,
            compaction_mode: self.compaction_mode,
            two_pass,
            segments_written: stats.segments_written,
            degenerate_retries: stats.degenerate_retries,
            input_overflow_retries: stats.input_overflow_retries,
            is_subagent: self.is_subagent,
            model_wait_ms: timing.model_wait_ms,
            pre_compaction_ms: timing.pre_compaction_ms,
            post_compaction_ms: timing.post_compaction_ms,
        });
    }
}

/// Auto-compaction suppressed after a deterministic failure so the turn loop stops
/// re-firing a doomed compaction. Fires once per transition into the suppressed
/// state; `reason` is a fixed classification: `credit_block | size | auth | schema | other`.
#[derive(Serialize)]
pub struct AutoCompactSuppressed {
    pub reason: &'static str,
    pub estimated_tokens: u64,
    pub context_window: u64,
}

#[derive(Serialize)]
pub struct CompactionRetryDegraded {
    pub trigger: CompactionTrigger,
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_stage: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_stage: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_chars: Option<u64>,
    pub attempt: u32,
    pub context_window: u64,
    pub compaction_id: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Subagents
// ─────────────────────────────────────────────────────────────────────────────

/// Which spawn path owns a subagent's lifecycle.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentOwnerKind {
    Task,
    Workflow,
    SchedulerLoop,
}

/// Which admission limit a spawn ran into.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentLimitKind {
    SessionConcurrent,
    WorkflowRunConcurrent,
}

/// What happened to the spawn that hit a limit.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentLimitDisposition {
    Queued,
    Failed,
}

#[derive(Serialize)]
pub struct SubagentLaunched {
    pub subagent_id: String,
    pub parent_session_id: String,
    pub subagent_type: String,
    pub owner: SubagentOwnerKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_run_id: Option<String>,
    /// Time parked in the admission queue; absent if admitted immediately.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_ms: Option<u64>,
    /// The session's running non-workflow subagents at launch, including
    /// this one; max per session is the session's peak concurrency.
    pub session_running: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    pub fork_context: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_from: Option<String>,
    pub isolated_worktree: bool,
    pub mcp_inherited_count: u32,
    pub mcp_owned_count: u32,
    pub skills_inherited_count: u32,
}

#[derive(Serialize)]
pub struct SubagentCompleted {
    pub subagent_id: String,
    pub parent_session_id: String,
    pub owner: SubagentOwnerKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_run_id: Option<String>,
    pub outcome: Outcome,
    pub duration_ms: u64,
    pub tool_calls: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
    // Spawn-phase durations (`crate::subagent_spawn`, the
    // `echo_agent_code_subagent_spawn_*` taxonomy); absent when a phase did not run.
    // Populated through `SubagentSpawnTimer::write_event_phases`' single match,
    // which fails to compile until a new phase is given a field below.
    // Phases are hierarchical (agent_build + tool_setup nest in
    // session_bootstrap); summing all of them double-counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_wait_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_prepare_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_bootstrap_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_build_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_setup_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_to_first_turn_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct SubagentLimitHit {
    pub parent_session_id: String,
    pub limit_kind: SubagentLimitKind,
    pub disposition: SubagentLimitDisposition,
    pub limit: u64,
    pub running: u32,
    /// A queued spawn counts itself; absent for the workflow pool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<u32>,
    pub owner: SubagentOwnerKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_run_id: Option<String>,
}

impl SubagentLimitHit {
    /// The session pool's producer.
    pub fn session_concurrent(
        parent_session_id: String,
        disposition: SubagentLimitDisposition,
        limit: u64,
        running: u32,
        queue_depth: u32,
        owner: SubagentOwnerKind,
    ) -> Self {
        Self {
            parent_session_id,
            limit_kind: SubagentLimitKind::SessionConcurrent,
            disposition,
            limit,
            running,
            queued: Some(queue_depth),
            owner,
            workflow_run_id: None,
        }
    }

    /// The workflow pool's producer: waiters block on the run's semaphore,
    /// so there is no queue depth to report.
    pub fn workflow_run_concurrent(
        parent_session_id: String,
        workflow_run_id: String,
        limit: u64,
        slots_in_use: u32,
    ) -> Self {
        Self {
            parent_session_id,
            limit_kind: SubagentLimitKind::WorkflowRunConcurrent,
            disposition: SubagentLimitDisposition::Queued,
            limit,
            running: slots_in_use,
            queued: None,
            owner: SubagentOwnerKind::Workflow,
            workflow_run_id: Some(workflow_run_id),
        }
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitWaitOutcome {
    Recovered,
    BudgetSpent,
    Unresolved,
}

/// Emitted once per inner `process_conversation_turn`, so one `turn_number`
/// can carry several rows; do not blindly GROUP BY turn_number.
#[derive(Serialize)]
pub struct SubagentRateLimitWaited {
    /// Resubmits (waits) this turn, excluding the initial send.
    pub attempts: u32,
    pub max_attempts: u32,
    pub waited_ms: u64,
    pub budget_ms: u64,
    pub outcome: RateLimitWaitOutcome,
}

/// Where a workflow script came from.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowSourceKind {
    Builtin,
    File,
    Inline,
}

/// One workflow execution episode began (fresh launch or resume).
#[derive(Serialize)]
pub struct WorkflowRunStarted {
    pub run_id: String,
    pub parent_session_id: String,
    pub source: WorkflowSourceKind,
    /// Built-in workflow names only; user script names stay local.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_budget: Option<u64>,
    /// Effective cap, after the CPU clamp.
    pub max_concurrent_agents: u32,
    pub resumed: bool,
}

/// The run tracker's status labels, plus `superseded` for an episode whose
/// run a quick resume took over.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunEndStatus {
    Active,
    UserPaused,
    BackOffPaused,
    NoProgressPaused,
    InfraPaused,
    Blocked,
    BudgetLimited,
    Interrupted,
    Complete,
    Failed,
    Cancelled,
    Superseded,
}

#[derive(Serialize)]
pub struct WorkflowRunEnded {
    pub run_id: String,
    pub parent_session_id: String,
    pub status: WorkflowRunEndStatus,
    /// Cumulative across the run's episodes.
    pub duration_ms: u64,
    /// Cumulative across the run's episodes.
    pub agents_used: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_budget: Option<u64>,
    /// This episode only.
    pub agents_failed: u32,
    /// This episode only.
    pub peak_concurrent_agents: u32,
    /// This episode only.
    pub slot_waits: u32,
    /// This episode only.
    pub slot_wait_ms_total: u64,
    /// This episode only.
    pub slot_wait_ms_max: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Model Switching
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ModelSwitched {
    pub session_id: String,
    pub previous_model_id: String,
    pub new_model_id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_agent_type: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugins
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct PluginAdded {
    pub source: PluginSource,
    pub success: bool,
}

#[derive(Serialize)]
pub struct PluginRemoved {
    pub success: bool,
}

#[derive(Serialize)]
pub struct PluginInstalled {
    pub install_kind: InstallKind,
    pub trust: bool,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_category: Option<String>,
}

#[derive(Serialize)]
pub struct PluginUninstalled {
    pub confirmed: bool,
    pub success: bool,
}

#[derive(Serialize)]
pub struct PluginReloaded {
    pub success: bool,
}

#[derive(Serialize)]
pub struct PluginUsed {
    pub plugin_id: String,
    pub plugin_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_event: Option<String>,
    pub success: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin CTA (inline marketplace "Connect" upsell)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct PluginCtaImpression {
    pub plugin_name: String,
}

#[derive(Serialize)]
pub struct PluginCtaConnectClicked {
    pub plugin_name: String,
    pub is_retry: bool,
}

#[derive(Serialize)]
pub struct PluginCtaDismissed {
    pub plugin_name: String,
}

#[derive(Serialize)]
pub struct PluginCtaInstalled {
    pub plugin_name: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_category: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Extensions modal
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionsModalTrigger {
    SlashCommand,
    KeyboardShortcut,
    CommandPalette,
    AuthHandoff,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionsInputMethod {
    Keyboard,
    Mouse,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionsModalTab {
    Hooks,
    Plugins,
    Marketplace,
    Skills,
    Workflows,
    McpServers,
}

#[derive(Serialize)]
pub struct ExtensionsModalOpened {
    pub trigger: ExtensionsModalTrigger,
    pub tab: ExtensionsModalTab,
}

#[derive(Serialize)]
pub struct ExtensionsModalAction {
    pub tab: ExtensionsModalTab,
    pub action: String,
    pub input_method: ExtensionsInputMethod,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Hooks
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct HookAdded {
    pub success: bool,
}

#[derive(Serialize)]
pub struct HookRemoved {
    pub success: bool,
}

#[derive(Serialize)]
pub struct HookTrusted {
    pub success: bool,
}

#[derive(Serialize)]
pub struct HookExecuted {
    pub hook_name: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    pub duration_ms: u64,
    pub outcome: HookOutcome,
}

#[derive(Serialize)]
pub struct HookBlocked {
    pub hook_name: String,
}

/// Per-callback outcome of a `PreToolUse` gate. A deny returns early, so callbacks
/// still pending at that point are not logged.
#[derive(Serialize)]
pub struct ClientHookGate {
    pub callback_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    pub outcome: ClientHookGateOutcome,
    pub duration_ms: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Skills
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct SkillAdded {
    pub added_count: u32,
    pub total_skills: u32,
    pub success: bool,
}

#[derive(Serialize)]
pub struct SkillRemoved {
    pub success: bool,
}

#[derive(Serialize, Clone, Copy, strum::IntoStaticStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SkillTrigger {
    /// The user ran `/skill-name`, at turn start or mid-turn.
    SlashCommand,
    /// The model read the skill's `SKILL.md` with `read_file`.
    SkillMdRead,
    /// The model called the skill tool, which only vendor-compat toolsets register.
    SkillTool,
}

#[derive(Serialize)]
pub struct SkillDispatched {
    pub skill_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_source: Option<String>,
    pub trigger: SkillTrigger,
}

// ─────────────────────────────────────────────────────────────────────────────
// MCP
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct McpServerConnected {
    pub server_name: String,
    pub tool_count: u32,
    pub transport: McpTransport,
    pub duration_ms: u64,
}

#[derive(Serialize)]
pub struct McpServerFailed {
    pub server_name: String,
    pub error_type: McpErrorType,
    pub duration_ms: u64,
    pub timeout_sec: u64,
}

#[derive(Serialize)]
pub struct McpInitCompleted {
    pub total_duration_ms: u64,
    pub server_count: u32,
    pub servers_succeeded: u32,
    pub servers_failed: u32,
    pub servers_auth_required: u32,
    pub total_tools_registered: u32,
    pub strategy: McpStrategy,
    pub is_reinit: bool,
}

#[derive(Serialize)]
pub struct McpToolCalled {
    pub server_name: String,
    pub tool_name: String,
    pub qualified_name: String,
    pub success: bool,
    pub duration_ms: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Session Lifecycle
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SessionHarness {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_identifier: Option<String>,
    pub model_id: String,
    pub agent_name: String,
    pub permission_mode: PermissionMode,
    pub mcp_server_names: Vec<String>,
    pub plugin_names: Vec<String>,
    pub skill_names: Vec<String>,
    pub lsp_server_names: Vec<String>,
    pub hook_names: Vec<String>,
    pub agents_md_dir_names: Vec<String>,
    pub memory_enabled: bool,
    pub memory_retrieval_mode: MemoryRetrievalMode,
    /// Whether the session cwd is inside a git repo (same value `SessionNew`
    /// carries). Additive analytics-visible field, added for the external
    /// `session_start` event (design ‡ footnote).
    pub is_git_repo: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
}

#[derive(Serialize)]
pub struct SessionLoad {
    pub session_id: String,
    pub compaction_count: u64,
    pub turn_count: u64,
    pub tool_call_count: u64,
    pub plan_mode_state: PlanModeState,
    pub permission_mode: PermissionMode,
    pub model_id: String,
    pub restored_from_disk: bool,
}

#[derive(Serialize)]
pub struct SessionNew {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    pub is_git_repo: bool,
    pub permission_mode: PermissionMode,
}

#[derive(Serialize)]
pub struct PromptSubmitted {
    pub prompt_length: usize,
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_identifier: Option<String>,
    /// Pager screen mode from the prompt request `_meta.screenMode`
    /// (`fullscreen` | `inline` | `minimal` | `headless`). `None` for
    /// non-pager clients and synthetic prompts (goal summaries, drains,
    /// interjections).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_mode: Option<String>,
    /// Raw prompt text for the external stream's `OTEL_LOG_USER_PROMPTS`
    /// gate **only**. `#[serde(skip)]`: never serialized to product events/analytics;
    /// dropped at external emit time unless the gate is on (then capped at
    /// 60 KB and secret-scrubbed).
    #[serde(skip)]
    pub prompt_text: Option<String>,
}

#[derive(Serialize)]
pub struct UserFeedback {
    pub session_id: String,
    pub has_feedback_text: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_value: Option<i32>,
    pub is_solicited: bool,
}

#[derive(Serialize)]
pub struct RolloutSurvey {
    pub session_id: String,
    pub preferences: Vec<String>,
    pub has_feedback: bool,
}

/// PR created via the session (bash `gh pr create` or MCP create_pull_request).
/// Counts only — PR url/number stay in the turn_result.json signals, not here.
#[derive(Serialize)]
pub struct PrCreated {
    pub source: PrCreationSource,
    /// Whether the session recorded a `git commit` before the create
    /// (end-to-end attribution vs unknown work start).
    pub had_commit_in_session: bool,
}

/// PR merged via the session bash tool (`gh pr merge`).
#[derive(Serialize)]
pub struct PrMerged {}

#[derive(Serialize)]
pub struct MultiAgentFollowup {
    pub preferred_agent_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_agent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_agent_model_id: Option<String>,
    pub other_agents: Vec<AgentInfo>,
    pub total_agents: usize,
}

#[derive(Serialize)]
pub struct MultiAgentApply {
    pub applied_agent_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_agent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_agent_model_id: Option<String>,
    pub discarded_agents: Vec<AgentInfo>,
    pub total_agents: usize,
}

#[derive(Serialize)]
pub struct MultiAgentDiscard {
    pub discarded_agents: Vec<AgentInfo>,
    pub total_agents_discarded: usize,
}

#[derive(Serialize)]
pub struct AgentInfo {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

#[derive(Serialize)]
pub struct RepoChanges {
    pub commit_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged_files_changed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged_insertions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged_deletions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unstaged_files_changed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unstaged_insertions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unstaged_deletions: Option<u64>,
    pub untracked_file_count: usize,
    pub untracked_total_bytes: u64,
    pub is_detached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
}

#[derive(Serialize)]
pub struct NonGitDecisionEvent {
    pub decision: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
}

// ---------------------------------------------------------------------------
// Prompt Latency (every turn)
// ---------------------------------------------------------------------------

/// Why a [`ProcessResourceUsage`] was sampled, so a mid-life reading is not
/// read as a post-teardown one.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ResourceReportTrigger {
    SessionClose,
    Periodic,
}

/// The ceilings this process runs under. The denominator for
/// `ProcessResourceUsage`: usage against limits is headroom.
#[derive(Serialize)]
pub struct ProcessResourceLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nofile_soft: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nofile_hard: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nproc_soft: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nproc_hard: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_parallelism: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_pids_max: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_memory_max: Option<String>,
}

/// Emitted when the jemalloc heap monitor crosses a configured threshold.
/// The acute signal that a build is growing without bound.
#[derive(Serialize)]
pub struct HeapThresholdCrossed {
    pub threshold_bytes: u64,
    pub resident_bytes: u64,
    pub allocated_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_peak_bytes: Option<u64>,
}

/// What this process still holds just after a session was removed. Aggregated
/// per release, a rising tail is a leak; `resident_sessions` separates leader
/// mode, where one process serves many sessions and a leak compounds.
#[derive(Serialize)]
pub struct ProcessResourceUsage {
    pub trigger: ResourceReportTrigger,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footprint_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allocated_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threads: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_files: Option<u64>,
    pub resident_sessions: usize,
    pub session_threads: usize,
}

#[derive(Serialize)]
pub struct PromptLatency {
    pub turn_index: u32,
    pub total_ms: u64,
    pub mcp_wait_ms: u64,
    pub tool_collection_ms: u64,
    pub model_call_ms: u64,
    pub pre_model_ms: u64,
    pub mcp_server_count: u32,
    pub mcp_tools_registered: u32,
    pub mcp_strategy: McpStrategy,
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    pub ttlb_ms: u64,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Turn Lifecycle
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct TurnCompleted {
    pub outcome: Outcome,
    pub duration_ms: u64,
    pub tool_call_count: u32,
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancellation_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_category: Option<String>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CancellationScope {
    Turn,
    Compaction,
}

#[derive(Serialize, Clone, Copy)]
pub struct CancellationCompleted {
    pub latency_ms: u64,
    pub scope: CancellationScope,
}

/// Model issued a shell tool call whose command is `true` (keepalive thrash signal).
#[derive(Serialize)]
pub struct ShellTrueNoop {
    pub tool_name: String,
}

/// Harness nudged the model to break a run of identical tool calls. Pairs with
/// [`ActionStationarityStop`]: the nudge fires first and once per run, the stop only
/// if the run continues to the hard limit.
///
/// `problematically_repeating` splits the two threshold tiers (tools whose identical
/// repeats are never productive versus everything else), so nudge and stop each break
/// down by tier.
#[derive(Serialize)]
pub struct ActionStationarityNudge {
    pub problematically_repeating: bool,
    pub run_len: u32,
    pub tool_name: String,
}

/// Harness hard-stopped a turn after identical tool thrash (silent EndTurn).
#[derive(Serialize)]
pub struct ActionStationarityStop {
    pub true_noop: bool,
    pub problematically_repeating: bool,
    pub run_len: u32,
    pub tool_name: String,
}

// ---------------------------------------------------------------------------
// Tool Calls
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ToolCallCompleted {
    pub tool_name: String,
    pub outcome: echo_agent_session_events::types::ToolOutcome,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_result_size_bytes: Option<u64>,
    /// Primary file path of the call, for the external stream only
    /// (`#[serde(skip)]`: never serialized to product events/analytics). Always reduced to
    /// `file_extension`; the full path rides the `OTEL_LOG_TOOL_DETAILS` gate.
    #[serde(skip)]
    pub file_path: Option<String>,
    /// Tool parameters for the external stream's `OTEL_LOG_TOOL_DETAILS`
    /// gate **only** (`#[serde(skip)]`; reduced to 4 KB / depth 2 / 20 items
    /// at emit time).
    #[serde(skip)]
    pub parameters: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Model Response
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ModelResponseReceived {
    pub model_id: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_prompt_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct MemoryFlushed {
    pub trigger: MemoryFlushTrigger,
    pub success: bool,
    pub duration_ms: u64,
    pub response_length: usize,
}

// ---------------------------------------------------------------------------
// Media Generation
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct MediaGenerated {
    pub media_type: MediaType,
    pub success: bool,
    pub prompt_length: usize,
}

// ---------------------------------------------------------------------------
// Session End
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SessionEnded {
    pub duration_secs: u64,
    pub turn_count: u64,
    pub tool_call_count: u64,
    pub compaction_count: u64,
    pub model_id: String,
}

// ---------------------------------------------------------------------------
// Auth lock contention (aggregate layer; unified_log carries the forensics)
// ---------------------------------------------------------------------------

/// A contended `auth.json.lock` acquisition; instant acquisitions stay silent.
#[derive(Serialize)]
pub struct AuthLockWait {
    pub wait_ms: u64,
    pub budget_ms: u64,
}

/// An `auth.json.lock` wait that exhausted its budget.
#[derive(Serialize)]
pub struct AuthLockTimeout {
    pub budget_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_state: Option<&'static str>,
}

/// A held lock's file was replaced out from under it: an unlink-recovery
/// binary is still active in the fleet. The holder fields describe the replacer.
#[derive(Serialize)]
pub struct AuthLockReplacedOutFromUnder {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_age_secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// Pager events (called from echo-agent-pager via log_event)
// ---------------------------------------------------------------------------

/// Connect outcome: the `agent_connect` product event, plus OTEL metrics.
#[derive(Serialize)]
pub struct AgentConnect {
    pub connect_target: crate::startup::AgentKind,
    pub outcome: crate::startup::StartupOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stuck_in: Option<String>,
    pub phases: String,
    pub phase_durations_ms: std::collections::BTreeMap<String, u64>,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    pub embedded_fallback: bool,
    pub auth_mode: crate::startup::AuthMode,
}

#[derive(Serialize)]
pub struct StartupCompleted {
    pub total_ms: u64,
    pub outcome: crate::startup::StartupOutcome,
    pub phases: String,
    pub auth_mode: crate::startup::AuthMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefetch_wait_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_load_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_replay_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_git_scan_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_spawn_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_first_frame_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct PagerSlashCommand {
    pub command_name: String,
    pub source: PagerCommandSource,
}

#[derive(Serialize)]
pub struct PlanSubmit {
    pub action: String,
}

#[derive(Serialize)]
pub struct EventLoopStall {
    pub max_stall_ms: u64,
    pub window_ms: u64,
    pub events_handled: u32,
    pub stall_compaction_active: bool,
    pub stall_subagents_active: u32,
    pub stall_mcp_servers_connected: u32,
}

// ---------------------------------------------------------------------------
// EchoAgent upsell
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EchoAgentUpsell {
    WelcomeScreen,
    RateLimitError,
    /// Free-usage-exhausted paywall modal (free-tier 429 with the
    /// `subscription:free-usage-exhausted` well-known error code).
    FreeUsagePaywall,
    /// Upsell modal shown when a tier-restricted slash command
    /// (`/usage`, `/imagine`, …) is invoked on the free / X Basic tiers.
    RestrictedCommand,
}

#[derive(Serialize)]
pub struct EchoAgentUpsellShown {
    pub source: EchoAgentUpsell,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
}

#[derive(Serialize)]
pub struct EchoAgentUpsellClicked {
    pub source: EchoAgentUpsell,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
}

/// Which surface a promo announcement's upgrade CTA was activated from.
/// Modeled on [`EchoAgentUpsell`]; lets the funnel attribute the click to the
/// welcome hero vs the in-session header vs the banner vs the dashboard, and
/// distinguish keyboard (`Ctrl+O`) activations from pointer/OSC 8 ones.
/// Ord/Eq exist for the pager's per-(announcement, surface) impression latch.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AnnouncementCtaSurface {
    Banner,
    Welcome,
    Header,
    Dashboard,
    Keyboard,
}

/// A promo announcement's CTA button was painted on a surface — the
/// impression half of the per-surface CTR funnel with
/// [`AnnouncementCtaClicked`]. Emitted once per (announcement, surface) per
/// pager process (cleared on logout); never emitted for `Keyboard` (a
/// click-only surface).
#[derive(Serialize)]
pub struct AnnouncementCtaShown {
    /// Announcement `id` from the server push (`None` for id-less items).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Which surface painted the button.
    pub source: AnnouncementCtaSurface,
}

/// User activated a promo announcement's CTA button (the `[label]` open).
#[derive(Serialize)]
pub struct AnnouncementCtaClicked {
    /// Announcement `id` from the server push (`None` for id-less items).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Which surface the activation came from (per-surface conversion signal).
    pub source: AnnouncementCtaSurface,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodingDataConsentSource {
    PrivacyBanner,
    Settings,
    /// "Opt in" on the `/feedback` trace-consent card
    /// while individually opted out.
    FeedbackTraceCard,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodingDataConsentChoice {
    OptIn,
    OptOut,
}

impl CodingDataConsentChoice {
    pub fn from_opted_in(opted_in: bool) -> Self {
        if opted_in { Self::OptIn } else { Self::OptOut }
    }
}

#[derive(Serialize)]
pub struct CodingDataConsentSelected {
    pub source: CodingDataConsentSource,
    pub choice: CodingDataConsentChoice,
    pub previous_choice: CodingDataConsentChoice,
    pub changed: bool,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackTraceConsentChoice {
    /// "Opt in".
    TurnOn,
    /// "Opt out this time" — also the Esc/skip outcome.
    NoUpload,
    /// "Opt out and don't ask again".
    NeverAsk,
}

/// The `/feedback` trace-consent card was shown (funnel denominator for
/// [`FeedbackTraceConsentSelected`]).
#[derive(Serialize)]
pub struct FeedbackTraceCardShown {
    /// The "yes" option disclosed that it re-enables coding-data sharing.
    pub reenables_sharing: bool,
}

/// Outcome of the `/feedback` trace-consent card (only emitted when the card
/// was shown).
#[derive(Serialize)]
pub struct FeedbackTraceConsentSelected {
    pub choice: FeedbackTraceConsentChoice,
    /// The "yes" option disclosed that it re-enables coding-data sharing.
    pub reenables_sharing: bool,
}

/// Flat snapshot of the terminal environment for telemetry.
///
/// Shared across pager events so terminal fields are typed once.
/// Constructed by the pager's `TerminalContext::telemetry_snapshot()`.
#[derive(Clone, Debug, Serialize)]
pub struct TerminalTelemetry {
    pub brand: String,
    pub multiplexer: String,
    pub is_ssh: bool,
    pub is_byobu: bool,
    pub term_var: String,
    pub tmux_version: String,
    pub xtversion: String,
    /// Raw, as its source reported it — shapes vary (`"3.5.6"`,
    /// `"20240203-110809-5046fc22"`, `"7402"`). Empty when unknown.
    pub term_version: String,
    pub term_version_source: String,
    /// The Kitty protocol was negotiated *without* `REPORT_EVENT_TYPES` because
    /// `term_version` identified a build that mis-encodes key releases
    /// (Alacritty ≤ 0.14.x). A field rather than its own event so the affected
    /// population always has a denominator.
    pub kitty_event_types_withheld: bool,
    pub host_os: String,
    pub display_server: String,
    pub modifier_cmd_fate: String,
    pub modifier_opt_fate: String,
    pub enter_modifier_fate: String,
    pub hyperlink_osc8: String,
    pub hyperlink_skip_reason: String,
    pub clipboard_route: String,
    pub clipboard_native_tool: String,
    /// Wayland data-control protocol availability: "yes" | "no" | "n/a"
    /// (n/a off Wayland).
    pub clipboard_data_control: String,
}

/// One-shot OS primary-display refresh probe + auto-cadence decision at process start.
#[derive(Serialize)]
pub struct DisplayRefreshProbe {
    #[serde(flatten)]
    pub terminal: TerminalTelemetry,
    /// `ok` | `skipped` | `error`
    pub outcome: String,
    /// Refresh rate as `i64` so OTLP/analytics keep a numeric field.
    pub hz: Option<i64>,
    /// Backend token, e.g. `macos_core_graphics`.
    pub source: String,
    /// Empty when ok; else stable skip/error reason (`ssh`, `wsl`, …).
    pub skip_reason: String,
    /// Wall ms as `i64` so OTLP/analytics keep a numeric field (u64 serializes as string).
    pub duration_ms: i64,
    pub auto_cadence_enabled: bool,
    /// True when derived auto ms is used on at least one motion clock.
    pub auto_cadence_applied: bool,
    pub effective_min_draw_ms: i64,
    pub effective_scroll_cadence_ms: i64,
    /// `flag_off` | `disabled` | `probe_skip` | `hz_out_of_range` | `env_override` | `applied`.
    pub auto_cadence_reason: String,
}

/// Emitted once per system-clipboard attachment read during paste
/// (Ctrl/Cmd+V). Diagnoses silent image-paste failures, e.g. Wayland-only
/// sessions where the X11 CLIPBOARD probe comes back empty.
#[derive(Serialize)]
pub struct ClipboardImagePaste {
    #[serde(flatten)]
    pub terminal: TerminalTelemetry,
    /// Which read ran: "attachments" (file URLs + image) or "image".
    pub probe: String,
    /// "image" | "file_urls" | "empty" | "error".
    pub outcome: String,
    /// MIME type when outcome == "image", else "".
    pub image_mime: String,
    /// Wall-clock duration of the clipboard read in milliseconds.
    pub duration_ms: u64,
}

/// Emitted when Ctrl/Cmd+V (paste key) is handled but the **host** process
/// clipboard has no pasteable text/image/file URLs. Diagnoses silent no-ops
/// on remote/ETX sessions. Does not change paste behavior.
#[derive(Serialize)]
pub struct PasteKeyEmptyHostClipboard {
    #[serde(flatten)]
    pub terminal: TerminalTelemetry,
    /// Call site: "agent" | "prompt_widget" | "dashboard" | "peek" | "picker".
    pub surface: String,
}

/// Emitted once per user-visible text copy (`copy_text` / TUI yank, etc.).
///
/// Captures per-leg write outcomes so we can diagnose "copy doesn't work"
/// reports (e.g. Wayland + xclip probe; did wl-copy actually succeed?) without
/// relying on toast text alone.
#[derive(Serialize)]
pub struct ClipboardCopy {
    #[serde(flatten)]
    pub terminal: TerminalTelemetry,
    /// Call site; currently always `copy_text`.
    pub source: &'static str,
    /// Payload size only (no content).
    pub text_len: u64,
    /// Route policy (enabled legs), independent of which legs succeeded.
    pub route_native: bool,
    pub route_tmux: bool,
    pub route_osc52: bool,
    /// `ClipboardRoute` Display, e.g. `native+osc52`.
    pub route_label: String,
    /// CLI tools actually invoked, `+`-joined (e.g. `wl-copy+xclip`); empty if none.
    pub cli_tools_tried: String,
    /// CLI tools that returned Ok, `+`-joined; empty if none succeeded.
    /// On Wayland, wl-copy is read-back-verified only when `data_control` is
    /// false; with `data_control && arboard_ok` its exit-0 is credited
    /// unverified (the arboard write is authoritative) — condition wl-copy
    /// success rates on `data_control`.
    pub cli_ok_tools: String,
    pub cli_ok: bool,
    pub arboard_ok: bool,
    /// The Wayland data-control protocol was available for this write (the
    /// environment probe — NOT proof the arboard write landed; a focus-free
    /// authoritative write additionally requires `arboard_ok`). Always false
    /// off-Wayland.
    pub data_control: bool,
    pub tmux_ok: bool,
    pub osc52_ok: bool,
    /// Evidence classification: `confirmed` | `unverified` | `failed`.
    pub delivery: &'static str,
    /// An explicit `echoagent wrap` OSC 52 sink was active.
    pub osc52_sink: bool,
    /// The process was inside a container without a display server.
    pub container_no_display: bool,
    /// Historical boolean projection: true unless `delivery == failed`.
    pub reported_success: bool,
    /// Exact UX toast branch selected by the environment policy.
    pub toast_kind: &'static str,
    pub duration_ms: u64,
}

/// Emitted when backspace/delete is pressed but produces no text change
/// on a non-empty prompt. Used to diagnose the "backspace lock" bug.
#[derive(Serialize)]
pub struct BackspaceNoEffect {
    #[serde(flatten)]
    pub terminal: TerminalTelemetry,
    pub key_code: String,
    pub key_modifiers: String,
    pub key_kind: String,
    pub cursor_pos: usize,
    pub text_len: usize,
    pub has_selection: bool,
}

/// Emitted each time a terminal notification is actually sent (not filtered
/// by condition or event kind). Used for protocol distribution analysis.
#[derive(Serialize)]
pub struct NotificationEmitted {
    pub protocol: &'static str,
    pub event_kind: &'static str,
    pub was_focused: bool,
}

#[derive(Serialize)]
pub struct DashboardOpened {
    pub agents: usize,
    pub subagents: usize,
    pub leader_mode: bool,
}

/// User pressed an allowlisted registry shortcut.
///
/// **Product contract (authoritative):** intent-only telemetry for the
/// bindings that can own **Ctrl+L**. Emits when the chord resolves to the
/// action, whether the effect succeeds, defers, or soft-no-ops. Soft no-ops
/// still count as intent.
///
/// Allowlist: `interject_prompt` (VS Code family often Ctrl+L; elsewhere the
/// interject/send-now chord) and `open_extensions` (Ctrl+L on other
/// terminals). Absence of other actions is not “unused.” Expand the allowlist
/// deliberately; this is not full-registry coverage.
///
/// Fields are content-free. `key` is a platform-stable encoding (`Ctrl+L`,
/// not locale-specific `Cmd`/`Opt` or mixed case). `context` is a surface
/// label (`prompt_focused`, `agent_screen`, `queue`, …).
#[derive(Serialize)]
pub struct ShortcutUsed {
    /// Stable chord encoding (`Ctrl+L`, `Ctrl+Enter`, …).
    pub key: String,
    /// Allowlisted action id (`interject_prompt`, `open_extensions`).
    pub action: String,
    /// Surface label (`prompt_focused`, `agent_screen`, `queue`, …).
    pub context: String,
}

#[derive(Serialize)]
pub struct DashboardClosed {
    pub agents: usize,
}

#[derive(Serialize)]
pub struct DashboardAgentAttached {
    pub kind: &'static str,
}

#[derive(Serialize)]
pub struct DashboardAgentLaunched {
    pub source: &'static str,
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

/// Emitted when a user's turn fails due to rate limiting (all retries
/// exhausted). Key conversion-funnel signal: rate limit → upsell → subscribe.
#[derive(Serialize)]
pub struct RateLimitHit {
    pub model_id: String,
    /// Number of retry attempts before giving up.
    pub attempts: u32,
}

/// Model-API failure at the turn level (non-rate-limit). Category/class only
/// — no message text (external `api_error` event; also a product event).
#[derive(Serialize)]
pub struct ApiError {
    /// Fixed classification (`auth`, `server_error`, `timeout`, …).
    pub error_category: String,
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Internal (our-code) error class for the external `internal_error` event.
/// Error class only — no message, no location (user decision, RQ5).
#[derive(Serialize)]
pub struct InternalError {
    pub error_type: String,
}

// ---------------------------------------------------------------------------
// External-OTEL stream meta-events (product-events only — adoption visibility;
// never exported externally)
// ---------------------------------------------------------------------------

/// Emitted once per process (post-auth) when the external OTEL stream is
/// configured. Endpoint reduced to `scheme://host[:port]` — we measure
/// adoption without learning collector details.
#[derive(Serialize)]
pub struct ExternalOtelConfigured {
    pub metrics_exporter: String,
    pub logs_exporter: String,
    pub protocol: String,
    pub logs_endpoint_origin: String,
    pub metrics_endpoint_origin: String,
    pub prompts_gate: bool,
    pub details_gate: bool,
    /// Startup source of the master switch: `env` | `config`.
    pub source: String,
}

/// Remote (fleet) policy applied to the external stream mid-run.
#[derive(Serialize)]
pub struct ExternalOtelRemotePolicyApplied {
    /// `force_disable` | `gates_locked`.
    pub action: String,
}

/// Export-health counters for the external stream, emitted on the internal
/// pipeline at shutdown (never externally — avoid feedback loops).
#[derive(Serialize)]
pub struct ExternalOtelExportHealth {
    pub records_dropped: u64,
    pub metric_exports_dropped: u64,
    pub export_failures: u64,
    pub export_successes: u64,
}

/// Once per session. Carries no `command` string or script output.
#[derive(Serialize)]
pub struct StatusLineConfigured {
    /// `unset` when the config named no mode, which is adoption's denominator.
    pub kind: &'static str,
    /// Always `false` once the user wrote `type = "disabled"`, and reported even
    /// by a client that draws no row.
    pub row_shows_a_problem: bool,
    pub items: String,
    pub custom_items: bool,
}

/// How the status line fared, at shutdown, for every session that enabled it.
#[derive(Serialize)]
pub struct StatusLineHealth {
    pub kind: &'static str,
    /// A run's error text counts, a config diagnostic does not, so `false` can
    /// still mean a bar that showed one all session.
    pub had_content: bool,
    pub runs_ok: u64,
    /// Shown on the row as `[status line: …]`.
    pub runs_failed: u64,
    pub runs_timed_out: u64,
    /// Given up on; counted again under its outcome if it ever lands.
    pub runs_abandoned: u64,
    pub slowest_ms: u64,
}

// ---------------------------------------------------------------------------
// Credit limit
// ---------------------------------------------------------------------------

/// 403 "run out of credits" — billing exhaustion (not request throttling).
#[derive(Serialize)]
pub struct CreditLimitHit {
    pub model_id: String,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum CreditLimitUpsellSurface {
    /// Q&A modal with "Upgrade tier" + "Pay as you go" options (non-max-tier).
    QuestionModal,
    /// Inline scrollback card with PAYG link (max-tier / Heavy users).
    InlineCard,
}

/// Credit-limit upsell displayed to the user.
#[derive(Serialize)]
pub struct CreditLimitUpsellShown {
    pub surface: CreditLimitUpsellSurface,
    pub max_tier: bool,
    pub pay_as_you_go: bool,
    /// User is on unified usage billing (buy-credits wording). When false,
    /// legacy on-demand / PAYG wording was used.
    #[serde(default)]
    pub unified_billing: bool,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum CreditLimitChoice {
    UpgradeTier,
    /// Covers both "Pay as you go" (enable) and "Increase limit" (raise cap).
    PayAsYouGo,
    /// Unified-billing / credits-pool users: purchase prepaid credits.
    PurchaseCredits,
}

/// User clicked an option in the credit-limit upsell.
#[derive(Serialize)]
pub struct CreditLimitUpsellClicked {
    pub surface: CreditLimitUpsellSurface,
    pub choice: CreditLimitChoice,
}

// ---------------------------------------------------------------------------
// Subscription conversion
// ---------------------------------------------------------------------------

/// Emitted when a previously access-gated user re-authenticates and the gate
/// is lifted — i.e. they subscribed (externally on cloud.echo-agent.invalid) and came back.
/// This is the actual conversion signal for EchoAgent Heavy subscriptions
/// attributed to EchoAgent Build: the user saw the gate in EchoAgent Build, went and
/// paid, then returned with access.
#[derive(Serialize)]
pub struct SubscriptionActivated {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
    /// Whether the subscribe CTA was shown in this session before the gate
    /// was lifted (`access_gate_shown_logged`). When `true`, the conversion
    /// is strongly attributable to EchoAgent Build's upsell surface.
    pub upsell_shown_this_session: bool,
}

/// Why auth recovery could not refresh the credential, forcing the user to
/// manually re-authenticate. Mapped from shell's `AuthError`; only terminal
/// failures map — transient ones don't emit (recovery retries).
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ManualAuthReason {
    /// IdP rejected the refresh token (`invalid_grant`); a re-login is required.
    RefreshTokenRejected,
    /// Token type has no refresh authority (API key / legacy / OIDC sans refresh token).
    NoRefreshAuthority,
    /// The operator's auth-provider command could not mint a credential
    /// unattended, so only an interactive run of it can restore the session.
    ProviderInteractiveRequired,
    RecoveryExhausted,
    TokenExpiredNoRefresh,
    /// Recovered session violated the `force_login_team_uuid` pin.
    WrongTeam,
}

/// User-facing surface where the manual re-auth was triggered. Background
/// recoveries (storage/telemetry uploads) do not emit this event.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ManualAuthSurface {
    /// A chat/inference turn (the yellow `ReAuthRequired` banner).
    Turn,
    /// The relay / leader connection handshake.
    Relay,
}

/// The kind of bearer that was rejected. Mirrors shell's `TokenType` as a
/// stable wire enum (don't serialize the shell `Debug` repr).
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AuthTokenKind {
    OidcSession,
    ExternalBinary,
    LegacySession,
    ApiKey,
    None,
}

/// KPI: a user-facing 401 recovery (`Turn`/`Relay`) terminally failed, forcing a
/// manual re-login. Product-events only (no external export).
///
/// Alerting contract: the event lands under the Shell-origin name
/// `echo-agent-shell-manual_auth` (the `manual_auth` binding gets the `echo-agent-shell-`
/// prefix at emit). Count `distinct(principal)`, never raw events — the debounce
/// is a single slot per process (repeats on the most-recent dead credential
/// collapse; alternating credentials can re-emit), and `trigger` is whichever
/// surface fired first, not a reliable per-surface split. `principal` is absent for
/// unattributed lockouts (all collapse into one NULL bucket). API-key sessions
/// are excluded (a 401 there means rotate the key, not `/login`).
// `Debug`/`Clone`/`PartialEq` let shell tests assert the emitted event by value
// (a downstream crate's `cfg(test)` can't turn on `cfg_attr(test, ...)` here).
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ManualAuth {
    pub reason: ManualAuthReason,
    pub trigger: ManualAuthSurface,
    pub token_kind: AuthTokenKind,
    /// `user_id` of the locked-out account, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

/// Release channel bucketed to the known set: channel is free-text user
/// config, and recording it verbatim would leak private mirror names.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliUpdateChannel {
    Stable,
    Alpha,
    Enterprise,
    Other,
}

impl CliUpdateChannel {
    /// Empty means stable — the installers' default (mirrors the updater's
    /// `is_stable_channel`).
    pub fn from_channel_str(raw: &str) -> Self {
        match raw.trim() {
            "" | "stable" => Self::Stable,
            "alpha" => Self::Alpha,
            "enterprise" => Self::Enterprise,
            _ => Self::Other,
        }
    }
}

/// One attempt to download + activate a new `echoagent` binary. Analytics name:
/// `echo-agent-shell-cli_update`. Emitted on failure too; failures carry the
/// typed `error_kind` only — freeform strings leak home paths.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct CliUpdate {
    pub outcome: CliUpdateOutcome,
    pub trigger: CliUpdateTrigger,
    pub from_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_version: Option<String>,
    pub channel: CliUpdateChannel,
    pub installer: CliUpdateInstaller,
    /// `{os}-{arch}` from platform detection — closed by construction.
    pub platform: String,
    pub rosetta: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<CliUpdateErrorKind>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Event name bindings
// ─────────────────────────────────────────────────────────────────────────────

telemetry_event!(ManualAuth, "manual_auth");
telemetry_event!(AuthLockWait, "auth_lock_wait");
telemetry_event!(AuthLockTimeout, "auth_lock_timeout");
telemetry_event!(
    AuthLockReplacedOutFromUnder,
    "auth_lock_replaced_out_from_under"
);
telemetry_event!(CliUpdate, "cli_update");

telemetry_event!(Login, "login", external = crate::external::schema::map_auth);
telemetry_event!(LoginPickerShown, "login_picker_shown");
telemetry_event!(LoginMethodChosen, "login_method_chosen");
telemetry_event!(LoginCompleted, "login_completed");
telemetry_event!(LoginFailed, "login_failed");
telemetry_event!(LoginAbandoned, "login_abandoned");
telemetry_event!(ApiKeySaveResult, "api_key_save_result");
telemetry_event!(
    PlanModeToggled,
    "plan_mode_toggled",
    external = crate::external::schema::map_plan_mode_toggled
);
telemetry_event!(
    ContextualTip,
    "contextual_tip",
    external = crate::external::schema::map_contextual_tip
);
telemetry_event!(PromptSuggestion, "prompt_suggestion");
telemetry_event!(
    YoloToggled,
    "yolo_toggled",
    external = crate::external::schema::map_yolo_toggled
);
telemetry_event!(SlashCommandUsed, "slash_command_used");
telemetry_event!(PermissionPrompted, "permission_prompted");
telemetry_event!(
    PermissionDecisionPayload,
    "permission_decision",
    external = crate::external::schema::map_tool_decision
);
telemetry_event!(AutoCompactFired, "auto_compact_fired");
telemetry_event!(CompactionTriggered, "compaction_triggered");
telemetry_event!(
    CompactionCompleted,
    "compaction_completed",
    external = crate::external::schema::map_compaction
);
telemetry_event!(AutoCompactSuppressed, "auto_compact_suppressed");
telemetry_event!(CompactionRetryDegraded, "compaction_retry_degraded");
telemetry_event!(
    SubagentLaunched,
    "subagent_launched",
    external = crate::external::schema::map_subagent_launched
);
telemetry_event!(
    SubagentCompleted,
    "subagent_completed",
    external = crate::external::schema::map_subagent_completed
);
telemetry_event!(SubagentLimitHit, "subagent_limit_hit");
telemetry_event!(SubagentRateLimitWaited, "subagent_rate_limit_waited");
telemetry_event!(WorkflowRunStarted, "workflow_run_started");
telemetry_event!(WorkflowRunEnded, "workflow_run_ended");
telemetry_event!(
    ModelSwitched,
    "model_switched",
    external = crate::external::schema::map_model_switched
);
telemetry_event!(PluginAdded, "plugin_added");
telemetry_event!(PluginRemoved, "plugin_removed");
telemetry_event!(
    PluginInstalled,
    "plugin_installed",
    external = crate::external::schema::map_plugin_installed
);
telemetry_event!(PluginUninstalled, "plugin_uninstalled");
telemetry_event!(PluginReloaded, "plugin_reloaded");
telemetry_event!(
    PluginUsed,
    "plugin_used",
    external = crate::external::schema::map_plugin_used
);
telemetry_event!(PluginCtaImpression, "plugin_cta_impression");
telemetry_event!(PluginCtaConnectClicked, "plugin_cta_connect_clicked");
telemetry_event!(PluginCtaDismissed, "plugin_cta_dismissed");
telemetry_event!(PluginCtaInstalled, "plugin_cta_installed");
telemetry_event!(ExtensionsModalOpened, "extensions_modal_opened");
telemetry_event!(ExtensionsModalAction, "extensions_modal_action");
telemetry_event!(HookAdded, "hook_added");
telemetry_event!(HookRemoved, "hook_removed");
telemetry_event!(HookTrusted, "hook_trusted");
telemetry_event!(HookExecuted, "hook_executed");
telemetry_event!(HookBlocked, "hook_blocked");
telemetry_event!(ClientHookGate, "client_hook_gate");
telemetry_event!(SkillAdded, "skill_added");
telemetry_event!(SkillRemoved, "skill_removed");
telemetry_event!(
    SkillDispatched,
    "skill_dispatched",
    external = crate::external::schema::map_skill_activated
);
telemetry_event!(
    McpServerConnected,
    "mcp_server_connected",
    external = crate::external::schema::map_mcp_server_connected
);
telemetry_event!(
    McpServerFailed,
    "mcp_server_failed",
    external = crate::external::schema::map_mcp_server_failed
);
telemetry_event!(McpInitCompleted, "mcp_init_completed");
telemetry_event!(McpToolCalled, "mcp_tool_called");
telemetry_event!(
    SessionHarness,
    "session_harness",
    external = crate::external::schema::map_session_start
);
telemetry_event!(SessionLoad, "session_load");
telemetry_event!(
    SessionNew,
    "session_new",
    external = crate::external::schema::map_session_new
);
telemetry_event!(
    PromptSubmitted,
    "prompt_submitted",
    external = crate::external::schema::map_user_prompt
);
telemetry_event!(UserFeedback, "user_feedback");
telemetry_event!(RolloutSurvey, "rollout_survey");
telemetry_event!(PrCreated, "pr_created");
telemetry_event!(PrMerged, "pr_merged");
telemetry_event!(MultiAgentFollowup, "multi_agent_followup");
telemetry_event!(MultiAgentApply, "multi_agent_apply");
telemetry_event!(MultiAgentDiscard, "multi_agent_discard");
telemetry_event!(RepoChanges, "repo_changes");
telemetry_event!(NonGitDecisionEvent, "non_git_decision");
telemetry_event!(PromptLatency, "prompt_latency");
telemetry_event!(CancellationCompleted, "cancellation_completed");
telemetry_event!(HeapThresholdCrossed, "heap_threshold_crossed");
telemetry_event!(ProcessResourceUsage, "process_resource_usage");
telemetry_event!(ProcessResourceLimits, "process_resource_limits");
telemetry_event!(
    TurnCompleted,
    "turn_completed",
    external = crate::external::schema::map_turn_completed
);
telemetry_event!(ShellTrueNoop, "shell_true_noop");
telemetry_event!(ActionStationarityNudge, "action_stationarity_nudge");
telemetry_event!(ActionStationarityStop, "action_stationarity_stop");
telemetry_event!(
    ToolCallCompleted,
    "tool_call_completed",
    external = crate::external::schema::map_tool_result
);
telemetry_event!(
    ModelResponseReceived,
    "model_response_received",
    external = crate::external::schema::map_api_request
);
telemetry_event!(MemoryFlushed, "memory_flushed");
telemetry_event!(MediaGenerated, "media_generated");
telemetry_event!(
    SessionEnded,
    "session_ended",
    external = crate::external::schema::map_session_end
);
telemetry_event!(
    AgentConnect,
    "agent_connect",
    external = crate::external::schema::map_agent_connect
);
telemetry_event!(
    StartupCompleted,
    "startup_completed",
    external = crate::external::schema::map_startup_completed
);
telemetry_event!(PagerSlashCommand, "pager_slash_command");
telemetry_event!(PlanSubmit, "plan_submit");
telemetry_event!(EventLoopStall, "event_loop_stall");
telemetry_event!(EchoAgentUpsellShown, "echoagent_upsell_shown");
telemetry_event!(EchoAgentUpsellClicked, "echoagent_upsell_clicked");
telemetry_event!(AnnouncementCtaShown, "announcement_cta_shown");
telemetry_event!(AnnouncementCtaClicked, "announcement_cta_clicked");
telemetry_event!(CodingDataConsentSelected, "coding_data_consent_selected");
telemetry_event!(FeedbackTraceCardShown, "feedback_trace_card_shown");
telemetry_event!(
    FeedbackTraceConsentSelected,
    "feedback_trace_consent_selected"
);
telemetry_event!(TerminalTelemetry, "terminal_context");
telemetry_event!(DisplayRefreshProbe, "display_refresh_probe");
telemetry_event!(BackspaceNoEffect, "backspace_no_effect");
telemetry_event!(ClipboardImagePaste, "clipboard_image_paste");
telemetry_event!(PasteKeyEmptyHostClipboard, "paste_key_empty_host_clipboard");
telemetry_event!(ClipboardCopy, "clipboard_copy");
telemetry_event!(NotificationEmitted, "notification_emitted");
telemetry_event!(DashboardOpened, "dashboard_opened");
telemetry_event!(DashboardClosed, "dashboard_closed");
telemetry_event!(DashboardAgentAttached, "dashboard_agent_attached");
telemetry_event!(DashboardAgentLaunched, "dashboard_agent_launched");
telemetry_event!(ShortcutUsed, "shortcut_used");
telemetry_event!(
    RateLimitHit,
    "rate_limit_hit",
    external = crate::external::schema::map_rate_limit_hit
);
telemetry_event!(CreditLimitHit, "credit_limit_hit");
telemetry_event!(CreditLimitUpsellShown, "credit_limit_upsell_shown");
telemetry_event!(CreditLimitUpsellClicked, "credit_limit_upsell_clicked");
telemetry_event!(SubscriptionActivated, "subscription_activated");
telemetry_event!(StatusLineConfigured, "status_line_configured");
telemetry_event!(StatusLineHealth, "status_line_health");
telemetry_event!(
    ApiError,
    "api_error",
    external = crate::external::schema::map_api_error
);
telemetry_event!(
    InternalError,
    "internal_error",
    external = crate::external::schema::map_internal_error
);
telemetry_event!(ExternalOtelConfigured, "external_otel_configured");
telemetry_event!(
    ExternalOtelRemotePolicyApplied,
    "external_otel_remote_policy_applied"
);
telemetry_event!(ExternalOtelExportHealth, "external_otel_export_health");

// Session lifecycle (structs in session_metrics)
telemetry_event!(crate::session_metrics::SessionStarted, "session_started");
telemetry_event!(crate::session_metrics::Turn, "turn");
telemetry_event!(
    crate::session_metrics::TurnCompletedLifecycle,
    "turn_completed_lifecycle"
);
telemetry_event!(
    crate::session_metrics::DoomLoopRecovery,
    "doom_loop_recovery"
);
telemetry_event!(
    crate::session_metrics::TraceUploadAttempted,
    "trace_upload_attempted"
);
telemetry_event!(
    crate::session_metrics::TraceUploadSucceeded,
    "trace_upload_succeeded"
);
telemetry_event!(
    crate::session_metrics::TraceUploadSkipped,
    "trace_upload_skipped"
);
telemetry_event!(
    crate::session_metrics::TraceUploadFailed,
    "trace_upload_failed"
);

// Memory subsystem (structs in memory_telemetry)
telemetry_event!(
    crate::memory_telemetry::MemorySessionInit,
    "memory_session_init"
);
telemetry_event!(crate::memory_telemetry::MemorySearch, "memory_search");
telemetry_event!(
    crate::memory_telemetry::MemoryFlushStart,
    "memory_flush_start"
);
telemetry_event!(
    crate::memory_telemetry::MemoryFlushComplete,
    "memory_flush_complete"
);
telemetry_event!(crate::memory_telemetry::MemoryInjection, "memory_injection");
telemetry_event!(crate::memory_telemetry::MemoryReindex, "memory_reindex");
telemetry_event!(
    crate::memory_telemetry::MemoryWatcherSync,
    "memory_watcher_sync"
);
telemetry_event!(
    crate::memory_telemetry::MemorySessionSummary,
    "memory_session_summary"
);

#[cfg(test)]
mod tests {
    /// Reserved keys insert only-if-absent, so an event field that collides
    /// intentionally wins over the enrichment. Walk every registered event's
    /// fields from source and pin the intentional shadows, so a new event
    /// cannot silently shadow a reserved key.
    #[test]
    fn event_fields_shadow_reserved_keys_only_on_the_allowlist() {
        const SOURCES: &[&str] = &[
            include_str!("mod.rs"),
            include_str!("permission_analytics.rs"),
            include_str!("../session_metrics.rs"),
            include_str!("../memory_telemetry.rs"),
        ];

        let mut registry: Vec<&str> = Vec::new();
        for src in SOURCES {
            for chunk in src.split("telemetry_event!(").skip(1) {
                let path = chunk
                    .trim_start()
                    .split(',')
                    .next()
                    .unwrap_or_default()
                    .trim();
                let name = path.rsplit("::").next().unwrap_or(path);
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    registry.push(name);
                }
            }
        }

        let mut fields: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
        for src in SOURCES {
            let mut lines = src.lines();
            while let Some(line) = lines.next() {
                let Some(decl) = line.trim_start().strip_prefix("pub struct ") else {
                    continue;
                };
                let name = decl
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or_default();
                let entry = fields.entry(name).or_default();
                if !decl.contains('{') || decl.contains('}') {
                    continue;
                }
                for body in lines.by_ref() {
                    if body == "}" {
                        break;
                    }
                    let b = body.trim_start();
                    if b.starts_with("//") || b.starts_with('#') {
                        continue;
                    }
                    let b = b.strip_prefix("pub ").unwrap_or(b);
                    if let Some((ident, _)) = b.split_once(':')
                        && !ident.is_empty()
                        && ident
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    {
                        entry.push(ident.to_string());
                    }
                }
            }
        }

        let reserved: std::collections::BTreeSet<&str> =
            crate::client::RESERVED_EVENT_KEYS.iter().copied().collect();
        let mut shadows: std::collections::BTreeSet<(String, String)> = Default::default();
        let mut seen = std::collections::BTreeSet::new();
        for event in registry {
            assert!(seen.insert(event), "event {event} registered twice");
            let event_fields = fields
                .get(event)
                .unwrap_or_else(|| panic!("registered event {event} has no parsed struct"));
            for field in event_fields {
                if reserved.contains(field.as_str()) {
                    shadows.insert((event.to_string(), field.clone()));
                }
            }
        }
        assert!(
            seen.len() > 100,
            "the registry walk collapsed: {}",
            seen.len()
        );

        const ALLOWED: &[(&str, &str)] = &[
            ("DoomLoopRecovery", "session_id"),
            ("DoomLoopRecovery", "turn_number"),
            ("MemoryFlushComplete", "session_id"),
            ("MemoryFlushStart", "session_id"),
            ("MemoryInjection", "session_id"),
            ("MemoryReindex", "session_id"),
            ("MemorySearch", "session_id"),
            ("MemorySessionInit", "session_id"),
            ("MemorySessionSummary", "session_id"),
            ("MemoryWatcherSync", "session_id"),
            ("ModelSwitched", "session_id"),
            ("NonGitDecisionEvent", "session_id"),
            ("ProcessResourceUsage", "footprint_bytes"),
            ("ProcessResourceUsage", "rss_bytes"),
            ("RolloutSurvey", "session_id"),
            ("SessionHarness", "session_id"),
            ("SessionLoad", "session_id"),
            ("SessionNew", "session_id"),
            ("SessionStarted", "session_id"),
            ("TraceUploadAttempted", "session_id"),
            ("TraceUploadAttempted", "turn_number"),
            ("TraceUploadFailed", "session_id"),
            ("TraceUploadFailed", "turn_number"),
            ("TraceUploadSkipped", "session_id"),
            ("TraceUploadSkipped", "turn_number"),
            ("TraceUploadSucceeded", "session_id"),
            ("TraceUploadSucceeded", "turn_number"),
            ("Turn", "session_id"),
            ("Turn", "turn_number"),
            ("TurnCompletedLifecycle", "session_id"),
            ("TurnCompletedLifecycle", "turn_number"),
            ("UserFeedback", "session_id"),
        ];
        let allowed: std::collections::BTreeSet<(String, String)> = ALLOWED
            .iter()
            .map(|(s, f)| (s.to_string(), f.to_string()))
            .collect();
        assert_eq!(
            shadows, allowed,
            "reserved-key shadows changed; extend the allowlist only for intentional event-owned values"
        );
    }

    use super::*;

    #[test]
    fn process_resource_usage_omits_allocated_bytes_when_unavailable() {
        assert_eq!(
            serde_json::to_value(ProcessResourceUsage {
                trigger: ResourceReportTrigger::Periodic,
                rss_bytes: None,
                peak_rss_bytes: None,
                footprint_bytes: None,
                allocated_bytes: Some(4_096),
                threads: None,
                open_files: None,
                resident_sessions: 2,
                session_threads: 3,
            })
            .unwrap(),
            serde_json::json!({
                "trigger": "periodic",
                "allocated_bytes": 4_096,
                "resident_sessions": 2,
                "session_threads": 3,
            })
        );
        assert_eq!(
            serde_json::to_value(ProcessResourceUsage {
                trigger: ResourceReportTrigger::Periodic,
                rss_bytes: None,
                peak_rss_bytes: None,
                footprint_bytes: None,
                allocated_bytes: None,
                threads: None,
                open_files: None,
                resident_sessions: 2,
                session_threads: 3,
            })
            .unwrap(),
            serde_json::json!({
                "trigger": "periodic",
                "resident_sessions": 2,
                "session_threads": 3,
            })
        );
    }

    #[test]
    fn tool_call_completed_omits_tool_result_size_bytes_when_absent() {
        assert_eq!(
            serde_json::to_value(ToolCallCompleted {
                tool_name: "bash".into(),
                outcome: echo_agent_session_events::types::ToolOutcome::Success,
                duration_ms: 7,
                tool_result_size_bytes: Some(2_048),
                file_path: None,
                parameters: None,
            })
            .unwrap(),
            serde_json::json!({
                "tool_name": "bash",
                "outcome": "success",
                "duration_ms": 7,
                "tool_result_size_bytes": 2_048,
            })
        );
        assert_eq!(
            serde_json::to_value(ToolCallCompleted {
                tool_name: "bash".into(),
                outcome: echo_agent_session_events::types::ToolOutcome::Success,
                duration_ms: 7,
                tool_result_size_bytes: None,
                file_path: None,
                parameters: None,
            })
            .unwrap(),
            serde_json::json!({
                "tool_name": "bash",
                "outcome": "success",
                "duration_ms": 7,
            })
        );
    }

    #[test]
    fn auth_lock_wait_event_carries_wait_and_budget() {
        assert_eq!(
            serde_json::to_value(AuthLockWait {
                wait_ms: 4321,
                budget_ms: 25_000,
            })
            .unwrap(),
            serde_json::json!({ "wait_ms": 4321, "budget_ms": 25_000 })
        );
    }

    #[test]
    fn auth_lock_timeout_event_omits_an_unknown_holder_state() {
        assert_eq!(
            serde_json::to_value(AuthLockTimeout {
                budget_ms: 25_000,
                holder_state: Some("stuck_live"),
            })
            .unwrap(),
            serde_json::json!({ "budget_ms": 25_000, "holder_state": "stuck_live" })
        );
        assert_eq!(
            serde_json::to_value(AuthLockTimeout {
                budget_ms: 10_000,
                holder_state: None,
            })
            .unwrap(),
            serde_json::json!({ "budget_ms": 10_000 })
        );
    }

    #[test]
    fn auth_lock_replaced_event_omits_unknown_holder_fields() {
        assert_eq!(
            serde_json::to_value(AuthLockReplacedOutFromUnder {
                holder_pid: Some(42),
                holder_state: Some("alive"),
                holder_age_secs: None,
            })
            .unwrap(),
            serde_json::json!({ "holder_pid": 42, "holder_state": "alive" })
        );
    }

    fn terminal_telemetry_fixture() -> TerminalTelemetry {
        TerminalTelemetry {
            brand: "Unknown".into(),
            multiplexer: "none".into(),
            is_ssh: true,
            is_byobu: false,
            term_var: "xterm-256color".into(),
            tmux_version: "".into(),
            xtversion: "".into(),
            term_version: "".into(),
            term_version_source: "none".into(),
            kitty_event_types_withheld: false,
            host_os: "linux".into(),
            display_server: "unknown".into(),
            modifier_cmd_fate: "unknown".into(),
            modifier_opt_fate: "unknown".into(),
            enter_modifier_fate: "unknown".into(),
            hyperlink_osc8: "unknown".into(),
            hyperlink_skip_reason: "none".into(),
            clipboard_route: "native+osc52".into(),
            clipboard_native_tool: "arboard".into(),
            clipboard_data_control: "n/a".into(),
        }
    }

    #[test]
    fn memory_retrieval_mode_serializes_as_closed_snake_case_values() {
        let modes = [
            MemoryRetrievalMode::Disabled,
            MemoryRetrievalMode::FtsOnly,
            MemoryRetrievalMode::Hybrid,
        ];
        assert_eq!(
            modes.map(|mode| serde_json::to_value(mode).unwrap()),
            ["disabled", "fts_only", "hybrid"]
        );
    }

    #[test]
    fn clipboard_copy_serialization_preserves_boolean_and_adds_delivery_evidence() {
        for delivery in ["confirmed", "unverified", "failed"] {
            let value = serde_json::to_value(ClipboardCopy {
                terminal: terminal_telemetry_fixture(),
                source: "copy_text",
                text_len: 12,
                route_native: true,
                route_tmux: false,
                route_osc52: true,
                route_label: "native+osc52".into(),
                cli_tools_tried: String::new(),
                cli_ok_tools: String::new(),
                cli_ok: false,
                arboard_ok: false,
                data_control: false,
                tmux_ok: false,
                osc52_ok: true,
                delivery,
                osc52_sink: false,
                container_no_display: false,
                reported_success: delivery != "failed",
                toast_kind: "unverified_osc_remote",
                duration_ms: 1,
            })
            .unwrap();
            assert_eq!(value["delivery"], serde_json::json!(delivery));
            assert_eq!(
                value["reported_success"],
                serde_json::Value::Bool(delivery != "failed")
            );
            assert_eq!(value["osc52_sink"], serde_json::json!(false));
            assert_eq!(value["container_no_display"], serde_json::json!(false));
        }
    }

    #[test]
    fn manual_auth_name_and_shape() {
        assert_eq!(ManualAuth::NAME, "manual_auth");

        let with_principal = serde_json::to_value(ManualAuth {
            reason: ManualAuthReason::RefreshTokenRejected,
            trigger: ManualAuthSurface::Turn,
            token_kind: AuthTokenKind::OidcSession,
            principal: Some("user-1".into()),
        })
        .unwrap();
        assert_eq!(
            with_principal,
            serde_json::json!({
                "reason": "refresh_token_rejected",
                "trigger": "turn",
                "token_kind": "oidc_session",
                "principal": "user-1",
            })
        );

        // `principal` is omitted (not null) when unknown. `LegacySession` is a
        // reachable fixture (API-key sessions never emit this event).
        let no_principal = serde_json::to_value(ManualAuth {
            reason: ManualAuthReason::NoRefreshAuthority,
            trigger: ManualAuthSurface::Relay,
            token_kind: AuthTokenKind::LegacySession,
            principal: None,
        })
        .unwrap();
        assert!(!no_principal.as_object().unwrap().contains_key("principal"));
    }

    #[test]
    fn plugin_cta_event_names() {
        assert_eq!(PluginCtaImpression::NAME, "plugin_cta_impression");
        assert_eq!(PluginCtaConnectClicked::NAME, "plugin_cta_connect_clicked");
        assert_eq!(PluginCtaDismissed::NAME, "plugin_cta_dismissed");
        assert_eq!(PluginCtaInstalled::NAME, "plugin_cta_installed");
    }

    #[test]
    fn announcement_cta_event_names() {
        assert_eq!(AnnouncementCtaShown::NAME, "announcement_cta_shown");
        assert_eq!(AnnouncementCtaClicked::NAME, "announcement_cta_clicked");
    }

    #[test]
    fn shortcut_used_name_and_shape() {
        assert_eq!(ShortcutUsed::NAME, "shortcut_used");
        let value = serde_json::to_value(ShortcutUsed {
            key: "Ctrl+L".into(),
            action: "interject_prompt".into(),
            context: "prompt_focused".into(),
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "key": "Ctrl+L",
                "action": "interject_prompt",
                "context": "prompt_focused",
            })
        );
    }

    #[test]
    fn coding_data_consent_selected_name_and_shape() {
        assert_eq!(
            CodingDataConsentSelected::NAME,
            "coding_data_consent_selected"
        );
        let event = serde_json::to_value(CodingDataConsentSelected {
            source: CodingDataConsentSource::Settings,
            choice: CodingDataConsentChoice::OptIn,
            previous_choice: CodingDataConsentChoice::OptIn,
            changed: false,
        })
        .unwrap();
        assert_eq!(
            event,
            serde_json::json!({
                "source": "settings",
                "choice": "opt_in",
                "previous_choice": "opt_in",
                "changed": false,
            })
        );
    }

    /// Serde renames the payload field, strum renders the external label: two snake_case implementations, so pin that they agree on every variant.
    #[test]
    fn skill_trigger_serializes_the_same_string_strum_yields() {
        for trigger in [
            SkillTrigger::SlashCommand,
            SkillTrigger::SkillMdRead,
            SkillTrigger::SkillTool,
        ] {
            let serde = serde_json::to_value(SkillDispatched {
                skill_name: "pdf".into(),
                plugin_source: None,
                trigger,
            })
            .unwrap();
            assert_eq!(
                serde,
                serde_json::json!({ "skill_name": "pdf", "trigger": <&'static str>::from(trigger) })
            );
        }
    }

    #[test]
    fn compaction_retry_degraded_name_and_shape() {
        assert_eq!(CompactionRetryDegraded::NAME, "compaction_retry_degraded");

        let degenerate = serde_json::to_value(CompactionRetryDegraded {
            trigger: CompactionTrigger::Auto,
            reason: "degenerate_summary",
            from_stage: None,
            to_stage: None,
            summary_chars: Some(130),
            attempt: 1,
            context_window: 128_000,
            compaction_id: "cid-1".into(),
        })
        .unwrap();
        assert_eq!(
            degenerate,
            serde_json::json!({
                "trigger": "auto",
                "reason": "degenerate_summary",
                "summary_chars": 130,
                "attempt": 1,
                "context_window": 128_000,
                "compaction_id": "cid-1",
            })
        );

        let overflow = serde_json::to_value(CompactionRetryDegraded {
            trigger: CompactionTrigger::Manual,
            reason: "input_overflow",
            from_stage: Some("verbatim"),
            to_stage: Some("verbatim_fitted"),
            summary_chars: None,
            attempt: 2,
            context_window: 128_000,
            compaction_id: "cid-2".into(),
        })
        .unwrap();
        assert_eq!(
            overflow,
            serde_json::json!({
                "trigger": "manual",
                "reason": "input_overflow",
                "from_stage": "verbatim",
                "to_stage": "verbatim_fitted",
                "attempt": 2,
                "context_window": 128_000,
                "compaction_id": "cid-2",
            })
        );
    }

    #[test]
    fn compaction_triggered_name_and_shape() {
        assert_eq!(CompactionTriggered::NAME, "compaction_triggered");
        let event = serde_json::to_value(CompactionTriggered {
            trigger: CompactionTrigger::Auto,
            tokens_used: 100_000,
            context_window: 128_000,
            percentage: 78,
            model_id: "echo-agent-4".into(),
            user_context_provided: false,
            compaction_id: "cid-1".into(),
            compaction_mode: CompactionModeLabel::Segments,
            two_pass_enabled: true,
            is_subagent: false,
        })
        .unwrap();
        assert_eq!(
            event,
            serde_json::json!({
                "trigger": "auto",
                "tokens_used": 100_000,
                "context_window": 128_000,
                "percentage": 78,
                "model_id": "echo-agent-4",
                "user_context_provided": false,
                "compaction_id": "cid-1",
                "compaction_mode": "segments",
                "two_pass_enabled": true,
                "is_subagent": false,
            })
        );

        let disarmed = serde_json::to_value(CompactionTriggered {
            trigger: CompactionTrigger::Manual,
            tokens_used: 10_000,
            context_window: 128_000,
            percentage: 8,
            model_id: "echo-agent-4".into(),
            user_context_provided: false,
            compaction_id: "cid-2".into(),
            compaction_mode: CompactionModeLabel::Summary,
            two_pass_enabled: false,
            is_subagent: false,
        })
        .unwrap();
        assert_eq!(
            disarmed,
            serde_json::json!({
                "trigger": "manual",
                "tokens_used": 10_000,
                "context_window": 128_000,
                "percentage": 8,
                "model_id": "echo-agent-4",
                "user_context_provided": false,
                "compaction_id": "cid-2",
                "compaction_mode": "summary",
                "two_pass_enabled": false,
                "is_subagent": false,
            })
        );
    }

    #[test]
    fn compaction_completed_name_and_shape() {
        assert_eq!(CompactionCompleted::NAME, "compaction_completed");
        let with_model = serde_json::to_value(CompactionCompleted {
            duration_ms: 63_000,
            tokens_before: 399_000,
            tokens_after: 15_000,
            model_id: Some("echo-agent-4".into()),
            compaction_id: "cid-1".into(),
            compaction_mode: CompactionModeLabel::Summary,
            two_pass: TwoPassOutcome::Used,
            segments_written: 0,
            degenerate_retries: 1,
            input_overflow_retries: 2,
            is_subagent: false,
            model_wait_ms: None,
            pre_compaction_ms: None,
            post_compaction_ms: None,
        })
        .unwrap();
        assert_eq!(
            with_model,
            serde_json::json!({
                "duration_ms": 63_000,
                "tokens_before": 399_000,
                "tokens_after": 15_000,
                "model_id": "echo-agent-4",
                "compaction_id": "cid-1",
                "compaction_mode": "summary",
                "two_pass": "used",
                "segments_written": 0,
                "degenerate_retries": 1,
                "input_overflow_retries": 2,
                "is_subagent": false,
            })
        );

        let no_model = serde_json::to_value(CompactionCompleted {
            duration_ms: 1,
            tokens_before: 1,
            tokens_after: 1,
            model_id: None,
            compaction_id: "cid-2".into(),
            compaction_mode: CompactionModeLabel::Transcript,
            two_pass: TwoPassOutcome::Disabled,
            segments_written: 0,
            degenerate_retries: 0,
            input_overflow_retries: 0,
            is_subagent: true,
            model_wait_ms: None,
            pre_compaction_ms: None,
            post_compaction_ms: None,
        })
        .unwrap();
        assert_eq!(
            no_model,
            serde_json::json!({
                "duration_ms": 1,
                "tokens_before": 1,
                "tokens_after": 1,
                "compaction_id": "cid-2",
                "compaction_mode": "transcript",
                "two_pass": "disabled",
                "segments_written": 0,
                "degenerate_retries": 0,
                "input_overflow_retries": 0,
                "is_subagent": true,
            })
        );

        let miss = serde_json::to_value(CompactionCompleted {
            duration_ms: 2,
            tokens_before: 2,
            tokens_after: 2,
            model_id: None,
            compaction_id: "cid-3".into(),
            compaction_mode: CompactionModeLabel::Segments,
            two_pass: TwoPassOutcome::Miss,
            segments_written: 1,
            degenerate_retries: 0,
            input_overflow_retries: 0,
            is_subagent: false,
            model_wait_ms: None,
            pre_compaction_ms: None,
            post_compaction_ms: None,
        })
        .unwrap();
        assert_eq!(
            miss,
            serde_json::json!({
                "duration_ms": 2,
                "tokens_before": 2,
                "tokens_after": 2,
                "compaction_id": "cid-3",
                "compaction_mode": "segments",
                "two_pass": "miss",
                "segments_written": 1,
                "degenerate_retries": 0,
                "input_overflow_retries": 0,
                "is_subagent": false,
            })
        );
    }

    #[test]
    fn resolve_two_pass_covers_armed_and_used() {
        assert_eq!(resolve_two_pass(false, false), TwoPassOutcome::Disabled);
        assert_eq!(resolve_two_pass(false, true), TwoPassOutcome::Disabled);
        assert_eq!(resolve_two_pass(true, false), TwoPassOutcome::Miss);
        assert_eq!(resolve_two_pass(true, true), TwoPassOutcome::Used);
    }

    #[test]
    fn two_pass_outcome_and_mode_label_serialize_snake_case() {
        for outcome in [
            TwoPassOutcome::Disabled,
            TwoPassOutcome::Miss,
            TwoPassOutcome::Used,
        ] {
            let expected = match outcome {
                TwoPassOutcome::Disabled => "disabled",
                TwoPassOutcome::Miss => "miss",
                TwoPassOutcome::Used => "used",
            };
            assert_eq!(
                serde_json::to_value(outcome).unwrap(),
                serde_json::json!(expected)
            );
        }
        for mode in [
            CompactionModeLabel::Summary,
            CompactionModeLabel::Transcript,
            CompactionModeLabel::Segments,
        ] {
            let expected = match mode {
                CompactionModeLabel::Summary => "summary",
                CompactionModeLabel::Transcript => "transcript",
                CompactionModeLabel::Segments => "segments",
            };
            assert_eq!(
                serde_json::to_value(mode).unwrap(),
                serde_json::json!(expected)
            );
        }
    }

    #[test]
    fn plugin_cta_impression_serializes_plugin_name() {
        let v = serde_json::to_value(PluginCtaImpression {
            plugin_name: "figma".into(),
        })
        .unwrap();
        assert_eq!(v, serde_json::json!({ "plugin_name": "figma" }));
    }

    #[test]
    fn plugin_cta_connect_clicked_serializes_is_retry() {
        let fresh = serde_json::to_value(PluginCtaConnectClicked {
            plugin_name: "figma".into(),
            is_retry: false,
        })
        .unwrap();
        assert_eq!(
            fresh,
            serde_json::json!({ "plugin_name": "figma", "is_retry": false })
        );
        let retry = serde_json::to_value(PluginCtaConnectClicked {
            plugin_name: "figma".into(),
            is_retry: true,
        })
        .unwrap();
        assert_eq!(
            retry,
            serde_json::json!({ "plugin_name": "figma", "is_retry": true })
        );
    }

    #[test]
    fn plugin_cta_installed_omits_error_category_when_none() {
        let v = serde_json::to_value(PluginCtaInstalled {
            plugin_name: "figma".into(),
            success: true,
            error_category: None,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "plugin_name": "figma", "success": true })
        );
    }

    #[test]
    fn login_funnel_event_names() {
        assert_eq!(LoginPickerShown::NAME, "login_picker_shown");
        assert_eq!(LoginMethodChosen::NAME, "login_method_chosen");
        assert_eq!(LoginCompleted::NAME, "login_completed");
        assert_eq!(LoginFailed::NAME, "login_failed");
        assert_eq!(LoginAbandoned::NAME, "login_abandoned");
        assert_eq!(ApiKeySaveResult::NAME, "api_key_save_result");
    }

    #[test]
    fn login_completed_serializes_all_fields() {
        let v = serde_json::to_value(LoginCompleted {
            method: "echoagent".into(),
            mode: "device".into(),
            duration_ms: 1234,
            mid_session: false,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "method": "echoagent",
                "mode": "device",
                "duration_ms": 1234,
                "mid_session": false,
            })
        );
    }

    #[test]
    fn login_failed_serializes_kind_and_os_code() {
        let v = serde_json::to_value(LoginFailed {
            error_kind: LoginFailureKind::TransportInterrupted,
            os_error: Some(104),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "error_kind": "transport_interrupted", "os_error": 104 })
        );
    }

    #[test]
    fn login_failed_omits_absent_os_code() {
        let v = serde_json::to_value(LoginFailed {
            error_kind: LoginFailureKind::Decode,
            os_error: None,
        })
        .unwrap();
        assert_eq!(v, serde_json::json!({ "error_kind": "decode" }));
    }

    #[test]
    fn api_key_save_result_omits_error_when_ok() {
        let ok = serde_json::to_value(ApiKeySaveResult {
            ok: true,
            error: None,
        })
        .unwrap();
        assert_eq!(ok, serde_json::json!({ "ok": true }));
        let err = serde_json::to_value(ApiKeySaveResult {
            ok: false,
            error: Some("failed to write key".into()),
        })
        .unwrap();
        assert_eq!(
            err,
            serde_json::json!({ "ok": false, "error": "failed to write key" })
        );
    }

    #[test]
    fn cli_update_event_name_and_serde() {
        assert_eq!(CliUpdate::NAME, "cli_update");
        let ok = serde_json::to_value(CliUpdate {
            outcome: CliUpdateOutcome::Success,
            trigger: CliUpdateTrigger::UserCommand,
            from_version: "0.2.118".into(),
            to_version: Some("0.2.120".into()),
            channel: CliUpdateChannel::Alpha,
            installer: CliUpdateInstaller::Internal,
            platform: "macos-x86_64".into(),
            rosetta: true,
            duration_ms: 12_000,
            error_kind: None,
        })
        .unwrap();
        assert_eq!(
            ok,
            serde_json::json!({
                "outcome": "success",
                "trigger": "user_command",
                "from_version": "0.2.118",
                "to_version": "0.2.120",
                "channel": "alpha",
                "installer": "internal",
                "platform": "macos-x86_64",
                "rosetta": true,
                "duration_ms": 12000,
            })
        );
        let fail = serde_json::to_value(CliUpdate {
            outcome: CliUpdateOutcome::Failed,
            trigger: CliUpdateTrigger::AutoBackground,
            from_version: "0.2.118".into(),
            to_version: Some("0.2.120".into()),
            channel: CliUpdateChannel::Alpha,
            installer: CliUpdateInstaller::Internal,
            platform: "macos-x86_64".into(),
            rosetta: true,
            duration_ms: 60_100,
            error_kind: Some(CliUpdateErrorKind::SmokeTimeout),
        })
        .unwrap();
        assert_eq!(fail["outcome"], "failed");
        assert_eq!(fail["error_kind"], "smoke_timeout");
        assert_eq!(fail["trigger"], "auto_background");
        assert!(fail.get("error").is_none());
        assert_eq!(
            serde_json::to_value(CliUpdateTrigger::LeaderConverge).unwrap(),
            "leader_converge"
        );
        // Trigger as_str / FromStr / serde are one rendering.
        for t in [
            CliUpdateTrigger::UserCommand,
            CliUpdateTrigger::AutoBackground,
            CliUpdateTrigger::LeaderConverge,
        ] {
            assert_eq!(serde_json::to_value(t).unwrap(), t.as_str());
            assert_eq!(t.as_str().parse::<CliUpdateTrigger>().unwrap(), t);
        }
        assert!("bogus".parse::<CliUpdateTrigger>().is_err());
        // Wire values and from_installer_str round-trip — one mapping.
        for (installer, wire) in [
            (CliUpdateInstaller::Npm, "npm"),
            (CliUpdateInstaller::GhRelease, "gh-release"),
            (CliUpdateInstaller::Internal, "internal"),
            (CliUpdateInstaller::Other, "other"),
        ] {
            assert_eq!(serde_json::to_value(installer).unwrap(), wire);
            assert_eq!(CliUpdateInstaller::from_installer_str(wire), installer);
        }
        assert_eq!(
            CliUpdateInstaller::from_installer_str("homebrew"),
            CliUpdateInstaller::Other
        );
    }

    /// Private mirror names bucket to Other; empty means stable.
    #[test]
    fn cli_update_channel_buckets() {
        assert_eq!(
            CliUpdateChannel::from_channel_str(" alpha "),
            CliUpdateChannel::Alpha
        );
        assert_eq!(
            CliUpdateChannel::from_channel_str(""),
            CliUpdateChannel::Stable
        );
        assert_eq!(
            CliUpdateChannel::from_channel_str("stable"),
            CliUpdateChannel::Stable
        );
        assert_eq!(
            CliUpdateChannel::from_channel_str("enterprise"),
            CliUpdateChannel::Enterprise
        );
        for private in ["acme-mirror.1", "x'; rm -rf ~;'", "a b"] {
            assert_eq!(
                CliUpdateChannel::from_channel_str(private),
                CliUpdateChannel::Other,
                "{private:?} must bucket to other"
            );
        }
    }

    #[test]
    fn plugin_cta_installed_includes_error_category_when_some() {
        let v = serde_json::to_value(PluginCtaInstalled {
            plugin_name: "figma".into(),
            success: false,
            error_category: Some("not_found".into()),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "plugin_name": "figma",
                "success": false,
                "error_category": "not_found",
            })
        );
    }
}
