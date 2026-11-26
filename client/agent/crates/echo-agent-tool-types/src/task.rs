//! Input/output types for the background-task / sub-agent task tools
//! (`task`, `get_task_output`, `wait_tasks`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ───────────────────────────────────────────────────────────────────────────
// `task` (spawn) tool — Input
// ───────────────────────────────────────────────────────────────────────────

/// Input for the `task` tool — launches a subagent to handle a task
/// autonomously.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskToolInput {
    /// The full task prompt for the subagent to execute.
    #[schemars(description = "The full task prompt for the subagent to execute.")]
    pub prompt: String,

    /// Short description of the task (3-5 words).
    #[schemars(description = "Short description of the task (3-5 words).")]
    pub description: String,

    /// Name of the subagent type to launch. Built-in types: "general-purpose",
    /// "explore", "plan". Additional user-defined types may also be available.
    #[schemars(
        description = "Name of the subagent type to launch. Built-in types: \"general-purpose\", \"explore\", \"plan\". Additional user-defined types may also be available."
    )]
    #[serde(default = "default_subagent_type")]
    pub subagent_type: String,

    /// Whether to run the subagent in the background.
    ///
    /// Returns immediately with a subagent_id. Use the task output tool to
    /// retrieve results. This is set to true by default.
    #[schemars(
        description = "Returns immediately with a subagent_id. Use the task output tool to \
            retrieve results. This is set to true by default."
    )]
    #[serde(
        default = "default_true",
        deserialize_with = "crate::serde_lenient::deserialize_lenient_bool"
    )]
    pub run_in_background: bool,

    /// Harness-internal only. Not advertised on the model-facing schema;
    /// JSON that still sends this key is ignored so a `general-purpose`
    /// child keeps its type's full toolset. Compat-harness adapters and
    /// role/definition defaults still set this in-process.
    #[schemars(skip)]
    #[serde(default, skip_deserializing, skip_serializing)]
    pub capability_mode: Option<SubagentCapabilityMode>,

    /// Isolation mode for the child's execution environment.
    #[schemars(
        description = "Isolation mode: \"none\" (default, shared workspace) or \"worktree\" \
            (isolated git worktree). Worktree mode prevents the child's edits from \
            affecting the parent workspace until explicitly merged."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "desktop", schemars(skip))]
    pub isolation: Option<SubagentIsolationMode>,

    /// Resume a previous subagent's conversation instead of starting fresh.
    ///
    /// The new subagent inherits the source's raw transcript, tool state, and
    /// model. The system prompt and tool configuration are freshly rendered
    /// from the current agent definition. The new task prompt is appended as
    /// the next user message.
    ///
    /// The source subagent must be completed (not active or unknown) and
    /// belong to the same parent session.
    #[schemars(
        description = "Resume from a previously completed subagent's conversation. \
            Pass the subagent_id returned by a prior task call. The new subagent \
            continues the previous one's raw transcript with the new task prompt \
            appended. The source must be completed (not running), belong to the \
            current session, and use the same subagent_type."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from: Option<String>,

    /// Explicit working directory for the subagent. When set, the child
    /// session operates in this directory instead of the parent's cwd.
    /// Mutually exclusive with `isolation: "worktree"` (both set the
    /// effective cwd — setting both is ambiguous).
    /// Path validation (exists, is a directory) happens at subagent launch
    /// time, not here.
    #[cfg_attr(not(feature = "desktop"), schemars(
        description = "Explicit working directory for the subagent. The path must exist and \
            be a directory. Mutually exclusive with isolation=\"worktree\". \
            Ignored when resume_from is set (the resumed child inherits \
            its source's cwd/worktree)."
    ))]
    #[cfg_attr(feature = "desktop", schemars(description = "Working directory for the subagent. The path must be an existing directory."))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Optional model slug for this subagent.
    #[schemars(
        description = "Optional model slug for this agent. If provided, it must resolve to one \
            of the available model slugs. If omitted, the subagent uses the same model as the \
            parent agent. Do not pass if resume_from is set (prior model will be used). Only \
            choose an explicit model when the user directly requests it."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Server-injected before execution. Becomes the subagent's session ID.
    #[schemars(skip)]
    #[serde(default)]
    pub task_id: Option<String>,
}

/// Default `subagent_type` for [`TaskToolInput`] when the caller omits it.
pub fn default_subagent_type() -> String {
    "general-purpose".to_string()
}

/// True when `s` is not a model-emitted placeholder (`""`, `"null"`, `"none"`,
/// `"undefined"`, or whitespace-only after trim).
#[inline]
pub fn is_not_sentinel(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && !t.eq_ignore_ascii_case("null")
        && !t.eq_ignore_ascii_case("none")
        && !t.eq_ignore_ascii_case("undefined")
}

/// Drop sentinels and trim; move the original `String` when no trim is needed.
pub fn sanitize_optional_arg(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        if !is_not_sentinel(&s) {
            return None;
        }
        let trimmed = s.trim();
        if trimmed.len() == s.len() {
            Some(s)
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn default_true() -> bool {
    true
}

/// Capability mode controlling which tool classes a child agent can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SubagentCapabilityMode {
    #[serde(
        alias = "readonly",
        alias = "readOnly",
        alias = "read_only",
        alias = "ReadOnly"
    )]
    ReadOnly,
    #[serde(
        alias = "readwrite",
        alias = "readWrite",
        alias = "read_write",
        alias = "ReadWrite"
    )]
    ReadWrite,
    #[serde(alias = "Execute", alias = "EXECUTE")]
    Execute,
    #[serde(alias = "All", alias = "ALL")]
    All,
}

impl SubagentCapabilityMode {
    /// Canonical wire string (matches the serde `kebab-case` representation).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ReadWrite => "read-write",
            Self::Execute => "execute",
            Self::All => "all",
        }
    }
}

/// Isolation mode for subagent execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SubagentIsolationMode {
    #[default]
    #[serde(alias = "None")]
    None,
    #[serde(alias = "Worktree", alias = "work_tree", alias = "work-tree")]
    Worktree,
}

impl SubagentIsolationMode {
    /// Canonical wire string (matches the serde `kebab-case` representation).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Worktree => "worktree",
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// `task` (spawn) tool — Output
// ───────────────────────────────────────────────────────────────────────────

/// Structured completion output from a subagent (`task` tool).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentCompletedOutput {
    pub output: String,
    pub subagent_id: String,
    pub subagent_type: String,
    pub tool_calls: u32,
    pub turns: u32,
    pub duration_ms: u64,
    pub worktree_path: Option<String>,
    /// Persona used by this subagent, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// The `subagent_id` to pass as `resume_from` to continue this subagent.
    /// Always equals `subagent_id` — provided as a convenience so programmatic
    /// consumers can extract the resume handle without parsing text.
    pub resume_from_hint: String,
    /// If the subagent used a persona, the persona name to pass when resuming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_hint: Option<String>,
}

impl SubagentCompletedOutput {
    /// Render the resume footer showing the subagent ID and resume hint.
    pub fn resume_footer(&self) -> String {
        format_resume_footer(
            &self.subagent_id,
            &self.subagent_type,
            self.persona.as_deref(),
        )
    }

    /// Render the full model-facing completion block: the answer text, the
    /// `<subagent_meta>` line, and the `<subagent_result>` resume footer.
    pub fn to_model_text(&self) -> String {
        format_subagent_completed(
            &self.output,
            &self.subagent_id,
            &self.subagent_type,
            self.tool_calls,
            self.turns,
            self.duration_ms,
            self.persona.as_deref(),
        )
    }
}

/// Plain-text CTA after a background-spawn notice.
///
/// Keep this unwrapped (no `<system-reminder>` / `<system_reminder>` tags).
/// Hardcoding either tag in shared tool text clashes with harness-specific
/// wrappers and can make UIs hide the whole spawn result as a reminder block.
/// Harness-owned reminders go through `format_with_reminders` with
/// `system_reminder_tag`.
pub const BACKGROUND_SUBAGENT_CONTINUE_PARENT_WORK: &str =
    "Do not only poll the child. Continue unfinished parent work now.";

/// How many asks *before* the latest one may still count as leftover parent
/// exec. Older implement/fix history after the user switched to review-only
/// must not keep the CTA on.
const PRIOR_EXEC_LOOKBACK: usize = 2;

/// Whether background-spawn text should tell the parent to keep its own work.
///
/// `user_asks` are recent parent user texts (oldest → newest), not including
/// this spawn's tool call. `child_description` / `child_prompt` are the spawn
/// being acknowledged.
///
/// Returns true only when the latest ask (or either of the two before it)
/// shows unfinished parent exec work besides the delegated child job.
/// No user asks → false.
pub fn should_continue_parent_work(
    user_asks: &[String],
    child_description: &str,
    child_prompt: &str,
) -> bool {
    let child = format!("{child_description}\n{child_prompt}");
    let Some((last, prior)) = user_asks.split_last() else {
        return false;
    };
    let skip = prior.len().saturating_sub(PRIOR_EXEC_LOOKBACK);
    let recent_prior = &prior[skip..];
    if recent_prior.iter().any(|a| blob_has_exec(a)) {
        return true;
    }
    if blob_has_exec(last) && (blob_is_delegate(last) || blob_is_delegate(&child)) {
        return true;
    }
    if blob_has_exec(last) && !blob_is_delegate(last) {
        return true;
    }
    // "while waiting, spawn …" — parent still has the waiting work.
    let last_l = last.to_ascii_lowercase();
    if last_l.contains("while waiting") || last_l.contains("whilst waiting") {
        return true;
    }
    false
}

fn blob_has_exec(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "smoke",
        "op_chain",
        "ci fail",
        "ci failure",
        "still fail",
        "still fails",
        "failing",
        "bazel test",
        "pytest",
        "cargo test",
        "npm test",
        "deploy",
        "bringup",
        "implement",
        "unfinished",
        "rebase",
        "adler",
        "nondetermin",
        "fix the ",
        "fix these ",
        "fix all ",
        "gt submit",
        "check ",
        " bug",
        "bugs",
        "run the test",
        "run tests",
        "pass/fail",
        "integration test",
        "hw5",
        "pr check",
        "ci check",
    ];
    NEEDLES.iter().any(|n| t.contains(n))
}

fn blob_is_delegate(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "spawn an agent",
        "spawn a subagent",
        "spawn a agent",
        "spawn subagent",
        "spawn agent",
        "spawn as many",
        "spawn 4",
        "spawn four",
        "/pr-babysit",
        "pr-babysit",
        "/code-review",
        "/review",
        "review this pr",
        "review the pr",
        "review this pull",
        "code review",
        "colossus-review",
        "peer review",
        "subagent review",
        "independent review",
        "reviewer",
    ];
    NEEDLES.iter().any(|n| t.contains(n))
}

/// Model-facing names used by the background-subagent notices. The retrieval
/// tool and both of its parameters are host-renameable (tool randomization),
/// so callers resolve them (e.g. from their `TemplateRenderer`) instead of
/// baking the canonical names into the notice text.
#[derive(Clone, Copy, Debug)]
pub struct BackgroundNoticeNaming<'a> {
    /// Task-result retrieval tool (canonical: `get_task_output`).
    pub task_output_tool: &'a str,
    /// Its ids parameter (canonical: `task_ids`).
    pub task_ids_param: &'a str,
    /// Its wait parameter (canonical: `timeout_ms`).
    pub timeout_ms_param: &'a str,
}

impl BackgroundNoticeNaming<'static> {
    /// Canonical echo-agent-build names, for hosts without renaming.
    pub const CANONICAL: Self = Self {
        task_output_tool: "get_task_output",
        task_ids_param: "task_ids",
        timeout_ms_param: "timeout_ms",
    };
}

/// Shared retrieval line for background notices: names this id and the
/// host-facing get-output tool/params. Polling policy lives in the system prompt.
fn background_result_line(subagent_id: &str, naming: &BackgroundNoticeNaming) -> String {
    let BackgroundNoticeNaming {
        task_output_tool,
        task_ids_param,
        timeout_ms_param,
    } = *naming;
    format!(
        "When you need its result, use {task_output_tool} with {task_ids_param}=[\"{subagent_id}\"] and a positive {timeout_ms_param}."
    )
}

/// Render the model-facing notice for a subagent that was spawned in the
/// background and is still running.
///
/// When `continue_parent_work` is true, append the continue-parent CTA.
pub fn format_subagent_started_background(
    subagent_id: &str,
    subagent_type: &str,
    description: &str,
    naming: &BackgroundNoticeNaming,
    continue_parent_work: bool,
) -> String {
    let result_line = background_result_line(subagent_id, naming);
    let mut text = format!(
        "Subagent started in background.\n\
         subagent_id: {subagent_id}\n\
         type: {subagent_type}\n\
         description: {description}\n\n\
         {result_line}"
    );
    if continue_parent_work {
        text.push_str("\n\n");
        text.push_str(BACKGROUND_SUBAGENT_CONTINUE_PARENT_WORK);
    }
    text
}

/// Render the notice for a blocking spawn whose foreground wait budget
/// expired and was auto-backgrounded by the coordinator.
///
/// `notified_on_completion` gates the notification promise: only clients that
/// deliver system reminders actually wake the model when the child finishes.
pub fn format_subagent_auto_backgrounded(
    subagent_id: &str,
    subagent_type: &str,
    description: &str,
    naming: &BackgroundNoticeNaming,
    notified_on_completion: bool,
    continue_parent_work: bool,
) -> String {
    let notify_clause = if notified_on_completion {
        " — you will be notified when it completes"
    } else {
        ""
    };
    let result_line = background_result_line(subagent_id, naming);
    let mut text = format!(
        "Subagent took longer than the foreground budget and was moved to the \
         background to keep the conversation responsive. It is still running{notify_clause}.\n\
         subagent_id: {subagent_id}\n\
         type: {subagent_type}\n\
         description: {description}\n\n\
         {result_line}"
    );
    if continue_parent_work {
        text.push_str("\n\n");
        text.push_str(BACKGROUND_SUBAGENT_CONTINUE_PARENT_WORK);
    }
    text
}

/// Render the full model-facing completion block for a finished subagent:
/// the answer text, a `<subagent_meta>` line carrying run stats, and the
/// `<subagent_result>` resume footer.
pub fn format_subagent_completed(
    output: &str,
    subagent_id: &str,
    subagent_type: &str,
    tool_calls: u32,
    turns: u32,
    duration_ms: u64,
    persona: Option<&str>,
) -> String {
    let footer = format_resume_footer(subagent_id, subagent_type, persona);
    format!(
        "{output}\n\n<subagent_meta>id={subagent_id}, type={subagent_type}, \
         tool_calls={tool_calls}, turns={turns}, duration_ms={duration_ms}</subagent_meta>\n\n\
         {footer}"
    )
}

/// Render a resume footer from bare fields (when [`SubagentCompletedOutput`] is
/// not available, e.g. in the `get_task_output` path).
pub fn format_resume_footer(
    subagent_id: &str,
    subagent_type: &str,
    persona: Option<&str>,
) -> String {
    let mut footer = format!(
        "<subagent_result>\n\
         subagent_id: {subagent_id}\n\
         subagent_type: {subagent_type}\n\
         To continue this subagent's conversation, use resume_from=\"{subagent_id}\"."
    );
    if let Some(persona) = persona {
        footer.push_str(&format!(
            "\nThe subagent used persona=\"{persona}\". Pass the same persona when resuming."
        ));
    }
    footer.push_str("\n</subagent_result>");
    footer
}

/// Maximum number of task IDs accepted by a single multi-id `get_task_output`
/// (or legacy `wait_tasks`) call. Shared by the tool schema, the server-side
/// fan-out, and the toolbox wait path so the cap cannot drift.
pub const MAX_MULTI_WAIT_IDS: usize = 20;

/// Input for the `get_task_output` tool.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct TaskOutputToolInput {
    /// Task IDs to query. Pass one or more; a single task is a one-element list.
    ///
    /// Lenient on the wire (invisible to the advertised schema — schemars
    /// ignores serde aliases and custom deserializers): also accepts the
    /// singular `task_id` key and a bare string/number instead of an array.
    /// Models frequently mirror `kill_task`'s singular `task_id` here (in
    /// soak rollouts 3 of 4 organic calls did) and previously hard-failed
    /// with "Provide a non-empty task_ids list", after which they abandoned
    /// the background-task workflow for shell polling.
    #[schemars(
        description = "Task IDs to get output from. Pass one or more; for a single task use a one-element array. With a positive timeout_ms, multiple ids wait until all complete. Omit timeout_ms or pass 0 for a non-blocking snapshot."
    )]
    #[serde(
        default,
        alias = "task_id",
        deserialize_with = "crate::serde_lenient::deserialize_lenient_string_list"
    )]
    pub task_ids: Vec<String>,

    /// When set and positive, wait up to this many milliseconds; omit or `0` polls.
    ///
    /// `{max_wait_ms}` is resolved at finalize from the session's wait ceiling,
    /// which also pins it as the schema `maximum` — the tool description cannot
    /// carry the bound alone, since randomization may replace it wholesale.
    #[schemars(
        description = "Max wait time in milliseconds, up to {max_wait_ms}. A positive value waits for completion; omit or pass 0 for a non-blocking status poll."
    )]
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Trimmed, de-duplicated task IDs preserving first-seen order. Single source
/// of truth for how `task_ids` args resolve — used by tool execution and by
/// the tool-usage-card mapping so displayed IDs match what actually runs.
pub fn resolve_task_ids(ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();
    for id in ids {
        let id = id.trim();
        if !id.is_empty() && seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    }
    out
}

impl TaskOutputToolInput {
    /// Resolved, de-duplicated task IDs preserving first-seen order.
    pub fn resolved_task_ids(&self) -> Vec<String> {
        resolve_task_ids(&self.task_ids)
    }

    /// True only when `timeout_ms` is set and greater than zero.
    pub fn waits(&self) -> bool {
        task_output_waits(self.timeout_ms)
    }
}

/// Whether `get_task_output` should wait, from optional `timeout_ms`.
///
/// Positive `timeout_ms` waits; omit or `0` polls without blocking.
pub fn task_output_waits(timeout_ms: Option<u64>) -> bool {
    timeout_ms.is_some_and(|ms| ms > 0)
}

/// Default ceiling on a single blocking wait (`get_task_output` with a positive
/// `timeout_ms`, `wait_tasks`). Capping is safe because a completed task pings
/// the model, so a truncated wait costs one more poll, not the result.
pub const MAX_WAIT_BLOCK_MS_DEFAULT: u64 = 600_000;

/// The blocking-wait ceiling in effect, honoring `ECHO_AGENT_MAX_WAIT_BLOCK_MS`.
///
/// A host whose transport deadline is shorter than the default sets the env var
/// so the server enforces — and the tool descriptions advertise — the same
/// number the caller will actually wait for. Without that, a model believing the
/// default asks for a wait its own client will abandon first.
pub fn max_wait_block_ms() -> u64 {
    std::env::var("ECHO_AGENT_MAX_WAIT_BLOCK_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(MAX_WAIT_BLOCK_MS_DEFAULT)
}

/// Render a wait ceiling for tool descriptions, e.g. `600000 (~10 min)`.
///
/// The unit is derived from the value, so it cannot drift from the millisecond
/// figure beside it. Both branches round *down*: a cap must never read as
/// longer than it is.
pub fn format_wait_cap_ms(ms: u64) -> String {
    if ms < 60_000 {
        format!("{ms} (~{} s)", ms / 1_000)
    } else {
        format!("{ms} (~{} min)", ms / 60_000)
    }
}

/// Placeholder the description builders emit for the wait ceiling.
///
/// Resolved per session by `TruncationConfig::interpolate_description` in the
/// finalize loop, the same way `{max_lines_read}` is: the cap is client
/// configurable, so it cannot be baked in when the description is built.
pub const MAX_WAIT_MS_PLACEHOLDER: &str = "{max_wait_ms}";

/// Same as [`task_output_waits`], from raw tool-arg JSON (fingerprint / doom-loop).
pub fn task_output_waits_from_json(args: &serde_json::Value) -> bool {
    let timeout_ms = args.get("timeout_ms").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
    });
    task_output_waits(timeout_ms)
}

/// Output from the `get_task_output` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum TaskOutputOutput {
    Result(TaskOutputResult),
    TaskNotFound(String),
    MultiResult(MultiTaskOutputResult),
}

/// Successful result from the `get_task_output` tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TaskOutputResult {
    pub task_id: String,
    pub command: String,
    pub status: String,
    pub exit_code: Option<i32>,
    /// Wall-clock start time (ISO 8601 format)
    pub started: String,
    /// Wall-clock end time if completed (ISO 8601 format)
    pub ended: Option<String>,
    /// Duration in seconds
    pub duration_secs: f64,
    pub output: String,
    pub output_file: String,
    pub truncated: bool,
    /// Pre-resolved hint text for truncated output.
    /// Built by the tool's run() using resolved tool names.
    #[serde(default)]
    pub truncation_hint: String,
    /// Raw output byte count before any truncation or soft-wrapping.
    ///
    /// When `truncated` is true the `output` field only contains a short
    /// preview; the formatted string length stays roughly constant even as
    /// the underlying task output grows.  This field always reflects the
    /// actual task output size and is therefore used by doom-loop polling-
    /// progress detection to distinguish genuine output growth from
    /// stagnation.
    #[serde(default)]
    pub raw_output_bytes: usize,
}

impl TaskOutputOutput {
    pub fn is_terminal(&self) -> bool {
        match self {
            Self::Result(r) => r.is_terminal(),
            Self::TaskNotFound(_) => false,
            Self::MultiResult(_) => true,
        }
    }
}

/// Result from a multi-wait `get_task_output` / `wait_tasks` call.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MultiTaskOutputResult {
    pub mode: String,
    pub results: Vec<TaskOutputResult>,
    pub summary: String,
}

impl TaskOutputResult {
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "failed" | "cancelled")
    }

    /// Compute a progress signature from the semantically meaningful output
    /// fields of a `get_task_output` result.
    ///
    /// Two results with the same signature are considered stagnant — the task
    /// state has not changed between polls.  Used by the doom-loop detector to
    /// distinguish legitimate waiting (progress) from a true polling stall.
    ///
    /// Included fields: `status`, `exit_code`, `ended` (presence), and
    /// `raw_output_bytes`.
    ///
    /// `raw_output_bytes` is used instead of `output.len()` because when the
    /// output is truncated the formatted `output` string stays roughly the
    /// same length even as the underlying task output grows.  `raw_output_bytes`
    /// always reflects the true task output size, so it correctly detects
    /// progress for long-running tasks whose output exceeds the truncation limit.
    pub fn progress_signature(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        self.status.hash(&mut hasher);
        self.exit_code.hash(&mut hasher);
        self.ended.is_some().hash(&mut hasher);
        self.raw_output_bytes.hash(&mut hasher);
        hasher.finish()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// `wait_tasks` tool — Input
// ───────────────────────────────────────────────────────────────────────────

/// How a multi-wait (`wait_tasks`) request should resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitMode {
    WaitAny,
    WaitAll,
}

/// Input for the `wait_tasks` tool — blocks until multiple background tasks /
/// sub-agents reach a terminal state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitTasksToolInput {
    #[schemars(description = "Task IDs to wait for")]
    pub task_ids: Vec<String>,

    #[schemars(
        description = "Wait mode: 'wait_any' (return when first completes) or 'wait_all' (wait for all)"
    )]
    pub mode: WaitMode,

    /// Carries the same `{max_wait_ms}` marker as `TaskOutputToolInput`: this
    /// tool blocks on the same ceiling, so it needs the same resolved bound.
    #[schemars(description = "Max wait time in milliseconds, up to {max_wait_ms}")]
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

// ───────────────────────────────────────────────────────────────────────────
// `kill_task` (cancel) tool — Input / Output
// ───────────────────────────────────────────────────────────────────────────

/// Input for the `kill_task` tool — terminates a running background task,
/// monitor, or subagent by id.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KillTaskToolInput {
    #[schemars(description = "The task ID to terminate")]
    pub task_id: String,
}

/// Output from the `kill_task` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum KillTaskOutput {
    Result(KillTaskResult),
    TaskNotFound(String),
}

/// Successful result from the `kill_task` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KillTaskResult {
    pub task_id: String,
    /// `"killed"` or `"already_exited"`.
    pub outcome: String,
    pub message: String,
}

impl KillTaskOutput {
    pub fn was_killed(&self) -> bool {
        matches!(self, Self::Result(r) if r.outcome == "killed")
    }
}

/// One entry in the dynamic `task` tool description: a subagent type the model
/// may launch, with a human-readable summary and an optional fragment listing
/// the tools that agent can use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentDescriptor {
    /// `subagent_type` value the model passes to the `task` tool.
    pub name: String,
    /// One-line summary of what this subagent does.
    pub description: String,
    /// Optional fragment summarizing the tools the subagent can use. Appended
    /// verbatim after the description; may itself contain product-specific
    /// template variables (e.g. the CLI's `${{ tools.by_kind.* }}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<String>,
}

/// A built-in subagent type shared by the CLI (`echo-agent-core`) and other
/// agent hosts: its `subagent_type` name, canonical model-facing description,
/// tool-access fragment, and type-specific prompt body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinSubagent {
    /// `subagent_type` value the model passes to the `task` tool.
    pub name: &'static str,
    /// Canonical one-paragraph "when to use" description.
    pub description: &'static str,
    /// Tool-access fragment with `${{ tools.by_kind.* }}` placeholders.
    pub tools_template: &'static str,
    /// Type-specific prompt body injected into the child agent's context.
    pub prompt_template: &'static str,
}

impl BuiltinSubagent {
    /// Render the tool-access fragment, substituting each `${{ tools.by_kind.* }}`
    /// placeholder with the matching name from `naming`. Kinds the naming doesn't
    /// cover fall back to the bare kind name, so a partial map still renders.
    pub fn render_tools(&self, naming: &SubagentToolNaming) -> String {
        substitute_tool_placeholders(self.tools_template, |kind| {
            naming.tool_for_kind(kind).map(str::to_owned)
        })
    }

    /// Build a [`SubagentDescriptor`], rendering the tool-access fragment via
    /// [`Self::render_tools`] with the supplied `naming`.
    pub fn to_descriptor(&self, naming: &SubagentToolNaming) -> SubagentDescriptor {
        SubagentDescriptor {
            name: self.name.to_owned(),
            description: self.description.to_owned(),
            tools: Some(self.render_tools(naming)),
        }
    }

    /// Render the type-specific prompt body ([`Self::prompt_template`]),
    /// resolving `${{ tools.by_kind.<kind> }}` placeholders against
    /// `tool_by_kind`. Kinds absent from the map render as empty (and
    /// `${%- if %}` guards hide their sections), matching the CLI renderer's
    /// behavior.
    #[cfg(feature = "prompt-render")]
    pub fn render_prompt(
        &self,
        tool_by_kind: &std::collections::BTreeMap<String, String>,
    ) -> Option<String> {
        let syntax = minijinja::syntax::SyntaxConfig::builder()
            .block_delimiters("${%", "%}")
            .variable_delimiters("${{", "}}")
            .comment_delimiters("${#", "#}")
            .build()
            .ok()?;
        let mut env = minijinja::Environment::new();
        env.set_syntax(syntax);
        let ctx = minijinja::context! {
            tools => minijinja::context! { by_kind => tool_by_kind },
        };
        env.render_str(self.prompt_template, ctx).ok()
    }
}

/// The real tool names (by kind) substituted into the built-in subagents'
/// tool-access fragments (`tools_template`) when building the `task` description.
#[derive(Clone, Copy, Debug)]
pub struct SubagentToolNaming<'a> {
    /// Command-execution tool (kind `execute`).
    pub execute: &'a str,
    /// File-read tool (kind `read`).
    pub read: &'a str,
    /// File-edit tool (kind `edit`).
    pub edit: &'a str,
    /// Directory-listing tool (kind `list`).
    pub list: &'a str,
    /// Code-search tool (kind `search`).
    pub search: &'a str,
    /// Web-search tool (kind `web_search`).
    pub web_search: &'a str,
    /// Planning tool (kind `plan`).
    pub plan: &'a str,
}

impl SubagentToolNaming<'_> {
    /// The tool name for a `${{ tools.by_kind.<kind> }}` placeholder, or `None`
    /// for a kind this naming doesn't cover (the renderer then falls back to the
    /// bare kind name).
    fn tool_for_kind(&self, kind: &str) -> Option<&str> {
        Some(match kind {
            "execute" => self.execute,
            "read" => self.read,
            "edit" => self.edit,
            "list" => self.list,
            "search" => self.search,
            "web_search" => self.web_search,
            "plan" => self.plan,
            _ => return None,
        })
    }
}

/// Rewrite `${{ tools.by_kind.NAME }}` placeholders in a tool-access fragment,
/// substituting each with `resolve(NAME)` (the last dotted segment) or the bare
/// `NAME` when `resolve` returns `None`. Non-placeholder text is left intact and
/// an unclosed `${{` is emitted as-is.
fn substitute_tool_placeholders(
    template: &str,
    resolve: impl Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        match after.find("}}") {
            Some(end) => {
                let expr = after[..end].trim();
                let kind = expr.rsplit('.').next().unwrap_or(expr).trim();
                match resolve(kind) {
                    Some(name) => out.push_str(&name),
                    None => out.push_str(kind),
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("${{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Prompt body for the **general-purpose** subagent.
///
/// This agent has access to all tools and is used for complex search,
/// code exploration, and multi-step research tasks.
pub const GENERAL_PURPOSE_PROMPT: &str = "\
Complete the assigned task directly. Do what was asked; nothing more, nothing less. \
Report results in the format and length the task specifies; otherwise give a clear, \
complete writeup.

Strengths:
- Searching across workspace files and reference materials
- Multi-file analysis and information synthesis
- Multi-step research requiring exploration of many files

Guidelines:\
${%- if tools.by_kind.search and tools.by_kind.list %}
- Use ${{ tools.by_kind.search }} or ${{ tools.by_kind.list }} for broad searches; ${{ tools.by_kind.read }} for known paths.\
${%- endif %}
- Start broad and narrow down. Try multiple search strategies.
- Be thorough: check multiple locations, consider different naming conventions.\
${%- if tools.by_kind.edit %}
- NEVER create files unless absolutely necessary. Prefer editing existing files.
- NEVER create documentation files (*.md) unless explicitly requested.\
${%- endif %}
- Return absolute file paths and relevant source excerpts in your final response.

Workspace boundary:
- Default scope is the workspace in <user_info>. Stay within it unless told otherwise.
- Do not run whole-filesystem searches unless the user clearly requires it.";

/// Prompt body for the **explore** subagent.
///
/// A fast, read-only agent specialized for workspace research.
pub const EXPLORE_PROMPT: &str = "\
You are a fast, read-only workspace research agent.

=== READ-ONLY MODE ===
\
You have NO file editing tools. Do not create, modify, or delete files.\
${%- if tools.by_kind.execute %} \
Use ${{ tools.by_kind.execute }} only for read-only commands \
(ls, git status, git log, git diff, find, cat, head, tail).\
${%- endif %}

Strengths:
- Rapidly finding files using glob patterns
- Searching code with regex patterns
- Reading and analyzing file contents

Guidelines:
- Use ${{ tools.by_kind.list }} for file pattern matching, ${{ tools.by_kind.search }} for content search, ${{ tools.by_kind.read }} for known paths.
- Adapt search approach based on the thoroughness level specified by the caller.
- Return absolute file paths in your final response.
- Maximize parallel tool calls for speed.

Workspace boundary:
- Your default search scope is the workspace in <user_info>. Do not search outside it unless asked.
- If not found in the workspace, report that rather than broadening scope.";

/// Prompt body for the **plan** subagent.
///
/// A read-only architect agent that explores the codebase and produces
/// implementation plans.
pub const PLAN_PROMPT: &str = "\
You are a read-only planning specialist. Review relevant materials and develop actionable plans.

=== READ-ONLY MODE ===
\
You have NO file editing tools. Do not create, modify, or delete files.\
${%- if tools.by_kind.execute %} \
Use ${{ tools.by_kind.execute }} only for read-only commands \
(ls, git status, git log, git diff, find, cat, head, tail).\
${%- endif %}

Process:
1. **Understand** the requirements and any assigned perspective.
2. **Explore**: read provided files, find patterns with ${{ tools.by_kind.list }}/${{ tools.by_kind.search }}/${{ tools.by_kind.read }}, trace relevant code paths.
3. **Design**: consider trade-offs, follow existing patterns, create implementation approach.
4. **Detail**: step-by-step strategy, dependencies, sequencing, potential challenges.

## Required Output

End your response with:

### Relevant Materials for Execution
List 3-5 files most critical for implementing this plan:
- path/to/file1 - [Brief reason: e.g., \"Core logic to modify\"]
- path/to/file2 - [Brief reason: e.g., \"Interfaces to implement\"]
- path/to/file3 - [Brief reason: e.g., \"Pattern to follow\"]

Workspace boundary:
- Your default analysis scope is the workspace in <user_info>. Stay within it unless asked otherwise.
- Note explicitly if the design requires understanding external dependencies.";

/// The **general-purpose** built-in subagent.
pub const GENERAL_PURPOSE_SUBAGENT: BuiltinSubagent = BuiltinSubagent {
    name: "general-purpose",
    description: "General purpose agent for multi-step tasks.",
    tools_template: "Has access to: \
         ${{ tools.by_kind.execute }}, ${{ tools.by_kind.read }}, ${{ tools.by_kind.edit }}, \
         ${{ tools.by_kind.list }}, ${{ tools.by_kind.search }}, ${{ tools.by_kind.web_search }}, \
         and ${{ tools.by_kind.plan }}.",
    prompt_template: GENERAL_PURPOSE_PROMPT,
};

/// The **explore** built-in subagent.
pub const EXPLORE_SUBAGENT: BuiltinSubagent = BuiltinSubagent {
    name: "explore",
    description: "Fast, read-only agent specialized for workspace research.",
    tools_template: "Read-only \u{2014} has access to: \
         ${{ tools.by_kind.read }}, ${{ tools.by_kind.list }}, \
         ${{ tools.by_kind.search }}.",
    prompt_template: EXPLORE_PROMPT,
};

/// The **plan** built-in subagent.
pub const PLAN_SUBAGENT: BuiltinSubagent = BuiltinSubagent {
    name: "plan",
    description: "Software architect for planning implementation strategies.",
    tools_template: "Read-only \u{2014} has access to: \
         ${{ tools.by_kind.read }}, ${{ tools.by_kind.list }}, ${{ tools.by_kind.search }}, \
         ${{ tools.by_kind.web_search }}, and ${{ tools.by_kind.plan }}. \
         File editing and command execution are not available.",
    prompt_template: PLAN_PROMPT,
};

/// The built-in subagent types advertised to the model, in display order.
pub const BUILTIN_SUBAGENTS: [BuiltinSubagent; 3] =
    [GENERAL_PURPOSE_SUBAGENT, EXPLORE_SUBAGENT, PLAN_SUBAGENT];

/// Look up a built-in subagent by its `subagent_type` name
/// (e.g. `"explore"`), or `None` for user-defined / unknown types.
pub fn builtin_subagent_by_name(name: &str) -> Option<&'static BuiltinSubagent> {
    BUILTIN_SUBAGENTS.iter().find(|b| b.name == name)
}

/// Tool/parameter names (and optional features) that vary between products when
/// rendering the shared `task` tool description.
#[derive(Clone, Copy, Debug)]
pub struct TaskToolNaming<'a> {
    /// Name of the spawn tool (canonical: `task`).
    pub task_tool: &'a str,
    /// Name of the `subagent_type` parameter.
    pub subagent_type_param: &'a str,
    /// Name of the `run_in_background` parameter.
    pub run_in_background_param: &'a str,
    /// Name of the `resume_from` parameter.
    pub resume_from_param: &'a str,
    /// Name of the task result retrieval tool.
    pub background_retrieval_tool: &'a str,
    /// Name of the `isolation` parameter, used in the isolation/worktree
    /// paragraph.
    pub isolation_param: &'a str,
}

/// Build the `task` tool description from an effective subagent list.
///
/// Assembles the canonical header, the agent roster, and the usage-notes
/// footer, substituting the product-specific tool/parameter names from
/// `naming`. Agent lines render as `- **{name}**: {description} {tools}` (the
/// trailing tools fragment is omitted when [`SubagentDescriptor::tools`] is
/// `None`, e.g. for user-defined agents).
pub fn build_task_description(subagents: &[SubagentDescriptor], naming: &TaskToolNaming) -> String {
    let agent_lines = subagents
        .iter()
        .map(|s| match &s.tools {
            Some(tools) => format!("- **{}**: {} {}", s.name, s.description, tools),
            None => format!("- **{}**: {}", s.name, s.description),
        })
        .collect::<Vec<_>>()
        .join("\n");

    let TaskToolNaming {
        task_tool,
        subagent_type_param,
        run_in_background_param,
        resume_from_param,
        background_retrieval_tool,
        ..
    } = *naming;

    #[allow(unused_mut)]
    let mut out = format!(
        "Start a subagent that works on a task independently and reports back.\n\n\
         Agent types:\n\n\
         {agent_lines}\n\n\
         ## Usage notes\n\
         - When the agent is done, it returns a single message with its agent ID. Use that ID to resume the agent later for follow-up work.\n\
         - {run_in_background_param}: Returns immediately with a subagent_id. Use {background_retrieval_tool} to retrieve results. This is set to true by default.\n\
         - Subagents receive a compacted version of project instructions (AGENTS.md). If the task requires detailed conventions (e.g., build rules, testing patterns), include the relevant rules directly in the prompt.\n\
         - When using the {task_tool} tool, you must specify a {subagent_type_param} parameter to select which agent type to use.\n\
         - When launching independent subagents, you MUST incorporate the results into the task based on requirements BEFORE concluding.\n\n\
         Resuming a previous agent (resume_from):\n\
         - Use {resume_from_param} to continue a previously completed subagent's conversation. Pass the subagent_id returned by a prior {task_tool} call. A resumed agent keeps its full transcript and tool state, so you only need to describe what changed since the last run — don't re-explain the original task.\n\
         - The resumed agent must use the same subagent_type as the source."
    );

    #[cfg(not(feature = "desktop"))]
    {
        use std::fmt::Write;
        let _ = write!(
            out,
            "\n\nIsolation mode:\n- Use {} to control the child's execution environment. With \"worktree\", the child runs in an isolated git worktree whose edits don't affect the parent workspace; the worktree is preserved after completion and its path is returned in the output.",
            naming.isolation_param,
        );
    }
    out
}

/// Shared `background task or subagent`-style target suffix used by the
/// `kill_task` / `get_task_output` opening line.
fn lifecycle_target_suffix(monitor_present: bool, subagent_present: bool) -> &'static str {
    match (monitor_present, subagent_present) {
        (true, true) => ", monitor, or subagent",
        (true, false) => " or monitor",
        (false, true) => " or subagent",
        (false, false) => "",
    }
}

/// Optional "(a monitor's {id_name} is returned by {monitor})" clause.
///
/// `id_name` is the model-facing singular id name — kill_task's `task_id`
/// input (tracks renames). get_task_output's `task_ids` array is plural and
/// must not be used here; both tools share this wording so randomization
/// cannot disagree across kill vs get-output docs.
fn monitor_task_id_note(monitor_tool: Option<&str>, id_name: &str) -> String {
    match monitor_tool {
        Some(m) => format!(" (a monitor's {id_name} is returned by {m})"),
        None => String::new(),
    }
}

/// Naming/feature inputs for [`build_kill_task_description`].
#[derive(Clone, Copy, Debug)]
pub struct KillTaskToolNaming<'a> {
    /// Monitor tool name, or `None` when no monitor tool is present.
    pub monitor_tool: Option<&'a str>,
    /// Whether a `task`/subagent tool is present (adds subagent wording).
    pub subagent_present: bool,
    /// Whether a bash/`execute` tool is present (adds the bash-task wording).
    pub bash_present: bool,
    /// Whether termination uses a Windows Job Object (vs POSIX signals).
    pub is_windows: bool,
    /// Model-facing name of the `task_id` input (tracks param renames).
    pub task_id_param: &'a str,
}

/// Build the shared `kill_task` tool description.
pub fn build_kill_task_description(naming: &KillTaskToolNaming) -> String {
    let KillTaskToolNaming {
        monitor_tool,
        subagent_present,
        bash_present,
        is_windows,
        task_id_param,
    } = *naming;
    let monitor_present = monitor_tool.is_some();

    let target_suffix = lifecycle_target_suffix(monitor_present, subagent_present);
    let monitor_note = monitor_task_id_note(monitor_tool, task_id_param);

    let verb = if is_windows {
        "Terminates the Job Object of"
    } else {
        "Sends SIGTERM/SIGKILL to"
    };
    let action = if bash_present {
        let mut s = format!("{verb} a bash task");
        if monitor_present {
            s.push_str(" or monitor");
        }
        if subagent_present {
            s.push_str("; sends Cancel+Shutdown to a subagent");
        }
        s
    } else if subagent_present {
        "Sends Cancel+Shutdown to a subagent".to_string()
    } else if monitor_present {
        format!("{verb} a monitor")
    } else {
        String::new()
    };

    format!(
        "Terminate a running background task{target_suffix}.\n\n\
         Usage notes:\n\
         - Pass its {task_id_param}{monitor_note}.\n\
         - {action}.\n\
         - Returns success if the task was killed or had already exited."
    )
}

/// Naming/feature inputs for [`build_task_output_description`] (`get_task_output`).
#[derive(Clone, Copy, Debug)]
pub struct TaskOutputToolNaming<'a> {
    /// Monitor tool name, or `None` when no monitor tool is present.
    pub monitor_tool: Option<&'a str>,
    /// Read tool name for the "large output" hint, or `None`.
    pub read_tool: Option<&'a str>,
    /// The bash `is_background` param name, when a bash/`execute` tool is present.
    pub bash_background_param: Option<&'a str>,
    /// The subagent `run_in_background` param name, when a `task` tool is present.
    pub subagent_background_param: Option<&'a str>,
    /// Model-facing name of the `task_ids` input (tracks param renames).
    pub task_ids_param: &'a str,
    /// Model-facing name of the `timeout_ms` input (tracks param renames).
    pub timeout_ms_param: &'a str,
    /// Singular monitor-id name for the monitor aside — kill_task's `task_id`
    /// (tracks renames). Not get_task_output's plural `task_ids`.
    pub task_id_param: &'a str,
}

/// Build the shared `get_task_output` tool description.
pub fn build_task_output_description(naming: &TaskOutputToolNaming) -> String {
    let TaskOutputToolNaming {
        monitor_tool,
        read_tool,
        bash_background_param,
        subagent_background_param,
        task_ids_param,
        timeout_ms_param,
        task_id_param,
    } = *naming;
    let monitor_present = monitor_tool.is_some();
    let subagent_present = subagent_background_param.is_some();

    let target_suffix = lifecycle_target_suffix(monitor_present, subagent_present);

    let sources = match (bash_background_param, subagent_background_param) {
        // Both params share one client-facing name: don't repeat it.
        (Some(b), Some(s)) if b == s => format!("{b}=true commands or subagents"),
        (Some(b), Some(s)) => format!("{b}=true commands or {s}=true subagents"),
        (Some(b), None) => format!("{b}=true commands"),
        (None, Some(s)) => format!("{s}=true subagents"),
        (None, None) => "background tasks".to_string(),
    };

    let monitor_note = monitor_task_id_note(monitor_tool, task_id_param);
    let read_note = match read_tool {
        Some(r) => format!("\n- If output is large, use {r} on the output_file path"),
        None => String::new(),
    };
    let wait_cap = MAX_WAIT_MS_PLACEHOLDER;

    format!(
        "Get output and status from a background task{target_suffix}.\n\n\
         Usage notes:\n\
         - Pass {task_ids_param} with one or more ids from {sources}{monitor_note}; for a single task use a one-element array. Multiple ids with a positive {timeout_ms_param} wait until all complete\n\
         - Omit {timeout_ms_param} or pass 0 for a non-blocking status snapshot; set a positive {timeout_ms_param} to wait up to that many milliseconds, capped at {wait_cap}\n\
         - Returns current output, status, and exit code if completed{read_note}"
    )
}

/// Naming/feature inputs for [`build_wait_tasks_description`].
#[derive(Clone, Copy, Debug)]
pub struct WaitTasksToolNaming<'a> {
    /// The preferred retrieval tool name shown in the "Prefer …" line.
    pub background_retrieval_tool: &'a str,
    /// The bash `is_background` param name, when a bash/`execute` tool is present.
    pub bash_background_param: Option<&'a str>,
    /// The subagent `run_in_background` param name, when a `task` tool is present.
    pub subagent_background_param: Option<&'a str>,
}

/// Build the shared `wait_tasks` tool description.
pub fn build_wait_tasks_description(naming: &WaitTasksToolNaming) -> String {
    let WaitTasksToolNaming {
        background_retrieval_tool,
        bash_background_param,
        subagent_background_param,
    } = *naming;

    let sources = match (bash_background_param, subagent_background_param) {
        // Both params share one client-facing name: don't repeat it.
        (Some(b), Some(s)) if b == s => format!("{b}=true commands or subagents"),
        (Some(b), Some(s)) => format!("{b}=true commands or {s}=true subagents"),
        (Some(b), None) => format!("{b}=true commands"),
        (None, Some(s)) => format!("{s}=true subagents"),
        (None, None) => "background tasks".to_string(),
    };

    let wait_cap = MAX_WAIT_MS_PLACEHOLDER;

    format!(
        "Wait for multiple background tasks or subagents to complete.\n\n\
         Prefer {background_retrieval_tool} with task_ids and a positive timeout_ms. This tool is kept for compatibility.\n\n\
         Usage notes:\n\
         - task_ids: list of task IDs from {sources}\n\
         - mode: 'wait_all' or 'wait_any'\n\
         - timeout_ms: optional max wait, default 30s, capped at {wait_cap}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_with_status(status: &str) -> TaskOutputOutput {
        TaskOutputOutput::Result(TaskOutputResult {
            task_id: "t".into(),
            status: status.into(),
            ..Default::default()
        })
    }

    #[test]
    fn is_terminal_by_status() {
        assert!(!result_with_status("running").is_terminal());
        assert!(!result_with_status("pending").is_terminal());
        assert!(!result_with_status("initializing").is_terminal());
        assert!(!result_with_status("").is_terminal());

        assert!(result_with_status("completed").is_terminal());
        assert!(result_with_status("failed").is_terminal());
        assert!(result_with_status("cancelled").is_terminal());
    }

    #[test]
    fn is_terminal_not_found_and_multi() {
        assert!(!TaskOutputOutput::TaskNotFound("x".into()).is_terminal());
        assert!(
            TaskOutputOutput::MultiResult(MultiTaskOutputResult {
                mode: "wait_all".into(),
                results: vec![],
                summary: String::new(),
            })
            .is_terminal()
        );
    }

    fn literal_naming() -> TaskToolNaming<'static> {
        TaskToolNaming {
            task_tool: "task",
            subagent_type_param: "subagent_type",
            run_in_background_param: "run_in_background",
            resume_from_param: "resume_from",
            background_retrieval_tool: "get_task_output",
            isolation_param: "isolation",
        }
    }

    #[test]
    fn task_tool_input_defaults_background_true() {
        let input: TaskToolInput =
            serde_json::from_str(r#"{"description": "test", "prompt": "do it"}"#).unwrap();
        assert_eq!(input.subagent_type, "general-purpose");
        assert!(
            input.run_in_background,
            "run_in_background should default to true"
        );

        let foreground: TaskToolInput = serde_json::from_str(
            r#"{"description": "test", "prompt": "do it", "run_in_background": false}"#,
        )
        .unwrap();
        assert!(!foreground.run_in_background);
    }

    #[test]
    fn task_tool_input_model_omitted_is_none() {
        let input: TaskToolInput =
            serde_json::from_str(r#"{"description": "d", "prompt": "p"}"#).unwrap();
        assert!(input.model.is_none());
    }

    #[test]
    fn task_tool_input_model_parses_explicit() {
        let input: TaskToolInput =
            serde_json::from_str(r#"{"description": "d", "prompt": "p", "model": "echo-agent-3"}"#)
                .unwrap();
        assert_eq!(input.model.as_deref(), Some("echo-agent-3"));
    }

    #[test]
    fn task_tool_input_model_none_skips_serialize() {
        let input = TaskToolInput {
            prompt: "p".into(),
            description: "d".into(),
            subagent_type: default_subagent_type(),
            run_in_background: false,
            capability_mode: None,
            isolation: None,
            resume_from: None,
            cwd: None,
            model: None,
            task_id: None,
        };
        let value = serde_json::to_value(&input).unwrap();
        assert!(value.get("model").is_none());
        assert!(value.get("capability_mode").is_none());
    }

    #[test]
    fn task_tool_input_ignores_capability_mode_json() {
        let input: TaskToolInput = serde_json::from_str(
            r#"{"description":"d","prompt":"p","capability_mode":"read-only"}"#,
        )
        .unwrap();
        assert!(input.capability_mode.is_none());
    }

    #[test]
    fn task_tool_input_schema_omits_capability_mode() {
        let schema = serde_json::to_value(schemars::schema_for!(TaskToolInput)).unwrap();
        assert!(schema["properties"].get("capability_mode").is_none());
    }

    #[test]
    fn task_output_input_accepts_singular_task_id_alias() {
        // Canonical plural form (unchanged).
        let input: TaskOutputToolInput =
            serde_json::from_str(r#"{"task_ids": ["a", "b"]}"#).unwrap();
        assert_eq!(input.resolved_task_ids(), vec!["a", "b"]);

        // Singular key with a bare string — the shape models organically send
        // (mirroring kill_task's singular task_id).
        let input: TaskOutputToolInput =
            serde_json::from_str(r#"{"task_id": "abc-123", "timeout_ms": 0}"#).unwrap();
        assert_eq!(input.resolved_task_ids(), vec!["abc-123"]);
        assert_eq!(input.timeout_ms, Some(0));

        // Singular key with an array also works.
        let input: TaskOutputToolInput =
            serde_json::from_str(r#"{"task_id": ["x", "y"]}"#).unwrap();
        assert_eq!(input.resolved_task_ids(), vec!["x", "y"]);

        // Plural key with a bare string.
        let input: TaskOutputToolInput = serde_json::from_str(r#"{"task_ids": "solo"}"#).unwrap();
        assert_eq!(input.resolved_task_ids(), vec!["solo"]);

        // Bare number (observed: an OS PID) becomes a string id, so the tool
        // answers "Task 228 not found" instead of a deserialize error.
        let input: TaskOutputToolInput = serde_json::from_str(r#"{"task_id": 228}"#).unwrap();
        assert_eq!(input.resolved_task_ids(), vec!["228"]);
    }

    #[test]
    fn task_output_input_schema_does_not_advertise_the_alias() {
        // The leniency is wire-only: the advertised schema must keep exactly
        // the canonical properties (task_ids, timeout_ms) so tool-definition
        // dumps and param randomization are unaffected.
        let schema = serde_json::to_value(schemars::schema_for!(TaskOutputToolInput)).unwrap();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("task_ids"));
        assert!(props.contains_key("timeout_ms"));
        assert!(
            !props.contains_key("task_id"),
            "singular alias must not leak into the schema: {props:?}"
        );
        assert_eq!(props.len(), 2);
        // And task_ids stays a plain string array.
        assert_eq!(props["task_ids"]["type"], "array");
        assert_eq!(props["task_ids"]["items"]["type"], "string");
    }

    #[test]
    fn sanitize_optional_arg_moves_when_no_trim() {
        assert_eq!(
            sanitize_optional_arg(Some("echo-agent-3".into())).as_deref(),
            Some("echo-agent-3")
        );
        assert_eq!(
            sanitize_optional_arg(Some("  echo-agent-3  ".into())).as_deref(),
            Some("echo-agent-3")
        );
        assert!(sanitize_optional_arg(Some("null".into())).is_none());
        assert!(sanitize_optional_arg(Some("  NULL  ".into())).is_none());
        assert!(sanitize_optional_arg(None).is_none());
    }

    #[test]
    fn build_task_description_substitutes_names_and_lists_agents() {
        let subagents = vec![
            SubagentDescriptor {
                name: "general-purpose".into(),
                description: "General-purpose agent.".into(),
                tools: Some("Has access to all tools.".into()),
            },
            SubagentDescriptor {
                name: "code-reviewer".into(),
                description: "Reviews code.".into(),
                tools: None,
            },
        ];
        let desc = build_task_description(&subagents, &literal_naming());
        assert!(desc.starts_with("Start a subagent that works on a task independently"));
        assert!(desc.contains("Agent types:"));
        assert!(
            desc.contains("- **general-purpose**: General-purpose agent. Has access to all tools.")
        );
        // User-defined entries (tools = None) get no trailing fragment.
        assert!(desc.contains("- **code-reviewer**: Reviews code."));
        assert!(desc.contains("## Usage notes"));
        assert!(desc.contains(
            "run_in_background: Returns immediately with a subagent_id. Use get_task_output to retrieve results. This is set to true by default."
        ));
        assert!(desc.contains("you must specify a subagent_type parameter"));
        assert!(desc.contains(
            "When launching independent subagents, you MUST incorporate the results into the task based on requirements BEFORE concluding."
        ));
        assert!(
            !desc.contains("only actual task calls do")
                && !desc.contains("If the user explicitly asked for subagents"),
            "delegation timing belongs in the shared system prompt, not the task contract: {desc}"
        );
        assert!(desc.contains("Use resume_from to continue"));
    }

    #[test]
    fn build_task_description_includes_isolation_paragraph() {
        let subagents = vec![SubagentDescriptor {
            name: "explore".into(),
            description: "Explore.".into(),
            tools: None,
        }];

        let desc = build_task_description(
            &subagents,
            &TaskToolNaming {
                isolation_param: "isolation",
                ..literal_naming()
            },
        );
        assert!(desc.contains("Isolation mode:"));
        assert!(desc.contains("Use isolation to control the child's execution environment."));
    }

    #[test]
    fn builtin_subagent_catalog_names_and_descriptor_conversion() {
        assert_eq!(
            BUILTIN_SUBAGENTS.map(|b| b.name),
            ["general-purpose", "explore", "plan"]
        );

        let desc = EXPLORE_SUBAGENT.to_descriptor(&plain_tool_naming());
        assert_eq!(desc.name, "explore");
        assert_eq!(desc.description, EXPLORE_SUBAGENT.description);
        // A bare-kind naming reduces the CLI placeholders to bare tool-kind
        // names, staying aligned with the CLI's `tools_template`.
        assert_eq!(
            desc.tools.as_deref(),
            Some("Read-only \u{2014} has access to: read, list, search.")
        );

        // Descriptions must be single-spaced (line-continuation whitespace is
        // stripped), so they read as one clean paragraph in the tool listing.
        assert!(!GENERAL_PURPOSE_SUBAGENT.description.contains("  "));
    }

    #[test]
    fn builtin_subagent_by_name_finds_builtins_only() {
        assert_eq!(
            builtin_subagent_by_name("explore").map(|b| b.name),
            Some("explore")
        );
        assert_eq!(
            builtin_subagent_by_name("general-purpose").map(|b| b.prompt_template),
            Some(GENERAL_PURPOSE_PROMPT)
        );
        assert_eq!(
            builtin_subagent_by_name("plan").map(|b| b.prompt_template),
            Some(PLAN_PROMPT)
        );
        assert!(builtin_subagent_by_name("code-reviewer").is_none());
    }

    #[test]
    fn prompt_templates_reference_tools_via_by_kind_placeholders() {
        for b in &BUILTIN_SUBAGENTS {
            assert!(
                !b.prompt_template.is_empty(),
                "{} must have a prompt body",
                b.name
            );
            assert!(
                b.prompt_template.contains("${{ tools.by_kind."),
                "{} prompt must resolve tool names via placeholders",
                b.name
            );
        }
        // Read-only profiles carry the read-only banner; general-purpose doesn't.
        assert!(EXPLORE_PROMPT.contains("=== READ-ONLY MODE ==="));
        assert!(PLAN_PROMPT.contains("=== READ-ONLY MODE ==="));
        assert!(!GENERAL_PURPOSE_PROMPT.contains("READ-ONLY"));
    }

    #[cfg(feature = "prompt-render")]
    #[test]
    fn render_prompt_resolves_tools_and_conditionals() {
        let by_kind: std::collections::BTreeMap<String, String> = [
            ("execute", "run_terminal_cmd"),
            ("read", "read_file"),
            ("edit", "search_replace"),
            ("list", "list_dir"),
            ("search", "grep"),
            ("web_search", "web_search"),
            ("plan", "todo_write"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        for b in &BUILTIN_SUBAGENTS {
            let rendered = b
                .render_prompt(&by_kind)
                .unwrap_or_else(|| panic!("{} prompt must render", b.name));
            assert!(
                !rendered.contains("${{") && !rendered.contains("${%"),
                "{} prompt has unresolved placeholders:\n{rendered}",
                b.name
            );
        }

        let explore = EXPLORE_SUBAGENT.render_prompt(&by_kind).unwrap();
        assert!(explore.contains("Use run_terminal_cmd only for read-only commands"));
        assert!(explore.contains("Use list_dir for file pattern matching"));

        // Missing execute tool hides its `${%- if %}` section.
        let mut without_execute = by_kind.clone();
        without_execute.remove("execute");
        let plan = PLAN_SUBAGENT.render_prompt(&without_execute).unwrap();
        assert!(!plan.contains("only for read-only commands"));
        assert!(plan.contains("=== READ-ONLY MODE ==="));
    }

    /// A [`SubagentToolNaming`] whose fields are the bare kind names, so
    /// rendering reproduces the placeholder kinds verbatim.
    fn plain_tool_naming() -> SubagentToolNaming<'static> {
        SubagentToolNaming {
            execute: "execute",
            read: "read",
            edit: "edit",
            list: "list",
            search: "search",
            web_search: "web_search",
            plan: "plan",
        }
    }

    #[test]
    fn render_tools_substitutes_naming_with_bare_kind_fallback() {
        // Bare-kind naming reproduces the placeholder kinds verbatim.
        assert_eq!(
            GENERAL_PURPOSE_SUBAGENT.render_tools(&plain_tool_naming()),
            "Has access to: execute, read, edit, list, search, web_search, and plan."
        );
        assert_eq!(
            PLAN_SUBAGENT.render_tools(&plain_tool_naming()),
            "Read-only \u{2014} has access to: read, list, search, web_search, and plan. \
             File editing and command execution are not available."
        );

        // Real tool names are substituted per kind.
        let naming = SubagentToolNaming {
            execute: "run_terminal_cmd",
            read: "read_file",
            edit: "search_replace",
            list: "list_dir",
            search: "grep",
            web_search: "web_search",
            plan: "todo_write",
        };
        assert_eq!(
            EXPLORE_SUBAGENT.render_tools(&naming),
            "Read-only \u{2014} has access to: read_file, list_dir, grep."
        );
    }

    #[test]
    fn build_task_description_preserves_template_placeholders() {
        // The CLI passes `${{ ... }}` placeholders; the builder must emit them
        // verbatim for its downstream TemplateRenderer to resolve.
        let subagents = vec![SubagentDescriptor {
            name: "explore".into(),
            description: "Explore.".into(),
            tools: Some("Read-only — has access to: ${{ tools.by_kind.read }}.".into()),
        }];
        let desc = build_task_description(
            &subagents,
            &TaskToolNaming {
                task_tool: "${{ tools.by_kind.task }}",
                subagent_type_param: "${{ params.task.subagent_type }}",
                run_in_background_param: "${{ params.task.run_in_background }}",
                resume_from_param: "${{ params.task.resume_from }}",
                background_retrieval_tool: "${{ tools.by_kind.background_task_action }}",
                isolation_param: "${{ params.task.isolation }}",
            },
        );
        assert!(desc.contains("When using the ${{ tools.by_kind.task }} tool"));
        assert!(desc.contains("${{ tools.by_kind.read }}"));
        assert!(desc.contains(
            "${{ params.task.run_in_background }}: Returns immediately with a subagent_id. Use ${{ tools.by_kind.background_task_action }} to retrieve results. This is set to true by default."
        ));
        assert!(desc.contains("Use ${{ params.task.isolation }} to control"));
    }

    // ── Lifecycle tool descriptions ──────────────────────────────────────
    //
    // These lock the exact model-facing text. The "cli_default" cases must
    // match what the echo-agent-shell MiniJinja templates render for the default
    // echo-agent-build toolset (monitor + task + bash + read present, POSIX). The
    // "toolbox" cases lock the subagent-only rendering used by the backend toolbox.

    #[test]
    fn kill_task_matches_cli_default_posix() {
        let desc = build_kill_task_description(&KillTaskToolNaming {
            monitor_tool: Some("monitor"),
            subagent_present: true,
            bash_present: true,
            is_windows: false,
            task_id_param: "task_id",
        });
        assert_eq!(
            desc,
            "Terminate a running background task, monitor, or subagent.\n\n\
             Usage notes:\n\
             - Pass its task_id (a monitor's task_id is returned by monitor).\n\
             - Sends SIGTERM/SIGKILL to a bash task or monitor; sends Cancel+Shutdown to a subagent.\n\
             - Returns success if the task was killed or had already exited."
        );
    }

    #[test]
    fn kill_task_matches_cli_default_windows() {
        let desc = build_kill_task_description(&KillTaskToolNaming {
            monitor_tool: Some("monitor"),
            subagent_present: true,
            bash_present: true,
            is_windows: true,
            task_id_param: "task_id",
        });
        assert!(desc.contains(
            "- Terminates the Job Object of a bash task or monitor; sends Cancel+Shutdown to a subagent."
        ));
    }

    #[test]
    fn kill_task_subagent_only_toolbox() {
        let desc = build_kill_task_description(&KillTaskToolNaming {
            monitor_tool: None,
            subagent_present: true,
            bash_present: false,
            is_windows: false,
            task_id_param: "task_id",
        });
        assert_eq!(
            desc,
            "Terminate a running background task or subagent.\n\n\
             Usage notes:\n\
             - Pass its task_id.\n\
             - Sends Cancel+Shutdown to a subagent.\n\
             - Returns success if the task was killed or had already exited."
        );
    }

    #[test]
    fn kill_task_description_tracks_renamed_task_id() {
        let desc = build_kill_task_description(&KillTaskToolNaming {
            monitor_tool: Some("monitor"),
            subagent_present: false,
            bash_present: true,
            is_windows: false,
            task_id_param: "id",
        });
        assert!(
            desc.contains("Pass its id (a monitor's id is returned by monitor)"),
            "renamed task_id must appear in pass-line and monitor aside: {desc}"
        );
        assert!(
            !desc.contains("task_id"),
            "canonical task_id must not remain after rename: {desc}"
        );
    }

    #[test]
    fn format_wait_cap_ms_derives_its_unit_and_rounds_down() {
        assert_eq!(
            format_wait_cap_ms(MAX_WAIT_BLOCK_MS_DEFAULT),
            "600000 (~10 min)"
        );
        assert_eq!(format_wait_cap_ms(300_000), "300000 (~5 min)");
        // Rounds down: 1.5 min must not read as 2.
        assert_eq!(format_wait_cap_ms(90_000), "90000 (~1 min)");
        // Sub-minute caps switch unit rather than rendering "~0 min".
        assert_eq!(format_wait_cap_ms(30_000), "30000 (~30 s)");
    }

    #[test]
    fn task_output_description_tracks_renamed_params() {
        let desc = build_task_output_description(&TaskOutputToolNaming {
            monitor_tool: Some("monitor"),
            read_tool: None,
            bash_background_param: Some("is_background"),
            subagent_background_param: None,
            task_ids_param: "process_ids",
            timeout_ms_param: "max_wait",
            task_id_param: "id",
        });
        assert!(
            desc.contains("Pass process_ids with"),
            "renamed task_ids must appear: {desc}"
        );
        assert!(
            desc.contains("positive max_wait wait") && desc.contains("Omit max_wait or pass 0"),
            "renamed timeout_ms must appear: {desc}"
        );
        assert!(
            desc.contains("a monitor's id is returned by monitor"),
            "renamed kill_task task_id must appear in monitor aside: {desc}"
        );
        assert!(
            !desc.contains("task_ids") && !desc.contains("timeout_ms") && !desc.contains("task_id"),
            "canonical param names must not remain after rename: {desc}"
        );
    }

    #[test]
    fn task_output_matches_cli_default() {
        let desc = build_task_output_description(&TaskOutputToolNaming {
            monitor_tool: Some("monitor"),
            read_tool: Some("read_file"),
            bash_background_param: Some("background"),
            subagent_background_param: Some("background"),
            task_ids_param: "task_ids",
            timeout_ms_param: "timeout_ms",
            task_id_param: "task_id",
        });
        assert_eq!(
            desc,
            "Get output and status from a background task, monitor, or subagent.\n\n\
             Usage notes:\n\
             - Pass task_ids with one or more ids from background=true commands or subagents (a monitor's task_id is returned by monitor); for a single task use a one-element array. Multiple ids with a positive timeout_ms wait until all complete\n\
             - Omit timeout_ms or pass 0 for a non-blocking status snapshot; set a positive timeout_ms to wait up to that many milliseconds, capped at {max_wait_ms}\n\
             - Returns current output, status, and exit code if completed\n\
             - If output is large, use read_file on the output_file path"
        );
    }

    #[test]
    fn task_output_subagent_only_toolbox() {
        let desc = build_task_output_description(&TaskOutputToolNaming {
            monitor_tool: None,
            read_tool: Some("read_file"),
            bash_background_param: None,
            subagent_background_param: Some("run_in_background"),
            task_ids_param: "task_ids",
            timeout_ms_param: "timeout_ms",
            task_id_param: "task_id",
        });
        assert_eq!(
            desc,
            "Get output and status from a background task or subagent.\n\n\
             Usage notes:\n\
             - Pass task_ids with one or more ids from run_in_background=true subagents; for a single task use a one-element array. Multiple ids with a positive timeout_ms wait until all complete\n\
             - Omit timeout_ms or pass 0 for a non-blocking status snapshot; set a positive timeout_ms to wait up to that many milliseconds, capped at {max_wait_ms}\n\
             - Returns current output, status, and exit code if completed\n\
             - If output is large, use read_file on the output_file path"
        );
    }

    #[test]
    fn wait_tasks_matches_cli_default() {
        let desc = build_wait_tasks_description(&WaitTasksToolNaming {
            background_retrieval_tool: "get_command_or_subagent_output",
            bash_background_param: Some("background"),
            subagent_background_param: Some("background"),
        });
        assert_eq!(
            desc,
            "Wait for multiple background tasks or subagents to complete.\n\n\
             Prefer get_command_or_subagent_output with task_ids and a positive timeout_ms. This tool is kept for compatibility.\n\n\
             Usage notes:\n\
             - task_ids: list of task IDs from background=true commands or subagents\n\
             - mode: 'wait_all' or 'wait_any'\n\
             - timeout_ms: optional max wait, default 30s, capped at {max_wait_ms}"
        );
    }

    #[test]
    fn wait_tasks_subagent_only_toolbox() {
        let desc = build_wait_tasks_description(&WaitTasksToolNaming {
            background_retrieval_tool: "get_task_output",
            bash_background_param: None,
            subagent_background_param: Some("run_in_background"),
        });
        assert!(
            desc.contains("- task_ids: list of task IDs from run_in_background=true subagents\n")
        );
        assert!(desc.contains("Prefer get_task_output with task_ids"));
    }

    #[test]
    fn background_spawn_notice_keeps_poll_hint_and_open_parent_work() {
        let naming = BackgroundNoticeNaming {
            task_output_tool: "get_command_or_subagent_output",
            ..BackgroundNoticeNaming::CANONICAL
        };
        let with_cta = format_subagent_started_background(
            "sa-1",
            "general-purpose",
            "author board-setup skill",
            &naming,
            true,
        );
        assert!(
            with_cta.contains("subagent_id: sa-1"),
            "id must stay pollable: {with_cta}"
        );
        assert!(
            with_cta.contains("get_command_or_subagent_output") && with_cta.contains("timeout_ms"),
            "poll instruction must remain: {with_cta}"
        );
        assert!(
            !with_cta.contains("to wait for results")
                && !with_cta.contains("finish remaining independent work first")
                && !with_cta.contains("Continue other work"),
            "spawn notice must stay factual and not restate polling policy: {with_cta}"
        );
        assert!(
            !with_cta.contains("<system-reminder>") && !with_cta.contains("<system_reminder>"),
            "shared spawn text must not hardcode either reminder tag: {with_cta}"
        );
        assert!(
            with_cta.contains(BACKGROUND_SUBAGENT_CONTINUE_PARENT_WORK),
            "open parent work must get the continue-parent CTA: {with_cta}"
        );

        let poll_only = format_subagent_started_background(
            "sa-1",
            "general-purpose",
            "review pr",
            &naming,
            false,
        );
        assert!(
            poll_only.contains("timeout_ms")
                && !poll_only.contains(BACKGROUND_SUBAGENT_CONTINUE_PARENT_WORK),
            "no leftover parent work must not get the CTA: {poll_only}"
        );
    }

    #[test]
    fn background_notices_track_renamed_tool_and_params() {
        // Randomized host names: the notice must never emit canonical
        // `task_ids` / `timeout_ms` keys the retrieval tool's schema lacks.
        let naming = BackgroundNoticeNaming {
            task_output_tool: "FetchJobResult",
            task_ids_param: "job_ids",
            timeout_ms_param: "max_wait",
        };
        let spawn = format_subagent_started_background("sa-9", "explore", "scan", &naming, false);
        assert!(
            spawn.contains("use FetchJobResult with job_ids=[\"sa-9\"]")
                && spawn.contains("a positive max_wait"),
            "renamed tool/params must appear: {spawn}"
        );
        assert!(
            !spawn.contains("task_ids") && !spawn.contains("timeout_ms"),
            "canonical param names must not remain after rename: {spawn}"
        );

        let auto =
            format_subagent_auto_backgrounded("sa-9", "explore", "scan", &naming, true, true);
        assert!(
            auto.contains("moved to the background")
                && auto.contains("you will be notified when it completes")
                && auto.contains("use FetchJobResult with job_ids=[\"sa-9\"]")
                && auto.contains("a positive max_wait"),
            "auto-bg notice must share the renamed retrieval line: {auto}"
        );
        assert!(
            auto.contains(BACKGROUND_SUBAGENT_CONTINUE_PARENT_WORK),
            "auto-bg notice must carry the CTA when parent work remains: {auto}"
        );

        let quiet =
            format_subagent_auto_backgrounded("sa-9", "explore", "scan", &naming, false, false);
        assert!(
            !quiet.contains("you will be notified"),
            "no notification promise without system reminders: {quiet}"
        );
    }

    #[test]
    fn continue_parent_work_detects_prior_exec_and_skips_review_only() {
        assert!(should_continue_parent_work(
            &[
                "run bazel test //hw5:op_chain and don't skip smoke".into(),
                "spawn an agent to create a skill for board setup".into(),
            ],
            "Create bringup skill",
            "write SKILL.md",
        ));
        assert!(should_continue_parent_work(
            &["PRs still fail, fix and gt submit + /pr-babysit".into()],
            "[pr-babysit] stack",
            "fix CI",
        ));
        assert!(should_continue_parent_work(
            &[
                "rerun 512 and 672 adler tests".into(),
                "while waiting, use 8 subagents on the nondetermin bug".into(),
            ],
            "FWD experiment",
            "kernel repro",
        ));
        assert!(!should_continue_parent_work(
            &["review this PR https://github.com/example/repo/pull/1".into()],
            "[reviewer] pr",
            "review the diff",
        ));
        assert!(!should_continue_parent_work(
            &["/pr-babysit https://example.com/github/pr/demo/1".into()],
            "[pr-babysit]",
            "babysit checks",
        ));
        assert!(!should_continue_parent_work(&[], "explore", "look around",));
        // Distant implement + recent review-only: do not inherit old exec.
        assert!(!should_continue_parent_work(
            &[
                "implement the auth feature and run tests".into(),
                "what would example secrets look like".into(),
                "and what if we have more than one".into(),
                "ok. can you do a 3 agent review please".into(),
            ],
            "3 agent review",
            "review the diff",
        ));
        // Exec two asks ago still counts (check bugs → nudge → review spawn).
        assert!(should_continue_parent_work(
            &[
                "kan je is de recente signal bug's checken?".into(),
                "loop alle 4 de punten even na aub".into(),
                "ja, en review die meteen met andere agents".into(),
            ],
            "review bugs",
            "independent review",
        ));
        // Exec three asks ago is too old once the user is review-only.
        assert!(!should_continue_parent_work(
            &[
                "implement foo and fix the tests".into(),
                "thanks".into(),
                "ok noted".into(),
                "/code-review the PR".into(),
            ],
            "review",
            "review the diff",
        ));
    }
}
