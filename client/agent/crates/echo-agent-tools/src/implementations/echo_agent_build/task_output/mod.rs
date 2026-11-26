//! `get_task_output` tool — output/status for one or many background tasks.
//!
//! Positive `timeout_ms` waits (multi-id = wait-all). Also provides helpers used
//! by the legacy `wait_tasks` tool.

pub mod terminal_command;
pub mod wait_tasks;
use std::time::Duration;
pub use terminal_command::GetTerminalCommandOutputTool;
pub use wait_tasks::WaitTasksTool;

use crate::DEFAULT_TOOL_OUTPUT_BYTES;
use crate::implementations::BashTool;
use crate::implementations::echo_agent_build::task::TaskTool;
use crate::implementations::echo_agent_build::task::backend::SubagentBackendResource;
use crate::implementations::echo_agent_build::task::types::{
    SubagentSnapshot, SubagentSnapshotStatus,
};
use crate::implementations::echo_agent_build_concise::BashConciseTool;
use crate::implementations::opencode::OpenCodeBashTool;
use crate::implementations::task_output::tool::snapshot_to_result;
use crate::types::requirements::{Expr, ToolParamsRequirement, ToolRequirement};
use crate::types::resources::{SharedResources, Terminal, TruncationCfg};
use crate::types::template_renderer::TemplateRenderer;
use crate::types::tool::{ToolKind, ToolNamespace};
use echo_agent_tool_types::{
    MultiTaskOutputResult, TaskOutputOutput, TaskOutputResult, TaskOutputToolInput,
};

/// Default wait budget when a caller is already in wait mode but omitted
/// `timeout_ms` (legacy `wait_tasks` / internal `capped_wait_timeout`). On
/// `get_task_output`, omitting `timeout_ms` is a non-blocking snapshot — this
/// constant is not applied unless a wait is active.
pub(crate) const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// The blocking-wait ceiling: `ECHO_AGENT_MAX_WAIT_BLOCK_MS`, else 10 min.
///
/// The same value fills `{max_wait_ms}` in the descriptions, so a wait can
/// never exceed what the model was told it may ask for.
pub(crate) fn max_wait_block() -> Duration {
    Duration::from_millis(echo_agent_tool_types::max_wait_block_ms())
}

/// Resolve a model-supplied `timeout_ms` into the effective blocking-wait
/// duration: default when omitted, then clamped to `cap` so a single wait call
/// can never wedge the turn for longer than the ceiling.
pub(crate) fn capped_wait_timeout(timeout_ms: Option<u64>, cap: Duration) -> Duration {
    let base = timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_WAIT_TIMEOUT);
    base.min(cap)
}

/// The caller's requested wait before capping, or the default when omitted.
fn requested_wait_timeout(timeout_ms: Option<u64>) -> Duration {
    timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_WAIT_TIMEOUT)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitHint {
    NotRequested,
    Elapsed {
        requested: Duration,
        waited: Duration,
    },
    ReturnedEarly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitSubject {
    Task,
    Subagent,
}

impl WaitSubject {
    fn noun(self) -> &'static str {
        match self {
            WaitSubject::Task => "task",
            WaitSubject::Subagent => "subagent",
        }
    }
}

fn still_running_wait_hint(hint: WaitHint, subject: WaitSubject) -> String {
    let noun = subject.noun();
    let lead = match hint {
        WaitHint::Elapsed { requested, waited } => {
            let waited_label = format_waited_duration(waited);
            if requested > waited {
                let requested_label = format_waited_duration(requested);
                format!(
                    "Waited {waited_label}, the per-call maximum, of the {requested_label} you requested; \
                     the {noun} is still running. You do not need to call this again."
                )
            } else {
                format!("Waited the requested {waited_label}; the {noun} is still running.")
            }
        }
        WaitHint::ReturnedEarly => {
            format!("Wait returned early because another finished; this {noun} is still running.")
        }
        WaitHint::NotRequested => "Use timeout_ms to wait for completion.".to_string(),
    };
    format!("{lead} You will be notified automatically when the {noun} completes.")
}

fn format_waited_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{}s", ms / 1000)
    }
}

fn with_still_running_wait_hint(body: String, hint: WaitHint, subject: WaitSubject) -> String {
    format!("{body}\n\n{}", still_running_wait_hint(hint, subject))
}

fn apply_running_wait_hint(
    mut result: TaskOutputResult,
    hint: WaitHint,
    subject: WaitSubject,
) -> TaskOutputResult {
    if result.status == "running" {
        result.output =
            with_still_running_wait_hint(std::mem::take(&mut result.output), hint, subject);
    }
    result
}

pub(crate) fn background_bash_requires_exprs() -> Vec<Expr<ToolRequirement>> {
    use crate::types::tool_metadata::ToolMetadata;
    let echo_agent_build_bash = Expr::Value(ToolRequirement::Tool {
        namespace: ToolMetadata::tool_namespace(&BashTool).to_string(),
        id: echo_agent_tool_runtime::Tool::id(&BashTool)
            .as_str()
            .to_string(),
        if_params: Some(Expr::Value(ToolParamsRequirement {
            key: "enabled_background".to_string(),
            value: Expr::Value(serde_json::Value::Bool(true)),
        })),
    });
    let echo_agent_build_concise_bash = Expr::Value(ToolRequirement::Tool {
        namespace: ToolMetadata::tool_namespace(&BashConciseTool).to_string(),
        id: echo_agent_tool_runtime::Tool::id(&BashConciseTool)
            .as_str()
            .to_string(),
        if_params: Some(Expr::Value(ToolParamsRequirement {
            key: "enabled_background".to_string(),
            value: Expr::Value(serde_json::Value::Bool(true)),
        })),
    });
    let opencode_bash = Expr::Value(ToolRequirement::Tool {
        namespace: ToolMetadata::tool_namespace(&OpenCodeBashTool).to_string(),
        id: echo_agent_tool_runtime::Tool::id(&OpenCodeBashTool)
            .as_str()
            .to_string(),
        if_params: None,
    });
    vec![
        echo_agent_build_bash,
        echo_agent_build_concise_bash,
        opencode_bash,
    ]
}

/// Shared `requires_expr` for both `get_task_output` and `wait_tasks`.
pub(crate) fn task_output_requires_expr() -> Expr<ToolRequirement> {
    use crate::types::tool_metadata::ToolMetadata;
    let task_tool = Expr::Value(ToolRequirement::Tool {
        namespace: ToolMetadata::tool_namespace(&TaskTool).to_string(),
        id: echo_agent_tool_runtime::Tool::id(&TaskTool)
            .as_str()
            .to_string(),
        if_params: None,
    });
    let mut arms = background_bash_requires_exprs();
    arms.push(task_tool);
    Expr::Or(arms)
}

#[derive(Debug, Default)]
pub struct TaskOutputTool;

impl TaskOutputTool {
    async fn run_single_task(
        &self,
        task_id: &str,
        timeout_ms: Option<u64>,
        ctx: &echo_agent_tool_runtime::ToolCallContext,
        resources: SharedResources,
    ) -> Result<TaskOutputOutput, echo_agent_tool_runtime::ToolError> {
        let contract_version = ctx
            .extensions
            .get::<echo_agent_tool_runtime::BehaviorVersion>()
            .map(|v| v.0.clone());
        let is_legacy = crate::versions::is_legacy_contract(contract_version.as_deref());
        let terminal;
        {
            terminal = resources.lock().await.require::<Terminal>()?.0.clone();
        }

        let waits = echo_agent_tool_types::task_output_waits(timeout_ms);
        let wait_cap = max_wait_block();
        let wait_hint = if waits {
            WaitHint::Elapsed {
                requested: requested_wait_timeout(timeout_ms),
                waited: capped_wait_timeout(timeout_ms, wait_cap),
            }
        } else {
            WaitHint::NotRequested
        };
        let snapshot = if waits {
            // Cap the blocking wait so a large `timeout_ms` can't wedge the turn;
            // the model is pinged on completion regardless (see `capped_wait_timeout`).
            let timeout = capped_wait_timeout(timeout_ms, wait_cap);
            terminal.wait_for_completion(task_id, Some(timeout)).await
        } else {
            terminal.get_task(task_id).await
        };

        if let Some(snapshot) = snapshot {
            let read_file_name;
            {
                let res = resources.lock().await;
                let renderer = res.require::<TemplateRenderer>()?;
                read_file_name = renderer.render("${{ tools.by_kind.read }}").map_err(|e| {
                    echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string())
                })?;
            }
            let max_output_bytes = resources
                .lock()
                .await
                .get::<TruncationCfg>()
                .map(|cfg| {
                    cfg.0.max_output_bytes_for(
                        "get_command_or_subagent_output",
                        DEFAULT_TOOL_OUTPUT_BYTES,
                    )
                })
                .unwrap_or(DEFAULT_TOOL_OUTPUT_BYTES);
            return Ok(TaskOutputOutput::Result(apply_running_wait_hint(
                snapshot_to_result(snapshot, &read_file_name, max_output_bytes),
                wait_hint,
                WaitSubject::Task,
            )));
        }

        let backend = {
            resources
                .lock()
                .await
                .get::<SubagentBackendResource>()
                .cloned()
        };
        // Same cap as the bash path: a blocking subagent query can't wedge the
        // turn beyond the wait cap (the parent is pinged when the child finishes).
        let query_timeout_ms = if waits {
            Some(capped_wait_timeout(timeout_ms, wait_cap).as_millis() as u64)
        } else {
            timeout_ms
        };
        if let Some(backend) = backend
            && let Some(snapshot) = backend
                .backend()
                .query(task_id, waits, query_timeout_ms)
                .await
        {
            return Ok(format_subagent_snapshot(&snapshot, wait_hint));
        }

        // Neither found
        {
            let msg = if is_legacy {
                render_legacy_task_output_not_found(task_id)
            } else {
                let known = terminal.list_tasks().await;
                if known.is_empty() {
                    format!(
                        "Task {task_id} not found. No background tasks or subagents exist in this session.",
                    )
                } else {
                    let ids: Vec<&str> = known.iter().map(|t| t.task_id.as_str()).collect();
                    format!(
                        "Task {task_id} not found. Known task IDs: [{}]",
                        ids.join(", ")
                    )
                }
            };
            Ok(TaskOutputOutput::TaskNotFound(msg))
        }
    }

    pub(crate) async fn run_multi_tasks(
        task_ids: &[String],
        timeout_ms: Option<u64>,
        resources: SharedResources,
        tool_name_for_truncation: &str,
    ) -> Result<TaskOutputOutput, echo_agent_tool_runtime::ToolError> {
        let waits = echo_agent_tool_types::task_output_waits(timeout_ms);
        let requested = requested_wait_timeout(timeout_ms);
        let timeout = capped_wait_timeout(timeout_ms, max_wait_block());

        let (terminal, backend, read_file_name, max_output_bytes) = {
            let res = resources.lock().await;
            let terminal = res.require::<Terminal>()?.0.clone();
            let backend = res.get::<SubagentBackendResource>().cloned();
            let renderer = res.require::<TemplateRenderer>()?;
            let rfn = renderer.render("${{ tools.by_kind.read }}").map_err(|e| {
                echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string())
            })?;
            let mob = res
                .get::<TruncationCfg>()
                .map(|cfg| {
                    cfg.0
                        .max_output_bytes_for(tool_name_for_truncation, DEFAULT_TOOL_OUTPUT_BYTES)
                })
                .unwrap_or(DEFAULT_TOOL_OUTPUT_BYTES);
            (terminal, backend, rfn, mob)
        };

        let initial = resolve_tasks(
            task_ids,
            &terminal,
            &backend,
            &read_file_name,
            max_output_bytes,
            WaitHint::NotRequested,
        )
        .await;

        let results = if waits
            && (!initial.pending_bash_ids.is_empty() || !initial.pending_subagent_ids.is_empty())
        {
            let deadline = tokio::time::Instant::now() + timeout;
            let wait_hint = wait_all_event_driven(
                &terminal,
                &backend,
                &initial.pending_bash_ids,
                &initial.pending_subagent_ids,
                deadline,
            )
            .await
            .hint(requested, timeout);
            resolve_tasks(
                task_ids,
                &terminal,
                &backend,
                &read_file_name,
                max_output_bytes,
                wait_hint,
            )
            .await
            .results
        } else {
            initial.results
        };

        let completed_count = results
            .iter()
            .filter(|r| is_terminal_status(&r.status))
            .count();
        let total = results.len();
        let mode_str = if waits { "wait_all" } else { "poll" };
        let summary = format!("{completed_count}/{total} tasks completed ({mode_str})");

        Ok(TaskOutputOutput::MultiResult(MultiTaskOutputResult {
            mode: mode_str.to_string(),
            results,
            summary,
        }))
    }
}

pub(crate) use echo_agent_tool_types::MAX_MULTI_WAIT_IDS;

/// Terminal task statuses as produced by `snapshot_to_result` /
/// `format_subagent_snapshot`; multi-wait summaries count these as finished.
pub(crate) fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "timed_out")
}

pub(crate) fn not_found_result(task_id: &str) -> TaskOutputResult {
    TaskOutputResult {
        task_id: task_id.to_string(),
        command: String::new(),
        status: "not_found".to_string(),
        exit_code: None,
        started: String::new(),
        ended: None,
        duration_secs: 0.0,
        output: format!("Task {task_id} not found."),
        output_file: String::new(),
        truncated: false,
        truncation_hint: String::new(),
        raw_output_bytes: 0,
    }
}

/// Outcome of resolving all tasks against terminal + subagent coordinator.
pub(crate) struct ResolveResult {
    pub(crate) results: Vec<TaskOutputResult>,
    pub(crate) pending_bash_ids: Vec<String>,
    pub(crate) pending_subagent_ids: Vec<String>,
}

pub(crate) async fn resolve_tasks(
    task_ids: &[String],
    terminal: &std::sync::Arc<dyn crate::computer::types::TerminalBackend>,
    backend: &Option<SubagentBackendResource>,
    read_file_name: &str,
    max_output_bytes: usize,
    wait_hint: WaitHint,
) -> ResolveResult {
    let mut results = Vec::with_capacity(task_ids.len());
    let mut pending_bash_ids = Vec::new();
    let mut pending_subagent_ids = Vec::new();

    for id in task_ids {
        if let Some(snap) = terminal.get_task(id).await {
            let is_pending = !snap.completed;
            results.push(apply_running_wait_hint(
                snapshot_to_result(snap, read_file_name, max_output_bytes),
                wait_hint,
                WaitSubject::Task,
            ));
            if is_pending {
                pending_bash_ids.push(id.clone());
            }
            continue;
        }

        if let Some(be) = backend
            && let Some(snap) = be.backend().query(id, false, None).await
        {
            let is_terminal = snap.status.is_terminal();
            if let TaskOutputOutput::Result(r) = format_subagent_snapshot(&snap, wait_hint) {
                if !is_terminal {
                    pending_subagent_ids.push(id.clone());
                }
                results.push(r);
                continue;
            }
        }

        results.push(not_found_result(id));
    }

    ResolveResult {
        results,
        pending_bash_ids,
        pending_subagent_ids,
    }
}
//
// Uses `TerminalBackend::wait_for_completion` for bash tasks (event-driven via
// the underlying `Notify`) and `SubagentQueryRequest { block: true }` for
// subagents (blocks in the coordinator until the child session finishes).
// No 200ms polling loop — wakeups happen on actual state transitions.

/// Aborts all wrapped helper-wait tasks when dropped.
///
/// The per-task waits below are `tokio::spawn`ed so they can race each other,
/// but they must not outlive the wait call itself: a detached wait left
/// running after the caller returns (first completion, deadline) or is
/// cancelled (turn abort dropping the tool future) becomes a zombie that
/// consumes its task's completion later — marking the task `block_waited`
/// and swallowing a result the model never saw, which suppresses the
/// completion auto-wake. Aborting drops the underlying
/// `wait_for_completion` future, whose dropped reply channel the terminal
/// actor detects to keep `block_waited` accurate.
struct AbortWaitsOnDrop(Vec<tokio::task::AbortHandle>);

impl Drop for AbortWaitsOnDrop {
    fn drop(&mut self) {
        for h in &self.0 {
            h.abort();
        }
    }
}

/// Whether a multi-task wait returned because the deadline was hit or because
/// the wait condition (any/all) completed first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitOutcome {
    DeadlineElapsed,
    CompletedEarly,
}

impl WaitOutcome {
    pub(crate) fn hint(self, requested: Duration, waited: Duration) -> WaitHint {
        match self {
            WaitOutcome::DeadlineElapsed => WaitHint::Elapsed { requested, waited },
            WaitOutcome::CompletedEarly => WaitHint::ReturnedEarly,
        }
    }
}

fn finalize_wait_outcome(outcome: WaitOutcome, deadline: tokio::time::Instant) -> WaitOutcome {
    // select! is non-deterministic when both arms are ready at the deadline.
    if tokio::time::Instant::now() >= deadline {
        WaitOutcome::DeadlineElapsed
    } else {
        outcome
    }
}

/// Wait until any one task (bash or subagent) completes, or deadline is reached.
pub(crate) async fn wait_any_event_driven(
    terminal: &std::sync::Arc<dyn crate::computer::types::TerminalBackend>,
    backend: &Option<SubagentBackendResource>,
    bash_ids: &[String],
    subagent_ids: &[String],
    deadline: tokio::time::Instant,
) -> WaitOutcome {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return WaitOutcome::DeadlineElapsed;
    }

    // Register waiter BEFORE spawns to avoid race: a spawned task could complete
    // and call notify_waiters() before the Notified future exists.
    use tokio::sync::Notify as TokioNotify;
    let done = std::sync::Arc::new(TokioNotify::new());
    let notified = done.notified();

    let mut waits: Vec<tokio::task::AbortHandle> = Vec::new();

    for id in bash_ids {
        let terminal = terminal.clone();
        let id = id.clone();
        let timeout = remaining;
        let done = done.clone();
        waits.push(
            tokio::spawn(async move {
                terminal.wait_for_completion(&id, Some(timeout)).await;
                done.notify_waiters();
            })
            .abort_handle(),
        );
    }

    for id in subagent_ids {
        if let Some(be) = backend {
            let be = be.clone();
            let id = id.clone();
            let timeout_ms = remaining.as_millis() as u64;
            let done = done.clone();
            waits.push(
                tokio::spawn(async move {
                    let _ = be.backend().query(&id, true, Some(timeout_ms)).await;
                    done.notify_waiters();
                })
                .abort_handle(),
            );
        }
    }

    // Tear the helper waits down on every exit path: first completion,
    // deadline, or cancellation of this future.
    let _guard = AbortWaitsOnDrop(waits);

    let outcome = tokio::select! {
        _ = notified => WaitOutcome::CompletedEarly,
        _ = tokio::time::sleep_until(deadline) => WaitOutcome::DeadlineElapsed,
    };
    finalize_wait_outcome(outcome, deadline)
}

/// Wait until all tasks (bash and subagent) complete, or deadline is reached.
pub(crate) async fn wait_all_event_driven(
    terminal: &std::sync::Arc<dyn crate::computer::types::TerminalBackend>,
    backend: &Option<SubagentBackendResource>,
    bash_ids: &[String],
    subagent_ids: &[String],
    deadline: tokio::time::Instant,
) -> WaitOutcome {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return WaitOutcome::DeadlineElapsed;
    }

    let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    for id in bash_ids {
        let terminal = terminal.clone();
        let id = id.clone();
        let timeout = remaining;
        handles.push(tokio::spawn(async move {
            terminal.wait_for_completion(&id, Some(timeout)).await;
        }));
    }

    for id in subagent_ids {
        if let Some(be) = backend {
            let be = be.clone();
            let id = id.clone();
            let timeout_ms = remaining.as_millis() as u64;
            handles.push(tokio::spawn(async move {
                let _ = be.backend().query(&id, true, Some(timeout_ms)).await;
            }));
        }
    }

    // Tear the helper waits down on every exit path: all complete, deadline,
    // or cancellation of this future (see `AbortWaitsOnDrop`).
    let _guard = AbortWaitsOnDrop(handles.iter().map(|h| h.abort_handle()).collect());

    let all_fut = futures_util::future::join_all(handles);
    let outcome = tokio::select! {
        _ = all_fut => WaitOutcome::CompletedEarly,
        _ = tokio::time::sleep_until(deadline) => WaitOutcome::DeadlineElapsed,
    };
    finalize_wait_outcome(outcome, deadline)
}

//
// Historical fixture captured from the 0.4.10 implementation.
//
// In 0.4.10, get_task_output returned:
//   Err(ToolError::ProcessManagerError(format!("Task {} not found", input.task_id)))
//
// The meaningful customer-facing message content is the inner string.
// Subagent wording is out of scope — subagents didn't exist in 0.4.10.

/// Exact historical not-found message for `get_task_output` in legacy-0.4.10.
fn render_legacy_task_output_not_found(task_id: &str) -> String {
    format!("Task {} not found", task_id)
}

fn format_subagent_snapshot(snap: &SubagentSnapshot, wait_hint: WaitHint) -> TaskOutputOutput {
    let started = format_epoch_ms_as_rfc3339(snap.started_at_epoch_ms);
    match &snap.status {
        SubagentSnapshotStatus::Initializing => {
            let duration_secs = snap.duration_ms as f64 / 1000.0;
            // Measure body only — wait-hint is harness advisory, not task output.
            let body = format!(
                "Subagent is initializing (creating worktree, resolving config).\n\
                 Type: {}\n\
                 Description: {}\n\
                 Elapsed: {duration_secs:.1}s",
                snap.subagent_type, snap.description,
            );
            let raw_output_bytes = body.len();
            let output = with_still_running_wait_hint(body, wait_hint, WaitSubject::Subagent);
            TaskOutputOutput::Result(TaskOutputResult {
                task_id: snap.subagent_id.clone(),
                command: format!("[subagent:{}] {}", snap.subagent_type, snap.description),
                status: "initializing".to_string(),
                exit_code: None,
                started,
                ended: None,
                duration_secs,
                output,
                output_file: String::new(),
                truncated: false,
                truncation_hint: String::new(),
                raw_output_bytes,
            })
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
            let tools_str = if tools_used.is_empty() {
                "none yet".to_string()
            } else {
                tools_used.join(", ")
            };
            let tokens_k = tokens_used / 1000;
            let capacity_k = context_window_tokens / 1000;
            // Measure body only — wait-hint is harness advisory, not task output.
            let body = format!(
                "Subagent is still running.\n\
                 Type: {}\n\
                 Description: {}\n\
                 Elapsed: {:.1}s\n\
                 Progress: turn {turn_count}, {tool_call_count} tool calls, \
                 {tokens_k}K/{capacity_k}K tokens ({context_usage_pct}% context)\n\
                 Tools used: {tools_str}\n\
                 Errors: {error_count}",
                snap.subagent_type,
                snap.description,
                snap.duration_ms as f64 / 1000.0,
            );
            let raw_output_bytes = body.len();
            let output = with_still_running_wait_hint(body, wait_hint, WaitSubject::Subagent);
            TaskOutputOutput::Result(TaskOutputResult {
                task_id: snap.subagent_id.clone(),
                command: format!("[subagent:{}] {}", snap.subagent_type, snap.description),
                status: "running".to_string(),
                exit_code: None,
                started,
                ended: None,
                duration_secs: snap.duration_ms as f64 / 1000.0,
                output,
                output_file: String::new(),
                truncated: false,
                truncation_hint: String::new(),
                raw_output_bytes,
            })
        }
        SubagentSnapshotStatus::Completed {
            output,
            tool_calls,
            turns,
            worktree_path,
        } => {
            let mut output = format!(
                "{output}\n\n<subagent_meta>id={}, type={}, tool_calls={tool_calls}, \
                 turns={turns}, duration_ms={}</subagent_meta>",
                snap.subagent_id, snap.subagent_type, snap.duration_ms,
            );
            if let Some(wt) = &worktree_path {
                output.push_str(&format!("\n<worktree_path>{wt}</worktree_path>"));
            }
            output.push_str("\n\n");
            output.push_str(&echo_agent_tool_types::format_resume_footer(
                &snap.subagent_id,
                &snap.subagent_type,
                snap.persona.as_deref(),
            ));
            let raw_output_bytes = output.len();
            TaskOutputOutput::Result(TaskOutputResult {
                task_id: snap.subagent_id.clone(),
                command: format!("[subagent:{}] {}", snap.subagent_type, snap.description),
                status: "completed".to_string(),
                exit_code: Some(0),
                started,
                ended: Some(format_epoch_ms_as_rfc3339(
                    snap.started_at_epoch_ms + snap.duration_ms,
                )),
                duration_secs: snap.duration_ms as f64 / 1000.0,
                output,
                output_file: String::new(),
                truncated: false,
                truncation_hint: String::new(),
                raw_output_bytes,
            })
        }
        SubagentSnapshotStatus::Failed { error } => {
            let raw_output_bytes = error.len();
            TaskOutputOutput::Result(TaskOutputResult {
                task_id: snap.subagent_id.clone(),
                command: format!("[subagent:{}] {}", snap.subagent_type, snap.description),
                status: "failed".to_string(),
                exit_code: Some(1),
                started,
                ended: Some(format_epoch_ms_as_rfc3339(
                    snap.started_at_epoch_ms + snap.duration_ms,
                )),
                duration_secs: snap.duration_ms as f64 / 1000.0,
                output: error.clone(),
                output_file: String::new(),
                truncated: false,
                truncation_hint: String::new(),
                raw_output_bytes,
            })
        }
        SubagentSnapshotStatus::Cancelled { reason } => {
            let output = reason
                .clone()
                .unwrap_or_else(|| "Subagent was cancelled".to_string());
            let raw_output_bytes = output.len();
            TaskOutputOutput::Result(TaskOutputResult {
                task_id: snap.subagent_id.clone(),
                command: format!("[subagent:{}] {}", snap.subagent_type, snap.description),
                status: "cancelled".to_string(),
                exit_code: None,
                started,
                ended: Some(format_epoch_ms_as_rfc3339(
                    snap.started_at_epoch_ms + snap.duration_ms,
                )),
                duration_secs: snap.duration_ms as f64 / 1000.0,
                output,
                output_file: String::new(),
                truncated: false,
                truncation_hint: String::new(),
                raw_output_bytes,
            })
        }
    }
}

fn format_epoch_ms_as_rfc3339(epoch_ms: u64) -> String {
    use chrono::{DateTime, Utc};
    let secs = (epoch_ms / 1000) as i64;
    let nanos = ((epoch_ms % 1000) * 1_000_000) as u32;
    match DateTime::from_timestamp(secs, nanos) {
        Some(dt) => dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        None => Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    }
}

impl crate::types::tool_metadata::ToolMetadata for TaskOutputTool {
    fn kind(&self) -> ToolKind {
        ToolKind::BackgroundTaskAction
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        // Canonical wording lives in the shared builder; `versioned_definition`
        // renders it context-aware from the finalized toolset. This static
        // fallback mirrors the default echo-agent-build toolset.
        static DESC: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            echo_agent_tool_types::build_task_output_description(
                &echo_agent_tool_types::TaskOutputToolNaming {
                    monitor_tool: Some("monitor"),
                    read_tool: Some("read_file"),
                    bash_background_param: Some("is_background"),
                    subagent_background_param: Some("run_in_background"),
                    task_ids_param: "task_ids",
                    timeout_ms_param: "timeout_ms",
                    task_id_param: "task_id",
                },
            )
        });
        &DESC
    }

    fn versioned_definition(
        &self,
        _contract_version: Option<&str>,
        client_name: &str,
        description_override: Option<&str>,
        renderer: &TemplateRenderer,
        param_map: &std::collections::HashMap<String, String>,
        input_schema: &serde_json::Value,
        _effective_params: &serde_json::Value,
    ) -> crate::types::definition::ToolDefinition {
        let description = task_output_description(renderer, description_override);
        let remapped_schema = if param_map.is_empty() {
            input_schema.clone()
        } else {
            crate::util::remap::remap_schema_properties(input_schema, param_map)
        };
        crate::types::definition::ToolDefinition::function(
            client_name,
            Some(&description),
            remapped_schema,
        )
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        task_output_requires_expr()
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

/// Resolve the model-facing `get_task_output` description from the finalized
/// toolset, honoring an explicit config override. Wording lives in the shared
/// [`echo_agent_tool_types::build_task_output_description`] builder so the CLI and
/// prod-chat can't drift; presence-gated clauses (monitor note, subagent
/// source, read-file hint) follow the tools actually registered this turn.
fn task_output_description(
    renderer: &TemplateRenderer,
    description_override: Option<&str>,
) -> String {
    if let Some(ovr) = description_override {
        return renderer.render(ovr).unwrap_or_else(|e| {
            tracing::warn!("get_task_output description override render failed, using raw: {e}");
            ovr.to_string()
        });
    }
    echo_agent_tool_types::build_task_output_description(
        &echo_agent_tool_types::TaskOutputToolNaming {
            monitor_tool: renderer.tool_for_kind(ToolKind::Monitor),
            read_tool: renderer.tool_for_kind(ToolKind::Read),
            bash_background_param: renderer.param_for_kind(ToolKind::Execute, "is_background"),
            subagent_background_param: renderer.param_for_kind(ToolKind::Task, "run_in_background"),
            task_ids_param: renderer
                .param_for_kind(ToolKind::BackgroundTaskAction, "task_ids")
                .unwrap_or("task_ids"),
            timeout_ms_param: renderer
                .param_for_kind(ToolKind::BackgroundTaskAction, "timeout_ms")
                .unwrap_or("timeout_ms"),
            // Same singular id name kill_task uses in its monitor aside.
            task_id_param: renderer
                .param_for_kind(ToolKind::KillTaskAction, "task_id")
                .unwrap_or("task_id"),
        },
    )
}

impl echo_agent_tool_runtime::Tool for TaskOutputTool {
    type Args = TaskOutputToolInput;
    type Output = TaskOutputOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("get_task_output").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "get_task_output",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    #[tracing::instrument(
        name = "tool.get_task_output",
        skip_all,
        fields(waits = %input.waits())
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: TaskOutputToolInput,
    ) -> Result<TaskOutputOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let ids = input.resolved_task_ids();
        if ids.is_empty() {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Provide a non-empty task_ids list.".to_string(),
            ));
        }
        if ids.len() > MAX_MULTI_WAIT_IDS {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                format!("task_ids exceeds maximum of {MAX_MULTI_WAIT_IDS} entries."),
            ));
        }

        if ids.len() == 1 {
            return self
                .run_single_task(&ids[0], input.timeout_ms, &ctx, resources)
                .await;
        }

        Self::run_multi_tasks(
            &ids,
            input.timeout_ms,
            resources,
            "get_command_or_subagent_output",
        )
        .await
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use crate::computer::types::{
        BackgroundHandle, KillOutcome, TaskSnapshot, TerminalBackend, TerminalRunRequest,
        TerminalRunResult,
    };
    use crate::types::resources::{Resources, Terminal};
    use crate::types::template_renderer::TemplateRenderer;
    use crate::types::tool::ToolKind;

    /// Mock backend that returns a pre-configured snapshot.
    pub(crate) struct MockTerminal {
        snapshot: Option<TaskSnapshot>,
    }

    impl MockTerminal {
        pub(crate) fn with_snapshot(snapshot: TaskSnapshot) -> Self {
            Self {
                snapshot: Some(snapshot),
            }
        }

        pub(crate) fn empty() -> Self {
            Self { snapshot: None }
        }
    }

    #[async_trait::async_trait]
    impl TerminalBackend for MockTerminal {
        async fn run(
            &self,
            _request: TerminalRunRequest,
        ) -> Result<TerminalRunResult, crate::computer::types::ComputerError> {
            unimplemented!()
        }

        async fn run_background(
            &self,
            _request: TerminalRunRequest,
        ) -> Result<BackgroundHandle, crate::computer::types::ComputerError> {
            unimplemented!()
        }

        async fn kill_task(&self, _task_id: &str) -> KillOutcome {
            unimplemented!()
        }

        async fn get_task(&self, _task_id: &str) -> Option<TaskSnapshot> {
            self.snapshot.clone()
        }

        async fn wait_for_completion(
            &self,
            _task_id: &str,
            _timeout: Option<Duration>,
        ) -> Option<TaskSnapshot> {
            self.snapshot.clone()
        }

        async fn list_tasks(&self) -> Vec<TaskSnapshot> {
            self.snapshot.iter().cloned().collect()
        }
    }

    pub(crate) fn make_snapshot(
        task_id: &str,
        completed: bool,
        exit_code: Option<i32>,
    ) -> TaskSnapshot {
        TaskSnapshot {
            task_id: task_id.to_string(),
            command: "echo hello".to_string(),
            display_command: None,
            cwd: "/tmp".to_string(),
            start_time: SystemTime::now(),
            end_time: if completed {
                Some(SystemTime::now())
            } else {
                None
            },
            output: "test output".to_string(),
            output_file: PathBuf::from(format!("/tmp/{}.log", task_id)),
            truncated: false,
            exit_code,
            signal: None,
            completed,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        }
    }

    pub(crate) fn resources_with_terminal(snapshot: Option<TaskSnapshot>) -> Resources {
        let mut resources = Resources::new();
        let backend: Arc<dyn TerminalBackend> = match snapshot {
            Some(s) => Arc::new(MockTerminal::with_snapshot(s)),
            None => Arc::new(MockTerminal::empty()),
        };
        resources.insert(Terminal(backend));
        resources.insert(TemplateRenderer::new(
            std::collections::HashMap::from([(ToolKind::Read, "read_file".to_string())]),
            std::collections::HashMap::new(),
        ));
        resources
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::*;
    use super::*;
    use crate::computer::types::{
        BackgroundHandle, KillOutcome, TaskSnapshot, TerminalBackend, TerminalRunRequest,
        TerminalRunResult,
    };
    use crate::types::resources::Resources;
    use crate::types::tool_metadata::ToolMetadata;
    use crate::types::tool_metadata::test_ctx;
    use std::sync::Arc;

    // A blocking wait must never hold the turn for longer than the wait
    // cap, regardless of the model's requested `timeout_ms` (repro: an
    // unbounded blocking wait wedged the turn for hours).
    #[test]
    fn capped_wait_timeout_clamps_and_defaults() {
        let cap = Duration::from_millis(echo_agent_tool_types::MAX_WAIT_BLOCK_MS_DEFAULT);
        assert_eq!(capped_wait_timeout(None, cap), DEFAULT_WAIT_TIMEOUT);
        assert_eq!(
            capped_wait_timeout(Some(5_000), cap),
            Duration::from_millis(5_000)
        );
        assert_eq!(capped_wait_timeout(Some(36_000_000), cap), cap);
        assert_eq!(capped_wait_timeout(Some(600_000), cap), cap);
    }

    /// A client that shortens the cap at finalize must also shorten the wait —
    /// otherwise the server outlasts the deadline the client will honor.
    #[test]
    fn capped_wait_timeout_honors_a_shortened_cap() {
        let cap = Duration::from_millis(300_000);
        assert_eq!(capped_wait_timeout(Some(600_000), cap), cap);
        assert_eq!(
            capped_wait_timeout(Some(120_000), cap),
            Duration::from_millis(120_000)
        );
    }

    #[test]
    fn still_running_wait_hint_omitted_invites_timeout_ms() {
        assert_eq!(
            still_running_wait_hint(WaitHint::NotRequested, WaitSubject::Task),
            "Use timeout_ms to wait for completion. You will be notified automatically when the task completes."
        );
        assert_eq!(
            still_running_wait_hint(WaitHint::NotRequested, WaitSubject::Subagent),
            "Use timeout_ms to wait for completion. You will be notified automatically when the subagent completes."
        );
    }

    #[test]
    fn still_running_wait_hint_within_cap_reports_auto_wake() {
        let hint = WaitHint::Elapsed {
            requested: Duration::from_secs(30),
            waited: Duration::from_secs(30),
        };
        assert_eq!(
            still_running_wait_hint(hint, WaitSubject::Task),
            "Waited the requested 30s; the task is still running. \
             You will be notified automatically when the task completes."
        );
        assert_eq!(
            still_running_wait_hint(hint, WaitSubject::Subagent),
            "Waited the requested 30s; the subagent is still running. \
             You will be notified automatically when the subagent completes."
        );
    }

    #[test]
    fn still_running_wait_hint_clamped_reports_cap_and_auto_wake() {
        let hint = WaitHint::Elapsed {
            requested: Duration::from_secs(2_400),
            waited: Duration::from_secs(600),
        };
        assert_eq!(
            still_running_wait_hint(hint, WaitSubject::Task),
            "Waited 600s, the per-call maximum, of the 2400s you requested; \
             the task is still running. You do not need to call this again. \
             You will be notified automatically when the task completes."
        );
        assert_eq!(
            still_running_wait_hint(hint, WaitSubject::Subagent),
            "Waited 600s, the per-call maximum, of the 2400s you requested; \
             the subagent is still running. You do not need to call this again. \
             You will be notified automatically when the subagent completes."
        );
    }

    #[test]
    fn still_running_wait_hint_returned_early_is_honest() {
        assert_eq!(
            still_running_wait_hint(WaitHint::ReturnedEarly, WaitSubject::Task),
            "Wait returned early because another finished; this task is still running. \
             You will be notified automatically when the task completes."
        );
        assert_eq!(
            still_running_wait_hint(WaitHint::ReturnedEarly, WaitSubject::Subagent),
            "Wait returned early because another finished; this subagent is still running. \
             You will be notified automatically when the subagent completes."
        );
    }

    #[test]
    fn tool_name_and_description() {
        let tool = TaskOutputTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "get_task_output"
        );
        // The static fallback is the shared builder's default echo-agent-build
        // rendering (monitor + task + bash + read present): concrete names, no
        // leftover template markers.
        let desc = ToolMetadata::description_template(&tool);
        assert!(desc.contains("Get output and status from a background task"));
        // Must name "monitor" so the model connects polling a monitor to this tool.
        assert!(desc.contains("monitor"));
        assert!(
            desc.contains("read_file"),
            "fallback names the read tool: {desc}"
        );
        assert!(
            !desc.contains("${"),
            "fallback must not leak template markers: {desc}"
        );
    }

    /// The context-aware `versioned_definition` path only mentions a tool when
    /// it's actually in the finalized toolset. Renders every relevant subset
    /// through the shared builder via `task_output_description`.
    #[test]
    fn description_context_aware_for_tool_subsets() {
        use crate::types::template_renderer::TemplateRenderer;
        use std::collections::HashMap;

        let cases: &[(&str, &[(ToolKind, &str)])] = &[
            (
                "execute only",
                &[(ToolKind::Execute, "run_terminal_command")],
            ),
            (
                "execute + read",
                &[
                    (ToolKind::Execute, "run_terminal_command"),
                    (ToolKind::Read, "read_file"),
                ],
            ),
            (
                "execute + task",
                &[
                    (ToolKind::Execute, "run_terminal_command"),
                    (ToolKind::Task, "spawn_subagent"),
                ],
            ),
            (
                "task + monitor + read (no bash)",
                &[
                    (ToolKind::Task, "spawn_subagent"),
                    (ToolKind::Monitor, "monitor"),
                    (ToolKind::Read, "read_file"),
                ],
            ),
        ];

        for (label, kinds) in cases {
            let tools: HashMap<ToolKind, String> =
                kinds.iter().map(|(k, n)| (*k, n.to_string())).collect();
            // Seed the param map for present tools the way `finalize` does, so
            // the background sources / subagent header resolve via
            // `param_for_kind` (which reads the param map, not the tool map).
            let mut params: HashMap<ToolKind, HashMap<String, String>> = HashMap::new();
            for (k, _) in kinds.iter() {
                match k {
                    ToolKind::Execute => {
                        params
                            .entry(ToolKind::Execute)
                            .or_default()
                            .insert("is_background".to_string(), "is_background".to_string());
                    }
                    ToolKind::Task => {
                        params.entry(ToolKind::Task).or_default().insert(
                            "run_in_background".to_string(),
                            "run_in_background".to_string(),
                        );
                    }
                    _ => {}
                }
            }
            let renderer = TemplateRenderer::new(tools, params);
            let rendered = task_output_description(&renderer, None);

            let has_monitor = kinds.iter().any(|(k, _)| *k == ToolKind::Monitor);
            let has_task = kinds.iter().any(|(k, _)| *k == ToolKind::Task);
            let has_read = kinds.iter().any(|(k, _)| *k == ToolKind::Read);

            assert!(
                !rendered.contains("${"),
                "[{label}] left an unrendered template marker:\n{rendered}"
            );
            assert!(
                rendered.contains("background task"),
                "[{label}] must always mention background task:\n{rendered}"
            );
            assert_eq!(
                rendered.contains("monitor"),
                has_monitor,
                "[{label}] monitor mention must match monitor-tool presence:\n{rendered}"
            );
            assert_eq!(
                rendered.contains("subagent"),
                has_task,
                "[{label}] subagent mention must match task-tool presence:\n{rendered}"
            );
            assert_eq!(
                rendered.contains("output_file"),
                has_read,
                "[{label}] read-file hint must match read-tool presence:\n{rendered}"
            );
        }
    }

    #[test]
    fn description_tracks_renamed_task_ids_and_timeout_ms() {
        use crate::types::template_renderer::TemplateRenderer;
        use std::collections::HashMap;

        let tools = HashMap::from([
            (ToolKind::Execute, "run_terminal_command".to_string()),
            (ToolKind::Monitor, "monitor".to_string()),
            (
                ToolKind::BackgroundTaskAction,
                "get_task_output".to_string(),
            ),
            (ToolKind::KillTaskAction, "kill_task".to_string()),
        ]);
        let params = HashMap::from([
            (
                ToolKind::Execute,
                HashMap::from([("is_background".to_string(), "is_background".to_string())]),
            ),
            (
                ToolKind::BackgroundTaskAction,
                HashMap::from([
                    ("task_ids".to_string(), "process_ids".to_string()),
                    ("timeout_ms".to_string(), "max_wait".to_string()),
                ]),
            ),
            (
                ToolKind::KillTaskAction,
                HashMap::from([("task_id".to_string(), "id".to_string())]),
            ),
        ]);
        let rendered = task_output_description(&TemplateRenderer::new(tools, params), None);
        assert!(
            rendered.contains("Pass process_ids with"),
            "renamed task_ids must appear:\n{rendered}"
        );
        assert!(
            rendered.contains("Omit max_wait or pass 0")
                && rendered.contains("positive max_wait wait"),
            "renamed timeout_ms must appear:\n{rendered}"
        );
        assert!(
            rendered.contains("a monitor's id is returned by monitor"),
            "renamed kill_task task_id must appear in monitor aside:\n{rendered}"
        );
        assert!(
            !rendered.contains("task_ids")
                && !rendered.contains("timeout_ms")
                && !rendered.contains("task_id"),
            "canonical param names must not remain after rename:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn get_task_running() {
        let snapshot = make_snapshot("task-1", false, None);
        let resources = resources_with_terminal(Some(snapshot));
        let tool = TaskOutputTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-1".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::Result(r) => {
                assert_eq!(r.task_id, "task-1");
                assert_eq!(r.status, "running");
                assert!(r.exit_code.is_none());
                assert!(r.ended.is_none());
            }
            other => panic!("Expected Success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn get_task_completed_success() {
        let snapshot = make_snapshot("task-2", true, Some(0));
        let resources = resources_with_terminal(Some(snapshot));
        let tool = TaskOutputTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-2".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::Result(r) => {
                assert_eq!(r.status, "completed");
                assert_eq!(r.exit_code, Some(0));
                assert!(r.ended.is_some());
            }
            other => panic!("Expected Success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn get_task_completed_failed() {
        let snapshot = make_snapshot("task-3", true, Some(1));
        let resources = resources_with_terminal(Some(snapshot));
        let tool = TaskOutputTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-3".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::Result(r) => {
                assert_eq!(r.status, "failed");
                assert_eq!(r.exit_code, Some(1));
            }
            other => panic!("Expected Success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn get_task_not_found_no_tasks_exist() {
        let resources = resources_with_terminal(None);
        let tool = TaskOutputTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-x".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap(); // Should be Ok, not Err

        match result {
            TaskOutputOutput::TaskNotFound(msg) => {
                assert!(msg.contains("not found"), "msg: {msg}");
                assert!(
                    msg.contains("No background tasks or subagents exist"),
                    "msg: {msg}"
                );
            }
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn get_task_not_found_lists_known_tasks() {
        // Mock that returns None for get_task but has known tasks in list_tasks.
        struct MockTerminalWithKnown;

        #[async_trait::async_trait]
        impl TerminalBackend for MockTerminalWithKnown {
            async fn run(
                &self,
                _request: TerminalRunRequest,
            ) -> Result<TerminalRunResult, crate::computer::types::ComputerError> {
                unimplemented!()
            }

            async fn run_background(
                &self,
                _request: TerminalRunRequest,
            ) -> Result<BackgroundHandle, crate::computer::types::ComputerError> {
                unimplemented!()
            }

            async fn kill_task(&self, _task_id: &str) -> KillOutcome {
                unimplemented!()
            }

            async fn get_task(&self, _task_id: &str) -> Option<TaskSnapshot> {
                None // requested task not found
            }

            async fn wait_for_completion(
                &self,
                _task_id: &str,
                _timeout: Option<Duration>,
            ) -> Option<TaskSnapshot> {
                None
            }

            async fn list_tasks(&self) -> Vec<TaskSnapshot> {
                vec![
                    make_snapshot("task-abc", false, None),
                    make_snapshot("task-def", true, Some(0)),
                ]
            }
        }

        let mut resources = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminalWithKnown);
        resources.insert(Terminal(backend));
        resources.insert(TemplateRenderer::new(
            std::collections::HashMap::from([(ToolKind::Read, "read_file".to_string())]),
            std::collections::HashMap::new(),
        ));

        let tool = TaskOutputTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-unknown".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap(); // Should be Ok, not Err

        match result {
            TaskOutputOutput::TaskNotFound(msg) => {
                assert!(msg.contains("task-unknown"), "msg: {msg}");
                assert!(msg.contains("Known task IDs"), "msg: {msg}");
                assert!(msg.contains("task-abc"), "msg: {msg}");
                assert!(msg.contains("task-def"), "msg: {msg}");
            }
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn get_task_not_found_block_mode_lists_known_tasks() {
        // Verify that blocking mode also provides helpful errors.
        struct MockTerminalBlockNotFound;

        #[async_trait::async_trait]
        impl TerminalBackend for MockTerminalBlockNotFound {
            async fn run(
                &self,
                _request: TerminalRunRequest,
            ) -> Result<TerminalRunResult, crate::computer::types::ComputerError> {
                unimplemented!()
            }

            async fn run_background(
                &self,
                _request: TerminalRunRequest,
            ) -> Result<BackgroundHandle, crate::computer::types::ComputerError> {
                unimplemented!()
            }

            async fn kill_task(&self, _task_id: &str) -> KillOutcome {
                unimplemented!()
            }

            async fn get_task(&self, _task_id: &str) -> Option<TaskSnapshot> {
                None
            }

            async fn wait_for_completion(
                &self,
                _task_id: &str,
                _timeout: Option<Duration>,
            ) -> Option<TaskSnapshot> {
                None // task not found even when blocking
            }

            async fn list_tasks(&self) -> Vec<TaskSnapshot> {
                vec![make_snapshot("bg-task-1", false, None)]
            }
        }

        let mut resources = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminalBlockNotFound);
        resources.insert(Terminal(backend));
        resources.insert(TemplateRenderer::new(
            std::collections::HashMap::from([(ToolKind::Read, "read_file".to_string())]),
            std::collections::HashMap::new(),
        ));

        let tool = TaskOutputTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["nonexistent".into()],
                timeout_ms: Some(1000),
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::TaskNotFound(msg) => {
                assert!(msg.contains("Known task IDs"), "msg: {msg}");
                assert!(msg.contains("bg-task-1"), "msg: {msg}");
            }
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn errors_when_terminal_not_in_resources() {
        let resources = Resources::new();
        let tool = TaskOutputTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-x".into()],
                timeout_ms: None,
            },
        )
        .await;

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .to_lowercase()
                .contains("terminal")
        );
    }

    #[tokio::test]
    async fn uses_tool_name_mapping_for_truncation_hint() {
        let mut snapshot = make_snapshot("task-4", true, Some(0));
        snapshot.output = "x".repeat(500_000); // large output triggers truncation

        let mut resources = resources_with_terminal(Some(snapshot));
        // Set a custom model-facing name for the Read tool
        resources.insert(TemplateRenderer::new(
            [(ToolKind::Read, "Read".to_string())].into(),
            Default::default(),
        ));

        let tool = TaskOutputTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-4".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::Result(r) => {
                assert!(r.truncated);
                assert!(
                    r.output.contains("Read"),
                    "truncation hint should use 'Read'"
                );
                assert!(r.truncation_hint.contains("Read"));
            }
            other => panic!("Expected Success, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn block_waits_for_completion() {
        let snapshot = make_snapshot("task-5", true, Some(0));
        let resources = resources_with_terminal(Some(snapshot));
        let tool = TaskOutputTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-5".into()],
                timeout_ms: Some(5000),
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::Result(r) => assert_eq!(r.status, "completed"),
            other => panic!("Expected Success, got {:?}", other),
        }
    }

    struct StampWaitTerminal {
        snapshot: TaskSnapshot,
        waited: std::sync::Arc<std::sync::atomic::AtomicBool>,
        stamped_block_waited: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl TerminalBackend for StampWaitTerminal {
        async fn run(
            &self,
            _request: TerminalRunRequest,
        ) -> Result<TerminalRunResult, crate::computer::types::ComputerError> {
            unimplemented!()
        }

        async fn run_background(
            &self,
            _request: TerminalRunRequest,
        ) -> Result<BackgroundHandle, crate::computer::types::ComputerError> {
            unimplemented!()
        }

        async fn kill_task(&self, _task_id: &str) -> KillOutcome {
            unimplemented!()
        }

        async fn get_task(&self, _task_id: &str) -> Option<TaskSnapshot> {
            Some(self.snapshot.clone())
        }

        async fn wait_for_completion(
            &self,
            _task_id: &str,
            timeout: Option<Duration>,
        ) -> Option<TaskSnapshot> {
            self.waited.store(true, std::sync::atomic::Ordering::SeqCst);
            if self.snapshot.completed {
                self.stamped_block_waited
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let mut snapshot = self.snapshot.clone();
                snapshot.block_waited = true;
                return Some(snapshot);
            }
            tokio::time::sleep(timeout.unwrap_or(Duration::from_secs(600))).await;
            Some(self.snapshot.clone())
        }

        async fn list_tasks(&self) -> Vec<TaskSnapshot> {
            vec![self.snapshot.clone()]
        }
    }

    fn resources_with_stamp_wait(
        snapshot: TaskSnapshot,
    ) -> (
        Resources,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let waited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stamped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut resources = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(StampWaitTerminal {
            snapshot,
            waited: waited.clone(),
            stamped_block_waited: stamped.clone(),
        });
        resources.insert(Terminal(backend));
        resources.insert(TemplateRenderer::new(
            std::collections::HashMap::from([(ToolKind::Read, "read_file".to_string())]),
            std::collections::HashMap::new(),
        ));
        (resources, waited, stamped)
    }

    #[tokio::test]
    async fn blocking_get_on_already_completed_task_stamps_block_waited() {
        let snapshot = make_snapshot("task-done", true, Some(0));
        let (resources, waited, stamped) = resources_with_stamp_wait(snapshot);
        let started = std::time::Instant::now();
        let result = echo_agent_tool_runtime::Tool::run(
            &TaskOutputTool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-done".into()],
                timeout_ms: Some(600_000),
            },
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "already-completed wait must not burn the 600s cap; elapsed {:?}",
            started.elapsed()
        );
        assert!(
            waited.load(std::sync::atomic::Ordering::SeqCst),
            "waits must call wait_for_completion so ACP can stamp block_waited"
        );
        assert!(
            stamped.load(std::sync::atomic::Ordering::SeqCst),
            "completed wait_for_completion must stamp block_waited"
        );
        match result {
            TaskOutputOutput::Result(r) => assert_eq!(r.status, "completed"),
            other => panic!("Expected Result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn blocking_get_on_still_running_task_still_waits() {
        let snapshot = make_snapshot("task-run", false, None);
        let (resources, waited, stamped) = resources_with_stamp_wait(snapshot);
        let started = std::time::Instant::now();
        let result = echo_agent_tool_runtime::Tool::run(
            &TaskOutputTool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-run".into()],
                timeout_ms: Some(200),
            },
        )
        .await
        .unwrap();
        assert!(
            waited.load(std::sync::atomic::Ordering::SeqCst),
            "still-running get must call wait_for_completion"
        );
        assert!(!stamped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "still-running wait must block; elapsed {:?}",
            started.elapsed()
        );
        match result {
            TaskOutputOutput::Result(r) => assert_eq!(r.status, "running"),
            other => panic!("Expected Result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wait_all_on_already_completed_tasks_does_not_wait() {
        let snapshot = make_snapshot("task-done", true, Some(0));
        let (resources, waited, _) = resources_with_stamp_wait(snapshot);
        let started = std::time::Instant::now();
        let result = TaskOutputTool::run_multi_tasks(
            &["task-done".into()],
            Some(600_000),
            resources.into_shared(),
            "get_command_or_subagent_output",
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "wait-all on a terminal task must not burn the 600s cap; elapsed {:?}",
            started.elapsed()
        );
        assert!(
            !waited.load(std::sync::atomic::Ordering::SeqCst),
            "wait-all must not wait_for_completion on an already-terminal task"
        );
        match result {
            TaskOutputOutput::MultiResult(m) => {
                assert_eq!(m.results.len(), 1);
                assert_eq!(m.results[0].status, "completed");
            }
            other => panic!("Expected MultiResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wait_all_on_still_running_task_still_waits() {
        let snapshot = make_snapshot("task-run", false, None);
        let (resources, waited, _) = resources_with_stamp_wait(snapshot);
        let started = std::time::Instant::now();
        let result = TaskOutputTool::run_multi_tasks(
            &["task-run".into()],
            Some(200),
            resources.into_shared(),
            "get_command_or_subagent_output",
        )
        .await
        .unwrap();
        assert!(
            waited.load(std::sync::atomic::Ordering::SeqCst),
            "wait-all must wait_for_completion on a still-running task"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "wait-all on a running task must block; elapsed {:?}",
            started.elapsed()
        );
        match result {
            TaskOutputOutput::MultiResult(m) => {
                assert_eq!(m.results[0].status, "running");
            }
            other => panic!("Expected MultiResult, got {other:?}"),
        }
    }

    #[test]
    fn is_read_only_returns_true() {
        let tool = TaskOutputTool;
        assert!(
            ToolMetadata::is_read_only(&tool),
            "get_task_output must be classified as read-only"
        );
    }

    /// Positive `timeout_ms` deserializes as waits(); omit/0 does not.
    #[test]
    fn deserialize_timeout_ms_controls_wait() {
        let with_timeout: TaskOutputToolInput = serde_json::from_value(serde_json::json!({
            "task_ids": ["t"],
            "timeout_ms": 120_000
        }))
        .unwrap();
        assert!(with_timeout.waits());

        // Legacy block is ignored; wait is driven only by timeout_ms.
        let legacy_block_false: TaskOutputToolInput = serde_json::from_value(serde_json::json!({
            "task_ids": ["t"],
            "block": false,
            "timeout_ms": 120_000
        }))
        .unwrap();
        assert!(
            legacy_block_false.waits(),
            "positive timeout_ms must wait (legacy block ignored)"
        );

        let legacy_block_only: TaskOutputToolInput = serde_json::from_value(serde_json::json!({
            "task_ids": ["t"],
            "block": true
        }))
        .unwrap();
        assert!(
            !legacy_block_only.waits(),
            "legacy block=true without timeout_ms must not wait"
        );

        let no_timeout: TaskOutputToolInput =
            serde_json::from_value(serde_json::json!({"task_ids": ["t"]})).unwrap();
        assert!(!no_timeout.waits());

        let zero_timeout: TaskOutputToolInput = serde_json::from_value(serde_json::json!({
            "task_ids": ["t"],
            "timeout_ms": 0
        }))
        .unwrap();
        assert!(
            !zero_timeout.waits(),
            "timeout_ms: 0 must stay non-blocking (await_shell parity)"
        );
    }

    #[tokio::test]
    async fn respects_truncation_config() {
        let mut snapshot = make_snapshot("task-6", true, Some(0));
        snapshot.output = "x".repeat(10_000); // 10KB

        let mut resources = resources_with_terminal(Some(snapshot));

        // Set a custom truncation config with 5KB limit
        let mut trunc = crate::types::context::TruncationConfig::default();
        trunc
            .per_tool_max_output_bytes
            .insert("get_command_or_subagent_output".to_string(), 5_000);
        resources.insert(TruncationCfg(trunc));

        let tool = TaskOutputTool;
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-6".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::Result(r) => {
                assert!(
                    r.truncated,
                    "10KB output should be truncated with 5KB limit"
                );
                assert!(r.output.contains("[Output truncated"));
            }
            other => panic!("Expected Success, got {:?}", other),
        }
    }

    // ── Legacy message parity fixture tests ────────────────────────
    //
    // These tests verify exact historical wording for legacy-0.4.10.
    // Fixture source: the historical 0.4.10 task_output implementation.
    //
    // Historical 0.4.10 message (inner string from ToolError::ProcessManagerError):
    //   "Task {task_id} not found"
    //
    // Subagent wording is out of scope — subagents didn't exist in 0.4.10.

    #[tokio::test]
    async fn legacy_get_task_not_found_exact_historical_message() {
        let resources = resources_with_terminal(None);
        let tool = TaskOutputTool;

        let mut ctx = test_ctx(resources.into_shared());
        ctx.extensions
            .insert(echo_agent_tool_runtime::BehaviorVersion(
                "legacy-0.4.10".to_string(),
            ));

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            ctx,
            TaskOutputToolInput {
                task_ids: vec!["task-xyz".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::TaskNotFound(msg) => {
                // Exact historical fixture — no trailing period.
                assert_eq!(msg, "Task task-xyz not found");
            }
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn current_get_task_not_found_includes_discoverability() {
        // Current (non-legacy) path must still include known task IDs
        // or "No background tasks" text for discoverability.
        let resources = resources_with_terminal(None);
        let tool = TaskOutputTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["task-xyz".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        match result {
            TaskOutputOutput::TaskNotFound(msg) => {
                assert!(
                    msg.contains("No background tasks or subagents exist"),
                    "Current path must include discoverability text, got: {msg}"
                );
            }
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }

    // ── Subagent running snapshot formatting ─────────────────────────────

    #[test]
    fn format_initializing_subagent_reports_status() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-init".to_string(),
            description: "Check PR status".to_string(),
            subagent_type: "general-purpose".to_string(),
            persona: None,
            status: SubagentSnapshotStatus::Initializing,
            started_at_epoch_ms: 1_700_000_000_000,
            duration_ms: 8_500,
        };
        let result = format_subagent_snapshot(&snap, WaitHint::NotRequested);
        match result {
            TaskOutputOutput::Result(r) => {
                assert_eq!(r.task_id, "sub-init");
                assert_eq!(r.status, "initializing");
                assert!(r.exit_code.is_none());
                assert!(r.ended.is_none());
                assert!(
                    r.output.contains("initializing"),
                    "output should mention initializing: {}",
                    r.output
                );
                assert!(
                    r.output.contains("8.5s"),
                    "output should contain elapsed time: {}",
                    r.output
                );
                assert!(
                    r.output.contains("timeout_ms"),
                    "output should suggest timeout_ms: {}",
                    r.output
                );
            }
            other => panic!("Expected Result, got {:?}", other),
        }
    }

    #[test]
    fn format_running_subagent_includes_progress_fields() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-abc".to_string(),
            description: "Find all API endpoints".to_string(),
            subagent_type: "explore".to_string(),
            persona: None,
            status: SubagentSnapshotStatus::Running {
                turn_count: 3,
                tool_call_count: 12,
                tokens_used: 45_000,
                context_window_tokens: 128_000,
                context_usage_pct: 35,
                tools_used: vec![
                    "bash".to_string(),
                    "read_file".to_string(),
                    "grep".to_string(),
                ],
                error_count: 0,
            },
            started_at_epoch_ms: 1_700_000_000_000,
            duration_ms: 12_500,
        };
        let result = format_subagent_snapshot(&snap, WaitHint::NotRequested);
        match result {
            TaskOutputOutput::Result(r) => {
                assert_eq!(r.task_id, "sub-abc");
                assert_eq!(r.status, "running");
                assert!(r.exit_code.is_none());
                assert!(r.ended.is_none());
                // Progress line
                assert!(
                    r.output.contains("turn 3"),
                    "should contain turn count: {}",
                    r.output
                );
                assert!(
                    r.output.contains("12 tool calls"),
                    "should contain tool call count: {}",
                    r.output
                );
                assert!(
                    r.output.contains("45K/128K tokens"),
                    "should contain token usage: {}",
                    r.output
                );
                assert!(
                    r.output.contains("35% context"),
                    "should contain context pct: {}",
                    r.output
                );
                // Tools used
                assert!(
                    r.output.contains("bash, read_file, grep"),
                    "should contain tools list: {}",
                    r.output
                );
                // Errors
                assert!(
                    r.output.contains("Errors: 0"),
                    "should contain error count: {}",
                    r.output
                );
            }
            other => panic!("Expected Result, got {:?}", other),
        }
    }

    // raw_output_bytes is body-only so identical Running state is stable across WaitHints.
    #[test]
    fn format_running_subagent_raw_output_bytes_stable_across_wait_hints() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-stable".to_string(),
            description: "stable body".to_string(),
            subagent_type: "explore".to_string(),
            persona: None,
            status: SubagentSnapshotStatus::Running {
                turn_count: 1,
                tool_call_count: 2,
                tokens_used: 3_000,
                context_window_tokens: 128_000,
                context_usage_pct: 2,
                tools_used: vec!["bash".to_string()],
                error_count: 0,
            },
            started_at_epoch_ms: 1_700_000_000_000,
            duration_ms: 1_000,
        };
        let expected_body = format!(
            "Subagent is still running.\n\
             Type: {}\n\
             Description: {}\n\
             Elapsed: {:.1}s\n\
             Progress: turn 1, 2 tool calls, \
             3K/128K tokens (2% context)\n\
             Tools used: bash\n\
             Errors: 0",
            snap.subagent_type,
            snap.description,
            snap.duration_ms as f64 / 1000.0,
        );
        let not_requested = match format_subagent_snapshot(&snap, WaitHint::NotRequested) {
            TaskOutputOutput::Result(r) => r,
            other => panic!("Expected Result, got {:?}", other),
        };
        let clamped = match format_subagent_snapshot(
            &snap,
            WaitHint::Elapsed {
                requested: Duration::from_secs(2_400),
                waited: Duration::from_secs(600),
            },
        ) {
            TaskOutputOutput::Result(r) => r,
            other => panic!("Expected Result, got {:?}", other),
        };
        assert_eq!(not_requested.raw_output_bytes, expected_body.len());
        assert_eq!(clamped.raw_output_bytes, expected_body.len());
        assert_eq!(not_requested.raw_output_bytes, clamped.raw_output_bytes);
        assert!(not_requested.output.starts_with(&expected_body));
        assert!(clamped.output.starts_with(&expected_body));
        assert_ne!(
            not_requested.output.len(),
            clamped.output.len(),
            "hint variants must still produce different formatted output"
        );
    }

    #[test]
    fn format_running_subagent_with_no_tools_shows_none_yet() {
        let snap = SubagentSnapshot {
            subagent_id: "sub-new".to_string(),
            description: "just started".to_string(),
            subagent_type: "general-purpose".to_string(),
            persona: None,
            status: SubagentSnapshotStatus::Running {
                turn_count: 0,
                tool_call_count: 0,
                tokens_used: 0,
                context_window_tokens: 128_000,
                context_usage_pct: 0,
                tools_used: vec![],
                error_count: 0,
            },
            started_at_epoch_ms: 1_700_000_000_000,
            duration_ms: 500,
        };
        let result = format_subagent_snapshot(&snap, WaitHint::NotRequested);
        match result {
            TaskOutputOutput::Result(r) => {
                assert!(
                    r.output.contains("none yet"),
                    "empty tools should show 'none yet': {}",
                    r.output
                );
                assert!(
                    r.output.contains("turn 0"),
                    "should show turn 0: {}",
                    r.output
                );
            }
            other => panic!("Expected Result, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn empty_task_id_fails_validation() {
        let resources = resources_with_terminal(None);
        let tool = TaskOutputTool;

        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec![String::new()],
                timeout_ms: None,
            },
        )
        .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("task_id")
                || err_msg.contains("task_ids")
                || err_msg.contains("empty"),
            "expected validation error, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn multi_task_ids_poll_returns_multi_result_mode_poll() {
        let resources = resources_with_terminal(None);
        let tool = TaskOutputTool;
        let out = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["missing-a".into(), "missing-b".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        match out {
            TaskOutputOutput::MultiResult(m) => {
                assert_eq!(m.mode, "poll");
                assert_eq!(m.results.len(), 2);
                assert!(m.results.iter().all(|r| r.status == "not_found"));
            }
            other => panic!("expected MultiResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn one_element_task_ids_returns_single_result_not_multi() {
        let resources = resources_with_terminal(None);
        let tool = TaskOutputTool;
        let out = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["only-one".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        match out {
            TaskOutputOutput::TaskNotFound(_) | TaskOutputOutput::Result(_) => {}
            TaskOutputOutput::MultiResult(_) => {
                panic!("single resolved id must not use MultiResult envelope")
            }
        }
    }

    // ── Subagent backend query fallback tests ────────────────────────────

    /// Build resources with a terminal that returns None (task not found)
    /// and a SubagentBackendResource backed by the unified event channel.
    fn resources_with_backend_query() -> (
        Resources,
        tokio::sync::mpsc::UnboundedReceiver<
            crate::implementations::echo_agent_build::task::types::SubagentEvent,
        >,
    ) {
        use crate::implementations::echo_agent_build::task::backend::{
            ChannelBackend, SubagentBackendResource,
        };
        use crate::implementations::echo_agent_build::task::types::SubagentEvent;
        use std::sync::Arc as StdArc;

        let mut resources = resources_with_terminal(None);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
        resources.insert(SubagentBackendResource(StdArc::new(ChannelBackend::new(
            tx,
        ))));
        (resources, rx)
    }

    /// Extract a `SubagentQueryRequest` from a `SubagentEvent`, panicking on wrong variant.
    fn unwrap_query(
        event: crate::implementations::echo_agent_build::task::types::SubagentEvent,
    ) -> crate::implementations::echo_agent_build::task::types::SubagentQueryRequest {
        match event {
            crate::implementations::echo_agent_build::task::types::SubagentEvent::Query(q) => q,
            _ => panic!("Expected SubagentEvent::Query"),
        }
    }

    #[tokio::test]
    async fn get_task_subagent_completed() {
        let (resources, mut query_rx) = resources_with_backend_query();
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let req = unwrap_query(query_rx.recv().await.unwrap());
            assert_eq!(req.subagent_id, "sub-done");
            req.respond_to
                .send(Some(SubagentSnapshot {
                    subagent_id: "sub-done".to_string(),
                    description: "find files".to_string(),
                    subagent_type: "explore".to_string(),
                    status: SubagentSnapshotStatus::Completed {
                        output: "Found 3 files".to_string(),
                        tool_calls: 5,
                        turns: 2,
                        worktree_path: None,
                    },
                    started_at_epoch_ms: 1_700_000_000_000,
                    duration_ms: 1500,
                    persona: None,
                }))
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskOutputTool,
            test_ctx(shared),
            TaskOutputToolInput {
                task_ids: vec!["sub-done".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        handle.await.unwrap();

        match result {
            TaskOutputOutput::Result(r) => {
                assert_eq!(r.task_id, "sub-done");
                assert_eq!(r.status, "completed");
                assert!(r.output.contains("Found 3 files"), "output: {}", r.output);
            }
            other => panic!("Expected Result(completed), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn get_task_subagent_running() {
        let (resources, mut query_rx) = resources_with_backend_query();
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let req = unwrap_query(query_rx.recv().await.unwrap());
            req.respond_to
                .send(Some(SubagentSnapshot {
                    subagent_id: "sub-run".to_string(),
                    description: "exploring".to_string(),
                    subagent_type: "general-purpose".to_string(),
                    status: SubagentSnapshotStatus::Running {
                        turn_count: 2,
                        tool_call_count: 5,
                        tokens_used: 10_000,
                        context_window_tokens: 128_000,
                        context_usage_pct: 8,
                        tools_used: vec!["grep".to_string()],
                        error_count: 0,
                    },
                    started_at_epoch_ms: 1_700_000_000_000,
                    duration_ms: 3000,
                    persona: None,
                }))
                .unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskOutputTool,
            test_ctx(shared),
            TaskOutputToolInput {
                task_ids: vec!["sub-run".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        handle.await.unwrap();

        match result {
            TaskOutputOutput::Result(r) => {
                assert_eq!(r.task_id, "sub-run");
                assert_eq!(r.status, "running");
                assert!(r.output.contains("still running"), "output: {}", r.output);
            }
            other => panic!("Expected Result(running), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn get_task_subagent_not_found_falls_through() {
        let (resources, mut query_rx) = resources_with_backend_query();
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let req = unwrap_query(query_rx.recv().await.unwrap());
            req.respond_to.send(None).unwrap();
        });

        let result = echo_agent_tool_runtime::Tool::run(
            &TaskOutputTool,
            test_ctx(shared),
            TaskOutputToolInput {
                task_ids: vec!["sub-nope".into()],
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        handle.await.unwrap();

        match result {
            TaskOutputOutput::TaskNotFound(msg) => {
                assert!(msg.contains("not found"), "msg: {msg}");
            }
            other => panic!("Expected TaskNotFound, got {:?}", other),
        }
    }

    fn completed_subagent_snapshot(id: &str) -> SubagentSnapshot {
        SubagentSnapshot {
            subagent_id: id.to_string(),
            description: "find files".to_string(),
            subagent_type: "explore".to_string(),
            status: SubagentSnapshotStatus::Completed {
                output: "Found 3 files".to_string(),
                tool_calls: 5,
                turns: 2,
                worktree_path: None,
            },
            started_at_epoch_ms: 1_700_000_000_000,
            duration_ms: 1500,
            persona: None,
        }
    }

    #[tokio::test]
    async fn blocking_get_on_already_completed_subagent_does_not_block() {
        let (resources, mut query_rx) = resources_with_backend_query();
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let req = unwrap_query(query_rx.recv().await.unwrap());
            assert!(
                req.block,
                "waits must issue a blocking query; backends short-circuit already-terminal"
            );
            req.respond_to
                .send(Some(completed_subagent_snapshot("sub-done")))
                .unwrap();
        });

        let started = std::time::Instant::now();
        let result = echo_agent_tool_runtime::Tool::run(
            &TaskOutputTool,
            test_ctx(shared),
            TaskOutputToolInput {
                task_ids: vec!["sub-done".into()],
                timeout_ms: Some(600_000),
            },
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "already-completed subagent must not burn the 600s cap; elapsed {:?}",
            started.elapsed()
        );
        handle.await.unwrap();

        match result {
            TaskOutputOutput::Result(r) => assert_eq!(r.status, "completed"),
            other => panic!("Expected Result(completed), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn blocking_get_on_still_running_subagent_still_blocks() {
        let (resources, mut query_rx) = resources_with_backend_query();
        let shared = resources.into_shared();

        let handle = tokio::spawn(async move {
            let req = unwrap_query(query_rx.recv().await.unwrap());
            assert!(req.block);
            tokio::time::sleep(Duration::from_millis(150)).await;
            req.respond_to
                .send(Some(SubagentSnapshot {
                    subagent_id: "sub-run".to_string(),
                    description: "exploring".to_string(),
                    subagent_type: "general-purpose".to_string(),
                    status: SubagentSnapshotStatus::Running {
                        turn_count: 2,
                        tool_call_count: 5,
                        tokens_used: 10_000,
                        context_window_tokens: 128_000,
                        context_usage_pct: 8,
                        tools_used: vec!["grep".to_string()],
                        error_count: 0,
                    },
                    started_at_epoch_ms: 1_700_000_000_000,
                    duration_ms: 3000,
                    persona: None,
                }))
                .unwrap();
        });

        let started = std::time::Instant::now();
        let result = echo_agent_tool_runtime::Tool::run(
            &TaskOutputTool,
            test_ctx(shared),
            TaskOutputToolInput {
                task_ids: vec!["sub-run".into()],
                timeout_ms: Some(5_000),
            },
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "still-running subagent wait must block; elapsed {:?}",
            started.elapsed()
        );
        handle.await.unwrap();

        match result {
            TaskOutputOutput::Result(r) => assert_eq!(r.status, "running"),
            other => panic!("Expected Result(running), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn blocking_get_on_unknown_task_returns_immediately() {
        let resources = resources_with_terminal(None);
        let started = std::time::Instant::now();
        let result = echo_agent_tool_runtime::Tool::run(
            &TaskOutputTool,
            test_ctx(resources.into_shared()),
            TaskOutputToolInput {
                task_ids: vec!["never-existed".into()],
                timeout_ms: Some(600_000),
            },
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "not-found wait must not burn the 600s cap; elapsed {:?}",
            started.elapsed()
        );
        match result {
            TaskOutputOutput::TaskNotFound(_) => {}
            other => panic!("Expected TaskNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn blocking_get_on_cancelled_and_failed_subagent_returns_immediately() {
        for (id, snapshot, expected) in [
            (
                "sub-cancel",
                SubagentSnapshot {
                    subagent_id: "sub-cancel".to_string(),
                    description: "cancelled".to_string(),
                    subagent_type: "explore".to_string(),
                    status: SubagentSnapshotStatus::Cancelled {
                        reason: Some("stop".to_string()),
                    },
                    started_at_epoch_ms: 1_700_000_000_000,
                    duration_ms: 10,
                    persona: None,
                },
                "cancelled",
            ),
            (
                "sub-fail",
                SubagentSnapshot {
                    subagent_id: "sub-fail".to_string(),
                    description: "failed".to_string(),
                    subagent_type: "explore".to_string(),
                    status: SubagentSnapshotStatus::Failed {
                        error: "boom".to_string(),
                    },
                    started_at_epoch_ms: 1_700_000_000_000,
                    duration_ms: 10,
                    persona: None,
                },
                "failed",
            ),
        ] {
            let (resources, mut query_rx) = resources_with_backend_query();
            let shared = resources.into_shared();
            let handle = tokio::spawn(async move {
                let req = unwrap_query(query_rx.recv().await.unwrap());
                assert!(req.block);
                req.respond_to.send(Some(snapshot)).unwrap();
            });
            let started = std::time::Instant::now();
            let result = echo_agent_tool_runtime::Tool::run(
                &TaskOutputTool,
                test_ctx(shared),
                TaskOutputToolInput {
                    task_ids: vec![id.to_string()],
                    timeout_ms: Some(600_000),
                },
            )
            .await
            .unwrap();
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "{expected} subagent must not burn the 600s cap; elapsed {:?}",
                started.elapsed()
            );
            handle.await.unwrap();
            match result {
                TaskOutputOutput::Result(r) => assert_eq!(r.status, expected),
                other => panic!("Expected Result({expected}), got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn wait_all_on_completed_subagent_does_not_block() {
        let (resources, mut query_rx) = resources_with_backend_query();
        let shared = resources.into_shared();
        let handle = tokio::spawn(async move {
            let req = unwrap_query(query_rx.recv().await.unwrap());
            assert!(
                !req.block,
                "wait-all resolve must snapshot, not block, a terminal subagent"
            );
            req.respond_to
                .send(Some(completed_subagent_snapshot("sub-done")))
                .unwrap();
            if query_rx.recv().await.is_some() {
                panic!("wait-all must not issue a second query");
            }
        });
        let started = std::time::Instant::now();
        let result = TaskOutputTool::run_multi_tasks(
            &["sub-done".into()],
            Some(600_000),
            shared,
            "get_command_or_subagent_output",
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "wait-all terminal subagent must not burn the 600s cap; elapsed {:?}",
            started.elapsed()
        );
        handle.await.unwrap();
        match result {
            TaskOutputOutput::MultiResult(m) => {
                assert_eq!(m.results.len(), 1);
                assert_eq!(m.results[0].status, "completed");
            }
            other => panic!("Expected MultiResult, got {other:?}"),
        }
    }
}
