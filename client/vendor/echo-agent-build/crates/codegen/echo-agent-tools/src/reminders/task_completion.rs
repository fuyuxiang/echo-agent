//! Background task and subagent completion reminder.
//!
//! On each tool call, [`TaskCompletionReminder`] queries the
//! [`TerminalBackend`] (already on `SharedResources`) via `list_tasks()`
//! and reports any newly-completed background tasks as plain reminder
//! text. It also queries the subagent coordinator via
//! [`SubagentEventSender`] for newly-completed subagents.
//!
//! The tool pipeline wraps each string in `<system-reminder>` tags
//! inside the tool result so the model learns about completions without
//! polling `get_task_output`.
//!
//! A [`ReportedTaskCompletions`] state set tracks which task/subagent IDs
//! have already been surfaced, preventing duplicate reminders.
use crate::bridge::ToolBridge;
use crate::implementations::echo_agent_build::task::types::{
    SubagentCompletionSummary, SubagentCompletionsRequest, SubagentEvent, SubagentEventSender,
};
use crate::types::TaskSnapshot;
use crate::types::output::ToolOutput;
use crate::types::resources::{SharedResources, State, Terminal};
use crate::types::tool::{Reminder, ToolKind};
use crate::util::truncate::{PREVIEW_SIZE, PartialOutput, truncate_with_preview};
use echo_agent_tool_types::KillTaskOutput;
use echo_agent_tool_types::SubagentCompletedOutput;
use echo_agent_tool_types::TaskOutputOutput;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
/// Default tool name used in auto-wake completion messages.
pub const DEFAULT_TASK_OUTPUT_TOOL: &str = "get_task_output";
/// UI/Stop kill with no live waiter: tell the model not to relaunch the task.
const USER_KILLED_NOTICE: &str = "This task was killed by the user — do not restart it.\n";
const COMPLETION_RECONCILIATION: &str = "Result of existing work, not a new request. Reconcile with the latest task state; skip completed or superseded actions. Continue only remaining work. Report changed findings briefly, or say the conclusion is unchanged.\n";
fn user_killed_notice(task: &TaskSnapshot) -> &'static str {
    if task.explicitly_killed && !task.kill_result_delivered {
        USER_KILLED_NOTICE
    } else {
        ""
    }
}
/// Inline preview cap applied ONLY to bash completion reminders that ship
/// with a disk-pointer footer. Subagent completions (which have no
/// disk-backed output file) are never truncated -- the inline branch is
/// their only chance to see the output.
const MAX_INLINE_COMPLETION_BYTES: usize = 4_000;
#[derive(Clone, Debug, Default)]
pub struct TaskCompletionReservations(pub Arc<std::sync::Mutex<HashMap<String, usize>>>);
impl TaskCompletionReservations {
    pub fn reserve(&self, id: String) {
        let mut ids = self.0.lock().unwrap_or_else(|e| e.into_inner());
        *ids.entry(id).or_default() += 1;
    }
    pub fn release(&self, id: &str) {
        let mut ids = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = ids.get_mut(id) {
            if *count > 1 {
                *count -= 1;
            } else {
                ids.remove(id);
            }
        }
    }
    pub fn contains(&self, id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }
    pub fn snapshot(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}
crate::register_resource!(
    "echo_agent_build",
    "TaskCompletionReservations",
    TaskCompletionReservations
);
#[derive(Clone, Debug, Default)]
pub struct TaskWakeSuppressed(pub Arc<std::sync::atomic::AtomicBool>);
impl TaskWakeSuppressed {
    pub fn set(&self, suppressed: bool) {
        self.0
            .store(suppressed, std::sync::atomic::Ordering::Release);
    }
    pub fn get(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}
crate::register_resource!("echo_agent_build", "TaskWakeSuppressed", TaskWakeSuppressed);
/// Set of task IDs whose completion has already been surfaced as a
/// `<system-reminder>`.  Persisted via `State<T>` so it survives across
/// tool calls within a session.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ReportedTaskCompletions {
    reported: HashSet<String>,
}
impl ReportedTaskCompletions {
    /// Returns `true` if the ID was newly inserted.
    pub fn mark_reported(&mut self, id: &str) -> bool {
        if self.reported.contains(id) {
            return false;
        }
        self.reported.insert(id.to_owned())
    }
}
crate::register_resource!(
    "echo_agent_build",
    "ReportedTaskCompletions",
    ReportedTaskCompletions
);
/// Format a model-facing message from a [`TaskSnapshot`].
///
/// `task_output_name` controls the pointer-vs-inline rendering of the output
/// section (see [`render_completion_output_delivery`]). When the inline
/// branch fires and `read_tool_name` is set, the output is truncated and
/// followed by a footer pointing the model at `task.output_file` so the
/// full log is still recoverable from disk.
pub fn format_bash_completion(
    task: &TaskSnapshot,
    task_output_name: Option<&str>,
    read_tool_name: Option<&str>,
) -> String {
    let command = task.display_command.as_deref().unwrap_or(&task.command);
    let duration_secs = task.duration_secs();
    let status_str = match task.signal.as_deref() {
        Some(sig) => format!("terminated by signal {sig}"),
        None => {
            let exit_code_str = task
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into());
            format!("exit code: {exit_code_str}")
        }
    };
    let notice = user_killed_notice(task);
    let mut msg = format!(
        "Background task \"{}\" completed ({}).\n\
         Command: {} | Duration: {:.1}s\n\
         {notice}",
        task.task_id, status_str, command, duration_secs,
    );
    msg.push_str(COMPLETION_RECONCILIATION);
    if task.signal.is_some() && duration_secs < 1.0 {
        msg.push_str(
            "Note: this is much shorter than expected for a backgrounded command. \
             The wrapper bash may have been killed by signal (e.g. `pkill -f <pat>` \
             matching its own argv) before the inner command ran. Re-check the \
             command for self-matching kill patterns, signals sent by the script \
             itself, or upstream sources of SIGTERM/SIGHUP.\n",
        );
    }
    let disk_pointer_footer = read_tool_name.map(|name| {
        format!(
            "Use {} on {} for full content",
            name,
            task.output_file.display()
        )
    });
    render_completion_output_delivery(
        &mut msg,
        &task.task_id,
        task.output_view(),
        task_output_name,
        disk_pointer_footer.as_deref(),
    );
    msg
}
/// Format a model-facing auto-wake message for a completed **monitor** task.
///
/// Uses the same `[monitor ended: <reason>]` wording as the monitor pipeline's
/// terminal `MonitorEvent`, but is delivered via the immediate `TaskCompleted`
/// synthetic-prompt path (same as bash). That path does not depend on the
/// idle-gated `InjectNotification` drain, which was easy to miss when a monitor
/// exited while the agent was idle and produced no further stdout.
pub fn format_monitor_completion(task: &TaskSnapshot, task_output_name: Option<&str>) -> String {
    let reason = match task.signal.as_deref() {
        Some(sig) => format!("killed by signal {sig}"),
        None => match task.exit_code {
            Some(code) => format!("exited (code {code})"),
            None => "ended".to_string(),
        },
    };
    let description = task
        .display_command
        .as_deref()
        .and_then(|d| d.strip_prefix("[monitor] "))
        .unwrap_or("monitor");
    let tool = task_output_name.unwrap_or(DEFAULT_TASK_OUTPUT_TOOL);
    let notice = user_killed_notice(task);
    format!(
        "Monitor \"{id}\" ended: [monitor ended: {reason}].\n\
         Description: {description}\n\
         Command: {cmd}\n\
         Duration: {dur:.1}s\n\
         Use {tool}(\"{id}\") for full output.\n\
         {notice}",
        id = task.task_id,
        cmd = task.command,
        dur = task.duration_secs(),
    )
}
/// Warn the model about other background tasks that are still running.
fn format_running_tasks_warning(running: &[&TaskSnapshot], kill_task_name: Option<&str>) -> String {
    use std::fmt::Write as _;
    let n = running.len();
    let label = if n == 1 { "task is" } else { "tasks are" };
    let mut buf = format!("Note: {n} other background {label} still running:\n");
    for task in running {
        let cmd = task.display_command.as_deref().unwrap_or(&task.command);
        let _ = writeln!(
            buf,
            "- \"{}\" (running for {:.0}s): {}",
            task.task_id,
            task.duration_secs(),
            cmd,
        );
    }
    let kill_name = kill_task_name.unwrap_or("kill_command_or_subagent");
    let _ = write!(
        buf,
        "Consider killing duplicate tasks with {kill_name} before launching new ones."
    );
    buf
}
/// Split a pre-wrapped `<monitor-event description="…" task_id="…">…</monitor-event>`
/// into `(description, inner_text)`. `wrap_monitor_event` is the single
/// writer with fixed attribute order; the `rfind` of `" task_id="`
/// tolerates quotes inside the model-supplied description. `None` =>
/// caller includes the text verbatim.
fn split_wrapped_monitor_event(event_text: &str) -> Option<(&str, &str)> {
    let rest = event_text.strip_prefix("<monitor-event description=\"")?;
    let open_end = rest.find(">\n")?;
    let open_tag = &rest[..open_end];
    let desc_end = open_tag.rfind("\" task_id=\"")?;
    let description = &open_tag[..desc_end];
    let inner = rest[open_end + 2..].strip_suffix("\n</monitor-event>")?;
    Some((description, inner))
}
/// Format drained [`MonitorEventNotification`]s for the turn loop's hidden
/// synthetic user message. Model-facing only — the pager renders monitor
/// events from the structured `echo.agent/monitor_event` notification, never by
/// parsing this text.
///
/// One event (also the common case for slow monitors, which drain one
/// event per loop-top pass) renders the lean form:
///
/// ```text
/// <monitor-event task_id="0199…">
/// [heartbeat] beat 1
/// </monitor-event>
/// ```
///
/// Multiple events batch under one count preamble, grouped per monitor
/// (first-seen order, within-monitor order kept). Identity — description
/// AND task id — is stated once on the group tag; tick lines carry only
/// ordinals (a 100-tick monitor must not repeat its description 100×):
///
/// ```text
/// <monitor description="heartbeat" task_id="0199…">
/// [1] beat 1
/// [2] beat 2
/// </monitor>
/// ```
///
/// Buffered `event_text` arrives pre-wrapped (`wrap_monitor_event`); it is
/// unwrapped via [`split_wrapped_monitor_event`] with verbatim fallback.
pub fn format_monitor_events(
    events: &[crate::implementations::echo_agent_build::monitor::types::MonitorEventNotification],
    task_output_name: Option<&str>,
) -> Option<String> {
    use std::fmt::Write as _;
    let tool_hint = task_output_name.unwrap_or("get_task_output");
    match events {
        [] => None,
        [event] => {
            let (label, inner) = match split_wrapped_monitor_event(&event.event_text) {
                Some((desc, inner)) if !desc.is_empty() => (desc, inner),
                Some((_, inner)) => ("event", inner),
                None => ("event", event.event_text.as_str()),
            };
            let label =
                crate::implementations::echo_agent_build::monitor::event::sanitize_monitor_description(
                    label,
                );
            Some(format!(
                "<monitor-event task_id=\"{}\">\n[{}] {}\n</monitor-event>",
                event.task_id, label, inner,
            ))
        }
        _ => {
            type Event =
                crate::implementations::echo_agent_build::monitor::types::MonitorEventNotification;
            let mut groups: Vec<(&str, Vec<&Event>)> = Vec::new();
            for event in events {
                match groups.iter_mut().find(|(id, _)| *id == event.task_id) {
                    Some((_, group)) => group.push(event),
                    None => groups.push((&event.task_id, vec![event])),
                }
            }
            let mut buf = format!(
                "{} monitor events from {} {} (use {} to identify each monitor):",
                events.len(),
                groups.len(),
                if groups.len() == 1 {
                    "monitor"
                } else {
                    "monitors"
                },
                tool_hint,
            );
            for (task_id, group) in &groups {
                let description = group
                    .iter()
                    .find_map(|e| split_wrapped_monitor_event(&e.event_text))
                    .map(|(desc, _)| desc)
                    .filter(|d| !d.is_empty())
                    .unwrap_or("event");
                let description = crate::implementations::echo_agent_build::monitor::event::sanitize_monitor_description(
                    description,
                );
                let _ = write!(
                    buf,
                    "\n\n<monitor description=\"{description}\" task_id=\"{task_id}\">"
                );
                for (n, event) in group.iter().enumerate() {
                    let inner = split_wrapped_monitor_event(&event.event_text)
                        .map(|(_, inner)| inner)
                        .unwrap_or(&event.event_text);
                    let _ = write!(buf, "\n[{}] {}", n + 1, inner);
                }
                buf.push_str("\n</monitor>");
            }
            Some(buf)
        }
    }
}
/// Whether a background task should be surfaced to the session whose owner id
/// is `my_owner`.
///
/// Subagents share the parent's terminal backend, so `list_tasks()` returns
/// tasks owned by other sessions (the parent, sibling subagents). A task is in
/// scope only when it has no recorded owner (legacy / non-echo-agent-build backends)
/// or its owner matches the current session; cross-session tasks are filtered
/// out so their completions surface in the owning session, not here.
pub(crate) fn task_owned_by_session(task: &TaskSnapshot, my_owner: Option<&str>) -> bool {
    match (my_owner, task.owner_session_id.as_deref()) {
        (Some(me), Some(owner)) => me == owner,
        _ => true,
    }
}
/// Append the completion-output delivery section for a bash task or subagent.
///
/// - `Some(name)` writes `Use {name}("{subagent_id}") to see the full output.`
///   (polling tool available; the model can pull the full output via that
///   tool on demand).
/// - `None` writes `response:\n{output}`. When `disk_pointer_footer` is
///   `Some(line)`, the output is capped at [`MAX_INLINE_COMPLETION_BYTES`]
///   and the footer line is appended so the model can recover the full log
///   from disk -- this is the bash-completion path. When
///   `disk_pointer_footer` is `None`, the full output is inlined verbatim
///   -- this is the subagent path (there is no disk-backed output file and
///   this notification is the model's only chance to see the output).
///
/// The pointer-vs-inline decision is centralised here so all completion
/// notification surfaces stay in lock-step. Callers control any leading
/// indentation or newlines around the section.
pub fn render_completion_output_delivery(
    buf: &mut String,
    subagent_id: &str,
    output: PartialOutput<'_>,
    task_output_name: Option<&str>,
    disk_pointer_footer: Option<&str>,
) {
    use std::fmt::Write as _;
    match task_output_name {
        Some(name) => {
            let _ = write!(buf, "Use {name}(\"{subagent_id}\") to see the full output.");
        }
        None => match disk_pointer_footer {
            Some(footer) => {
                let (output, _) = truncate_with_preview(
                    output,
                    MAX_INLINE_COMPLETION_BYTES,
                    PREVIEW_SIZE,
                    Some(footer),
                );
                let _ = write!(buf, "response:\n{output}");
            }
            None => {
                let _ = write!(buf, "response:\n{}", output.text());
            }
        },
    }
}
/// Resolve the active toolset's `BackgroundTaskAction` tool name (e.g.
/// `"get_command_or_subagent_output"`), or `None` when no such tool is registered.
///
/// Centralises the structural "is a polling tool available?" check so all
/// callers route the same answer into [`render_completion_output_delivery`].
pub async fn resolve_task_output_tool_name(bridge: &ToolBridge) -> Option<String> {
    bridge.tool_for_kind(ToolKind::BackgroundTaskAction).await
}
/// Resolve the active toolset's `Read` tool name, used for the bash
/// completion disk-pointer footer in [`render_completion_output_delivery`].
pub async fn resolve_read_tool_name(bridge: &ToolBridge) -> Option<String> {
    bridge.tool_for_kind(ToolKind::Read).await
}
/// Format a model-facing message from a [`SubagentCompletionSummary`] for
/// the next-tool-call reminder surface.
///
/// `task_output_name`: the resolved name of the BackgroundTaskAction tool
/// in the current agent's toolset. When `None`, the subagent's full
/// `output` is inlined verbatim -- this notification is the only place
/// the model will see it (no disk-backed output file exists for subagents).
///
/// KEEP IN SYNC: the exact wording of this message is a compatibility
/// surface — downstream mirrors reproduce it verbatim (grep for
/// `format_subagent_completion_reminder`). Update them when changing it.
pub fn format_subagent_completion(
    c: &SubagentCompletionSummary,
    task_output_name: Option<&str>,
) -> String {
    let status = if c.success {
        "successfully"
    } else {
        "with failure"
    };
    let mut out = format!(
        "Background subagent \"{}\" ({}: \"{}\") completed {}.\n\
         Duration: {:.1}s | Tool calls: {} | Turns: {}",
        c.subagent_id,
        c.subagent_type,
        c.description,
        status,
        c.duration_ms as f64 / 1000.0,
        c.tool_calls,
        c.turns,
    );
    out.push_str(match task_output_name {
        Some(_) => "\n",
        None => "\n\n",
    });
    render_completion_output_delivery(
        &mut out,
        &c.subagent_id,
        PartialOutput::whole(&c.output),
        task_output_name,
        None,
    );
    out
}
/// Format buffered between-turn subagent completions into a system-reminder
/// string. When `task_output_name` is `None` each subagent's full output is
/// inlined verbatim; see [`render_completion_output_delivery`].
pub fn format_between_turn_completions(
    completions: &[SubagentCompletionSummary],
    task_output_name: Option<&str>,
) -> String {
    use std::fmt::Write as _;
    let n = completions.len();
    let label = if n == 1 { "subagent" } else { "subagents" };
    let mut buf = format!("While you were idle, {n} background {label} completed:\n");
    for c in completions {
        let status = if c.success {
            "completed successfully"
        } else {
            "failed"
        };
        let secs = c.duration_ms as f64 / 1000.0;
        let _ = write!(
            buf,
            "- [{}] {:?} \u{2014} {status} ({secs:.1}s, {} tool calls)\n  subagent_id: {}",
            c.subagent_type, c.description, c.tool_calls, c.subagent_id,
        );
        match task_output_name {
            Some(_) => buf.push_str(". "),
            None => buf.push_str("\n  "),
        }
        render_completion_output_delivery(
            &mut buf,
            &c.subagent_id,
            PartialOutput::whole(&c.output),
            task_output_name,
            None,
        );
        buf.push('\n');
    }
    buf
}
/// Format buffered between-turn subagent completions, resolving the
/// `BackgroundTaskAction` tool name from the supplied bridge in one place.
///
/// Wraps [`resolve_task_output_tool_name`] +
/// [`format_between_turn_completions`] so callers don't repeat the lookup
/// at every emission site.
pub async fn format_between_turn_completion_reminder(
    completions: &[SubagentCompletionSummary],
    bridge: &ToolBridge,
) -> String {
    let task_output_name = resolve_task_output_tool_name(bridge).await;
    format_between_turn_completions(completions, task_output_name.as_deref())
}
/// Format between-turn bash task completions into a system-reminder string.
pub fn format_between_turn_bash_completions(
    tasks: &[TaskSnapshot],
    task_output_name: Option<&str>,
    read_tool_name: Option<&str>,
) -> String {
    let n = tasks.len();
    let label = if n == 1 {
        "background task"
    } else {
        "background tasks"
    };
    let mut buf = format!("While you were idle, {n} {label} completed:\n");
    for task in tasks {
        buf.push_str(&format_bash_completion(
            task,
            task_output_name,
            read_tool_name,
        ));
        buf.push('\n');
    }
    buf
}
/// Extract task / subagent IDs whose completion the model already
/// learned about from this tool result. Used by:
/// - `TaskCompletionReminder::collect_reminders` to suppress the
///   per-tool-call `<system-reminder>` for the same ID.
/// - `echo-agent-runtime`'s `SessionActor` to sweep matching synthetic
///   auto-wake prompts and notifications out of `pending_inputs` /
///   `pending_notifications` (closes the TOCTOU race that produces
///   trailing `<system-reminder>` items in `chat_history.jsonl`).
///
/// Centralising this here means the two consumer surfaces cannot drift:
/// both call the same function, and the exhaustive `match` below
/// forces every new `ToolOutput` variant to opt in or out at compile
/// time.
///
/// Returns borrowed `&str` slices (no allocation) — the strings live in
/// `output` for the duration of the call.
/// Extract the subagent UUID from the body of a Task-tool-formatted
/// text output. The shape is exactly:
///
/// ```text
/// This is the output of the subagent:
///
/// response:
/// <response>...</response>
///
/// Agent ID: <uuid> (can be used with the `resume` parameter to send a follow-up after it completes)
/// ```
///
/// We anchor on `"Agent ID: "` and consume up to (but excluding) the next
/// whitespace character. Returns `None` if the text doesn't match.
fn task_text_agent_id(text: &str) -> Option<&str> {
    if !text.starts_with("This is the output of the subagent:") {
        return None;
    }
    let after = text.split_once("\nAgent ID: ")?.1;
    let end = after
        .find(|c: char| c.is_whitespace())
        .unwrap_or(after.len());
    if end == 0 { None } else { Some(&after[..end]) }
}
pub fn consumed_completion_ids(output: &ToolOutput) -> Vec<&str> {
    let mut ids = Vec::new();
    if let ToolOutput::Text(t) = output
        && let Some(uuid) = task_text_agent_id(&t.text)
    {
        ids.push(uuid);
    }
    match output {
        ToolOutput::TaskOutput(TaskOutputOutput::Result(r)) if r.status == "completed" => {
            ids.push(r.task_id.as_str());
        }
        ToolOutput::TaskOutput(TaskOutputOutput::Result(_)) => {}
        ToolOutput::TaskOutput(TaskOutputOutput::MultiResult(mr)) => {
            for r in &mr.results {
                if r.status == "completed" {
                    ids.push(r.task_id.as_str());
                }
            }
        }
        ToolOutput::TaskOutput(TaskOutputOutput::TaskNotFound(_)) => {}
        ToolOutput::KillTask(KillTaskOutput::Result(r)) => {
            ids.push(r.task_id.as_str());
        }
        ToolOutput::KillTask(KillTaskOutput::TaskNotFound(_)) => {}
        ToolOutput::SubagentCompleted(SubagentCompletedOutput { subagent_id, .. }) => {
            ids.push(subagent_id.as_str());
        }
        ToolOutput::Text(text) => {
            if let Some(id) = text.consumed_completion_task_id.as_deref() {
                ids.push(id);
            }
        }
        ToolOutput::Bash(_)
        | ToolOutput::BackgroundTaskStarted(_)
        | ToolOutput::GrepSearch(_)
        | ToolOutput::ReadFile(_)
        | ToolOutput::ListDir(_)
        | ToolOutput::SearchReplace(_)
        | ToolOutput::Todo(_)
        | ToolOutput::WebSearch(_)
        | ToolOutput::WebFetch(_)
        | ToolOutput::MCP(_)
        | ToolOutput::Skill(_)
        | ToolOutput::ApplyPatch(_)
        | ToolOutput::CodexGrepFiles(_)
        | ToolOutput::SearchTool(_)
        | ToolOutput::EnterPlanMode(_)
        | ToolOutput::ExitPlanMode(_)
        | ToolOutput::AskUserQuestion(_)
        | ToolOutput::Monitor(_)
        | ToolOutput::SchedulerCreate(_)
        | ToolOutput::SchedulerDelete(_)
        | ToolOutput::SchedulerList(_)
        | ToolOutput::UpdateGoal(_)
        | ToolOutput::Workflow(_)
        | ToolOutput::ImageGen(_)
        | ToolOutput::ImageToVideo(_)
        | ToolOutput::ReferenceToVideo(_)
        | ToolOutput::ImageEdit(_)
        | ToolOutput::Dynamic(_) => {}
    }
    ids
}
/// Cross-cutting reminder that queries the terminal backend for completed
/// background tasks and the subagent coordinator for completed subagents,
/// surfacing newly-completed ones as `<system-reminder>` text inside the
/// next tool result.
///
/// Registered on `FinalizedToolset` as a cross-cutting reminder.
/// Returns plain strings; the tool pipeline wraps
/// each one in `<system-reminder>` tags automatically.
pub struct TaskCompletionReminder;
#[async_trait::async_trait]
impl Reminder for TaskCompletionReminder {
    async fn collect_reminders(
        &self,
        resources: SharedResources,
        tool_output: &ToolOutput,
    ) -> Vec<String> {
        let consumed_ids: Vec<String> = consumed_completion_ids(tool_output)
            .into_iter()
            .map(str::to_string)
            .collect();
        let reserved_ids = {
            let res = resources.lock().await;
            if res
                .get::<TaskWakeSuppressed>()
                .is_some_and(TaskWakeSuppressed::get)
            {
                tracing::debug!("task wake reminder suppressed");
                return Vec::new();
            }
            res.get::<TaskCompletionReservations>()
                .map(TaskCompletionReservations::snapshot)
                .unwrap_or_default()
        };
        let suppress_ids = consumed_ids
            .iter()
            .chain(&reserved_ids)
            .cloned()
            .collect::<Vec<_>>();
        let (terminal, event_sender, parent_session_id) = {
            let res = resources.lock().await;
            (
                res.get::<Terminal>().map(|t| t.0.clone()),
                res.get::<SubagentEventSender>().cloned(),
                res.get::<crate::types::resources::OwnerSessionId>()
                    .map(|owner| owner.0.clone()),
            )
        };
        let mut reminders = Vec::new();
        if let Some(terminal) = terminal {
            let all_tasks = terminal.list_tasks().await;
            let mut res = resources.lock().await;
            let my_owner = res
                .get::<crate::types::resources::OwnerSessionId>()
                .map(|o| o.0.clone());
            let tasks: Vec<TaskSnapshot> = all_tasks
                .into_iter()
                .filter(|t| task_owned_by_session(t, my_owner.as_deref()))
                .collect();
            let goal_loop_active = res
                .get::<crate::implementations::echo_agent_build::task::types::GoalLoopActive>()
                .is_some_and(|g| g.0);
            let surface_reminders = !goal_loop_active
                && res
                    .get::<crate::types::resources::Params<
                        crate::implementations::echo_agent_build::bash::BashParams,
                    >>()
                    .map(|p| p.0.surface_bg_completion_reminders)
                    .unwrap_or(true);
            let renderer = res.get::<crate::types::template_renderer::TemplateRenderer>();
            let task_output_name: Option<String> = renderer.and_then(|r| {
                r.tool_for_kind(crate::types::tool::ToolKind::BackgroundTaskAction)
                    .map(str::to_string)
            });
            let read_tool_name: Option<String> = renderer.and_then(|r| {
                r.tool_for_kind(crate::types::tool::ToolKind::Read)
                    .map(str::to_string)
            });
            let kill_task_name: Option<String> = renderer.and_then(|r| {
                r.tool_for_kind(crate::types::tool::ToolKind::KillTaskAction)
                    .map(str::to_string)
            });
            let state = res.get_or_default::<State<ReportedTaskCompletions>>();
            for id in &consumed_ids {
                state.reported.insert(id.clone());
            }
            if surface_reminders {
                reminders.extend(
                    tasks
                        .iter()
                        .filter(|task| {
                            task.completed
                                && !reserved_ids.contains(&task.task_id)
                                && state.reported.insert(task.task_id.clone())
                        })
                        .map(|task| {
                            format_bash_completion(
                                task,
                                task_output_name.as_deref(),
                                read_tool_name.as_deref(),
                            )
                        }),
                );
            } else {
                for task in &tasks {
                    if task.completed && !reserved_ids.contains(&task.task_id) {
                        state.reported.insert(task.task_id.clone());
                    }
                }
            }
            if let ToolOutput::BackgroundTaskStarted(bg) = tool_output {
                let running: Vec<&TaskSnapshot> = tasks
                    .iter()
                    .filter(|t| !t.completed && t.task_id != bg.task_id)
                    .collect();
                if !running.is_empty() {
                    reminders.push(format_running_tasks_warning(
                        &running,
                        kill_task_name.as_deref(),
                    ));
                }
            }
        }
        if let Some(sender) = event_sender {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if sender
                .0
                .send(SubagentEvent::Completions(SubagentCompletionsRequest {
                    parent_session_id,
                    suppress_ids,
                    respond_to: tx,
                }))
                .is_err()
            {
                tracing::debug!("SubagentEventSender: receiver dropped, skipping");
            } else if let Ok(completions) = rx.await {
                let mut res = resources.lock().await;
                let goal_loop_active = res
                    .get::<crate::implementations::echo_agent_build::task::types::GoalLoopActive>()
                    .is_some_and(|g| g.0);
                let task_output_name: Option<String> = res
                    .get::<crate::types::template_renderer::TemplateRenderer>()
                    .and_then(|r| {
                        r.tool_for_kind(crate::types::tool::ToolKind::BackgroundTaskAction)
                            .map(str::to_string)
                    });
                let state = res.get_or_default::<State<ReportedTaskCompletions>>();
                for c in &completions {
                    if state.reported.insert(c.subagent_id.clone()) && !goal_loop_active {
                        reminders.push(format_subagent_completion(c, task_output_name.as_deref()));
                    }
                }
            }
        }
        reminders
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::output::TextOutput;
    #[test]
    fn consumed_completion_ids_from_text_with_consumed_id() {
        let output = ToolOutput::Text(TextOutput {
            text: "Task completed in 100ms with exit code: 0.".into(),
            consumed_completion_task_id: Some("bg-uuid-42".into()),
        });
        let ids = consumed_completion_ids(&output);
        assert_eq!(ids, vec!["bg-uuid-42"]);
    }
    #[test]
    fn consumed_completion_ids_from_text_without_consumed_id() {
        let output = ToolOutput::Text(TextOutput {
            text: "Task completed in 100ms with exit code: 0.".into(),
            consumed_completion_task_id: None,
        });
        assert!(consumed_completion_ids(&output).is_empty());
    }
    #[test]
    fn format_bash_completion_basic() {
        let task = TaskSnapshot {
            task_id: "abc-123".into(),
            command: "cargo test".into(),
            display_command: None,
            cwd: String::new(),
            start_time: std::time::SystemTime::now(),
            end_time: Some(std::time::SystemTime::now()),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: Some(0),
            signal: None,
            completed: true,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        };
        let msg = format_bash_completion(&task, Some("get_command_or_subagent_output"), None);
        assert!(msg.contains("abc-123"));
        assert!(msg.contains("exit code: 0"));
        assert!(msg.contains("cargo test"));
        assert!(msg.contains("get_command_or_subagent_output(\"abc-123\")"));
        assert!(
            !msg.contains("killed by the user"),
            "natural completion must not carry the UI-kill notice: {msg}"
        );
    }
    #[test]
    fn format_bash_completion_ui_kill_says_do_not_restart() {
        let mut task = TaskSnapshot {
            task_id: "ui-kill".into(),
            command: "sleep 60".into(),
            display_command: None,
            cwd: String::new(),
            start_time: std::time::SystemTime::now(),
            end_time: Some(std::time::SystemTime::now()),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: None,
            signal: Some("SIGKILL".into()),
            completed: true,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: true,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: true,
            output_total_bytes: 0,
        };
        let msg = format_bash_completion(&task, Some("get_command_or_subagent_output"), None);
        assert!(
            msg.contains("killed by the user — do not restart it"),
            "UI-kill wake must include the do-not-restart line: {msg}"
        );
        task.kill_result_delivered = true;
        let model_msg = format_bash_completion(&task, Some("get_command_or_subagent_output"), None);
        assert!(
            !model_msg.contains("killed by the user"),
            "model-tool kill must not tell the model the user killed it: {model_msg}"
        );
    }
    #[test]
    fn format_monitor_completion_exit_zero() {
        let task = TaskSnapshot {
            task_id: "mon-1".into(),
            command: "tail -f /var/log/app".into(),
            display_command: Some("[monitor] app logs".into()),
            cwd: String::new(),
            start_time: std::time::SystemTime::now(),
            end_time: Some(std::time::SystemTime::now()),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: Some(0),
            signal: None,
            completed: true,
            kind: crate::computer::types::TaskKind::Monitor,
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        };
        let msg = format_monitor_completion(&task, Some("get_command_or_subagent_output"));
        assert!(
            msg.contains("[monitor ended: exited (code 0)]"),
            "expected ended wording: {msg}"
        );
        assert!(msg.contains("app logs"), "description: {msg}");
        assert!(msg.contains("tail -f /var/log/app"), "command: {msg}");
        assert!(
            msg.contains("get_command_or_subagent_output(\"mon-1\")"),
            "poll tool pointer: {msg}"
        );
    }
    #[test]
    fn format_monitor_completion_signal() {
        let task = TaskSnapshot {
            task_id: "mon-sig".into(),
            command: "sleep 999".into(),
            display_command: Some("[monitor] sleep".into()),
            cwd: String::new(),
            start_time: std::time::SystemTime::now(),
            end_time: Some(std::time::SystemTime::now()),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: None,
            signal: Some("SIGTERM".into()),
            completed: true,
            kind: crate::computer::types::TaskKind::Monitor,
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        };
        let msg = format_monitor_completion(&task, None);
        assert!(
            msg.contains("[monitor ended: killed by signal SIGTERM]"),
            "expected signal wording: {msg}"
        );
        assert!(msg.contains("get_task_output(\"mon-sig\")"), "{msg}");
    }
    #[test]
    fn format_monitor_completion_ui_kill_says_do_not_restart() {
        let mut task = TaskSnapshot {
            task_id: "mon-ui".into(),
            command: "tail -f app.log".into(),
            display_command: Some("[monitor] app".into()),
            cwd: String::new(),
            start_time: std::time::SystemTime::now(),
            end_time: Some(std::time::SystemTime::now()),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: None,
            signal: Some("SIGKILL".into()),
            completed: true,
            kind: crate::computer::types::TaskKind::Monitor,
            block_waited: false,
            explicitly_killed: true,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: true,
            output_total_bytes: 0,
        };
        let msg = format_monitor_completion(&task, None);
        assert!(
            msg.contains("\nThis task was killed by the user — do not restart it.\n"),
            "UI-killed monitor notice must be on its own line: {msg}"
        );
        task.kill_result_delivered = true;
        let model_msg = format_monitor_completion(&task, None);
        assert!(
            !model_msg.contains("killed by the user"),
            "model-tool monitor kill must not carry the UI-kill notice: {model_msg}"
        );
    }
    #[test]
    fn format_bash_completion_prefers_display_command() {
        let task = TaskSnapshot {
            task_id: "t1".into(),
            command: "unshare --mount -- cargo test".into(),
            display_command: Some("cargo test".into()),
            cwd: String::new(),
            start_time: std::time::SystemTime::now(),
            end_time: Some(std::time::SystemTime::now()),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: Some(0),
            signal: None,
            completed: true,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        };
        let msg = format_bash_completion(&task, Some("get_command_or_subagent_output"), None);
        assert!(msg.contains("cargo test"));
        assert!(!msg.contains("unshare"));
    }
    #[test]
    fn format_bash_completion_unknown_exit_code() {
        let task = TaskSnapshot {
            task_id: "t1".into(),
            command: "server".into(),
            display_command: None,
            cwd: String::new(),
            start_time: std::time::SystemTime::now(),
            end_time: Some(std::time::SystemTime::now()),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: None,
            signal: None,
            completed: true,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        };
        let msg = format_bash_completion(&task, Some("get_command_or_subagent_output"), None);
        assert!(msg.contains("exit code: unknown"));
    }
    /// A long-running task killed by signal renders `terminated by
    /// signal SIGTERM`, *not* `exit code: ...`, mirroring the foreground
    /// `[killed by signal {sig}]` convention.
    #[test]
    fn format_bash_completion_signal_renders_signal_name() {
        let start = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
        let task = TaskSnapshot {
            task_id: "sig-1".into(),
            command: "./server".into(),
            display_command: None,
            cwd: String::new(),
            start_time: start,
            end_time: Some(std::time::SystemTime::now()),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: None,
            signal: Some("SIGTERM".into()),
            completed: true,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        };
        let msg = format_bash_completion(&task, Some("get_command_or_subagent_output"), None);
        assert!(
            msg.contains("terminated by signal SIGTERM"),
            "expected signal phrase, got: {msg}"
        );
        assert!(
            !msg.contains("exit code:"),
            "signal must take precedence: {msg}"
        );
        assert!(
            !msg.contains("wrapper bash may have been killed"),
            "long-running task should not get the wrapper-killed hint: {msg}"
        );
    }
    /// A signalled task with sub-second duration triggers the
    /// wrapper-killed hint -- this is the diagnostic for the
    /// self-matching pkill footgun.
    #[test]
    fn format_bash_completion_signal_short_duration_adds_hint() {
        let now = std::time::SystemTime::now();
        let task = TaskSnapshot {
            task_id: "sig-short".into(),
            command: "pkill -f ./server && ./server".into(),
            display_command: None,
            cwd: String::new(),
            start_time: now,
            end_time: Some(now),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: None,
            signal: Some("SIGTERM".into()),
            completed: true,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        };
        let msg = format_bash_completion(&task, Some("get_command_or_subagent_output"), None);
        assert!(
            msg.contains("terminated by signal SIGTERM"),
            "still includes signal phrase: {msg}"
        );
        assert!(
            msg.contains("wrapper bash may have been killed"),
            "expected wrapper-killed hint for short-duration signalled task: {msg}"
        );
        assert!(
            msg.contains("`pkill -f <pat>`"),
            "hint should mention the pkill footgun: {msg}"
        );
    }
    /// Short-duration tasks that exited cleanly (no signal) are normal
    /// (`true`, `:`, etc.) — the hint must NOT fire.
    #[test]
    fn format_bash_completion_no_signal_short_duration_no_hint() {
        let now = std::time::SystemTime::now();
        let task = TaskSnapshot {
            task_id: "fast".into(),
            command: "true".into(),
            display_command: None,
            cwd: String::new(),
            start_time: now,
            end_time: Some(now),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: Some(0),
            signal: None,
            completed: true,
            kind: Default::default(),
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
            output_total_bytes: 0,
        };
        let msg = format_bash_completion(&task, Some("get_command_or_subagent_output"), None);
        assert!(msg.contains("exit code: 0"));
        assert!(
            !msg.contains("wrapper bash may have been killed"),
            "no-signal short-duration task must not get the hint: {msg}"
        );
    }
    #[test]
    fn reported_state_deduplicates() {
        let mut state = ReportedTaskCompletions::default();
        assert!(state.mark_reported("t1"));
        assert!(!state.mark_reported("t1"));
        assert!(state.mark_reported("t2"));
    }
    #[test]
    fn format_between_turn_bash_single() {
        let tasks = vec![make_completed("bg-1")];
        let msg = format_between_turn_bash_completions(
            &tasks,
            Some("get_command_or_subagent_output"),
            Some("read_file"),
        );
        assert!(msg.starts_with("While you were idle, 1 background task completed:"));
        assert!(msg.contains("bg-1"));
        assert!(msg.contains(r#"get_command_or_subagent_output("bg-1")"#));
        assert!(!msg.contains("response:"));
    }
    #[test]
    fn format_between_turn_bash_multiple() {
        let tasks = vec![make_completed("bg-1"), make_completed("bg-2")];
        let msg = format_between_turn_bash_completions(
            &tasks,
            Some("get_command_or_subagent_output"),
            Some("read_file"),
        );
        assert!(msg.starts_with("While you were idle, 2 background tasks completed:"));
        assert!(msg.contains("bg-1"));
        assert!(msg.contains("bg-2"));
    }
    #[test]
    fn format_bash_completion_reconciles_existing_work_with_latest_state() {
        let msg = format_bash_completion(&make_completed("bg-1"), Some("get_task_output"), None);
        for instruction in [
            "not a new request",
            "latest task state",
            "skip completed or superseded actions",
            "Continue only remaining work",
            "conclusion is unchanged",
        ] {
            assert!(
                msg.contains(instruction),
                "missing reconciliation instruction: {instruction}"
            );
        }
    }
    #[test]
    fn format_bash_completion_pointer_form_is_small() {
        let mut task = make_completed("big-bg");
        task.output = "x".repeat(5_000_000);
        let msg = format_bash_completion(
            &task,
            Some("get_command_or_subagent_output"),
            Some("read_file"),
        );
        assert!(msg.len() < 500, "pointer reminder was {} bytes", msg.len());
        assert!(msg.contains(r#"get_command_or_subagent_output("big-bg")"#));
        assert!(!msg.contains(&"x".repeat(100)));
    }
    /// Without a polling tool AND without a Read-tool fallback, there is no
    /// disk-pointer footer to anchor a truncated preview against, so the
    /// full output text is inlined verbatim. The bash truncation cap only
    /// fires when the caller supplies a disk-pointer footer.
    #[test]
    fn format_bash_completion_inline_form_without_footer_is_verbatim() {
        let mut task = make_completed("big-bg");
        let large_output = "x".repeat(5_000_000);
        task.output = large_output.clone();
        let msg = format_bash_completion(&task, None, None);
        assert!(msg.contains("response:\n"));
        assert!(
            msg.contains(&large_output),
            "expected full output verbatim, got len={}",
            msg.len()
        );
        assert!(!msg.contains("[Output truncated"));
    }
    #[test]
    fn format_bash_completion_inline_points_at_output_file_when_available() {
        let mut task = make_completed("big-bg");
        task.output = "x".repeat(5_000_000);
        task.output_file = std::path::PathBuf::from("/tmp/bg.log");
        let msg = format_bash_completion(&task, None, Some("read_file"));
        assert!(
            msg.contains("Use read_file on /tmp/bg.log for full content"),
            "expected disk-pointer footer in inline reminder: {msg}"
        );
        assert!(msg.len() < 4_500, "inline reminder was {} bytes", msg.len());
        assert!(msg.contains("response:\n"));
    }
    #[test]
    fn format_between_turn_bash_completions_uses_pointer() {
        let mut first = make_completed("bg-1");
        first.output = "a".repeat(5_000_000);
        let mut second = make_completed("bg-2");
        second.output = "b".repeat(5_000_000);
        let msg = format_between_turn_bash_completions(
            &[first, second],
            Some("get_command_or_subagent_output"),
            Some("read_file"),
        );
        assert!(
            msg.len() < 1_000,
            "batched reminder was {} bytes",
            msg.len()
        );
        assert!(msg.contains(r#"get_command_or_subagent_output("bg-1")"#));
        assert!(msg.contains(r#"get_command_or_subagent_output("bg-2")"#));
        assert!(!msg.contains("response:"));
    }
    use crate::computer::types::{
        BackgroundHandle, KillOutcome, TerminalBackend, TerminalRunRequest, TerminalRunResult,
    };
    use crate::types::resources::Resources;
    use echo_agent_tool_types::KillTaskResult;
    use echo_agent_tool_types::{MultiTaskOutputResult, TaskOutputResult};
    use std::sync::Arc;
    use std::time::Duration;
    struct MockTerminal {
        tasks: Vec<TaskSnapshot>,
    }
    #[async_trait::async_trait]
    impl TerminalBackend for MockTerminal {
        async fn run(
            &self,
            _: TerminalRunRequest,
        ) -> Result<TerminalRunResult, crate::computer::types::ComputerError> {
            unimplemented!()
        }
        async fn run_background(
            &self,
            _: TerminalRunRequest,
        ) -> Result<BackgroundHandle, crate::computer::types::ComputerError> {
            unimplemented!()
        }
        async fn kill_task(&self, _: &str) -> KillOutcome {
            KillOutcome::NotFound
        }
        async fn get_task(&self, _: &str) -> Option<TaskSnapshot> {
            None
        }
        async fn wait_for_completion(&self, _: &str, _: Option<Duration>) -> Option<TaskSnapshot> {
            None
        }
        async fn list_tasks(&self) -> Vec<TaskSnapshot> {
            self.tasks.clone()
        }
    }
    fn make_completed(id: &str) -> TaskSnapshot {
        TaskSnapshot {
            task_id: id.into(),
            command: "echo test".into(),
            display_command: None,
            cwd: String::new(),
            start_time: std::time::SystemTime::now(),
            end_time: Some(std::time::SystemTime::now()),
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: Some(0),
            signal: None,
            completed: true,
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
    /// A log that cannot be read produces an empty snapshot with a
    /// non-zero total. The completion must still say how big the output
    /// is and where to read it.
    #[test]
    fn bash_completion_for_an_unreadable_log_still_points_at_the_file() {
        let mut task = make_completed("bg-unreadable");
        task.output = String::new();
        task.truncated = true;
        task.output_total_bytes = 123_456;
        task.output_file = std::path::PathBuf::from("/tmp/bg-unreadable.log");
        let msg = format_bash_completion(&task, None, Some("read_file"));
        assert!(msg.contains("123456 bytes total"), "{msg}");
        assert!(msg.contains("/tmp/bg-unreadable.log"), "{msg}");
    }
    /// The snapshot holds part of a large log. The footer the model reads must
    /// state the task's real size, not the size of the part on hand.
    #[test]
    fn bash_completion_footer_states_the_real_log_size() {
        let mut task = make_completed("bg-large");
        task.output = "x".repeat(20_000);
        task.output_total_bytes = 5_000_000;
        task.output_file = std::path::PathBuf::from("/tmp/bg-large.log");
        let msg = format_bash_completion(&task, None, Some("read_file"));
        assert!(msg.contains("5000000 bytes total"), "{msg}");
    }
    fn make_running(id: &str) -> TaskSnapshot {
        TaskSnapshot {
            task_id: id.into(),
            command: "python3 scripts/add_clips.py".into(),
            display_command: None,
            cwd: String::new(),
            start_time: std::time::SystemTime::now(),
            end_time: None,
            output: String::new(),
            output_file: std::path::PathBuf::new(),
            truncated: false,
            exit_code: None,
            signal: None,
            completed: false,
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
    fn make_bg_started(id: &str) -> crate::types::output::BackgroundTaskStarted {
        crate::types::output::BackgroundTaskStarted {
            task_id: id.into(),
            task_type: "bash".into(),
            output_file: String::new(),
            status: "running".into(),
            command: "echo hello".into(),
            summary: String::new(),
            retrieval_hint: String::new(),
            pre_formatted: None,
            pid: None,
        }
    }
    fn shared_with(tasks: Vec<TaskSnapshot>) -> SharedResources {
        let mut res = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminal { tasks });
        res.insert(Terminal(backend));
        res.register_state::<ReportedTaskCompletions>();
        res.into_shared()
    }
    fn shared_with_gate(tasks: Vec<TaskSnapshot>, gate: TaskWakeSuppressed) -> SharedResources {
        let mut res = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminal { tasks });
        res.insert(Terminal(backend));
        res.insert(gate);
        res.register_state::<ReportedTaskCompletions>();
        res.into_shared()
    }
    /// Like `shared_with` but inserts `BashParams` with
    /// `surface_bg_completion_reminders = false` so the
    /// reminder is suppressed.
    fn shared_with_reminders_disabled(tasks: Vec<TaskSnapshot>) -> SharedResources {
        let mut res = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminal { tasks });
        res.insert(Terminal(backend));
        res.register_state::<ReportedTaskCompletions>();
        let params = crate::implementations::echo_agent_build::bash::BashParams {
            surface_bg_completion_reminders: false,
            ..Default::default()
        };
        res.insert(crate::types::resources::Params(params));
        res.into_shared()
    }
    /// Bash completions are scoped to the session that OWNS the task. A
    /// subagent shares the parent's terminal backend, so `list_tasks()` returns
    /// the parent's (and sibling subagents') tasks too; only this session's
    /// (and unowned) tasks may surface. Regression guard for the parent →
    /// subagent background-task completion leak.
    #[tokio::test]
    async fn bash_completions_scoped_to_owning_session() {
        let mine = TaskSnapshot {
            owner_session_id: Some("subagent-1".into()),
            ..make_completed("mine-task")
        };
        let parents = TaskSnapshot {
            owner_session_id: Some("parent-0".into()),
            ..make_completed("parent-task")
        };
        let unowned = make_completed("unowned-task");
        let mut res = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminal {
            tasks: vec![mine, parents, unowned],
        });
        res.insert(Terminal(backend));
        res.register_state::<ReportedTaskCompletions>();
        res.insert(crate::types::resources::OwnerSessionId("subagent-1".into()));
        let shared = res.into_shared();
        let output = ToolOutput::Text(crate::types::output::TextOutput {
            text: "ok".into(),
            consumed_completion_task_id: None,
        });
        let reminders = TaskCompletionReminder
            .collect_reminders(shared, &output)
            .await;
        let joined = reminders.join("\n\n");
        assert!(
            joined.contains("mine-task"),
            "this session's own task must surface: {joined}"
        );
        assert!(
            joined.contains("unowned-task"),
            "unowned task must surface (backwards compat): {joined}"
        );
        assert!(
            !joined.contains("parent-task"),
            "another session's task must NOT leak into this session: {joined}"
        );
    }
    #[tokio::test]
    async fn suppressed_after_kill_task() {
        let shared = shared_with(vec![make_completed("t1")]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::KillTask(KillTaskOutput::Result(KillTaskResult {
            task_id: "t1".into(),
            outcome: "killed".into(),
            message: "Task was terminated successfully".into(),
        }));
        let r = reminder.collect_reminders(shared, &output).await;
        assert!(r.is_empty(), "kill_task result should suppress reminder");
    }
    #[tokio::test]
    async fn suppressed_after_await_text_with_consumed_id() {
        let shared = shared_with(vec![make_completed("t1")]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::Text(TextOutput {
            text: "Task completed in 100ms with exit code: 0.".into(),
            consumed_completion_task_id: Some("t1".into()),
        });
        let r = reminder.collect_reminders(shared, &output).await;
        assert!(
            r.is_empty(),
            "Await Text with consumed_completion_task_id should suppress reminder"
        );
    }
    #[tokio::test]
    async fn suppressed_after_get_task_output_completed() {
        let shared = shared_with(vec![make_completed("t1")]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::TaskOutput(TaskOutputOutput::Result(TaskOutputResult {
            task_id: "t1".into(),
            command: "echo test".into(),
            status: "completed".into(),
            exit_code: Some(0),
            started: "2026-01-01T00:00:00Z".into(),
            ended: Some("2026-01-01T00:00:01Z".into()),
            duration_secs: 1.0,
            output: "test".into(),
            output_file: "/tmp/out.log".into(),
            truncated: false,
            truncation_hint: String::new(),
            raw_output_bytes: 4,
        }));
        let r = reminder.collect_reminders(shared.clone(), &output).await;
        assert!(
            r.is_empty(),
            "get_task_output(completed) should suppress reminder"
        );
        assert!(
            shared
                .lock()
                .await
                .get::<State<ReportedTaskCompletions>>()
                .expect("reported state")
                .reported
                .contains("t1")
        );
    }
    #[tokio::test]
    async fn ctrl_c_gate_suppresses_visible_completion_without_reporting_it() {
        let gate = TaskWakeSuppressed::default();
        gate.set(true);
        let shared = shared_with_gate(vec![make_completed("visible")], gate.clone());
        let output = ToolOutput::Dynamic(serde_json::Value::Null.into());
        assert!(
            TaskCompletionReminder
                .collect_reminders(shared.clone(), &output)
                .await
                .is_empty()
        );
        assert!(
            shared
                .lock()
                .await
                .get::<State<ReportedTaskCompletions>>()
                .is_none_or(|state| !state.reported.contains("visible"))
        );
        gate.set(false);
        let reminders = TaskCompletionReminder
            .collect_reminders(shared, &output)
            .await;
        assert_eq!(reminders.len(), 1);
        assert!(reminders[0].contains("visible"));
    }
    #[tokio::test]
    async fn not_suppressed_for_unrelated_output() {
        let shared = shared_with(vec![make_completed("t1")]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::Dynamic(serde_json::Value::Null.into());
        let r = reminder.collect_reminders(shared, &output).await;
        assert_eq!(
            r.len(),
            1,
            "unrelated tool output should not suppress reminder"
        );
        assert!(r[0].contains("t1"));
    }
    #[tokio::test]
    async fn dedup_across_calls() {
        let shared = shared_with(vec![make_completed("t1")]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::Dynamic(serde_json::Value::Null.into());
        let first = reminder.collect_reminders(shared.clone(), &output).await;
        assert_eq!(first.len(), 1);
        let second = reminder.collect_reminders(shared, &output).await;
        assert!(second.is_empty(), "should not repeat");
    }
    /// In a toolset that opts out of bash-completion reminders via
    /// `BashParams.surface_bg_completion_reminders = false` (compat
    /// namespace), the reminders are silently dropped so the model
    /// does not see `Use get_task_output(...)` text referring to a
    /// non-existent tool. The completed-task IDs are still marked as
    /// reported, so a subsequent call won't surface them either.
    #[tokio::test]
    async fn bash_completion_suppressed_when_reminders_flag_disabled() {
        let shared = shared_with_reminders_disabled(vec![
            make_completed("bash-1"),
            make_completed("bash-2"),
        ]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::Dynamic(serde_json::Value::Null.into());
        let reminders = reminder.collect_reminders(shared, &output).await;
        assert!(
            reminders.is_empty(),
            "expected no bash completion reminders when flag is disabled, got: {reminders:?}"
        );
    }
    fn shared_with_subagent_completions(
        tasks: Vec<TaskSnapshot>,
        completions: Vec<SubagentCompletionSummary>,
    ) -> SharedResources {
        let mut res = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminal { tasks });
        res.insert(Terminal(backend));
        res.register_state::<ReportedTaskCompletions>();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        res.insert(SubagentEventSender(tx));
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let SubagentEvent::Completions(req) = event {
                    let filtered: Vec<_> = completions
                        .iter()
                        .filter(|c| !req.suppress_ids.contains(&c.subagent_id))
                        .cloned()
                        .collect();
                    let _ = req.respond_to.send(filtered);
                }
            }
        });
        res.into_shared()
    }
    fn make_subagent_completion(id: &str, success: bool) -> SubagentCompletionSummary {
        SubagentCompletionSummary {
            subagent_id: id.into(),
            subagent_type: "general-purpose".into(),
            description: "test task".into(),
            success,
            duration_ms: 5000,
            tool_calls: 3,
            turns: 2,
            output: std::sync::Arc::from(format!("output for {id}")),
        }
    }
    #[tokio::test]
    async fn subagent_completion_surfaced() {
        let shared =
            shared_with_subagent_completions(vec![], vec![make_subagent_completion("sub-1", true)]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::Dynamic(serde_json::Value::Null.into());
        let r = reminder.collect_reminders(shared, &output).await;
        assert_eq!(r.len(), 1);
        assert!(r[0].contains("sub-1"));
        assert!(r[0].contains("successfully"));
        assert!(r[0].contains("general-purpose"));
        assert!(r[0].contains("5.0s"));
    }
    #[tokio::test]
    async fn subagent_completion_suppressed_by_task_output() {
        let shared =
            shared_with_subagent_completions(vec![], vec![make_subagent_completion("sub-1", true)]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::TaskOutput(TaskOutputOutput::Result(TaskOutputResult {
            task_id: "sub-1".into(),
            command: String::new(),
            status: "completed".into(),
            exit_code: None,
            started: String::new(),
            ended: None,
            duration_secs: 0.0,
            output: "done".into(),
            output_file: String::new(),
            truncated: false,
            truncation_hint: String::new(),
            raw_output_bytes: 0,
        }));
        let r = reminder.collect_reminders(shared, &output).await;
        assert!(
            r.is_empty(),
            "completed get_task_output should suppress subagent reminder"
        );
    }
    #[tokio::test]
    async fn subagent_completion_suppressed_by_wait_tasks_multi_result() {
        let shared = shared_with_subagent_completions(
            vec![],
            vec![
                make_subagent_completion("sub-1", true),
                make_subagent_completion("sub-2", true),
            ],
        );
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::TaskOutput(TaskOutputOutput::MultiResult(MultiTaskOutputResult {
            mode: "wait_all".into(),
            results: vec![
                TaskOutputResult {
                    task_id: "sub-1".into(),
                    command: String::new(),
                    status: "completed".into(),
                    exit_code: None,
                    started: String::new(),
                    ended: None,
                    duration_secs: 0.0,
                    output: "done".into(),
                    output_file: String::new(),
                    truncated: false,
                    truncation_hint: String::new(),
                    raw_output_bytes: 0,
                },
                TaskOutputResult {
                    task_id: "sub-2".into(),
                    command: String::new(),
                    status: "completed".into(),
                    exit_code: None,
                    started: String::new(),
                    ended: None,
                    duration_secs: 0.0,
                    output: "done".into(),
                    output_file: String::new(),
                    truncated: false,
                    truncation_hint: String::new(),
                    raw_output_bytes: 0,
                },
            ],
            summary: "2/2 tasks completed (wait_all)".into(),
        }));
        let r = reminder.collect_reminders(shared, &output).await;
        assert!(
            r.is_empty(),
            "wait_tasks MultiResult should suppress reminders for completed subagents"
        );
    }
    #[tokio::test]
    async fn subagent_completion_dedup_across_calls() {
        let shared =
            shared_with_subagent_completions(vec![], vec![make_subagent_completion("sub-1", true)]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::Dynamic(serde_json::Value::Null.into());
        let first = reminder.collect_reminders(shared.clone(), &output).await;
        assert_eq!(first.len(), 1);
        let second = reminder.collect_reminders(shared, &output).await;
        assert!(
            second.is_empty(),
            "same subagent completion should not repeat"
        );
    }
    /// While a `/goal` loop is active the per-tool-call reminder must not
    /// surface bash or subagent completions (they would derail a weak model
    /// mid-goal), but the IDs must still be marked reported so they never
    /// resurface once the goal ends.
    #[tokio::test]
    async fn completions_suppressed_when_goal_loop_active() {
        let mut res = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminal {
            tasks: vec![make_completed("bash-1")],
        });
        res.insert(Terminal(backend));
        res.register_state::<ReportedTaskCompletions>();
        res.insert(crate::implementations::echo_agent_build::task::types::GoalLoopActive(true));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        res.insert(SubagentEventSender(tx));
        tokio::spawn(async move {
            while let Some(SubagentEvent::Completions(req)) = rx.recv().await {
                let _ = req
                    .respond_to
                    .send(vec![make_subagent_completion("sub-1", true)]);
            }
        });
        let shared = res.into_shared();
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::Dynamic(serde_json::Value::Null.into());
        let first = reminder.collect_reminders(shared.clone(), &output).await;
        assert!(
            first.is_empty(),
            "goal loop active should suppress bash + subagent reminders, got: {first:?}"
        );
        shared
            .lock()
            .await
            .insert(crate::implementations::echo_agent_build::task::types::GoalLoopActive(false));
        let second = reminder.collect_reminders(shared, &output).await;
        assert!(
            second.is_empty(),
            "suppressed completions must stay reported after goal ends, got: {second:?}"
        );
    }
    #[tokio::test]
    async fn subagent_and_bash_completions_together() {
        let shared = shared_with_subagent_completions(
            vec![make_completed("bash-1")],
            vec![make_subagent_completion("sub-1", false)],
        );
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::Dynamic(serde_json::Value::Null.into());
        let r = reminder.collect_reminders(shared, &output).await;
        assert_eq!(r.len(), 2);
        assert!(r[0].contains("bash-1"));
        assert!(r[1].contains("sub-1"));
        assert!(r[1].contains("with failure"));
    }
    #[tokio::test]
    async fn warns_about_running_tasks_on_bg_launch() {
        let shared = shared_with(vec![make_running("old-bg"), make_completed("done-1")]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::BackgroundTaskStarted(make_bg_started("new-bg"));
        let r = reminder.collect_reminders(shared, &output).await;
        assert_eq!(
            r.len(),
            2,
            "expected completion + running warning, got: {r:?}"
        );
        assert!(r[0].contains("done-1"), "first should be the completion");
        assert!(
            r[1].contains("old-bg"),
            "second should warn about old-bg still running"
        );
        assert!(r[1].contains("Consider killing"));
    }
    #[tokio::test]
    async fn no_warning_when_no_other_running_tasks() {
        let shared = shared_with(vec![make_completed("done-1")]);
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::BackgroundTaskStarted(make_bg_started("new-bg"));
        let r = reminder.collect_reminders(shared, &output).await;
        assert_eq!(r.len(), 1);
        assert!(r[0].contains("done-1"));
    }
    #[test]
    fn format_subagent_completion_success_with_poll_tool() {
        let c = make_subagent_completion("sub-abc", true);
        let msg = format_subagent_completion(&c, Some("get_task_output"));
        assert!(msg.contains("sub-abc"));
        assert!(msg.contains("successfully"));
        assert!(msg.contains("general-purpose"));
        assert!(msg.contains("test task"));
        assert!(msg.contains("5.0s"));
        assert!(msg.contains("Tool calls: 3"));
        assert!(msg.contains("Turns: 2"));
        assert!(msg.contains(r#"get_task_output("sub-abc")"#));
    }
    #[test]
    fn format_subagent_completion_failure() {
        let c = make_subagent_completion("sub-fail", false);
        let msg = format_subagent_completion(&c, Some("get_task_output"));
        assert!(msg.contains("with failure"));
    }
    #[test]
    fn format_subagent_completion_inlines_output_when_no_poll_tool() {
        let c = make_subagent_completion("sub-abc", true);
        let msg = format_subagent_completion(&c, None);
        assert!(msg.contains("sub-abc"));
        assert!(msg.contains("successfully"));
        assert!(
            !msg.contains("get_task_output"),
            "must not mention a polling tool when none is available"
        );
        assert!(
            !msg.contains("to see the full output"),
            "must omit the polling hint line entirely"
        );
        assert!(
            msg.contains("response:\noutput for sub-abc"),
            "must inline the subagent's output text: {msg}"
        );
    }
    #[test]
    fn task_completion_reservations_are_reference_counted() {
        let reservations = TaskCompletionReservations::default();
        reservations.reserve("t1".into());
        reservations.reserve("t1".into());
        reservations.release("t1");
        assert!(reservations.contains("t1"));
        reservations.release("t1");
        assert!(!reservations.contains("t1"));
    }
    #[test]
    fn task_completion_reservations_snapshot_is_non_destructive() {
        let reservations = TaskCompletionReservations::default();
        reservations.reserve("t1".into());
        reservations.reserve("t2".into());
        let snapshot = reservations.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.contains(&"t1".to_string()));
        assert!(snapshot.contains(&"t2".to_string()));
        assert!(reservations.contains("t1"));
        assert!(reservations.contains("t2"));
    }
    #[tokio::test]
    async fn task_completion_reservations_suppress_reminders() {
        let mut res = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminal {
            tasks: vec![make_completed("t1"), make_completed("t2")],
        });
        res.insert(Terminal(backend));
        res.register_state::<ReportedTaskCompletions>();
        let reservations = TaskCompletionReservations::default();
        reservations.reserve("t1".into());
        res.insert(reservations);
        let shared = res.into_shared();
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::Dynamic(serde_json::Value::Null.into());
        let r = reminder.collect_reminders(shared.clone(), &output).await;
        assert_eq!(r.len(), 1, "reserved ID should suppress reminder");
        assert!(r[0].contains("t2"));
        let res = shared.lock().await;
        assert!(
            res.get::<TaskCompletionReservations>()
                .is_some_and(|ids| ids.contains("t1"))
        );
        assert!(
            !res.get::<State<ReportedTaskCompletions>>()
                .expect("reported state")
                .reported
                .contains("t1")
        );
    }
    #[tokio::test]
    async fn reserved_completion_surfaces_after_release() {
        let mut res = Resources::new();
        let backend: Arc<dyn TerminalBackend> = Arc::new(MockTerminal {
            tasks: vec![make_completed("reserved")],
        });
        res.insert(Terminal(backend));
        res.register_state::<ReportedTaskCompletions>();
        let reservations = TaskCompletionReservations::default();
        reservations.reserve("reserved".into());
        res.insert(reservations.clone());
        let shared = res.into_shared();
        let reminder = TaskCompletionReminder;
        let output = ToolOutput::Dynamic(serde_json::Value::Null.into());
        assert!(
            reminder
                .collect_reminders(shared.clone(), &output)
                .await
                .is_empty()
        );
        assert!(reservations.contains("reserved"));
        assert!(
            !shared
                .lock()
                .await
                .get::<State<ReportedTaskCompletions>>()
                .expect("reported state")
                .reported
                .contains("reserved")
        );
        reservations.release("reserved");
        let reminders = reminder.collect_reminders(shared.clone(), &output).await;
        assert_eq!(reminders.len(), 1);
        assert!(reminders[0].contains("reserved"));
        assert!(
            shared
                .lock()
                .await
                .get::<State<ReportedTaskCompletions>>()
                .expect("reported state")
                .reported
                .contains("reserved")
        );
    }
    /// Regression: subagent inline output larger than the bash-completion
    /// inline cap MUST be preserved verbatim. The inline branch is the
    /// model's only chance to see subagent output (no disk file exists),
    /// so the bash-only [`MAX_INLINE_COMPLETION_BYTES`] cap must not leak
    /// into the subagent path.
    #[test]
    fn format_subagent_completion_no_poll_tool_preserves_large_output_verbatim() {
        let mut c = make_subagent_completion("sub-large", true);
        let large_output = "y".repeat(MAX_INLINE_COMPLETION_BYTES * 5);
        c.output = std::sync::Arc::from(large_output.as_str());
        let msg = format_subagent_completion(&c, None);
        assert!(
            msg.contains(&large_output),
            "subagent inline output must be preserved verbatim, got len={}",
            msg.len()
        );
        assert!(
            !msg.contains("[Output truncated"),
            "subagent inline output must not be truncated: {msg}"
        );
    }
    /// Same invariant for the between-turn batched reminder surface.
    #[test]
    fn format_between_turn_completions_no_poll_tool_preserves_large_output_verbatim() {
        let mut c = make_subagent_completion("sub-batch", true);
        let large_output = "z".repeat(MAX_INLINE_COMPLETION_BYTES * 3);
        c.output = std::sync::Arc::from(large_output.as_str());
        let msg = format_between_turn_completions(&[c], None);
        assert!(
            msg.contains(&large_output),
            "between-turn subagent inline output must be preserved verbatim"
        );
        assert!(!msg.contains("[Output truncated"));
    }
    /// The reminder pipeline ignores `MonitorEventBuffer` — the turn loop
    /// owns the drain. Guards against the tool-result append path being
    /// reintroduced.
    #[tokio::test]
    async fn reminder_pipeline_ignores_monitor_event_buffer() {
        use crate::implementations::echo_agent_build::monitor::types::{
            MonitorEventBuffer, MonitorEventNotification,
        };
        use crate::types::resources::Resources;
        let session_buffer = MonitorEventBuffer::default();
        session_buffer.push(MonitorEventNotification {
            task_id: "own-1".into(),
            event_text: "own line".into(),
            owner_session_id: None,
        });
        let mut res = Resources::new();
        res.insert(session_buffer.clone());
        let shared = res.into_shared();
        let output = ToolOutput::Text(crate::types::output::TextOutput {
            text: "ok".into(),
            consumed_completion_task_id: None,
        });
        let reminders = TaskCompletionReminder
            .collect_reminders(shared, &output)
            .await;
        assert!(
            reminders.is_empty(),
            "monitor events must not surface as tool-result reminders: {reminders:?}"
        );
        assert_eq!(
            session_buffer.len(),
            1,
            "the reminder pipeline must leave the buffer for the turn loop to drain"
        );
    }
    /// `drain_owned` leader-mode partition: the draining session takes its
    /// own + owner-less legacy events; foreign events stay buffered.
    #[test]
    fn drain_owned_partitions_by_session_owner() {
        use crate::implementations::echo_agent_build::monitor::types::{
            MonitorEventBuffer, MonitorEventNotification, drain_owned,
        };
        let shared_buffer = MonitorEventBuffer::default();
        shared_buffer.push(MonitorEventNotification {
            task_id: "mine-1".into(),
            event_text: "mine line".into(),
            owner_session_id: Some("session-B".into()),
        });
        shared_buffer.push(MonitorEventNotification {
            task_id: "foreign-1".into(),
            event_text: "foreign line".into(),
            owner_session_id: Some("session-A".into()),
        });
        shared_buffer.push(MonitorEventNotification {
            task_id: "legacy-1".into(),
            event_text: "legacy line".into(),
            owner_session_id: None,
        });
        let mine = drain_owned(&shared_buffer, Some("session-B"));
        let ids: Vec<&str> = mine.iter().map(|e| e.task_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["mine-1", "legacy-1"],
            "own + legacy events drain, in arrival order"
        );
        assert_eq!(shared_buffer.len(), 1, "foreign event must remain buffered");
        let foreign = drain_owned(&shared_buffer, Some("session-A"));
        assert_eq!(foreign.len(), 1);
        assert_eq!(foreign[0].task_id, "foreign-1");
        assert!(shared_buffer.is_empty());
    }
    /// Single event => lean `<monitor-event>` form; multiple => count-led
    /// batch with per-monitor `<monitor>` groups and numbered labels;
    /// empty => `None`.
    #[test]
    fn format_monitor_events_single_vs_batched() {
        use crate::implementations::echo_agent_build::monitor::types::MonitorEventNotification;
        let event = |task: &str, desc: &str, text: &str| MonitorEventNotification {
            task_id: task.to_string(),
            event_text: format!(
                "<monitor-event description=\"{desc}\" task_id=\"{task}\">\n{text}\n</monitor-event>"
            ),
            owner_session_id: None,
        };
        assert_eq!(format_monitor_events(&[], Some("get_task_output")), None);
        let single = format_monitor_events(
            &[event("task-0", "alpha", "line 0")],
            Some("get_task_output"),
        )
        .expect("single event formats");
        assert_eq!(
            single, "<monitor-event task_id=\"task-0\">\n[alpha] line 0\n</monitor-event>",
            "single event must use the lean monitor-event form"
        );
        let bare =
            crate::implementations::echo_agent_build::monitor::types::MonitorEventNotification {
                task_id: "task-9".into(),
                event_text: "bare text, no wrapper".into(),
                owner_session_id: None,
            };
        let single_bare =
            format_monitor_events(std::slice::from_ref(&bare), None).expect("bare event formats");
        assert_eq!(
            single_bare,
            "<monitor-event task_id=\"task-9\">\n[event] bare text, no wrapper\n</monitor-event>"
        );
        let batched = format_monitor_events(
            &[
                event("task-0", "alpha", "a first"),
                event("task-1", "beta", "b first"),
                event("task-0", "alpha", "a second"),
            ],
            None,
        )
        .expect("multiple events format");
        assert!(
            batched.starts_with(
                "3 monitor events from 2 monitors \
                 (use get_task_output to identify each monitor):"
            ),
            "batch must lead with event + monitor counts and default tool hint: {batched}"
        );
        assert!(
            batched.contains(
                "<monitor description=\"alpha\" task_id=\"task-0\">\n[1] a first\n[2] a second\n</monitor>"
            ),
            "task-0 group: description once on the tag, ordinal tick labels: {batched}"
        );
        assert!(
            batched.contains(
                "<monitor description=\"beta\" task_id=\"task-1\">\n[1] b first\n</monitor>"
            ),
            "task-1 group must carry its own description: {batched}"
        );
        assert!(
            !batched.contains("<monitor-event "),
            "per-event attribute wrappers must be hoisted into the group: {batched}"
        );
        assert_eq!(
            batched.matches("to identify each monitor").count(),
            1,
            "exactly one preamble: {batched}"
        );
    }
    /// `split_wrapped_monitor_event`: parses the exact `wrap_monitor_event`
    /// shape (including quotes inside the description), and returns `None`
    /// for non-conforming text so the batch falls back to verbatim.
    #[test]
    fn split_wrapped_monitor_event_parses_and_rejects() {
        let wrapped = "<monitor-event description=\"watch \\\"prod\\\" logs\" task_id=\"t-1\">\nline a\nline b\n</monitor-event>";
        let (desc, inner) = split_wrapped_monitor_event(wrapped).expect("conforming text parses");
        assert_eq!(desc, "watch \\\"prod\\\" logs");
        assert_eq!(inner, "line a\nline b");
        assert_eq!(split_wrapped_monitor_event("bare text, no wrapper"), None);
        assert_eq!(
            split_wrapped_monitor_event("<monitor-event task_id=\"t\">\nx\n</monitor-event>"),
            None,
            "missing description attribute must not parse"
        );
        let multibyte = "<monitor-event description=\"日本語ログ 监视 🚨\" task_id=\"t-utf8\">\n警告: ライン①\n二行目 — ürgent 🚨\n</monitor-event>";
        let (desc, inner) = split_wrapped_monitor_event(multibyte).expect("multibyte parses");
        assert_eq!(desc, "日本語ログ 监视 🚨");
        assert_eq!(inner, "警告: ライン①\n二行目 — ürgent 🚨");
    }
    /// Writer↔parser round-trip through the REAL `wrap_monitor_event`,
    /// including a hostile description (quotes + newline + `">\n` sequence).
    /// The writer sanitizes, so the parser always recovers cleanly — if the
    /// writer's shape ever drifts from the parser, this fails loudly.
    #[test]
    fn wrap_monitor_event_round_trips_through_split() {
        use crate::implementations::echo_agent_build::monitor::event::wrap_monitor_event;
        let wrapped = wrap_monitor_event("plain watcher", "tick 1\ntick 2", "t-1");
        let (desc, inner) = split_wrapped_monitor_event(&wrapped).expect("plain round-trip");
        assert_eq!(desc, "plain watcher");
        assert_eq!(inner, "tick 1\ntick 2");
        let wrapped = wrap_monitor_event("evil\">\nfake task_id=\"x", "payload", "t-2");
        let (desc, inner) = split_wrapped_monitor_event(&wrapped).expect("hostile round-trip");
        assert_eq!(desc, "evil'> fake task_id='x");
        assert_eq!(inner, "payload");
    }
    /// End-to-end multibyte safety through the formatter (single + batch).
    #[test]
    fn format_monitor_events_handles_multibyte_content() {
        use crate::implementations::echo_agent_build::monitor::types::MonitorEventNotification;
        let event = |task: &str, desc: &str, text: &str| MonitorEventNotification {
            task_id: task.to_string(),
            event_text: format!(
                "<monitor-event description=\"{desc}\" task_id=\"{task}\">\n{text}\n</monitor-event>"
            ),
            owner_session_id: None,
        };
        let single = format_monitor_events(&[event("t-1", "журнал 🚨", "строка №1 ✓")], None)
            .expect("single formats");
        assert!(single.contains("[журнал 🚨] строка №1 ✓"), "{single}");
        let batched = format_monitor_events(
            &[
                event("t-1", "журнал 🚨", "строка №1"),
                event("t-1", "журнал 🚨", "строка №2"),
            ],
            None,
        )
        .expect("batch formats");
        assert!(batched.contains("description=\"журнал 🚨\""), "{batched}");
        assert!(batched.contains("[1] строка №1"), "{batched}");
        assert!(batched.contains("[2] строка №2"), "{batched}");
    }
}
