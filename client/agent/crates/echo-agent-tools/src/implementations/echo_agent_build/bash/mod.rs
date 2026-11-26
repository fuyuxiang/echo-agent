//! `run_terminal_cmd` (Bash) tool — new architecture (`Tool` trait).
//!
//! Executes bash commands with optional timeout. Whether shell state (cwd,
//! env vars, aliases) persists between calls depends on the `Terminal`
//! backend: the local backend emulates persistence by capturing and
//! replaying state across commands (see [`crate::computer::local::shell_state`]);
//! other backends may run each command in a fresh session.
//! Supports both foreground (blocking) and background (returns task_id) execution.
//!
//! Optional `find`→`bfs` / `grep`→`ugrep` shadows: see
//! [`crate::computer::local::embedded_search_tools`].
//!
//! ## Resources
//!
//! - `Terminal` — terminal backend for running commands (required)
//! - `Cwd` — working directory (required)
//! - `ToolCallId` — notification correlation + output file naming (required)
//! - `SessionFolder` — output file path prefix (required)
//! - `SessionEnv` — environment variables (optional, defaults empty)
//! - `NotificationHandle` — bash execution notifications (optional, noop fallback)
//! - `TruncationCfg` — max output bytes override (optional)
//! - `TemplateRenderer` — resolve client-facing tool/param names in hints/errors (optional)
//! - `Params<BashParams>` — timeout, output_byte_limit, cmd_prefix (optional, all have defaults)

use std::sync::LazyLock;
use std::time::Duration;

use echo_agent_config::shell::AmpersandSemantics;
use regex::Regex;

use crate::DEFAULT_TOOL_OUTPUT_CHARS;
use crate::computer::types::{ComputerError, TerminalRunRequest};
use crate::notification::types::{
    BashExecutionBackgrounded, BashExecutionComplete, BashExecutionFailed, BashExecutionTimeout,
    BashNotificationBase, BashOutputChunk, PerCallNotificationSink, ToolNotification,
    ToolNotificationHandle,
};
use crate::types::definition::ToolDefinition;
use crate::types::output::{BackgroundTaskStarted, BashOutput};
use crate::types::requirements::{Expr, ToolParamsRequirement, ToolRequirement};
#[allow(unused_imports)]
use crate::types::resources::{
    Cwd, NotificationHandle, Params, SessionEnv, SessionFolder, SharedResources, Terminal,
    TruncationCfg,
};
use crate::types::template_renderer::TemplateRenderer;
use crate::types::tool::{ToolKind, ToolNamespace};

#[derive(thiserror::Error, Debug)]
pub enum BashError {
    #[error("Failed to spawn command: {0}")]
    SpawnFailed(String),

    #[error("Command execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Terminal error: {0}")]
    TerminalError(#[from] ComputerError),
}

fn default_true() -> bool {
    true
}

/// Maximum size, in bytes, of a single emitted progress `delta`. Guards
/// against a pathological single-tick burst (a large accumulation flushed in
/// one ~100 ms tick) flooding the harness in one frame. A delta larger than
/// this is cut on a UTF-8 char boundary and the remainder is held back for the
/// next tick (append is lossless); `total_bytes` still reflects the true
/// monotonic count.
const MAX_PROGRESS_DELTA_BYTES: usize = 16 * 1024;

/// Bash's capabilities incl. its streaming spec (single source of truth):
/// raw stdout is the terminal projection, so `RawTerminal` / `Append`,
/// capped per frame at [`MAX_PROGRESS_DELTA_BYTES`].
static BASH_CAPABILITIES: LazyLock<echo_agent_tool_protocol::ToolCapabilities> =
    LazyLock::new(|| echo_agent_tool_protocol::ToolCapabilities {
        is_read_only: false,
        tool_scope: Some(echo_agent_tool_protocol::ToolScope::Write),
        streaming: Some(echo_agent_tool_protocol::StreamingSpec {
            subkind: "bash_output_chunk".to_owned(),
            max_delta_bytes: Some(MAX_PROGRESS_DELTA_BYTES as u32),
        }),
        ..Default::default()
    });

/// One `ToolProgress` delta from a `BashOutputChunk`; `None` when no new bytes.
fn bash_output_chunk_progress(
    spec: &echo_agent_tool_protocol::StreamingSpec,
    chunk: &BashOutputChunk,
    last_total: &mut usize,
) -> Option<echo_agent_tool_runtime::ToolProgress> {
    // `stream_chunk` counts in `u64`; convert at the boundary.
    let mut cursor = *last_total as u64;
    let progress = echo_agent_tool_runtime::stream_chunk(
        spec,
        &chunk.base.output,
        chunk.base.total_bytes as u64,
        &mut cursor,
        // Cumulative truncation maps to `truncated`; per-tick overflow is `gap`.
        chunk.base.truncated,
    );
    *last_total = cursor as usize;
    progress
}

/// Extract the final [`BashNotificationBase`] carried by a terminal bash
/// notification (`Complete` / `Timeout` / `Backgrounded`), or `None` for any
/// other notification variant.
///
/// `BashTool::run` always sends one of these as its last notification, and
/// `LocalTerminalActor::drain_remaining_output` can append bytes *after* the
/// final periodic `BashOutputChunk` has been emitted. The streaming loop folds
/// this final base into a synthetic chunk so the in-band `bash_output_chunk`
/// deltas reach the terminal `total_bytes` without losing the tail.
fn terminal_notification_base(notif: &ToolNotification) -> Option<&BashNotificationBase> {
    match notif {
        ToolNotification::BashExecutionComplete(c) => Some(&c.base),
        ToolNotification::BashExecutionTimeout(t) => Some(&t.base),
        ToolNotification::BashExecutionBackgrounded(b) => Some(&b.base),
        _ => None,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Params
// ───────────────────────────────────────────────────────────────────────────

/// Configuration for the bash tool, stored as `Params<BashParams>` in Resources.
///
/// All fields are optional — `None` means "use the built-in default".
///
/// **Backwards compatibility:** new optional fields use `#[serde(default)]` so
/// older clients that omit them keep working on new servers.
///
/// **Unknown fields are ignored** (no `deny_unknown_fields`): clients may send
/// newer keys to older *or* newer servers without finalize failures. Typos in
/// `params_json` will silently no-op — prefer pin-matched servers in CI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BashParams {
    /// Default command timeout in seconds. None → 120s.
    pub timeout_secs: Option<f64>,
    /// **Foreground-only** ceiling for model-provided `timeout` (seconds).
    /// None → built-in [`DEFAULT_MAX_TIMEOUT_MS`] (5 minutes); production
    /// echo-agent-build opts up to 10h via echo-agent-runtime's `BashToolConfig`.
    ///
    /// When set:
    /// - Positive model `timeout` values are clamped to this ceiling (ms).
    /// - The foreground omit-default is capped by it.
    /// - The model-facing schema description advertises this max (plus an
    ///   optional JSON Schema `maximum`).
    ///
    /// This never bounds background tasks: `timeout: 0` / omitted in background
    /// mode always resolves to `Duration::MAX` regardless of this value (the
    /// model owns their lifetime via the background-task tooling; the terminal
    /// backend's own hard cap is the only backstop). Unset reproduces prior
    /// behavior with a 5-minute foreground default.
    #[serde(default)]
    pub max_timeout_secs: Option<f64>,
    /// Max output chars. None → DEFAULT_TOOL_OUTPUT_CHARS (20k).
    pub output_byte_limit: Option<usize>,
    /// Command prefix to prepend to all bash commands.
    pub cmd_prefix: Option<String>,
    #[serde(default = "default_true")]
    pub enabled_background: bool,
    /// When true (and [`Self::enabled_background`]), a foreground command that
    /// hits its FG wait deadline is **moved to the background** instead of killed.
    ///
    /// The FG wait deadline is `min(resolved_timeout, foreground_block_budget)`:
    /// - resolved timeout: model `timeout` or [`Self::timeout_secs`] (default 120s),
    ///   clamped by [`Self::max_timeout_secs`] (default 5m).
    /// - short budget: [`Self::foreground_block_budget_ms`] (default 15s).
    ///
    /// Set `foreground_block_budget_ms: 0` to disable the short budget so only
    /// the resolved timeout triggers auto-bg (reference-compatible).
    #[serde(default)]
    pub auto_background_on_timeout: bool,
    /// Max FG block before auto-bg when [`Self::auto_background_on_timeout`] is
    /// true (milliseconds). Independent of model `timeout`.
    ///
    /// - `None` → 15_000 (default short budget).
    /// - `Some(0)` → no short budget; auto-bg only when model/default timeout elapses.
    /// - `Some(ms)` → auto-bg after `ms` if still running.
    #[serde(default)]
    pub foreground_block_budget_ms: Option<u64>,
    /// Ceiling for `Shell` model `block_until_ms` (and OLD-variant
    /// `timeout`) in **milliseconds**.
    ///
    /// - `None` or `Some(0)` → no dedicated block_until ceiling (omit still
    ///   defaults to 30s; bash FG still respects [`Self::max_timeout_secs`]).
    /// - `Some(ms)` with `ms > 0` → clamp positive block waits to `ms`.
    ///   Model `block_until_ms: 0` (immediate background) is never clamped.
    #[serde(default)]
    pub max_block_until_ms: Option<u64>,
    /// Allow a background `&` operator in foreground commands (default `true`).
    /// Defaults `true` at the struct level so hosts that reuse `BashParams`
    /// without a client config resolver keep the `&` rejection off
    /// (toolsets that disable backgrounding still reject `&` via the
    /// `enabled_background` coupling in `should_reject_background_op`).
    #[serde(default = "default_true")]
    pub allow_background_operator: bool,
    /// Surface a `<system-reminder>` block listing newly-completed
    /// background tasks at the top of the next tool result (default
    /// `true`). Set to `false` in toolsets whose retrieval flow does
    /// not match the reminder wording.
    #[serde(default = "default_true")]
    pub surface_bg_completion_reminders: bool,
}

impl Default for BashParams {
    fn default() -> Self {
        Self {
            timeout_secs: None,
            max_timeout_secs: None,
            output_byte_limit: None,
            cmd_prefix: None,
            enabled_background: true,
            auto_background_on_timeout: false,
            foreground_block_budget_ms: None,
            max_block_until_ms: None,
            allow_background_operator: true,
            surface_bg_completion_reminders: true,
        }
    }
}

impl crate::types::resources::ResourceType for BashParams {
    const ID: &'static str = "echo_agent_build.Bash";

    fn validate_params_value(
        value: &Self,
    ) -> Result<(), crate::types::params_validation::ParamValidationError> {
        if value.auto_background_on_timeout && !value.enabled_background {
            return Err(crate::types::params_validation::ParamValidationError::new(
                "auto_background_on_timeout requires enabled_background to be true",
                "params_constraint",
            )
            .with_field_path("auto_background_on_timeout")
            .with_expected("false (when enabled_background is false)")
            .with_bad_value(serde_json::Value::Bool(true)));
        }
        Ok(())
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Input
// ───────────────────────────────────────────────────────────────────────────

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Product default advertised in the model-facing schema (FG). Not applied as a
/// serde default: omit/`None` must remain "use host/FG policy, BG unbounded".
fn schema_default_timeout_ms() -> Option<u64> {
    Some(120_000)
}

/// Input for the bash/terminal command tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BashToolInput {
    #[cfg_attr(unix, schemars(description = "The bash command to run."))]
    #[cfg_attr(not(unix), schemars(description = "The command to run."))]
    pub command: String,

    /// Optional timeout in milliseconds (max 300000). Default: 120000
    /// (2 minutes), enforced for foreground commands only. Background
    /// semantics live in the tool-description usage notes.
    // keep in sync with the rustdoc above
    #[schemars(
        description = "Optional timeout in milliseconds (max 300000). Default: 120000 (2 minutes), enforced for foreground commands only.",
        default = "schema_default_timeout_ms"
    )]
    // Some models serialize numeric tool args
    // as JSON strings (`"120000"`), which a plain `Option<u64>` rejects. Accept
    // string-or-number here; the schema still advertises an integer.
    // Serde default stays None so omit ≠ Some(120000): background omit must stay
    // unbounded (see resolve_effective_timeout). Schema still advertises 120000.
    #[serde(
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_u64",
        skip_serializing_if = "Option::is_none"
    )]
    pub timeout: Option<u64>,

    /// One sentence explanation as to why this command needs to be run and how it contributes to the goal.
    #[schemars(
        description = "One sentence explanation as to why this command needs to be run and how it contributes to the goal."
    )]
    pub description: String,

    /// Set to true for long-running commands that should run in the background (e.g., dev servers, long builds).
    /// Returns a task id immediately while the command keeps running in the background; you are notified on completion, so do not poll or sleep-wait for it.
    // "task id" stays plain English: the kill/get-output input params are
    // renameable, so naming a literal key here goes stale after randomization.
    // The notification sentence renders only when the client delivers system
    // reminders (`system_reminders_enabled` template flag); otherwise it
    // points at the get-output tool when one is served.
    #[schemars(
        description = "Set to true for long-running commands that should run in the background (e.g., dev servers, long builds). Returns a task id immediately while the command keeps running in the background${%- if system_reminders_enabled %}; you are notified on completion, so do not poll or sleep-wait for it${%- elif tools.by_kind.background_task_action %}; check on it later with the ${{ tools.by_kind.background_task_action }} tool${%- endif %}."
    )]
    #[serde(
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_bool"
    )]
    pub is_background: bool,
}

// ───────────────────────────────────────────────────────────────────────────
// Output
// ───────────────────────────────────────────────────────────────────────────

/// The bash tool can produce either a foreground result or a background task handle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum BashToolOutput {
    Foreground(BashOutput),
    Background(BackgroundTaskStarted),
}

impl echo_agent_tool_runtime::ToolOutput for BashToolOutput {
    fn chat_completion_output(
        &self,
    ) -> Option<echo_agent_tool_runtime::ToolChatCompletionResponse> {
        match self {
            Self::Foreground(bash) => {
                echo_agent_tool_runtime::ToolOutput::chat_completion_output(bash)
            }
            Self::Background(_) => None,
        }
    }
}

impl From<BashToolOutput> for crate::types::output::ToolOutput {
    fn from(o: BashToolOutput) -> Self {
        match o {
            BashToolOutput::Foreground(b) => crate::types::output::ToolOutput::Bash(b),
            BashToolOutput::Background(b) => {
                crate::types::output::ToolOutput::BackgroundTaskStarted(b)
            }
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// DEFAULT prompt formatting
// ───────────────────────────────────────────────────────────────────────────

use crate::util::truncate::format_bytes;

/// Reason a `run_terminal_cmd` child was terminated before it could exit
/// normally. When [`BashOutput::signal`] parses into one of these, the
/// `bash.exit_code` field is the `unwrap_or(-1)` sentinel (no real exit
/// code was captured), so the prompt header reads `exit: killed (reason)`
/// instead of the misleading `exit: -1 [signal=…]`.
///
/// Variants correspond 1:1 with the synthesized strings written into
/// `ExitStatus::signal` by the local terminal actor (see
/// `crate::computer::local::terminal`).
/// `Display` round-trips back to the original string so existing log /
/// notification consumers see no wire change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KillReason {
    /// Foreground command exceeded its configured timeout.
    Timeout,
    /// Background task exceeded `BACKGROUND_MAX_RUNTIME`.
    MaxRuntime,
    /// User-initiated cancel via the `kill_task` tool.
    Cancelled,
    /// Actor shutdown abandoned the process.
    Killed,
    /// Unix child terminated by POSIX signal `N`
    /// (from `extract_exit_status` → `"signal N"`).
    Signal(i32),
}

impl std::str::FromStr for KillReason {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "timeout" => Ok(Self::Timeout),
            "max_runtime" => Ok(Self::MaxRuntime),
            "cancelled" => Ok(Self::Cancelled),
            "killed" => Ok(Self::Killed),
            s => s
                .strip_prefix("signal ")
                .and_then(|n| n.parse::<i32>().ok())
                .map(Self::Signal)
                .ok_or(()),
        }
    }
}

impl std::fmt::Display for KillReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => f.write_str("timeout"),
            Self::MaxRuntime => f.write_str("max_runtime"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Killed => f.write_str("killed"),
            Self::Signal(n) => write!(f, "signal {n}"),
        }
    }
}

fn annotations(bash: &BashOutput) -> String {
    let mut s = String::new();
    if bash.truncated {
        let shown = format_bytes(bash.output.len() as u64);
        let total = format_bytes(bash.total_bytes as u64);
        s.push_str(&format!(
            " [truncated: showing first/last {} of {} - full output at: {}]",
            shown, total, bash.output_file
        ));
    }
    if let Some(signal) = &bash.signal {
        // Synthetic kill reasons are conveyed by the `exit: killed (reason)`
        // header in `format_default_prompt`; suppress the redundant
        // `[signal=…]` (and the now-superfluous `[timeout]`) here.
        if signal.parse::<KillReason>().is_err() {
            s.push_str(&format!(" [signal={}]", signal));
        }
    }
    s
}

/// Build the full DEFAULT prompt text from a `BashOutput`.
///
/// - Normal: `exit: N [annotations]\n<stripped_output>`
/// - Killed by harness/signal: `exit: killed (reason) [annotations]\n<stripped_output>`
/// - Backgrounded: verbose `[Command moved to background]...` format.
pub(crate) fn format_default_prompt(bash: &BashOutput) -> String {
    let output_str = if bash.output_for_prompt.is_empty() {
        let raw = String::from_utf8_lossy(&bash.output);
        strip_ansi_escapes::strip_str(&raw).to_string()
    } else {
        bash.output_for_prompt.clone()
    };
    let is_backgrounded = bash.signal.as_deref() == Some("backgrounded");

    if is_backgrounded {
        let shown = format_bytes(bash.output.len() as u64);
        let total = format_bytes(bash.total_bytes as u64);
        format!(
            "[Command moved to background]\n\n\
             Partial output ({} of {} total):\n\n\
             ```\n{}\n```\n\n\
             The command is still running in the background. You can continue with other tasks.\n\
             Full output is being written to: {}\n\n\
             On the next terminal tool call, the directory of the shell will be {}.",
            shown, total, output_str, bash.output_file, bash.current_dir
        )
    } else {
        let header = match bash
            .signal
            .as_deref()
            .and_then(|s| s.parse::<KillReason>().ok())
        {
            Some(reason) => format!("exit: killed ({}){}", reason, annotations(bash)),
            None => format!("exit: {}{}", bash.exit_code, annotations(bash)),
        };
        format!("{}\n{}", header, output_str)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Constants
// ───────────────────────────────────────────────────────────────────────────

// Default upper bound for model-provided *foreground* command timeouts when
// `BashParams.max_timeout_secs` is unset: a transport-safe **5 minutes**.
// Consumers that want longer opt in per session via `max_timeout_secs` — in
// particular production echo-agent-build sets it to 10h in echo-agent-runtime's
// `BashToolConfig`. `max_timeout_secs` only lowers/raises this *foreground*
// ceiling; background tasks (`timeout: 0` / omitted in background mode) are
// always unbounded regardless of this value — the model owns their lifetime via
// the background-task tooling. Absolute safety clamp for configured maxes: 10h.
pub(crate) const DEFAULT_MAX_TIMEOUT_MS: u64 = 300_000; // 5 minutes
const ABSOLUTE_MAX_TIMEOUT_MS: u64 = 36_000_000;
/// Default short FG block before auto-bg when auto_background_on_timeout is on.
/// Matches terminal `FOREGROUND_BLOCK_BUDGET`.
///
/// Currently used by tests / `effective_auto_bg_wait_ms` (description follow-up);
/// production runtime uses the terminal backend default when budget is unset.
#[allow(dead_code)] // description follow-up + tests (not yet model-facing)
pub(crate) const DEFAULT_FOREGROUND_BLOCK_BUDGET_MS: u64 = 15_000;

/// Internal version discriminant for run_terminal_cmd.
///
/// Use `from_contract()` instead of raw string comparisons against
/// `ctx.contract_version`. If additional version-sensitive schema or
/// validation behavior accumulates, consider promoting to a full
/// `versions/` module structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BashVersion {
    Current,
    Legacy0_4_10,
}

impl BashVersion {
    pub(crate) fn from_contract(v: Option<&str>) -> Self {
        match v {
            Some("legacy-0.4.10") => Self::Legacy0_4_10,
            _ => Self::Current,
        }
    }

    pub(crate) fn is_legacy(self) -> bool {
        self == Self::Legacy0_4_10
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Background operator detection
// ───────────────────────────────────────────────────────────────────────────

/// Detects only a trailing `&` (after trimming), excluding `&&` and `>&`.
///
/// Used where only a trailing `&` backgrounds a command: the legacy-0.4.10 bash
/// contract, and PowerShell (a *leading* `&` is the call
/// operator, so only a trailing `&` is the job operator). The current bash
/// contract uses `contains_background_operator()`, which detects `&` anywhere.
fn has_trailing_background_operator(command: &str) -> bool {
    let trimmed = command.trim();
    // Must end with `&` but NOT `&&` (logical AND) or `>&`/`&>` (redirects).
    trimmed.ends_with('&') && !trimmed.ends_with("&&") && !trimmed.ends_with(">&")
}

/// Check whether a bash command string contains a `&` used as a background
/// operator (i.e. one that would fork a process into the background).
///
/// Returns `true` when the command contains a **backgrounding `&`** — a bare
/// `&` that is *not* part of:
///
/// | Pattern | Meaning                    |
/// |---------|----------------------------|
/// | `&&`    | logical AND                |
/// | `&>`    | redirect stdout+stderr     |
/// | `&>>`   | append-redirect both       |
/// | `>&`    | fd duplication / redirect  |
/// | `<&`    | fd duplication             |
/// | `\&`    | escaped literal            |
/// | `'&'`   | inside single quotes       |
/// | `"&"`   | inside double quotes       |
fn contains_unwaited_background_operator(command: &str) -> bool {
    contains_background_operator(command) && !ends_with_wait_builtin(command)
}

/// Whether `command` uses `&` as a bash background operator under the given
/// contract version: legacy (0.4.10) flags only a trailing `&`; current flags
/// an unwaited `&` anywhere. Callers apply this only when
/// [`echo_agent_config::shell::ampersand_semantics`] reports POSIX `&` semantics
/// (Unix + Git Bash).
fn command_has_bash_background_operator(command: &str, is_legacy: bool) -> bool {
    if is_legacy {
        has_trailing_background_operator(command)
    } else {
        contains_unwaited_background_operator(command)
    }
}

/// PowerShell trailing-`&` detection: a leading `&` is the call operator, so only
/// a trailing `&` backgrounds. Strips trailing statement separators (`;`,
/// newlines) before reusing [`has_trailing_background_operator`] so `cmd &;` and
/// `cmd & ;` are caught too. Distinct from the bash legacy check, which must keep
/// its frozen `0.4.10` behavior.
fn powershell_has_trailing_background(command: &str) -> bool {
    let stripped = command.trim_end().trim_end_matches([';', '\n']);
    has_trailing_background_operator(stripped)
}

/// A background-operator `&` that must be rejected, carrying the shell-specific
/// remediation to surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundOpViolation {
    /// Bash/POSIX background `&` (Unix + Git Bash).
    Bash,
    /// PowerShell trailing `&`. `trailing_is_syntax_error` is true for Windows
    /// PowerShell 5.1 (a parse error) and false for pwsh 7+ (a background job).
    PowerShell { trailing_is_syntax_error: bool },
}

/// Classify the background-operator `&` violation in `command` for the active
/// shell's [`AmpersandSemantics`], or `None` when there is none:
/// - bash/POSIX rejects an unwaited `&` anywhere (legacy: only a trailing `&`);
/// - PowerShell rejects only a *trailing* `&` (a leading `&` is the call
///   operator and is allowed; a mid-line `&` job is intentionally not detected,
///   to avoid false-positives on the call operator — a no-op);
/// - `cmd.exe` never rejects (`&` is a sequential separator).
///
/// Kept pure so the per-shell decision is unit-testable on every platform
/// without the process-global shell detector.
fn detect_background_op_violation(
    semantics: AmpersandSemantics,
    command: &str,
    is_legacy: bool,
) -> Option<BackgroundOpViolation> {
    match semantics {
        AmpersandSemantics::PosixBackground => {
            command_has_bash_background_operator(command, is_legacy)
                .then_some(BackgroundOpViolation::Bash)
        }
        AmpersandSemantics::PowerShellCore => powershell_has_trailing_background(command)
            .then_some(BackgroundOpViolation::PowerShell {
                trailing_is_syntax_error: false,
            }),
        AmpersandSemantics::WindowsPowerShell => powershell_has_trailing_background(command)
            .then_some(BackgroundOpViolation::PowerShell {
                trailing_is_syntax_error: true,
            }),
        AmpersandSemantics::CmdSeparator => None,
    }
}

/// Decide whether a command's background-operator `&` must be rejected, folding
/// the escape hatches over [`detect_background_op_violation`]: an explicit
/// `is_background=true` (the whole command is backgrounded, so the inner `&` is
/// redundant) always bypasses; a foreground `&` is allowed only when the
/// `allow_background_operator` flag is on (defaults on; see its field doc for why)
/// AND the toolset has backgrounding enabled. When backgrounding is disabled the
/// forked child can't be tracked or killed, so `&` stays rejected regardless of
/// the flag (covers the ephemeral container toolset without touching it).
fn should_reject_background_op(
    is_background: bool,
    allow_background_operator: bool,
    background_enabled: bool,
    semantics: AmpersandSemantics,
    command: &str,
    is_legacy: bool,
) -> Option<BackgroundOpViolation> {
    if is_background || (allow_background_operator && background_enabled) {
        return None;
    }
    detect_background_op_violation(semantics, command, is_legacy)
}

/// Returns `true` when the command's last statement is the `wait` builtin.
///
/// `wait` makes the shell block until all backgrounded children finish, so
/// a command like `cmd1 & cmd2 & wait` is effectively blocking — the `&`
/// is being used to parallelise sub-tasks, not to background the whole
/// command. We allow this pattern.
fn ends_with_wait_builtin(command: &str) -> bool {
    // Strip trailing whitespace / semicolons / newlines.
    let trimmed = command
        .trim_end()
        .trim_end_matches(';')
        .trim_end_matches('\n')
        .trim_end();
    // `wait` must be a standalone word.
    trimmed == "wait"
        || trimmed.ends_with(" wait")
        || trimmed.ends_with(";wait")
        || trimmed.ends_with("\twait")
        || trimmed.ends_with("\nwait")
}

/// Parse the heredoc operator starting at `chars[start]` (first `<` of `<<`).
///
/// Returns `(delimiter, strip_tabs, position_after_delimiter_token)`, or `None`
/// if the `<<` is not followed by a valid delimiter word.
///
/// This only parses the operator on the command line (e.g. `<< 'EOF'`).
/// The heredoc *body* is consumed separately by `skip_heredoc_body`.
fn parse_heredoc_start(chars: &[char], start: usize) -> Option<(String, bool, usize)> {
    let len = chars.len();
    let mut i = start + 2; // skip `<<`

    // `<<-` strips leading tabs from the body and the closing delimiter.
    let strip_tabs = i < len && chars[i] == '-';
    if strip_tabs {
        i += 1;
    }

    // Skip horizontal whitespace (not newlines).
    while i < len && (chars[i] == ' ' || chars[i] == '\t') {
        i += 1;
    }

    if i >= len || chars[i] == '\n' {
        return None; // no delimiter
    }

    let delimiter: String;
    if chars[i] == '\'' {
        // Single-quoted delimiter: << 'WORD'
        i += 1;
        let d_start = i;
        while i < len && chars[i] != '\'' {
            i += 1;
        }
        if i >= len {
            return None; // unclosed quote
        }
        delimiter = chars[d_start..i].iter().collect();
        i += 1; // skip closing quote
    } else if chars[i] == '"' {
        // Double-quoted delimiter: << "WORD"
        i += 1;
        let d_start = i;
        while i < len && chars[i] != '"' {
            i += 1;
        }
        if i >= len {
            return None;
        }
        delimiter = chars[d_start..i].iter().collect();
        i += 1;
    } else {
        // Unquoted (or backslash-escaped) delimiter: << WORD
        let d_start = i;
        while i < len
            && !chars[i].is_whitespace()
            && !matches!(chars[i], ';' | '&' | '|' | '(' | ')' | '<' | '>')
        {
            i += 1;
        }
        if i == d_start {
            return None;
        }
        // Strip backslashes (they quote individual chars in unquoted delimiters).
        delimiter = chars[d_start..i].iter().filter(|&&c| c != '\\').collect();
    }

    if delimiter.is_empty() {
        return None;
    }

    Some((delimiter, strip_tabs, i))
}

/// Skip past a heredoc body that starts at `chars[start]`.
///
/// Scans lines until one matches `delimiter` (for `<<-`, after stripping
/// leading tabs). Returns the position immediately after the delimiter line.
/// If the delimiter is never found, returns `chars.len()` (treats the rest
/// of the input as heredoc content to prevent false positives).
fn skip_heredoc_body(chars: &[char], start: usize, delimiter: &str, strip_tabs: bool) -> usize {
    let len = chars.len();
    let mut i = start;

    while i < len {
        let line_start = i;
        while i < len && chars[i] != '\n' {
            i += 1;
        }
        let line: String = chars[line_start..i].iter().collect();

        let check = if strip_tabs {
            line.trim_start_matches('\t')
        } else {
            &line
        };

        if check == delimiter {
            if i < len {
                i += 1; // skip the `\n` after the delimiter
            }
            return i;
        }

        if i < len {
            i += 1; // skip `\n`
        }
    }

    // Delimiter not found -- treat the rest as heredoc content.
    len
}

fn contains_background_operator(command: &str) -> bool {
    let chars: Vec<char> = command.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    // Pending heredoc delimiters: (delimiter, strip_leading_tabs).
    // Collected on the command line; bodies consumed at the next newline.
    let mut pending_heredocs: Vec<(String, bool)> = Vec::new();

    while i < len {
        let ch = chars[i];

        // ── Backslash escape (honoured everywhere except inside single quotes) ──
        if ch == '\\' && !in_single_quote {
            i += 2; // skip the escaped character
            continue;
        }

        // ── Quote tracking ──
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            i += 1;
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }

        // Newline: if heredocs are pending, skip their bodies.
        if ch == '\n' && !in_single_quote && !in_double_quote && !pending_heredocs.is_empty() {
            i += 1; // skip the newline
            for (delim, strip_tabs) in pending_heredocs.drain(..) {
                i = skip_heredoc_body(&chars, i, &delim, strip_tabs);
            }
            continue;
        }

        // Heredoc detection: `<<` outside quotes (but not `<<<` here-string).
        if ch == '<'
            && !in_single_quote
            && !in_double_quote
            && i + 1 < len
            && chars[i + 1] == '<'
            && !(i + 2 < len && chars[i + 2] == '<')
            && let Some((delim, strip_tabs, after)) = parse_heredoc_start(&chars, i)
        {
            pending_heredocs.push((delim, strip_tabs));
            i = after;
            continue;
        }

        // Only inspect `&` outside of any quoting context.
        if ch == '&' && !in_single_quote && !in_double_quote {
            // `&&` — logical AND, skip both characters.
            if i + 1 < len && chars[i + 1] == '&' {
                i += 2;
                continue;
            }

            // `&>` / `&>>` — redirect stdout+stderr.
            if i + 1 < len && chars[i + 1] == '>' {
                i += 2;
                // Skip an extra `>` for `&>>`.
                if i < len && chars[i] == '>' {
                    i += 1;
                }
                continue;
            }

            // `>&` or `<&` — the `&` is part of a fd-duplication redirect.
            // Look at the immediately preceding character (no whitespace allowed
            // between `>` / `<` and `&` for these to be valid redirects).
            if i > 0 && (chars[i - 1] == '>' || chars[i - 1] == '<') {
                i += 1;
                continue;
            }

            // None of the non-background patterns matched — this is a real
            // background `&`.
            return true;
        }

        i += 1;
    }

    false
}
// ───────────────────────────────────────────────────────────────────────────
// Self-matching pkill/pgrep detection
// ───────────────────────────────────────────────────────────────────────────

/// Matches a `pkill` / `pgrep` invocation at a command-word position with a
/// `-f`-bearing flag bundle (short cluster `-X*fX*` or long `--full`) and
/// captures the command word plus the first positional argument (the
/// pattern). The pattern may be a bare token, single-quoted, or
/// double-quoted.
///
/// Regex (read top-down):
///   (?m)                          -- multiline (^ matches after \n)
///   (?:^|[;&|\(\n])               -- statement boundary (start, ;, &, |, (, \n)
///   \s*                           -- skipping leading whitespace
///   (?P<cmd>pkill|pgrep)          -- the command word (captured)
///   (?P<args>                     -- one or more flag tokens, at least one
///       (?:\s+-[A-Za-z]*f[A-Za-z]*  --   short cluster containing `f`
///         |\s+--full\b)+              --   or the long `--full` form
///   )
///   \s+                           -- separator before pattern
///   (?:'(?P<sq>[^']*)'|"(?P<dq>[^"]*)"|(?P<bare>[^\s;&|()]+))  -- pattern arg
static PKILL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?m)(?:^|[;&|\(\n])\s*(?P<cmd>pkill|pgrep)(?P<args>(?:\s+-[A-Za-z]*f[A-Za-z]*|\s+--full\b)+)\s+(?:'(?P<sq>[^']*)'|"(?P<dq>[^"]*)"|(?P<bare>[^\s;&|()]+))"#,
    )
    .expect("valid pkill/pgrep regex")
});

/// Detection of a `kill` token in the excised rest. Used to gate `pgrep`
/// invocations (which only print PIDs unless piped into a killer). Matches
/// `kill` as its own command word, including basic `xargs ... kill ...`
/// chains.
static KILL_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[\s;&|()])kill(?:\s|$)").expect("valid kill-token regex"));

/// Smallest pattern length we'll flag. One- and two-character `-f` patterns
/// are realistically never used (they would kill huge swaths of the
/// process table), and substring-matching them produces high noise.
const MIN_PKILL_PATTERN_LEN: usize = 3;

/// A self-matching pkill/pgrep invocation: the command word that was
/// matched and the literal pattern argument.
struct SelfMatchingPkill {
    /// `"pkill"` or `"pgrep"`.
    cmd: &'static str,
    /// The literal pattern argument (quotes stripped).
    pattern: String,
}

/// Detect a `pkill -f <pat>` / `pgrep -f <pat>` whose literal pattern
/// substring-matches the rest of the command (i.e. outside the pkill
/// invocation itself).
///
/// `pgrep` is only flagged when the excised rest also contains a `kill`
/// token -- a bare `pgrep -f X && echo found` does not kill anything and
/// must not be rejected.
///
/// Returns `Some(_)` if a self-matching invocation is found.
///
/// Known acceptable false-positive: a `pkill -f ...` literal that appears
/// inside a quoted argument to another command (e.g. `echo "pkill -f ./foo
/// && ./foo"`) is detected here -- we don't model bash quoting context
/// around the pkill invocation. This is a small false-positive risk we
/// accept to keep the check simple; the check is intentionally lenient
/// elsewhere (`$(...)` patterns are skipped) so the net risk is low.
fn self_matching_pkill_pattern(command: &str) -> Option<SelfMatchingPkill> {
    for caps in PKILL_RE.captures_iter(command) {
        let pat_str: &str = caps
            .name("sq")
            .or_else(|| caps.name("dq"))
            .or_else(|| caps.name("bare"))
            .map(|m| m.as_str())?;

        // Skip non-literal patterns (command substitution / shell expansion).
        if pat_str.is_empty()
            || pat_str.len() < MIN_PKILL_PATTERN_LEN
            || pat_str.contains("$(")
            || pat_str.contains("$`")
            || pat_str.starts_with('`')
            || pat_str.contains("${")
        {
            continue;
        }

        // Build "rest of command" = everything outside this pkill call.
        // The single `\n` separator keeps adjacent tokens from smashing
        // together (e.g. `pkill -f X;./X` would otherwise become `;./X`
        // glued to the preceding char); newline is safe because all
        // statement-boundary parsing already treats `\n` as a separator.
        let m = caps.get(0).expect("group 0 always present");
        let mut rest = String::with_capacity(command.len());
        rest.push_str(&command[..m.start()]);
        rest.push('\n');
        rest.push_str(&command[m.end()..]);

        // Resolve the matched command word as a static slice so callers
        // get a `&'static str` rather than borrowing the input command.
        let cmd: &'static str = match caps.name("cmd").map(|m| m.as_str()) {
            Some("pkill") => "pkill",
            Some("pgrep") => "pgrep",
            _ => continue,
        };

        // `pgrep` is a read-only PID lookup -- only escalate to a
        // rejection when the surrounding command actually kills via the
        // matched PIDs (e.g. `pgrep -f X | xargs kill`).
        if cmd == "pgrep" && !KILL_TOKEN_RE.is_match(&rest) {
            continue;
        }

        if rest.contains(pat_str) {
            return Some(SelfMatchingPkill {
                cmd,
                pattern: pat_str.to_owned(),
            });
        }
    }
    None
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const BACKGROUND_TIMEOUT: Duration = Duration::from_secs(86400); // 24 hours

/// Max time a *non-backgroundable* foreground command may block the turn. Such
/// a command has only its requested `timeout` (up to 10h), so a long timeout
/// would wedge the turn; we clamp and kill at this cap instead. Backgroundable
/// commands use the terminal's `FOREGROUND_BLOCK_BUDGET` instead. Long work
/// should use `background: true`. Env override: `ECHO_AGENT_MAX_FOREGROUND_BLOCK_MS`.
const MAX_FOREGROUND_BLOCK: Duration = Duration::from_secs(300); // 5 minutes

fn max_foreground_block() -> Duration {
    std::env::var("ECHO_AGENT_MAX_FOREGROUND_BLOCK_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(MAX_FOREGROUND_BLOCK)
}

/// Cap a non-backgroundable foreground `timeout` to `max_block` (or the
/// session's `config_timeout`, whichever is larger).
fn clamp_foreground_block(
    timeout: Duration,
    config_timeout: Duration,
    max_block: Duration,
) -> Duration {
    timeout.min(max_block.max(config_timeout))
}

// ───────────────────────────────────────────────────────────────────────────
// Bare `echo "<msg>"` detection (for statistics + hints in echo_agent_build bash)
// ───────────────────────────────────────────────────────────────────────────

/// Tracks usage of bare `echo "<msg>"` (and close variants) inside the bash tool.
///
/// These are often used by the model as a substitute for direct output or
/// communication. We want statistics on this pattern for the echo_agent_build
/// implementation and can surface educational hints on repeated use.
#[derive(Debug, Clone, Default)]
struct BareEchoHintState {
    call_count: usize,
}

/// Returns true for simple "bare echo" commands whose primary purpose appears
/// to be emitting a short literal message (as opposed to scripting, logging
/// with complex formatting, or part of a larger pipeline).
///
/// Heuristics (conservative to start):
/// - Starts with `echo` (possibly with output-control flags -n/-e/-E).
/// - Followed by what looks like a single message token (quoted or bare).
/// - No shell metacharacters indicating chaining, redirection, substitution,
///   or complex post-processing after the message.
///
/// Check whether `rest` (the portion after the command name and any flags)
/// is a simple narration message with no shell metacharacters.
///
/// `$` is intentionally treated as a metachar to conservatively reject
/// commands like `echo "cost $5"` that could contain variable expansions.
fn is_simple_narration_tail(rest: &str) -> bool {
    if rest.is_empty() {
        return false;
    }
    let has_metachar = rest.contains(|c: char| {
        matches!(
            c,
            ';' | '|' | '&' | '>' | '<' | '$' | '`' | '(' | ')' | '\n'
        )
    });
    if has_metachar {
        return false;
    }
    let msg = rest.trim();
    !msg.is_empty() && msg.len() < 512
}

fn is_bare_echo(command: &str) -> bool {
    let t = command.trim_start();
    if !t.starts_with("echo") {
        return false;
    }
    // Word boundary check.
    let after_prefix = &t[4..];
    if !after_prefix.is_empty() && !after_prefix.starts_with(char::is_whitespace) {
        return false;
    }

    // Skip optional output flags (-n, -e, -E, combinations).
    // Intentionally loose: `-nnnn` or `--ne` are accepted since models
    // don't generate weird flag combos and false positives are harmless.
    let mut rest = after_prefix.trim_start();
    while rest.starts_with('-') {
        let flag_part: String = rest
            .chars()
            .take_while(|c| matches!(c, '-' | 'n' | 'e' | 'E'))
            .collect();
        if flag_part.is_empty() || flag_part == "-" {
            break;
        }
        rest = &rest[flag_part.len()..];
        rest = rest.trim_start();
    }

    is_simple_narration_tail(rest)
}

/// Detect bare `printf` commands used for narration.
///
/// Rejects `printf -v var ...` (variable assignment — actual work, not
/// narration) and `printf --` (end-of-options marker, usually complex usage).
fn is_bare_printf(command: &str) -> bool {
    let t = command.trim_start();
    if !t.starts_with("printf") {
        return false;
    }
    let after_prefix = &t[6..];
    if !after_prefix.is_empty() && !after_prefix.starts_with(char::is_whitespace) {
        return false;
    }
    let rest = after_prefix.trim_start();
    // printf -v var ... is variable assignment, not narration.
    if rest.starts_with("-v") || rest.starts_with("--") {
        return false;
    }
    is_simple_narration_tail(rest)
}

#[cfg(test)]
mod bare_echo_tests {
    use super::{is_bare_echo, is_bare_printf};

    #[test]
    fn simple_quoted() {
        assert!(is_bare_echo(r#"echo "hello""#));
        assert!(is_bare_echo(r#"echo 'hello world'"#));
    }

    #[test]
    fn simple_unquoted() {
        assert!(is_bare_echo("echo hello"));
        assert!(is_bare_echo("echo some-text_123"));
    }

    #[test]
    fn with_output_flags() {
        assert!(is_bare_echo(r#"echo -n "no newline""#));
        assert!(is_bare_echo(r#"echo -e "line1\nline2""#));
        assert!(is_bare_echo(r#"echo -ne 'foo'"#));
    }

    #[test]
    fn with_leading_whitespace() {
        assert!(is_bare_echo("  echo hi"));
        assert!(is_bare_echo("\techo hi"));
    }

    #[test]
    fn not_bare_when_complex() {
        assert!(!is_bare_echo(r#"echo "hi" | cat"#));
        assert!(!is_bare_echo(r#"echo foo > /tmp/x"#));
        assert!(!is_bare_echo(r#"echo $(date)"#));
        assert!(!is_bare_echo(r#"echo a; echo b"#));
        assert!(!is_bare_echo(r#"echo hi && true"#));
        assert!(!is_bare_echo(r#"echo hi &"#));
    }

    #[test]
    fn not_bare_for_other_commands() {
        assert!(!is_bare_echo("ls"));
        assert!(!is_bare_echo("python -c 'print(1)'"));
        assert!(!is_bare_echo("printf 'hi\n'"));
        assert!(!is_bare_echo(""));
    }

    #[test]
    fn word_boundary_rejects_prefix_match() {
        assert!(!is_bare_echo("echoing something"));
        assert!(!is_bare_echo("echo_foo bar"));
        assert!(!is_bare_echo("echotool"));
    }

    // ── is_bare_printf tests ──

    #[test]
    fn printf_simple() {
        assert!(is_bare_printf(r#"printf "hello\n""#));
        assert!(is_bare_printf(r#"printf 'hi %s' world"#));
        assert!(is_bare_printf("printf hello"));
    }

    #[test]
    fn printf_not_bare_when_complex() {
        assert!(!is_bare_printf(r#"printf "%s\n" "$(date)""#));
        assert!(!is_bare_printf(r#"printf "hi" | cat"#));
        assert!(!is_bare_printf(r#"printf "hi" > /tmp/x"#));
        assert!(!is_bare_printf(""));
    }

    #[test]
    fn printf_not_bare_for_other_commands() {
        assert!(!is_bare_printf("echo hello"));
        assert!(!is_bare_printf("ls"));
    }

    #[test]
    fn printf_word_boundary_rejects_prefix_match() {
        assert!(!is_bare_printf("printfoo"));
        assert!(!is_bare_printf("printf_bar"));
    }

    #[test]
    fn printf_variable_assignment_not_bare() {
        assert!(!is_bare_printf("printf -v myvar '%s' hello"));
        assert!(!is_bare_printf("printf -- '%s' hello"));
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tool implementation
// ───────────────────────────────────────────────────────────────────────────

/// Bash tool — new architecture.
///
/// Executes bash commands via the `Terminal` backend. Supports foreground
/// (blocking) and background (returns task_id) execution modes.
#[derive(Debug, Default)]
pub struct BashTool;

impl BashTool {
    /// Resolve the effective ceiling (ms) for model-provided timeouts.
    ///
    /// `BashParams.max_timeout_secs` when set (floored to 1ms and clamped to
    /// [`ABSOLUTE_MAX_TIMEOUT_MS`]); otherwise [`DEFAULT_MAX_TIMEOUT_MS`].
    ///
    /// The `.max(1)` floor matters: a sub-millisecond positive `max_timeout_secs`
    /// (e.g. `0.0004`) rounds to 0ms, but `max_timeout_configured` stays true, so
    /// without the floor the schema would advertise `max 0` while timeout
    /// resolution enforces a 1ms ceiling — a model-visible mismatch. Flooring
    /// here keeps the advertised max equal to the enforced max (≥1ms).
    ///
    /// `pub` (not `pub(crate)`): the cursor `Shell` adapter
    /// (echo-agent-cursor) uses this ceiling to report the true FG wait in
    /// its auto-background template.
    pub fn effective_max_timeout_ms(params: &BashParams) -> u64 {
        let configured = params
            .max_timeout_secs
            .filter(|s| *s > 0.0)
            .map(|s| ((s * 1000.0).round() as u64).max(1))
            .map(|ms| ms.min(ABSOLUTE_MAX_TIMEOUT_MS));
        configured.unwrap_or(DEFAULT_MAX_TIMEOUT_MS)
    }

    /// Whether a session-level max timeout was explicitly configured.
    /// When true, the model-facing schema advertises a JSON Schema `maximum`
    /// equal to the ceiling. (Does not affect background tasks, which are
    /// always unbounded.)
    fn max_timeout_configured(params: &BashParams) -> bool {
        params.max_timeout_secs.is_some_and(|s| s > 0.0)
    }

    /// Effective FG omit-default (ms): [`BashParams::timeout_secs`] or 120s,
    /// always **≤** [`Self::effective_max_timeout_ms`].
    pub(crate) fn effective_default_timeout_ms(params: &BashParams) -> u64 {
        let configured = params
            .timeout_secs
            .filter(|s| *s > 0.0)
            .map(|s| (s * 1000.0).round() as u64);
        let default_ms = configured
            .unwrap_or(DEFAULT_TIMEOUT.as_millis() as u64)
            .max(1);
        let max_ms = Self::effective_max_timeout_ms(params).max(1);
        default_ms.min(max_ms)
    }

    /// Resolve the effective wrapper timeout to apply to the runtime request.
    ///
    /// Semantics (`max_timeout_ms` is the **foreground-only** ceiling from
    /// `max_timeout_secs`):
    /// - `Some(ms)` with `ms > 0`, foreground → `ms` clamped to `max_timeout_ms`.
    /// - `Some(ms)` with `ms > 0`, background → `ms` clamped only to
    ///   [`ABSOLUTE_MAX_TIMEOUT_MS`] (10h). The foreground ceiling never bounds
    ///   a background command's model-chosen kill-backstop; only the absolute
    ///   safety limit does.
    /// - `Some(0)` or `None` in foreground → `config_timeout` capped by max.
    /// - `Some(0)` or `None` in background → always [`Duration::MAX`]
    ///   (unbounded). `max_timeout_secs` never bounds background tasks; the model
    ///   owns their lifetime via the background-task tooling, matching
    ///   long-standing behavior.
    fn resolve_effective_timeout(
        input_timeout_ms: Option<u64>,
        is_background: bool,
        config_timeout: Duration,
        max_timeout_ms: u64,
    ) -> Duration {
        let fg_ceiling = max_timeout_ms.max(1);
        match input_timeout_ms {
            Some(ms) if ms > 0 => {
                // Background positive timeouts are foreground-ceiling-exempt:
                // bound only by the absolute limit so `max_timeout_secs` (a
                // foreground knob) can't cap a backgrounded command.
                let ceiling = if is_background {
                    ABSOLUTE_MAX_TIMEOUT_MS
                } else {
                    fg_ceiling
                };
                Duration::from_millis(ms.min(ceiling))
            }
            _ => {
                if is_background {
                    Duration::MAX
                } else {
                    let cfg_ms = config_timeout.as_millis().min(u128::from(u64::MAX)) as u64;
                    Duration::from_millis(cfg_ms.min(fg_ceiling).max(1))
                }
            }
        }
    }

    fn resolve_effective_timeout_for_params(
        input_timeout_ms: Option<u64>,
        is_background: bool,
        config_timeout: Duration,
        params: &BashParams,
    ) -> Duration {
        Self::resolve_effective_timeout(
            input_timeout_ms,
            is_background,
            config_timeout,
            Self::effective_max_timeout_ms(params),
        )
    }

    fn background_enabled(params: &BashParams) -> bool {
        params.enabled_background
    }

    fn auto_background_on_timeout_enabled(params: &BashParams) -> bool {
        params.auto_background_on_timeout && Self::background_enabled(params)
    }

    /// Per-request FG auto-bg budget for the terminal when auto_bg is on.
    ///
    /// - `None` when auto_bg is off, **or** when `foreground_block_budget_ms` is
    ///   unset — leave `TerminalRunRequest.foreground_block_budget` as `None` so
    ///   the terminal backend default applies (`ECHO_AGENT_FOREGROUND_BLOCK_BUDGET_MS`
    ///   / 15s). Do not materialize a fixed 15s here; that would override the env.
    /// - `Some(Duration::MAX)` when budget is `0` (timeout-only auto-bg).
    /// - `Some(ms)` when an explicit budget is configured.
    pub(crate) fn effective_foreground_block_budget(
        params: &BashParams,
    ) -> Option<std::time::Duration> {
        if !Self::auto_background_on_timeout_enabled(params) {
            return None;
        }
        match params.foreground_block_budget_ms {
            // 0 = disable short budget: only model/default timeout auto-bgs.
            Some(0) => Some(std::time::Duration::MAX),
            Some(ms) => Some(std::time::Duration::from_millis(ms)),
            // Unset → backend default (env-overridable).
            None => None,
        }
    }

    /// Effective auto-bg wait: min(default_timeout, budget) when auto_bg is on
    /// and budget is finite; otherwise default_timeout.
    ///
    /// When the session does not set `foreground_block_budget_ms`, assumes the
    /// backend's documented default (15s) for this helper — the real process
    /// still honors `ECHO_AGENT_FOREGROUND_BLOCK_BUDGET_MS` via `None` on the request.
    ///
    /// Not yet used in model-facing descriptions (historical auto-bg copy only).
    #[allow(dead_code)] // description follow-up + unit tests
    pub(crate) fn effective_auto_bg_wait_ms(params: &BashParams) -> Option<u64> {
        if !Self::auto_background_on_timeout_enabled(params) {
            return None;
        }
        let default_ms = Self::effective_default_timeout_ms(params);
        let budget_ms = match params.foreground_block_budget_ms {
            Some(0) => return Some(default_ms),
            Some(ms) => ms,
            None => DEFAULT_FOREGROUND_BLOCK_BUDGET_MS,
        };
        Some(default_ms.min(budget_ms).max(1))
    }

    /// Background retrieval hint naming the get-output tool and its task-ids
    /// param. Kind-wide resolution is correct here: this names *another*
    /// tool's param (the get-output tool), not this bash tool's own schema
    /// key — do not switch it to invoking-tool param names.
    async fn background_retrieval_hint(
        resources: &SharedResources,
        task_id: &str,
    ) -> Result<String, echo_agent_tool_runtime::ToolError> {
        let res = resources.lock().await;
        let renderer = res.require::<TemplateRenderer>()?;
        // Presence-aware lookup (not a template render): a missing kind
        // renders as empty-`Ok`, so a `Result` fallback never fires.
        let get_task_name = renderer
            .tool_for_kind(ToolKind::BackgroundTaskAction)
            .unwrap_or("get_task_output")
            .to_string();
        let task_ids_param = renderer
            .param_for_kind(ToolKind::BackgroundTaskAction, "task_ids")
            .unwrap_or("task_ids");
        Ok(format!(
            "Use {get_task_name} with {task_ids_param}=[\"{task_id}\"] when you need the output."
        ))
    }

    /// Model-facing input schema. `timeout_param_name` is the client-facing
    /// timeout field (canonical or alias) — must match the remapped key.
    fn exported_input_schema(
        input_schema: &serde_json::Value,
        params: &BashParams,
        timeout_param_name: &str,
    ) -> serde_json::Value {
        let background_enabled = Self::background_enabled(params);
        let auto_bg = Self::auto_background_on_timeout_enabled(params);
        let mut schema = input_schema.clone();
        if let Some(obj) = schema.as_object_mut() {
            if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
                if !background_enabled {
                    props.remove("is_background");
                }
                if let Some(timeout_prop) = props.get_mut("timeout").and_then(|t| t.as_object_mut())
                {
                    let max_ms = Self::effective_max_timeout_ms(params);
                    let default_ms = Self::effective_default_timeout_ms(params);
                    // The default and max ceiling are foreground-only.
                    // Background semantics live in the tool-description usage
                    // notes (single copy); the "foreground only" scoping here
                    // keeps the property from contradicting them.
                    // Keep main-style auto-bg wording (no FG-budget ms advertised).
                    // Follow-up: surface effective_auto_bg_wait_ms / FG budget here
                    // once we deliberately change model-facing copy.
                    let desc = if !background_enabled {
                        format!(
                            "Optional {timeout_param_name} in milliseconds (max {max_ms}). Default: {default_ms}."
                        )
                    } else if auto_bg {
                        format!(
                            "Optional {timeout_param_name} in milliseconds (max {max_ms}). Default: {default_ms}; foreground commands exceeding it are automatically backgrounded."
                        )
                    } else {
                        format!(
                            "Optional {timeout_param_name} in milliseconds (max {max_ms}). Default: {default_ms}, enforced for foreground commands only."
                        )
                    };
                    timeout_prop.insert("description".to_string(), serde_json::json!(desc));
                    if Self::max_timeout_configured(params) {
                        timeout_prop.insert("maximum".to_string(), serde_json::json!(max_ms));
                    }
                }
            }
            if let Some(required) = obj.get_mut("required").and_then(|r| r.as_array_mut())
                && !background_enabled
            {
                required.retain(|v| v.as_str() != Some("is_background"));
            }
        }
        schema
    }

    fn rendered_description(
        description_override: Option<&str>,
        renderer: &TemplateRenderer,
        params: &BashParams,
    ) -> String {
        let background_enabled = Self::background_enabled(params);
        let auto_bg = Self::auto_background_on_timeout_enabled(params);
        let raw_desc = match description_override {
            Some(desc) => desc,
            None => Self::default_description_template(background_enabled),
        };
        // Template only interpolates max/default timeout numbers + auto_bg flag.
        // Do not advertise FG block budget ms here yet (follow-up PR).
        let extras = serde_json::json!({
            "auto_background_on_timeout": auto_bg,
            "max_timeout_ms": Self::effective_max_timeout_ms(params),
            "default_timeout_ms": Self::effective_default_timeout_ms(params),
            "max_timeout_configured": Self::max_timeout_configured(params),
        });
        renderer
            .render_with_extra(raw_desc, &extras)
            .unwrap_or_else(|e| {
                crate::types::template_renderer::strip_markers_on_render_failure(raw_desc, &e)
            })
    }

    fn default_description_template(background_enabled: bool) -> &'static str {
        if background_enabled {
            Self::default_description_template_enabled()
        } else {
            Self::default_description_template_disabled()
        }
    }

    fn default_description_template_enabled() -> &'static str {
        // NOTE: auto-bg wording is intentionally the historical main copy (no
        // FG-block-budget ms). Runtime auto-bg uses min(timeout, FG budget);
        // advertising that wait is a separate description PR.
        r#"Run a ${%- if is_windows %} shell command${%- else %} bash command${%- endif %} and return its output.

Usage notes:
  - You can specify an optional ${{ params.execute.timeout }} in milliseconds (up to ${{ max_timeout_ms | default(300000) }}ms). ${%- if auto_background_on_timeout %} If not specified, foreground commands exceeding the default timeout will be automatically backgrounded instead of killed. You will receive a task id to check output later.${%- else %} If not specified, foreground commands will timeout after ${{ default_timeout_ms | default(120000) }}ms.${%- endif %} Background tasks are not bounded by the default: with ${{ params.execute.timeout }} omitted or 0 they run until they exit or are killed; a positive ${{ params.execute.timeout }} still applies.
  - Timeout enforcement: when the timeout fires, the wrapper${%- if is_windows %} terminates the child's Job Object, killing every descendant process immediately (no graceful-termination grace period).${%- else %} kills the child process group (SIGTERM, escalated to SIGKILL after a ~1s grace period). Descendants that did not detach via `setsid` / `nohup` will also be killed.${%- endif %} `${{ params.execute.timeout }}: 0` in `${%- if params is defined and params.execute is defined and params.execute.is_background %}${{ params.execute.is_background }}${%- else %}background${%- endif %}: true` mode disables the wrapper timeout entirely${%- if tools.by_kind.kill_task_action %}; the child's lifetime is owned by the model via ${{ tools.by_kind.kill_task_action }}${%- endif %}.
  - If the output exceeds {max_output_bytes} characters, the middle is truncated (you keep the beginning and end) and the result includes the path to a log file with the full output, which you can read or search.
  - You can use the ${{ params.execute.is_background }} parameter to run the command in the background (e.g., dev servers, long builds): it returns a task id immediately and keeps running in the background.${%- if system_reminders_enabled %} You are notified on completion, so do not poll or sleep-wait for it.${%- elif tools.by_kind.background_task_action %} Check on it later with the ${{ tools.by_kind.background_task_action }} tool.${%- endif %}${%- if has_unix_utilities %} You do not need to use '&' at the end of the command when using this parameter.${%- endif %}
${%- if shell_uses_semicolon %}
  - '&&' is not supported in this shell; chain sequential commands with ';'.
${%- endif %}
${%- if not has_unix_utilities %}
  - The Unix utilities `grep`, `head`, `tail`, `sed`, `awk`, and `find` are NOT available in this shell. Use the dedicated tools instead.
${%- endif %}"#
    }

    fn default_description_template_disabled() -> &'static str {
        r#"Run a ${%- if is_windows %} shell command${%- else %} bash command${%- endif %} and return its output.

Usage notes:
  - You can specify an optional ${{ params.execute.timeout }} in milliseconds (up to ${{ max_timeout_ms | default(300000) }}ms). If not specified, commands will timeout after ${{ default_timeout_ms | default(120000) }}ms.
  - Timeout enforcement: when the timeout fires, the wrapper${%- if is_windows %} terminates the child's Job Object, killing every descendant process immediately (no graceful-termination grace period).${%- else %} kills the child process group (SIGTERM, escalated to SIGKILL after a ~1s grace period).${%- endif %}
  - If the output exceeds {max_output_bytes} characters, output will be truncated before being returned to you.
${%- if shell_uses_semicolon %}
  - '&&' is not supported in this shell; chain sequential commands with ';'.
${%- endif %}
${%- if not has_unix_utilities %}
  - The Unix utilities `grep`, `head`, `tail`, `sed`, `awk`, and `find` are NOT available in this shell. Use the dedicated tools instead.
${%- endif %}"#
    }

    fn background_operator_validation_message(
        is_legacy: bool,
        background_enabled: bool,
        param_name: &str,
    ) -> String {
        match (background_enabled, is_legacy) {
            (true, true) => format!(
                "Command must not end with '&'. Remove the '&' and set {}=true to run the command in the background.",
                param_name
            ),
            (true, false) => {
                format!("Remove the background '&' from your command and set {}=true instead.", param_name)
            }
            (false, true) => {
                "Command must not end with '&' because background execution is disabled. Remove the '&' and run the command in the foreground."
                    .to_string()
            }
            (false, false) => {
                "Remove the background '&' from your command; background execution is disabled.".to_string()
            }
        }
    }

    /// Concise remediation for a trailing background `&` in PowerShell, tailored
    /// to the edition: `trailing_is_syntax_error` is true for Windows PowerShell
    /// 5.1 (a parse error) and false for pwsh 7+ (a background job). A leading
    /// `& <path>` is the call operator and never triggers this.
    fn powershell_background_operator_message(
        background_enabled: bool,
        param_name: &str,
        trailing_is_syntax_error: bool,
    ) -> String {
        let effect = if trailing_is_syntax_error {
            "is a syntax error in Windows PowerShell 5.1"
        } else {
            "starts a background job"
        };
        if background_enabled {
            format!("Trailing '&' {effect}. Set {param_name}=true instead.")
        } else {
            format!("Trailing '&' {effect}. Background execution is disabled; remove it.")
        }
    }

    /// Prepend cmd_prefix to command if configured.
    fn get_prefixed_command(cmd_prefix: &Option<String>, command: &str) -> String {
        match cmd_prefix {
            Some(prefix) => {
                let sep = echo_agent_config::shell::chain_separator();
                format!("{prefix} {sep} {command}")
            }
            None => command.to_string(),
        }
    }
}

impl crate::types::tool_metadata::ToolMetadata for BashTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        Self::default_description_template_enabled()
    }

    fn emitted_notifications(&self) -> &'static [&'static str] {
        &[
            "BashExecutionBackgrounded",
            "BashExecutionComplete",
            "BashExecutionFailed",
            "BashExecutionTimeout",
            "BashOutputChunk",
            "TaskCompleted",
        ]
    }

    fn versioned_definition(
        &self,
        contract_version: Option<&str>,
        client_name: &str,
        description_override: Option<&str>,
        renderer: &TemplateRenderer,
        param_map: &std::collections::HashMap<String, String>,
        input_schema: &serde_json::Value,
        effective_params: &serde_json::Value,
    ) -> ToolDefinition {
        let params: BashParams =
            serde_json::from_value(effective_params.clone()).unwrap_or_default();
        let description = Self::rendered_description(description_override, renderer, &params);
        // Only this tool's param_map renames schema property keys — do not
        // fall back to kind-wide renderer aliases (another Execute tool's
        // override could advertise e.g. max_wait while this schema still
        // exposes timeout).
        let timeout_param_name = param_map
            .get("timeout")
            .map(String::as_str)
            .unwrap_or("timeout");
        let exported_schema =
            Self::exported_input_schema(input_schema, &params, timeout_param_name);
        let remapped_schema = if param_map.is_empty() {
            exported_schema
        } else {
            crate::util::remap::remap_schema_properties(&exported_schema, param_map)
        };
        let _ = contract_version;
        ToolDefinition::function(client_name, Some(&description), remapped_schema)
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        // If our param `enabled_background` is true (the default),
        // we need both a BackgroundTaskAction tool (get_task_output) and
        // a KillTaskAction tool (kill_task) so the agent can interact
        // with backgrounded commands.
        Expr::And(vec![
            Expr::Value(ToolRequirement::if_params(
                ToolParamsRequirement::new("enabled_background", true),
                ToolRequirement::tool_kind(ToolKind::BackgroundTaskAction),
            )),
            Expr::Value(ToolRequirement::if_params(
                ToolParamsRequirement::new("enabled_background", true),
                ToolRequirement::tool_kind(ToolKind::KillTaskAction),
            )),
        ])
    }
}

impl echo_agent_tool_runtime::Tool for BashTool {
    type Args = BashToolInput;
    type Output = BashToolOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("run_terminal_cmd").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "run_terminal_cmd",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        // Clone of `BASH_CAPABILITIES`; read at registration time only.
        BASH_CAPABILITIES.clone()
    }

    /// Streaming entry point.
    ///
    /// `Tool` is an `#[async_trait]` trait, so `execute` MUST keep the
    /// `async fn ... -> ToolStream<_>` signature (the macro rewrites it into a
    /// boxed future). The body, however, never `.await`s: it builds and
    /// returns a `'static` boxed stream synchronously. `BashTool` is a ZST, so
    /// we move an owned `BashTool` into the stream rather than borrowing
    /// `&self` (the returned stream must not borrow `self`).
    ///
    /// Background / monitor calls (`input.is_background`) take the blocking
    /// path: the single `Terminal` resolves immediately, later output
    /// continues on the existing session-wide side-channel — there is no
    /// foreground tick to coalesce.
    ///
    /// Foreground calls inject a `PerCallNotificationSink`, run the command,
    /// and concurrently turn this call's per-tick `BashOutputChunk`s into
    /// coalesced `ToolProgress` deltas via a manual `select!` (NOT
    /// `with_progress`, which would serialize progress-then-terminal and
    /// deadlock — the progress producer only ends when `run` finishes and
    /// drops the sink).
    ///
    /// Emission gated by `WorkspaceViewerContext::stream_tool_progress`.
    /// Absent extension = no emission (pre-streaming behavior).
    async fn execute(
        &self,
        mut ctx: echo_agent_tool_runtime::ToolCallContext,
        input: BashToolInput,
    ) -> echo_agent_tool_runtime::ToolStream<BashToolOutput> {
        // Background / monitor calls reach their single `Terminal` immediately
        // (the task continues asynchronously on the side-channel). Owned ZST,
        // no `&self` capture.
        if input.is_background {
            let this = BashTool;
            return Box::pin(async_stream::stream! {
                yield echo_agent_tool_runtime::ToolStreamItem::Terminal(this.run(ctx, input).await);
            });
        }

        // Per-user gate (default off if extension absent). Read BEFORE any
        // streaming-machinery allocation so the gate-off path can take the
        // non-streaming fast path below without paying for a sink, channel,
        // or `select!` loop it would only drain-and-discard from.
        let stream_progress = ctx
            .get::<echo_agent_tool_runtime::WorkspaceViewerContext>()
            .map(|c| c.stream_tool_progress)
            .unwrap_or(false);

        // Fast path: gate off → no `PerCallNotificationSink`, no channel,
        // no coalescing `select!`. Just run the command and yield a single
        // `Terminal`. The session-wide notification side-channel still
        // receives chunks via the resource-level `NotificationHandle` in
        // `run(...)`; we just don't fan a per-call copy out for Progress
        // emission that would be dropped anyway. Mirrors the
        // `is_background` shape above and matches the observable
        // pre-streaming contract asserted by
        // `bash_streaming_progress_suppressed_when_gate_off`.
        if !stream_progress {
            let this = BashTool;
            return Box::pin(async_stream::stream! {
                yield echo_agent_tool_runtime::ToolStreamItem::Terminal(this.run(ctx, input).await);
            });
        }

        // Absent spec is a can't-happen bug → terminal error, not a panic.
        let Some(spec) = BASH_CAPABILITIES.streaming.as_ref() else {
            return Box::pin(async_stream::stream! {
                yield echo_agent_tool_runtime::ToolStreamItem::Terminal(Err(
                    echo_agent_tool_runtime::ToolError::custom(
                        "internal_error",
                        "BASH_CAPABILITIES has no StreamingSpec",
                    ),
                ));
            });
        };

        // Foreground + gate ON: thread a per-call sink so this call's chunks
        // reach us in-band (in addition to the session side-channel via
        // `tee`).
        let (sink, mut rx) = ToolNotificationHandle::channel();
        ctx.extensions.insert(PerCallNotificationSink(sink));
        let this = BashTool;

        Box::pin(async_stream::stream! {
            let mut run_fut = std::pin::pin!(this.run(ctx, input));
            // Monotonic byte count already surfaced as a progress delta.
            let mut last_total: usize = 0;

            loop {
                tokio::select! {
                    biased;
                    maybe = rx.recv() => {
                        match maybe {
                            Some(ToolNotification::BashOutputChunk(chunk)) => {
                                // Coalesce: drain queued chunks, keep the newest
                                // (largest total_bytes), emit one delta per drain.
                                let mut latest = chunk;
                                while let Ok(ToolNotification::BashOutputChunk(next)) = rx.try_recv() {
                                    latest = next;
                                }
                                if stream_progress
                                    && let Some(p) =
                                        bash_output_chunk_progress(spec, &latest, &mut last_total)
                                {
                                    yield echo_agent_tool_runtime::ToolStreamItem::Progress(p);
                                }
                            }
                            Some(notif) => {
                                // Terminal bash notifications carry a final
                                // `BashNotificationBase` whose `total_bytes` can
                                // exceed the last periodic chunk because the
                                // actor's `drain_remaining_output` runs after
                                // its last tick. Fold the final base into a
                                // synthetic chunk so the tail bytes still
                                // become an in-band delta and `last_total`
                                // reaches the terminal `total_bytes`.
                                if stream_progress
                                    && let Some(base) = terminal_notification_base(&notif)
                                {
                                    let synthetic = BashOutputChunk { base: base.clone() };
                                    if let Some(p) =
                                        bash_output_chunk_progress(spec, &synthetic, &mut last_total)
                                    {
                                        yield echo_agent_tool_runtime::ToolStreamItem::Progress(p);
                                    }
                                }
                            }
                            None => {
                                // Channel closed before `run_fut` resolved.
                                // The next loop iteration will see `run_fut`
                                // complete; nothing to emit here.
                            }
                        }
                    }
                    result = &mut run_fut => {
                        // Flush any residual buffered notifications before the
                        // terminal so the last delta always precedes Terminal.
                        // We process BOTH residual chunks and a residual
                        // terminal bash notification (the actor sends it just
                        // before `run` returns) so the final-tail bytes are
                        // never lost.
                        if stream_progress {
                            while let Ok(notif) = rx.try_recv() {
                                match notif {
                                    ToolNotification::BashOutputChunk(chunk) => {
                                        if let Some(p) =
                                            bash_output_chunk_progress(spec, &chunk, &mut last_total)
                                        {
                                            yield echo_agent_tool_runtime::ToolStreamItem::Progress(p);
                                        }
                                    }
                                    other => {
                                        if let Some(base) = terminal_notification_base(&other) {
                                            let synthetic =
                                                BashOutputChunk { base: base.clone() };
                                            if let Some(p) = bash_output_chunk_progress(
                                                spec,
                                                &synthetic,
                                                &mut last_total,
                                            ) {
                                                yield echo_agent_tool_runtime::ToolStreamItem::Progress(p);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        yield echo_agent_tool_runtime::ToolStreamItem::Terminal(result);
                        break;
                    }
                }
            }
        })
    }

    #[tracing::instrument(
        name = "tool.run_terminal_cmd",
        skip_all,
        fields(
            is_background = %input.is_background,
        )
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: BashToolInput,
    ) -> Result<BashToolOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let cwd = crate::types::tool_metadata::resolve_cwd(&ctx, &resources).await?;
        let tool_call_id = ctx.call_id.clone();

        // --- Read resources ---
        let (backend, session_folder, env, notification_handle, owner_session_id) = {
            let res = resources.lock().await;
            (
                res.require::<Terminal>()?.0.clone(),
                res.require::<SessionFolder>()?.0.clone(),
                res.require::<SessionEnv>()?.0.as_ref().clone(),
                res.require::<NotificationHandle>()?.0.clone(),
                res.get::<crate::types::resources::OwnerSessionId>()
                    .map(|o| o.0.clone()),
            )
        };

        // Per-call streaming sink: when `execute` injected a
        // `PerCallNotificationSink`, fan the session handle out so this call's
        // chunks reach BOTH the session-wide side-channel and the per-call sink.
        let notification_handle = match ctx.extensions.get::<PerCallNotificationSink>() {
            Some(sink) => ToolNotificationHandle::tee(vec![notification_handle, sink.0.clone()]),
            None => notification_handle,
        };

        // Read params
        let params = resources
            .lock()
            .await
            .get::<Params<BashParams>>()
            .cloned()
            .unwrap_or_default();

        let config_timeout = Duration::from_millis(Self::effective_default_timeout_ms(&params));
        let background_enabled = Self::background_enabled(&params);

        let config_output_byte_limit = params
            .output_byte_limit
            .unwrap_or(DEFAULT_TOOL_OUTPUT_CHARS);

        // Use truncation config override if available
        let output_byte_limit = resources
            .lock()
            .await
            .get::<TruncationCfg>()
            .map(|cfg| {
                cfg.0
                    .max_output_bytes_for("run_terminal_cmd", config_output_byte_limit)
            })
            .unwrap_or(config_output_byte_limit);

        // --- Validate: reject commands that use `&` as a background operator ---
        let version = BashVersion::from_contract(
            crate::types::tool_metadata::behavior_version(&ctx).as_deref(),
        );
        let is_legacy = version.is_legacy();
        // `&` means different things per shell, so detection and remediation are
        // shell-specific: bash/POSIX backgrounds with a bare `&`; PowerShell
        // backgrounds only with a trailing `&` (a leading `&` is the call
        // operator); cmd.exe uses `&` as a sequential separator (never rejected).
        let ampersand = echo_agent_config::shell::ampersand_semantics();
        if let Some(violation) = should_reject_background_op(
            input.is_background,
            params.allow_background_operator,
            background_enabled,
            ampersand,
            &input.command,
            is_legacy,
        ) {
            // `is_background` is the canonical param key (the input-schema
            // property name). Presence-aware lookup (not a template render):
            // a missing entry renders as empty-`Ok`, so a `Result` fallback
            // never fires.
            let bg_param_name = {
                let res = resources.lock().await;
                res.get::<TemplateRenderer>()
                    .and_then(|r| r.param_for_kind(ToolKind::Execute, "is_background"))
                    .unwrap_or("is_background")
                    .to_string()
            };
            let message = match violation {
                BackgroundOpViolation::Bash => Self::background_operator_validation_message(
                    is_legacy,
                    background_enabled,
                    &bg_param_name,
                ),
                BackgroundOpViolation::PowerShell {
                    trailing_is_syntax_error,
                } => Self::powershell_background_operator_message(
                    background_enabled,
                    &bg_param_name,
                    trailing_is_syntax_error,
                ),
            };
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                message,
            ));
        }

        // --- Validate: reject self-matching pkill/pgrep -f <pat> ---
        // `pkill -f` matches the wrapper bash's full argv (which contains
        // the literal pattern), causing the wrapper to SIGTERM itself
        // before the rest of the script runs. Reject obvious cases at
        // parse time. See `self_matching_pkill_pattern` for the precise
        // detection rules.
        if let Some(hit) = self_matching_pkill_pattern(&input.command) {
            let SelfMatchingPkill { cmd, pattern } = hit;
            let message = format!(
                "self-matching {cmd}/-f: `{cmd} -f <pat>` matches against the full \
                 /proc/PID/cmdline of every process, including the bash wrapper that \
                 runs this command (its argv contains `{pattern}`). The wrapper would \
                 be killed by the resulting signal before the rest of the script runs. \
                 Use one of: `pkill -x <basename>` (no `-f`), `pgrep -f <pat> | xargs \
                 -r kill` invoked from a separate command, a fully-qualified path that \
                 does not appear later in the script, or kill by PID file."
            );
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                message,
            ));
        }

        if input.is_background && !background_enabled {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Background execution is disabled.".to_string(),
            ));
        }

        // --- Prefix ---
        let command = Self::get_prefixed_command(&params.cmd_prefix, &input.command);

        // No command-wrapping is performed here today; `display_command` is
        // populated only by tools that intentionally surface a friendlier form
        // to the model (e.g. the monitor tool). Bash commands run as-is.
        let display_command: Option<String> = None;

        // --- Route to foreground or background ---
        let output_file = session_folder
            .join("terminal")
            .join(format!("{}.log", tool_call_id.as_str()));

        if input.is_background {
            // ─── Background execution ───
            // Force unbuffered Python stdout so output reaches files and
            // pipes in real time rather than buffering ~8 KB.
            let mut env = env;
            env.entry("PYTHONUNBUFFERED".to_string())
                .or_insert("1".to_string());
            // Background timeout resolution:
            // - explicit positive `timeout` → use it (model can pin a
            //   shorter cap for short-lived bg tasks)
            // - `0` / `None` → unbounded; lifecycle owned by the model
            //   via the kill tool. (See [`resolve_effective_timeout`].)
            let timeout = Self::resolve_effective_timeout_for_params(
                input.timeout,
                true,
                BACKGROUND_TIMEOUT,
                &params,
            );
            let request = TerminalRunRequest {
                command: command.clone(),
                working_directory: cwd.clone(),
                env,
                timeout,
                output_byte_limit,
                output_file,
                notification_handle: notification_handle.clone(),
                tool_call_id: tool_call_id.as_str().to_owned(),
                display_command: display_command.clone(),
                auto_background_on_timeout: false,
                foreground_block_budget: None,
                kind: crate::computer::types::TaskKind::Bash,
                owner_session_id: owner_session_id.clone(),
                description: Some(input.description.clone()).filter(|d| !d.trim().is_empty()),
            };

            let handle = match backend.run_background(request).await {
                Ok(h) => h,
                Err(e) => {
                    notification_handle.send_failed(BashExecutionFailed {
                        tool_call_id: tool_call_id.as_str().to_owned(),
                        command: input.command.clone(),
                        cwd: cwd.clone(),
                        error: e.to_string(),
                    });
                    let bash_err = BashError::from(e);
                    return Err(bash_err.into());
                }
            };

            let task_id = handle.task_id;
            let bg_output_file = handle.output_file;
            let bg_pid = handle.pid;

            // Send backgrounded notification
            let base = BashNotificationBase {
                tool_call_id: tool_call_id.as_str().to_owned(),
                command: input.command.clone(),
                output: Vec::new(),
                total_bytes: 0,
                truncated: false,
                cwd: cwd.clone(),
            };

            notification_handle.send_backgrounded(BashExecutionBackgrounded {
                base,
                output_file: bg_output_file.clone(),
                task_id: task_id.clone(),
                monitor_description: None,
                description: Some(input.description.clone()).filter(|d| !d.trim().is_empty()),
            });

            let retrieval_hint = Self::background_retrieval_hint(&resources, &task_id).await?;

            Ok(BashToolOutput::Background(BackgroundTaskStarted {
                task_id: task_id.clone(),
                task_type: "bash".to_string(),
                output_file: bg_output_file.to_string_lossy().to_string(),
                status: "running".to_string(),
                command: input.command,
                summary: format!("Background task {} started", task_id),
                retrieval_hint,
                pre_formatted: None,
                pid: bg_pid,
            }))
        } else {
            // ─── Foreground execution ───
            let timeout = Self::resolve_effective_timeout_for_params(
                input.timeout,
                false,
                config_timeout,
                &params,
            );
            // Backgroundable commands get the terminal's short budget (the
            // requested `timeout` stays as the kill backstop, nothing to clamp).
            // Non-backgroundable ones have no background path, so a large
            // `timeout` would wedge the turn — clamp it to `MAX_FOREGROUND_BLOCK`.
            let timeout = if Self::auto_background_on_timeout_enabled(&params) {
                timeout
            } else {
                clamp_foreground_block(timeout, config_timeout, max_foreground_block())
            };

            let request = TerminalRunRequest {
                command: command.clone(),
                working_directory: cwd.clone(),
                env,
                timeout,
                output_byte_limit,
                output_file: output_file.clone(),
                notification_handle: notification_handle.clone(),
                tool_call_id: tool_call_id.as_str().to_owned(),
                display_command,
                // Honour `auto_background_on_timeout` regardless of
                // whether the model supplied an explicit `timeout`.
                // The previous gate (`input.timeout.is_none()`) hard-
                // timed out commands with explicit timeouts even when
                // the session had opted into auto-bg behavior.
                // The relaxed semantics matter here:
                // every Shell call carries an explicit `block_until_ms`
                // and the harness's observed behavior is to auto-background
                // past that deadline rather than kill the process.
                // Backwards compatible because
                // `auto_background_on_timeout` defaults to `false`;
                // existing echo_agent_build callers that never opted in are
                // unaffected.
                auto_background_on_timeout: Self::auto_background_on_timeout_enabled(&params),
                foreground_block_budget: Self::effective_foreground_block_budget(&params),
                kind: crate::computer::types::TaskKind::Bash,
                owner_session_id: owner_session_id.clone(),
                description: Some(input.description.clone()).filter(|d| !d.trim().is_empty()),
            };

            let result = match backend.run(request).await {
                Ok(r) => r,
                Err(e) => {
                    notification_handle.send_failed(BashExecutionFailed {
                        tool_call_id: tool_call_id.as_str().to_owned(),
                        command: input.command.clone(),
                        cwd: cwd.clone(),
                        error: e.to_string(),
                    });
                    let bash_err = BashError::from(e);
                    return Err(bash_err.into());
                }
            };

            // ─── Backgrounded (user Ctrl+G or auto-timeout): return BackgroundTaskStarted ───
            let auto_backgrounded = result.signal.as_deref() == Some("auto_backgrounded");
            if auto_backgrounded || result.signal.as_deref() == Some("backgrounded") {
                let base = BashNotificationBase {
                    tool_call_id: tool_call_id.as_str().to_owned(),
                    command: input.command.clone(),
                    output: result.combined_output.as_bytes().to_vec(),
                    total_bytes: result.total_bytes,
                    truncated: result.truncated,
                    cwd: cwd.clone(),
                };

                notification_handle.send_backgrounded(BashExecutionBackgrounded {
                    base,
                    output_file: output_file.clone(),
                    task_id: tool_call_id.as_str().to_owned(),
                    monitor_description: None,
                    description: Some(input.description.clone()).filter(|d| !d.trim().is_empty()),
                });

                let retrieval_hint =
                    Self::background_retrieval_hint(&resources, tool_call_id.as_str()).await?;

                let summary = if auto_backgrounded {
                    format!(
                        "Command \"{}\" exceeded the default timeout and was automatically moved to background. \
                         Process is still running.",
                        input.command,
                    )
                } else {
                    format!(
                        "User moved command \"{}\" to background. Process is still running.",
                        input.command
                    )
                };

                return Ok(BashToolOutput::Background(BackgroundTaskStarted {
                    task_id: tool_call_id.as_str().to_owned(),
                    task_type: "bash".to_string(),
                    output_file: output_file.to_string_lossy().to_string(),
                    status: "running".to_string(),
                    command: input.command,
                    summary,
                    retrieval_hint,
                    pre_formatted: None,
                    // Real PID from the foreground spawn surfaced via
                    // `TerminalRunResult::pid`. Adapters rely on this
                    // being non-None for their auto-bg template.
                    pid: result.pid,
                }));
            }

            // Send completion notification
            let base = BashNotificationBase {
                tool_call_id: tool_call_id.as_str().to_owned(),
                command: input.command.clone(),
                output: result.combined_output.as_bytes().to_vec(),
                total_bytes: result.total_bytes,
                truncated: result.truncated,
                cwd: cwd.clone(),
            };

            if result.timed_out {
                notification_handle.send_timeout(BashExecutionTimeout {
                    base,
                    elapsed: timeout,
                    timeout,
                });
            } else {
                notification_handle.send_complete(BashExecutionComplete {
                    base,
                    exit_code: result.exit_code,
                    signal: result.signal.clone(),
                });
            }

            let mut bash = BashOutput {
                output_for_prompt: BashOutput::make_output_for_prompt(&result.combined_output),
                output: result.combined_output.into_bytes(),
                exit_code: result.exit_code.unwrap_or(-1),
                command: input.command,
                truncated: result.truncated,
                signal: result.signal,
                timed_out: result.timed_out,
                description: Some(input.description).filter(|d| !d.trim().is_empty()),
                current_dir: cwd.to_string_lossy().to_string(),
                output_file: output_file.to_string_lossy().to_string(),
                total_bytes: result.total_bytes,
                output_delta: None,
                was_bare_echo: false,
            };
            bash.output_for_prompt = format_default_prompt(&bash);

            // Bare `echo "<msg>"` usage (common model anti-pattern for "just output something").
            // We tag it for statistics (echo_agent_build backend) and can surface an educational
            // hint on repeated use.
            bash.was_bare_echo = is_bare_echo(&bash.command) || is_bare_printf(&bash.command);
            if bash.was_bare_echo {
                let mut res = resources.lock().await;
                let state = res.get_or_default::<BareEchoHintState>();
                state.call_count += 1;
            }

            // Counter spans for commit / PR create / PR merge. The shell
            // independently runs the same detector over the tool result for
            // PR-metric session signals (see `record_git_pr_signals`).
            if bash.exit_code == 0
                && let Some(ops) =
                    crate::util::git_detect::detect_git_ops(&bash.command, &bash.output_for_prompt)
            {
                if ops.committed {
                    tracing::info_span!("git.commit", tool_name = "run_terminal_cmd")
                        .in_scope(|| {});
                }
                if ops.pr_created.is_some() {
                    tracing::info_span!("git.pull_request", tool_name = "run_terminal_cmd")
                        .in_scope(|| {});
                }
                if ops.pr_merged {
                    tracing::info_span!("git.pr_merge", tool_name = "run_terminal_cmd")
                        .in_scope(|| {});
                }
            }

            Ok(BashToolOutput::Foreground(bash))
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bash_timeout_schema_defaults_to_120s() {
        let schema = serde_json::to_value(schemars::schema_for!(BashToolInput)).unwrap();
        let timeout = &schema["properties"]["timeout"];
        assert_eq!(
            timeout.get("default"),
            Some(&serde_json::json!(120_000)),
            "timeout schema should advertise default 120000, got {timeout}"
        );
        // Serde omit stays None so background without timeout remains unbounded.
        let missing: BashToolInput =
            serde_json::from_str(r#"{"command":"ls","description":"list"}"#).unwrap();
        assert_eq!(missing.timeout, None);
        let zero: BashToolInput =
            serde_json::from_str(r#"{"command":"ls","description":"list","timeout":0}"#).unwrap();
        assert_eq!(zero.timeout, Some(0));
        // Explicit BG omit still resolves unbounded.
        assert_eq!(
            BashTool::resolve_effective_timeout(
                missing.timeout,
                true,
                DEFAULT_TIMEOUT,
                DEFAULT_MAX_TIMEOUT_MS,
            ),
            std::time::Duration::MAX
        );
    }

    use crate::computer::types::{
        BackgroundHandle, ComputerError, KillOutcome, TaskSnapshot, TerminalBackend,
        TerminalRunRequest, TerminalRunResult,
    };
    use crate::types::resources::Resources;
    use crate::types::tool_metadata::test_ctx;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// Models occasionally serialize numeric tool args as JSON strings. The
    /// `timeout` field must accept both `120000` and `"120000"`, stay `None`
    /// when omitted or null (FG host policy / BG unbounded).
    #[test]
    fn timeout_accepts_string_or_integer() {
        let from_int: BashToolInput =
            serde_json::from_str(r#"{"command":"echo hi","description":"test","timeout":120000}"#)
                .unwrap();
        assert_eq!(from_int.timeout, Some(120_000));

        let from_str: BashToolInput = serde_json::from_str(
            r#"{"command":"echo hi","description":"test","timeout":"120000"}"#,
        )
        .unwrap();
        assert_eq!(from_str.timeout, Some(120_000));

        let absent: BashToolInput =
            serde_json::from_str(r#"{"command":"echo hi","description":"test"}"#).unwrap();
        assert_eq!(absent.timeout, None);

        let null: BashToolInput =
            serde_json::from_str(r#"{"command":"echo hi","description":"test","timeout":null}"#)
                .unwrap();
        assert_eq!(null.timeout, None);
    }

    #[test]
    fn is_background_accepts_lenient_bool_forms() {
        let parse = |json: &str| -> BashToolInput { serde_json::from_str(json).unwrap() };

        assert!(
            parse(r#"{"command":"x","description":"test","is_background":true}"#).is_background
        );
        assert!(
            parse(r#"{"command":"x","description":"test","is_background":"true"}"#).is_background
        );
        assert!(
            parse(r#"{"command":"x","description":"test","is_background":"yes"}"#).is_background
        );
        assert!(parse(r#"{"command":"x","description":"test","is_background":"1"}"#).is_background);
        assert!(parse(r#"{"command":"x","description":"test","is_background":1}"#).is_background);

        assert!(
            !parse(r#"{"command":"x","description":"test","is_background":false}"#).is_background
        );
        assert!(
            !parse(r#"{"command":"x","description":"test","is_background":"false"}"#).is_background
        );
        assert!(
            !parse(r#"{"command":"x","description":"test","is_background":"NO"}"#).is_background
        );
        assert!(!parse(r#"{"command":"x","description":"test","is_background":0}"#).is_background);
        assert!(
            !parse(r#"{"command":"x","description":"test","is_background":null}"#).is_background
        );
        assert!(!parse(r#"{"command":"x","description":"test"}"#).is_background);

        assert!(
            serde_json::from_str::<BashToolInput>(
                r#"{"command":"x","description":"test","is_background":"maybe"}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<BashToolInput>(r#"{"command":"x"}"#).is_err());
    }

    // A foreground command must not block longer than the cap, whatever its
    // requested `timeout`.
    #[test]
    fn foreground_block_clamps_explicit_long_timeout() {
        let cfg = DEFAULT_TIMEOUT; // 120s session default
        let cap = MAX_FOREGROUND_BLOCK;
        // 10h explicit timeout → clamped to the cap.
        assert_eq!(
            clamp_foreground_block(Duration::from_secs(36_000), cfg, cap),
            MAX_FOREGROUND_BLOCK,
        );
        // Short timeout → untouched.
        assert_eq!(
            clamp_foreground_block(Duration::from_secs(10), cfg, cap),
            Duration::from_secs(10),
        );
        // A session default larger than the cap is respected.
        let big_cfg = Duration::from_secs(600);
        assert_eq!(
            clamp_foreground_block(Duration::from_secs(36_000), big_cfg, cap),
            big_cfg,
        );
    }

    // ─── Mock terminal ───

    /// Configurable mock terminal backend for testing.
    ///
    /// Stores foreground results as clonable `TerminalRunResult`, and
    /// background results as (task_id, output_file) since `BackgroundHandle`
    /// doesn't derive Clone.
    type CapturedRequest = std::sync::Arc<std::sync::Mutex<Option<TerminalRunRequest>>>;

    struct MockTerminal {
        /// Result to return from `run()`.
        foreground_result: Result<TerminalRunResult, ComputerError>,
        /// Background task_id and output_file, or error.
        bg_task_id: String,
        bg_output_file: PathBuf,
        bg_error: Option<String>,
        /// Captured background request for assertions.
        captured_bg_request: CapturedRequest,
    }

    impl MockTerminal {
        fn success(output: &str, exit_code: i32) -> Self {
            Self {
                foreground_result: Ok(TerminalRunResult {
                    combined_output: output.to_string(),
                    exit_code: Some(exit_code),
                    truncated: false,
                    signal: None,
                    timed_out: false,
                    output_file: PathBuf::from("/tmp/test.log"),
                    total_bytes: output.len(),
                    pid: None,
                }),
                bg_task_id: "task-1".to_string(),
                bg_output_file: PathBuf::from("/tmp/bg.log"),
                bg_error: None,
                captured_bg_request: CapturedRequest::default(),
            }
        }

        fn timed_out(output: &str) -> Self {
            Self {
                foreground_result: Ok(TerminalRunResult {
                    combined_output: output.to_string(),
                    exit_code: None,
                    truncated: false,
                    signal: None,
                    timed_out: true,
                    output_file: PathBuf::from("/tmp/test.log"),
                    total_bytes: output.len(),
                    pid: None,
                }),
                bg_task_id: "task-1".to_string(),
                bg_output_file: PathBuf::from("/tmp/bg.log"),
                bg_error: None,
                captured_bg_request: CapturedRequest::default(),
            }
        }

        fn failing() -> Self {
            Self {
                foreground_result: Err(ComputerError::io("command failed")),
                bg_task_id: String::new(),
                bg_output_file: PathBuf::new(),
                bg_error: Some("command failed".to_string()),
                captured_bg_request: CapturedRequest::default(),
            }
        }

        fn background_ok(task_id: &str) -> Self {
            Self {
                foreground_result: Ok(TerminalRunResult {
                    combined_output: String::new(),
                    exit_code: Some(0),
                    truncated: false,
                    signal: None,
                    timed_out: false,
                    output_file: PathBuf::new(),
                    total_bytes: 0,
                    pid: None,
                }),
                bg_task_id: task_id.to_string(),
                bg_output_file: PathBuf::from(format!("/tmp/{}.log", task_id)),
                bg_error: None,
                captured_bg_request: CapturedRequest::default(),
            }
        }

        fn background_ok_capturing(task_id: &str) -> (Self, CapturedRequest) {
            let captured = CapturedRequest::default();
            let mut mock = Self::background_ok(task_id);
            mock.captured_bg_request = captured.clone();
            (mock, captured)
        }
    }

    #[async_trait::async_trait]
    impl TerminalBackend for MockTerminal {
        async fn run(
            &self,
            _request: TerminalRunRequest,
        ) -> Result<TerminalRunResult, ComputerError> {
            self.foreground_result.clone()
        }

        async fn run_background(
            &self,
            request: TerminalRunRequest,
        ) -> Result<BackgroundHandle, ComputerError> {
            *self.captured_bg_request.lock().unwrap() = Some(request);
            if let Some(ref err) = self.bg_error {
                return Err(ComputerError::io(err.clone()));
            }
            Ok(BackgroundHandle {
                task_id: self.bg_task_id.clone(),
                output_file: self.bg_output_file.clone(),
                pid: None,
            })
        }

        async fn get_task(&self, _task_id: &str) -> Option<TaskSnapshot> {
            None
        }

        async fn kill_task(&self, _task_id: &str) -> KillOutcome {
            KillOutcome::NotFound
        }

        async fn wait_for_completion(
            &self,
            _task_id: &str,
            _timeout: Option<Duration>,
        ) -> Option<TaskSnapshot> {
            None
        }

        async fn list_tasks(&self) -> Vec<TaskSnapshot> {
            vec![]
        }
    }

    // ─── Test helpers ───

    fn make_resources(mock: MockTerminal) -> Resources {
        make_resources_with_params(mock, BashParams::default())
    }

    fn make_resources_with_params(mock: MockTerminal, params: BashParams) -> Resources {
        let mut resources = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(mock);
        resources.insert(Terminal(backend));
        resources.insert(Cwd(PathBuf::from("/tmp")));
        resources.insert(SessionFolder(PathBuf::from("/tmp/session")));
        resources.insert(SessionEnv(Arc::new(HashMap::new())));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources.insert(Params(params));

        // Add TemplateRenderer with default mappings for background hint and validation
        let execute_params =
            HashMap::from([("is_background".to_string(), "is_background".to_string())]);
        let bg_params = HashMap::from([("task_id".to_string(), "task_id".to_string())]);
        resources.insert(TemplateRenderer::new(
            HashMap::from([(
                ToolKind::BackgroundTaskAction,
                "get_task_output".to_string(),
            )]),
            HashMap::from([
                (ToolKind::Execute, execute_params),
                (ToolKind::BackgroundTaskAction, bg_params),
            ]),
        ));

        resources
    }

    /// Resources with the `&` rejection enabled. The flag defaults on (allow),
    /// so the detection-path tests opt back into rejection explicitly.
    fn make_resources_reject_bg_op(mock: MockTerminal) -> Resources {
        make_resources_with_params(
            mock,
            BashParams {
                allow_background_operator: false,
                ..BashParams::default()
            },
        )
    }

    fn make_input(command: &str) -> BashToolInput {
        BashToolInput {
            command: command.to_string(),
            timeout: None,
            description: "test".to_string(),
            is_background: false,
        }
    }

    fn make_bg_input(command: &str) -> BashToolInput {
        BashToolInput {
            command: command.to_string(),
            timeout: None,
            description: "test".to_string(),
            is_background: true,
        }
    }

    // ─── Streaming (BashTool::execute) test scaffolding ───
    //
    // `test_ctx` stamps `WorkspaceViewerContext { stream_tool_progress:
    // true }` by default, so these tests exercise the streaming path.

    /// Build resources backed by the *real* `LocalTerminalBackend` so the
    /// terminal actor emits `BashOutputChunk`s. Returns the `TempDir` too so
    /// the caller keeps the session/cwd directory alive for the test.
    fn make_real_resources(output_byte_limit: Option<usize>) -> (Resources, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut resources = Resources::new();
        let backend: Arc<dyn TerminalBackend> =
            Arc::new(crate::computer::local::LocalTerminalBackend::new());
        resources.insert(Terminal(backend));
        resources.insert(Cwd(tmp.path().to_path_buf()));
        resources.insert(SessionFolder(tmp.path().to_path_buf()));
        resources.insert(SessionEnv(Arc::new(HashMap::new())));
        resources.insert(NotificationHandle(ToolNotificationHandle::noop()));
        resources.insert(Params(BashParams {
            output_byte_limit,
            ..BashParams::default()
        }));
        resources.insert(TemplateRenderer::new(
            HashMap::from([(
                ToolKind::BackgroundTaskAction,
                "get_task_output".to_string(),
            )]),
            HashMap::from([
                (
                    ToolKind::Execute,
                    HashMap::from([("is_background".to_string(), "is_background".to_string())]),
                ),
                (
                    ToolKind::BackgroundTaskAction,
                    HashMap::from([("task_id".to_string(), "task_id".to_string())]),
                ),
            ]),
        ));
        (resources, tmp)
    }

    /// Destructure a `bash_output_chunk` payload, asserting the canonical
    /// `raw_terminal` / `append` envelope.
    fn read_chunk_progress(
        p: &echo_agent_tool_runtime::ToolProgress,
    ) -> (String, usize, bool, bool) {
        match p {
            echo_agent_tool_runtime::ToolProgress::Custom { subkind, payload } => {
                assert_eq!(subkind, "bash_output_chunk", "unexpected subkind");
                (
                    payload["delta"].as_str().unwrap().to_owned(),
                    payload["total_bytes"].as_u64().unwrap() as usize,
                    payload["truncated"].as_bool().unwrap(),
                    payload["gap"].as_bool().unwrap(),
                )
            }
            other => panic!("expected Custom progress, got {other:?}"),
        }
    }

    // ─── Streaming tests ───

    /// Deterministic regression guard for Key Decision 6 (delta math keyed off
    /// the monotonic `total_bytes`, not buffer length). Exercises the common
    /// suffix-slice case, the no-new-bytes case, the post-truncation shrinking
    /// tail, and the single-tick overflow `gap`.
    #[test]
    fn bash_output_chunk_progress_delta_math() {
        fn chunk(output: &[u8], total: usize, truncated: bool) -> BashOutputChunk {
            BashOutputChunk {
                base: BashNotificationBase {
                    tool_call_id: "t".into(),
                    command: "c".into(),
                    output: output.to_vec(),
                    total_bytes: total,
                    truncated,
                    cwd: PathBuf::from("/"),
                },
            }
        }

        let spec = BASH_CAPABILITIES.streaming.as_ref().unwrap();
        let mut last = 0usize;

        // 5 brand-new bytes, all present in the tail buffer.
        let p = bash_output_chunk_progress(spec, &chunk(b"hello", 5, false), &mut last).unwrap();
        assert_eq!(read_chunk_progress(&p), ("hello".into(), 5, false, false));
        assert_eq!(last, 5);

        // No new bytes (total unchanged) → no delta.
        assert!(bash_output_chunk_progress(spec, &chunk(b"hello", 5, false), &mut last).is_none());
        assert_eq!(last, 5);

        // After truncation the buffer is a *shrinking* tail. total 5 → 12
        // (7 new bytes); tail holds the last 8 bytes "lo world", and 7 <= 8 so
        // we slice the suffix: the last 7 bytes = "o world".
        let p = bash_output_chunk_progress(spec, &chunk(b"lo world", 12, true), &mut last).unwrap();
        assert_eq!(read_chunk_progress(&p), ("o world".into(), 12, true, false));
        assert_eq!(last, 12);

        // Single-tick burst overflow: total 12 → 100 (88 new) but the tail only
        // holds 4 bytes → the middle was dropped this tick. Emit the surviving
        // tail with gap=true. `truncated` carries the caller's cumulative flag
        // (base.truncated=false here) and is kept distinct from the per-tick gap.
        let p = bash_output_chunk_progress(spec, &chunk(b"tail", 100, false), &mut last).unwrap();
        assert_eq!(read_chunk_progress(&p), ("tail".into(), 100, false, true));
        assert_eq!(last, 100);
    }

    /// A single tick whose delta exceeds [`MAX_PROGRESS_DELTA_BYTES`] is cut to
    /// the cap (on a UTF-8 char boundary, never splitting a multi-byte
    /// sequence) and the remainder is deferred to the next tick (append is
    /// lossless); `total_bytes` still reflects the full count.
    #[test]
    fn bash_output_chunk_progress_caps_oversized_delta() {
        // Multi-byte chars (`€` = 3 bytes) ensure the cap lands mid-char so we
        // exercise the char-boundary back-off, not just a clean byte cut.
        let payload_str = "€".repeat(7_000); // 21_000 bytes, well over the cap.
        let total = payload_str.len();
        assert!(total > MAX_PROGRESS_DELTA_BYTES);
        let chunk = BashOutputChunk {
            base: BashNotificationBase {
                tool_call_id: "t".into(),
                command: "c".into(),
                output: payload_str.clone().into_bytes(),
                total_bytes: total,
                truncated: false,
                cwd: PathBuf::from("/"),
            },
        };

        let spec = BASH_CAPABILITIES.streaming.as_ref().unwrap();
        let mut last = 0usize;
        let p = bash_output_chunk_progress(spec, &chunk, &mut last).unwrap();
        match p {
            echo_agent_tool_runtime::ToolProgress::Custom { subkind, payload } => {
                assert_eq!(subkind, "bash_output_chunk");
                let delta = payload["delta"].as_str().unwrap();
                // Capped: never larger than the per-frame limit.
                assert!(
                    delta.len() <= MAX_PROGRESS_DELTA_BYTES,
                    "delta len {} exceeded cap {MAX_PROGRESS_DELTA_BYTES}",
                    delta.len()
                );
                // UTF-8 safe: the truncated delta is a valid prefix of the
                // original (no replacement char from a split `€`). With `€`
                // being 3 bytes, the cap (16384) is not a boundary, so we back
                // off below it.
                assert!(delta.len() < MAX_PROGRESS_DELTA_BYTES);
                assert_eq!(delta, &payload_str[..delta.len()]);
                // Append defers rather than drops: the over-cap remainder is
                // held back for the next tick.
                // total_bytes still reflects the true monotonic count.
                assert_eq!(payload["total_bytes"].as_u64().unwrap() as usize, total);
                // `last` advanced only past the emitted bytes (deferral), so
                // subsequent ticks re-slice the held remainder.
                assert_eq!(last, delta.len());
            }
            other => panic!("expected Custom progress, got {other:?}"),
        }

        // Drain the deferred remainder: repeated calls with the same chunk
        // pace it out in capped frames until the full payload is surfaced.
        let mut reassembled = String::new();
        reassembled.push_str(&payload_str[..last]);
        while last < total {
            let p = bash_output_chunk_progress(spec, &chunk, &mut last).unwrap();
            let echo_agent_tool_runtime::ToolProgress::Custom { payload, .. } = p else {
                panic!("expected Custom progress");
            };
            let delta = payload["delta"].as_str().unwrap().to_owned();
            assert!(delta.len() <= MAX_PROGRESS_DELTA_BYTES);
            reassembled.push_str(&delta);
        }
        assert_eq!(reassembled, payload_str, "deferred pacing is lossless");
        assert_eq!(last, total);
    }

    /// Absent `WorkspaceViewerContext` extension = no Progress emitted;
    /// terminal still surfaces.
    #[tokio::test]
    async fn bash_streaming_progress_suppressed_when_gate_off() {
        use futures::StreamExt;

        let (resources, _tmp) = make_real_resources(None);
        // No streaming gate stamped — exercises the default path.
        let mut ctx = echo_agent_tool_runtime::ToolCallContext::default();
        ctx.extensions.insert(resources.into_shared());
        let tool = BashTool;
        let mut stream = echo_agent_tool_runtime::Tool::execute(
            &tool,
            ctx,
            make_input("for i in 1 2 3; do echo $i; sleep 0.1; done"),
        )
        .await;

        let mut progress = 0usize;
        let mut terminal: Option<Result<BashToolOutput, echo_agent_tool_runtime::ToolError>> = None;
        while let Some(item) = stream.next().await {
            match item {
                echo_agent_tool_runtime::ToolStreamItem::Progress(_) => {
                    progress += 1;
                }
                echo_agent_tool_runtime::ToolStreamItem::Terminal(r) => {
                    assert!(terminal.is_none(), "more than one Terminal yielded");
                    terminal = Some(r);
                }
            }
        }
        assert_eq!(
            progress, 0,
            "absent gate must suppress all Progress, got {progress}"
        );
        match terminal.expect("stream ended without a Terminal") {
            Ok(BashToolOutput::Foreground(bash)) => {
                assert_eq!(bash.exit_code, 0);
                let out = String::from_utf8_lossy(&bash.output);
                assert!(
                    out.contains('1') && out.contains('3'),
                    "terminal preserved regardless of gate, got: {out}"
                );
            }
            other => panic!("expected Terminal(Ok(Foreground)), got {other:?}"),
        }
    }

    /// Gate ON (via `test_ctx`): ≥1 `bash_output_chunk` then exactly
    /// one `Terminal(Ok(Foreground))`, in order.
    #[tokio::test]
    async fn bash_streaming_progress() {
        use futures::StreamExt;

        let (resources, _tmp) = make_real_resources(None);
        let tool = BashTool;
        let mut stream = echo_agent_tool_runtime::Tool::execute(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("for i in 1 2 3; do echo $i; sleep 0.1; done"),
        )
        .await;

        let mut progress = 0usize;
        let mut terminal: Option<Result<BashToolOutput, echo_agent_tool_runtime::ToolError>> = None;
        while let Some(item) = stream.next().await {
            match item {
                echo_agent_tool_runtime::ToolStreamItem::Progress(p) => {
                    assert!(terminal.is_none(), "Progress arrived after Terminal");
                    // Asserts subkind == "bash_output_chunk".
                    let _ = read_chunk_progress(&p);
                    progress += 1;
                }
                echo_agent_tool_runtime::ToolStreamItem::Terminal(r) => {
                    assert!(terminal.is_none(), "more than one Terminal yielded");
                    terminal = Some(r);
                }
            }
        }

        assert!(
            progress >= 1,
            "expected at least one bash_output_chunk progress, got {progress}"
        );
        match terminal.expect("stream ended without a Terminal") {
            Ok(BashToolOutput::Foreground(bash)) => {
                assert_eq!(bash.exit_code, 0);
                let out = String::from_utf8_lossy(&bash.output);
                assert!(out.contains('1') && out.contains('3'), "got output: {out}");
            }
            other => panic!("expected Terminal(Ok(Foreground)), got {other:?}"),
        }
    }

    /// Critical regression guard for Key Decision 6: when output exceeds the
    /// byte limit mid-stream, deltas KEEP arriving after truncation, the
    /// reported `total_bytes` stays monotonic and consistent with the delta
    /// lengths, and `truncated` is surfaced.
    #[tokio::test]
    async fn bash_streaming_progress_survives_truncation() {
        use futures::StreamExt;

        // Small limit so truncation fires early; ~1.8 KB of ASCII output over
        // ~1.8s easily exceeds it and keeps emitting across the shrinking tail.
        let (resources, _tmp) = make_real_resources(Some(200));
        let tool = BashTool;
        let mut stream = echo_agent_tool_runtime::Tool::execute(
            &tool,
            test_ctx(resources.into_shared()),
            make_input(
                "for i in $(seq 1 60); do printf 'LINE%03d-XXXXXXXXXXXXXXXXXXXX\\n' \"$i\"; sleep 0.03; done",
            ),
        )
        .await;

        // (total_bytes, truncated, gap, delta_len)
        let mut deltas: Vec<(usize, bool, bool, usize)> = Vec::new();
        let mut terminal: Option<Result<BashToolOutput, echo_agent_tool_runtime::ToolError>> = None;
        while let Some(item) = stream.next().await {
            match item {
                echo_agent_tool_runtime::ToolStreamItem::Progress(p) => {
                    assert!(terminal.is_none(), "Progress arrived after Terminal");
                    let (delta, total, truncated, gap) = read_chunk_progress(&p);
                    deltas.push((total, truncated, gap, delta.len()));
                }
                echo_agent_tool_runtime::ToolStreamItem::Terminal(r) => {
                    assert!(terminal.is_none(), "more than one Terminal yielded");
                    terminal = Some(r);
                }
            }
        }

        assert!(
            deltas.len() >= 2,
            "expected multiple deltas across truncation, got {}",
            deltas.len()
        );

        // total_bytes is strictly increasing (keyed off the monotonic counter).
        for w in deltas.windows(2) {
            assert!(
                w[1].0 > w[0].0,
                "total_bytes must strictly increase: {} !> {}",
                w[1].0,
                w[0].0
            );
        }

        // Per-delta consistency: a non-gap delta's bytes account for exactly the
        // advance in total_bytes (output here is pure ASCII, so lossy == exact).
        let mut prev = 0usize;
        for &(total, _trunc, gap, len) in &deltas {
            if !gap {
                assert_eq!(
                    prev + len,
                    total,
                    "non-gap delta len must match total_bytes advance"
                );
            }
            prev = total;
        }

        // Truncation must be surfaced, and deltas must KEEP arriving after the
        // first truncated delta (the length-based gate would have stalled here).
        let first_truncated = deltas.iter().position(|d| d.1);
        let first_truncated =
            first_truncated.expect("expected at least one truncated delta past the byte limit");
        assert!(
            first_truncated < deltas.len() - 1,
            "deltas stopped arriving after truncation (first_truncated={first_truncated}, total={})",
            deltas.len()
        );

        match terminal.expect("stream ended without a Terminal") {
            Ok(BashToolOutput::Foreground(bash)) => {
                assert_eq!(bash.exit_code, 0);
                assert!(
                    bash.truncated,
                    "expected the terminal output to be truncated"
                );
                // The terminal's total_bytes is the actor's final monotonic
                // count; the last emitted delta can only lag it (output may
                // arrive between the final tick and process exit).
                assert!(
                    deltas.last().unwrap().0 <= bash.total_bytes,
                    "last delta total {} exceeded terminal total {}",
                    deltas.last().unwrap().0,
                    bash.total_bytes
                );
                assert!(
                    bash.total_bytes > 200,
                    "output should exceed the byte limit"
                );
            }
            other => panic!("expected Terminal(Ok(Foreground)), got {other:?}"),
        }
    }

    /// Regression guard: the streaming loop must fold the
    /// final terminal bash notification (`BashExecutionComplete` /
    /// `Timeout` / `Backgrounded`) into a synthetic chunk so bytes that the
    /// terminal actor's `drain_remaining_output` captures *after* the last
    /// periodic `BashOutputChunk` are not lost.
    ///
    /// Asserts that for an untruncated foreground run the concatenation of
    /// the non-gap deltas equals the terminal output exactly and the last
    /// delta's `total_bytes` reaches the terminal `total_bytes`.
    #[tokio::test]
    async fn bash_streaming_progress_includes_final_drain() {
        use futures::StreamExt;

        let (resources, _tmp) = make_real_resources(None);
        let tool = BashTool;
        // Fast-exiting command that emits its full output in one burst —
        // exercises the "drained after the last periodic chunk" path that the
        // bug missed. ASCII so lossy UTF-8 conversion is exact.
        let cmd = "printf 'tail-bytes-after-final-tick\\n'";
        let mut stream = echo_agent_tool_runtime::Tool::execute(
            &tool,
            test_ctx(resources.into_shared()),
            make_input(cmd),
        )
        .await;

        let mut concatenated = String::new();
        let mut last_delta_total: Option<usize> = None;
        let mut terminal: Option<Result<BashToolOutput, echo_agent_tool_runtime::ToolError>> = None;
        while let Some(item) = stream.next().await {
            match item {
                echo_agent_tool_runtime::ToolStreamItem::Progress(p) => {
                    assert!(terminal.is_none(), "Progress arrived after Terminal");
                    let (delta, total, _truncated, gap) = read_chunk_progress(&p);
                    assert!(
                        !gap,
                        "small untruncated run should never report a gap delta"
                    );
                    concatenated.push_str(&delta);
                    last_delta_total = Some(total);
                }
                echo_agent_tool_runtime::ToolStreamItem::Terminal(r) => {
                    assert!(terminal.is_none(), "more than one Terminal yielded");
                    terminal = Some(r);
                }
            }
        }

        match terminal.expect("stream ended without a Terminal") {
            Ok(BashToolOutput::Foreground(bash)) => {
                assert_eq!(bash.exit_code, 0);
                assert!(!bash.truncated, "test command output is small");
                let terminal_out = String::from_utf8_lossy(&bash.output).into_owned();
                // Tail-byte regression: every byte in the terminal output
                // must have been surfaced via at least one delta.
                assert_eq!(
                    concatenated, terminal_out,
                    "concatenated deltas must equal terminal output (tail bytes lost?)"
                );
                let last_total = last_delta_total
                    .expect("at least one delta is required so the tail is surfaced");
                assert_eq!(
                    last_total, bash.total_bytes,
                    "last delta total_bytes must reach the terminal total_bytes \
                     (final BashNotificationBase was discarded?)"
                );
            }
            other => panic!("expected Terminal(Ok(Foreground)), got {other:?}"),
        }
    }

    // ─── Tests ───

    #[tokio::test]
    async fn foreground_command_success() {
        let resources = make_resources(MockTerminal::success("hello world\n", 0));
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("echo hello"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Foreground(bash) => {
                assert_eq!(bash.exit_code, 0);
                assert_eq!(bash.command, "echo hello");
                assert!(!bash.timed_out);
                assert!(!bash.truncated);
                assert_eq!(String::from_utf8_lossy(&bash.output), "hello world\n");
                assert_eq!(bash.current_dir, "/tmp");
            }
            BashToolOutput::Background(_) => panic!("Expected foreground output"),
        }
    }

    #[tokio::test]
    async fn foreground_command_timeout() {
        let resources = make_resources(MockTerminal::timed_out("partial output"));
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("sleep 999"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Foreground(bash) => {
                assert!(bash.timed_out);
                assert_eq!(bash.exit_code, -1); // no exit code when timed out
            }
            BashToolOutput::Background(_) => panic!("Expected foreground output"),
        }
    }

    #[tokio::test]
    async fn foreground_command_error() {
        let resources = make_resources(MockTerminal::failing());
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("bad_cmd"),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command failed"));
    }

    #[tokio::test]
    async fn background_command_starts() {
        let resources = make_resources(MockTerminal::background_ok("bg-task-42"));
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_bg_input("sleep 3600"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Background(bg) => {
                assert_eq!(bg.task_id, "bg-task-42");
                assert_eq!(bg.task_type, "bash");
                assert_eq!(bg.status, "running");
                assert_eq!(bg.command, "sleep 3600");
                assert!(bg.retrieval_hint.contains("get_task_output"));
                assert!(bg.retrieval_hint.contains("task_id"));
                assert!(bg.retrieval_hint.contains("bg-task-42"));
            }
            BashToolOutput::Foreground(_) => panic!("Expected background output"),
        }
    }

    #[tokio::test]
    async fn background_injects_python_unbuffered() {
        let (mock, captured) = MockTerminal::background_ok_capturing("bg-env");
        let resources = make_resources(mock);
        let tool = BashTool;

        let _ = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_bg_input("python3 script.py"),
        )
        .await
        .unwrap();

        let req = captured.lock().unwrap();
        let req = req
            .as_ref()
            .expect("run_background should have been called");
        assert_eq!(
            req.env.get("PYTHONUNBUFFERED").map(String::as_str),
            Some("1")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_operator_rejected() {
        let resources = make_resources_reject_bg_op(MockTerminal::success("", 0));
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("sleep 10 &"),
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("background"),
            "Error should mention background: {}",
            err
        );
        assert!(err.contains("&"), "Error should mention &: {}", err);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_operator_mid_command_rejected() {
        let resources = make_resources_reject_bg_op(MockTerminal::success("", 0));
        let tool = BashTool;

        // Previously this would pass because the old check only looked at
        // trailing `&`. Now the parser detects mid-command `&` too.
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("sleep 600 &; echo done"),
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("background"),
            "Error should mention background: {}",
            err
        );
    }

    /// With default params (flag on) a foreground `&` is accepted end-to-end, not
    /// just at the pure-helper level — guards against a second `&` check creeping
    /// into `run()` that the gate-helper tests would miss.
    #[cfg(unix)]
    #[tokio::test]
    async fn background_operator_allowed_by_default() {
        let resources = make_resources(MockTerminal::success("hi\n", 0));
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("make & echo hi"),
        )
        .await;
        assert!(
            result.is_ok(),
            "default flag should accept foreground `&`, got: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn background_request_rejected_when_disabled() {
        let resources = make_resources_with_params(
            MockTerminal::background_ok("bg-task-disabled"),
            BashParams {
                enabled_background: false,
                ..BashParams::default()
            },
        );
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_bg_input("sleep 3600"),
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert_eq!(err, "Background execution is disabled.");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_operator_rejected_without_is_background_hint_when_disabled() {
        let resources = make_resources_with_params(
            MockTerminal::success("", 0),
            BashParams {
                enabled_background: false,
                ..BashParams::default()
            },
        );
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("sleep 10 &"),
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert_eq!(
            err,
            "Remove the background '&' from your command; background execution is disabled."
        );
        assert!(
            !err.contains("is_background=true"),
            "disabled-background error should not suggest background: {err}"
        );
    }

    /// Enabled-background `&` rejection must name the real param resolved from
    /// the template (`is_background`), never a blank "set =true". Regression: the
    /// template previously used the non-existent `params.execute.background` key,
    /// which resolved to "".
    #[cfg(unix)]
    #[tokio::test]
    async fn background_operator_rejection_names_is_background_param() {
        let resources = make_resources_reject_bg_op(MockTerminal::success("", 0));
        let tool = BashTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("sleep 10 &"),
        )
        .await;
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("is_background=true"),
            "rejection must name the real param: {err}"
        );
        assert!(
            !err.contains(" =true"),
            "rejection must never render a blank param: {err}"
        );
    }

    #[tokio::test]
    async fn cmd_prefix_prepended() {
        // We can test this via the static helper
        assert_eq!(
            BashTool::get_prefixed_command(&Some("source ~/.bashrc".to_string()), "ls"),
            "source ~/.bashrc && ls"
        );
        assert_eq!(BashTool::get_prefixed_command(&None, "ls"), "ls");
    }

    #[tokio::test]
    async fn errors_when_terminal_not_in_resources() {
        let mut resources = Resources::new();
        resources.insert(Cwd(PathBuf::from("/tmp")));
        resources.insert(SessionFolder(PathBuf::from("/tmp/s")));
        // No Terminal inserted
        let tool = BashTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_input("ls"),
        )
        .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required resource")
        );
    }

    #[tokio::test]
    async fn tool_name_mapping_in_background_hint() {
        let mut resources = make_resources(MockTerminal::background_ok("t1"));
        // Custom model-facing tool AND param names — the hint must track both
        // (a hardcoded `task_ids` goes stale after randomization renames).
        resources.insert(TemplateRenderer::new(
            [(ToolKind::BackgroundTaskAction, "GetOutput".to_string())].into(),
            [(
                ToolKind::BackgroundTaskAction,
                HashMap::from([("task_ids".to_string(), "jobs".to_string())]),
            )]
            .into(),
        ));

        let tool = BashTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_bg_input("sleep 60"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Background(bg) => {
                assert!(
                    bg.retrieval_hint.contains("GetOutput"),
                    "Hint should use model-facing tool name 'GetOutput': {}",
                    bg.retrieval_hint
                );
                assert!(
                    bg.retrieval_hint.contains("jobs=[\"t1\"]"),
                    "Hint should pass the task id via the renamed task_ids param: {}",
                    bg.retrieval_hint
                );
            }
            BashToolOutput::Foreground(_) => panic!("Expected background output"),
        }
    }

    /// The get-output tool is absent from the finalized toolset (no
    /// `BackgroundTaskAction` mapping): the retrieval hint must fall back to
    /// the canonical `get_task_output` name instead of rendering an empty
    /// tool name. A missing kind renders as empty-`Ok` (lenient undefined),
    /// so the old `Result`-based fallback never fired.
    #[tokio::test]
    async fn background_hint_falls_back_when_get_output_tool_absent() {
        let mut resources = make_resources(MockTerminal::background_ok("t2"));
        resources.insert(TemplateRenderer::new(HashMap::new(), HashMap::new()));

        let tool = BashTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            make_bg_input("sleep 60"),
        )
        .await
        .unwrap();

        match result {
            BashToolOutput::Background(bg) => {
                assert!(
                    bg.retrieval_hint.contains(
                        "Use get_task_output with task_ids=[\"t2\"] when you need the output."
                    ),
                    "Hint should fall back to canonical names: {}",
                    bg.retrieval_hint
                );
            }
            BashToolOutput::Foreground(_) => panic!("Expected background output"),
        }
    }

    // -----------------------------------------------------------------------
    // format_default_prompt tests
    // -----------------------------------------------------------------------

    fn make_bash_output(exit_code: i32, output: &str) -> BashOutput {
        let mut bash = BashOutput {
            output: output.as_bytes().to_vec(),
            output_for_prompt: BashOutput::make_output_for_prompt(output),
            exit_code,
            command: "cat test".to_string(),
            truncated: false,
            signal: None,
            timed_out: false,
            description: None,
            current_dir: "/tmp".to_string(),
            output_file: String::new(),
            total_bytes: output.len(),
            output_delta: None,
            was_bare_echo: false,
        };
        bash.output_for_prompt = format_default_prompt(&bash);
        bash
    }

    #[test]
    fn foreground_chat_completion_emits_code_execution_result() {
        let resp = echo_agent_tool_runtime::ToolOutput::chat_completion_output(
            &BashToolOutput::Foreground(make_bash_output(0, "hi\n")),
        )
        .unwrap();
        let result = resp.result.as_ref().unwrap();
        assert_eq!(result.sender, "assistant");
        assert_eq!(result.message_tag.as_deref(), Some("raw_function_result"));
        let cer = result.code_execution_result.as_ref().unwrap();
        assert_eq!(cer.stdout, "hi\n");
        assert!(cer.stderr.is_empty());
        assert_eq!(cer.exit_code, 0);
        assert!(!cer.command_timed_out);
    }

    #[test]
    fn background_chat_completion_is_none() {
        let out = BashToolOutput::Background(BackgroundTaskStarted {
            task_id: "t1".into(),
            task_type: "bash".into(),
            output_file: "/tmp/out".into(),
            status: "running".into(),
            command: "sleep 99".into(),
            summary: "running".into(),
            retrieval_hint: String::new(),
            pre_formatted: None,
            pid: None,
        });
        assert!(echo_agent_tool_runtime::ToolOutput::chat_completion_output(&out).is_none());
    }

    #[test]
    fn default_prompt_starts_with_exit() {
        let bash = make_bash_output(0, "hello world\n");
        assert!(
            bash.output_for_prompt.starts_with("exit: 0"),
            "DEFAULT should start with 'exit: 0', got: {}",
            bash.output_for_prompt
        );
        assert!(!bash.output_for_prompt.contains("Exit code:"));
        assert!(!bash.output_for_prompt.contains("```"));
    }

    #[test]
    fn default_prompt_timeout_annotations() {
        let mut bash = make_bash_output(-1, "partial\n");
        bash.signal = Some("timeout".to_string());
        bash.timed_out = true;
        bash.output_for_prompt = format_default_prompt(&bash);
        // Synthetic kill reasons render as `exit: killed (reason)` — no
        // redundant `[signal=…]` / `[timeout]` annotation.
        assert!(
            bash.output_for_prompt.starts_with("exit: killed (timeout)"),
            "expected `exit: killed (timeout)` header, got: {}",
            bash.output_for_prompt,
        );
        assert!(!bash.output_for_prompt.contains("[signal=timeout]"));
        assert!(!bash.output_for_prompt.contains("[timeout]"));
        assert!(!bash.output_for_prompt.starts_with("exit: -1"));
    }

    #[test]
    fn kill_reason_parse_and_display_round_trip() {
        for s in ["timeout", "max_runtime", "cancelled", "killed", "signal 15"] {
            let parsed = s
                .parse::<KillReason>()
                .unwrap_or_else(|_| panic!("should parse: {s}"));
            assert_eq!(parsed.to_string(), s, "Display must round-trip: {s}");
        }
        // Non-kill labels — `oom`, `backgrounded`, ad-hoc errors, malformed
        // signals — must not parse as a kill reason.
        for s in [
            "oom",
            "backgrounded",
            "auto_backgrounded",
            "error: io",
            "signal",
            "signal abc",
            "",
        ] {
            assert!(
                s.parse::<KillReason>().is_err(),
                "must not parse as kill reason: {s:?}"
            );
        }
    }

    #[test]
    fn default_prompt_killed_reasons() {
        // Every synthetic harness-kill signal renders as `exit: killed (reason)`.
        for reason in ["timeout", "max_runtime", "cancelled", "killed", "signal 15"] {
            let mut bash = make_bash_output(-1, "partial\n");
            bash.signal = Some(reason.to_string());
            bash.output_for_prompt = format_default_prompt(&bash);
            let expected = format!("exit: killed ({})", reason);
            assert!(
                bash.output_for_prompt.starts_with(&expected),
                "expected header `{}`, got: {}",
                expected,
                bash.output_for_prompt,
            );
            assert!(!bash.output_for_prompt.contains("[signal="));
        }
    }

    #[test]
    fn default_prompt_real_exit_code_unchanged() {
        // Real exit codes (including OOM at 137) keep `exit: N [signal=…]`.
        let bash = make_bash_output(1, "boom\n");
        assert!(bash.output_for_prompt.starts_with("exit: 1\n"));

        let mut oom = make_bash_output(137, "killed\n");
        oom.signal = Some("oom".to_string());
        oom.output_for_prompt = format_default_prompt(&oom);
        assert!(oom.output_for_prompt.starts_with("exit: 137 [signal=oom]"));
    }

    #[test]
    fn default_prompt_backgrounded() {
        let mut bash = make_bash_output(0, "partial output\n");
        bash.signal = Some("backgrounded".to_string());
        bash.output_file = "/tmp/bg.log".to_string();
        bash.total_bytes = 10000;
        bash.output_for_prompt = format_default_prompt(&bash);
        assert!(
            bash.output_for_prompt
                .starts_with("[Command moved to background]")
        );
        assert!(
            bash.output_for_prompt
                .contains("still running in the background")
        );
    }

    // ─── contains_background_operator unit tests ───

    mod background_operator_tests {
        use super::super::{
            command_has_bash_background_operator, contains_background_operator,
            contains_unwaited_background_operator,
        };

        // ── Should detect (true) ──

        #[test]
        fn trailing_ampersand() {
            assert!(contains_background_operator("sleep 600 &"));
        }

        #[test]
        fn helper_dispatches_on_contract_version() {
            // current: flags an unwaited `&` anywhere, allows `&&`.
            assert!(command_has_bash_background_operator(
                "echo a & echo b",
                false
            ));
            assert!(!command_has_bash_background_operator(
                "echo a && echo b",
                false
            ));
            // legacy: only a trailing `&`.
            assert!(command_has_bash_background_operator("sleep 1 &", true));
            assert!(!command_has_bash_background_operator(
                "echo a & echo b",
                true
            ));
        }

        #[test]
        fn trailing_ampersand_with_whitespace() {
            assert!(contains_background_operator("sleep 600 &  "));
        }

        #[test]
        fn mid_command_background() {
            assert!(contains_background_operator("sleep 600 &; echo done"));
        }

        #[test]
        fn mid_command_background_no_semicolon() {
            assert!(contains_background_operator("sleep 600 & echo done"));
        }

        #[test]
        fn multiple_backgrounds() {
            assert!(contains_background_operator("cmd1 & cmd2 & cmd3"));
        }

        #[test]
        fn background_in_subshell() {
            assert!(contains_background_operator("(sleep 600 &)"));
        }

        #[test]
        fn background_after_logical_and() {
            // `cmd1 && cmd2 &` — the last `&` backgrounds cmd2
            assert!(contains_background_operator("cmd1 && cmd2 &"));
        }

        #[test]
        fn background_with_redirect_before() {
            // `cmd > out.txt &` — redirect then background
            assert!(contains_background_operator("cmd > out.txt &"));
        }

        // ── Should NOT detect (false) ──

        #[test]
        fn no_ampersand() {
            assert!(!contains_background_operator("echo hello"));
        }

        #[test]
        fn logical_and() {
            assert!(!contains_background_operator("cmd1 && cmd2"));
        }

        #[test]
        fn multiple_logical_and() {
            assert!(!contains_background_operator("cmd1 && cmd2 && cmd3"));
        }

        #[test]
        fn redirect_stdout_stderr() {
            assert!(!contains_background_operator("cmd &>/dev/null"));
        }

        #[test]
        fn append_redirect_stdout_stderr() {
            assert!(!contains_background_operator("cmd &>>output.log"));
        }

        #[test]
        fn fd_duplication_stdout() {
            assert!(!contains_background_operator("cmd 2>&1"));
        }

        #[test]
        fn fd_duplication_stdin() {
            assert!(!contains_background_operator("cmd <&3"));
        }

        #[test]
        fn fd_redirect_stderr_to_stdout() {
            assert!(!contains_background_operator("cmd >&2"));
        }

        #[test]
        fn inside_single_quotes() {
            assert!(!contains_background_operator("echo 'sleep 600 &'"));
        }

        #[test]
        fn inside_double_quotes() {
            assert!(!contains_background_operator("echo \"sleep 600 &\""));
        }

        #[test]
        fn escaped_ampersand() {
            assert!(!contains_background_operator("echo hello \\& world"));
        }

        #[test]
        fn ampersand_in_url_in_quotes() {
            assert!(!contains_background_operator(
                "curl \"http://example.com?a=1&b=2\""
            ));
        }

        #[test]
        fn mixed_redirect_and_logical_and() {
            assert!(!contains_background_operator(
                "cmd1 2>&1 && cmd2 &>/dev/null"
            ));
        }

        #[test]
        fn empty_command() {
            assert!(!contains_background_operator(""));
        }

        #[test]
        fn only_ampersand_in_single_quotes() {
            assert!(!contains_background_operator("'&'"));
        }

        #[test]
        fn escaped_ampersand_in_double_quotes() {
            // Inside double quotes, `\&` is just `\&` (backslash is not
            // special before `&` in double quotes), but our parser still
            // sees it as inside quotes, so it's fine.
            assert!(!contains_background_operator("\"\\&\""));
        }

        // ── Mixed cases ──

        #[test]
        fn logical_and_then_background() {
            // `echo a && sleep 10 &` — the trailing `&` IS a background op.
            assert!(contains_background_operator("echo a && sleep 10 &"));
        }

        #[test]
        fn redirect_then_background() {
            // `cmd 2>&1 &` — fd dup `>&` is fine, but trailing `&` is background.
            assert!(contains_background_operator("cmd 2>&1 &"));
        }

        #[test]
        fn background_between_quoted_ampersands() {
            // The middle `&` is outside quotes, the others are inside.
            assert!(contains_background_operator("echo '&' & echo \"&\""));
        }

        // ── contains_unwaited_background_operator (combined check) ──

        #[test]
        fn unwaited_trailing_ampersand_rejected() {
            assert!(contains_unwaited_background_operator("sleep 600 &"));
        }

        #[test]
        fn unwaited_mid_command_rejected() {
            assert!(contains_unwaited_background_operator(
                "sleep 600 &; echo done"
            ));
        }

        #[test]
        fn waited_fork_pattern_allowed() {
            // `cmd1 & cmd2 & wait` — fork sub-processes, then wait.
            assert!(!contains_unwaited_background_operator("cmd1 & cmd2 & wait"));
        }

        #[test]
        fn waited_fork_pattern_with_trailing_semicolon() {
            assert!(!contains_unwaited_background_operator(
                "cmd1 & cmd2 & wait;"
            ));
        }

        #[test]
        fn waited_fork_pattern_with_trailing_whitespace() {
            assert!(!contains_unwaited_background_operator(
                "cmd1 & cmd2 & wait  "
            ));
        }

        #[test]
        fn waited_fork_pattern_process_group_test() {
            // Exact pattern used in test_cancel_kills_process_group.
            let cmd = ": marker; bash -c 'exec -a child sleep 300' & bash -c 'exec -a child sleep 300' & wait";
            assert!(!contains_unwaited_background_operator(cmd));
        }

        #[test]
        fn no_ampersand_still_allowed() {
            assert!(!contains_unwaited_background_operator("echo hello"));
        }

        #[test]
        fn logical_and_still_allowed() {
            assert!(!contains_unwaited_background_operator("cmd1 && cmd2"));
        }

        #[test]
        fn wait_without_background_allowed() {
            // `wait` alone is fine (no `&` to detect).
            assert!(!contains_unwaited_background_operator("wait"));
        }

        #[test]
        fn await_not_confused_with_wait() {
            // `await` at end should NOT suppress the check — it's not the
            // `wait` builtin.
            assert!(contains_unwaited_background_operator("sleep 600 & await"));
        }

        // ── Heredoc: `&` inside heredoc bodies must not be detected ──

        #[test]
        fn heredoc_single_quoted_delimiter() {
            // `&http.Request` inside a heredoc is not a background op.
            assert!(!contains_background_operator(
                "cat > /tmp/test.go << 'EOF'\nreq := &http.Request{}\nEOF",
            ));
        }

        #[test]
        fn heredoc_double_quoted_delimiter() {
            assert!(!contains_background_operator(
                "cat << \"END\"\nfoo & bar\nEND",
            ));
        }

        #[test]
        fn heredoc_unquoted_delimiter() {
            assert!(!contains_background_operator("cat << EOF\nfoo & bar\nEOF",));
        }

        #[test]
        fn heredoc_strip_tabs() {
            // <<- strips leading tabs from body and delimiter.
            assert!(!contains_background_operator(
                "cat <<- EOF\n\tfoo & bar\n\tEOF",
            ));
        }

        #[test]
        fn heredoc_go_code_false_positive() {
            // Exact pattern from the bug report: Go code with &http.Request
            // inside a heredoc was incorrectly flagged.
            let cmd = "cd /testbed && cat > /tmp/test_header.go << 'EOF'\n\
                        package main\n\
                        \n\
                        import (\n\
                        \t\"net/http\"\n\
                        \t\"fmt\"\n\
                        )\n\
                        \n\
                        func main() {\n\
                        \treq := &http.Request{\n\
                        \t\tHeader: http.Header{\n\
                        \t\t\t\"X-Flow-ID\": []string{\"test-flow-12345\"},\n\
                        \t\t},\n\
                        \t}\n\
                        \tfmt.Println(\"Direct map access:\", req.Header[\"X-Flow-ID\"])\n\
                        }\n\
                        EOF\n\
                        go run /tmp/test_header.go";
            assert!(
                !contains_background_operator(cmd),
                "& inside heredoc body should not be detected as background operator",
            );
        }

        #[test]
        fn heredoc_with_background_after() {
            // `&` AFTER the heredoc is still detected.
            assert!(contains_background_operator(
                "cat << EOF\ncontent\nEOF\nsleep 60 &",
            ));
        }

        #[test]
        fn heredoc_with_background_on_same_line() {
            // `cat << EOF &` -- the `&` is on the command line (before the
            // heredoc body starts) and IS a background operator.
            assert!(contains_background_operator("cat << EOF &\ncontent\nEOF",));
        }

        #[test]
        fn here_string_not_treated_as_heredoc() {
            // `<<<` is a here-string, not a heredoc. `&` after it is background.
            assert!(contains_background_operator("cat <<< 'hello' &"));
        }

        #[test]
        fn heredoc_logical_and_on_same_line_still_parsed() {
            // `&&` on the heredoc line is still correctly parsed as logical AND,
            // even though the heredoc body contains `&`.
            assert!(!contains_background_operator(
                "cat << EOF && echo done\ncontent & stuff\nEOF",
            ));
        }

        #[test]
        fn heredoc_background_on_same_line_detected() {
            // Single `&` on the heredoc command line IS a background op.
            assert!(contains_background_operator(
                "cat << EOF & echo done\ncontent\nEOF",
            ));
        }

        #[test]
        fn heredoc_multiple_on_same_line() {
            // Two heredocs on the same line: bodies consumed in order.
            assert!(!contains_background_operator(
                "cat << EOF1; cat << EOF2\nbody1 &\nEOF1\nbody2 &\nEOF2",
            ));
        }

        #[test]
        fn heredoc_body_not_detected_unwaited() {
            assert!(!contains_unwaited_background_operator(
                "cat << 'EOF'\nreq := &http.Request{}\nEOF",
            ));
        }

        #[test]
        fn heredoc_unclosed_treats_rest_as_body() {
            // If the delimiter is never found, everything after is treated
            // as heredoc content (safe: avoids false positives).
            assert!(!contains_background_operator("cat << EOF\nfoo & bar",));
        }
    }

    // ─── resolve_effective_timeout unit tests ───

    mod resolve_effective_timeout_tests {
        use super::super::{BashParams, BashTool, DEFAULT_MAX_TIMEOUT_MS, DEFAULT_TIMEOUT};
        use std::time::Duration;

        fn resolve(input: Option<u64>, is_bg: bool, max_configured: Option<u64>) -> Duration {
            let max_ms = max_configured.unwrap_or(DEFAULT_MAX_TIMEOUT_MS);
            BashTool::resolve_effective_timeout(input, is_bg, DEFAULT_TIMEOUT, max_ms)
        }

        #[test]
        fn foreground_zero_uses_default() {
            assert_eq!(resolve(Some(0), false, None), DEFAULT_TIMEOUT);
        }

        #[test]
        fn foreground_none_uses_default() {
            assert_eq!(resolve(None, false, None), DEFAULT_TIMEOUT);
        }

        #[test]
        fn background_zero_is_unbounded_without_max_config() {
            assert_eq!(resolve(Some(0), true, None), Duration::MAX);
        }

        #[test]
        fn background_none_is_unbounded_without_max_config() {
            assert_eq!(resolve(None, true, None), Duration::MAX);
        }

        #[test]
        fn background_zero_unbounded_even_with_max_config() {
            // `max_timeout_secs` is a foreground-only ceiling; background tasks
            // (`timeout: 0`) are always unbounded regardless of a configured max.
            assert_eq!(resolve(Some(0), true, Some(300_000)), Duration::MAX);
        }

        #[test]
        fn background_positive_applied() {
            assert_eq!(
                resolve(Some(5_000), true, None),
                Duration::from_millis(5_000),
            );
        }

        #[test]
        fn background_positive_not_clamped_to_foreground_max() {
            // A background command's explicit positive timeout is a model-chosen
            // kill-backstop; max_timeout_secs (foreground-only) must NOT cap it.
            // With a 60s foreground max, a 20-minute background timeout is honored.
            let params = BashParams {
                max_timeout_secs: Some(60.0),
                ..Default::default()
            };
            assert_eq!(BashTool::effective_max_timeout_ms(&params), 60_000);
            assert_eq!(
                BashTool::resolve_effective_timeout_for_params(
                    Some(20 * 60 * 1_000),
                    true, // background
                    super::super::BACKGROUND_TIMEOUT,
                    &params,
                ),
                Duration::from_millis(20 * 60 * 1_000),
            );
            // Only the absolute 10h limit bounds it.
            let above_absolute: u64 = 50 * 60 * 60 * 1_000;
            assert_eq!(
                BashTool::resolve_effective_timeout_for_params(
                    Some(above_absolute),
                    true,
                    super::super::BACKGROUND_TIMEOUT,
                    &params,
                ),
                Duration::from_millis(super::super::ABSOLUTE_MAX_TIMEOUT_MS),
            );
        }

        #[test]
        fn foreground_positive_clamped_to_default_max() {
            // The built-in default foreground ceiling is 5 min; production
            // echo-agent-shell opts up to 10h via max_timeout_secs. Background unaffected.
            assert_eq!(DEFAULT_MAX_TIMEOUT_MS, 300_000);
            let above_max: u64 = 50 * 60 * 60 * 1_000;
            assert_eq!(
                resolve(Some(above_max), false, None),
                Duration::from_millis(300_000),
            );
        }

        #[test]
        fn configured_max_clamps_positive_timeout() {
            let params = BashParams {
                max_timeout_secs: Some(60.0),
                ..Default::default()
            };
            assert_eq!(BashTool::effective_max_timeout_ms(&params), 60_000);
            assert_eq!(
                BashTool::resolve_effective_timeout_for_params(
                    Some(120_000),
                    false,
                    DEFAULT_TIMEOUT,
                    &params,
                ),
                Duration::from_millis(60_000),
            );
        }

        #[test]
        fn sub_millisecond_max_floors_to_1ms() {
            // A positive sub-ms max rounds to 0ms; max_timeout_configured stays
            // true, so the advertised max must not be 0 (it would mismatch the
            // 1ms floor used in timeout resolution). effective_max_timeout_ms
            // floors to 1ms so schema/description == enforcement.
            let params = BashParams {
                max_timeout_secs: Some(0.0004),
                ..Default::default()
            };
            assert!(BashTool::max_timeout_configured(&params));
            assert_eq!(BashTool::effective_max_timeout_ms(&params), 1);
            // Enforcement matches the advertised 1ms ceiling.
            assert_eq!(
                BashTool::resolve_effective_timeout_for_params(
                    Some(5_000),
                    false,
                    DEFAULT_TIMEOUT,
                    &params,
                ),
                Duration::from_millis(1),
            );
        }

        #[test]
        fn unset_max_uses_default_ceiling() {
            assert_eq!(
                BashTool::effective_max_timeout_ms(&BashParams::default()),
                DEFAULT_MAX_TIMEOUT_MS,
            );
            let json = r#"{"timeout_secs":null,"enabled_background":true}"#;
            let p: BashParams = serde_json::from_str(json).unwrap();
            assert!(p.max_timeout_secs.is_none());
        }

        #[test]
        fn foreground_default_capped_when_above_max() {
            let params = BashParams {
                timeout_secs: Some(30.0),
                max_timeout_secs: Some(2.0),
                ..BashParams::default()
            };
            assert_eq!(BashTool::effective_default_timeout_ms(&params), 2_000);
            assert_eq!(
                BashTool::resolve_effective_timeout_for_params(
                    None,
                    false,
                    Duration::from_secs(30),
                    &params,
                ),
                Duration::from_millis(2_000),
            );
        }
    }

    // ─── FG block budget + schema description unit tests ───

    mod foreground_block_budget_tests {
        use super::*;
        use std::time::Duration;

        fn base_schema() -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout": {
                        "type": "integer",
                        "description": "placeholder"
                    },
                    "is_background": { "type": "boolean" },
                    "description": { "type": "string" }
                },
                "required": ["command", "description"]
            })
        }

        fn timeout_desc(params: &BashParams) -> String {
            let schema = BashTool::exported_input_schema(&base_schema(), params, "timeout");
            schema["properties"]["timeout"]["description"]
                .as_str()
                .expect("timeout description")
                .to_string()
        }

        fn tool_desc(params: &BashParams) -> String {
            let renderer = TemplateRenderer::new(
                HashMap::from([
                    (
                        ToolKind::BackgroundTaskAction,
                        "get_task_output".to_string(),
                    ),
                    (ToolKind::KillTaskAction, "kill_task".to_string()),
                ]),
                HashMap::from([(
                    ToolKind::Execute,
                    HashMap::from([("is_background".to_string(), "is_background".to_string())]),
                )]),
            );
            BashTool::rendered_description(None, &renderer, params)
        }

        #[test]
        fn budget_none_when_auto_bg_off() {
            let params = BashParams::default();
            assert!(!params.auto_background_on_timeout);
            assert!(BashTool::effective_foreground_block_budget(&params).is_none());
            assert!(BashTool::effective_auto_bg_wait_ms(&params).is_none());
        }

        #[test]
        fn unset_budget_leaves_request_none_for_backend_default() {
            let params = BashParams {
                auto_background_on_timeout: true,
                foreground_block_budget_ms: None,
                ..BashParams::default()
            };
            // Must not pin 15s on the request — backend applies env/default.
            assert!(BashTool::effective_foreground_block_budget(&params).is_none());
            // Helper still assumes documented 15s default for wait math.
            assert_eq!(
                BashTool::effective_auto_bg_wait_ms(&params),
                Some(DEFAULT_FOREGROUND_BLOCK_BUDGET_MS),
            );
        }

        #[test]
        fn budget_zero_disables_short_budget() {
            let params = BashParams {
                auto_background_on_timeout: true,
                foreground_block_budget_ms: Some(0),
                timeout_secs: Some(30.0),
                ..BashParams::default()
            };
            assert_eq!(
                BashTool::effective_foreground_block_budget(&params),
                Some(Duration::MAX),
            );
            // Wait is purely the default timeout when short budget is off.
            assert_eq!(BashTool::effective_auto_bg_wait_ms(&params), Some(30_000));
        }

        #[test]
        fn custom_budget_and_timeout_min() {
            let params = BashParams {
                auto_background_on_timeout: true,
                foreground_block_budget_ms: Some(5_000),
                timeout_secs: Some(60.0),
                ..BashParams::default()
            };
            assert_eq!(
                BashTool::effective_foreground_block_budget(&params),
                Some(Duration::from_millis(5_000)),
            );
            assert_eq!(BashTool::effective_auto_bg_wait_ms(&params), Some(5_000));
        }

        #[test]
        fn budget_larger_than_default_timeout_uses_timeout() {
            let params = BashParams {
                auto_background_on_timeout: true,
                foreground_block_budget_ms: Some(600_000),
                timeout_secs: Some(10.0),
                ..BashParams::default()
            };
            assert_eq!(BashTool::effective_auto_bg_wait_ms(&params), Some(10_000));
        }

        /// Descriptions already advertise max/default timeout numbers — those
        /// must track BashParams. Do **not** require FG-budget ms in copy yet.
        #[test]
        fn schema_timeout_numbers_track_config() {
            let params = BashParams {
                max_timeout_secs: Some(60.0),
                timeout_secs: Some(30.0),
                ..BashParams::default()
            };
            let desc = timeout_desc(&params);
            assert!(
                desc.contains("max 60000") || desc.contains("60000"),
                "max must track config: {desc}"
            );
            assert!(
                desc.contains("Default: 30000") || desc.contains("30000"),
                "default must track config: {desc}"
            );
            let schema = BashTool::exported_input_schema(&base_schema(), &params, "timeout");
            assert_eq!(
                schema["properties"]["timeout"]["maximum"].as_u64(),
                Some(60_000)
            );
            // Historical auto-bg note only when flag on — no budget ms.
            let auto = BashParams {
                auto_background_on_timeout: true,
                max_timeout_secs: Some(60.0),
                foreground_block_budget_ms: Some(2_000),
                ..BashParams::default()
            };
            let auto_desc = timeout_desc(&auto);
            assert!(
                auto_desc.contains("automatically backgrounded"),
                "auto-bg flag still uses historical copy: {auto_desc}"
            );
            assert!(
                !auto_desc.contains("2000") && !auto_desc.contains("FG block"),
                "must not advertise FG budget ms yet: {auto_desc}"
            );
        }

        /// Property description must track rename after `remap_schema_properties`
        /// (regression: stale `` `timeout: 0` `` under `properties.<alias>`).
        #[test]
        fn schema_property_description_tracks_renamed_timeout() {
            let param_map =
                std::collections::HashMap::from([("timeout".to_string(), "max_wait".to_string())]);
            let exported =
                BashTool::exported_input_schema(&base_schema(), &BashParams::default(), "max_wait");
            let remapped = crate::util::remap::remap_schema_properties(&exported, &param_map);
            let desc = remapped["properties"]["max_wait"]["description"]
                .as_str()
                .expect("max_wait description");
            assert!(
                desc.contains("Optional max_wait in milliseconds"),
                "renamed timeout must appear in property description:\n{desc}"
            );
            assert!(
                !desc.contains("Optional timeout in milliseconds"),
                "canonical timeout must not remain in property description:\n{desc}"
            );
        }

        /// Kind-wide renderer aliases must not rewrite this tool's property
        /// description when this tool's own param_map did not rename timeout —
        /// schema keys only follow param_map.
        #[test]
        fn schema_property_description_ignores_kind_wide_timeout_alias() {
            use crate::types::tool_metadata::ToolMetadata;

            let renderer = TemplateRenderer::new(
                HashMap::from([(ToolKind::Execute, "run_terminal_cmd".to_string())]),
                HashMap::from([(
                    ToolKind::Execute,
                    // Another Execute tool (or identity-seed collision) renamed
                    // timeout kind-wide; this bash tool's param_map is empty.
                    HashMap::from([("timeout".to_string(), "max_wait".to_string())]),
                )]),
            );
            let def = ToolMetadata::versioned_definition(
                &BashTool,
                None,
                "run_terminal_cmd",
                None,
                &renderer,
                &HashMap::new(),
                &base_schema(),
                &serde_json::json!({}),
            );
            let props = def
                .function
                .parameters
                .get("properties")
                .expect("properties");
            assert!(
                props.get("timeout").is_some() && props.get("max_wait").is_none(),
                "empty param_map must keep schema key timeout, got: {props}"
            );
            let desc = props["timeout"]["description"]
                .as_str()
                .expect("timeout description");
            assert!(
                desc.contains("Optional timeout in milliseconds"),
                "property description must match schema key, not kind-wide alias:\n{desc}"
            );
            assert!(
                !desc.contains("max_wait"),
                "kind-wide alias must not leak into property description:\n{desc}"
            );
        }

        #[test]
        fn tool_description_timeout_numbers_track_config() {
            let params = BashParams {
                max_timeout_secs: Some(90.0),
                timeout_secs: Some(45.0),
                auto_background_on_timeout: false,
                ..BashParams::default()
            };
            let desc = tool_desc(&params);
            assert!(
                desc.contains("90000") || desc.contains("90_000"),
                "tool desc max must track config: {desc}"
            );
            assert!(
                desc.contains("45000") || desc.contains("45_000"),
                "tool desc default must track config when auto_bg off: {desc}"
            );
            assert!(
                !desc.contains("FG block") && !desc.contains("auto_bg_wait"),
                "must not advertise FG budget in tool desc yet: {desc}"
            );
        }

        #[test]
        fn serde_accepts_foreground_block_budget_ms() {
            let p: BashParams = serde_json::from_str(
                r#"{"enabled_background":true,"auto_background_on_timeout":true,"foreground_block_budget_ms":0}"#,
            )
            .unwrap();
            assert_eq!(p.foreground_block_budget_ms, Some(0));
            // Omitted → None (server default)
            let p2: BashParams = serde_json::from_str(r#"{"enabled_background":true}"#).unwrap();
            assert!(p2.foreground_block_budget_ms.is_none());
        }
    }

    // ─── self_matching_pkill_pattern unit tests ───

    mod self_matching_pkill_tests {
        use super::super::self_matching_pkill_pattern;

        fn hit(command: &str) -> Option<(&'static str, String)> {
            self_matching_pkill_pattern(command).map(|h| (h.cmd, h.pattern))
        }

        // ── Should detect (Some) ──

        #[test]
        fn pkill_then_run_self() {
            assert_eq!(
                hit("pkill -f ./clavitor-web && sleep 0.5 && ./clavitor-web > log 2>&1"),
                Some(("pkill", "./clavitor-web".into())),
            );
        }

        #[test]
        fn pkill_single_quoted_pattern() {
            assert_eq!(
                hit("pkill -f './clavitor-web' && nohup ./clavitor-web > log 2>&1"),
                Some(("pkill", "./clavitor-web".into())),
            );
        }

        #[test]
        fn pkill_double_quoted_pattern_then_semicolon() {
            assert_eq!(
                hit("pkill -f \"myserver\" ; ./myserver --foo"),
                Some(("pkill", "myserver".into())),
            );
        }

        #[test]
        fn pgrep_pipe_xargs_then_run_self() {
            assert_eq!(
                hit("pgrep -f ./server | xargs -r kill ; ./server &"),
                Some(("pgrep", "./server".into())),
            );
        }

        #[test]
        fn pkill_clustered_flag_fe() {
            // `-fe` includes f.
            assert!(self_matching_pkill_pattern("pkill -fe ./server && ./server").is_some());
        }

        /// Long-form `--full` flag is equivalent to `-f` per `man pkill`
        /// and must be flagged the same way.
        #[test]
        fn pkill_long_full_flag() {
            assert_eq!(
                hit("pkill --full ./server && ./server"),
                Some(("pkill", "./server".into())),
            );
        }

        // ── Should NOT detect (None) ──

        #[test]
        fn pkill_alone_no_later_reference() {
            assert!(self_matching_pkill_pattern("pkill -f ./clavitor-web").is_none());
        }

        #[test]
        fn pkill_x_no_f_flag() {
            // `-x` matches basename only -- not the footgun.
            assert!(
                self_matching_pkill_pattern("pkill -x clavitor-web && ./clavitor-web").is_none()
            );
        }

        #[test]
        fn pkill_different_pattern() {
            assert!(
                self_matching_pkill_pattern("pkill -f ./otherserver && ./clavitor-web").is_none()
            );
        }

        #[test]
        fn pkill_command_substitution_pattern_skipped() {
            // `$(...)` is a non-literal pattern -- skip without emitting.
            assert!(
                self_matching_pkill_pattern("pkill -f \"$(cat pidpat)\" && ./clavitor-web")
                    .is_none()
            );
        }

        #[test]
        fn no_pkill_at_all() {
            assert!(self_matching_pkill_pattern("./clavitor-web --foo bar").is_none());
        }

        #[test]
        fn xpkill_not_a_match() {
            // `xpkill` must not be matched as `pkill`.
            assert!(self_matching_pkill_pattern("xpkill -f ./server && ./server").is_none());
        }

        #[test]
        fn pkill_no_flag_at_all() {
            // `pkill foo` (no -f) is not the footgun -- not detected.
            assert!(self_matching_pkill_pattern("pkill clavitor-web && ./clavitor-web").is_none());
        }

        /// `pgrep` without a subsequent `kill` is a read-only PID lookup
        /// and must not be flagged.
        #[test]
        fn pgrep_without_kill_allowed() {
            assert!(self_matching_pkill_pattern("pgrep -f ./server && echo found").is_none());
        }

        /// One-/two-character patterns are well below the noise floor.
        #[test]
        fn pkill_single_char_pattern_skipped() {
            assert!(self_matching_pkill_pattern("pkill -f a && echo a").is_none());
        }

        /// Realistic multi-character patterns (like `./clavitor-web`) are
        /// still trippable -- guard does not regress the headline footgun.
        #[test]
        fn realistic_pattern_still_flagged() {
            assert!(
                self_matching_pkill_pattern("pkill -f ./clavitor-web && ./clavitor-web > log 2>&1")
                    .is_some()
            );
        }
    }

    // ─── Legacy trailing-only `&` detection ───

    mod legacy_trailing_ampersand {
        use super::*;

        #[test]
        fn trailing_ampersand_detected() {
            assert!(has_trailing_background_operator("sleep 600 &"));
        }

        #[test]
        fn trailing_ampersand_with_whitespace() {
            assert!(has_trailing_background_operator("sleep 600 &  "));
        }

        #[test]
        fn mid_command_ampersand_not_detected() {
            // Legacy only checks trailing — mid-command `&` is allowed.
            assert!(!has_trailing_background_operator("echo a & echo b"));
        }

        #[test]
        fn ampersand_in_quotes_not_detected() {
            assert!(!has_trailing_background_operator(r#"echo "a & b""#));
        }

        #[test]
        fn logical_and_not_detected() {
            assert!(!has_trailing_background_operator("cmd1 && cmd2"));
        }

        #[test]
        fn redirect_not_detected() {
            assert!(!has_trailing_background_operator("cmd 2>&1"));
        }

        #[test]
        fn no_ampersand() {
            assert!(!has_trailing_background_operator("echo hello"));
        }
    }

    // ─── PowerShell `&` detection + remediation ───

    mod powershell_background_check {
        use super::*;

        /// A leading `&` is the PowerShell call operator and must not be flagged;
        /// only a trailing `&` is the background-job operator. `has_trailing_background_operator`
        /// (reused for PowerShell) encodes exactly this.
        #[test]
        fn call_operator_not_flagged_trailing_job_flagged() {
            assert!(!has_trailing_background_operator(
                r#"& "C:\tools\app.exe" --flag"#
            ));
            assert!(!has_trailing_background_operator("& $exe arg1 arg2"));
            assert!(!has_trailing_background_operator(
                "Get-Process | & { $_.Path }"
            ));
            assert!(!has_trailing_background_operator("echo a 2>&1"));
            assert!(!has_trailing_background_operator("cmd *>&1"));
            // A trailing `&` is the job operator (pwsh 7+) / parse error (5.1).
            assert!(has_trailing_background_operator("Start-Sleep 30 &"));
            assert!(has_trailing_background_operator("cmd *>&1 &"));
        }

        #[test]
        fn message_is_edition_specific() {
            let pwsh =
                BashTool::powershell_background_operator_message(true, "is_background", false);
            assert!(pwsh.contains("starts a background job"));
            assert!(pwsh.contains("is_background=true"));

            let ps51 =
                BashTool::powershell_background_operator_message(true, "is_background", true);
            assert!(ps51.contains("Windows PowerShell 5.1"));
            assert!(ps51.contains("is_background=true"));

            let disabled =
                BashTool::powershell_background_operator_message(false, "is_background", false);
            assert!(disabled.contains("disabled"));
            assert!(!disabled.contains("is_background=true"));
        }

        /// A trailing `&` before a statement separator (`cmd &;` / `cmd & ;`) is
        /// still a background op; the bash primitive alone misses it.
        #[test]
        fn trailing_ampersand_before_separator_flagged() {
            assert!(powershell_has_trailing_background("Start-Sleep 30 &;"));
            assert!(powershell_has_trailing_background("cmd & ; "));
            assert!(powershell_has_trailing_background("Start-Sleep 30 &"));
            // Call operator / redirects with a trailing separator stay allowed.
            assert!(!powershell_has_trailing_background(r#"& "C:\app.exe";"#));
            assert!(!powershell_has_trailing_background("echo a 2>&1;"));
        }
    }

    // ─── Shell-aware gate decision (pure, all platforms) ───

    mod gate_decision {
        use super::*;

        /// The pure gate decision is exercised for every shell semantics (and
        /// both contract versions) on all platforms; the `.run()` integration
        /// path can only cover Unix/Git Bash.
        #[test]
        fn detect_background_op_violation_per_shell() {
            use AmpersandSemantics::*;
            use BackgroundOpViolation::*;
            let ps_core = Some(PowerShell {
                trailing_is_syntax_error: false,
            });
            let ps_51 = Some(PowerShell {
                trailing_is_syntax_error: true,
            });

            // bash/POSIX current contract: an unwaited `&` anywhere is rejected.
            assert_eq!(
                detect_background_op_violation(PosixBackground, "sleep 10 &", false),
                Some(Bash)
            );
            assert_eq!(
                detect_background_op_violation(PosixBackground, "echo a & echo b", false),
                Some(Bash)
            );
            assert_eq!(
                detect_background_op_violation(PosixBackground, "echo a && echo b", false),
                None
            );
            // bash/POSIX legacy contract: only a trailing `&` (mid-command allowed).
            assert_eq!(
                detect_background_op_violation(PosixBackground, "sleep 10 &", true),
                Some(Bash)
            );
            assert_eq!(
                detect_background_op_violation(PosixBackground, "echo a & echo b", true),
                None
            );
            // PowerShell: only a trailing `&`; call operator allowed; the edition
            // bit is independent of the bash contract version (`is_legacy`).
            for legacy in [false, true] {
                assert_eq!(
                    detect_background_op_violation(PowerShellCore, "Start-Sleep 10 &", legacy),
                    ps_core
                );
                // Trailing `&` before a separator is still caught.
                assert_eq!(
                    detect_background_op_violation(PowerShellCore, "Start-Sleep 10 &;", legacy),
                    ps_core
                );
                assert_eq!(
                    detect_background_op_violation(WindowsPowerShell, "Start-Sleep 10 &", legacy),
                    ps_51
                );
                assert_eq!(
                    detect_background_op_violation(PowerShellCore, r#"& "C:\app.exe""#, legacy),
                    None
                );
                assert_eq!(
                    detect_background_op_violation(WindowsPowerShell, "echo a 2>&1", legacy),
                    None
                );
            }
            // cmd.exe: `&` is a sequential separator, never rejected.
            assert_eq!(
                detect_background_op_violation(CmdSeparator, "dir & echo done", false),
                None
            );
            assert_eq!(
                detect_background_op_violation(CmdSeparator, "ping -n 1 x &", false),
                None
            );
        }

        /// The flag ships on by default — via both the `Default` impl and,
        /// critically, the serde default: hosts that omit the key from
        /// `params_json` rely on the serde default (not `Default`) to keep
        /// the `&` rejection off on backgrounding-enabled surfaces
        /// (no-background toolsets still reject it).
        #[test]
        fn allow_background_operator_defaults_true() {
            assert!(BashParams::default().allow_background_operator);
            assert!(
                serde_json::from_str::<BashParams>("{}")
                    .unwrap()
                    .allow_background_operator
            );
        }

        /// The gate's behaviors: `is_background` always bypasses (fixes the old
        /// reject-when-already-backgrounded bug); a foreground `&` is accepted only
        /// when the flag AND backgrounding are both on (the default); backgrounding
        /// disabled rejects it even with the flag on (the child can't be tracked);
        /// otherwise it delegates verbatim to `detect_background_op_violation` (whose
        /// full per-shell matrix is covered by `detect_background_op_violation_per_shell`).
        #[test]
        fn gate_short_circuits_else_delegates() {
            use AmpersandSemantics::PosixBackground as P;
            // is_background bypasses regardless of flag/backgrounding.
            assert_eq!(
                should_reject_background_op(true, false, false, P, "sleep 10 &", false),
                None
            );
            // Default: flag on + backgrounding on + foreground → accepted.
            assert_eq!(
                should_reject_background_op(false, true, true, P, "sleep 10 &", false),
                None
            );
            // Backgrounding disabled rejects a foreground `&` even with the flag on.
            assert_eq!(
                should_reject_background_op(false, true, false, P, "sleep 10 &", false),
                Some(BackgroundOpViolation::Bash)
            );
            // Flag off (backgrounding on) + foreground delegates by construction.
            for (s, c, l) in [(P, "sleep 10 &", false), (P, "echo a 2>&1", false)] {
                assert_eq!(
                    should_reject_background_op(false, false, true, s, c, l),
                    detect_background_op_violation(s, c, l)
                );
            }
        }
    }

    // ─── Versioned `&` detection integration ───

    mod versioned_background_check {
        use super::*;

        fn legacy_rt_ctx(
            resources: crate::types::resources::SharedResources,
        ) -> echo_agent_tool_runtime::ToolCallContext {
            let mut ctx = echo_agent_tool_runtime::ToolCallContext::new(
                echo_agent_tool_protocol::ToolCallId::new_v7(),
            );
            ctx.extensions.insert(resources);
            ctx.extensions
                .insert(echo_agent_tool_runtime::BehaviorVersion(
                    "legacy-0.4.10".to_string(),
                ));
            ctx
        }

        /// Under current version, mid-command `&` is rejected (Unix/Git Bash only;
        /// PowerShell/cmd give `&` other meanings — see `ampersand_semantics`).
        #[cfg(unix)]
        #[tokio::test]
        async fn current_rejects_mid_command_ampersand() {
            let resources = make_resources_reject_bg_op(MockTerminal::success("", 0));
            let tool = BashTool;
            let result = echo_agent_tool_runtime::Tool::run(
                &tool,
                test_ctx(resources.into_shared()),
                make_input("echo a & echo b"),
            )
            .await;
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("background '&'"),
                "expected a background '&' error, got: {err}"
            );
        }

        /// Under legacy version, mid-command `&` is allowed.
        #[tokio::test]
        async fn legacy_allows_mid_command_ampersand() {
            let resources = make_resources_reject_bg_op(MockTerminal::success("output", 0));
            let tool = BashTool;
            let result = echo_agent_tool_runtime::Tool::run(
                &tool,
                legacy_rt_ctx(resources.into_shared()),
                make_input("echo a & echo b"),
            )
            .await;
            assert!(
                result.is_ok(),
                "legacy should allow mid-command &, got: {:?}",
                result.err()
            );
        }

        /// Under legacy version, trailing `&` is still rejected (Unix/Git Bash only).
        #[cfg(unix)]
        #[tokio::test]
        async fn legacy_rejects_trailing_ampersand() {
            let resources = make_resources_reject_bg_op(MockTerminal::success("", 0));
            let tool = BashTool;
            let result = echo_agent_tool_runtime::Tool::run(
                &tool,
                legacy_rt_ctx(resources.into_shared()),
                make_input("sleep 600 &"),
            )
            .await;
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("must not end with"),
                "expected 'must not end with' error, got: {err}"
            );
        }

        /// Under legacy version, logical AND (&&) is not rejected.
        #[tokio::test]
        async fn legacy_allows_logical_and() {
            let resources = make_resources_reject_bg_op(MockTerminal::success("output", 0));
            let tool = BashTool;
            let result = echo_agent_tool_runtime::Tool::run(
                &tool,
                legacy_rt_ctx(resources.into_shared()),
                make_input("ls && echo done"),
            )
            .await;
            assert!(
                result.is_ok(),
                "legacy should allow &&, got: {:?}",
                result.err()
            );
        }
    }

    // ─── Description template shell-awareness tests ───
    //
    // Exercises the `${%- if has_unix_utilities %}` branch — fix for
    // `'grep' is not recognized` on PowerShell / cmd.exe. We can't
    // toggle the host shell from a unit test, so we render the template
    // directly with `render_with_extra` and a forced flag value.

    mod description_shell_branches {
        use super::*;

        /// Renderer with every dedicated-tool kind so the inner
        /// `${%- if tools.by_kind.* %}` branches all expand.
        fn full_renderer() -> TemplateRenderer {
            TemplateRenderer::new(
                HashMap::from([
                    (ToolKind::List, "list_dir".to_string()),
                    (ToolKind::Search, "grep".to_string()),
                    (ToolKind::Read, "read_file".to_string()),
                    (ToolKind::Edit, "search_replace".to_string()),
                    (ToolKind::Write, "write".to_string()),
                    (
                        ToolKind::BackgroundTaskAction,
                        "get_task_output".to_string(),
                    ),
                ]),
                HashMap::from([(
                    ToolKind::Execute,
                    HashMap::from([("is_background".to_string(), "is_background".to_string())]),
                )]),
            )
        }

        /// Coupled convenience: a PowerShell host lacks the utilities AND reports
        /// Windows; Unix is the inverse. Use [`render_flags`] to decouple the two
        /// axes (e.g. the Windows + Git Bash quadrant, where both are true).
        fn render(template: &str, has_unix_utilities: bool) -> String {
            render_flags(template, !has_unix_utilities, has_unix_utilities)
        }

        fn render_flags(template: &str, is_windows: bool, has_unix_utilities: bool) -> String {
            let renderer = full_renderer();
            let extras = serde_json::json!({
                "auto_background_on_timeout": true,
                "is_windows": is_windows,
                "shell_uses_semicolon": !has_unix_utilities,
                "has_unix_utilities": has_unix_utilities,
            });
            renderer.render_with_extra(template, &extras).unwrap()
        }

        #[test]
        fn description_tracks_renamed_timeout() {
            let renderer = TemplateRenderer::new(
                HashMap::from([
                    (ToolKind::Execute, "run_terminal_cmd".to_string()),
                    (ToolKind::KillTaskAction, "kill_task".to_string()),
                ]),
                HashMap::from([(
                    ToolKind::Execute,
                    HashMap::from([
                        ("timeout".to_string(), "max_wait".to_string()),
                        ("is_background".to_string(), "is_background".to_string()),
                    ]),
                )]),
            );
            let extras = serde_json::json!({
                "auto_background_on_timeout": true,
                "is_windows": false,
                "shell_uses_semicolon": false,
                "has_unix_utilities": true,
            });
            let out = renderer
                .render_with_extra(BashTool::default_description_template_enabled(), &extras)
                .unwrap();
            assert!(
                out.contains("optional max_wait in milliseconds") && out.contains("`max_wait: 0`"),
                "renamed timeout must appear:\n{out}"
            );
            assert!(
                !out.contains("optional timeout in milliseconds") && !out.contains("`timeout: 0`"),
                "canonical timeout must not remain after rename:\n{out}"
            );
        }

        #[test]
        fn unix_shell_omits_utility_and_chaining_notes() {
            let out = render(BashTool::default_description_template_enabled(), true);
            // On a real bash/unix shell the utilities exist and `&&` works, so
            // neither the unavailable-utilities note nor the `;`-chaining note
            // renders — that guidance is trained in, not repeated in the schema.
            assert!(
                !out.contains("are NOT available in this shell"),
                "must not emit PowerShell warning on Unix, got:\n{out}"
            );
            assert!(
                !out.contains("'&&' is not supported"),
                "must not emit the `;`-chaining note on Unix, got:\n{out}"
            );
            // The OS-neutral contract (timeout/kill mechanics) is still present.
            assert!(out.contains("disables the wrapper timeout"));
        }

        #[test]
        fn powershell_emits_unavailable_and_chaining_notes() {
            let out = render(BashTool::default_description_template_enabled(), false);
            assert!(out.contains(
                "Unix utilities `grep`, `head`, `tail`, `sed`, `awk`, and `find` are NOT available in this shell"
            ), "missing unavailability note, got:\n{out}");
            assert!(
                out.contains("'&&' is not supported in this shell"),
                "missing the `;`-chaining note, got:\n{out}"
            );
            // The verbose legacy discouragement wording must not return.
            assert!(
                !out.contains("Avoid using this tool with the `find`, `grep`, `cat`, `head`"),
                "legacy discouragement wording leaked, got:\n{out}"
            );
        }

        /// Timeout/kill wording and the bash `&` note are shell-specific: Unix
        /// shows SIGTERM/SIGKILL + setsid/nohup; Windows shows Job Object
        /// termination and drops the Unix-only jargon and the trailing-`&` note.
        #[test]
        fn timeout_and_ampersand_text_branch_on_shell() {
            let enabled = BashTool::default_description_template_enabled();

            // Unix (is_windows=false, utilities=true): SIGTERM/SIGKILL + setsid + `&` note.
            let unix = render_flags(enabled, false, true);
            assert!(unix.contains("SIGTERM, escalated to SIGKILL"));
            assert!(unix.contains("setsid"));
            assert!(unix.contains("You do not need to use '&' at the end"));

            // PowerShell (is_windows=true, utilities=false): Job Object wording, no
            // Unix jargon, no `&` note; OS-neutral timeout tail still present.
            let pwsh = render_flags(enabled, true, false);
            assert!(
                pwsh.contains("terminates the child's Job Object"),
                "missing Windows Job Object wording, got:\n{pwsh}"
            );
            assert!(
                !pwsh.contains("SIGTERM"),
                "Unix SIGTERM leaked, got:\n{pwsh}"
            );
            assert!(!pwsh.contains("SIGKILL"));
            assert!(!pwsh.contains("setsid"));
            assert!(!pwsh.contains("nohup"));
            assert!(
                !pwsh.contains("You do not need to use '&' at the end"),
                "bash `&` note leaked into PowerShell description, got:\n{pwsh}"
            );
            assert!(
                pwsh.contains("disables the wrapper timeout"),
                "OS-neutral timeout tail dropped on Windows, got:\n{pwsh}"
            );

            // Windows + Git Bash (is_windows=true, utilities=true): the kill text is
            // OS-level (Job Object) while the `&` note is shell-level — both appear.
            let git_bash = render_flags(enabled, true, true);
            assert!(git_bash.contains("terminates the child's Job Object"));
            assert!(!git_bash.contains("SIGTERM"));
            assert!(
                git_bash.contains("You do not need to use '&' at the end"),
                "Git Bash must keep the `&` note, got:\n{git_bash}"
            );

            // The disabled template branches the timeout line identically.
            let pwsh_disabled = render_flags(
                BashTool::default_description_template_disabled(),
                true,
                false,
            );
            assert!(pwsh_disabled.contains("terminates the child's Job Object"));
            assert!(!pwsh_disabled.contains("SIGTERM"));
        }

        /// The background-disabled template must branch on shell identically:
        /// nothing on Unix, the unavailable-utilities + `;`-chaining notes on
        /// non-bash shells, so containerized agents don't regress.
        #[test]
        fn disabled_template_also_branches_on_shell() {
            let unix = render(BashTool::default_description_template_disabled(), true);
            let pwsh = render(BashTool::default_description_template_disabled(), false);
            assert!(!unix.contains("are NOT available in this shell"));
            assert!(!unix.contains("'&&' is not supported"));
            assert!(pwsh.contains("are NOT available in this shell"));
            assert!(pwsh.contains("'&&' is not supported in this shell"));
        }
    }
}
