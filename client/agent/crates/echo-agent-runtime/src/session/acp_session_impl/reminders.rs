//! System-reminder injection concern for `SessionActor`: reminder policy,
//! the TodoGate, date reminders, post-cancel interrupt framing, and
//! between-turn completion reminders.
use super::*;
/// Owned snapshot returned by [`SessionActor::collect_todo_gate_input`].
///
/// The borrowed `TodoGateInput<'_>` consumed by [`evaluate_todo_gate`]
/// is built from this owned data via [`Self::as_input`], which performs
/// the "first N in_progress are backed" insertion-order partition.
///
/// Exposed as `pub` solely so the replay-trace integration test in
/// `tests/trace_replay.rs` can drive the gate against synthetic JSON
/// fixtures. Not part of the public API.
#[doc(hidden)]
pub struct CollectedTodoGateInput {
    /// Pairs of `(id, content, status)` in `TodoState.todo_items_with_ids()`
    /// (insertion) order — `IndexMap` preserves this so the partition
    /// between backed and unbacked in-progress items is deterministic.
    pub todos: Vec<(String, String, crate::tools::todo::TodoStatus)>,
    /// `|outstanding subagents| + |incomplete bash/monitor tasks|` at
    /// the moment of gate evaluation.
    pub backing_task_count: usize,
}
impl CollectedTodoGateInput {
    /// Borrowed view used by [`evaluate_todo_gate`]. Pure transformation —
    /// no I/O, no clones (besides the borrow into `&str`).
    ///
    /// Allocates exactly two `Vec<&str>`s: one for pending, one for
    /// in-progress (which is then split in place via `split_off` —
    /// the leading slice is reused as `in_progress_backed`, no extra
    /// allocation).
    pub fn as_input(&self) -> TodoGateInput<'_> {
        use crate::tools::todo::TodoStatus;
        let mut pending = Vec::new();
        let mut in_progress: Vec<&str> = Vec::new();
        for (_, content, status) in &self.todos {
            match status {
                TodoStatus::Pending => pending.push(content.as_str()),
                TodoStatus::InProgress => in_progress.push(content.as_str()),
                TodoStatus::Completed | TodoStatus::Cancelled => {}
            }
        }
        let backed_count = in_progress.len().min(self.backing_task_count);
        let in_progress_unbacked = in_progress.split_off(backed_count);
        let in_progress_backed = in_progress;
        TodoGateInput {
            pending,
            in_progress_unbacked,
            in_progress_backed,
            backing_task_count: self.backing_task_count,
        }
    }
}
/// Inputs to `evaluate_todo_gate`. All fields are deliberately owned
/// borrows from the gate's call-site so the helper is a pure function.
///
/// The struct itself is `pub` (with `#[doc(hidden)]`) only so the
/// replay-trace integration test in `tests/trace_replay.rs` can name
/// the type as `&TodoGateInput<'_>` when calling `evaluate_todo_gate`.
/// Fields stay crate-private — the test never constructs the struct
/// directly; it obtains an instance via `CollectedTodoGateInput::as_input()`.
#[doc(hidden)]
pub struct TodoGateInput<'a> {
    pub(super) pending: Vec<&'a str>,
    pub(super) in_progress_unbacked: Vec<&'a str>,
    pub(super) in_progress_backed: Vec<&'a str>,
    pub(super) backing_task_count: usize,
}
impl TodoGateReason {
    /// Wire-string form, byte-identical to a `TODO_GATE_*` const in
    /// `crate::session::events`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InFlight => crate::session::events::TODO_GATE_IN_FLIGHT,
        }
    }
}
/// Pure decision function: does the gate fire, and with what reminder?
///
/// The function does NOT consult the cap — the caller folds the cap check
/// in around this function. Keeping cap logic out makes `evaluate_todo_gate`
/// trivially testable.
///
/// Exposed as `pub` solely so the replay-trace integration test in
/// `tests/trace_replay.rs` can call the gate directly. Not part of the
/// public API.
#[doc(hidden)]
pub fn evaluate_todo_gate(input: &TodoGateInput<'_>) -> TodoGateDecision {
    if input.pending.is_empty() && input.in_progress_unbacked.is_empty() {
        return TodoGateDecision::Continue;
    }
    TodoGateDecision::Nudge {
        reminder: build_todo_gate_reminder(&input.pending, &input.in_progress_unbacked),
        reason: TodoGateReason::InFlight,
    }
}
/// Build the in-flight TodoGate reminder text.
///
/// Uses the doubled-`${{{{ tools.by_kind.* }}}}` convention so the
/// caller's `format!` pass leaves a single `${{ tools.by_kind.* }}`
/// for `TemplateRenderer` / `render_prompt` to resolve into the
/// model-facing tool name.
pub(super) fn build_todo_gate_reminder(pending: &[&str], unbacked_in_progress: &[&str]) -> String {
    use std::fmt::Write as _;
    let mut buf =
        String::from("You have outstanding todos but ended your turn without a tool call.\n\n");
    if !unbacked_in_progress.is_empty() {
        buf.push_str("In-progress (no backing background task):\n");
        for c in unbacked_in_progress {
            let _ = writeln!(buf, "- {c}");
        }
        buf.push('\n');
    }
    if !pending.is_empty() {
        buf.push_str("Pending:\n");
        for c in pending {
            let _ = writeln!(buf, "- {c}");
        }
        buf.push('\n');
    }
    let _ = write!(
        buf,
        "Per <task_completion_discipline>, advance the next pending todo \
         with the appropriate tool call NOW. If you have a genuine external \
         blocker (missing credential, denied permission, network unreachable), \
         state it explicitly AND mark the affected todos `cancelled` via \
         ${{{{ tools.by_kind.plan }}}} with a reason in the same turn."
    );
    buf
}
/// Resolve the runtime `ReminderPolicy` from the resolved inputs.
///
/// Precedence: CLI `--todo-gate` > remote `/settings` > built-in default
/// (which is disabled). Extracted from `spawn_session_actor` so the
/// precedence rules are unit-testable. Named `resolve_*` to match the
/// sibling precedence helpers in `crate::util::config`
/// (`resolve_zdr_access_enabled`, `resolve_restore_code`, …).
pub(crate) fn resolve_reminder_policy(
    remote: Option<&crate::util::config::RemoteSettings>,
    todo_gate: bool,
) -> echo_agent_core::ReminderPolicy {
    let mut policy = echo_agent_core::ReminderPolicy::default();
    if let Some(remote) = remote {
        if let Some(enabled) = remote.todo_gate_enabled {
            policy.todo_gate.enabled = enabled;
        }
        if let Some(cap) = remote.todo_gate_max_fires_per_prompt {
            policy.todo_gate.max_fires_per_prompt = cap;
        }
    }
    if todo_gate {
        policy.todo_gate.enabled = true;
    }
    policy
}
/// Build the date-rollover reminder when the local calendar
/// date has advanced past the date last surfaced to the model.
///
/// Returns `None` when the date is unchanged (already announced) or has moved
/// backwards (e.g. a manual clock adjustment), so the caller injects nothing
/// in the common case. Pure (no `self`, no clock access) so the rollover
/// boundary logic is unit-testable; see `reminder_policy_tests`.
pub(crate) fn date_rollover_reminder(
    today: chrono::NaiveDate,
    last_announced: chrono::NaiveDate,
) -> Option<String> {
    if today <= last_announced {
        return None;
    }
    Some(format!(
        "The local date has changed since this session started. Today's date is now \
         {today}. Any date shown earlier in this session was set at startup and is now stale; \
         use {today} as the current date."
    ))
}
const WORKFLOW_RESULT_SUMMARY_REMINDER_CAP: usize = 4 * 1024;
pub(super) const WORKFLOW_OBJECTIVE_REMINDER_CAP: usize = 256;
fn workflow_completion_detail(detail: &str) -> std::borrow::Cow<'_, str> {
    let normalized = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized == detail {
        echo_agent_tools::util::truncate_str_with_marker(
            detail,
            WORKFLOW_RESULT_SUMMARY_REMINDER_CAP,
        )
    } else {
        std::borrow::Cow::Owned(
            echo_agent_tools::util::truncate_str_with_marker(
                &normalized,
                WORKFLOW_RESULT_SUMMARY_REMINDER_CAP,
            )
            .into_owned(),
        )
    }
}
impl SessionActor {
    pub(super) fn push_workflow_launch_reminder(
        &self,
        display_name: &str,
        run_id: &str,
        objective: &str,
        command_line: &str,
        resumed: bool,
    ) {
        let verb = if resumed { "resumed" } else { "launched" };
        let command_line = command_line
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut body = format!(
            "The user {verb} background workflow '{display_name}' (run id {run_id}) with the \
             slash command: {}\nThis was handled host-side; no tool call was involved.",
            echo_agent_tools::util::truncate_str(&command_line, WORKFLOW_OBJECTIVE_REMINDER_CAP)
        );
        let objective = objective.split_whitespace().collect::<Vec<_>>().join(" ");
        let objective_redundant = !objective.is_empty()
            && (objective == command_line || command_line.ends_with(&format!(" {objective}")));
        if !objective.is_empty() && !objective_redundant {
            body.push_str(&format!(
                "\nObjective: {}",
                echo_agent_tools::util::truncate_str(&objective, WORKFLOW_OBJECTIVE_REMINDER_CAP)
            ));
        }
        body.push_str(&format!(
            "\nIt runs in the background: status snapshots and the final result arrive as \
             reminders at turn starts, and the user can watch it in /workflow runs. If it pauses, \
             it can be resumed by calling the workflow tool with resume_from_run_id: \
             \"{run_id}\". Keep run ids internal — the user knows runs by display name. No \
             action needed unless the user asks."
        ));
        self.push_system_reminder(&body);
    }
    pub(super) async fn inject_workflow_status_reminder(&self) {
        if self.goal_loop_active() {
            return;
        }
        let tracker = self.workflow_tracker().await;
        let report = tracker.lock().take_status_report();
        if report.is_empty() {
            return;
        }
        self.push_system_reminder(&format_workflow_status_reminder(&report));
    }
}
fn format_workflow_status_reminder(
    runs: &[crate::session::workflow::tracker::WorkflowRunState],
) -> String {
    use std::fmt::Write as _;
    let n = runs.len();
    let noun = if n == 1 {
        "background workflow run"
    } else {
        "background workflow runs"
    };
    let mut buf = format!("Status of {n} {noun} in this session:\n");
    for run in runs {
        let _ = write!(
            buf,
            "\n- Workflow '{}' (run id {}) — status: {}",
            run.name,
            run.run_id,
            run.status.as_str()
        );
        let objective = run
            .objective
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !objective.is_empty() {
            let _ = write!(
                buf,
                "\n  Objective: {}",
                echo_agent_tools::util::truncate_str(&objective, WORKFLOW_OBJECTIVE_REMINDER_CAP)
            );
        }
        if let Some(line) = workflow_phase_line(run) {
            let _ = write!(buf, "\n  {line}");
        }
        if let Some(line) = workflow_agents_line(&run.agents) {
            let _ = write!(buf, "\n  {line}");
        }
        match run.agent_budget {
            Some(budget) => {
                let _ = write!(buf, "\n  Agents: {} of {} budget", run.agents_used, budget);
            }
            None if run.agents_used > 0 => {
                let _ = write!(buf, "\n  Agents: {}", run.agents_used);
            }
            None => {}
        }
        if run.agent_usage_incomplete {
            let _ = write!(
                buf,
                "\n  Agent accounting incomplete: this run predates logical-agent \
                 budgeting or contains legacy unresolved reservations"
            );
        }
        let _ = write!(
            buf,
            "\n  Elapsed: {}",
            format_workflow_elapsed(run.elapsed_ms_floor)
        );
        if run.status.is_paused() {
            if let Some(msg) = run.pause_message.as_deref() {
                let _ = write!(
                    buf,
                    "\n  Paused: {}",
                    echo_agent_tools::util::truncate_str(msg, WORKFLOW_RESULT_SUMMARY_REMINDER_CAP)
                );
            }
            let max_budget_exhausted = run.status
                == crate::session::workflow::tracker::WorkflowRunStatus::BudgetLimited
                && run.agents_used >= echo_agent_workflow::MAX_AGENT_BUDGET;
            if max_budget_exhausted {
                let _ = write!(buf, "\n  Not resumable: start a new workflow run.");
            } else {
                let budget_suffix = if run.status
                    == crate::session::workflow::tracker::WorkflowRunStatus::BudgetLimited
                {
                    " and a raised agent_budget (the resume is rejected while usage \
                     is at or over the cap)"
                } else {
                    ""
                };
                let _ = write!(
                    buf,
                    "\n  Resumable: call the workflow tool with resume_from_run_id: \"{}\"{}.",
                    run.run_id, budget_suffix
                );
            }
        }
    }
    buf.push_str(
        "\nThese run in the background — do not poll task tools for them; updates arrive as \
         reminders. Keep run ids internal (the user knows runs by display name).",
    );
    buf
}
/// "Phase: {title} ({i}/{n})" for a run's current phase, if any; a stale
/// title absent from the phase list renders bare. Shared by the model-facing
/// status reminder and the user-facing `/workflow` overview.
pub(super) fn workflow_phase_line(
    run: &crate::session::workflow::tracker::WorkflowRunState,
) -> Option<String> {
    let cur = run.current_phase.as_deref()?;
    Some(match run.phases.iter().position(|p| p.title == cur) {
        Some(pos) => format!("Phase: {} ({}/{})", cur, pos + 1, run.phases.len()),
        None => format!("Phase: {cur}"),
    })
}
/// "Agents: {done} done[, {running} running][, {failed} failed]" for a
/// non-empty roster. Shared like [`workflow_phase_line`].
pub(super) fn workflow_agents_line(
    agents: &[crate::session::workflow::tracker::WorkflowAgentRow],
) -> Option<String> {
    if agents.is_empty() {
        return None;
    }
    let done = agents.iter().filter(|a| a.state == "done").count();
    let running = agents.iter().filter(|a| a.state == "running").count();
    let failed = agents.iter().filter(|a| a.state == "failed").count();
    let mut parts = vec![format!("{done} done")];
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    Some(format!("Agents: {}", parts.join(", ")))
}
pub(super) fn format_workflow_elapsed(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}
fn format_workflow_completion_reminder(
    runs: &[crate::session::workflow::tracker::WorkflowRunState],
    session_dir: &std::path::Path,
    before_resume: bool,
    read_tool_name: Option<&str>,
) -> String {
    use std::fmt::Write as _;
    let n = runs.len();
    let noun = if n == 1 {
        "background workflow run"
    } else {
        "background workflow runs"
    };
    let verb = if runs.iter().any(|r| !r.status.is_terminal()) {
        "stopped (finished or paused)"
    } else {
        "finished"
    };
    let mut buf = if before_resume {
        format!("This session was resumed. {n} {noun} {verb} before the resume:\n")
    } else {
        format!("While you were idle, {n} {noun} {verb}:\n")
    };
    for run in runs {
        let _ = write!(
            buf,
            "\n- Workflow '{}' (run id {}) — status: {}",
            run.name,
            run.run_id,
            run.status.as_str()
        );
        let objective = run
            .objective
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !objective.is_empty() {
            let _ = write!(
                buf,
                "\n  Objective: {}",
                echo_agent_tools::util::truncate_str(&objective, WORKFLOW_OBJECTIVE_REMINDER_CAP)
            );
        }
        let _ = write!(
            buf,
            "\n  Elapsed: {}",
            format_workflow_elapsed(run.elapsed_ms_floor)
        );
        if let Some(summary) = run.result_summary.as_deref() {
            let capped =
                echo_agent_tools::util::truncate_str(summary, WORKFLOW_RESULT_SUMMARY_REMINDER_CAP);
            buf.push_str("\n  Result:\n");
            for line in capped.lines() {
                let _ = writeln!(buf, "    {line}");
            }
            if capped.len() < summary.len() {
                let _ = writeln!(
                    buf,
                    "    [... result truncated ({} bytes total)]",
                    summary.len()
                );
            }
        } else if let Some(detail) = run.pause_message.as_deref() {
            let detail = workflow_completion_detail(detail);
            let _ = write!(buf, "\n  Detail: {detail}\n");
        } else {
            buf.push('\n');
        }
        if run.status == crate::session::workflow::tracker::WorkflowRunStatus::BudgetLimited {
            if run.agents_used >= echo_agent_workflow::MAX_AGENT_BUDGET {
                let _ = writeln!(
                    buf,
                    "  Not resumable: this run reached the maximum agent budget; start a new \
                     workflow run."
                );
            } else {
                let _ = writeln!(
                    buf,
                    "  Resumable: call the workflow tool with resume_from_run_id: \"{}\" \
                     and a raised agent_budget (the resume is rejected while usage is at \
                     or over the cap).",
                    run.run_id
                );
            }
        }
        if run.status == crate::session::workflow::tracker::WorkflowRunStatus::Failed {
            let _ = writeln!(
                buf,
                "  Resumable: call the workflow tool with resume_from_run_id: \"{}\" — \
                 completed agents replay from the journal and the failed step re-executes.",
                run.run_id
            );
        }
        let report_path = session_dir
            .join("workflows")
            .join(&run.run_id)
            .join("scratch")
            .join("report.md");
        if report_path.is_file() {
            let _ = writeln!(
                buf,
                "  Full report: {} (use {} on that path to view it)",
                report_path.display(),
                read_tool_name.unwrap_or("Read"),
            );
        }
    }
    buf
}
/// TodoGate when enabled and the prompt carries `<task_completion_discipline>`
/// (`{DISCIPLINE_BLOCK}`), but NOT while the goal loop is active — the
/// continuation directive drives the loop there (see the body).
pub(super) fn todo_gate_active(
    policy: &echo_agent_core::system_reminder::ReminderPolicy,
    audience: echo_agent_core::prompt::context::PromptAudience,
    definition: &AgentDefinition,
    goal_harness_enabled: bool,
    goal_status: Option<crate::session::goal_tracker::GoalStatus>,
) -> bool {
    if !policy.todo_gate.enabled {
        return false;
    }
    if laziness_injection_active(goal_harness_enabled, goal_status) {
        return false;
    }
    definition.carries_task_completion_discipline(audience)
}
impl SessionActor {
    /// Injects a one-shot date-rollover `<system-reminder>` when a long session crosses local
    /// midnight, since the cached `<user_info>` prefix keeps its startup date to preserve the prompt
    /// cache. Self-dedupes via `last_announced_local_date` (at most once per day). Skipped for
    /// date-free templates and the harness that owns this surface.
    pub(super) async fn maybe_inject_date_rollover_reminder(&self) {
        let template_surfaces_date = self
            .agent
            .borrow()
            .definition()
            .user_message_template
            .surfaces_local_date();
        if !template_surfaces_date && !self.prefix_carries_fallback_date.get() {
            return;
        }
        let today = chrono::Local::now().date_naive();
        let last = self.last_announced_local_date.get();
        let Some(reminder) = date_rollover_reminder(today, last) else {
            return;
        };
        self.last_announced_local_date.set(today);
        self.push_system_reminder(&reminder);
        tracing::debug!(
            previous = %last,
            today = %today,
            "Injected date rollover reminder"
        );
    }
    /// Frame the already-assembled user turn when a mid-stream abort left the model no other
    /// signal.
    /// One-shot; skipped for the harness that owns this surface.
    /// Verbatim prompts still consume the flag (this is the next real user turn) but keep the
    /// caller-owned bytes, matching truncation and send-now.
    /// Callers must gate to `PromptOrigin::User` so synthetic turns leave the flag.
    pub(super) fn maybe_apply_interrupt_envelope(
        &self,
        user_message: String,
        verbatim: bool,
    ) -> String {
        if !self.events.take_pending_interrupt_reminder() {
            return user_message;
        }
        if verbatim {
            return user_message;
        }
        frame_user_turn(INTERRUPT_NOTE, &user_message)
    }
    /// Push a `<system-reminder>`-wrapped user message into the conversation.
    pub(super) fn push_system_reminder(&self, content: &str) {
        self.push_system_reminder_with_tag(content, "system-reminder");
    }
    /// The active reminder wrapper tag, backed by the canonical tag constants
    /// in `echo_agent_tools::reminders`.
    pub(super) fn reminder_wrapper_tag(&self) -> &'static str {
        echo_agent_tools::reminders::DEFAULT_REMINDER_TAG
    }
    /// Push a `<{tag}>`-wrapped user message.
    pub(super) fn push_system_reminder_with_tag(&self, content: &str, tag: &str) {
        let content = content.replace(&format!("</{tag}>"), &format!("<\\/{tag}>"));
        let message = ConversationItem::system_reminder(format!("<{tag}>\n{content}\n</{tag}>"));
        self.chat_state_handle.push_user_message(message);
    }
    /// Mark completion IDs as reported in the shared
    /// `ReportedTaskCompletions` state so the per-tool-call
    /// `TaskCompletionReminder` won't (re-)surface them. Used both to dedupe
    /// completions the model actually saw (notification-drain / started
    /// auto-wake prompts) and to drop them during the goal loop (between-turn drain).
    /// No-op on an empty list.
    pub(super) async fn mark_completions_reported(&self, ids: &[&str]) {
        if ids.is_empty() {
            return;
        }
        use echo_agent_tools::reminders::task_completion::ReportedTaskCompletions;
        use echo_agent_tools::types::resources::State;
        let bridge = self.agent.borrow().tool_bridge().clone();
        let resources = bridge.shared_resources().await;
        let mut res = resources.lock().await;
        let reported = res.get_or_default::<State<ReportedTaskCompletions>>();
        for id in ids {
            reported.mark_reported(id);
        }
    }
    pub(super) async fn drain_between_turn_completions(&self) {
        let goal_loop_active = self.goal_loop_active();
        let bridge = self.agent.borrow().tool_bridge().clone();
        let reserved = self
            .tool_context
            .task_completion_reservations
            .as_ref()
            .map(|reservations| reservations.snapshot())
            .unwrap_or_default();
        let bash_completions = bridge.drain_between_turn_bash_completions(&reserved).await;
        if !bash_completions.is_empty() {
            let ids: Vec<&str> = bash_completions
                .iter()
                .map(|t| t.task_id.as_str())
                .collect();
            if goal_loop_active {
                tracing::info!(
                    count = bash_completions.len(),
                    task_ids = ?ids,
                    "dropping between-turn bash task completions (goal loop active)"
                );
                self.mark_completions_reported(&ids).await;
            } else {
                tracing::info!(
                    count = bash_completions.len(),
                    task_ids = ?ids,
                    "draining between-turn bash task completions"
                );
                let task_output_name =
                    echo_agent_tools::reminders::task_completion::resolve_task_output_tool_name(
                        &bridge,
                    )
                    .await;
                let read_tool_name =
                    echo_agent_tools::reminders::task_completion::resolve_read_tool_name(&bridge)
                        .await;
                let reminder = echo_agent_tools::reminders::task_completion::format_between_turn_bash_completions(
                    &bash_completions,
                    task_output_name.as_deref(),
                    read_tool_name.as_deref(),
                );
                self.push_system_reminder(&reminder);
            }
        }
        self.drain_between_turn_workflow_completions(goal_loop_active)
            .await;
        let Some(tx) = &self.tool_context.subagent_event_tx else {
            return;
        };
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentCompletionsRequest, SubagentEvent,
        };
        let suppress_ids = self
            .tool_context
            .task_completion_reservations
            .as_ref()
            .map(|reservations| reservations.snapshot())
            .unwrap_or_default();
        let parent_session_id = Some(self.session_id_string());
        let (respond_to, rx) = tokio::sync::oneshot::channel();
        if tx
            .send(SubagentEvent::Completions(SubagentCompletionsRequest {
                parent_session_id,
                suppress_ids,
                respond_to,
            }))
            .is_err()
        {
            return;
        }
        let Ok(completions) = rx.await else {
            return;
        };
        if completions.is_empty() {
            return;
        }
        let ids: Vec<&str> = completions.iter().map(|c| c.subagent_id.as_str()).collect();
        if goal_loop_active {
            tracing::info!(
                count = completions.len(),
                subagent_ids = ?ids,
                "dropping between-turn subagent completions (goal loop active)"
            );
            self.mark_completions_reported(&ids).await;
            return;
        }
        tracing::info!(
            count = completions.len(),
            subagent_ids = ?ids,
            "draining between-turn subagent completions"
        );
        let reminder =
            echo_agent_tools::reminders::task_completion::format_between_turn_completion_reminder(
                &completions,
                &bridge,
            )
            .await;
        self.push_system_reminder(&reminder);
    }
    pub(super) async fn drain_between_turn_workflow_completions(&self, goal_loop_active: bool) {
        if goal_loop_active {
            return;
        }
        let (restored, fresh) = {
            let tracker = self.workflow_tracker().await;
            let mut tracker = tracker.lock();
            tracker.take_unreported_terminal_runs()
        };
        if restored.is_empty() && fresh.is_empty() {
            return;
        }
        let names = |runs: &[crate::session::workflow::tracker::WorkflowRunState]| {
            runs.iter().map(|r| r.name.clone()).collect::<Vec<_>>()
        };
        tracing::info!(
            restored = ?names(&restored),
            fresh = ?names(&fresh),
            "draining between-turn workflow completions"
        );
        let session_dir = crate::session::persistence::session_dir(&self.session_info);
        let bridge = self.tool_bridge_handle();
        let read_tool_name =
            echo_agent_tools::reminders::task_completion::resolve_read_tool_name(&bridge).await;
        if !restored.is_empty() {
            self.push_system_reminder(&format_workflow_completion_reminder(
                &restored,
                &session_dir,
                true,
                read_tool_name.as_deref(),
            ));
        }
        if !fresh.is_empty() {
            self.push_system_reminder(&format_workflow_completion_reminder(
                &fresh,
                &session_dir,
                false,
                read_tool_name.as_deref(),
            ));
        }
    }
    /// Persist a manifest of running background tasks to the session directory.
    ///
    /// Called during session shutdown (both explicit and channel-closed paths)
    /// so a resumed session can inform the model about processes that were
    /// still alive when the session ended.
    pub(super) async fn persist_background_task_manifest(&self) {
        let tasks = self
            .agent
            .borrow()
            .tool_bridge()
            .list_background_tasks()
            .await;
        let entries: Vec<crate::terminal::BackgroundTaskManifestEntry> = tasks
            .into_iter()
            .filter(|t| !t.completed)
            .map(|t| crate::terminal::BackgroundTaskManifestEntry {
                task_id: t.task_id,
                command: t.command,
                display_command: t.display_command,
                output_file: t.output_file,
                start_time: t.start_time,
                cwd: t.cwd,
                kind: t.kind,
            })
            .collect();
        if !entries.is_empty() {
            tracing::info!(
                count = entries.len(),
                "persisting background task manifest for session resume"
            );
        }
        let session_dir = crate::session::persistence::session_dir(&self.session_info);
        crate::terminal::persist_manifest(&session_dir, entries);
    }
    /// Load the background task manifest from a prior session and inject a
    /// system-reminder so the model knows about orphaned tasks.
    ///
    /// The manifest file is deleted after loading so it is only shown once.
    pub(super) fn inject_resumed_tasks_reminder(&self) {
        let session_dir = crate::session::persistence::session_dir(&self.session_info);
        let entries = crate::terminal::load_and_clear_manifest(&session_dir);
        if entries.is_empty() {
            return;
        }
        tracing::info!(
            count = entries.len(),
            "injecting resumed background tasks reminder"
        );
        let reminder = crate::terminal::format_resumed_tasks_reminder(&entries);
        self.push_system_reminder(&reminder);
    }
    /// Turn-end TodoGate config, or `None` when [`todo_gate_active`] is false.
    pub(super) fn todo_gate_policy(
        &self,
    ) -> Option<echo_agent_core::system_reminder::TodoGateConfig> {
        let goal_status = self.goal_tracker.lock().status();
        let agent = self.agent.borrow();
        let policy = agent.reminder_policy();
        let active = todo_gate_active(
            policy,
            agent.prompt_audience(),
            agent.definition(),
            self.goal_harness_enabled(),
            goal_status,
        );
        tracing::debug!(
            enabled = policy.todo_gate.enabled,
            goal_harness_enabled = self.goal_harness_enabled(),
            ?goal_status,
            active,
            "todo_gate_policy"
        );
        if !active {
            return None;
        }
        Some(policy.todo_gate)
    }
    /// Gather the inputs needed by `evaluate_todo_gate` from live session
    /// state.
    ///
    /// Each `.await` is preceded by an owned `Arc<ToolBridge>` clone
    /// from `tool_bridge_handle()` — no `RefCell::Ref<Agent>` guard is
    /// held across a suspension point.
    pub(super) async fn collect_todo_gate_input(&self, prompt_id: &str) -> CollectedTodoGateInput {
        use crate::tools::todo::{TodoState, TodoStatus};
        use echo_agent_tools::types::resources::State;
        let bridge = self.tool_bridge_handle();
        let todos: Vec<(String, String, TodoStatus)> = bridge
            .read_resource::<State<TodoState>>()
            .await
            .map(|state| {
                state
                    .0
                    .todo_items_with_ids()
                    .map(|(id, item)| (id.clone(), item.content.clone(), item.status))
                    .collect()
            })
            .unwrap_or_default();
        let outstanding_live = self
            .outstanding_reply_for_prompt(prompt_id)
            .await
            .map(|r| r.live_ids.len())
            .unwrap_or(0);
        let incomplete_terminal_tasks = bridge
            .list_background_tasks()
            .await
            .into_iter()
            .filter(echo_agent_tools::computer::types::TaskSnapshot::is_outstanding)
            .count();
        let backing_task_count = outstanding_live + incomplete_terminal_tasks;
        CollectedTodoGateInput {
            todos,
            backing_task_count,
        }
    }
}
#[cfg(test)]
mod workflow_reminder_tests {
    use super::*;
    use crate::session::workflow::tracker::{WorkflowRunState, WorkflowRunStatus};
    fn failed_run(detail: String) -> WorkflowRunState {
        WorkflowRunState {
            run_id: "wf_1".to_owned(),
            revision: 2,
            name: "demo".to_owned(),
            objective: "exercise formatter".to_owned(),
            status: WorkflowRunStatus::Failed,
            phases: Vec::new(),
            current_phase: None,
            agent_budget: None,
            agents_used: 0,
            token_leases: Vec::new(),
            agent_usage_incomplete: false,
            elapsed_ms_floor: 1_000,
            pause_message: Some(detail),
            history: Vec::new(),
            journal_path: None,
            result_summary: None,
            agents: Vec::new(),
        }
    }
    #[test]
    fn completion_detail_is_normalized_and_utf8_safely_capped_with_marker() {
        let detail = format!(
            "first\n\tsecond   {} tail",
            "😀".repeat(WORKFLOW_RESULT_SUMMARY_REMINDER_CAP)
        );
        let run = failed_run(detail);
        let session_dir = tempfile::tempdir().unwrap();
        let reminder = format_workflow_completion_reminder(&[run], session_dir.path(), false, None);
        let rendered_detail = reminder
            .split_once("  Detail: ")
            .unwrap()
            .1
            .lines()
            .next()
            .unwrap()
            .trim_end();
        assert!(reminder.contains("resume_from_run_id: \"wf_1\""));
        assert!(rendered_detail.starts_with("first second "));
        assert!(rendered_detail.ends_with('…'));
        assert!(rendered_detail.len() <= WORKFLOW_RESULT_SUMMARY_REMINDER_CAP);
        assert!(!rendered_detail.contains('\n'));
        assert!(!rendered_detail.contains('\t'));
        assert!(!rendered_detail.contains("  "));
    }
}
