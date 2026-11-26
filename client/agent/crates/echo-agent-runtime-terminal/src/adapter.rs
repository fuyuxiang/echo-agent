//! `AcpTerminalAdapter`: implements `echo-agent-tools::TerminalBackend` over ACP
//! gateway calls, for bash execution when the terminal is served by the client.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::exit_watcher::{poll_for_terminal_exit, release_terminal, watch_for_exit};
use super::output_recorder::{OutputRecorder, read_log_tail};
use agent_client_protocol as acp;
use echo_agent_acp::{AcpAgentGatewaySender as GatewaySender, acp_channel_failure};
use echo_agent_tools::computer::types::{
    BackgroundHandle, ComputerError, KillOutcome, KillSource, TaskKind, TaskSnapshot,
    TerminalBackend, TerminalRunRequest, TerminalRunResult,
};

/// A snapshot's per-completion fields, grouped to avoid transposed positional args.
#[derive(Clone)]
pub(super) struct SnapshotOutput {
    pub(super) output: String,
    pub(super) truncated: bool,
    pub(super) exit_code: Option<i32>,
    pub(super) signal: Option<String>,
}

pub(super) struct TrackedTask {
    command: String,
    display_command: Option<String>,
    cwd: String,
    output_file: PathBuf,
    start_time: std::time::SystemTime,
    /// Stamped once when the task completes, so repeated snapshots agree.
    end_time: Option<std::time::SystemTime>,
    completed: bool,
    exit_code: Option<i32>,
    signal: Option<String>,
    last_output: String,
    last_truncated: bool,
    block_waited: bool,
    explicitly_killed: bool,
    kill_result_delivered: bool,
    /// In-flight `wait_for_completion` callers. ACP has no oneshot list;
    /// this is the live-waiter count used for kill delivery.
    live_waiters: usize,
    kind: TaskKind,
    owner_session_id: Option<String>,
    description: Option<String>,
    output_byte_limit: usize,
}

/// Hand-written (`SystemTime` has no `Default`); call sites spread from it.
impl Default for TrackedTask {
    fn default() -> Self {
        Self {
            command: String::new(),
            display_command: None,
            cwd: String::new(),
            output_file: PathBuf::new(),
            start_time: std::time::SystemTime::now(),
            end_time: None,
            completed: false,
            exit_code: None,
            signal: None,
            last_output: String::new(),
            last_truncated: false,
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            live_waiters: 0,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
            output_byte_limit: crate::DEFAULT_OUTPUT_BYTE_LIMIT,
        }
    }
}

impl TrackedTask {
    pub(super) fn mark_completed(&mut self, out: SnapshotOutput) {
        self.completed = true;
        self.end_time = Some(std::time::SystemTime::now());
        self.exit_code = out.exit_code;
        self.signal = out.signal;
        self.last_output = out.output;
        self.last_truncated = out.truncated;
    }

    pub(super) fn to_snapshot(&self, task_id: &str, out: SnapshotOutput) -> TaskSnapshot {
        let completed = self.completed || out.exit_code.is_some() || out.signal.is_some();
        TaskSnapshot {
            task_id: task_id.to_string(),
            command: self.command.clone(),
            display_command: self.display_command.clone(),
            cwd: self.cwd.clone(),
            start_time: self.start_time,
            end_time: self
                .end_time
                .or_else(|| completed.then(std::time::SystemTime::now)),
            output: out.output,
            output_file: self.output_file.clone(),
            truncated: out.truncated,
            exit_code: out.exit_code,
            signal: out.signal,
            completed,
            block_waited: self.block_waited,
            explicitly_killed: self.explicitly_killed,
            kill_result_delivered: self.kill_result_delivered,
            kind: self.kind,
            owner_session_id: self.owner_session_id.clone(),
            description: self.description.clone(),
            // ACP tracked tasks are only registered via run_background.
            is_backgrounded: true,
            output_total_bytes: 0,
        }
    }
}

pub(super) type TaskMap = Arc<Mutex<HashMap<String, TrackedTask>>>;

#[must_use]
struct LiveWaiter<'a> {
    tasks: TaskMap,
    task_id: &'a str,
    /// Set on the normal return path so Drop does not treat this wait as
    /// cancelled. A cancelled wait must clear `block_waited` immediately:
    /// ClientUi kill only does that after the kill RPC, and the exit
    /// watcher can emit `TaskCompleted` in that gap.
    finished: bool,
}

impl Drop for LiveWaiter<'_> {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.tasks.lock()
            && let Some(task) = tasks.get_mut(self.task_id)
        {
            task.live_waiters = task.live_waiters.saturating_sub(1);
            if task.live_waiters == 0 && !self.finished {
                task.block_waited = false;
            }
        }
    }
}

fn wrap_command(command: &str) -> Result<String, ComputerError> {
    #[cfg(not(unix))]
    {
        let _ = command;
        Ok(command.to_string())
    }
    #[cfg(unix)]
    {
        let quoted = shlex::try_quote(command).map_err(|_| ComputerError::CommandNotQuoted)?;
        Ok(format!("{} -lc {quoted}", crate::default_shell_path()))
    }
}

fn to_env(env: HashMap<String, String>) -> Vec<acp::EnvVariable> {
    env.into_iter()
        .map(|(name, value)| acp::EnvVariable::new(name, value))
        .collect()
}

pub(super) fn parse_exit(
    status: &Option<acp::TerminalExitStatus>,
) -> (Option<i32>, Option<String>) {
    match status {
        Some(e) => (e.exit_code.map(|v| v as i32), e.signal.clone()),
        None => (None, None),
    }
}

/// Wraps echo-agent-runtime's ACP gateway to satisfy echo-agent-tools' TerminalBackend.
pub struct AcpTerminalAdapter {
    gateway: GatewaySender,
    session_id: acp::SessionId,
    tasks: TaskMap,
}

impl AcpTerminalAdapter {
    pub fn new(gateway: GatewaySender, session_id: acp::SessionId) -> Self {
        Self {
            gateway,
            session_id,
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn create_terminal(
        &self,
        command: String,
        request: &TerminalRunRequest,
    ) -> Result<acp::CreateTerminalResponse, ComputerError> {
        self.gateway
            .send(
                acp::CreateTerminalRequest::new(self.session_id.clone(), command)
                    .args(vec![])
                    .env(to_env(request.env.clone()))
                    .cwd(Some(request.working_directory.clone()))
                    .output_byte_limit(Some(request.output_byte_limit as u64)),
            )
            .await
            .map_err(|e| ComputerError::io(e.to_string()))
    }

    fn terminal_id(&self, task_id: &str) -> acp::TerminalId {
        acp::TerminalId::new(task_id)
    }
}

#[async_trait::async_trait]
impl TerminalBackend for AcpTerminalAdapter {
    async fn run(&self, request: TerminalRunRequest) -> Result<TerminalRunResult, ComputerError> {
        let command = wrap_command(&request.command)?;
        let create_res = self.create_terminal(command, &request).await?;

        let timed_out = match tokio::time::timeout(
            request.timeout,
            self.gateway.send(acp::WaitForTerminalExitRequest::new(
                self.session_id.clone(),
                create_res.terminal_id.clone(),
            )),
        )
        .await
        {
            Ok(Ok(_)) => false,
            Ok(Err(e)) => {
                release_terminal(&self.gateway, &self.session_id, &create_res.terminal_id).await;
                return Err(ComputerError::io(e.to_string()));
            }
            Err(_) => {
                let _ = self
                    .gateway
                    .send(acp::KillTerminalRequest::new(
                        self.session_id.clone(),
                        create_res.terminal_id.clone(),
                    ))
                    .await;
                true
            }
        };

        let output = match self
            .gateway
            .send(acp::TerminalOutputRequest::new(
                self.session_id.clone(),
                create_res.terminal_id.clone(),
            ))
            .await
        {
            Ok(output) => output,
            Err(e) => {
                release_terminal(&self.gateway, &self.session_id, &create_res.terminal_id).await;
                return Err(ComputerError::io(e.to_string()));
            }
        };

        release_terminal(&self.gateway, &self.session_id, &create_res.terminal_id).await;

        let (exit_code, signal) = parse_exit(&output.exit_status);
        let total_bytes = output.output.len();

        let mut recorder =
            OutputRecorder::new(request.output_file.clone(), request.output_byte_limit);
        recorder.initialize().await;
        if let Err(e) = recorder.append(&output.output).await {
            tracing::warn!(error = %e, "output recorder failed to write foreground output");
        }

        Ok(TerminalRunResult {
            combined_output: output.output,
            exit_code,
            truncated: output.truncated,
            signal,
            timed_out,
            output_file: request.output_file,
            total_bytes,
            pid: None,
        })
    }

    async fn run_background(
        &self,
        request: TerminalRunRequest,
    ) -> Result<BackgroundHandle, ComputerError> {
        let command = wrap_command(&request.command)?;
        let notification_handle = request.notification_handle.clone();
        let display_command = request.display_command.clone();
        let cwd = request.working_directory.to_string_lossy().to_string();
        let output_file = request.output_file.clone();

        let create_res = self.create_terminal(command.clone(), &request).await?;
        let task_id = create_res.terminal_id.0.to_string();
        let description = request.description;

        {
            let mut tasks = self.tasks.lock().unwrap();
            tasks.insert(
                task_id.clone(),
                TrackedTask {
                    command,
                    display_command,
                    cwd,
                    output_file: output_file.clone(),
                    kind: request.kind,
                    owner_session_id: request.owner_session_id.clone(),
                    description,
                    output_byte_limit: request.output_byte_limit,
                    ..Default::default()
                },
            );
        }

        let recorder = OutputRecorder::new(output_file.clone(), request.output_byte_limit);
        recorder.initialize().await;
        tokio::spawn(watch_for_exit(
            self.gateway.clone(),
            self.session_id.clone(),
            task_id.clone(),
            Arc::clone(&self.tasks),
            notification_handle,
            recorder,
        ));

        Ok(BackgroundHandle {
            task_id,
            output_file,
            pid: None,
        })
    }

    async fn get_task(&self, task_id: &str) -> Option<TaskSnapshot> {
        let live = self
            .gateway
            .send(acp::TerminalOutputRequest::new(
                self.session_id.clone(),
                self.terminal_id(task_id),
            ))
            .await
            .ok();

        // The std Mutex guard cannot be held across the await below, so resolve
        // under the lock and read the log file after releasing it.
        enum Resolved {
            Ready(TaskSnapshot),
            FromLog {
                snapshot: TaskSnapshot,
                output_file: PathBuf,
                limit: usize,
            },
            Missing,
        }
        let resolved = {
            let tasks = self.tasks.lock().unwrap();
            match (live, tasks.get(task_id)) {
                (Some(output), Some(tracked)) => {
                    let (exit_code, signal) = parse_exit(&output.exit_status);
                    Resolved::Ready(tracked.to_snapshot(
                        task_id,
                        SnapshotOutput {
                            output: output.output,
                            truncated: output.truncated,
                            exit_code,
                            signal,
                        },
                    ))
                }
                (Some(output), None) => {
                    let (exit_code, signal) = parse_exit(&output.exit_status);
                    Resolved::Ready(TrackedTask::default().to_snapshot(
                        task_id,
                        SnapshotOutput {
                            output: output.output,
                            truncated: output.truncated,
                            exit_code,
                            signal,
                        },
                    ))
                }
                // Live poll failed: a completed task keeps its authoritative
                // last_output; only a still-running task falls back to the log.
                (None, Some(tracked)) => {
                    let snapshot = tracked.to_snapshot(
                        task_id,
                        SnapshotOutput {
                            output: tracked.last_output.clone(),
                            truncated: tracked.last_truncated,
                            exit_code: tracked.exit_code,
                            signal: tracked.signal.clone(),
                        },
                    );
                    if snapshot.completed {
                        Resolved::Ready(snapshot)
                    } else {
                        Resolved::FromLog {
                            snapshot,
                            output_file: tracked.output_file.clone(),
                            limit: tracked.output_byte_limit,
                        }
                    }
                }
                (None, None) => Resolved::Missing,
            }
        };

        match resolved {
            Resolved::Ready(snapshot) => Some(snapshot),
            Resolved::Missing => None,
            Resolved::FromLog {
                mut snapshot,
                output_file,
                limit,
            } => {
                if let Some(tail) = read_log_tail(&output_file, limit).await {
                    snapshot.output = tail.text;
                    snapshot.truncated = tail.truncated;
                }
                Some(snapshot)
            }
        }
    }

    async fn kill_task(&self, task_id: &str) -> KillOutcome {
        self.kill_task_with_source(task_id, KillSource::ModelTool)
            .await
    }

    async fn kill_task_with_source(&self, task_id: &str, source: KillSource) -> KillOutcome {
        #[derive(Copy, Clone)]
        enum Tracked {
            Running,
            Completed,
            Unknown,
        }
        let tracked = {
            let mut tasks = self.tasks.lock().unwrap();
            match tasks.get_mut(task_id) {
                Some(task) if task.completed => Tracked::Completed,
                Some(task) => {
                    task.explicitly_killed = true;
                    // ModelTool/Teardown suppress auto-wake immediately
                    // (`marks_result_delivered(false)` is true). ClientUi
                    // waits until after the kill RPC so a cancelled waiter
                    // is visible. Setting only `explicitly_killed` here
                    // would let the exit watcher emit TaskCompleted with
                    // `kill_result_delivered` still false.
                    if source.marks_result_delivered(false) {
                        task.kill_result_delivered = true;
                    }
                    if task.live_waiters == 0 {
                        task.block_waited = false;
                    }
                    Tracked::Running
                }
                None => Tracked::Unknown,
            }
        };

        match tracked {
            Tracked::Completed => return KillOutcome::AlreadyExited,
            Tracked::Running => {}
            Tracked::Unknown => {
                let probe = self
                    .gateway
                    .send(acp::TerminalOutputRequest::new(
                        self.session_id.clone(),
                        self.terminal_id(task_id),
                    ))
                    .await;
                match probe {
                    Err(err) if acp_channel_failure(&err).is_none() => {
                        return KillOutcome::NotFound;
                    }
                    Err(_) => {}
                    Ok(output) if output.exit_status.is_some() => {
                        return KillOutcome::AlreadyExited;
                    }
                    Ok(_) => {}
                }
            }
        }

        let outcome = match self
            .gateway
            .send(acp::KillTerminalRequest::new(
                self.session_id.clone(),
                self.terminal_id(task_id),
            ))
            .await
        {
            Ok(_) => KillOutcome::Killed,
            Err(_) => KillOutcome::NotFound,
        };
        // ClientUi: sample waiters after the RPC so a wait cancelled at
        // its own `.await` (Drop of `LiveWaiter`) is not still counted as
        // delivered. ACP has no oneshot list; this is the closest we get
        // to local's `reply.send(...).is_ok()`. ModelTool/Teardown already
        // marked delivered under the first lock.
        if matches!(tracked, Tracked::Running)
            && !source.marks_result_delivered(false)
            && let Ok(mut tasks) = self.tasks.lock()
            && let Some(task) = tasks.get_mut(task_id)
        {
            let any_delivered = task.live_waiters > 0;
            task.kill_result_delivered = any_delivered;
            if !any_delivered {
                task.block_waited = false;
            }
        }
        outcome
    }

    async fn wait_for_completion(
        &self,
        task_id: &str,
        timeout: Option<Duration>,
    ) -> Option<TaskSnapshot> {
        let timeout = timeout.unwrap_or(Duration::from_secs(30));

        enum WaitStart {
            Immediate,
            Block,
            ProbeThenMaybeBlock,
        }
        let start = {
            let mut tasks = self.tasks.lock().unwrap();
            match tasks.get_mut(task_id) {
                Some(task) => {
                    task.block_waited = true;
                    if task.completed {
                        WaitStart::Immediate
                    } else {
                        task.live_waiters = task.live_waiters.saturating_add(1);
                        WaitStart::Block
                    }
                }
                None => WaitStart::ProbeThenMaybeBlock,
            }
        };
        match start {
            WaitStart::Immediate => return self.get_task(task_id).await,
            WaitStart::ProbeThenMaybeBlock => {
                // Not tracked as running: one output probe. A dead or unknown
                // terminal must not burn the wait budget on WaitForTerminalExit.
                // Match kill's untracked probe: `get_task` maps every
                // TerminalOutput error to None, so a transport/channel blip
                // on a still-live resumed terminal must not look like
                // not-found. Only a client that answered and disowned the
                // id is a definitive miss.
                let probe = self
                    .gateway
                    .send(acp::TerminalOutputRequest::new(
                        self.session_id.clone(),
                        self.terminal_id(task_id),
                    ))
                    .await;
                match probe {
                    Err(err) if acp_channel_failure(&err).is_none() => return None,
                    Err(_) => {}
                    Ok(output) if output.exit_status.is_some() => {
                        let (exit_code, signal) = parse_exit(&output.exit_status);
                        return Some(TrackedTask::default().to_snapshot(
                            task_id,
                            SnapshotOutput {
                                output: output.output,
                                truncated: output.truncated,
                                exit_code,
                                signal,
                            },
                        ));
                    }
                    Ok(_) => {}
                }
            }
            WaitStart::Block => {}
        }
        let mut live_waiter = LiveWaiter {
            tasks: self.tasks.clone(),
            task_id,
            finished: false,
        };

        let gateway_result = tokio::time::timeout(
            timeout,
            self.gateway.send(acp::WaitForTerminalExitRequest::new(
                self.session_id.clone(),
                self.terminal_id(task_id),
            )),
        )
        .await;

        match &gateway_result {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                let completed_meanwhile = {
                    let tasks = self.tasks.lock().unwrap();
                    tasks.get(task_id).is_some_and(|task| task.completed)
                };
                if !completed_meanwhile {
                    tracing::warn!(task_id, error = %e, "gateway error waiting for terminal exit, falling back to polling");
                    let deadline = tokio::time::Instant::now() + timeout;
                    poll_for_terminal_exit(
                        &self.gateway,
                        &self.session_id,
                        &self.terminal_id(task_id),
                        Some(deadline),
                    )
                    .await;
                }
            }
            Err(_) => {
                tracing::debug!(task_id, "timeout waiting for terminal exit");
                let mut tasks = self.tasks.lock().unwrap();
                if let Some(task) = tasks.get_mut(task_id) {
                    task.block_waited = false;
                }
            }
        }

        let snapshot = self.get_task(task_id).await;
        live_waiter.finished = true;
        snapshot
    }

    async fn list_tasks(&self) -> Vec<TaskSnapshot> {
        let task_ids: Vec<String> = {
            let tasks = self.tasks.lock().unwrap();
            tasks.keys().cloned().collect()
        };
        let mut snapshots = Vec::new();
        for task_id in task_ids {
            if let Some(snapshot) = self.get_task(&task_id).await {
                snapshots.push(snapshot);
            }
        }
        snapshots
    }

    async fn kill_all_background_tasks(&self) {
        let task_ids: Vec<String> = {
            let tasks = self.tasks.lock().unwrap();
            tasks
                .iter()
                .filter(|(_, t)| !t.completed)
                .map(|(id, _)| id.clone())
                .collect()
        };
        for task_id in task_ids {
            self.kill_task_with_source(&task_id, KillSource::Teardown)
                .await;
        }
    }

    async fn kill_foreground_commands(&self) {
        let session_id = self.session_id.0.to_string();
        crate::kill_and_release_all_for_session(&session_id).await;
    }
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
