//! Goal-verification stage (harness-owned).
//!
//! The adversarial skeptic panel is the whole verification: it
//! spawns N independent skeptic subagents in parallel,
//! parses each one's JSON verdict (with terminal-token fallback), and
//! `tool_context.subagent_event_tx` — no `task` tool call, so the
//! parent model's transcript stays clean. The spawn is hidden behind
//! the [`GoalClassifierSpawner`] trait so tests can inject deterministic
//! responses; production uses [`ChannelSpawner`]. The struct / trait /
//! constant names retain the `classifier` prefix to keep the env /
//! remote / config wire contract stable across the rewire.

#![allow(dead_code)]

pub(crate) mod evidence;

use crate::session::events::{Event, GoalClassifierFailOpenReason};
use crate::session::goal_planner::{
    GOAL_ROLE_AWAIT_BUDGET_EXCEEDED, GOAL_ROLE_SUBAGENT_TYPE, RoleRenderedPrompt,
    RoleSpawnOverride, spawn_with_fail_open_retry,
};
use crate::session::goal_role_tools::RoleToolNames;
use crate::session::goal_tracker::GoalClassifierVerdict;
use echo_agent_session_events::EventWriter;
use echo_agent_tools::implementations::echo_agent_build::task::backend::{
    ChannelBackend, SubagentBackend,
};
use echo_agent_tools::implementations::echo_agent_build::task::types::{
    SubagentOwner, SubagentRequest, SubagentRuntimeOverrides,
};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

// Constants

/// Default per-goal classifier run cap. A sane local default; the stall
/// early-exit ([`crate::session::goal_tracker::GOAL_CLASSIFIER_STALL_THRESHOLD`])
/// is the primary, cheaper stop for stuck loops, so this cap is a
/// runaway-cost backstop. There is no upper ceiling — override via
/// `ECHO_AGENT_GOAL_CLASSIFIER_MAX` or remote `goal_classifier_max_runs` to
/// raise it arbitrarily (only the `GOAL_CLASSIFIER_MAX_RUNS_MIN` floor
/// is enforced).
pub(crate) const GOAL_CLASSIFIER_MAX_RUNS_DEFAULT: u32 = 10;

/// Floor for `ECHO_AGENT_GOAL_CLASSIFIER_MAX` / remote `goal_classifier_max_runs`.
/// Floor 1 keeps the gate live (0 would disable rejection entirely).
/// There is deliberately no upper ceiling so the cap can be raised
/// arbitrarily via remote/env.
pub(crate) const GOAL_CLASSIFIER_MAX_RUNS_MIN: u32 = 1;

/// Maximum size of the embedded diff in bytes. Past this the diff is
/// truncated with an explicit marker — the verifier prompt's
/// diff-based rules can still operate on the head of the diff plus the
/// truncation marker (and rule 5 if even the head is unavailable).
pub(crate) const GOAL_CLASSIFIER_DIFF_MAX_BYTES: usize = 256 * 1024;

/// Overall byte cap for the aggregated panel details file. A 3-skeptic
/// panel of rich reports runs ~30-40 KB; this ceiling leaves wide
/// headroom (≈5 large reports) while bounding a pathological skeptic.
/// Overall cap only — never per-line.
pub(crate) const GOAL_VERIFIER_PANEL_MAX_BYTES: usize = 512 * 1024;

/// Template for the per-attempt details FILE NAME, rooted under the
/// owner-only (0700) per-goal scratch root by `format_details_path`.
/// Classifier artifacts never live in bare `/tmp`: their names are
/// predictable from the prompt/log-visible `verifier_id`, so a
/// world-writable directory would let a local attacker pre-plant a
/// symlink and redirect the harness's writes (see
/// [`super::goal_tracker::ensure_goal_scratch_root`]).
pub(crate) const GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE: &str =
    "goal-classifier-{verifier_id}-{attempt}.md";

/// Template for the per-attempt patch FILE NAME (rooted like
/// [`GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE`]). The captured diff is
/// written here and each skeptic reads it via its `read_file` tool
/// instead of receiving the body inline in its prompt.
pub(crate) const GOAL_CLASSIFIER_CHANGES_PATH_TEMPLATE: &str =
    "goal-classifier-{verifier_id}-{attempt}.patch";

/// Wall-clock budget for the best-effort `git rev-parse HEAD` capture
/// during goal creation. The call must NEVER block goal creation; if
/// the workspace isn't a git repo or HEAD takes longer than this
/// (network filesystem, etc.) we drop the baseline and surface
/// `(unavailable)` to each skeptic — matching the verifier prompt's
/// rule 5.
const GIT_BASELINE_CAPTURE_TIMEOUT: Duration = Duration::from_secs(1);

/// Subagent type used for each verifier-skeptic spawn. `general-purpose`
/// gives the subagent the full read/grep/file tool inventory needed
/// to corroborate diff hunks against the workspace — the verifier
/// prompt explicitly forbids workspace mutation. The configured `agent_type`
/// selects the HARNESS, not this subagent type.
const GOAL_CLASSIFIER_SUBAGENT_TYPE: &str = GOAL_ROLE_SUBAGENT_TYPE;

/// Description shown in the pager subagent strip. Kept short — the
/// stage may spawn up to `GOAL_VERIFIER_SKEPTIC_MAX` skeptics per
/// attempt, but a stable label reads more cleanly in the strip than
/// a per-spawn suffix.
const GOAL_CLASSIFIER_SUBAGENT_DESCRIPTION: &str = "goal achievement skeptic";

const GOAL_VERIFIER_PROMPT_TEMPLATE: &str = include_str!("templates/goal_verifier_prompt.md");

/// Default number of adversarial skeptics spawned per verification
/// attempt. Override via `ECHO_AGENT_GOAL_VERIFIER_N` (clamped 1..=5) or the
/// remote `goal_verifier_count` setting. Default 3 yields a genuine
/// majority vote (`⌈3/2⌉ = 2` not-refuted to pass): a lone outlier in
/// either direction — one rubber-stamp or one false-refute — cannot
/// decide the outcome, unlike N=2 where a 1-1 tie survives and a single
/// lenient skeptic passes what a single strict one refutes.
pub(crate) const GOAL_VERIFIER_SKEPTIC_COUNT: u32 = 3;

/// Lower/upper bounds for `ECHO_AGENT_GOAL_VERIFIER_N` / remote
/// `goal_verifier_count`. Five is the practical ceiling — any more is
/// pointless cost and saturates the subagent coordinator.
pub(crate) const GOAL_VERIFIER_SKEPTIC_MIN: u32 = 1;
pub(crate) const GOAL_VERIFIER_SKEPTIC_MAX: u32 = 5;

/// Expand a skeptic `pool` to a per-index assignment of length `n` via
/// round-robin (index `i` → `pool[i % pool.len()]`), reusing the frozen
/// `existing` prefix verbatim.
///
/// Resume stability + monotonic growth: committed indices are never
/// rewritten, so skeptic-0 always keeps `pool[0]` across resume AND
/// cold-fallback, and a later `n` bump only appends new indices (continuing
/// the round-robin, clamped by the caller). An empty `pool` keeps `existing`
/// unchanged (a frozen assignment survives a remote-cleared pool); empty
/// `existing` + empty `pool` ⇒ empty (all skeptics inherit the current
/// model). `n` is the CLAMPED skeptic count — identical to the value used at
/// the fan-out site — so the assignment never desyncs from the spawned
/// indices.
pub(crate) fn expand_skeptic_assignment(
    existing: &[crate::util::config::GoalRoleModel],
    pool: &[crate::util::config::GoalRoleModel],
    n: usize,
) -> Vec<crate::util::config::GoalRoleModel> {
    let mut out = existing.to_vec();
    if pool.is_empty() || out.len() >= n {
        return out;
    }
    for i in out.len()..n {
        out.push(pool[i % pool.len()].clone());
    }
    out
}

/// Per-skeptic JSON verdict FILE NAME template (rooted under the
/// per-goal scratch root like [`GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE`]).
/// The harness reads each skeptic's JSON to drive the aggregation; the
/// terminal token is the fast-path signal but the JSON is authoritative.
pub(crate) const GOAL_VERIFIER_VERDICT_PATH_TEMPLATE: &str =
    "goal-verdict-{verifier_id}-{attempt}-{skeptic_idx}.json";

/// Per-skeptic Markdown details FILE NAME template (rooted like
/// [`GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE`]). Each skeptic writes its
/// own analysis here; the harness concatenates them into the canonical
/// `GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE` path the existing ack
/// contract surfaces.
pub(crate) const GOAL_VERIFIER_DETAILS_PATH_TEMPLATE: &str =
    "goal-classifier-{verifier_id}-{attempt}-skeptic-{skeptic_idx}.md";

// Outcome + spawner abstraction

/// Result of one classifier attempt. `Achieved` / `NotAchieved` are
/// PARSE-class outcomes: the subagent produced a usable verdict.
/// `FailOpenAchieved` is INFRA-class: the harness could not extract
/// a verdict and treats the goal as achieved so an internal failure
/// never blocks user progress. PARSE-class fail-closed outcomes
/// (malformed terminal token, missing details file) map onto
/// `NotAchieved`; telemetry distinguishes them via
/// `Event::GoalClassifierFailClosed`.
#[derive(Debug, Clone)]
pub(crate) enum GoalClassifierOutcome {
    Achieved {
        details_path: String,
    },
    NotAchieved {
        details_path: String,
        /// One-line-per-refuter gist inlined into the rejection nudge so
        /// a weak model sees the actionable gaps without a file read (see
        /// [`build_gaps_summary`]). Never empty for a real rejection
        /// (≥1 refuter).
        gaps_summary: String,
        /// Blocker bullets grouped by [`SkepticBlocking`] class for the
        /// user-facing auto-pause message (see [`build_pause_summary`]).
        pause_summary: String,
        /// Stall fingerprint computed at the SOURCE from the raw
        /// (undecorated, log-path-free) gap evidence via
        /// [`gap_fingerprint`]; the drain compares it across attempts.
        gap_fingerprint: String,
    },
    /// Every refuter classified its gap as a contradiction or
    /// environment-unverifiable blocker — no model-fixable gap remains,
    /// so iterating cannot help. The goal pauses for a user decision
    /// rather than receiving another retry nudge. No stall fingerprint
    /// is carried — the drain resets the streak when routing here.
    Blocked {
        details_path: String,
        /// Grouped blocker bullets (all non-model-fixable) used as the
        /// user-facing pause message.
        pause_summary: String,
    },
    FailOpenAchieved {
        reason: GoalClassifierFailOpenReason,
        /// Empty when the failure happened before path resolution
        /// (e.g. an unsafe path was rejected by the validator).
        details_path: String,
    },
}

/// Subagent spawn abstraction. Production uses [`ChannelSpawner`];
/// tests use [`MockSpawner`].
#[async_trait::async_trait]
pub(crate) trait GoalClassifierSpawner: Send + Sync {
    /// Spawn under `id` and return the terminal response when the subagent
    /// finishes. `resume_from`, when `Some`, names a previously-completed
    /// subagent session whose transcript / tool-state / model the new
    /// child inherits (used to resume skeptic 0 across attempts).
    async fn spawn_classifier(
        &self,
        id: &str,
        skeptic_idx: u32,
        prompt: RoleRenderedPrompt,
        details_path: &Path,
        resume_from: Option<&str>,
    ) -> Result<String, SpawnError>;
}

/// Spawn-time error. Distinguishes between transport errors (channel
/// closed, coordinator unreachable) and runtime errors (subagent
/// reported failure, was cancelled, etc.) so the runner can map them
/// to the correct fail-open reason.
#[derive(Debug)]
pub(crate) enum SpawnError {
    /// Subagent coordinator was unreachable (channel closed, no
    /// `subagent_event_tx` plumbed). Maps to `SamplerError`.
    Transport(String),
    /// Subagent ran but reported failure. `cancelled: true` maps to
    /// [`GoalClassifierFailOpenReason::Aborted`]; `cancelled: false`
    /// maps to [`GoalClassifierFailOpenReason::SamplerError`].
    Runtime { message: String, cancelled: bool },
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(d) => write!(f, "subagent transport error: {d}"),
            Self::Runtime { message, cancelled } => {
                write!(
                    f,
                    "subagent runtime error (cancelled={cancelled}): {message}"
                )
            }
        }
    }
}

impl crate::session::goal_planner::RetryableSpawnError for SpawnError {
    fn is_cancelled(&self) -> bool {
        matches!(
            self,
            SpawnError::Runtime {
                cancelled: true,
                ..
            }
        )
    }
}

// Path resolution + validation

/// Root a substituted classifier file name under the goal's private
/// scratch root. Single seam for every classifier artifact path so the
/// owner-only-directory invariant cannot drift per call site.
fn scratch_rooted(verifier_id: &str, file_name: String) -> String {
    super::goal_tracker::goal_scratch_root(verifier_id)
        .join(file_name)
        .to_string_lossy()
        .into_owned()
}

/// Substitute the `{verifier_id}` / `{attempt}` placeholders in
/// `GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE` and root the result under
/// the goal's scratch root. Pure string ops; no I/O.
pub(crate) fn format_details_path(verifier_id: &str, attempt: u32) -> String {
    scratch_rooted(
        verifier_id,
        GOAL_CLASSIFIER_DETAILS_PATH_TEMPLATE
            .replace("{verifier_id}", verifier_id)
            .replace("{attempt}", &attempt.to_string()),
    )
}

/// Substitute placeholders in `GOAL_CLASSIFIER_CHANGES_PATH_TEMPLATE`
/// and root the result under the goal's scratch root.
pub(crate) fn format_changes_path(verifier_id: &str, attempt: u32) -> String {
    scratch_rooted(
        verifier_id,
        GOAL_CLASSIFIER_CHANGES_PATH_TEMPLATE
            .replace("{verifier_id}", verifier_id)
            .replace("{attempt}", &attempt.to_string()),
    )
}

/// Errors classifying a candidate details-file path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PathValidationError {
    /// Path contains `..`, a NUL byte, or starts in a forbidden
    /// system prefix (`/etc`, `/proc`, `/sys`, `/dev`, `~`).
    UnsafeComponent,
    /// Path contains an unresolved `${...}` / `{...}` substitution
    /// marker other than the known classifier placeholders.
    UnresolvedSubstitution,
    /// Resolved path is outside the platform temp dir the classifier
    /// roots its artifacts under.
    OutsideAllowedPrefix,
}

impl std::fmt::Display for PathValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafeComponent => f.write_str("path contains an unsafe component"),
            Self::UnresolvedSubstitution => f.write_str("path contains unresolved substitution"),
            Self::OutsideAllowedPrefix => f.write_str("path is outside the allowed temp root"),
        }
    }
}

/// Validate the resolved classifier details-file path against the
/// platform temp dir (the goal scratch root's parent, where
/// `format_*_path` roots every artifact). No bare-`/tmp` allowance:
/// every production caller validates a freshly `format_*_path`-built
/// path. See [`validate_details_path_in_root`] for the rules.
pub(crate) fn validate_details_path(path: &Path) -> Result<(), PathValidationError> {
    validate_details_path_in_root(path, &std::env::temp_dir())
}

/// Root-injectable core of [`validate_details_path`], so the
/// allowed-prefix rule is unit-testable on every platform (on Linux
/// `temp_dir()` IS `/tmp`). String-structural only; symlink resistance
/// comes from the owner-only (0700) scratch root.
pub(crate) fn validate_details_path_in_root(
    path: &Path,
    temp_root: &Path,
) -> Result<(), PathValidationError> {
    let s = path.to_string_lossy();
    // Cheap structural checks first — these don't require any I/O.
    if s.contains("..") || s.contains('\0') {
        return Err(PathValidationError::UnsafeComponent);
    }
    for prefix in &["/etc", "/proc", "/sys", "/dev"] {
        if s.starts_with(prefix) {
            return Err(PathValidationError::UnsafeComponent);
        }
    }
    if s.starts_with('~') {
        return Err(PathValidationError::UnsafeComponent);
    }
    // Substitution markers other than the known classifier placeholders.
    // The runner substitutes `{verifier_id}` / `{attempt}` BEFORE
    // validation, so any remaining `{...}` is an error.
    if s.contains("${") || s.contains('{') || s.contains('}') {
        return Err(PathValidationError::UnresolvedSubstitution);
    }
    // Allowed prefix — the platform temp dir (on macOS this is
    // /var/folders/..., not /tmp). Extend this check for future
    // session-dir overrides without changing the failure-class taxonomy.
    if !path.starts_with(temp_root) {
        return Err(PathValidationError::OutsideAllowedPrefix);
    }
    Ok(())
}

// Terminal-token parse

/// Parse an adversarial skeptic's terminal response. `Refuted`
/// ⇒ `Some(true)`, `Not Refuted` ⇒ `Some(false)`. The JSON verdict
/// file is authoritative when present; the terminal token is the
/// fast-path signal for the skeptic's vote when JSON parsing fails.
///
/// Tolerates code fences/backticks and a trailing `.`/`!` around the
/// token, but the response must contain ONLY the token — any other
/// prose stays `None`.
pub(crate) fn parse_skeptic_terminal_response(text: &str) -> Option<bool> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        // Drop fence lines entirely, including language-tagged ones
        // ("```text") that backtick-trimming alone would leave behind.
        .filter(|l| !l.starts_with("```"))
        .map(|l| l.trim_matches('`').trim_end_matches(['.', '!']).trim())
        .filter(|l| !l.is_empty())
        .collect();
    match lines.as_slice() {
        ["Refuted"] => Some(true),
        ["Not Refuted"] => Some(false),
        _ => None,
    }
}

// Git baseline capture (called from `setup_goal`)

/// Best-effort `git rev-parse HEAD` capture for goal creation.
///
/// Returns the commit SHA on success; `None` for any failure
/// (workspace is not a git repo, `git` is not installed, HEAD has
/// no commits, the call timed out). NEVER blocks goal creation —
/// the wall-clock budget is bounded by `GIT_BASELINE_CAPTURE_TIMEOUT`
/// and the caller treats `None` as the documented "no baseline"
/// signal (each skeptic renders `CHANGES_FILE: (unavailable)` and the
/// verifier prompt's rule 5 takes over).
pub(crate) async fn capture_git_baseline(workspace_root: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(crate::util::subprocess::git_bin());
    cmd.arg("rev-parse").arg("HEAD").current_dir(workspace_root);

    let output = match tokio::time::timeout(GIT_BASELINE_CAPTURE_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            tracing::debug!(
                error = %err,
                "goal baseline capture: failed to spawn git rev-parse",
            );
            return None;
        }
        Err(_) => {
            tracing::debug!("goal baseline capture: git rev-parse exceeded budget");
            return None;
        }
    };
    if !output.status.success() {
        tracing::debug!(
            exit = ?output.status.code(),
            "goal baseline capture: git rev-parse non-zero exit",
        );
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        return None;
    }
    Some(sha)
}

// Trace-only recording for harness-spawned subagents

/// Build the synthetic `task` tool_call + tool_result pair for a
/// harness-spawned subagent, shaped like a model-issued `task` spawn.
///
/// The tool_result MUST carry the real task tool's `<subagent_result>` footer
/// (via [`echo_agent_tool_types::format_resume_footer`]): trace tooling
/// discovers subagents by scanning tool_result bodies for that
/// `subagent_id:` block, so without it the harness subagent never shows in the
/// session tree. The footer id equals the child session id, so the viewer can
/// fetch its uploaded trace.
pub(crate) fn build_subagent_trace_items(
    task_tool_name: &str,
    subagent_id: &str,
    subagent_type: &str,
    description: &str,
    prompt: &str,
    output: &str,
) -> Vec<echo_agent_sampling_types::conversation::ConversationItem> {
    use echo_agent_sampling_types::conversation::{ConversationItem, ToolCall};
    let arguments = serde_json::json!({
        "description": description,
        "subagent_type": subagent_type,
        "prompt": prompt,
    })
    .to_string();
    let call = ConversationItem::assistant_tool_calls(vec![ToolCall {
        id: std::sync::Arc::from(subagent_id),
        name: task_tool_name.to_string(),
        arguments: std::sync::Arc::from(arguments),
    }]);
    let footer = echo_agent_tool_types::format_resume_footer(subagent_id, subagent_type, None);
    let result = ConversationItem::tool_result(subagent_id, format!("{output}\n\n{footer}"));
    vec![call, result]
}

/// Record a harness-spawned subagent into the in-progress harness trace phase
/// as a synthetic `task` call (see [`build_subagent_trace_items`]). The items
/// accumulate in a side buffer (never the live model context); the caller seals
/// the phase via [`echo_agent_chat_state::ChatStateHandle::flush_harness_trace_turn`]
/// so it uploads as its own sibling `turn_{N}` artifact. No-op when tracing is
/// off (`sink` absent) or no prompt was captured. `sink` carries the chat-state
/// handle and the resolved `task` tool name.
pub(crate) fn record_subagent_trace(
    sink: Option<&(echo_agent_chat_state::ChatStateHandle, String)>,
    subagent_id: &str,
    subagent_type: &str,
    description: &str,
    prompt: Option<&str>,
    output: &str,
) {
    if let (Some((handle, task_tool)), Some(prompt)) = (sink, prompt) {
        handle.append_harness_trace_items(build_subagent_trace_items(
            task_tool,
            subagent_id,
            subagent_type,
            description,
            prompt,
            output,
        ));
    }
}

// Production spawner — wraps the subagent coordinator channel

/// Production spawner. Sends a `SubagentEvent::Spawn` to the session's
/// coordinator and awaits the result on a fresh oneshot. The parent model
/// never sees the spawn live — it is direct (no `task` tool call). When a
/// `trace_sink` is wired, each skeptic is recorded as a synthetic `task` call
/// (see [`record_subagent_trace`]) into the harness trace phase; the caller
/// seals the panel into its own sibling trace turn so the subagents are
/// discoverable in data collection.
pub(crate) struct ChannelSpawner {
    pub(crate) event_tx: tokio::sync::mpsc::UnboundedSender<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent,
    >,
    pub(crate) foreground_wait: Option<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentForegroundWait,
    >,
    pub(crate) parent_session_id: String,
    pub(crate) parent_prompt_id: Option<String>,
    pub(crate) cwd: Option<String>,
    /// Trace-artifact sink + the resolved `task` tool name. `None` disables
    /// trace recording (tests, or sessions without trace capture).
    pub(crate) trace_sink: Option<(echo_agent_chat_state::ChatStateHandle, String)>,
    /// Per-skeptic-index resolved model+toolset override, indexed by
    /// `skeptic_idx`. An out-of-range index (or `Default`) inherits the
    /// current model — round-robin expansion + auth/capability fail-open is
    /// resolved parent-side before the spawner is built.
    pub(crate) skeptic_overrides: Vec<RoleSpawnOverride>,
    /// Event sink for the spawn-and-retry-once fail-open telemetry; `None`
    /// in tests / when no event log is wired.
    pub(crate) events: Option<EventWriter>,
}

#[async_trait::async_trait]
impl GoalClassifierSpawner for ChannelSpawner {
    async fn spawn_classifier(
        &self,
        id: &str,
        skeptic_idx: u32,
        prompt: RoleRenderedPrompt,
        _details_path: &Path,
        resume_from: Option<&str>,
    ) -> Result<String, SpawnError> {
        // Clone the primary render for the trace pair only when tracing; the
        // wrapper moves each render into its attempt (no other clone).
        let trace_prompt = self.trace_sink.as_ref().map(|_| prompt.primary.clone());
        // Per-index override; out-of-range ⇒ inherit (defensive).
        let inherit = RoleSpawnOverride::default();
        let override_ = self
            .skeptic_overrides
            .get(skeptic_idx as usize)
            .unwrap_or(&inherit);
        let outcome = spawn_with_fail_open_retry(
            "skeptic",
            Some(skeptic_idx),
            override_,
            self.events.as_ref(),
            prompt,
            |model, harness, prompt| self.send_one(id, prompt, model, harness, resume_from),
        )
        .await;

        match &outcome {
            Ok(text) => record_subagent_trace(
                self.trace_sink.as_ref(),
                id,
                GOAL_CLASSIFIER_SUBAGENT_TYPE,
                GOAL_CLASSIFIER_SUBAGENT_DESCRIPTION,
                trace_prompt.as_deref(),
                text,
            ),
            Err(SpawnError::Runtime { message, .. }) => record_subagent_trace(
                self.trace_sink.as_ref(),
                id,
                GOAL_CLASSIFIER_SUBAGENT_TYPE,
                GOAL_CLASSIFIER_SUBAGENT_DESCRIPTION,
                trace_prompt.as_deref(),
                message,
            ),
            Err(SpawnError::Transport(_)) => {}
        }
        outcome
    }
}

impl ChannelSpawner {
    /// Send one skeptic spawn (model + harness override resolved by the caller)
    /// and await its terminal result. The fail-open wrapper calls this once
    /// or twice (retry on the current model + session harness). The
    /// subagent_type is always [`GOAL_CLASSIFIER_SUBAGENT_TYPE`];
    /// `harness_agent_type` selects the harness flavor (`None` ⇒ session
    /// harness).
    async fn send_one(
        &self,
        id: &str,
        prompt: String,
        model: Option<String>,
        harness_agent_type: Option<String>,
        resume_from: Option<&str>,
    ) -> Result<String, SpawnError> {
        let request = SubagentRequest {
            id: id.to_string(),
            prompt,
            description: GOAL_CLASSIFIER_SUBAGENT_DESCRIPTION.to_string(),
            subagent_type: GOAL_CLASSIFIER_SUBAGENT_TYPE.to_string(),
            parent_session_id: self.parent_session_id.clone(),
            parent_prompt_id: self.parent_prompt_id.clone(),
            resume_from: resume_from.map(str::to_string),
            cwd: self.cwd.clone(),
            runtime_overrides: SubagentRuntimeOverrides {
                model,
                harness_agent_type,
                ..Default::default()
            },
            run_in_background: false,
            // Harness-internal: never surface to the model's idle reminder.
            surface_completion: false,
            await_to_completion: false,
            fork_context: false,
            owner: SubagentOwner::Task,
            cancel_token: tokio_util::sync::CancellationToken::new(),
        };
        let backend = ChannelBackend::new(self.event_tx.clone());
        let result = backend
            .spawn_with_foreground_wait(request, self.foreground_wait.as_ref())
            .await
            .map_err(|error| SpawnError::Transport(error.to_string()))?;
        if result.backgrounded {
            let _ = backend.cancel(&result.subagent_id).await;
            return Err(SpawnError::Runtime {
                message: GOAL_ROLE_AWAIT_BUDGET_EXCEEDED.to_owned(),
                cancelled: true,
            });
        }
        if !result.success {
            let message = result.error.unwrap_or_else(|| "unknown error".to_string());
            return Err(SpawnError::Runtime {
                message,
                cancelled: result.cancelled,
            });
        }
        Ok(result.output.to_string())
    }
}

// Fail-open helper (shared by verification stage)

/// Record a fail-open outcome: emit telemetry, write a placeholder
/// details file (when the path is resolved), and return the
/// `FailOpenAchieved` value. Empty `details_raw` skips the write.
async fn record_fail_open(
    reason: GoalClassifierFailOpenReason,
    attempt: u32,
    started: std::time::Instant,
    emit_event: &dyn Fn(Event),
    details_path: Option<&Path>,
    details_raw: String,
) -> GoalClassifierOutcome {
    let latency_ms = started.elapsed().as_millis() as u64;
    emit_event(Event::GoalClassifierFailOpen {
        reason: reason.as_const_str(),
        attempt,
        latency_ms,
    });
    let resolved_path = match details_path {
        // Surface the path only when the placeholder is on disk — a failed
        // write would point the user at a missing file (empty = no details).
        Some(p) if maybe_write_fail_open_placeholder(p, reason).await => details_raw,
        _ => String::new(),
    };
    GoalClassifierOutcome::FailOpenAchieved {
        reason,
        details_path: resolved_path,
    }
}

/// Write `body` to `path` atomically via tempfile + rename. The
/// tempfile sits next to the target so `rename` stays on one FS.
async fn write_patch_file_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    // Scratch-rooted paths always have a parent; a rootless path is a bug.
    let Some(dir) = path.parent() else {
        return Err(std::io::Error::other("patch path has no parent directory"));
    };
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("goal-classifier.patch");
    let tmp = dir.join(format!(".{file_name}.{}.tmp", uuid::Uuid::now_v7()));
    tokio::fs::write(&tmp, body).await?;
    if let Err(err) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(err);
    }
    Ok(())
}

/// Write a placeholder file at `path` unless a non-empty file is
/// already there. `headline` becomes the Markdown `# <headline>`
/// header; `body` is appended verbatim. Best-effort.
///
/// Returns `true` when a non-empty details file exists at `path`
/// afterward (it already did, or the write succeeded) and `false` when
/// the write was attempted and failed — so the caller never surfaces a
/// path to a file that isn't there.
async fn maybe_write_classifier_placeholder(path: &Path, headline: &str, body: &str) -> bool {
    if let Ok(meta) = tokio::fs::metadata(path).await
        && meta.is_file()
        && meta.len() > 0
    {
        return true;
    }
    let content = format!("# {headline}\n\n{body}\n");
    match tokio::fs::write(path, content).await {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "goal classifier: failed to write placeholder",
            );
            false
        }
    }
}

/// Returns `true` iff the placeholder is on disk afterward (see
/// [`maybe_write_classifier_placeholder`]).
async fn maybe_write_fail_open_placeholder(
    path: &Path,
    reason: GoalClassifierFailOpenReason,
) -> bool {
    let reason_str = reason.as_const_str();
    let body = format!(
        "The verification stage did not produce a verdict (infra-class \
         failure). The harness treated the goal as Achieved as a \
         fail-open fallback. No skeptic analysis was captured.\n\n\
         ## Reason\n\n{reason_str}"
    );
    maybe_write_classifier_placeholder(
        path,
        &format!("Verification fail-open: {reason_str}"),
        &body,
    )
    .await
}

// Verifier — the adversarial skeptic panel

/// Confidence label on a skeptic verdict. The JSON wire vocabulary is
/// `high|medium|low`; any other (or missing) value normalises to
/// `Unknown` so a verifier with a botched JSON field still produces an
/// aggregable vote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkepticConfidence {
    High,
    Medium,
    Low,
    Unknown,
}

impl SkepticConfidence {
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::Unknown,
        }
    }
    pub(crate) fn as_const_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Unknown => "unknown",
        }
    }

    /// Sort key for the inlined gaps summary: high-confidence refuters
    /// surface first (`High` → 0 … `Unknown` → 3).
    fn rank(self) -> u8 {
        match self {
            Self::High => 0,
            Self::Medium => 1,
            Self::Low => 2,
            Self::Unknown => 3,
        }
    }
}

/// Classification of a refutation's blocker. `None` is an ordinary
/// model-fixable gap (the default — absent or unrecognised wire values
/// normalise here, keeping the JSON contract back-compatible).
/// `Contradiction` flags an objective/plan internal conflict;
/// `Unverifiable` flags evidence that is infeasible to capture in the
/// current environment. A rejection whose refuters are *all* non-`None`
/// cannot progress by iterating and routes to the blocked outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SkepticBlocking {
    #[default]
    None,
    Contradiction,
    Unverifiable,
}

impl SkepticBlocking {
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "contradiction" => Self::Contradiction,
            "unverifiable" => Self::Unverifiable,
            _ => Self::None,
        }
    }
    fn is_blocking(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Parsed skeptic verdict — JSON shape mirrors the verifier prompt's
/// contract. `evidence` and `details_md` are kept for the aggregated
/// details file; the harness operates on `refuted` + `confidence` +
/// `blocking`.
/// One concise verifier finding (the implementer-facing gap list). Fields
/// default to empty for weak-model robustness; an all-empty finding is
/// dropped at parse time.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(crate) struct Finding {
    /// `bug` | `gap` | `todo` (rendered verbatim after trim).
    #[serde(default)]
    pub kind: String,
    /// `path:line` when code-related, else a short place; may be empty.
    #[serde(default)]
    pub location: String,
    /// One-line description.
    #[serde(default)]
    pub detail: String,
}

impl Finding {
    fn is_empty(&self) -> bool {
        self.kind.trim().is_empty()
            && self.location.trim().is_empty()
            && self.detail.trim().is_empty()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SkepticVerdict {
    pub refuted: bool,
    pub evidence: String,
    pub confidence: SkepticConfidence,
    pub blocking: SkepticBlocking,
    pub details_md: String,
    /// Structured findings (the implementer-facing gap list); empty when
    /// the verifier emitted none (then the `evidence` fallback is used).
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SkepticVerdictRaw {
    #[serde(default)]
    refuted: Option<bool>,
    #[serde(default)]
    evidence: Option<String>,
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    blocking: Option<String>,
    #[serde(default)]
    details_md: Option<String>,
    #[serde(default)]
    findings: Option<Vec<Finding>>,
}

/// Parse the JSON body the skeptic wrote to its `{VERDICT_FILE}`.
///
/// Matches the verdict schema `required: ["refuted", "evidence",
/// "confidence"]`: all three are mandatory.
/// A missing or empty `evidence` field rejects (`None`) — without
/// evidence the rubber-stamp failure mode this contract explicitly
/// closes is back open. `details_md` is optional (it's a harness-side
/// extension to the schema; the aggregator prefers the on-disk
/// per-skeptic report and uses this JSON field only as a fallback when
/// that file is missing/empty). Extra fields are
/// tolerated. The skeptic-level fallback (`run_one_skeptic`) maps any
/// `None` here to a synthetic `refuted: true` vote.
pub(crate) fn parse_verdict_json(body: &str) -> Option<SkepticVerdict> {
    let raw: SkepticVerdictRaw = serde_json::from_str(body.trim()).ok()?;
    let refuted = raw.refuted?;
    let evidence = raw.evidence?;
    if evidence.trim().is_empty() {
        return None;
    }
    let confidence = SkepticConfidence::parse(&raw.confidence?);
    let blocking = raw
        .blocking
        .as_deref()
        .map(SkepticBlocking::parse)
        .unwrap_or_default();
    let findings = raw
        .findings
        .unwrap_or_default()
        .into_iter()
        .filter(|f| !f.is_empty())
        .collect();
    Some(SkepticVerdict {
        refuted,
        evidence,
        confidence,
        blocking,
        details_md: raw.details_md.unwrap_or_default(),
        findings,
    })
}

/// Result of one skeptic in the panel. The `refuted` flag is the
/// aggregator's input; the rest is for the details-file render. A
/// malformed / missing JSON file maps to `refuted: true` (fail-closed
/// at the skeptic level) per the verifier prompt's bias.
#[derive(Debug, Clone)]
pub(crate) struct SkepticResult {
    pub skeptic_idx: u32,
    pub refuted: bool,
    pub confidence: SkepticConfidence,
    /// Blocker classification carried over from the verdict JSON;
    /// `None` (default) for a model-fixable gap, a synthetic refute, or
    /// a terminal-token-only fallback.
    pub blocking: SkepticBlocking,
    /// Single-line `path:line` citation from the verdict JSON. Drives
    /// the stall fingerprint and the gaps-summary fallback when no
    /// structured `findings` were emitted.
    pub evidence: String,
    /// Structured findings for the implementer (preferred over `evidence`
    /// when non-empty). Empty on fallback / failure paths.
    pub findings: Vec<Finding>,
    /// `None` on a clean parse; populated when the JSON file was
    /// missing/malformed or the spawn failed.
    pub fallback_note: Option<String>,
    /// Per-skeptic spawn-to-verdict wall clock in ms. Plumbed up so
    /// the panel-level event can surface slow outliers even though
    /// emissions are batched after `join_all`.
    pub latency_ms: u64,
}

/// Substitute the per-skeptic JSON-verdict path placeholders and root
/// the result under the goal's scratch root.
pub(crate) fn format_verdict_path(verifier_id: &str, attempt: u32, skeptic_idx: u32) -> String {
    scratch_rooted(
        verifier_id,
        GOAL_VERIFIER_VERDICT_PATH_TEMPLATE
            .replace("{verifier_id}", verifier_id)
            .replace("{attempt}", &attempt.to_string())
            .replace("{skeptic_idx}", &skeptic_idx.to_string()),
    )
}

/// Substitute the per-skeptic Markdown-details path placeholders and
/// root the result under the goal's scratch root.
pub(crate) fn format_verifier_details_path(
    verifier_id: &str,
    attempt: u32,
    skeptic_idx: u32,
) -> String {
    scratch_rooted(
        verifier_id,
        GOAL_VERIFIER_DETAILS_PATH_TEMPLATE
            .replace("{verifier_id}", verifier_id)
            .replace("{attempt}", &attempt.to_string())
            .replace("{skeptic_idx}", &skeptic_idx.to_string()),
    )
}

/// Aggregate the panel into a quorum result.
///
/// **Variant-C** — for a fan-out panel (`total > 1`), skeptic 0's
/// not-refuted vote does NOT count: approval needs a STRICT MAJORITY of
/// the COLD panel (`skeptic_idx >= 1`), `needed = cold_count / 2 + 1`.
///
/// The required cold-approval COUNT is monotone non-decreasing in N
/// (1, 2, 2, 3 for cold sizes 1..4), so more skeptics never let fewer
/// independent cold judges carry approval. The tolerated-dissenter
/// FRACTION still loosens with N (N=3 needs 2/2, N=4 needs 2/3) — that is
/// majority voting's intended resilience to one flaky/biased skeptic, not
/// a defect. A strict majority of the FULL panel (incl. skeptic 0) is
/// rejected: it would force cold UNANIMITY on even N (N=4 → 3/3), making
/// the panel brittle to a single bad skeptic.
///
/// The bar derives from the cold-panel SIZE, not `total`: for a
/// contiguous panel `cold_count = total - 1` and `cold_count/2 + 1 ≡
/// ⌈total/2⌉`, but the cold-size form stays a true majority if skeptic 0
/// is ever absent from `results` (where `⌈total/2⌉` would slip to a
/// plurality).
///
/// Skeptic 0 is the resumed reject-gatekeeper, so letting its not-refuted
/// vote tip a borderline panel toward approval is the bias we explicitly
/// avoid. Its REFUTE still counts (in `refuted_count`, the pause/gaps
/// summaries, and the upstream high-confidence decisive-refute
/// short-circuit). `total <= 1` — the N==1 sole judge, and the
/// short-circuit case where `results` holds only skeptic 0 — keeps the
/// simple all-votes rule (`needed = 1`).
///
/// The adversarial bias-to-FAIL is deliberately enforced at the
/// per-skeptic level — transport / cancelled / runtime / malformed
/// outputs all degrade to a synthetic `refuted: true` vote in
/// [`run_one_skeptic`], NOT at the aggregator. The aggregator counts
/// votes; the bias lives upstream where the missing evidence is.
///
/// Returns `(refuted_count, total, quorum_achieved)`. `quorum_achieved`
/// is the quorum result only; the caller (`run_verification_stage`)
/// AND-tightens it with `!decisive_refute` for the final outcome.
pub(crate) fn aggregate_skeptic_verdicts(results: &[SkepticResult]) -> (u32, u32, bool) {
    let total = results.len() as u32;
    // Defensive empty-case: `run_verification_stage` clamps N >= 1
    // before fan-out, but the function is `pub(crate)` and tests
    // call it directly with `&[]`. Returning `(0, 0, false)` (not
    // achieved) matches the "default to refuted=true if uncertain"
    // bias if the clamp ever regresses.
    if total == 0 {
        return (0, 0, false);
    }
    let refuted_count = results.iter().filter(|r| r.refuted).count() as u32;
    let (needed, not_refuted) = if total <= 1 {
        // Sole judge / single-result short-circuit: the lone vote decides.
        (1, total - refuted_count)
    } else {
        // Variant-C: strict majority of the COLD panel; skeptic 0 excluded.
        let cold_count = results.iter().filter(|r| r.skeptic_idx >= 1).count() as u32;
        let cold_not_refuted = results
            .iter()
            .filter(|r| r.skeptic_idx >= 1 && !r.refuted)
            .count() as u32;
        (cold_count / 2 + 1, cold_not_refuted)
    };
    (refuted_count, total, not_refuted >= needed)
}

/// Per-evidence-line char cap for the inlined gaps summary — bounds a
/// runaway verdict yet holds a full multi-point gap without cutting the
/// primary finding mid-sentence. The model's reminder inlines only this
/// bounded summary; the untruncated per-skeptic writeup is persisted to
/// `last_classifier_details_path` for the user. Counted in `char`s, never
/// bytes, so truncation can't split a codepoint.
const GAPS_EVIDENCE_MAX_CHARS: usize = 800;

/// Neutralize and cap a model-written evidence string before it is
/// inlined into the `<system-reminder>` rejection nudge. The skeptic's
/// `evidence` is the only model-controlled text on the gaps path, so a
/// verifier emitting `</system-reminder>` or the `<goal-state>` tags
/// could otherwise close/reopen the reminder frame; a zero-width space
/// after the leading `<` breaks each literal tag while staying visually
/// identical. Capped on a `char` boundary (placeholder inertness is the
/// renderer's last-substitution concern, not this function's).
fn sanitize_evidence(evidence: &str) -> String {
    neutralize_reminder_tags(cap_chars(evidence.trim(), GAPS_EVIDENCE_MAX_CHARS))
}

/// Char cap for the whole multi-skeptic `{PRIOR_GAPS}` block, sized for
/// 2-3 skeptics × [`GAPS_MAX_FINDINGS`] findings — the per-line
/// [`GAPS_EVIDENCE_MAX_CHARS`] cap would chop later skeptics' gaps.
const PRIOR_GAPS_MAX_CHARS: usize = 4_000;

/// [`sanitize_evidence`]'s neutralization with the block-sized
/// [`PRIOR_GAPS_MAX_CHARS`] cap, for the `{PRIOR_GAPS}` prompt slot.
fn sanitize_prior_gaps(gaps: &str) -> String {
    neutralize_reminder_tags(cap_chars(gaps.trim(), PRIOR_GAPS_MAX_CHARS))
}

/// Truncate to `max_chars` `char`s (never bytes, so a codepoint can't
/// split) with an `…` suffix when capped; single pass via `char_indices`.
pub(crate) fn cap_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => {
            let mut s = String::with_capacity(cut + '…'.len_utf8());
            s.push_str(&text[..cut]);
            s.push('…');
            s
        }
        None => text.to_string(),
    }
}

/// Break the literal reminder-frame tags with a zero-width space so
/// model-written text cannot close/reopen the `<system-reminder>` /
/// `<goal-state>` frames it is embedded in.
pub(crate) fn neutralize_reminder_tags(text: String) -> String {
    text.replace("</system-reminder>", "<\u{200b}/system-reminder>")
        .replace("<system-reminder>", "<\u{200b}system-reminder>")
        .replace("</goal-state>", "<\u{200b}/goal-state>")
        .replace("<goal-state>", "<\u{200b}goal-state>")
}

/// Cap on findings rendered per refuter — bounds a runaway verdict while
/// holding a full multi-point gap list.
const GAPS_MAX_FINDINGS: usize = 12;

/// Render one structured finding as `kind · location — detail`, dropping
/// empty segments. Sanitized like evidence (tag-inert, char-capped).
fn render_finding(f: &Finding) -> String {
    let kind = f.kind.trim();
    let loc = f.location.trim();
    let detail = f.detail.trim();
    let head = if kind.is_empty() { "finding" } else { kind };
    let body = match (loc.is_empty(), detail.is_empty()) {
        (false, false) => format!("{head} · {loc} — {detail}"),
        (false, true) => format!("{head} · {loc}"),
        (true, false) => format!("{head} — {detail}"),
        (true, true) => head.to_string(),
    };
    sanitize_evidence(&body)
}

/// Render one refuter as a sanitized bullet. Prefers structured `findings`
/// (one sub-bullet each), else `evidence`, else the synthetic `fallback_note`,
/// else a bare no-evidence note. All model text is sanitized.
fn render_refuter_bullet(r: &SkepticResult) -> String {
    let header = format!(
        "- [skeptic {}, {}]",
        r.skeptic_idx,
        r.confidence.as_const_str()
    );
    if !r.findings.is_empty() {
        let lines: Vec<String> = r
            .findings
            .iter()
            .take(GAPS_MAX_FINDINGS)
            .map(|f| format!("  - {}", render_finding(f)))
            .collect();
        return format!("{header}\n{}", lines.join("\n"));
    }
    let evidence = r.evidence.trim();
    if !evidence.is_empty() {
        format!("{header} {}", sanitize_evidence(evidence))
    } else if let Some(note) = &r.fallback_note {
        format!(
            "- [skeptic {}] no verdict produced: {}",
            r.skeptic_idx,
            sanitize_evidence(note),
        )
    } else {
        format!("- [skeptic {}] refuted (no evidence)", r.skeptic_idx)
    }
}

/// Refuters ordered high→low confidence (stable within a tier, so
/// skeptic index breaks ties).
fn refuters_by_confidence(results: &[SkepticResult]) -> Vec<&SkepticResult> {
    let mut refuters: Vec<&SkepticResult> = results.iter().filter(|r| r.refuted).collect();
    refuters.sort_by_key(|r| r.confidence.rank());
    refuters
}

/// Build the inlined gaps summary for the rejection nudge: one bullet
/// per refuting skeptic, ordered high→low confidence. Bounded by the
/// panel size. Empty only for a no-refuter panel — unreachable on the
/// panel-reject path (`achieved == false` implies a refute majority).
fn build_gaps_summary(results: &[SkepticResult]) -> String {
    refuters_by_confidence(results)
        .into_iter()
        .map(render_refuter_bullet)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Section headers for the auto-pause blocker summary, one per
/// [`SkepticBlocking`] class. `PAUSE_GROUP_FIXABLE` is also reused by
/// the synthetic-sampler cap path in `acp_session`.
pub(crate) const PAUSE_GROUP_FIXABLE: &str = "Model-fixable gaps";
const PAUSE_GROUP_CONTRADICTION: &str = "Contradictions (objective/plan conflict)";
const PAUSE_GROUP_UNVERIFIABLE: &str = "Unverifiable in this environment";

/// Build the user-facing auto-pause summary: refuter bullets grouped by
/// [`SkepticBlocking`] class so a paused goal tells the user which
/// blockers are model-fixable versus contradictions versus
/// environment-unverifiable. Empty groups are omitted; reuses
/// [`render_refuter_bullet`] so sanitization stays single-sourced.
fn build_pause_summary(results: &[SkepticResult]) -> String {
    let refuters = refuters_by_confidence(results);
    [
        (SkepticBlocking::None, PAUSE_GROUP_FIXABLE),
        (SkepticBlocking::Contradiction, PAUSE_GROUP_CONTRADICTION),
        (SkepticBlocking::Unverifiable, PAUSE_GROUP_UNVERIFIABLE),
    ]
    .into_iter()
    .filter_map(|(class, header)| {
        let bullets: Vec<String> = refuters
            .iter()
            .copied()
            .filter(|r| r.blocking == class)
            .map(render_refuter_bullet)
            .collect();
        (!bullets.is_empty()).then(|| format!("{header}:\n{}", bullets.join("\n")))
    })
    .collect::<Vec<_>>()
    .join("\n")
}

/// Normalized fingerprint of a rejection's *raw* gaps, used to detect a
/// stuck loop (identical fingerprint across attempts). Operates on the
/// undecorated evidence — never the rendered `- [skeptic N, conf]`
/// bullets — so identical gaps map to one fingerprint regardless of
/// skeptic ordering/confidence. Uses the deduplicated, sorted,
/// lowercased `path:line` citations; with none present, falls back to
/// the sorted trimmed non-empty lines. Empty input → `""`, which the
/// stall guard treats as "no stable fingerprint".
pub(crate) fn gap_fingerprint(raw_evidence: &[&str]) -> String {
    let normalized: Vec<Cow<'_, str>> = raw_evidence
        .iter()
        .map(|e| normalize_scratch_paths(e))
        .collect();
    let mut tokens: Vec<String> = normalized
        .iter()
        .flat_map(|e| extract_path_line_tokens(e))
        .collect();
    if tokens.is_empty() {
        tokens = normalized
            .iter()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
    }
    tokens.sort();
    tokens.dedup();
    tokens.join("\n")
}

/// Replace scratch/temp-path tokens with `<scratch>`: they embed
/// per-attempt ids, so leaving them in makes an identical gap
/// fingerprint differently every attempt and the stall guard never
/// fires. Borrowed when no scratch token is present (the common case);
/// spacing collapses on the owned path — fine for a comparison-only
/// fingerprint.
fn normalize_scratch_paths(text: &str) -> Cow<'_, str> {
    const SCRATCH_MARKERS: &[&str] = &["/tmp/", "/var/folders/", "/private/tmp/"];
    if !SCRATCH_MARKERS.iter().any(|m| text.contains(m)) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.split_whitespace()
            .map(|tok| {
                if SCRATCH_MARKERS.iter().any(|m| tok.contains(m)) {
                    "<scratch>"
                } else {
                    tok
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Per-refuter fingerprint source: the raw model `evidence`, or the
/// `fallback_note` when a synthetic refute carries no evidence. Keeps
/// repeated infra-failure rejections stable without the bullet decoration.
fn refuter_fingerprint_source(r: &SkepticResult) -> &str {
    if r.evidence.trim().is_empty() {
        r.fallback_note.as_deref().unwrap_or("")
    } else {
        r.evidence.as_str()
    }
}

/// Pull `path:line` citations out of free text, lowercasing the path. A
/// token qualifies when the prefix (before the FIRST colon) looks path-ish
/// (contains `/` or `.`) and the first colon-segment after it is all
/// digits — tolerating the `path:line:col` / trailing-colon forms common
/// in compiler / test-runner output (e.g. `src/foo.rs:12:5: error`).
fn extract_path_line_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace())
        .filter_map(|raw| {
            let word = raw.trim_matches(|c: char| {
                !c.is_ascii_alphanumeric() && !matches!(c, '.' | '/' | '_' | '-' | ':')
            });
            let (path, rest) = word.split_once(':')?;
            let line = rest.split(':').next().unwrap_or_default();
            let path_ok = !path.is_empty() && (path.contains('/') || path.contains('.'));
            let line_ok = !line.is_empty() && line.chars().all(|c| c.is_ascii_digit());
            (path_ok && line_ok).then(|| format!("{}:{line}", path.to_ascii_lowercase()))
        })
        .collect()
}

/// The planner's `## Goal kind` tag (see `goal_planner_prompt.md`). Selects
/// the kind-specific verifier review lens; an unrecognised / absent kind maps
/// to `None` (no lens — the generic adversarial verifier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoalKind {
    Deliverable,
    Analysis,
    Research,
}

/// Parse the `## Goal kind` value from a plan-file body. Reads the first
/// non-empty line after the header; trims backticks/whitespace/emphasis
/// and normalizes space/underscore separators so a near-miss tag
/// (`**code-change**`, `code change`) does not silently drop the lens.
pub(crate) fn parse_goal_kind(plan: &str) -> Option<GoalKind> {
    let mut lines = plan.lines();
    while let Some(line) = lines.next() {
        if !line.trim().eq_ignore_ascii_case("## Goal kind") {
            continue;
        }
        for next in lines.by_ref() {
            let value = next.trim().trim_matches(['`', '*', '_']).trim();
            if value.is_empty() {
                continue;
            }
            let normalized: String = value
                .to_ascii_lowercase()
                .chars()
                .map(|c| if c == ' ' || c == '_' { '-' } else { c })
                .collect();
            return match normalized.as_str() {
                "deliverable" | "code-change" => Some(GoalKind::Deliverable),
                "analysis" => Some(GoalKind::Analysis),
                "research" => Some(GoalKind::Research),
                _ => None,
            };
        }
    }
    None
}

/// Additional evidence checks for goals that create or change a deliverable.
/// Leading `\n` so `{KIND_LENS}` splices as a blank-line-bounded section.
const KIND_LENS_DELIVERABLE: &str = concat!(
    "\n",
    include_str!("templates/goal_verifier_kind_lens_deliverable.md")
);

/// `research` fact-check lens — verify every claim against its cited source.
const KIND_LENS_RESEARCH: &str = concat!(
    "\n",
    include_str!("templates/goal_verifier_kind_lens_research.md")
);

/// `analysis` soundness lens — conclusions must be evidence-grounded and follow.
const KIND_LENS_ANALYSIS: &str = concat!(
    "\n",
    include_str!("templates/goal_verifier_kind_lens_analysis.md")
);

/// The review-lens block for `kind` (empty string for `None` — generic verifier).
fn kind_lens(kind: Option<GoalKind>) -> &'static str {
    match kind {
        Some(GoalKind::Deliverable) => KIND_LENS_DELIVERABLE,
        Some(GoalKind::Research) => KIND_LENS_RESEARCH,
        Some(GoalKind::Analysis) => KIND_LENS_ANALYSIS,
        None => "",
    }
}

/// Delta-focused resume prompt for skeptic 0 when it is RESUMED across
/// attempts (it already carries its prior transcript and the gaps it
/// flagged). It must re-read the changed files (its cached reads are
/// stale after the agent's further edits), confirm each prior gap is
/// genuinely fixed in the CURRENT files with no regression introduced,
/// and emit the same strict verdict-file + terminal-token contract.
const GOAL_VERIFIER_RESUME_PROMPT_TEMPLATE: &str =
    include_str!("templates/goal_verifier_resume_prompt.md");

/// Wrap the evidence packet (OBJECTIVE / CHANGES_FILE / PLAN_FILE /
/// FINAL_RESPONSE) in `template`, substituting the kind-specific review
/// lens into `{KIND_LENS}`, the runner-allocated output paths into
/// `{DETAILS_FILE}` / `{VERDICT_FILE}`, and the per-runner scratch dirs
/// into `{SKEPTIC_SCRATCH}` (this skeptic's own) / `{IMPLEMENTER_SCRATCH}`
/// (the goal-wide implementer dir). Shared by the cold and resume skeptic
/// prompts; only the template differs.
#[allow(clippy::too_many_arguments)]
fn render_verifier_prompt(
    template: &str,
    objective: &str,
    changes_ref: evidence::ChangesRef<'_>,
    changed_files: &[String],
    plan_file: Option<&Path>,
    plan_changes: Option<&str>,
    final_response: &str,
    details_path: &str,
    verdict_path: &str,
    kind_lens: &str,
    skeptic_scratch: &str,
    implementer_scratch: &str,
    prior_gaps: Option<&str>,
    tool_names: &RoleToolNames,
    scratch_ready: bool,
) -> String {
    let user_prompt = evidence::build_classifier_evidence_packet(
        objective,
        changes_ref,
        changed_files,
        plan_file,
        plan_changes,
        final_response,
    );
    let prior_gaps_rendered = match prior_gaps {
        Some(g) if !g.trim().is_empty() => sanitize_prior_gaps(g),
        _ => "(none — first verification round)".to_string(),
    };
    let rendered = template
        .replace("{KIND_LENS}", kind_lens)
        .replace("{DETAILS_FILE}", details_path)
        .replace("{VERDICT_FILE}", verdict_path)
        .replace("{SKEPTIC_SCRATCH}", skeptic_scratch)
        .replace("{IMPLEMENTER_SCRATCH}", implementer_scratch)
        // Only claim the dirs exist when both were actually created.
        .replace(
            "{SCRATCH_STATUS}",
            if scratch_ready {
                "Both dirs have been created for you."
            } else {
                "Create your own scratch dir with `mkdir -p` if it is missing."
            },
        )
        .replace("{PRIOR_GAPS}", &prior_gaps_rendered);
    let rendered = tool_names.apply(&rendered);
    let mut out = String::with_capacity(rendered.len() + user_prompt.len() + 8);
    out.push_str(&rendered);
    out.push_str("\n\n");
    out.push_str(&user_prompt);
    out
}

/// Build the per-skeptic cold user prompt — the full adversarial
/// verifier template plus the evidence packet.
#[allow(clippy::too_many_arguments)]
fn render_skeptic_prompt(
    objective: &str,
    changes_ref: evidence::ChangesRef<'_>,
    changed_files: &[String],
    plan_file: Option<&Path>,
    plan_changes: Option<&str>,
    final_response: &str,
    details_path: &str,
    verdict_path: &str,
    kind_lens: &str,
    skeptic_scratch: &str,
    implementer_scratch: &str,
    prior_gaps: Option<&str>,
    tool_names: &RoleToolNames,
    scratch_ready: bool,
) -> String {
    render_verifier_prompt(
        GOAL_VERIFIER_PROMPT_TEMPLATE,
        objective,
        changes_ref,
        changed_files,
        plan_file,
        plan_changes,
        final_response,
        details_path,
        verdict_path,
        kind_lens,
        skeptic_scratch,
        implementer_scratch,
        prior_gaps,
        tool_names,
        scratch_ready,
    )
}

/// Build the resumed-skeptic-0 delta prompt (see
/// [`GOAL_VERIFIER_RESUME_PROMPT_TEMPLATE`]).
#[allow(clippy::too_many_arguments)]
fn render_skeptic_resume_prompt(
    objective: &str,
    changes_ref: evidence::ChangesRef<'_>,
    changed_files: &[String],
    plan_file: Option<&Path>,
    plan_changes: Option<&str>,
    final_response: &str,
    details_path: &str,
    verdict_path: &str,
    kind_lens: &str,
    skeptic_scratch: &str,
    implementer_scratch: &str,
    prior_gaps: Option<&str>,
    tool_names: &RoleToolNames,
    scratch_ready: bool,
) -> String {
    render_verifier_prompt(
        GOAL_VERIFIER_RESUME_PROMPT_TEMPLATE,
        objective,
        changes_ref,
        changed_files,
        plan_file,
        plan_changes,
        final_response,
        details_path,
        verdict_path,
        kind_lens,
        skeptic_scratch,
        implementer_scratch,
        prior_gaps,
        tool_names,
        scratch_ready,
    )
}

/// Wrap a raw spawn failure / parse failure into a `SkepticResult` with
/// `refuted: true` (fail-closed at the skeptic level). The `note` is
/// surfaced in the aggregated details file so the user can see why this
/// skeptic produced a synthetic refute.
fn skeptic_failure(skeptic_idx: u32, note: String, latency_ms: u64) -> SkepticResult {
    SkepticResult {
        skeptic_idx,
        refuted: true,
        confidence: SkepticConfidence::Unknown,
        blocking: SkepticBlocking::None,
        evidence: String::new(),
        findings: Vec::new(),
        fallback_note: Some(note),
        latency_ms,
    }
}

/// Read skeptic `skeptic_idx`'s verdict after its terminal response.
/// The JSON verdict file is authoritative; the terminal token is a
/// secondary signal used only when the JSON is missing/malformed.
async fn read_skeptic_verdict(
    skeptic_idx: u32,
    details_raw: &str,
    verdict_raw: &str,
    terminal: &str,
    started: std::time::Instant,
) -> SkepticResult {
    let json_body = tokio::fs::read_to_string(verdict_raw).await.ok();
    if let Some(body) = json_body.as_deref()
        && let Some(SkepticVerdict {
            refuted,
            evidence,
            confidence,
            blocking,
            details_md: parsed_md,
            findings,
        }) = parse_verdict_json(body)
    {
        // Keep the referenced per-skeptic file non-empty: if the skeptic
        // produced a verdict but never wrote its report, persist the JSON
        // `details_md` fallback to the path the aggregate references.
        let file_empty = tokio::fs::read_to_string(details_raw)
            .await
            .map(|s| s.trim().is_empty())
            .unwrap_or(true);
        if file_empty && !parsed_md.trim().is_empty() {
            let _ = tokio::fs::write(details_raw, &parsed_md).await;
        }
        return SkepticResult {
            skeptic_idx,
            refuted,
            confidence,
            blocking,
            evidence,
            findings,
            fallback_note: None,
            latency_ms: started.elapsed().as_millis() as u64,
        };
    }

    // JSON missing / malformed — fall back to the terminal token.
    match parse_skeptic_terminal_response(terminal) {
        Some(refuted) => SkepticResult {
            skeptic_idx,
            refuted,
            confidence: SkepticConfidence::Unknown,
            blocking: SkepticBlocking::None,
            evidence: String::new(),
            findings: Vec::new(),
            fallback_note: Some("verdict JSON missing/malformed; used terminal token".into()),
            latency_ms: started.elapsed().as_millis() as u64,
        },
        None => skeptic_failure(
            skeptic_idx,
            format!(
                "verdict JSON missing/malformed AND terminal token unrecognised: {}",
                terminal.chars().take(120).collect::<String>()
            ),
            started.elapsed().as_millis() as u64,
        ),
    }
}

/// Spawn one skeptic under `spawn_id`, wait for its terminal response,
/// and read the JSON verdict file. Pure per-skeptic; no telemetry
/// side-effects so the orchestrator owns event emission for both the
/// happy and failure paths uniformly.
///
/// `resume_from` (skeptic 0 on attempt > 1) renders the delta resume
/// prompt and resumes the prior child session. If that spawn fails (e.g.
/// the prior session no longer exists after a restart) it falls back to
/// a cold spawn under the same `spawn_id` so verification still runs.
async fn run_one_skeptic(
    spawner: &Arc<dyn GoalClassifierSpawner>,
    skeptic_idx: u32,
    inputs: &SkepticInputs<'_>,
    spawn_id: &str,
    resume_from: Option<&str>,
    tool_names: &RoleToolNames,
    inherit_tool_names: &RoleToolNames,
) -> SkepticResult {
    let started = std::time::Instant::now();
    // An unsecurable (squatted) root makes every artifact path
    // untrustworthy — fail closed, like the unsafe-path arm below.
    if let Err(err) = super::goal_tracker::ensure_goal_scratch_root(inputs.verifier_id) {
        return skeptic_failure(
            skeptic_idx,
            format!("internal: could not secure the goal scratch root: {err}"),
            started.elapsed().as_millis() as u64,
        );
    }
    // This skeptic's own private scratch dir; created lazily here (the
    // implementer dir is created at goal setup). `{SCRATCH}` in the
    // re-run plan resolves to this so N skeptics never collide.
    let skeptic_scratch = super::goal_tracker::skeptic_scratch_dir(inputs.verifier_id, skeptic_idx);
    let skeptic_scratch_ready = tokio::fs::create_dir_all(&skeptic_scratch).await.is_ok();
    // Readiness for the verifier prompt = the implementer dir (from the
    // orchestration) AND this skeptic's own subdir both exist on disk.
    let scratch_ready = inputs.scratch_dir_ready && skeptic_scratch_ready;
    let skeptic_scratch = skeptic_scratch.to_string_lossy();
    let details_raw = format_verifier_details_path(inputs.verifier_id, inputs.attempt, skeptic_idx);
    let verdict_raw = format_verdict_path(inputs.verifier_id, inputs.attempt, skeptic_idx);
    if validate_details_path(Path::new(&details_raw)).is_err()
        || validate_details_path(Path::new(&verdict_raw)).is_err()
    {
        return skeptic_failure(
            skeptic_idx,
            "internal: unsafe per-skeptic file path".to_string(),
            started.elapsed().as_millis() as u64,
        );
    }

    // Resume attempt: a delta re-check of the prior gaps. A spawn error
    // here (stale/missing prior session) is non-fatal — fall through to
    // the cold spawn below.
    if let Some(prior) = resume_from {
        // Render once per toolset: `primary` for the skeptic's resolved
        // toolset, `fallback` for the default/parent toolset the explicit-pair
        // retry falls back to (so the retried prompt names the right tools).
        let render = |tn: &RoleToolNames| {
            render_skeptic_resume_prompt(
                inputs.objective,
                inputs.changes_ref,
                inputs.changed_files,
                inputs.plan_file,
                inputs.plan_changes,
                inputs.final_response,
                &details_raw,
                &verdict_raw,
                inputs.kind_lens,
                &skeptic_scratch,
                inputs.implementer_scratch,
                inputs.prior_gaps,
                tn,
                scratch_ready,
            )
        };
        let prompt = RoleRenderedPrompt {
            primary: render(tool_names),
            fallback: render(inherit_tool_names),
        };
        match spawner
            .spawn_classifier(
                spawn_id,
                skeptic_idx,
                prompt,
                Path::new(&details_raw),
                Some(prior),
            )
            .await
        {
            Ok(terminal) => {
                return read_skeptic_verdict(
                    skeptic_idx,
                    &details_raw,
                    &verdict_raw,
                    &terminal,
                    started,
                )
                .await;
            }
            Err(err) => {
                tracing::info!(
                    skeptic_idx,
                    %err,
                    "skeptic-0 resume spawn failed; falling back to a cold spawn",
                );
            }
        }
    }

    // Cold spawn: attempt 1, every idx >= 1, or a resume fallback.
    let render = |tn: &RoleToolNames| {
        render_skeptic_prompt(
            inputs.objective,
            inputs.changes_ref,
            inputs.changed_files,
            inputs.plan_file,
            inputs.plan_changes,
            inputs.final_response,
            &details_raw,
            &verdict_raw,
            inputs.kind_lens,
            &skeptic_scratch,
            inputs.implementer_scratch,
            inputs.prior_gaps,
            tn,
            scratch_ready,
        )
    };
    let prompt = RoleRenderedPrompt {
        primary: render(tool_names),
        fallback: render(inherit_tool_names),
    };
    match spawner
        .spawn_classifier(spawn_id, skeptic_idx, prompt, Path::new(&details_raw), None)
        .await
    {
        Ok(terminal) => {
            read_skeptic_verdict(skeptic_idx, &details_raw, &verdict_raw, &terminal, started).await
        }
        Err(SpawnError::Transport(d)) => skeptic_failure(
            skeptic_idx,
            format!("transport error: {d}"),
            started.elapsed().as_millis() as u64,
        ),
        Err(SpawnError::Runtime { message, cancelled }) => skeptic_failure(
            skeptic_idx,
            format!("runtime error (cancelled={cancelled}): {message}"),
            started.elapsed().as_millis() as u64,
        ),
    }
}

/// Shared per-skeptic inputs. Borrowed from the verification-stage
/// driver so each spawned skeptic shares the same evidence references.
struct SkepticInputs<'a> {
    objective: &'a str,
    final_response: &'a str,
    plan_file: Option<&'a Path>,
    /// Borrowed baseline→current plan diff, computed ONCE in
    /// [`run_verification_stage`] and shared by every skeptic (no per-skeptic
    /// clone). `None` renders the `PLAN_CHANGES: (none)` sentinel.
    plan_changes: Option<&'a str>,
    changes_ref: evidence::ChangesRef<'a>,
    changed_files: &'a [String],
    verifier_id: &'a str,
    attempt: u32,
    /// Kind-specific review lens (`kind_lens`), shared by every skeptic so the
    /// panel applies one consistent lens. Empty when the goal kind is absent.
    kind_lens: &'a str,
    /// The goal-wide implementer scratch dir as a string. Computed ONCE in
    /// [`run_verification_stage`] and shared by every skeptic (no per-skeptic
    /// clone); each skeptic derives its OWN dir from `verifier_id` instead.
    implementer_scratch: &'a str,
    /// Whether the implementer scratch dir was actually created (from the
    /// orchestration); combined with the skeptic's own subdir in `run_one_skeptic`.
    scratch_dir_ready: bool,
    /// Previous round's gaps summary for the `{PRIOR_GAPS}` placeholder
    /// (see [`VerificationStageInputs::prior_gaps`]).
    prior_gaps: Option<&'a str>,
}

/// Stage-level inputs threaded into [`run_verification_stage`]. Borrowed
/// throughout so the orchestrator stays pure and the test driver can
/// stamp fresh inputs per attempt without cloning.
pub(crate) struct VerificationStageInputs<'a> {
    pub objective: &'a str,
    pub final_response: &'a str,
    pub baseline_commit: Option<&'a str>,
    pub workspace_root: &'a Path,
    pub verifier_id: &'a str,
    pub attempt: u32,
    pub model_id: &'a str,
    pub goal_created_at: i64,
    pub plan_file: Option<&'a Path>,
    /// Path to the immutable baseline snapshot of the planner's original
    /// plan (`GoalOrchestration::plan_baseline_file`). The stage diffs the
    /// CURRENT `plan_file` against it to surface mid-run plan edits to the
    /// skeptics; `None` when no baseline was captured (planner-off goals or a
    /// snapshot failure).
    pub plan_baseline_file: Option<&'a Path>,
    /// The goal-wide implementer scratch dir
    /// ([`super::goal_tracker::implementer_scratch_dir`]). Threaded into
    /// every skeptic prompt so the panel knows where the implementer wrote
    /// its build outputs / screenshots and can READ them to verify.
    pub implementer_scratch_dir: &'a Path,
    /// Whether that implementer dir was actually created (from the goal
    /// orchestration), so the verifier prompt only claims it exists when true.
    pub scratch_dir_ready: bool,
    pub skeptic_count: u32,
    /// Effective per-goal classifier cap (resolved env > remote > default), so
    /// `GoalClassifierFired` reports the real cap, not the default constant.
    pub max_runs: u32,
    /// Child session id of skeptic 0 from the goal's previous attempt, if
    /// any. When present and N > 1 the stage resumes it (delta re-check)
    /// — including the first attempt after a user pause/resume, which
    /// resets the attempt counter but preserves the gatekeeper. `None`
    /// before the first panel, after a snapshot restore that lost the
    /// session, or whenever N == 1 (the sole judge never resumes).
    pub prior_skeptic0_session_id: Option<&'a str>,
    /// Previous round's gaps summary (`last_classifier_gaps`), threaded into
    /// every skeptic prompt as `{PRIOR_GAPS}` so cold skeptics keep
    /// cross-round memory instead of ratcheting the bar with fresh
    /// objections each attempt. `None` on the first round.
    pub prior_gaps: Option<&'a str>,
    /// Per-skeptic-index resolved tool names for the verifier prompt
    /// placeholders, indexed by skeptic index. Built parent-side from
    /// each index's resolved toolset (explicit pair ⇒ its `describe` summary;
    /// inherit ⇒ the parent bridge). An index past the slice end (e.g. an
    /// empty slice in tests) falls back to [`RoleToolNames::inherit_defaults`].
    pub tool_names: &'a [RoleToolNames],
    /// Default/parent-toolset tool names used to render each skeptic's
    /// fail-open RETRY prompt (the retry falls back to the default toolset, so
    /// it must name THAT toolset's tools). Shared across the panel.
    pub inherit_tool_names: &'a RoleToolNames,
}

/// Outcome of [`run_verification_stage`] plus skeptic 0's child session
/// id when an N > 1 panel ran, so the next attempt can resume it. `None`
/// for the N == 1 sole-judge panel and the fail-open early-exits —
/// neither resumes.
pub(crate) struct VerificationStageResult {
    pub outcome: GoalClassifierOutcome,
    pub skeptic0_session_id: Option<String>,
    /// `true` only when the skeptic panel actually ran: the apply path
    /// keys the stored `skeptic0_session_id` overwrite on this so a
    /// fail-open early-exit cannot sever the gatekeeper resume chain
    /// (an N == 1 run still clears the id deliberately).
    pub panel_ran: bool,
}

impl From<GoalClassifierOutcome> for VerificationStageResult {
    /// Fail-open / early-exit conversion: no panel ran.
    fn from(outcome: GoalClassifierOutcome) -> Self {
        Self {
            outcome,
            skeptic0_session_id: None,
            panel_ran: false,
        }
    }
}

/// Run the verification stage: the adversarial skeptic panel of
/// `skeptic_count` spawns. Skeptic 0 runs first (and is resumed across
/// attempts when N > 1); approval needs the cold-panel quorum (see
/// [`aggregate_skeptic_verdicts`]).
///
/// Always emits a `GoalClassifierFired` for dashboard symmetry with the
/// legacy single classifier, then `GoalVerifierSkepticVerdict` per skeptic
/// plus an aggregate `GoalVerifierAggregateVerdict` and a final
/// `GoalClassifierVerdict`. The terminal outcome is one of `Achieved`,
/// `NotAchieved`, `Blocked`, `FailOpenAchieved` — same enum the drain
/// path already consumes.
///
/// ## Cancellation
///
/// Verification runs inside the turn's `handle_prompt` (the abortable
/// running task), so a turn-cancel (`Cmd+C`) drops this future. Merely
/// dropping it does NOT notify the coordinator (it does not poll
/// `result_tx.is_closed()`), so the spawned skeptics are reaped instead
/// via the parent-prompt-id match: the `ChannelSpawner` tags each skeptic
/// `SubagentRequest` with the live `current_prompt_id`, and
/// `cancel_running_turn_subagents` → `cancel_by_parent_prompt_id` fires
/// each child's cancel token on a turn-cancel. The cancel handler also
/// pauses the goal (`UserPaused`), so a cancelled verification leaves no
/// partial verdict and the user resumes with `/goal resume`.
#[allow(clippy::too_many_lines)]
pub(crate) async fn run_verification_stage(
    spawner: Arc<dyn GoalClassifierSpawner>,
    inputs: VerificationStageInputs<'_>,
    emit_event: &dyn Fn(Event),
) -> VerificationStageResult {
    let started = std::time::Instant::now();
    emit_event(Event::GoalClassifierFired {
        attempt: inputs.attempt,
        max_runs: inputs.max_runs,
        model_id: inputs.model_id.to_string(),
    });

    let details_raw = format_details_path(inputs.verifier_id, inputs.attempt);
    let details_path = PathBuf::from(&details_raw);
    let changes_raw = format_changes_path(inputs.verifier_id, inputs.attempt);
    let changes_path = PathBuf::from(&changes_raw);

    if let Err(err) = validate_details_path(&details_path) {
        tracing::warn!(
            details_path = %details_raw,
            error = %err,
            "verification stage: rejecting unsafe details path; failing open",
        );
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            None,
            String::new(),
        )
        .await
        .into();
    }
    // Re-ensure the scratch root (it can be missing after a restart),
    // BEFORE the changes-path validation: that arm's fail-open writes a
    // placeholder, which must never happen under an unverified root.
    if let Err(err) = super::goal_tracker::ensure_goal_scratch_root(inputs.verifier_id) {
        tracing::warn!(
            error = %err,
            "verification stage: failed to ensure scratch root; failing open",
        );
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            None,
            String::new(),
        )
        .await
        .into();
    }
    if let Err(err) = validate_details_path(&changes_path) {
        tracing::warn!(
            changes_path = %changes_raw,
            error = %err,
            "verification stage: rejecting unsafe changes path; failing open",
        );
        return record_fail_open(
            GoalClassifierFailOpenReason::FileWriteFailed,
            inputs.attempt,
            started,
            emit_event,
            Some(&details_path),
            details_raw,
        )
        .await
        .into();
    }

    // Capture the diff ONCE; all skeptics read the same patch file.
    // `changed_files` comes from the FULL pre-truncation diff (plus
    // untracked files) so the list stays complete even when the patch
    // body is byte-capped.
    let mut changed_files: Vec<String> = Vec::new();
    let changes_written = match evidence::capture_changes_diff(
        inputs.baseline_commit,
        inputs.workspace_root,
        inputs.goal_created_at,
    )
    .await
    {
        Ok(captured) => {
            changed_files = captured.changed_files;
            match write_patch_file_atomic(&changes_path, &captured.diff).await {
                Ok(()) => true,
                Err(err) => {
                    tracing::warn!(
                        changes_path = %changes_raw,
                        error = %err,
                        "verification stage: failed to write patch file; failing open",
                    );
                    return record_fail_open(
                        GoalClassifierFailOpenReason::FileWriteFailed,
                        inputs.attempt,
                        started,
                        emit_event,
                        Some(&details_path),
                        details_raw,
                    )
                    .await
                    .into();
                }
            }
        }
        Err(err) => {
            tracing::info!(
                error = %err,
                "verification stage: changes-capture failed; rendering CHANGES_FILE as (unavailable)",
            );
            false
        }
    };
    let sanitized = evidence::sanitize_final_response(inputs.final_response);
    let changes_ref = if changes_written {
        evidence::ChangesRef::File(&changes_raw)
    } else {
        evidence::ChangesRef::Unavailable
    };

    // Compute the plan baseline→current diff ONCE; every skeptic shares the
    // same borrowed `&str` (no per-skeptic clone). The plan is agent-authored
    // text, so sanitize it for control tokens exactly like FINAL_RESPONSE.
    let plan_changes_raw = match (inputs.plan_baseline_file, inputs.plan_file) {
        (Some(baseline), Some(current)) => evidence::capture_plan_changes(baseline, current).await,
        _ => None,
    };
    let plan_changes_sanitized = plan_changes_raw
        .as_deref()
        .map(evidence::sanitize_final_response);

    // Select the shared review lens from the plan's `## Goal kind`. Best-effort:
    // an unreadable or untagged plan yields the generic verifier (empty lens).
    let goal_kind = match inputs.plan_file {
        Some(path) => tokio::fs::read_to_string(path)
            .await
            .ok()
            .and_then(|body| parse_goal_kind(&body)),
        None => None,
    };
    let kind_lens = kind_lens(goal_kind);

    let implementer_scratch = inputs.implementer_scratch_dir.to_string_lossy();

    let n = inputs
        .skeptic_count
        .clamp(GOAL_VERIFIER_SKEPTIC_MIN, GOAL_VERIFIER_SKEPTIC_MAX);
    // Per-index tool names for the prompt placeholders; an index past the
    // provided slice (e.g. an empty slice in tests) falls back to the
    // parent-toolset defaults so the prompt still renders fully.
    let default_tool_names = RoleToolNames::inherit_defaults();
    let tool_names_for = |idx: u32| -> &RoleToolNames {
        inputs
            .tool_names
            .get(idx as usize)
            .unwrap_or(&default_tool_names)
    };
    let skeptic_inputs = SkepticInputs {
        objective: inputs.objective,
        final_response: sanitized.as_ref(),
        plan_file: inputs.plan_file,
        plan_changes: plan_changes_sanitized.as_deref(),
        changes_ref,
        changed_files: &changed_files,
        verifier_id: inputs.verifier_id,
        attempt: inputs.attempt,
        kind_lens,
        implementer_scratch: implementer_scratch.as_ref(),
        scratch_dir_ready: inputs.scratch_dir_ready,
        prior_gaps: inputs.prior_gaps,
    };

    // Escalating panel: when N > 1, run skeptic 0 alone first. A
    // refuted+high skeptic 0 is DECISIVE — it can never yield Achieved.
    // An ordinary (NON-blocking) decisive refute short-circuits, skipping
    // the remaining N-1 spawns. A blocking (contradiction / unverifiable)
    // decisive refute instead fans out the full panel so the panel can
    // corroborate whether it is truly all-blocking (`Blocked`, needs-user)
    // or there is also a fixable gap (`NotAchieved`) — but skeptic 0's
    // refute still binds the outcome away from Achieved (see
    // `decisive_refute`). Any other skeptic-0 outcome (not-refuted, or
    // refuted with medium/low confidence) also fans out, and approval
    // then requires the full-panel quorum in `aggregate_skeptic_verdicts`.
    // Skeptic 0 is the persistent reject-gatekeeper: for N > 1 it follows
    // the goal across attempts and is resumed (delta re-check) whenever
    // the prior child id survives — including the first attempt after a
    // user pause/resume reset the attempt counter. The fresh `skeptic0_id` is
    // returned out of the stage so the apply path persists it for the next
    // attempt. N == 1 keeps skeptic 0 cold each attempt (a resumed sole
    // judge would be the biased approver we avoid), so it never resumes
    // and returns `None`.
    let (results, decisive_refute, skeptic0_session_id): (
        Vec<SkepticResult>,
        bool,
        Option<String>,
    ) = if n > 1 {
        let skeptic0_id = uuid::Uuid::now_v7().to_string();
        // Gate purely on a surviving prior id: a user pause/resume resets
        // `classifier_runs_attempted` (so attempt restarts at 1) while
        // preserving `skeptic0_session_id`, and the gatekeeper must still
        // resume in that case.
        let resume_from = inputs.prior_skeptic0_session_id;
        let first = run_one_skeptic(
            &spawner,
            0,
            &skeptic_inputs,
            &skeptic0_id,
            resume_from,
            tool_names_for(0),
            inputs.inherit_tool_names,
        )
        .await;
        let high_refute = first.refuted && first.confidence == SkepticConfidence::High;
        if high_refute && !first.blocking.is_blocking() {
            (vec![first], true, Some(skeptic0_id))
        } else {
            // `high_refute` here ⇒ skeptic 0 was blocking (the non-blocking
            // case short-circuited above), so its refute remains binding.
            let cold_ids: Vec<String> = (1..n).map(|_| uuid::Uuid::now_v7().to_string()).collect();
            let rest = (1..n).zip(&cold_ids).map(|(idx, id)| {
                run_one_skeptic(
                    &spawner,
                    idx,
                    &skeptic_inputs,
                    id.as_str(),
                    None,
                    tool_names_for(idx),
                    inputs.inherit_tool_names,
                )
            });
            let mut all = Vec::with_capacity(n as usize);
            all.push(first);
            all.extend(futures::future::join_all(rest).await);
            (all, high_refute, Some(skeptic0_id))
        }
    } else {
        let cold_ids: Vec<String> = (0..n).map(|_| uuid::Uuid::now_v7().to_string()).collect();
        let spawns = (0..n).zip(&cold_ids).map(|(idx, id)| {
            run_one_skeptic(
                &spawner,
                idx,
                &skeptic_inputs,
                id.as_str(),
                None,
                tool_names_for(idx),
                inputs.inherit_tool_names,
            )
        });
        (futures::future::join_all(spawns).await, false, None)
    };

    for r in &results {
        emit_event(Event::GoalVerifierSkepticVerdict {
            attempt: inputs.attempt,
            skeptic_idx: r.skeptic_idx,
            refuted: r.refuted,
            confidence: r.confidence.as_const_str(),
            latency_ms: r.latency_ms,
        });
    }
    let (refuted_count, total, quorum_achieved) = aggregate_skeptic_verdicts(&results);
    // A decisive skeptic-0 refute overrides the quorum: a refuted+high
    // skeptic 0 can never approve, even when the blocking fan-out ran the
    // full panel (the fan-out only chooses Blocked vs NotAchieved).
    let achieved = quorum_achieved && !decisive_refute;
    emit_event(Event::GoalVerifierAggregateVerdict {
        attempt: inputs.attempt,
        refuted_count,
        total,
        achieved,
    });

    let body = render_skeptic_panel_details(
        &results,
        refuted_count,
        total,
        achieved,
        inputs.verifier_id,
        inputs.attempt,
    );
    write_details_file(&details_path, &body).await;

    let latency_ms = started.elapsed().as_millis() as u64;
    let verdict = if achieved {
        GoalClassifierVerdict::Achieved
    } else {
        GoalClassifierVerdict::NotAchieved
    };
    emit_event(Event::GoalClassifierVerdict {
        verdict: verdict.into(),
        attempt: inputs.attempt,
        latency_ms,
    });

    if achieved {
        return VerificationStageResult {
            outcome: GoalClassifierOutcome::Achieved {
                details_path: details_raw,
            },
            skeptic0_session_id,
            panel_ran: true,
        };
    }

    // Route to Blocked only when EVERY refuter is a non-model-fixable
    // blocker (contradiction / unverifiable); a single fixable gap means
    // the loop can still make progress, so it stays NotAchieved.
    //
    // A lone blocking refuter (peers not refuting) is enough to route here
    // by design: Blocked is a fail-safe, resume-recoverable PAUSE, never an
    // approval — `decisive_refute` already forced not-achieved above, so
    // the only question is nudge-and-retry vs ask-the-user. With no fixable
    // gap to retry on, a high-confidence `unverifiable`/`contradiction`
    // legitimately needs a user decision; over-pausing is cheaply undone by
    // a resume, whereas nudging a model against an unfixable blocker is not.
    let all_blocking = results.iter().any(|r| r.refuted)
        && results
            .iter()
            .filter(|r| r.refuted)
            .all(|r| r.blocking.is_blocking());
    let outcome = if all_blocking {
        GoalClassifierOutcome::Blocked {
            details_path: details_raw,
            pause_summary: build_pause_summary(&results),
        }
    } else {
        let gap_fingerprint = gap_fingerprint(
            &results
                .iter()
                .filter(|r| r.refuted)
                .map(refuter_fingerprint_source)
                .collect::<Vec<_>>(),
        );
        GoalClassifierOutcome::NotAchieved {
            details_path: details_raw,
            gaps_summary: build_gaps_summary(&results),
            pause_summary: build_pause_summary(&results),
            gap_fingerprint,
        }
    };
    VerificationStageResult {
        outcome,
        skeptic0_session_id,
        panel_ran: true,
    }
}

/// Render the aggregated details file the rejection directive points the
/// model at: the headline, the concise `## Gaps to fix` checklist, and a
/// reference line listing the per-skeptic report paths (their full reasoning
/// stays in those files, not embedded here). Capped at
/// [`GOAL_VERIFIER_PANEL_MAX_BYTES`].
fn render_skeptic_panel_details(
    results: &[SkepticResult],
    refuted_count: u32,
    total: u32,
    achieved: bool,
    verifier_id: &str,
    attempt: u32,
) -> String {
    let headline = if achieved {
        format!(
            "# Goal verification — Achieved\n\n\
             {refuted_count} of {total} skeptics refuted; survives the panel.\n\n"
        )
    } else {
        format!(
            "# Goal verification — Not Achieved\n\n\
             {refuted_count} of {total} skeptics refuted; panel rejected the claim.\n\n"
        )
    };

    // Per-skeptic report paths (full reasoning lives in these files, each
    // written by its skeptic). Deterministic from (verifier_id, attempt,
    // idx); sorted by idx for a stable listing.
    let mut by_idx: Vec<&SkepticResult> = results.iter().collect();
    by_idx.sort_by_key(|r| r.skeptic_idx);
    let paths: Vec<String> = by_idx
        .iter()
        .map(|r| format_verifier_details_path(verifier_id, attempt, r.skeptic_idx))
        .collect();

    let mut out = String::with_capacity(headline.len() + 1024);
    out.push_str(&headline);
    if !achieved {
        let gaps = build_gaps_summary(results);
        if !gaps.is_empty() {
            out.push_str("## Gaps to fix\n\n");
            out.push_str(&gaps);
            out.push_str("\n\n");
        }
    }
    if !paths.is_empty() {
        if achieved {
            out.push_str("Per-skeptic reports: ");
        } else {
            out.push_str(
                "Fix the gaps above — they are what matters. For the full reasoning \
                 behind each, open the per-skeptic report files: ",
            );
        }
        out.push_str(&paths.join(", "));
        out.push('\n');
    }
    cap_panel_details(out)
}

/// Truncate the rendered panel to [`GOAL_VERIFIER_PANEL_MAX_BYTES`] at a
/// UTF-8 boundary, appending an explicit elision marker. Overall cap
/// only — never per-line — mirroring `evidence::truncate_diff`.
fn cap_panel_details(body: String) -> String {
    if body.len() <= GOAL_VERIFIER_PANEL_MAX_BYTES {
        return body;
    }
    let mut cut = GOAL_VERIFIER_PANEL_MAX_BYTES;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    // Count from the post-walk cut so the marker reports the exact
    // elided byte count, not the pre-boundary-walk approximation.
    let elided = body.len() - cut;
    let mut out = String::with_capacity(cut + 64);
    out.push_str(&body[..cut]);
    out.push_str(&format!(
        "\n... (panel details truncated, {elided} bytes elided) ...\n"
    ));
    out
}

async fn write_details_file(path: &Path, body: &str) {
    if let Err(err) = tokio::fs::write(path, body).await {
        tracing::warn!(
            path = %path.display(),
            error = %err,
            "verification stage: failed to write details file",
        );
    }
}

// Test helpers (shared between this module's tests and acp_session's
// drain-path tests; gated by `#[cfg(test)]` so prod builds don't carry
// them).

/// Pull the runner-allocated `{VERDICT_FILE}` path out of a rendered
/// verifier prompt. `None` if the prompt doesn't contain one (e.g. a
/// non-verifier mock). Shared by `goal_classifier::tests::MockSpawner`
/// and `acp_session::goal_classifier_e2e_tests::MockCoordinator`.
#[cfg(test)]
pub(crate) fn parse_verdict_path_from_prompt(prompt: &str) -> Option<String> {
    parse_prompt_path(prompt, "goal-verdict-", ".json")
}

/// Extract an absolute artifact path from a rendered prompt: the files
/// live under the per-goal scratch root (an arbitrary temp-dir path),
/// so anchor on a stable file-name `marker`, walk back to the start of
/// the whitespace/backtick-delimited token, and end at `suffix`.
#[cfg(test)]
fn parse_prompt_path(prompt: &str, marker: &str, suffix: &str) -> Option<String> {
    let marker = prompt.find(marker)?;
    let start = prompt[..marker]
        .rfind(|c: char| c.is_whitespace() || c == '`')
        .map_or(0, |i| i + 1);
    let tail = &prompt[start..];
    let end = tail.find(suffix)?;
    Some(tail[..end + suffix.len()].to_string())
}

// Tests

#[cfg(test)]
#[path = "goal_classifier_tests.rs"]
mod tests;
