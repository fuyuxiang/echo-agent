use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_client_protocol as acp;
use futures::future::{self, Either};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::runner::{AsyncTerminalRunner, TerminalError, TerminalRunRequest, TerminalRunResult};
use crate::{TerminalInfo, TerminalStatus};
use echo_agent_tools::types::output::{BashOutput, ToolOutput};

const DEFAULT_NOTIFICATION_INTERVAL_MS: u64 = 100;
const READ_BUFFER_SIZE: usize = 8192;

/// Upper bound on how long terminal teardown waits for a SIGKILL'd child to be
/// reaped.
///
/// `child.wait()` after `start_kill()` normally resolves in milliseconds, but a
/// process wedged in an uninterruptible kernel syscall (stuck network/disk I/O —
/// e.g. a hung `git clone` / `npm install`) only leaves the kernel, and thus
/// only becomes reapable, when that syscall returns, which can be many seconds
/// or effectively never. An unbounded wait is dangerous because terminal
/// teardown runs on the session actor's cancel path, and on the leader every
/// session shares one `LocalSet` thread — see `kill_and_release_all_for_session`
/// (which now reaps in a *detached* task to keep cancellation instant) and the
/// inline `kill_terminal` / `release_terminal` paths. Bounding the wait stops a
/// wedged child from making the reaper linger; the process is already SIGKILL'd
/// (and `KillOnDrop` is armed), so the OS / tokio reaper still tears it down
/// after we stop waiting. Mirrors the bound the `echo-agent-tools` local terminal
/// backend already applies to the same call.
const KILL_REAP_TIMEOUT: Duration = Duration::from_secs(2);

fn notification_interval() -> Duration {
    std::env::var("ECHO_AGENT_TERMINAL_NOTIFICATION_INTERVAL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(DEFAULT_NOTIFICATION_INTERVAL_MS))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExitStatus {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OutputSnapshot {
    pub output: String,
    pub truncated: bool,
    pub exit_status: Option<ExitStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillOutcome {
    Killed,
    AlreadyExited,
}

/// Sends ACP session notifications to the connected client.
///
/// **Important**: Implementations **must not** block for extended periods.
/// The terminal streaming loop calls [`SessionNotificationSender::session_notification`]
/// inside a `tokio::select!` branch.  If the call blocks, the loop stalls and
/// the command timeout cannot fire until the next iteration.  The default
/// Blackbox implementation (`AcpAgentGatewaySender`) uses fire-and-forget
/// delivery to satisfy this contract — see `gateway.rs`.
#[async_trait::async_trait]
pub trait SessionNotificationSender: Send + Sync {
    async fn session_notification(
        &self,
        notification: acp::SessionNotification,
    ) -> Result<(), acp::Error>;
}

#[async_trait::async_trait]
impl SessionNotificationSender for echo_agent_acp::AcpAgentGatewaySender {
    async fn session_notification(
        &self,
        notification: acp::SessionNotification,
    ) -> Result<(), acp::Error> {
        self.send(notification).await
    }
}

/// A wrapper around a [`SessionNotificationSender`] that respects the
/// `gateway_enabled` gate. When the gate is closed (e.g., for agent-initiated
/// fork sessions before `session/load`), notifications are silently dropped.
/// This prevents tool/bash output from leaking to the client before replay.
pub struct GatedNotifier {
    inner: Arc<dyn SessionNotificationSender>,
    gateway_enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl GatedNotifier {
    pub fn new(
        inner: Arc<dyn SessionNotificationSender>,
        gateway_enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            inner,
            gateway_enabled,
        }
    }
}

#[async_trait::async_trait]
impl SessionNotificationSender for GatedNotifier {
    async fn session_notification(
        &self,
        notification: acp::SessionNotification,
    ) -> Result<(), acp::Error> {
        if self
            .gateway_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.inner.session_notification(notification).await
        } else {
            Ok(())
        }
    }
}

type ChildHandle = Arc<Mutex<Box<dyn process_wrap::tokio::ChildWrapper>>>;

#[derive(Debug, Default)]
struct OutputState {
    output: Vec<u8>,
    truncated: bool,
    exit_status: Option<ExitStatus>,
    /// Whether the terminal has been backgrounded (agent should continue without waiting)
    backgrounded: bool,
}

struct TerminalEntry {
    child: ChildHandle,
    output_state: Arc<Mutex<OutputState>>,
    exit_notify: Arc<tokio::sync::Notify>,
    cwd: String,
    created_at: u64,
}

type TerminalKey = (String, String);
type TerminalMap = HashMap<TerminalKey, Arc<TerminalEntry>>;

static TERMINAL_REGISTRY: OnceLock<Mutex<TerminalMap>> = OnceLock::new();

fn registry() -> &'static Mutex<TerminalMap> {
    TERMINAL_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn get_entry(session_id: &str, terminal_id: &str) -> Option<Arc<TerminalEntry>> {
    let key = (session_id.to_string(), terminal_id.to_string());
    registry().lock().await.get(&key).cloned()
}

pub async fn find_terminal_session_id(terminal_id: &str) -> Option<String> {
    let reg = registry().lock().await;
    reg.keys()
        .find(|(_, id)| id == terminal_id)
        .map(|(session_id, _)| session_id.clone())
}

async fn register_entry(session_id: &str, terminal_id: &str, entry: Arc<TerminalEntry>) {
    let key = (session_id.to_string(), terminal_id.to_string());
    registry().lock().await.insert(key, entry);
}

async fn deregister_entry(session_id: &str, terminal_id: &str) -> Option<Arc<TerminalEntry>> {
    let key = (session_id.to_string(), terminal_id.to_string());
    registry().lock().await.remove(&key)
}

/// Atomically drain all non-backgrounded terminals for a session, killing their
/// child processes and removing them from the registry.
///
/// Backgrounded terminals (those the user explicitly asked to keep running) are
/// left untouched.
///
/// This avoids the TOCTOU race that would exist if we listed IDs first and then
/// killed them one-by-one (a new terminal could be registered between the two
/// steps).
pub async fn kill_and_release_all_for_session(session_id: &str) {
    // Phase 1: Remove all entries for this session under a short registry lock.
    // We don't check `backgrounded` here to avoid holding a nested async lock
    // (registry + output_state) which could deadlock with the streaming loop.
    let candidates: Vec<(TerminalKey, Arc<TerminalEntry>)> = {
        let mut reg = registry().lock().await;
        let keys: Vec<TerminalKey> = reg
            .keys()
            .filter(|(sid, _)| sid == session_id)
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|k| reg.remove(&k).map(|e| (k, e)))
            .collect()
    };
    // Registry lock released here.

    // Phase 2: Partition into backgrounded (kept alive) and foreground
    // (killed). Deliver SIGKILL to each foreground child *synchronously* so
    // teardown begins immediately, but do NOT wait for it to exit here.
    let mut reinsert = Vec::new();
    let mut to_reap = Vec::new();
    for (key, entry) in candidates {
        let is_bg = entry.output_state.lock().await.backgrounded;
        if is_bg {
            reinsert.push((key, entry));
        } else {
            {
                let mut child = entry.child.lock().await;
                if let Ok(None) = child.try_wait() {
                    let _ = child.start_kill();
                }
            }
            to_reap.push(entry);
        }
    }

    // Phase 3: Re-insert backgrounded entries that should stay alive.
    if !reinsert.is_empty() {
        let mut reg = registry().lock().await;
        for (key, entry) in reinsert {
            reg.insert(key, entry);
        }
    }

    // Phase 4: Reap the killed children OFF this task.
    //
    // This function is awaited inline in the session actor's cancel path
    // (`cancel_running_task`), and on the leader every session shares one
    // `LocalSet` thread. Awaiting `child.wait()` here would keep this session's
    // command loop parked until each process is reaped — delaying its
    // `session/prompt` resolution and, worse, its idle-unload (`Shutdown` /
    // `IsBusy` queue behind the in-flight cancel, so the leader cannot evict a
    // session stuck waiting on a slow-dying child). Reaping in a detached task
    // lets cancellation return immediately. The children are already SIGKILL'd
    // and `KillOnDrop` is armed, so they are torn down regardless of whether
    // this reaper is later cancelled; the bounded wait (`KILL_REAP_TIMEOUT`)
    // just keeps the reaper from lingering on a process wedged in the kernel.
    // The waits run concurrently so one wedged child can't delay the others.
    if !to_reap.is_empty() {
        tokio::task::spawn_local(async move {
            future::join_all(to_reap.into_iter().map(|entry| async move {
                let mut child = entry.child.lock().await;
                let _ = tokio::time::timeout(KILL_REAP_TIMEOUT, child.wait()).await;
            }))
            .await;
        });
    }
}

#[tracing::instrument(name = "terminal.create", skip_all, fields(session_id))]
pub async fn create_terminal(
    session_id: &str,
    command: &str,
    args: &[String],
    env: HashMap<String, String>,
    cwd: Option<&str>,
    output_byte_limit: Option<usize>,
) -> Result<String, String> {
    let terminal_id = uuid::Uuid::now_v7().to_string();
    let output_byte_limit = output_byte_limit.unwrap_or(super::DEFAULT_OUTPUT_BYTE_LIMIT);

    let working_dir = match cwd {
        Some(c) => c.to_string(),
        None => std::env::current_dir()
            .map_err(|e| format!("failed to get current directory: {e}"))?
            .to_string_lossy()
            .to_string(),
    };

    // Non-empty args → spawn program directly (argv preserved verbatim).
    // Empty args → treat command as a shell snippet (bash -c / cmd /C).
    let child = if args.is_empty() {
        spawn_shell_command(command, &working_dir, &env)
    } else {
        spawn_program_with_args(command, args, &working_dir, &env)
    }
    .map_err(|e| format!("failed to spawn '{command}': {e}"))?;

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let child_handle = Arc::new(Mutex::new(child));
    let output_state = Arc::new(Mutex::new(OutputState::default()));
    let exit_notify = Arc::new(tokio::sync::Notify::new());

    let entry = Arc::new(TerminalEntry {
        child: child_handle.clone(),
        output_state: output_state.clone(),
        exit_notify: exit_notify.clone(),
        cwd: working_dir.clone(),
        created_at,
    });
    register_entry(session_id, &terminal_id, entry).await;

    tokio::task::spawn_local(run_output_collector(
        child_handle,
        output_state,
        exit_notify,
        output_byte_limit,
    ));

    Ok(terminal_id)
}

pub async fn get_terminal_output(session_id: &str, terminal_id: &str) -> Option<OutputSnapshot> {
    let entry = get_entry(session_id, terminal_id).await?;
    let state = entry.output_state.lock().await;
    Some(OutputSnapshot {
        output: String::from_utf8_lossy(&state.output).into_owned(),
        truncated: state.truncated,
        exit_status: state.exit_status.clone(),
    })
}

pub async fn wait_for_terminal_exit(session_id: &str, terminal_id: &str) -> Option<ExitStatus> {
    let entry = get_entry(session_id, terminal_id).await?;

    // Notify::notified() captures notifications that occur either before or after the call,
    // so checking exit_status first, then awaiting is safe - we won't miss the notification.
    {
        let state = entry.output_state.lock().await;
        if let Some(status) = &state.exit_status {
            return Some(status.clone());
        }
        // If backgrounded, return None immediately so agent can continue
        if state.backgrounded {
            return None;
        }
    }

    entry.exit_notify.notified().await;

    let state = entry.output_state.lock().await;
    // If backgrounded, return None (process still running but agent should continue)
    if state.backgrounded {
        return None;
    }
    Some(
        state
            .exit_status
            .clone()
            .expect("exit_status must be set after exit notification"),
    )
}

pub async fn kill_terminal(
    session_id: &str,
    terminal_id: &str,
) -> Result<Option<KillOutcome>, String> {
    let Some(entry) = get_entry(session_id, terminal_id).await else {
        return Ok(None);
    };

    let mut child = entry.child.lock().await;
    if let Ok(Some(_)) = child.try_wait() {
        return Ok(Some(KillOutcome::AlreadyExited));
    }

    // `process_wrap::tokio::ChildWrapper::kill()` returns a boxed future that isn't guaranteed to be
    // `Unpin`, so we use `start_kill()` + `wait()` instead.
    match child.start_kill() {
        Ok(()) => {}
        Err(e) => {
            // Best-effort race handling: if it exited between our `try_wait()` and now, treat as
            // AlreadyExited instead of surfacing an error.
            if let Ok(Some(_)) = child.try_wait() {
                return Ok(Some(KillOutcome::AlreadyExited));
            }
            return Err(format!("failed to kill process: {e}"));
        }
    }

    // Bounded reap: the process has been SIGKILL'd; don't let a wedged child
    // (stuck in an uninterruptible syscall) block the caller indefinitely.
    let _ = tokio::time::timeout(KILL_REAP_TIMEOUT, child.wait()).await;
    Ok(Some(KillOutcome::Killed))
}

pub async fn release_terminal(session_id: &str, terminal_id: &str) {
    let Some(entry) = deregister_entry(session_id, terminal_id).await else {
        return;
    };

    let mut child = entry.child.lock().await;
    if let Ok(None) = child.try_wait() {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(KILL_REAP_TIMEOUT, child.wait()).await;
    }
}

/// Mark a terminal as backgrounded. This notifies any waiters so the agent can continue.
/// The process keeps running and will send completion notifications when done.
pub async fn background_terminal(session_id: &str, terminal_id: &str) {
    let Some(entry) = get_entry(session_id, terminal_id).await else {
        return;
    };

    {
        let mut state = entry.output_state.lock().await;
        state.backgrounded = true;
    }
    // Notify waiters so they can return early
    entry.exit_notify.notify_waiters();
}

pub(crate) async fn list_piped_terminals() -> Vec<TerminalInfo> {
    let entries: Vec<(TerminalKey, Arc<TerminalEntry>)> = {
        let reg = registry().lock().await;
        reg.iter()
            .map(|(key, entry)| (key.clone(), entry.clone()))
            .collect()
    };

    let mut result = Vec::with_capacity(entries.len());
    for ((_, terminal_id), entry) in entries {
        let (exit_status, output_len) = {
            let state = entry.output_state.lock().await;
            (state.exit_status.clone(), state.output.len() as u64)
        };

        let status = match exit_status {
            Some(_) => TerminalStatus::Exited,
            None => TerminalStatus::Connected,
        };
        let exit_code = exit_status.and_then(|s| s.exit_code);

        result.push(TerminalInfo {
            terminal_id,
            status,
            interactive: false,
            name: None,
            exit_code,
            cwd: Some(entry.cwd.clone()),
            output_offset: output_len,
            created_at: entry.created_at,
        });
    }

    result
}

pub struct StreamingLocalTerminalRunner {
    pub notifier: Arc<dyn SessionNotificationSender>,
    pub session_id: acp::SessionId,
}

#[async_trait::async_trait]
impl AsyncTerminalRunner for StreamingLocalTerminalRunner {
    async fn run(&self, request: TerminalRunRequest) -> Result<TerminalRunResult, TerminalError> {
        let child = spawn_shell_command(&request.command, &request.cwd, &request.env)
            .map_err(|e| TerminalError::Other(format!("failed to start shell: {e}")))?;

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let child_handle = Arc::new(Mutex::new(child));
        let output_state = Arc::new(Mutex::new(OutputState::default()));
        let exit_notify = Arc::new(tokio::sync::Notify::new());

        let entry = Arc::new(TerminalEntry {
            child: child_handle.clone(),
            output_state: output_state.clone(),
            exit_notify: exit_notify.clone(),
            cwd: request.cwd.to_string(),
            created_at,
        });

        let session_id_str = self.session_id.to_string();
        let tool_call_id_str = request.tool_call_id.to_string();

        register_entry(&session_id_str, &tool_call_id_str, entry).await;

        let result = self
            .run_streaming_loop(
                &request,
                child_handle.clone(),
                output_state.clone(),
                exit_notify.clone(),
                request.output_file.clone(),
            )
            .await;

        // Only deregister if process completed (not backgrounded)
        // If backgrounded, keep the entry so streaming continues
        let was_backgrounded = {
            let state = output_state.lock().await;
            state.backgrounded
        };

        if !was_backgrounded {
            deregister_entry(&session_id_str, &tool_call_id_str).await;
        } else {
            // Spawn a task to wait for completion and clean up
            let session_id = session_id_str.clone();
            let tool_call_id = tool_call_id_str.clone();
            let notifier = self.notifier.clone();
            let session = self.session_id.clone();
            let command = request.command.clone();
            let cwd = request.cwd.to_string();

            tokio::task::spawn_local(async move {
                wait_background_completion(
                    child_handle,
                    output_state,
                    exit_notify,
                    notifier,
                    session,
                    acp::ToolCallId::from(tool_call_id.clone()),
                    command,
                    cwd,
                )
                .await;
                deregister_entry(&session_id, &tool_call_id).await;
            });
        }

        result
    }
}

impl StreamingLocalTerminalRunner {
    async fn run_streaming_loop(
        &self,
        request: &TerminalRunRequest,
        child_handle: ChildHandle,
        output_state: Arc<Mutex<OutputState>>,
        exit_notify: Arc<tokio::sync::Notify>,
        output_file: Option<PathBuf>,
    ) -> Result<TerminalRunResult, TerminalError> {
        let (mut stdout, mut stderr) = take_child_io(&child_handle).await?;

        let mut output_buf = Vec::new();
        let mut stdout_tmp = [0u8; READ_BUFFER_SIZE];
        let mut stderr_tmp = [0u8; READ_BUFFER_SIZE];
        let mut last_sent_len = 0usize;
        let mut truncated = false;
        let mut ticker = tokio::time::interval(notification_interval());
        // Compute an absolute deadline so the timeout fires as a competing
        // branch inside `tokio::select!`.  This ensures the timeout is checked
        // even when another branch (e.g. `send_update`) is blocked.
        // Pinned once here to avoid re-creating the timer entry on every loop iteration.
        let sleep = tokio::time::sleep(request.timeout);
        tokio::pin!(sleep);

        // Open file handle once at start (more efficient than open/close per write)
        let mut file_handle: Option<tokio::fs::File> = match &output_file {
            Some(path) => {
                // Ensure parent directory exists
                if let Some(parent) = path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                match OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .await
                {
                    Ok(f) => Some(f),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to open output file");
                        None
                    }
                }
            }
            None => None,
        };

        self.send_update(
            &request.tool_call_id,
            &request.command,
            &[],
            0,
            false,
            false,
            None,
            acp::ToolCallStatus::InProgress,
            request.cwd.as_str(),
        )
        .await;

        loop {
            // Check if backgrounded - return early with partial output
            {
                let state = output_state.lock().await;
                if state.backgrounded {
                    // Flush file before returning
                    if let Some(ref mut file) = file_handle {
                        let _ = file.flush().await;
                    }
                    return Ok(TerminalRunResult {
                        combined_output: String::from_utf8_lossy(&output_buf).into_owned(),
                        exit_code: None,
                        truncated,
                        signal: Some("backgrounded".to_string()),
                        timed_out: false,
                    });
                }
            }

            if stdout.is_none()
                && stderr.is_none()
                && let Some(process_status) = try_get_exit_status(&child_handle).await
            {
                // Flush file before returning
                if let Some(ref mut file) = file_handle {
                    let _ = file.flush().await;
                }
                return self
                    .finish_exit(
                        request,
                        &output_buf,
                        truncated,
                        process_status,
                        &output_state,
                        &exit_notify,
                    )
                    .await;
            }

            let stdout_fut = match stdout.as_mut() {
                Some(s) => Either::Left(s.read(&mut stdout_tmp)),
                None => Either::Right(future::pending()),
            };
            let stderr_fut = match stderr.as_mut() {
                Some(s) => Either::Left(s.read(&mut stderr_tmp)),
                None => Either::Right(future::pending()),
            };

            tokio::pin!(stdout_fut);
            tokio::pin!(stderr_fut);

            // The timeout lives inside `tokio::select!` as a competing branch
            // so it fires even when another branch (e.g. `send_update` inside
            // the ticker arm) is blocked on a stale relay connection.
            tokio::select! {
                result = stdout_fut.as_mut() => {
                    match result {
                        Ok(0) | Err(_) => stdout = None,
                        Ok(n) => {
                            let bytes = &stdout_tmp[..n];
                            output_buf.extend_from_slice(bytes);

                            // Write IMMEDIATELY to file (before any truncation can happen)
                            // This ensures file always has complete output even if buffer is truncated
                            if let Some(ref mut file) = file_handle && let Err(e) = file.write_all(bytes).await {
                                    tracing::warn!("Failed to write stdout to output file: {}", e);
                            }
                        }
                    }
                }
                result = stderr_fut.as_mut() => {
                    match result {
                        Ok(0) | Err(_) => stderr = None,
                        Ok(n) => {
                            let bytes = &stderr_tmp[..n];
                            output_buf.extend_from_slice(bytes);

                            // Write IMMEDIATELY to file
                            if let Some(ref mut file) = file_handle && let Err(e) = file.write_all(bytes).await {
                                    tracing::warn!("Failed to write stderr to output file: {}", e);
                            }
                        }
                    }
                }
                _ = ticker.tick() => {
                    // Truncation only affects in-memory buffer, NOT the file
                    // File already has all bytes written immediately on read
                    if truncate_buffer(&mut output_buf, request.output_byte_limit) {
                        truncated = true;
                        last_sent_len = 0;
                    }

                    {
                        let mut state = output_state.lock().await;
                        state.output = output_buf.clone();
                        state.truncated = truncated;
                    }

                    if output_buf.len() > last_sent_len {
                        // NOTE: This `send_update` is safe because `session_notification`
                        // is fire-and-forget (see gateway.rs).  If it ever becomes blocking
                        // again, the deadline branch above won't save us once the ticker
                        // arm is selected — both changes are required for correctness.
                        self.send_update(
                            &request.tool_call_id,
                            &request.command,
                            &output_buf,
                            0,
                            truncated,
                            false,
                            None,
                            acp::ToolCallStatus::InProgress,
                            request.cwd.as_str(),
                        )
                        .await;
                        last_sent_len = output_buf.len();
                    }
                }
                _ = &mut sleep => {
                    // Flush file before returning
                    if let Some(ref mut file) = file_handle {
                        let _ = file.flush().await;
                    }
                    return self
                        .finish_timeout(
                            request,
                            &child_handle,
                            &output_buf,
                            truncated,
                            &output_state,
                            &exit_notify,
                        )
                        .await;
                }
            }
        }
    }

    async fn finish_timeout(
        &self,
        request: &TerminalRunRequest,
        child: &ChildHandle,
        output: &[u8],
        truncated: bool,
        output_state: &Arc<Mutex<OutputState>>,
        exit_notify: &Arc<tokio::sync::Notify>,
    ) -> Result<TerminalRunResult, TerminalError> {
        {
            // best-effort killing, not waiting for the process to exit
            let mut c = child.lock().await;
            let _ = c.start_kill();
        }

        let exit_status = ExitStatus {
            exit_code: None,
            signal: Some("timeout".to_string()),
        };
        finalize_output_state(output_state, exit_notify, output, truncated, &exit_status).await;

        self.send_update(
            &request.tool_call_id,
            &request.command,
            output,
            -1,
            truncated,
            true,
            None,
            acp::ToolCallStatus::Failed,
            request.cwd.as_str(),
        )
        .await;

        Ok(TerminalRunResult {
            combined_output: String::from_utf8_lossy(output).into_owned(),
            exit_code: None,
            truncated,
            signal: None,
            timed_out: true,
        })
    }

    async fn finish_exit(
        &self,
        request: &TerminalRunRequest,
        output: &[u8],
        truncated: bool,
        process_status: std::process::ExitStatus,
        output_state: &Arc<Mutex<OutputState>>,
        exit_notify: &Arc<tokio::sync::Notify>,
    ) -> Result<TerminalRunResult, TerminalError> {
        let exit_status = extract_exit_status(process_status);
        finalize_output_state(output_state, exit_notify, output, truncated, &exit_status).await;

        let tool_status = if exit_status.exit_code == Some(0) && exit_status.signal.is_none() {
            acp::ToolCallStatus::Completed
        } else {
            acp::ToolCallStatus::Failed
        };

        self.send_update(
            &request.tool_call_id,
            &request.command,
            output,
            exit_status.exit_code.unwrap_or(-1),
            truncated,
            false,
            exit_status.signal.clone(),
            tool_status,
            request.cwd.as_str(),
        )
        .await;

        Ok(TerminalRunResult {
            combined_output: String::from_utf8_lossy(output).into_owned(),
            exit_code: exit_status.exit_code,
            truncated,
            signal: exit_status.signal,
            timed_out: false,
        })
    }

    async fn send_update(
        &self,
        tool_call_id: &acp::ToolCallId,
        command: &str,
        output: &[u8],
        exit_code: i32,
        truncated: bool,
        timed_out: bool,
        signal: Option<String>,
        status: acp::ToolCallStatus,
        cwd: &str,
    ) {
        let bash_output = BashOutput {
            output_for_prompt: BashOutput::make_output_for_prompt(&String::from_utf8_lossy(output)),
            output: output.to_vec(),
            exit_code,
            command: command.to_string(),
            truncated,
            signal,
            timed_out,
            description: None,
            current_dir: cwd.to_owned(),
            output_file: String::new(),
            total_bytes: output.len(),
            output_delta: None,
            was_bare_echo: false,
        };

        let _ = self
            .notifier
            .session_notification(acp::SessionNotification::new(
                self.session_id.clone(),
                acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                    tool_call_id.clone(),
                    acp::ToolCallUpdateFields::new()
                        .status(Some(status))
                        .raw_output(serde_json::to_value(ToolOutput::Bash(bash_output)).ok()),
                )),
            ))
            .await;
    }
}

/// Spawn `command` via the detected shell (`bash -c` on Unix,
/// cascading pwsh / powershell.exe / Git Bash / cmd.exe on Windows).
fn spawn_shell_command(
    command: &str,
    cwd: &impl AsRef<std::path::Path>,
    env: &HashMap<String, String>,
) -> std::io::Result<Box<dyn process_wrap::tokio::ChildWrapper>> {
    #[cfg(unix)]
    {
        let program = crate::default_shell_path();
        spawn_with_argv(program, cwd, env, |cmd| {
            cmd.arg("-c").arg(command);
        })
    }
    #[cfg(not(unix))]
    {
        let inv = echo_agent_config::shell::shell_command_argv(command);
        spawn_with_argv(&inv.program, cwd, env, |cmd| {
            cmd.args(&inv.args).envs(inv.env);
        })
    }
}

/// Spawn `program` directly with pre-split `args` (no shell, argv preserved).
fn spawn_program_with_args(
    program: &str,
    args: &[String],
    cwd: &impl AsRef<std::path::Path>,
    env: &HashMap<String, String>,
) -> std::io::Result<Box<dyn process_wrap::tokio::ChildWrapper>> {
    spawn_with_argv(program, cwd, env, |cmd| {
        cmd.args(args);
    })
}

/// Shared spawn ceremony for [`spawn_shell_command`] and
/// [`spawn_program_with_args`]. `set_argv` populates argv only;
/// cwd/env/stdio/teardown are configured here.
fn spawn_with_argv(
    program: &str,
    cwd: &impl AsRef<std::path::Path>,
    env: &HashMap<String, String>,
    set_argv: impl FnOnce(&mut tokio::process::Command),
) -> std::io::Result<Box<dyn process_wrap::tokio::ChildWrapper>> {
    #[cfg(unix)]
    {
        use process_wrap::tokio::{CommandWrap, KillOnDrop, ProcessSession};

        let mut cmd = CommandWrap::with_new(program, |cmd| {
            set_argv(cmd);
            cmd.current_dir(cwd)
                .envs(env)
                .envs(crate::pager_env())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            #[cfg(target_os = "linux")]
            if echo_agent_sandbox::should_restrict_child_network() {
                // SAFETY: single prctl syscall (async-signal-safe).
                unsafe {
                    cmd.pre_exec(|| echo_agent_sandbox::child_net::install_child_network_filter());
                }
            }
        });
        // setsid: detach from TTY + new process group for tree teardown.
        cmd.wrap(ProcessSession);
        cmd.wrap(KillOnDrop);
        cmd.spawn()
    }
    #[cfg(not(unix))]
    {
        use process_wrap::tokio::{CommandWrap, CreationFlags, JobObject, KillOnDrop};
        use windows::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

        let mut cmd = CommandWrap::with_new(program, |cmd| {
            set_argv(cmd);
            cmd.current_dir(cwd)
                .envs(env)
                .envs(crate::pager_env())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        });
        // CreationFlags must precede JobObject per process-wrap docs.
        cmd.wrap(CreationFlags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW));
        // JobObject + KillOnDrop: dropping terminates every descendant.
        cmd.wrap(JobObject);
        cmd.wrap(KillOnDrop);
        cmd.spawn()
    }
}

fn extract_exit_status(status: std::process::ExitStatus) -> ExitStatus {
    let exit_code = status.code();

    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|s| format!("signal {}", s))
    };
    #[cfg(not(unix))]
    let signal: Option<String> = None;

    ExitStatus { exit_code, signal }
}

async fn take_child_io(
    child_handle: &ChildHandle,
) -> Result<
    (
        Option<tokio::process::ChildStdout>,
        Option<tokio::process::ChildStderr>,
    ),
    TerminalError,
> {
    let mut child = child_handle.lock().await;
    let stdout = child
        .stdout()
        .take()
        .ok_or_else(|| TerminalError::Other("failed to capture stdout".into()))?;
    let stderr = child
        .stderr()
        .take()
        .ok_or_else(|| TerminalError::Other("failed to capture stderr".into()))?;
    Ok((Some(stdout), Some(stderr)))
}

/// Truncate buffer to keep only the last `limit` bytes (drops oldest bytes).
/// Returns true if truncation occurred.
///
/// This function ensures we don't split UTF-8 characters when truncating
/// by using char_indices to find a valid character boundary.
fn truncate_buffer(buf: &mut Vec<u8>, limit: usize) -> bool {
    if buf.len() > limit {
        // Convert to string to work with character boundaries
        let s = String::from_utf8_lossy(buf);
        let excess = buf.len().saturating_sub(limit);

        // Find the first char boundary at or after `excess` bytes
        let start_idx = s
            .char_indices()
            .find(|(i, _)| *i >= excess)
            .map(|(i, _)| i)
            .unwrap_or(s.len());

        // Slice from that boundary and update buffer
        *buf = s[start_idx..].as_bytes().to_vec();

        true
    } else {
        false
    }
}

async fn try_get_exit_status(child_handle: &ChildHandle) -> Option<std::process::ExitStatus> {
    child_handle
        .try_lock()
        .ok()
        .and_then(|mut c| c.try_wait().ok())
        .flatten()
}

async fn finalize_output_state(
    output_state: &Arc<Mutex<OutputState>>,
    exit_notify: &Arc<tokio::sync::Notify>,
    output: &[u8],
    truncated: bool,
    exit_status: &ExitStatus,
) {
    {
        let mut state = output_state.lock().await;
        state.output = output.to_vec();
        state.truncated = truncated;
        state.exit_status = Some(exit_status.clone());
    }
    exit_notify.notify_waiters();
}

/// Waits for a backgrounded process to complete and sends the final notification.
/// Called when a process is backgrounded - the main run() returns but this keeps running
/// to wait for completion and clean up.
async fn wait_background_completion(
    child_handle: ChildHandle,
    output_state: Arc<Mutex<OutputState>>,
    exit_notify: Arc<tokio::sync::Notify>,
    notifier: Arc<dyn SessionNotificationSender>,
    session_id: acp::SessionId,
    tool_call_id: acp::ToolCallId,
    command: String,
    cwd: String,
) {
    use echo_agent_tools::types::output::{BashOutput, ToolOutput};

    // Wait for the process to exit
    loop {
        if let Some(process_status) = try_get_exit_status(&child_handle).await {
            let exit_status = extract_exit_status(process_status);

            // Get final output from state
            let (output_buf, truncated) = {
                let state = output_state.lock().await;
                (state.output.clone(), state.truncated)
            };

            finalize_output_state(
                &output_state,
                &exit_notify,
                &output_buf,
                truncated,
                &exit_status,
            )
            .await;

            // For backgrounded commands, always mark as Completed regardless of exit code.
            // The user explicitly chose to background this command and continue, so we
            // shouldn't show it as "failed" even if the process exits with non-zero.
            let final_status = acp::ToolCallStatus::Completed;

            let bash_output = BashOutput {
                output_for_prompt: BashOutput::make_output_for_prompt(&String::from_utf8_lossy(
                    &output_buf,
                )),
                output: output_buf,
                exit_code: exit_status.exit_code.unwrap_or(-1),
                command,
                truncated,
                signal: exit_status.signal,
                timed_out: false,
                description: None,
                current_dir: cwd,
                output_file: String::new(),
                total_bytes: 0,
                output_delta: None,
                was_bare_echo: false,
            };

            let _ = notifier
                .session_notification(acp::SessionNotification::new(
                    session_id,
                    acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                        tool_call_id,
                        acp::ToolCallUpdateFields::new()
                            .status(Some(final_status))
                            .raw_output(serde_json::to_value(ToolOutput::Bash(bash_output)).ok()),
                    )),
                ))
                .await;

            return;
        }

        // Sleep briefly before checking again
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_output_collector(
    child_handle: ChildHandle,
    output_state: Arc<Mutex<OutputState>>,
    exit_notify: Arc<tokio::sync::Notify>,
    output_byte_limit: usize,
) {
    let (mut stdout, mut stderr) = {
        let mut child = child_handle.lock().await;
        (child.stdout().take(), child.stderr().take())
    };

    let mut output_buf = Vec::new();
    let mut stdout_tmp = [0u8; READ_BUFFER_SIZE];
    let mut stderr_tmp = [0u8; READ_BUFFER_SIZE];
    let mut truncated = false;
    let mut ticker = tokio::time::interval(notification_interval());

    loop {
        if stdout.is_none()
            && stderr.is_none()
            && let Some(process_status) = try_get_exit_status(&child_handle).await
        {
            let exit_status = extract_exit_status(process_status);
            finalize_output_state(
                &output_state,
                &exit_notify,
                &output_buf,
                truncated,
                &exit_status,
            )
            .await;
            return;
        }

        let stdout_fut = match stdout.as_mut() {
            Some(s) => Either::Left(s.read(&mut stdout_tmp)),
            None => Either::Right(future::pending()),
        };
        let stderr_fut = match stderr.as_mut() {
            Some(s) => Either::Left(s.read(&mut stderr_tmp)),
            None => Either::Right(future::pending()),
        };

        tokio::pin!(stdout_fut);
        tokio::pin!(stderr_fut);

        tokio::select! {
            result = stdout_fut.as_mut() => {
                match result {
                    Ok(0) | Err(_) => stdout = None,
                    Ok(n) => output_buf.extend_from_slice(&stdout_tmp[..n]),
                }
            }
            result = stderr_fut.as_mut() => {
                match result {
                    Ok(0) | Err(_) => stderr = None,
                    Ok(n) => output_buf.extend_from_slice(&stderr_tmp[..n]),
                }
            }
            _ = ticker.tick() => {
                truncated |= truncate_buffer(&mut output_buf, output_byte_limit);

                {
                    let mut state = output_state.lock().await;
                    state.output = output_buf.clone();
                    state.truncated = truncated;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_OUTPUT_BYTE_LIMIT;
    use echo_agent_paths::AbsPathBuf;

    struct TestNotifier {
        notifications: Mutex<Vec<acp::SessionNotification>>,
    }

    #[async_trait::async_trait]
    impl SessionNotificationSender for TestNotifier {
        async fn session_notification(
            &self,
            notification: acp::SessionNotification,
        ) -> Result<(), acp::Error> {
            self.notifications.lock().await.push(notification);
            Ok(())
        }
    }

    fn make_request(tool_call_id: &str, command: &str) -> TerminalRunRequest {
        TerminalRunRequest {
            tool_call_id: acp::ToolCallId::new(tool_call_id),
            command: command.to_string(),
            cwd: AbsPathBuf::new(std::env::current_dir().unwrap()).unwrap(),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            output_byte_limit: DEFAULT_OUTPUT_BYTE_LIMIT,
            stream: true,
            output_file: None,
        }
    }

    fn extract_statuses(notifications: &[acp::SessionNotification]) -> Vec<acp::ToolCallStatus> {
        notifications
            .iter()
            .filter_map(|n| match &n.update {
                acp::SessionUpdate::ToolCallUpdate(tu) => tu.fields.status,
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn test_streaming_sends_status_updates() {
        let session_id = format!("s1-status-{}", std::process::id());
        let tool_id = format!("t1-status-{}", std::process::id());

        let notifier = Arc::new(TestNotifier {
            notifications: Mutex::new(vec![]),
        });
        let runner = StreamingLocalTerminalRunner {
            notifier: notifier.clone(),
            session_id: acp::SessionId::new(session_id),
        };

        let result = runner.run(make_request(&tool_id, "echo ok")).await.unwrap();

        assert_eq!(result.combined_output.trim(), "ok");
        let statuses = extract_statuses(&notifier.notifications.lock().await);
        assert!(statuses.contains(&acp::ToolCallStatus::InProgress));
        assert!(statuses.contains(&acp::ToolCallStatus::Completed));
    }

    #[tokio::test]
    async fn test_kill_returns_signal() {
        let session_id = format!("s1-kill-{}", std::process::id());
        let tool_id = format!("t1-kill-{}", std::process::id());

        tokio::task::LocalSet::new()
            .run_until(async {
                let notifier = Arc::new(TestNotifier {
                    notifications: Mutex::new(vec![]),
                });
                let session_id_clone = session_id.clone();
                let tool_id_clone = tool_id.clone();
                let runner = StreamingLocalTerminalRunner {
                    notifier: notifier.clone(),
                    session_id: acp::SessionId::new(session_id_clone),
                };

                let handle = tokio::task::spawn_local(async move {
                    runner.run(make_request(&tool_id_clone, "sleep 30")).await
                });

                // Wait for the process to start
                let mut attempts = 0;
                let max_attempts = 20;
                loop {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    let statuses = extract_statuses(&notifier.notifications.lock().await);
                    if statuses.contains(&acp::ToolCallStatus::InProgress) {
                        break;
                    }
                    attempts += 1;
                    if attempts >= max_attempts {
                        panic!("Process did not start within expected time");
                    }
                }

                assert_eq!(
                    kill_terminal(&session_id, &tool_id).await,
                    Ok(Some(KillOutcome::Killed))
                );

                let result = handle.await.unwrap().unwrap();
                assert_eq!(result.signal, Some("signal 9".to_string()));

                let statuses = extract_statuses(&notifier.notifications.lock().await);
                assert_eq!(statuses.last(), Some(&acp::ToolCallStatus::Failed));
            })
            .await;
    }

    #[tokio::test]
    async fn test_ext_create_output_wait_release() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let session_id = format!("ext-create-{}", std::process::id());

                let id = create_terminal(
                    &session_id,
                    "echo",
                    &["hello".to_string()],
                    HashMap::new(),
                    None,
                    None,
                )
                .await
                .unwrap();
                assert!(!id.is_empty());

                let status = wait_for_terminal_exit(&session_id, &id).await.unwrap();
                assert_eq!(status.exit_code, Some(0));

                let output = get_terminal_output(&session_id, &id).await.unwrap();
                assert!(output.output.contains("hello"));

                release_terminal(&session_id, &id).await;
                assert!(get_terminal_output(&session_id, &id).await.is_none());
            })
            .await;
    }

    #[tokio::test]
    async fn test_ext_release_cleans_up() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let session_id = format!("ext-release-{}", std::process::id());

                let id = create_terminal(
                    &session_id,
                    "sleep",
                    &["30".to_string()],
                    HashMap::new(),
                    None,
                    None,
                )
                .await
                .unwrap();

                tokio::time::sleep(Duration::from_millis(50)).await;

                release_terminal(&session_id, &id).await;

                assert!(get_terminal_output(&session_id, &id).await.is_none());
                assert_eq!(kill_terminal(&session_id, &id).await, Ok(None));
            })
            .await;
    }

    #[tokio::test]
    async fn test_kill_and_release_all_kills_non_bg_and_preserves_bg() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let session_id = format!("kill-all-{}", std::process::id());

                // Create two terminals: one normal, one we'll background.
                let normal_id = create_terminal(
                    &session_id,
                    "sleep",
                    &["30".to_string()],
                    HashMap::new(),
                    None,
                    None,
                )
                .await
                .unwrap();

                let bg_id = create_terminal(
                    &session_id,
                    "sleep",
                    &["30".to_string()],
                    HashMap::new(),
                    None,
                    None,
                )
                .await
                .unwrap();

                // Wait for both to start.
                tokio::time::sleep(Duration::from_millis(100)).await;

                // Mark one as backgrounded.
                background_terminal(&session_id, &bg_id).await;

                // Sanity: both are in the registry.
                assert!(get_terminal_output(&session_id, &normal_id).await.is_some());
                assert!(get_terminal_output(&session_id, &bg_id).await.is_some());

                // Kill all non-backgrounded terminals for the session.
                kill_and_release_all_for_session(&session_id).await;

                // Normal terminal should be gone.
                assert!(
                    get_terminal_output(&session_id, &normal_id).await.is_none(),
                    "non-backgrounded terminal should be removed from registry"
                );

                // Backgrounded terminal should still be present.
                assert!(
                    get_terminal_output(&session_id, &bg_id).await.is_some(),
                    "backgrounded terminal should remain in registry"
                );

                // Clean up: release the backgrounded terminal so the test doesn't leak.
                release_terminal(&session_id, &bg_id).await;
            })
            .await;
    }

    /// Verify that `ProcessSession` (setsid) prevents child processes from
    /// opening `/dev/tty`. After setsid(), the child has no controlling
    /// terminal, so `open("/dev/tty")` must fail with ENXIO.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_child_cannot_open_dev_tty() {
        // Skip in CI / environments without a controlling terminal.
        if std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .is_err()
        {
            eprintln!("skipping: no controlling terminal");
            return;
        }

        let session_id = format!("tty-detach-{}", std::process::id());
        let tool_id = format!("tty-detach-tool-{}", std::process::id());

        let notifier = Arc::new(TestNotifier {
            notifications: Mutex::new(vec![]),
        });
        let runner = StreamingLocalTerminalRunner {
            notifier: notifier.clone(),
            session_id: acp::SessionId::new(session_id),
        };

        let result = runner
            .run(make_request(
                &tool_id,
                "(exec 3>/dev/tty && echo ATTACHED || echo DETACHED) 2>/dev/null",
            ))
            .await
            .unwrap();

        assert_eq!(
            result.combined_output.trim(),
            "DETACHED",
            "child process should not be able to open /dev/tty after setsid()"
        );
    }

    /// Verify that process group kill still works after switching from
    /// ProcessGroup::leader() to ProcessSession. A parent shell spawns
    /// a background child; killing the terminal should reap both.
    #[tokio::test]
    async fn test_process_group_kill_with_session() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let session_id = format!("pgkill-{}", std::process::id());
                let tool_id = format!("pgkill-tool-{}", std::process::id());

                let notifier = Arc::new(TestNotifier {
                    notifications: Mutex::new(vec![]),
                });
                let session_id_clone = session_id.clone();
                let tool_id_clone = tool_id.clone();
                let runner = StreamingLocalTerminalRunner {
                    notifier: notifier.clone(),
                    session_id: acp::SessionId::new(session_id_clone),
                };

                let handle = tokio::task::spawn_local(async move {
                    runner
                        .run(make_request(&tool_id_clone, "sleep 300 & sleep 300 & wait"))
                        .await
                });

                // Wait for the process to start.
                let mut attempts = 0;
                loop {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    let statuses = extract_statuses(&notifier.notifications.lock().await);
                    if statuses.contains(&acp::ToolCallStatus::InProgress) {
                        break;
                    }
                    attempts += 1;
                    assert!(attempts < 40, "process did not start in time");
                }

                // Kill — should reap the whole process group.
                assert_eq!(
                    kill_terminal(&session_id, &tool_id).await,
                    Ok(Some(KillOutcome::Killed))
                );

                let result = handle.await.unwrap().unwrap();
                assert!(
                    result.signal.is_some(),
                    "process should have been killed by signal"
                );
            })
            .await;
    }

    #[tokio::test]
    async fn test_kill_and_release_all_noop_for_other_sessions() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let session_a = format!("kill-all-a-{}", std::process::id());
                let session_b = format!("kill-all-b-{}", std::process::id());

                // Create a terminal in session B.
                let id_b = create_terminal(
                    &session_b,
                    "sleep",
                    &["30".to_string()],
                    HashMap::new(),
                    None,
                    None,
                )
                .await
                .unwrap();

                tokio::time::sleep(Duration::from_millis(50)).await;

                // Kill all for session A (different session).
                kill_and_release_all_for_session(&session_a).await;

                // Session B terminal should be untouched.
                assert!(
                    get_terminal_output(&session_b, &id_b).await.is_some(),
                    "terminals in other sessions should not be affected"
                );

                // Clean up.
                release_terminal(&session_b, &id_b).await;
            })
            .await;
    }
}
