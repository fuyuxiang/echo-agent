use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::notification::MonitorEvent;
use crate::notification::types::ToolNotificationHandle;

use crate::types::requirements::Expr;
use crate::types::resources::Terminal;
use crate::types::tool::{ToolKind, ToolNamespace};

use super::event::{self, LineProcessor};
use super::rate_limiter::{MonitorRateLimiter, RateLimitOutcome};
use super::types::{MonitorInput, MonitorOutput, RATE_LIMIT_CAPACITY, RATE_LIMIT_REFILL_MS};

#[derive(Debug, Default)]
pub struct MonitorTool;

impl crate::types::tool_metadata::ToolMetadata for MonitorTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Monitor
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        r#"Start a background monitor that streams events from a long-running script. Each stdout line is an event${%- if system_reminders_enabled %} - you can keep working and notifications arrive in the chat${%- endif %}. Exit ends the watch.

**Output volume**: Every stdout line is a main-agent wake. Print only `DONE`/`FAILED`/`CANCELLED`. No progress or CHANGE lines. Use `grep --line-buffered` in pipes (plain `grep` buffers and delays events by minutes).

Set `persistent: true` for session-length watches (PR monitoring, log tails) -- the monitor runs${%- if tools.by_kind.kill_task_action %} until you call ${{ tools.by_kind.kill_task_action }} or${%- endif %} until the session ends. Otherwise it stops at `timeout_ms` (default 10h)."#
    }

    fn emitted_notifications(&self) -> &'static [&'static str] {
        &["BashExecutionBackgrounded", "MonitorEvent", "TaskCompleted"]
    }

    fn requires_expr(&self) -> Expr<crate::types::requirements::ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for MonitorTool {
    type Args = MonitorInput;
    type Output = MonitorOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("monitor").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "monitor",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.monitor", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: MonitorInput,
    ) -> Result<MonitorOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        input
            .validate()
            .map_err(|e| echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string()))?;

        let resolved_timeout = input.resolved_timeout_ms();
        let description = input.description;

        let (terminal, notification_handle, cwd, session_folder, owner_session_id) = {
            let res = resources.lock().await;
            let terminal = res.require::<Terminal>()?.0.clone();
            let notif = res
                .get::<crate::types::resources::NotificationHandle>()
                .map(|h| h.0.clone())
                .unwrap_or_default();
            let cwd = res
                .get::<crate::types::resources::Cwd>()
                .map(|c| c.0.clone())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let session_folder = res
                .require::<crate::types::resources::SessionFolder>()?
                .0
                .clone();
            let owner = res
                .get::<crate::types::resources::OwnerSessionId>()
                .map(|o| o.0.clone());
            (terminal, notif, cwd, session_folder, owner)
        };

        // Output file lives in the session folder alongside bash terminal logs.
        let output_file = session_folder
            .join("terminal")
            .join(format!("monitor-{}.log", ctx.call_id.as_str()));
        let bg_handle = terminal
            .run_background(crate::computer::types::TerminalRunRequest {
                command: input.command.clone(),
                working_directory: cwd.clone(),
                env: std::collections::HashMap::from([(
                    "PYTHONUNBUFFERED".to_string(),
                    "1".to_string(),
                )]),
                timeout: if resolved_timeout == 0 {
                    Duration::from_secs(86400 * 365) // long-running (until kill or session end)
                } else {
                    Duration::from_millis(resolved_timeout)
                },
                output_byte_limit: 10 * 1024 * 1024,
                output_file,
                notification_handle: notification_handle.clone(),
                tool_call_id: ctx.call_id.as_str().to_owned(),
                display_command: Some(format!("[monitor] {description}")),
                auto_background_on_timeout: false,
                foreground_block_budget: None,
                kind: crate::computer::types::TaskKind::Monitor,
                owner_session_id,
                description: Some(description.clone()).filter(|d| !d.trim().is_empty()),
            })
            .await
            .map_err(|e| {
                echo_agent_tool_runtime::ToolError::custom("process_manager", e.to_string())
            })?;

        let task_id = bg_handle.task_id.clone();
        let tray_description = Some(description.clone()).filter(|d| !d.trim().is_empty());

        // Notify the pager so the monitor appears in the tasks pane
        // (same notification that bash background tasks send).
        notification_handle.send_backgrounded(crate::notification::BashExecutionBackgrounded {
            base: crate::notification::BashNotificationBase {
                tool_call_id: ctx.call_id.as_str().to_owned(),
                // Send the real monitor command (so the block viewer shows the
                // actual script). The human-readable description travels in
                // `monitor_description` so the pager can render a "Monitor" tag
                // instead of bash-highlighting a "[monitor] …" pseudo-command.
                command: input.command.clone(),
                output: Vec::new(),
                total_bytes: 0,
                truncated: false,
                cwd,
            },
            output_file: bg_handle.output_file.clone(),
            task_id: task_id.clone(),
            monitor_description: tray_description.clone(),
            description: tray_description,
        });

        // Spawn the stdout processing pipeline.
        // Reads the output file, processes lines through the rate limiter,
        // and emits MonitorEvent notifications.
        let pipeline_task_id = task_id.clone();
        let pipeline_description = description;
        // Weak handle: the pipeline must not keep the session's terminal backend
        // (and the monitored process) alive past session end. See
        // `run_monitor_pipeline`.
        let pipeline_terminal = std::sync::Arc::downgrade(&terminal);
        let pipeline_notif = notification_handle.clone();
        let pipeline_output_file = bg_handle.output_file;

        // Resolve the kill tool name before spawning the pipeline (resources
        // can't cross the spawn boundary).
        let kill_tool_name = crate::types::template_renderer::TemplateRenderer::resolve_tool_name(
            &resources,
            crate::types::tool::ToolKind::KillTaskAction,
        )
        .await;

        let pipeline_kill_name = kill_tool_name.clone();
        tokio::spawn(async move {
            run_monitor_pipeline(
                &pipeline_task_id,
                &pipeline_description,
                pipeline_terminal,
                &pipeline_notif,
                &pipeline_output_file,
                pipeline_kill_name,
                0, // fresh pipeline — read from start
            )
            .await;
        });

        let kill_tool_display =
            kill_tool_name.unwrap_or_else(|| "kill_command_or_subagent".to_string());
        let result_message = if resolved_timeout == 0 {
            format!(
                "Monitor started (task {task_id}, persistent -- runs until {kill_tool_display} or session end).\n\
                 You will be notified on each event. Keep working -- do not poll or sleep.\n\
                 Events may arrive while you are waiting for the user -- an event is not their reply."
            )
        } else {
            format!(
                "Monitor started (task {task_id}, timeout {resolved_timeout}ms).\n\
                 You will be notified on each event. Keep working -- do not poll or sleep.\n\
                 Events may arrive while you are waiting for the user -- an event is not their reply."
            )
        };

        tracing::info!(
            task_id = %task_id,
            resolved_timeout,
            result_message = %result_message,
            "Monitor started"
        );

        Ok(MonitorOutput {
            task_id,
            timeout_ms: resolved_timeout,
            persistent: resolved_timeout == 0,
        })
    }
}

/// Background pipeline: polls the task output and feeds lines through
/// the processing pipeline (line processor -> rate limiter -> XML wrap -> notification).
///
/// `pub(crate)` so `reparent_notifications` can re-spawn the pipeline on the
/// parent's runtime when a subagent exits and its monitors are reparented.
///
/// Holds the backend as a [`Weak`](std::sync::Weak), never a strong `Arc`: a
/// persistent monitor loops for the session's whole lifetime, so a strong ref
/// would pin the terminal actor (and its process) and leak it across sessions
/// on shared-runtime hosts (hosts that build one backend per session on a
/// shared runtime). With a `Weak` the pipeline stops once the session drops
/// its backend, letting `shutdown_all()` reap the process.
pub(crate) async fn run_monitor_pipeline(
    task_id: &str,
    description: &str,
    terminal: std::sync::Weak<dyn crate::computer::types::TerminalBackend>,
    notification_handle: &ToolNotificationHandle,
    output_file: &std::path::Path,
    kill_tool_name: Option<String>,
    // Starting file offset. Pass 0 for fresh pipelines or the current file
    // size when re-spawning after reparent to avoid duplicate events.
    start_offset: u64,
) {
    let rate_limiter = Arc::new(Mutex::new(
        MonitorRateLimiter::new(RATE_LIMIT_CAPACITY, RATE_LIMIT_REFILL_MS).with_kill_tool_name(
            kill_tool_name.unwrap_or_else(|| "kill_command_or_subagent".to_string()),
        ),
    ));
    let mut line_processor = LineProcessor::new();
    let mut last_read_offset: u64 = start_offset;
    let mut last_owner: Option<String> = None;

    loop {
        // Strong handle for this tick only; `None` means the session dropped
        // the backend (torn down with the session), so stop streaming.
        let Some(terminal) = terminal.upgrade() else {
            break;
        };

        // Check if the task is still running.
        let snapshot = terminal.get_task(task_id).await;
        let completed = snapshot.is_none() || snapshot.as_ref().is_some_and(|s| s.completed);

        // Re-read each tick (a reparented monitor follows its new owner); keep
        // the last seen owner so an evicted-snapshot terminal event still routes.
        let snapshot_owner = snapshot.as_ref().and_then(|s| s.owner_session_id.clone());
        if snapshot_owner.is_some() {
            last_owner.clone_from(&snapshot_owner);
        }
        let owner_session_id = snapshot_owner.or_else(|| last_owner.clone());

        // Read new output from the file.
        let new_bytes = read_new_bytes(output_file, &mut last_read_offset).await;
        if !new_bytes.is_empty() {
            let lines = line_processor.push(&new_bytes);
            if !lines.is_empty() {
                let batched = event::batch_lines(&lines);
                process_event(
                    task_id,
                    description,
                    &batched,
                    &rate_limiter,
                    notification_handle,
                    owner_session_id.as_deref(),
                )
                .await;
            }
        }

        if completed {
            // Flush any remaining partial line.
            if let Some(remaining) = line_processor.flush() {
                process_event(
                    task_id,
                    description,
                    &remaining,
                    &rate_limiter,
                    notification_handle,
                    owner_session_id.as_deref(),
                )
                .await;
            }

            // Do NOT emit a terminal `[monitor ended: …]` MonitorEvent here.
            // Natural exit auto-wakes via `TaskCompleted` → immediate Prompt
            // (`format_monitor_completion` in the notification bridge). Emitting
            // a terminal event as well produced a second NotificationDrain turn
            // with the same ended signal. Stdout lines above still stream as
            // events while the process is alive; the UI learns completion from
            // `echo.agent/task_completed`.

            break;
        }

        if rate_limiter.lock().await.is_killed() {
            break;
        }

        // Drop the strong handle so only the `Weak` is held between ticks.
        drop(terminal);
        tokio::time::sleep(Duration::from_millis(super::types::DEBOUNCE_MS)).await;
    }
}

async fn read_new_bytes(path: &std::path::Path, offset: &mut u64) -> Vec<u8> {
    use tokio::io::AsyncReadExt;

    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return Vec::new();
    };

    let Ok(metadata) = file.metadata().await else {
        return Vec::new();
    };
    let file_len = metadata.len();
    if file_len <= *offset {
        return Vec::new();
    }

    use tokio::io::AsyncSeekExt;
    if file.seek(std::io::SeekFrom::Start(*offset)).await.is_err() {
        return Vec::new();
    }

    let to_read = (file_len - *offset) as usize;
    let mut buf = vec![0u8; to_read.min(1024 * 1024)]; // cap single read at 1MB
    let Ok(n) = file.read(&mut buf).await else {
        return Vec::new();
    };
    buf.truncate(n);
    *offset += n as u64;
    buf
}

async fn process_event(
    task_id: &str,
    description: &str,
    event_text: &str,
    rate_limiter: &Arc<Mutex<MonitorRateLimiter>>,
    notification_handle: &ToolNotificationHandle,
    owner_session_id: Option<&str>,
) {
    let owner = || owner_session_id.map(str::to_string);
    let mut rl = rate_limiter.lock().await;
    match rl.process_event(description) {
        RateLimitOutcome::Allowed { catch_up_notice } => {
            if let Some(notice) = catch_up_notice {
                let wrapped = event::wrap_monitor_event(description, &notice, task_id);
                notification_handle.send_monitor_event(MonitorEvent {
                    task_id: task_id.to_string(),
                    description: description.to_string(),
                    event_text: wrapped,
                    raw_text: notice,
                    owner_session_id: owner(),
                });
            }
            let wrapped = event::wrap_monitor_event(description, event_text, task_id);
            notification_handle.send_monitor_event(MonitorEvent {
                task_id: task_id.to_string(),
                description: description.to_string(),
                event_text: wrapped,
                raw_text: event_text.to_string(),
                owner_session_id: owner(),
            });
        }
        RateLimitOutcome::Suppressed => {
            // Silently dropped.
        }
        RateLimitOutcome::AutoKill { message } => {
            let wrapped = event::wrap_monitor_event(description, &message, task_id);
            notification_handle.send_monitor_event(MonitorEvent {
                task_id: task_id.to_string(),
                description: description.to_string(),
                event_text: wrapped,
                raw_text: message,
                owner_session_id: owner(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer::local::LocalTerminalBackend;
    use crate::computer::types::{TaskKind, TerminalBackend, TerminalRunRequest};
    use std::time::Duration;

    /// A persistent monitor must not keep the session's terminal backend (and
    /// thus the monitored process) alive after the session releases its handle.
    ///
    /// Regression for the cross-session monitor leak: the monitor pipeline held
    /// a strong `Arc<dyn TerminalBackend>`, so on a shared runtime (hosts that
    /// build one `LocalTerminalBackend` per session on one long-lived
    /// multi-threaded runtime) a persistent monitor's pipeline kept the actor's
    /// command channel open forever. The monitored process, the terminal actor,
    /// and the pipeline all leaked once the session ended. The interactive CLI
    /// only masked this because each session owns a dedicated thread+runtime
    /// that is torn down on session exit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn persistent_monitor_released_when_session_drops_backend() {
        let tmp = tempfile::tempdir().unwrap();
        let output_file = tmp.path().join("monitor.log");

        // Session-owned terminal backend (mirrors multi-session hosts that
        // build one LocalTerminalBackend per session on a shared runtime).
        let backend: Arc<dyn TerminalBackend> = Arc::new(LocalTerminalBackend::new());
        let weak = Arc::downgrade(&backend);

        // Start a persistent monitor: a process that effectively never exits.
        let handle = backend
            .run_background(TerminalRunRequest {
                command: "while true; do echo tick; sleep 0.1; done".to_string(),
                working_directory: tmp.path().to_path_buf(),
                env: std::collections::HashMap::new(),
                timeout: Duration::from_secs(86_400 * 365),
                output_byte_limit: 10 * 1024 * 1024,
                output_file: output_file.clone(),
                notification_handle: ToolNotificationHandle::noop(),
                tool_call_id: "tc-monitor".to_string(),
                display_command: Some("[monitor] tick".to_string()),
                auto_background_on_timeout: false,
                foreground_block_budget: None,
                kind: TaskKind::Monitor,
                owner_session_id: Some("session-A".to_string()),
                description: None,
            })
            .await
            .expect("spawn monitor");
        let task_id = handle.task_id.clone();

        // Spawn the streaming pipeline exactly like `MonitorTool::run`.
        let pipeline_terminal = Arc::downgrade(&backend);
        let pipeline_output = output_file.clone();
        let pipeline_task_id = task_id.clone();
        tokio::spawn(async move {
            run_monitor_pipeline(
                &pipeline_task_id,
                "tick",
                pipeline_terminal,
                &ToolNotificationHandle::noop(),
                &pipeline_output,
                None,
                0,
            )
            .await;
        });

        // The monitor is running and visible.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            backend.get_task(&task_id).await.is_some(),
            "monitor task should be running before the session ends"
        );

        // Session ends: drop every session-owned reference to the backend.
        // Only the pipeline's (weak) reference remains.
        drop(handle);
        drop(backend);

        // The backend — and the monitored process it owns — must be reclaimed
        // promptly. If the pipeline holds a strong `Arc`, `weak.upgrade()` never
        // returns `None` and the actor's `shutdown_all` never reaps the process.
        let mut reclaimed = false;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if weak.upgrade().is_none() {
                reclaimed = true;
                break;
            }
        }
        assert!(
            reclaimed,
            "monitor pipeline leaked the terminal backend past session end"
        );
    }

    /// On exit the pipeline must NOT emit a terminal `[monitor ended]` event
    /// (wake is owned by `TaskCompleted` auto-wake). Stdout lines still stream
    /// as MonitorEvents with the owner stamp so mid-run ticks reach the right
    /// session.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn monitor_exit_does_not_emit_terminal_ended_event() {
        let tmp = tempfile::tempdir().unwrap();
        let output_file = tmp.path().join("monitor.log");
        let backend: Arc<dyn TerminalBackend> = Arc::new(LocalTerminalBackend::new());

        let handle = backend
            .run_background(TerminalRunRequest {
                command: "echo done".to_string(),
                working_directory: tmp.path().to_path_buf(),
                env: std::collections::HashMap::new(),
                timeout: Duration::from_secs(60),
                output_byte_limit: 10 * 1024 * 1024,
                output_file: output_file.clone(),
                notification_handle: ToolNotificationHandle::noop(),
                tool_call_id: "tc-exit".to_string(),
                display_command: Some("[monitor] done".to_string()),
                auto_background_on_timeout: false,
                foreground_block_budget: None,
                kind: TaskKind::Monitor,
                owner_session_id: Some("session-A".to_string()),
                description: None,
            })
            .await
            .expect("spawn monitor");
        let task_id = handle.task_id.clone();

        tokio::time::sleep(Duration::from_millis(200)).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let capture = ToolNotificationHandle::from_sender(tx);
        run_monitor_pipeline(
            &task_id,
            "done",
            Arc::downgrade(&backend),
            &capture,
            &output_file,
            None,
            0,
        )
        .await;

        let mut saw_stdout = false;
        while let Ok(n) = rx.try_recv() {
            if let crate::notification::types::ToolNotification::MonitorEvent(e) = n {
                assert!(
                    !e.raw_text.contains("monitor ended"),
                    "terminal ended event must not be emitted (TaskCompleted owns wake): {}",
                    e.raw_text
                );
                if e.raw_text.contains("done") {
                    saw_stdout = true;
                    assert_eq!(
                        e.owner_session_id.as_deref(),
                        Some("session-A"),
                        "stdout events must still carry the owner"
                    );
                }
            }
        }
        assert!(
            saw_stdout,
            "stdout line from the monitor command must still stream as a MonitorEvent"
        );
    }

    /// When a subagent dies, its monitor is reparented to the parent: the owner
    /// flips to the parent and the pipeline is re-spawned on the parent's
    /// handle. The re-spawned pipeline must stamp the PARENT owner so the
    /// parent's bridge delivers the events instead of dropping them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reparented_monitor_emits_events_with_parent_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let output_file = tmp.path().join("monitor.log");
        let backend: Arc<dyn TerminalBackend> = Arc::new(LocalTerminalBackend::new());

        // Monitor owned by the (soon-dead) child session, emitting over time.
        let handle = backend
            .run_background(TerminalRunRequest {
                command: "for i in $(seq 1 50); do echo tick $i; sleep 0.1; done".to_string(),
                working_directory: tmp.path().to_path_buf(),
                env: std::collections::HashMap::new(),
                timeout: Duration::from_secs(60),
                output_byte_limit: 10 * 1024 * 1024,
                output_file: output_file.clone(),
                notification_handle: ToolNotificationHandle::noop(),
                tool_call_id: "tc-reparent".to_string(),
                display_command: Some("[monitor] tick".to_string()),
                auto_background_on_timeout: false,
                foreground_block_budget: None,
                kind: TaskKind::Monitor,
                owner_session_id: Some("child-session".to_string()),
                description: None,
            })
            .await
            .expect("spawn monitor");
        let task_id = handle.task_id.clone();

        // Subagent dies -> reparent its monitor to the parent. This flips the
        // owner and re-spawns the pipeline on the parent's capture handle.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let parent_handle = ToolNotificationHandle::from_sender(tx);
        backend
            .reparent_notifications(
                "child-session",
                "parent-session",
                parent_handle,
                Arc::downgrade(&backend),
            )
            .await;

        // The re-spawned pipeline streams post-reparent ticks, all of which must
        // carry the parent owner.
        let mut saw_parent_owned_event = false;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            while let Ok(n) = rx.try_recv() {
                if let crate::notification::types::ToolNotification::MonitorEvent(e) = n {
                    assert_eq!(
                        e.owner_session_id.as_deref(),
                        Some("parent-session"),
                        "reparented monitor events must carry the parent owner"
                    );
                    saw_parent_owned_event = true;
                }
            }
            if saw_parent_owned_event {
                break;
            }
        }
        assert!(
            saw_parent_owned_event,
            "re-spawned pipeline must emit monitor events to the parent after reparent"
        );

        backend.kill_task(&task_id).await;
    }
}
