//! Actor-based terminal implementation with support for foreground and background execution.
//!
//! This module implements a terminal backend using the actor pattern:
//! - `LocalTerminalBackend` is a handle that sends commands to the actor via channels
//! - `LocalTerminalActor` runs in a spawned task and owns all mutable state
//! - No mutex locks are needed - all state access is through message passing

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::computer::local::cgroup::{
    CgroupGuard, CgroupMemoryConfig, MemoryMonitor, PROCESS_OOM_EXIT_CODE,
};
use crate::computer::task_log;
use crate::computer::types::{
    BackgroundHandle, BackgroundedForeground, ComputerError, KillOutcome, KillSource, TaskSnapshot,
    TerminalBackend, TerminalRunRequest, TerminalRunResult,
};
use crate::notification::types::{BashNotificationBase, BashOutputChunk, ToolNotificationHandle};
use crate::util::truncate::FRONT_BACK_TRUNCATION_MARKER;

use super::SearchShadowConfig;
#[cfg(unix)]
use super::shell_state;

/// Result of spawning a shell command (persistent or plain).
struct SpawnResult {
    child: tokio::process::Child,
    process_group: crate::util::ProcessGroup,
    /// Handle for reading the state dump from fd 4 (persistent shell only).
    state_dump_handle: Option<tokio::task::JoinHandle<std::io::Result<String>>>,
}

const READ_BUFFER_SIZE: usize = 8192;
const DEFAULT_NOTIFICATION_INTERVAL_MS: u64 = 100;
const COMMAND_CHANNEL_SIZE: usize = 32;
/// How long to keep completed background tasks in memory before eviction.
/// The output file on disk persists for the session lifetime.
const COMPLETED_TASK_TTL: Duration = Duration::from_secs(300); // 5 minutes
/// SIGTERM → SIGKILL grace period. Uses a 1-second grace.
const SIGTERM_GRACE: Duration = Duration::from_secs(1);
/// Maximum lifetime for a background task. After this, the actor
/// will gracefully kill it. Set to 10 hours to support long
/// background monitor and bash runs.
const BACKGROUND_MAX_RUNTIME: Duration = Duration::from_secs(36_000);
/// Max time an *auto-backgroundable* foreground command blocks the turn before
/// it's moved to the background (kept running, never killed), independent of its
/// requested `timeout`. A short second timer for the auto-background budget.
/// Env override: `ECHO_AGENT_FOREGROUND_BLOCK_BUDGET_MS`.
const FOREGROUND_BLOCK_BUDGET: Duration = Duration::from_secs(15);

fn foreground_block_budget_from_env() -> Duration {
    std::env::var("ECHO_AGENT_FOREGROUND_BLOCK_BUDGET_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(FOREGROUND_BLOCK_BUDGET)
}

/// Max bytes a command's output file may reach before the actor kills it — the
/// size analogue of [`BACKGROUND_MAX_RUNTIME`], stopping an unbounded writer
/// (`yes`, a runaway log) from filling the disk. Env override:
/// `ECHO_AGENT_MAX_OUTPUT_FILE_BYTES`.
const MAX_OUTPUT_FILE_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GiB

fn output_file_cap_from_env() -> u64 {
    std::env::var("ECHO_AGENT_MAX_OUTPUT_FILE_BYTES")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(MAX_OUTPUT_FILE_BYTES)
}
/// Max time to drain stdout/stderr after process exit. Prevents `cmd &`
/// (inherited pipe, no redirect) from blocking the actor loop forever.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// How long completion waits on a kill before taking the output there is: a
/// process that never dies must not hold its task open forever.
const REAP_GRACE: Duration = Duration::from_secs(5);
/// Max bytes retained in the output file after process exit. Truncated
/// so `to_task_snapshot` / `read_file` don't materialize huge strings.
const MAX_RETAINED_OUTPUT_FILE_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB
/// Maximum number of completed-task tombstones to keep. When exceeded,
/// the oldest entries are evicted. Each tombstone is lightweight (metadata
/// only, no output), so 100 entries is ~10 KB.
const MAX_COMPLETED_TASK_SNAPSHOTS: usize = 100;

fn notification_interval() -> Duration {
    Duration::from_millis(DEFAULT_NOTIFICATION_INTERVAL_MS)
}

#[path = "lifecycle.rs"]
mod lifecycle;
use lifecycle::{Collection, Lifecycle};

/// Exit status of a terminal process
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExitStatus {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

/// Commands that can be sent to the LocalTerminalActor
enum TerminalCommand {
    /// Foreground: spawn process, block until exit or timeout, reply with result.
    Run {
        request: TerminalRunRequest,
        reply: oneshot::Sender<Result<TerminalRunResult, ComputerError>>,
    },

    /// Background: spawn process, register under a generated task_id,
    /// reply immediately with BackgroundHandle. Process keeps running.
    RunBackground {
        request: TerminalRunRequest,
        reply: oneshot::Sender<Result<BackgroundHandle, ComputerError>>,
    },

    /// Get snapshot of a background task (by task_id from RunBackground).
    GetTask {
        task_id: String,
        reply: oneshot::Sender<Option<TaskSnapshot>>,
    },

    /// Kill a background task.
    Kill {
        task_id: String,
        source: KillSource,
        reply: oneshot::Sender<KillOutcome>,
    },

    /// Kill foregrounded processes. Called on turn cancellation.
    KillForegroundCommands,

    /// Move a foreground command to background by tool_call_id.
    /// Unblocks the completion waiter with signal="backgrounded".
    BackgroundForeground {
        tool_call_id: String,
        reply: oneshot::Sender<bool>,
    },

    /// Move ALL running foreground commands to background, optionally scoped to an owner session.
    /// Used on a mid-turn redirect so in-flight commands are kept alive instead of SIGKILLed.
    BackgroundForegroundCommands {
        owner_session_id: Option<String>,
        reply: oneshot::Sender<Vec<BackgroundedForeground>>,
    },

    /// Wait for a background task to finish, with optional timeout.
    WaitForCompletion {
        task_id: String,
        timeout: Option<Duration>,
        reply: oneshot::Sender<Option<TaskSnapshot>>,
    },

    /// List all known background tasks.
    ListTasks {
        reply: oneshot::Sender<Vec<TaskSnapshot>>,
    },

    /// Query the persistent shell's current working directory.
    GetShellCwd {
        reply: oneshot::Sender<Option<PathBuf>>,
    },

    WarmShell {
        cwd: PathBuf,
    },

    /// Kill all running foreground processes owned by a specific session.
    KillForegroundCommandsByOwner {
        owner_session_id: String,
    },

    /// Kill all running background tasks owned by a specific session.
    KillTasksByOwner {
        owner_session_id: String,
        reply: oneshot::Sender<()>,
    },

    /// Reparent notification handles for all tasks owned by a session.
    /// Swaps the old notification handle with a new one so events from
    /// surviving processes route to the parent session. Also re-spawns
    /// monitor pipelines so monitor events continue streaming.
    ReparentNotifications {
        old_owner_session_id: String,
        new_owner_session_id: String,
        new_handle: crate::notification::types::ToolNotificationHandle,
        /// Weak (not strong) backend handle for re-spawning monitor pipelines,
        /// so a reparented monitor doesn't pin the backend. See `run_monitor_pipeline`.
        backend_weak: std::sync::Weak<dyn crate::computer::types::TerminalBackend>,
        reply: oneshot::Sender<()>,
    },
}

// ============================================================================
// Per-process state (for each running command)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundStatus {
    Foreground { auto_bg_on_timeout: bool },
    Backgrounded { reason: BackgroundReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundReason {
    /// Model requested `is_background=true`.
    Explicit,
    /// User pressed Ctrl+G.
    UserSignal,
    /// Foreground command exceeded default timeout.
    ForegroundTimeout,
}

impl BackgroundStatus {
    fn is_backgrounded(self) -> bool {
        matches!(self, Self::Backgrounded { .. })
    }
}

impl BackgroundReason {
    fn as_signal(&self) -> &'static str {
        match self {
            Self::Explicit | Self::UserSignal => "backgrounded",
            Self::ForegroundTimeout => "auto_backgrounded",
        }
    }
}

/// State for a single running process
struct ProcessState {
    /// The child process
    child: tokio::process::Child,
    /// Process-tree teardown handle, shared (`Arc`) with the process-global
    /// `ProcessScope` so the TUI exit paths can reap it if this actor never runs
    /// its own teardown. On Unix this stores the leader pid for `killpg`; on
    /// Windows it owns a Job Object that terminates every descendant when killed
    /// or dropped.
    ///
    /// On Unix this auto-drops to `None` the tick the child is reaped (`child.id()`
    /// is `None`) — the poll sweep (and the explicit-kill path) release the `Arc`
    /// so the scope's `Weak` dies at reap. A completed task lingers here for
    /// `completed_task_ttl`; holding the `Arc` that long would let `kill_all`
    /// `killpg` a pid the OS may have recycled. On Windows it stays `Some` until
    /// the `ProcessState` is removed (the JobObject HANDLE has no recyclable pid,
    /// so dropping early at reap is unnecessary).
    process_group: Option<std::sync::Arc<crate::util::ProcessGroup>>,
    /// Accumulated output buffer — tail portion (may be truncated)
    output_buffer: Vec<u8>,
    /// Front portion of output, captured before truncation kicks in.
    /// Once the total char count exceeds the limit, the first half of the
    /// budget is frozen here and only the tail is kept in `output_buffer`.
    front_buffer: Option<Vec<u8>>,
    /// Whether output was truncated
    truncated: bool,
    /// Total bytes written to file (before truncation)
    total_bytes: usize,
    lifecycle: Lifecycle,
    /// Whether process was backgrounded and how
    bg_status: BackgroundStatus,
    /// Waiters for this process to complete (foreground only)
    completion_waiters: Vec<oneshot::Sender<Result<TerminalRunResult, ComputerError>>>,
    /// Configuration
    output_byte_limit: usize,
    timeout: Duration,
    /// When auto_bg_on_timeout: max FG block before auto-bg (per-request or backend default).
    foreground_block_budget: Duration,
    start_time: Instant,
    /// Path to output file (always written to)
    output_file: PathBuf,
    /// Open file handle for incremental writes
    file_handle: Option<File>,
    /// The command that was executed (may be isolation-wrapped)
    command: String,
    /// Original user command before isolation wrapping (for display)
    display_command: Option<String>,
    /// Working directory where command was run
    cwd: String,
    /// Wall-clock start time (for TaskSnapshot)
    start_wall_time: std::time::SystemTime,
    /// Wall-clock end time (for TaskSnapshot duration calculation)
    end_wall_time: Option<std::time::SystemTime>,

    /// Notification handle for streaming output chunks.
    notification_handle: ToolNotificationHandle,
    /// Tool call ID for correlating notifications with the tool invocation.
    tool_call_id: String,
    /// Task kind: bash or monitor.
    kind: crate::computer::types::TaskKind,
    /// Monotonic `total_bytes` at the time of the last chunk notification.
    /// Used to detect "new output since last tick" — only send a chunk
    /// when total_bytes > last_notified_total. Keyed off the monotonic
    /// byte counter rather than `output_buffer.len()` because the buffer
    /// is a truncated tail that *shrinks* once `maybe_truncate` fires; a
    /// length-based gate would go (and stay) false after truncation.
    last_notified_total: usize,
    /// Set when a `block=true` waiter consumed this task's result.
    block_waited: bool,
    /// Display/tombstone: kill tool, UI, or teardown — not a natural exit.
    explicitly_killed: bool,
    kill_result_delivered: bool,

    /// Join handle for reading the state dump from fd 4 (persistent shell only).
    /// When present, the actor collects the dump on process exit and updates
    /// the canonical `ShellState`.
    state_dump_handle: Option<tokio::task::JoinHandle<std::io::Result<String>>>,

    /// Session that owns this process. Used to scope kill operations so
    /// subagent teardown only kills the subagent's own tasks.
    owner_session_id: Option<String>,
    description: Option<String>,
}

impl ProcessState {
    fn to_result(&self) -> TerminalRunResult {
        TerminalRunResult {
            combined_output: self.ring_output(),
            exit_code: self.lifecycle.exit_status().and_then(|s| s.exit_code),
            truncated: self.truncated,
            signal: match self.bg_status {
                BackgroundStatus::Backgrounded { reason } => Some(reason.as_signal().to_string()),
                _ => self.lifecycle.exit_status().and_then(|s| s.signal.clone()),
            },
            timed_out: self
                .lifecycle
                .exit_status()
                .map(|s| s.signal.as_deref() == Some("timeout"))
                .unwrap_or(false),
            output_file: self.output_file.clone(),
            total_bytes: self.total_bytes,
            pid: self.child.id(),
        }
    }

    fn notify_waiters(&mut self, result: Result<TerminalRunResult, ComputerError>) {
        for waiter in self.completion_waiters.drain(..) {
            let _ = waiter.send(result.clone());
        }
    }

    /// Front-and-back truncation using character counts.
    ///
    /// When the total char count of `output_buffer` exceeds `output_char_limit`:
    /// 1. Freeze the first `half` chars into `front_buffer` (done once).
    /// 2. Keep only the last `half` chars in `output_buffer`.
    ///
    /// The two halves are re-joined by `to_result()` with a separator.
    fn maybe_truncate(&mut self) {
        let s = String::from_utf8_lossy(&self.output_buffer);
        let char_count = s.chars().count();
        if char_count <= self.output_byte_limit {
            return;
        }
        let half = self.output_byte_limit / 2;

        // Capture the front half once — on the first truncation.
        if self.front_buffer.is_none() {
            let front_end = s
                .char_indices()
                .nth(half)
                .map(|(i, _)| i)
                .unwrap_or(s.len());
            self.front_buffer = Some(s[..front_end].as_bytes().to_vec());
        }

        // Keep only the last `half` chars in the tail buffer.
        let tail_start_char = char_count.saturating_sub(half);
        let tail_start_byte = s
            .char_indices()
            .nth(tail_start_char)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        self.output_buffer = s[tail_start_byte..].as_bytes().to_vec();
        self.truncated = true;
    }

    /// Flush and truncate the output file to [`MAX_RETAINED_OUTPUT_FILE_BYTES`].
    async fn flush_and_truncate_output_file(&mut self) {
        if let Some(ref mut file) = self.file_handle {
            let _ = file.flush().await;
            if self.total_bytes as u64 > MAX_RETAINED_OUTPUT_FILE_BYTES {
                let _ = file.set_len(MAX_RETAINED_OUTPUT_FILE_BYTES).await;
                // Seek to new end so post-exit drain appends correctly.
                let _ = file.seek(std::io::SeekFrom::End(0)).await;
            }
        }
    }

    fn is_timed_out(&self) -> bool {
        self.start_time.elapsed() > self.timeout
    }

    fn is_complete(&self) -> bool {
        self.lifecycle.is_complete()
    }

    /// The output is not final until `finish_output`.
    fn mark_exited(&mut self, status: ExitStatus) {
        if !self.lifecycle.has_exited() {
            self.lifecycle = Lifecycle::Exiting {
                status,
                since: Instant::now(),
            };
        }
    }

    fn finish_output(&mut self, collection: Collection) {
        self.lifecycle.finish_output(collection);
    }

    /// Build a snapshot of this process's current state.
    /// Uses async I/O to read output from disk for completed background tasks.
    async fn to_task_snapshot(&self, task_id: &str) -> TaskSnapshot {
        let swept = matches!(self.lifecycle, Lifecycle::Swept { .. });
        let (output, short_of_full_log) = if swept && !self.output_file.as_os_str().is_empty() {
            task_log::read_prefix(&self.output_file, task_log::MAX_SNAPSHOT_BYTES).await
        } else {
            (self.ring_output(), false)
        };

        TaskSnapshot {
            task_id: task_id.to_string(),
            command: self.command.clone(),
            display_command: self.display_command.clone(),
            cwd: self.cwd.clone(),
            start_time: self.start_wall_time,
            end_time: if self.lifecycle.has_exited() {
                // Use the recorded wall-clock end time if available,
                // otherwise fall back to now (process just completed this tick).
                Some(
                    self.end_wall_time
                        .unwrap_or_else(std::time::SystemTime::now),
                )
            } else {
                None
            },
            output,
            output_file: self.output_file.clone(),
            truncated: self.truncated || short_of_full_log,
            output_total_bytes: self.total_bytes,
            exit_code: self.lifecycle.exit_status().and_then(|s| s.exit_code),
            signal: self.lifecycle.exit_status().and_then(|s| s.signal.clone()),
            completed: self.is_complete(),
            block_waited: self.block_waited,
            explicitly_killed: self.explicitly_killed,
            kill_result_delivered: self.kill_result_delivered,
            kind: self.kind,
            owner_session_id: self.owner_session_id.clone(),
            description: self.description.clone(),
            is_backgrounded: self.bg_status.is_backgrounded(),
        }
    }

    /// Output held in memory: the latest part, after the earliest part once
    /// the task has run past its live limit.
    fn ring_output(&self) -> String {
        match self.front_buffer.as_ref() {
            Some(front) => format!(
                "{}{FRONT_BACK_TRUNCATION_MARKER}{}",
                String::from_utf8_lossy(front).trim_end(),
                String::from_utf8_lossy(&self.output_buffer).trim_start()
            ),
            None => String::from_utf8_lossy(&self.output_buffer).into_owned(),
        }
    }
}

// ============================================================================
// Actor
// ============================================================================

/// Waiter registered by WaitForCompletion commands.
/// Instead of blocking the actor loop, we store the reply sender and deadline,
/// then check on each poll tick whether to fire it.
struct CompletionWaiter {
    reply: oneshot::Sender<Option<TaskSnapshot>>,
    deadline: Instant,
}

/// The actor that owns all terminal state and processes commands
struct LocalTerminalActor {
    /// Command receiver
    cmd_rx: mpsc::Receiver<TerminalCommand>,

    /// Cancellation token for graceful shutdown
    cancel_token: CancellationToken,

    /// Reaper for spawned child trees. Each spawned process enrolls its
    /// `ProcessGroup` here (this actor keeps the owning `Arc`) so the TUI exit
    /// paths can `kill_all()` `setsid`-detached children that would otherwise
    /// outlive the process. Defaults to the process-global scope; tests inject
    /// their own to avoid latching the global.
    scope: crate::util::ProcessScope,

    /// Additional owner: the scope of the session that started this backend, so
    /// closing the session reaps its commands without waiting for process exit.
    /// Enrolling in both means whichever reaper fires first wins and the other
    /// finds a dead group.
    session_scope: Option<crate::util::ProcessScope>,

    /// Active processes: task_id -> ProcessState
    processes: HashMap<String, ProcessState>,

    /// task_id -> list of waiters registered by WaitForCompletion commands
    completion_waiters: HashMap<String, Vec<CompletionWaiter>>,

    /// Lightweight snapshots of completed background tasks that were evicted
    /// from `processes`. Prevents "Task not found" when the model queries
    /// a completed task after the 5-minute process eviction TTL.
    /// These are small (no child process, no file handles) and retained
    /// for the session lifetime.
    completed_task_snapshots: HashMap<String, TaskSnapshot>,

    /// How long to keep completed background tasks in `processes` before
    /// evicting them to `completed_task_snapshots`.
    completed_task_ttl: Duration,

    /// Foreground turn-blocking budget (on the actor so tests can shorten it).
    /// See [`FOREGROUND_BLOCK_BUDGET`].
    foreground_block_budget: Duration,

    /// Per-command output-file size cap (on the actor so tests can shrink it).
    /// See [`MAX_OUTPUT_FILE_BYTES`].
    output_file_cap: u64,

    /// Cgroup guard — owns the child cgroup's lifecycle.  Spawned processes
    /// are moved into this cgroup so their memory is bounded.
    _cgroup_guard: CgroupGuard,

    /// Memory-high monitor — polls for memory pressure events from the cgroup.
    memory_monitor: MemoryMonitor,

    /// Whether persistent shell state is enabled.
    persistent_shell: bool,

    login_shell_capture: bool,

    /// Per-backend `find`→`bfs` / `grep`→`ugrep` shadow enable state, resolved
    /// once by the host and baked in at construction. Passed to
    /// `search_injection` per command rather than read from a process-global, so
    /// a subagent reusing this backend can't clobber the parent's shadows.
    search_shadows: SearchShadowConfig,

    /// Shell-environment policy baked in at construction (like `search_shadows`);
    /// `None` inherits the full environment.
    shell_env_policy: Option<crate::util::ShellEnvironmentPolicy>,

    /// Persistent shell state (env vars, cwd, functions, aliases).
    /// Lazily initialized on first command when `persistent_shell` is true.
    #[cfg(unix)]
    shell_state: Option<shell_state::ShellState>,

    /// Static alias/function snapshot for the non-persistent path.
    #[cfg(unix)]
    static_shell: Option<super::static_shell::StaticShellSnapshot>,

    #[cfg(unix)]
    login_env: Option<HashMap<String, String>>,
}

impl LocalTerminalActor {
    fn new(
        cmd_rx: mpsc::Receiver<TerminalCommand>,
        cancel_token: CancellationToken,
        cgroup_guard: CgroupGuard,
        memory_monitor: MemoryMonitor,
        persistent_shell: bool,
        login_shell_capture: bool,
        search_shadows: SearchShadowConfig,
        completed_task_ttl: Duration,
        foreground_block_budget: Duration,
        output_file_cap: u64,
        scope: crate::util::ProcessScope,
        session_scope: Option<crate::util::ProcessScope>,
        shell_env_policy: Option<crate::util::ShellEnvironmentPolicy>,
    ) -> Self {
        Self {
            cmd_rx,
            cancel_token,
            scope,
            session_scope,
            shell_env_policy,
            processes: HashMap::new(),
            completion_waiters: HashMap::new(),
            completed_task_snapshots: HashMap::new(),
            completed_task_ttl,
            foreground_block_budget,
            output_file_cap,
            _cgroup_guard: cgroup_guard,
            memory_monitor,
            persistent_shell,
            login_shell_capture,
            search_shadows,
            #[cfg(unix)]
            shell_state: None,
            #[cfg(unix)]
            static_shell: None,
            #[cfg(unix)]
            login_env: None,
        }
    }

    /// Spawn a child process, using persistent shell wrapping when enabled.
    ///
    /// Returns the spawned child and an optional handle for reading the state dump
    /// from fd 4 (only present when persistent shell is active).
    async fn spawn_command(
        &mut self,
        command: &str,
        cwd: &std::path::Path,
        env: &HashMap<String, String>,
    ) -> Result<SpawnResult, ComputerError> {
        crate::util::process_safety::check_command_in(command, cwd).map_err(|error| {
            ComputerError::io_with_kind(error, std::io::ErrorKind::PermissionDenied)
        })?;
        #[cfg(unix)]
        if self.persistent_shell {
            return self.spawn_persistent_command(command, cwd, env).await;
        }

        #[cfg(unix)]
        if self.login_shell_capture && login_env_capture_enabled() {
            self.ensure_static_shell_initialized(cwd).await;
            return self.spawn_static_command(command, cwd, env).await;
        }

        #[cfg(unix)]
        if self.login_env.is_none() {
            self.login_env = Some(capture_login_env().await);
        }

        #[cfg(unix)]
        let login_env = self.login_env.as_ref();
        #[cfg(not(unix))]
        let login_env: Option<&HashMap<String, String>> = None;

        let (child, process_group) = spawn_shell_command(
            command,
            cwd,
            env,
            login_env,
            self.search_shadows,
            self.shell_env_policy.as_ref(),
        )?;
        Ok(SpawnResult {
            child,
            process_group,
            state_dump_handle: None,
        })
    }

    #[cfg(unix)]
    async fn ensure_static_shell_initialized(&mut self, cwd: &std::path::Path) {
        if self.static_shell.is_some() && self.login_env.is_some() {
            return;
        }
        let (snapshot, login_env) = tokio::join!(
            async {
                if self.static_shell.is_none() {
                    Some(super::static_shell::StaticShellSnapshot::init(cwd).await)
                } else {
                    None
                }
            },
            async {
                if self.login_env.is_none() {
                    Some(capture_login_env().await)
                } else {
                    None
                }
            }
        );
        if let Some(snapshot) = snapshot {
            self.static_shell = Some(snapshot);
        }
        if let Some(env) = login_env {
            self.login_env = Some(env);
        }
    }

    #[cfg(unix)]
    async fn spawn_static_command(
        &mut self,
        command: &str,
        cwd: &std::path::Path,
        env: &HashMap<String, String>,
    ) -> Result<SpawnResult, ComputerError> {
        use command_fds::CommandFdExt;

        let static_shell = self.static_shell.as_ref().unwrap();
        let prep = static_shell
            .prepare_command(command, self.search_shadows)
            .map_err(|e| ComputerError::io(format!("prepare static command: {e}")))?;

        let mut cmd = tokio::process::Command::new(&prep.binary);
        cmd.args(&prep.args)
            .current_dir(cwd)
            .stdin(echo_agent_tty_utils::null_stdio())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        apply_child_env(
            &mut cmd,
            self.shell_env_policy.as_ref(),
            self.login_env.as_ref(),
            env,
        );

        cmd.fd_mappings(prep.fd_mappings)
            .map_err(|e| ComputerError::io(format!("fd mapping: {e}")))?;

        unsafe {
            cmd.pre_exec(echo_agent_tty_utils::detach_pre_exec_hook());
        }

        #[cfg(target_os = "linux")]
        if echo_agent_sandbox::should_restrict_child_network() {
            unsafe {
                cmd.pre_exec(|| echo_agent_sandbox::child_net::install_child_network_filter());
            }
        }

        #[allow(clippy::disallowed_methods)] // attached to a process group below
        let child = cmd.spawn().map_err(|e| {
            ComputerError::io_with_kind(format!("spawn shell in {}: {e}", cwd.display()), e.kind())
        })?;
        drop(cmd);

        let mut process_group = crate::util::ProcessGroup::new()
            .map_err(|e| ComputerError::io(format!("ProcessGroup::new: {e}")))?;
        if let Err(e) = process_group.attach(&child) {
            tracing::debug!("Failed to attach static-shell child to ProcessGroup: {e}");
        }

        let snapshot = static_shell.snapshot.clone();
        tokio::spawn(async move {
            if let Err(e) =
                super::static_shell::write_snapshot_to_pipe(&snapshot, prep.state_in_write).await
            {
                tracing::debug!("failed to write static shell snapshot to pipe: {e}");
            }
        });

        Ok(SpawnResult {
            child,
            process_group,
            state_dump_handle: None,
        })
    }

    #[cfg(unix)]
    async fn ensure_persistent_shell_initialized(&mut self, cwd: &std::path::Path) {
        if self.shell_state.is_some() {
            return;
        }
        let shell = shell_state::ShellKind::detect();
        match shell_state::ShellState::init(shell, cwd, self.shell_env_policy.as_ref()).await {
            Ok(state) => self.shell_state = Some(state),
            Err(e) => {
                tracing::warn!("persistent shell init failed, using empty state: {e}");
                self.shell_state = Some(shell_state::ShellState {
                    cwd: cwd.to_path_buf(),
                    snapshot: String::new(),
                    shell,
                });
            }
        }
    }

    /// Spawn a command with persistent shell state: restore the prior snapshot
    /// via fd 3, run the user command, dump the new state to fd 4.
    #[cfg(unix)]
    async fn spawn_persistent_command(
        &mut self,
        command: &str,
        cwd: &std::path::Path,
        env: &HashMap<String, String>,
    ) -> Result<SpawnResult, ComputerError> {
        use command_fds::CommandFdExt;

        self.ensure_persistent_shell_initialized(cwd).await;

        let shell_state = self.shell_state.as_ref().unwrap();
        let tracked_cwd_alive = match tokio::fs::metadata(&shell_state.cwd).await {
            Ok(m) => m.is_dir(),
            Err(e) => !matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ),
        };
        let (cwd_override, spawn_notice): (Option<&std::path::Path>, Option<String>) =
            if tracked_cwd_alive {
                (None, None)
            } else {
                tracing::warn!(
                    tracked_cwd = %shell_state.cwd.display(),
                    fallback = %cwd.display(),
                    "persistent shell cwd no longer exists; falling back to request working directory"
                );
                (
                    Some(cwd),
                    Some(format!(
                        "warning: shell working directory {} no longer exists; this command ran in {} instead\n",
                        shell_state.cwd.display(),
                        cwd.display()
                    )),
                )
            };
        let prep = shell_state
            .prepare_command(
                command,
                cwd_override,
                self.search_shadows,
                spawn_notice.as_deref(),
            )
            .map_err(|e| ComputerError::io(format!("prepare persistent command: {e}")))?;

        let mut cmd = tokio::process::Command::new(&prep.binary);
        cmd.args(&prep.args)
            .current_dir(&prep.cwd)
            .stdin(echo_agent_tty_utils::null_stdio())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // The persistent backend restores login state from its snapshot, so no
        // login-env layering here.
        apply_child_env(&mut cmd, self.shell_env_policy.as_ref(), None, env);

        cmd.fd_mappings(prep.fd_mappings)
            .map_err(|e| ComputerError::io(format!("fd mapping: {e}")))?;

        unsafe {
            cmd.pre_exec(echo_agent_tty_utils::detach_pre_exec_hook());
        }

        #[cfg(target_os = "linux")]
        if echo_agent_sandbox::should_restrict_child_network() {
            unsafe {
                cmd.pre_exec(|| echo_agent_sandbox::child_net::install_child_network_filter());
            }
        }

        #[allow(clippy::disallowed_methods)] // attached to a process group below
        let child = cmd.spawn().map_err(|e| {
            ComputerError::io_with_kind(
                format!("spawn shell in {}: {e}", prep.cwd.display()),
                e.kind(),
            )
        })?;
        // Drop cmd to release the FdMapping OwnedFds held in its pre_exec closure.
        // Without this, the parent keeps the write-end of the state-out pipe open,
        // preventing the dump reader from seeing EOF.
        drop(cmd);

        let mut process_group = crate::util::ProcessGroup::new()
            .map_err(|e| ComputerError::io(format!("ProcessGroup::new: {e}")))?;
        if let Err(e) = process_group.attach(&child) {
            tracing::debug!("Failed to attach persistent-shell child to ProcessGroup: {e}");
        }

        // Write prior snapshot to fd 3 (state input pipe) in a background task.
        let snapshot = shell_state.snapshot.clone();
        tokio::spawn(async move {
            if let Err(e) =
                shell_state::write_snapshot_to_pipe(&snapshot, prep.state_in_write).await
            {
                tracing::debug!("failed to write shell snapshot to pipe: {e}");
            }
        });

        // Read new dump from fd 4 (state output pipe) in a background task.
        let dump_handle =
            tokio::spawn(
                async move { shell_state::read_dump_from_pipe(prep.state_out_read).await },
            );

        Ok(SpawnResult {
            child,
            process_group,
            state_dump_handle: Some(dump_handle),
        })
    }

    /// Main actor loop
    async fn run(mut self) {
        let mut ticker = tokio::time::interval(notification_interval());
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Bias towards commands (cancel, kill) over periodic ticking
                // so that kill_foreground_commands is handled promptly even
                // when poll_all_processes was slow (e.g. drain timeouts).
                biased;

                // Check for cancellation
                _ = self.cancel_token.cancelled() => {
                    self.shutdown_all().await;
                    break;
                }

                // Handle incoming commands
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => self.handle_command(cmd).await,
                        None => {
                            // Channel closed, all senders dropped
                            self.shutdown_all().await;
                            break;
                        }
                    }
                }

                // Periodic maintenance: check timeouts, read output, etc.
                // Gated on live processes: the actor exists for the whole
                // session lifetime, and an idle session must not wake 10x/sec
                // to poll an empty map (one actor per open session/tab adds
                // up). With the arm disabled the interval isn't polled, so no
                // timer is registered at all; the first command that spawns a
                // process re-enables it on the next loop iteration.
                _ = ticker.tick(), if !self.processes.is_empty() => {
                    self.poll_all_processes().await;
                }
            }
        }
    }

    async fn handle_command(&mut self, cmd: TerminalCommand) {
        match cmd {
            TerminalCommand::Run { request, reply } => {
                self.handle_run(request, reply).await;
            }
            TerminalCommand::RunBackground { request, reply } => {
                self.handle_run_background(request, reply).await;
            }
            TerminalCommand::GetTask { task_id, reply } => {
                let snapshot = match self.processes.get(&task_id) {
                    Some(p) => Some(p.to_task_snapshot(&task_id).await),
                    None => self.completed_task_snapshots.get(&task_id).cloned(),
                };
                let _ = reply.send(snapshot);
            }
            TerminalCommand::Kill {
                task_id,
                source,
                reply,
            } => {
                let outcome = self.handle_kill(&task_id, source).await;
                let _ = reply.send(outcome);
            }
            TerminalCommand::WaitForCompletion {
                task_id,
                timeout,
                reply,
            } => {
                self.handle_wait_for_completion(task_id, timeout, reply)
                    .await;
            }
            TerminalCommand::ListTasks { reply } => {
                let mut snapshots =
                    Vec::with_capacity(self.processes.len() + self.completed_task_snapshots.len());
                for (id, p) in &self.processes {
                    snapshots.push(p.to_task_snapshot(id).await);
                }
                for snap in self.completed_task_snapshots.values() {
                    snapshots.push(snap.clone());
                }
                let _ = reply.send(snapshots);
            }
            TerminalCommand::GetShellCwd { reply } => {
                #[cfg(unix)]
                let cwd = if self.persistent_shell {
                    self.shell_state.as_ref().map(|s| s.cwd.clone())
                } else {
                    None
                };
                #[cfg(not(unix))]
                let cwd = None;
                let _ = reply.send(cwd);
            }
            TerminalCommand::WarmShell { cwd } => {
                #[cfg(unix)]
                if self.persistent_shell {
                    // Cursor's persistent shell initializes lazily on first
                    // command; warming is only for the static capture path.
                } else if self.login_shell_capture && login_env_capture_enabled() {
                    self.ensure_static_shell_initialized(&cwd).await;
                } else if self.login_env.is_none() {
                    self.login_env = Some(capture_login_env().await);
                }
                #[cfg(not(unix))]
                let _ = cwd;
            }
            TerminalCommand::KillForegroundCommands => {
                self.kill_foreground_commands().await;
            }
            TerminalCommand::BackgroundForeground {
                tool_call_id,
                reply,
            } => {
                let found = self.handle_background_foreground(&tool_call_id);
                let _ = reply.send(found);
            }
            TerminalCommand::BackgroundForegroundCommands {
                owner_session_id,
                reply,
            } => {
                let backgrounded =
                    self.background_all_foreground_commands(owner_session_id.as_deref());
                let _ = reply.send(backgrounded);
            }
            TerminalCommand::KillForegroundCommandsByOwner { owner_session_id } => {
                self.kill_foreground_commands_by_owner(&owner_session_id)
                    .await;
            }
            TerminalCommand::KillTasksByOwner {
                owner_session_id,
                reply,
            } => {
                self.kill_tasks_by_owner(&owner_session_id).await;
                let _ = reply.send(());
            }
            TerminalCommand::ReparentNotifications {
                old_owner_session_id,
                new_owner_session_id,
                new_handle,
                backend_weak,
                reply,
            } => {
                self.reparent_notifications(
                    &old_owner_session_id,
                    &new_owner_session_id,
                    new_handle,
                    backend_weak,
                );
                let _ = reply.send(());
            }
        }
    }

    /// Wrap a freshly-spawned child's process group in an `Arc`, enroll a `Weak`
    /// into the `ProcessScope` so the TUI exit paths can reap this
    /// setsid-detached tree if the actor's own teardown never runs, and return
    /// the owning `Arc` for the `ProcessState` to hold. The actor keeps the only
    /// strong ref, so a clean reap leaves a dead `Weak` (PID-reuse-safe).
    fn enroll_spawned(
        &self,
        group: crate::util::ProcessGroup,
    ) -> std::sync::Arc<crate::util::ProcessGroup> {
        let group = std::sync::Arc::new(group);
        self.scope.register(&group);
        if let Some(session_scope) = &self.session_scope {
            // A closed session scope kills the group here, which is the point:
            // a command racing session teardown must not survive it.
            session_scope.register(&group);
        }
        group
    }

    async fn handle_run(
        &mut self,
        request: TerminalRunRequest,
        reply: oneshot::Sender<Result<TerminalRunResult, ComputerError>>,
    ) {
        // Generate an internal ID — foreground callers never see this; the reply
        // goes back on the oneshot channel.
        let internal_id = uuid::Uuid::now_v7().to_string();

        let SpawnResult {
            child,
            process_group,
            state_dump_handle,
        } = match self
            .spawn_command(&request.command, &request.working_directory, &request.env)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        };

        // Move the child process into the memory-limited cgroup (best-effort).
        if let Some(pid) = child.id()
            && let Err(e) = self._cgroup_guard.add_process(pid).await
        {
            tracing::debug!("Failed to add pid {pid} to cgroup (non-fatal): {e}");
        }

        // Open output file for writing (create parent dirs if needed)
        let file_handle = match open_output_file(&request.output_file).await {
            Ok(file) => Some(file),
            Err(e) => {
                tracing::warn!(
                    "Failed to open output file {}: {}",
                    request.output_file.display(),
                    e
                );
                None
            }
        };

        let process_state = ProcessState {
            child,
            process_group: Some(self.enroll_spawned(process_group)),
            output_buffer: Vec::new(),
            front_buffer: None,
            truncated: false,
            total_bytes: 0,
            lifecycle: Lifecycle::Running,
            bg_status: BackgroundStatus::Foreground {
                auto_bg_on_timeout: request.auto_background_on_timeout,
            },
            completion_waiters: vec![reply],
            output_byte_limit: request.output_byte_limit,
            timeout: request.timeout,
            foreground_block_budget: request
                .foreground_block_budget
                .unwrap_or(self.foreground_block_budget),
            start_time: Instant::now(),
            output_file: request.output_file,
            file_handle,
            command: request.command.clone(),
            display_command: request.display_command.clone(),
            cwd: request.working_directory.display().to_string(),
            start_wall_time: std::time::SystemTime::now(),
            end_wall_time: None,
            notification_handle: request.notification_handle.clone(),
            tool_call_id: request.tool_call_id.clone(),
            kind: request.kind,
            last_notified_total: 0,
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            state_dump_handle,
            owner_session_id: request.owner_session_id.clone(),
            description: request.description.filter(|d| !d.trim().is_empty()),
        };

        // Send an initial empty notification so the TUI shows the execution
        // timer immediately, before any stdout/stderr output arrives.
        request
            .notification_handle
            .send_output_chunk(BashOutputChunk {
                base: BashNotificationBase {
                    tool_call_id: request.tool_call_id.clone(),
                    command: request.command.clone(),
                    output: Vec::new(),
                    total_bytes: 0,
                    truncated: false,
                    cwd: request.working_directory.clone(),
                },
            });

        self.processes.insert(internal_id, process_state);
    }

    async fn handle_kill(&mut self, terminal_id: &str, source: KillSource) -> KillOutcome {
        let Some(process) = self.processes.get_mut(terminal_id) else {
            return KillOutcome::NotFound;
        };

        if process.lifecycle.has_exited() {
            return KillOutcome::AlreadyExited;
        }

        // Mark as explicitly killed BEFORE sending the kill signal so the
        // exit watcher's snapshot carries the flag.
        process.explicitly_killed = true;

        // Kill the process and finalize its state in one shot.
        let outcome = kill_and_finalize(process).await;

        // Resolve completion waiters immediately, so callers blocked on wait_for_completion() unblock right away.
        let mut any_delivered = false;
        if let Some(mut waiters) = self.completion_waiters.remove(terminal_id) {
            let snapshot = match self.processes.get(terminal_id) {
                Some(p) => Some(p.to_task_snapshot(terminal_id).await),
                None => None,
            };
            if let Some(last) = waiters.pop() {
                for waiter in waiters {
                    if waiter.reply.send(snapshot.clone()).is_ok() {
                        any_delivered = true;
                    }
                }
                if last.reply.send(snapshot).is_ok() {
                    any_delivered = true;
                }
            }
        }

        if let Some(process) = self.processes.get_mut(terminal_id) {
            process.kill_result_delivered = source.marks_result_delivered(any_delivered);
            if !any_delivered {
                process.block_waited = false;
            }
        }

        outcome
    }

    /// Handle a background execution request.
    /// Spawns the process, registers it under a generated task_id, and replies immediately.
    async fn handle_run_background(
        &mut self,
        request: TerminalRunRequest,
        reply: oneshot::Sender<Result<BackgroundHandle, ComputerError>>,
    ) {
        // Background commands fork the current shell state but don't update it on exit.
        let SpawnResult {
            child,
            process_group,
            state_dump_handle,
        } = match self
            .spawn_command(&request.command, &request.working_directory, &request.env)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        };

        // Move the child process into the memory-limited cgroup (best-effort).
        if let Some(pid) = child.id()
            && let Err(e) = self._cgroup_guard.add_process(pid).await
        {
            tracing::debug!("Failed to add pid {pid} to cgroup (non-fatal): {e}");
        }

        // Open output file for writing (create parent dirs if needed)
        let file_handle = match open_output_file(&request.output_file).await {
            Ok(file) => Some(file),
            Err(e) => {
                tracing::warn!(
                    "Failed to open output file {}: {}",
                    request.output_file.display(),
                    e
                );
                None
            }
        };

        // Generate task_id — the actor owns the identity
        let task_id = uuid::Uuid::now_v7().to_string();

        let process_state = ProcessState {
            child,
            process_group: Some(self.enroll_spawned(process_group)),
            output_buffer: Vec::new(),
            front_buffer: None,
            truncated: false,
            total_bytes: 0,
            lifecycle: Lifecycle::Running,
            bg_status: BackgroundStatus::Backgrounded {
                reason: BackgroundReason::Explicit,
            },
            completion_waiters: vec![], // no foreground waiter
            output_byte_limit: request.output_byte_limit,
            timeout: request.timeout,
            // Unused for already-backgrounded tasks; keep a defined value.
            foreground_block_budget: request
                .foreground_block_budget
                .unwrap_or(self.foreground_block_budget),
            start_time: Instant::now(),
            output_file: request.output_file.clone(),
            file_handle,
            command: request.command.clone(),
            display_command: request.display_command.clone(),
            cwd: request.working_directory.display().to_string(),
            start_wall_time: std::time::SystemTime::now(),
            end_wall_time: None,
            notification_handle: request.notification_handle.clone(),
            tool_call_id: request.tool_call_id.clone(),
            kind: request.kind,
            last_notified_total: 0,
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            // Background commands don't update the canonical shell state —
            // they may run for hours and their env mutations shouldn't leak.
            // Detach the dump reader so its result is discarded (the task
            // continues independently until EOF or DUMP_READ_TIMEOUT).
            state_dump_handle: if self.persistent_shell {
                // Still spawn with the state wrapping (so bg commands inherit
                // the session env), but discard the dump reader.
                drop(state_dump_handle);
                None
            } else {
                None
            },
            owner_session_id: request.owner_session_id.clone(),
            description: request.description.filter(|d| !d.trim().is_empty()),
        };

        // Store under task_id — this is the key that get_task/kill_task will use
        let pid = process_state.child.id();
        self.processes.insert(task_id.clone(), process_state);

        // Reply immediately
        let _ = reply.send(Ok(BackgroundHandle {
            task_id,
            output_file: request.output_file,
            pid,
        }));
    }

    /// Register a completion waiter and return immediately.
    /// The polling loop will notify this waiter when the process exits or the deadline passes.
    async fn handle_wait_for_completion(
        &mut self,
        task_id: String,
        timeout: Option<Duration>,
        reply: oneshot::Sender<Option<TaskSnapshot>>,
    ) {
        let Some(process) = self.processes.get_mut(&task_id) else {
            // Check completed snapshots (task already evicted from processes).
            // Mark `block_waited=true` in-place so any downstream consumer
            // (e.g. list_tasks, get_task) reflects that the model awaited
            // the result. Without this, a late-arriving wait would not
            // imprint the flag on the tombstone, leaving auto-wake noise
            // suppressed only on this one reply. Imprint only when the
            // waiter actually receives the reply — a dropped receiver
            // (cancelled turn) means the model never saw the result.
            let snapshot = self.completed_task_snapshots.get(&task_id).map(|s| {
                let mut s = s.clone();
                s.block_waited = true;
                s
            });
            let found = snapshot.is_some();
            let delivered = reply.send(snapshot).is_ok();
            if found
                && delivered
                && let Some(s) = self.completed_task_snapshots.get_mut(&task_id)
            {
                s.block_waited = true;
            }
            return;
        };

        // Mark so the notification bridge skips auto-wake for this task.
        // If the waiter is cancelled before receiving the result, this flag
        // is cleared again (timeout expiry in `poll_all_processes` step 2,
        // or undelivered completion in step 1) so auto-wake still fires.
        let prev_block_waited = process.block_waited;
        process.block_waited = true;

        if process.is_complete() {
            let snapshot = process.to_task_snapshot(&task_id).await;
            if reply.send(Some(snapshot)).is_err() {
                // Receiver dropped (e.g. the awaiting turn was cancelled):
                // the model never received the result, so don't let this
                // registration suppress auto-wake bookkeeping downstream.
                process.block_waited = prev_block_waited;
            }
            return;
        }

        // Register as a completion waiter and return control to the actor loop.
        let timeout = timeout.unwrap_or(Duration::from_secs(30));
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        self.completion_waiters
            .entry(task_id)
            .or_default()
            .push(CompletionWaiter { reply, deadline });

        // Return immediately — actor loop resumes processing other commands.
    }

    /// Poll all processes for output and completion
    async fn poll_all_processes(&mut self) {
        // 0a. Check if the memory monitor detected a memory.high breach.
        //     If so, kill the *newest* running foreground process (kill the
        //     most recent command first).
        if let Some(event) = self.memory_monitor.try_recv() {
            tracing::warn!(
                memory_current = event.memory_current,
                memory_high = event.memory_high_threshold,
                "Memory high threshold breached — killing newest running process"
            );

            // Find the newest running (non-exited) process by start_time.
            let newest_id = self
                .processes
                .iter()
                .filter(|(_, p)| !p.lifecycle.has_exited())
                .max_by_key(|(_, p)| p.start_time)
                .map(|(id, _)| id.clone());

            if let Some(id) = newest_id
                && let Some(process) = self.processes.get_mut(&id)
            {
                send_sigkill_to_group(process);
                drain_remaining_output(process).await;
                process.mark_exited(ExitStatus {
                    exit_code: Some(PROCESS_OOM_EXIT_CODE),
                    signal: Some("oom".to_owned()),
                });
                process.end_wall_time = Some(std::time::SystemTime::now());
                process.flush_and_truncate_output_file().await;
                process.finish_output(Collection::of(&process.child));
                let result = Ok(process.to_result());
                process.notify_waiters(result);
            }
        }

        // 0b. Kill backgrounded tasks that exceeded BACKGROUND_MAX_RUNTIME
        let bg_expired: Vec<String> = self
            .processes
            .iter()
            .filter(|(_, p)| {
                p.bg_status.is_backgrounded()
                    && !p.lifecycle.has_exited()
                    && p.start_time.elapsed() > BACKGROUND_MAX_RUNTIME
            })
            .map(|(id, _)| id.clone())
            .collect();

        for task_id in &bg_expired {
            if let Some(process) = self.processes.get_mut(task_id) {
                tracing::warn!(task_id, "Background task exceeded max runtime, killing");
                // Fire-and-forget SIGTERM — poll loop escalates to SIGKILL
                // on the next tick if the process doesn't exit.
                send_sigterm_to_group(process);
                process.mark_exited(ExitStatus {
                    exit_code: None,
                    signal: Some("max_runtime".to_owned()),
                });
                process.end_wall_time = Some(std::time::SystemTime::now());
                process.flush_and_truncate_output_file().await;
            }
        }

        // 0b (size). Kill any running task whose output file passed the cap
        // (a detached writer has no timeout watching its disk use).
        let output_cap = self.output_file_cap;
        let size_exceeded: Vec<String> = self
            .processes
            .iter()
            .filter(|(_, p)| !p.lifecycle.has_exited() && p.total_bytes as u64 > output_cap)
            .map(|(id, _)| id.clone())
            .collect();

        for task_id in &size_exceeded {
            if let Some(process) = self.processes.get_mut(task_id) {
                tracing::warn!(
                    task_id,
                    total_bytes = process.total_bytes,
                    cap = output_cap,
                    "Task exceeded output size cap, killing"
                );
                // Fire-and-forget SIGTERM — poll loop escalates to SIGKILL
                // on the next tick if the process doesn't exit.
                send_sigterm_to_group(process);
                process.mark_exited(ExitStatus {
                    exit_code: None,
                    signal: Some("output_limit".to_owned()),
                });
                process.end_wall_time = Some(std::time::SystemTime::now());
                process.flush_and_truncate_output_file().await;
                // Notify any waiter immediately (may be a still-foreground
                // command, unlike the bg-only max-runtime sweep; mirrors OOM).
                let result = Ok(process.to_result());
                process.notify_waiters(result);
            }
        }

        let task_ids: Vec<String> = self.processes.keys().cloned().collect();

        for task_id in &task_ids {
            self.poll_process(task_id).await;
        }

        // Once a child is reaped (`child.id()` is `None`), drop the actor's
        // strong `Arc<ProcessGroup>` so the `ProcessScope`'s `Weak` dies at reap.
        // A completed task lingers in `self.processes` for the TTL; keeping the
        // Arc that long would let `kill_all` upgrade the `Weak` and `killpg` a
        // pid the OS may have recycled. Runs after the poll loop so it catches a
        // reap from any path (normal/kill/oom/timeout) within one tick.
        //
        // Unix-only: only `killpg` can hit a recycled pid. On Windows the group
        // is a JobObject HANDLE (no recyclable pid), so an early drop buys
        // nothing — the Arc is released when the `ProcessState` is removed.
        #[cfg(unix)]
        for process in self.processes.values_mut() {
            if process.process_group.is_some() && process.child.id().is_none() {
                process.process_group = None;
            }
        }

        // 0c. Collect state dumps from completed foreground processes and update
        //     the canonical shell state (persistent shell only).
        //     Uses a scoped borrow on self.processes so self.shell_state can be
        //     updated afterwards.
        #[cfg(unix)]
        if self.persistent_shell {
            for task_id in &task_ids {
                let handle = {
                    let Some(process) = self.processes.get_mut(task_id) else {
                        continue;
                    };
                    // Only foreground processes update the canonical state.
                    if !process.lifecycle.has_exited() || process.bg_status.is_backgrounded() {
                        continue;
                    }
                    process.state_dump_handle.take()
                };
                // Borrow on self.processes is released — safe to access self.shell_state.
                if let Some(handle) = handle {
                    match handle.await {
                        Ok(Ok(dump)) => {
                            if let Some(ref mut state) = self.shell_state {
                                state.update_from_dump(&dump);
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::debug!("failed to read shell state dump: {e}");
                        }
                        Err(e) => {
                            tracing::debug!("shell state dump task panicked: {e}");
                        }
                    }
                }
            }
        }

        // 1. Notify completion waiters for processes that have exited
        let waiter_task_ids: Vec<String> = self.completion_waiters.keys().cloned().collect();
        for task_id in waiter_task_ids {
            let completed = self
                .processes
                .get(&task_id)
                .map(ProcessState::is_complete)
                .unwrap_or(true); // process gone = treat as completed

            if completed && let Some(waiters) = self.completion_waiters.remove(&task_id) {
                let snapshot = match self.processes.get(&task_id) {
                    Some(p) => Some(p.to_task_snapshot(&task_id).await),
                    None => None,
                };
                let mut any_delivered = false;
                for waiter in waiters {
                    if waiter.reply.send(snapshot.clone()).is_ok() {
                        any_delivered = true;
                    }
                }
                // If no waiter actually received the completion — every
                // oneshot receiver was dropped because the awaiting turn(s)
                // were cancelled (e.g. Ctrl+C mid `get_task_output`) — the
                // model never saw the result. Clear `block_waited` so the
                // TaskCompleted auto-wake in the notification bridge is NOT
                // suppressed (this runs before step 3 emits the completion
                // notification, so the cleared flag is what gets snapshotted).
                if !any_delivered && let Some(process) = self.processes.get_mut(&task_id) {
                    process.block_waited = false;
                }
            }
        }

        // 2. Expire timed-out waiters (process still running but deadline passed)
        let now = Instant::now();
        let waiter_keys: Vec<String> = self.completion_waiters.keys().cloned().collect();
        let mut timed_out_tasks: Vec<String> = Vec::new();
        for task_id in waiter_keys {
            let snapshot = match self.processes.get(&task_id) {
                Some(p) => Some(p.to_task_snapshot(&task_id).await),
                None => None,
            };
            if let Some(waiters) = self.completion_waiters.get_mut(&task_id) {
                let mut i = 0;
                while i < waiters.len() {
                    if now >= waiters[i].deadline {
                        let waiter = waiters.swap_remove(i);
                        let _ = waiter.reply.send(snapshot.clone());
                        timed_out_tasks.push(task_id.clone());
                    } else {
                        i += 1;
                    }
                }
            }
        }
        // Remove empty waiter lists
        self.completion_waiters.retain(|_, v| !v.is_empty());

        // Clear block_waited for tasks where all waiters timed out without
        // receiving the completion. Without this, auto-wake is permanently
        // suppressed even though the agent never saw the result.
        for task_id in timed_out_tasks {
            if !self.completion_waiters.contains_key(&task_id)
                && let Some(process) = self.processes.get_mut(&task_id)
            {
                process.block_waited = false;
            }
        }

        // 3. Sweep finished background tasks: drop the in-memory copy
        // First pass: mark completed and clear buffers, collect IDs for notification
        let mut newly_completed: Vec<String> = Vec::new();
        for (task_id, process) in self.processes.iter_mut() {
            if process.is_complete()
                && process.bg_status.is_backgrounded()
                && process.lifecycle.swept_at().is_none()
            {
                process.lifecycle.sweep();
                if process.end_wall_time.is_none() {
                    process.end_wall_time = Some(std::time::SystemTime::now());
                }
                // The log file has everything, and a drained task adds no more.
                process.output_buffer.clear();
                process.front_buffer = None;
                newly_completed.push(task_id.clone());
            }
        }
        // Second pass: send completion notifications (requires async file read).
        //
        // Fires unconditionally: the pager UI, persistence, and reservation
        // bookkeeping all need the snapshot. The auto-wake suppression for
        // awaited tasks lives in the bridge's `TaskCompleted` arm.
        for task_id in newly_completed {
            if let Some(process) = self.processes.get(&task_id) {
                let snapshot = process.to_task_snapshot(&task_id).await;
                process.notification_handle.send_task_complete(snapshot);
            }
        }

        // 4. Evict processes based on completion state and TTL.
        //    Before evicting completed background tasks, save a lightweight
        //    TaskSnapshot so get_task can still return status after eviction.
        let evict_ids: Vec<String> = self
            .processes
            .iter()
            .filter(|(_, p)| {
                if !p.lifecycle.has_exited() {
                    return false; // still running, keep
                }
                if !p.bg_status.is_backgrounded() {
                    return true; // foreground, already replied, evict
                }
                // Backgrounded + completed: evict after TTL
                matches!(p.lifecycle.swept_at(), Some(t) if t.elapsed() >= self.completed_task_ttl)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &evict_ids {
            if let Some(p) = self.processes.get(id)
                && p.bg_status.is_backgrounded()
            {
                // Save metadata-only snapshot (no output) before eviction so
                // get_task still returns status/exit_code. Output is on disk
                // at `output_file` — reading it into memory here would leak
                // unbounded data for long-running tasks.
                let snapshot = TaskSnapshot {
                    task_id: id.clone(),
                    command: p.command.clone(),
                    display_command: p.display_command.clone(),
                    cwd: p.cwd.clone(),
                    start_time: p.start_wall_time,
                    end_time: p.end_wall_time,
                    output: String::new(),
                    output_file: p.output_file.clone(),
                    // The output is dropped here; the log file keeps it.
                    truncated: p.truncated || p.total_bytes > 0,
                    exit_code: p.lifecycle.exit_status().and_then(|s| s.exit_code),
                    signal: p.lifecycle.exit_status().and_then(|s| s.signal.clone()),
                    completed: true,
                    kind: p.kind,
                    block_waited: p.block_waited,
                    explicitly_killed: p.explicitly_killed,
                    kill_result_delivered: p.kill_result_delivered,
                    owner_session_id: p.owner_session_id.clone(),
                    description: p.description.clone(),
                    is_backgrounded: true,
                    output_total_bytes: p.total_bytes,
                };
                self.completed_task_snapshots.insert(id.clone(), snapshot);
            }
            self.processes.remove(id);
        }
        // Cap the tombstone map by evicting the oldest entries.
        while self.completed_task_snapshots.len() > MAX_COMPLETED_TASK_SNAPSHOTS {
            let oldest = self
                .completed_task_snapshots
                .iter()
                .min_by_key(|(_, s)| s.start_time)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                self.completed_task_snapshots.remove(&id);
            } else {
                break;
            }
        }
    }

    async fn poll_process(&mut self, terminal_id: &str) {
        let Some(process) = self.processes.get_mut(terminal_id) else {
            return;
        };

        // An exited task may still hold a live child. Escalate to SIGKILL if
        // needed, drain the pipes once it dies, and keep trying to collect it.
        if process.lifecycle.has_exited() {
            if process.lifecycle.is_settled() {
                return;
            }
            let waiting_since = match &process.lifecycle {
                Lifecycle::Exiting { since, .. } => Some(*since),
                Lifecycle::Running | Lifecycle::Finished { .. } | Lifecycle::Swept { .. } => None,
            };
            match process.child.try_wait() {
                Ok(None) if process.is_complete() => {
                    // Already given up on this one; keep the kill signal fresh
                    // and keep trying to collect it.
                    send_sigkill_to_group(process);
                }
                Ok(None) => {
                    // Process was told to die but is still running — escalate to SIGKILL
                    send_sigkill_to_group(process);
                    let gave_up = waiting_since.is_some_and(|since| since.elapsed() >= REAP_GRACE);
                    if gave_up {
                        // It is not dying. Take the output there is so the task
                        // can report completion instead of waiting forever.
                        take_available_output(process).await;
                        process.flush_and_truncate_output_file().await;
                        process.finish_output(Collection::ABANDONED);
                    }
                }
                Ok(Some(_)) | Err(_) => {
                    // A second drain is harmless: the first one closes the pipes.
                    drain_remaining_output(process).await;
                    process.flush_and_truncate_output_file().await;
                    process.finish_output(Collection::of(&process.child));
                }
            }
            return;
        }

        // ── Non-blocking reads ──────────────────────────────────────────
        //
        // Read all *currently available* bytes from stdout and stderr using
        // non-blocking `poll_read`.  This avoids the old 10 ms timeout-per-
        // stream approach which cost 20 ms per process even when idle —
        // with N processes that compounded to N×20 ms per tick, easily
        // exceeding the 100 ms tick interval and delaying file writes.
        //
        // Data from both streams is collected into `new_bytes`, then written
        // to the output file in a single batch + flush at the end.

        let mut new_bytes: Vec<u8> = Vec::new();

        // Read all available stdout (non-blocking)
        let mut stdout_eof = false;
        if let Some(stdout) = process.child.stdout.as_mut() {
            loop {
                let mut buf = [0u8; READ_BUFFER_SIZE];
                match try_read_nonblocking(stdout, &mut buf) {
                    Some(Ok(0)) => {
                        stdout_eof = true;
                        break;
                    }
                    Some(Ok(n)) => {
                        new_bytes.extend_from_slice(&buf[..n]);
                    }
                    Some(Err(_)) => {
                        stdout_eof = true;
                        break;
                    }
                    None => break, // No data available right now — move on
                }
            }
        }

        // Read all available stderr (non-blocking)
        let mut stderr_eof = false;
        if let Some(stderr) = process.child.stderr.as_mut() {
            loop {
                let mut buf = [0u8; READ_BUFFER_SIZE];
                match try_read_nonblocking(stderr, &mut buf) {
                    Some(Ok(0)) => {
                        stderr_eof = true;
                        break;
                    }
                    Some(Ok(n)) => {
                        new_bytes.extend_from_slice(&buf[..n]);
                    }
                    Some(Err(_)) => {
                        stderr_eof = true;
                        break;
                    }
                    None => break, // No data available right now — move on
                }
            }
        }

        // Batch write to file + flush so the output is visible to readers
        // (e.g. `read_file` on the output_file from get_task_output).
        if !new_bytes.is_empty() {
            process.output_buffer.extend_from_slice(&new_bytes);
            process.total_bytes += new_bytes.len();
            if let Some(ref mut file) = process.file_handle {
                let _ = file.write_all(&new_bytes).await;
                let _ = file.flush().await;
            }
        }

        // Truncate in-memory buffer if needed (file has full output)
        process.maybe_truncate();

        // Send output chunk notification if there's new output since last tick.
        // This happens every ~100ms (the actor's tick interval).
        // If the handle is noop(), send() silently drops — no performance cost.
        //
        // Keyed off the monotonic `total_bytes` (not `output_buffer.len()`):
        // after `maybe_truncate` freezes the front half and keeps only the
        // shrinking tail, a length-based gate would go false and stay false,
        // starving the stream of chunks for the rest of a long-output command.
        if process.total_bytes > process.last_notified_total {
            process
                .notification_handle
                .send_output_chunk(BashOutputChunk {
                    base: BashNotificationBase {
                        tool_call_id: process.tool_call_id.clone(),
                        command: process.command.clone(),
                        output: process.output_buffer.clone(),
                        total_bytes: process.total_bytes,
                        truncated: process.truncated,
                        cwd: process.cwd.clone().into(),
                    },
                });
            process.last_notified_total = process.total_bytes;
        }

        // Foreground budget: auto-backgroundable commands stop blocking the
        // turn after the per-process budget (independent of `timeout`) — this
        // second timer only backgrounds, never kills. Default is 15s; sessions
        // can override via BashParams.foreground_block_budget_ms (0 = disable
        // short budget so only `timeout` auto-bgs). The `timeout` check below
        // also auto-bgs when auto_bg is on, or kills when it is off.
        if !process.lifecycle.has_exited()
            && matches!(
                process.bg_status,
                BackgroundStatus::Foreground {
                    auto_bg_on_timeout: true
                }
            )
            && process.start_time.elapsed() > process.foreground_block_budget
        {
            self.transition_to_background(terminal_id, BackgroundReason::ForegroundTimeout);
            return;
        }

        // Check for timeout.
        if process.is_timed_out() && !process.lifecycle.has_exited() {
            if matches!(
                process.bg_status,
                BackgroundStatus::Foreground {
                    auto_bg_on_timeout: true
                }
            ) {
                self.transition_to_background(terminal_id, BackgroundReason::ForegroundTimeout);
                return;
            }

            // Default: kill the process on timeout.
            send_sigterm_to_group(process);
            process.mark_exited(ExitStatus {
                exit_code: None,
                signal: Some("timeout".to_owned()),
            });
            process.end_wall_time = Some(std::time::SystemTime::now());
            process.flush_and_truncate_output_file().await;
            let result = Ok(process.to_result());
            process.notify_waiters(result);
            return;
        }

        // Check if process exited (both streams at EOF or process exited)
        let process_done = stdout_eof && stderr_eof;
        match process.child.try_wait() {
            Ok(Some(status)) => {
                // Process exited — drain any remaining stdout/stderr that arrived
                // after the timeout-based reads above. This fixes a race where fast
                // commands (e.g. `python3 -c "print('x')"`) exit before their pipe
                // buffers are read, resulting in empty output.
                drain_remaining_output(process).await;

                process.mark_exited(extract_exit_status(status));
                process.end_wall_time = Some(std::time::SystemTime::now());
                process.flush_and_truncate_output_file().await;
                process.finish_output(Collection::of(&process.child));
                let result = Ok(process.to_result());
                process.notify_waiters(result);
            }
            Ok(None) if process_done => {
                // Streams closed but process hasn't exited yet - wait a bit
            }
            Ok(None) => {
                // Still running
            }
            Err(e) => {
                drain_remaining_output(process).await;
                process.mark_exited(ExitStatus {
                    exit_code: None,
                    signal: Some(format!("error: {}", e)),
                });
                process.end_wall_time = Some(std::time::SystemTime::now());
                process.flush_and_truncate_output_file().await;
                // An erroring `try_wait` is no proof the child was
                // collected; keep polling.
                process.finish_output(Collection::of(&process.child));
                let result = Ok(process.to_result());
                process.notify_waiters(result);
            }
        }
    }

    async fn shutdown_all(&mut self) {
        for (_, process) in self.processes.iter_mut() {
            send_sigkill_to_group(process);
            // Abort the state dump reader so its spawn_blocking thread
            // doesn't outlive the actor.
            if let Some(handle) = process.state_dump_handle.take() {
                handle.abort();
            }
        }
        self.processes.clear();
    }

    /// Transition a foreground process to background (shared by auto-timeout
    /// and user Ctrl+G). Re-keys from `old_key` to `tool_call_id`.
    fn transition_to_background(&mut self, old_key: &str, reason: BackgroundReason) -> bool {
        let Some(mut process) = self.processes.remove(old_key) else {
            return false;
        };
        process.bg_status = BackgroundStatus::Backgrounded { reason };
        process.timeout = BACKGROUND_MAX_RUNTIME;
        let result = Ok(process.to_result());
        process.notify_waiters(result);
        let tool_call_id = process.tool_call_id.clone();

        tracing::info!(
            tool_call_id = %tool_call_id,
            ?reason,
            "Foreground command transitioned to background"
        );
        self.processes.insert(tool_call_id, process);
        true
    }

    /// Background a foreground command by `tool_call_id` (user Ctrl+G).
    fn handle_background_foreground(&mut self, tool_call_id: &str) -> bool {
        let internal_id = self
            .processes
            .iter()
            .find(|(_, p)| p.tool_call_id == tool_call_id && !p.bg_status.is_backgrounded())
            .map(|(id, _)| id.clone());

        let Some(internal_id) = internal_id else {
            return false;
        };

        self.transition_to_background(&internal_id, BackgroundReason::UserSignal)
    }

    /// The non-lethal twin of [`Self::kill_foreground_commands`]: on a mid-turn redirect a running command is kept alive, not SIGKILLed.
    fn background_all_foreground_commands(
        &mut self,
        owner_session_id: Option<&str>,
    ) -> Vec<BackgroundedForeground> {
        // Collect internal ids first: `transition_to_background` re-keys the map, so we cannot hold an iterator across the mutation.
        let targets: Vec<(String, BackgroundedForeground)> = self
            .processes
            .iter()
            .filter(|(_, p)| {
                !p.bg_status.is_backgrounded()
                    && !p.lifecycle.has_exited()
                    && owner_session_id
                        .is_none_or(|owner| p.owner_session_id.as_deref() == Some(owner))
            })
            .map(|(id, p)| {
                (
                    id.clone(),
                    BackgroundedForeground {
                        tool_call_id: p.tool_call_id.clone(),
                    },
                )
            })
            .collect();

        let mut backgrounded = Vec::with_capacity(targets.len());
        for (internal_id, info) in targets {
            if self.transition_to_background(&internal_id, BackgroundReason::UserSignal) {
                // The transition above re-keys the map, so the process now sits under its tool call id.
                if let Some(process) = self.processes.get(&info.tool_call_id) {
                    Self::announce_backgrounded_command(process);
                }
                backgrounded.push(info);
            }
        }
        backgrounded
    }

    /// Normally the bash tool tells the client itself when its command moves to the background, but its turn was stopped, so it usually never runs again.
    /// If it does still run, the client hears about the command twice, which is better than never seeing it at all.
    fn announce_backgrounded_command(process: &ProcessState) {
        process.notification_handle.send_backgrounded(
            crate::notification::BashExecutionBackgrounded {
                base: crate::notification::BashNotificationBase {
                    tool_call_id: process.tool_call_id.clone(),
                    command: process
                        .display_command
                        .clone()
                        .unwrap_or_else(|| process.command.clone()),
                    // The shell drops these three before the wire, so copying the whole buffer here would be wasted work.
                    output: Vec::new(),
                    total_bytes: 0,
                    truncated: false,
                    cwd: std::path::PathBuf::from(&process.cwd),
                },
                output_file: process.output_file.clone(),
                task_id: process.tool_call_id.clone(),
                monitor_description: None,
                description: process.description.clone().filter(|d| !d.trim().is_empty()),
            },
        );
    }

    /// Kill all non-backgrounded (foreground) processes and notify their waiters.
    /// Backgrounded processes are left untouched. The actor stays alive for reuse.
    ///
    /// After sending SIGKILL we **wait for each child to actually exit** (with a
    /// 5 s timeout) so the kernel reclaims its memory pages before the next tool
    /// call can allocate.  Without this wait, a rapid OOM → recover → OOM cycle
    /// can hit memory.max because the previous child's RSS hasn't been freed yet.
    async fn kill_foreground_commands(&mut self) {
        let fg_ids: Vec<String> = self
            .processes
            .iter()
            .filter(|(_, p)| !p.bg_status.is_backgrounded() && !p.lifecycle.has_exited())
            .map(|(id, _)| id.clone())
            .collect();

        for id in &fg_ids {
            if let Some(process) = self.processes.get_mut(id) {
                send_sigkill_to_group(process);

                // Wait for the child to actually exit so the kernel reclaims
                // its memory.  Bounded to 5 s — SIGKILL is unconditional so
                // this should resolve almost instantly in practice.
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(5), process.child.wait())
                        .await;

                // Abort the state dump reader task so its `spawn_blocking`
                // thread doesn't leak. Without this, a grandchild that
                // inherited fd 4 and escaped the process group keeps the
                // pipe's write end open, and the blocking `read_to_string`
                // hangs indefinitely (the timeout future was abandoned when
                // the JoinHandle was dropped).
                if let Some(handle) = process.state_dump_handle.take() {
                    handle.abort();
                }

                process.mark_exited(ExitStatus {
                    exit_code: None,
                    signal: Some("cancelled".to_owned()),
                });
                process.end_wall_time = Some(std::time::SystemTime::now());
                process.flush_and_truncate_output_file().await;

                let result = Ok(process.to_result());
                process.notify_waiters(result);
            }
        }

        // Remove dead foreground entries
        for id in &fg_ids {
            self.processes.remove(id);
        }
    }

    /// Kill all running foreground processes owned by a specific session.
    /// Processes owned by other sessions are left untouched.
    async fn kill_foreground_commands_by_owner(&mut self, owner_session_id: &str) {
        let fg_ids: Vec<String> = self
            .processes
            .iter()
            .filter(|(_, p)| {
                p.owner_session_id.as_deref() == Some(owner_session_id)
                    && !p.bg_status.is_backgrounded()
                    && !p.lifecycle.has_exited()
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in &fg_ids {
            if let Some(process) = self.processes.get_mut(id) {
                send_sigkill_to_group(process);
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(5), process.child.wait())
                        .await;
                if let Some(handle) = process.state_dump_handle.take() {
                    handle.abort();
                }
                process.mark_exited(ExitStatus {
                    exit_code: None,
                    signal: Some("cancelled".to_owned()),
                });
                process.end_wall_time = Some(std::time::SystemTime::now());
                process.flush_and_truncate_output_file().await;
                let result = Ok(process.to_result());
                process.notify_waiters(result);
            }
        }

        for id in &fg_ids {
            self.processes.remove(id);
        }
    }

    /// Kill all running background tasks owned by a specific session.
    /// Foreground tasks and tasks owned by other sessions are left untouched.
    async fn kill_tasks_by_owner(&mut self, owner_session_id: &str) {
        let owned_ids: Vec<String> = self
            .processes
            .iter()
            .filter(|(_, p)| {
                p.owner_session_id.as_deref() == Some(owner_session_id)
                    && !p.lifecycle.has_exited()
                    && p.bg_status.is_backgrounded()
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in owned_ids {
            self.handle_kill(&id, KillSource::Teardown).await;
        }
    }

    /// Reparent notification handles for all tasks owned by `old_owner_session_id`.
    /// Swaps the notification handle to `new_handle` so events from surviving
    /// processes (monitors, background tasks) route to the parent session.
    ///
    /// Also sends synthetic `BashExecutionBackgrounded` notifications through
    /// the new handle for each reparented background task, so the parent's TUI
    /// creates a `bg_tasks` entry and subsequent `MonitorEvent`/`TaskCompleted`
    /// notifications have a target to attach to.
    ///
    /// For monitors, re-spawns the output pipeline so events continue streaming.
    fn reparent_notifications(
        &mut self,
        old_owner_session_id: &str,
        new_owner_session_id: &str,
        new_handle: crate::notification::types::ToolNotificationHandle,
        backend_weak: std::sync::Weak<dyn crate::computer::types::TerminalBackend>,
    ) {
        for (task_id, process) in self.processes.iter_mut() {
            if process.owner_session_id.as_deref() == Some(old_owner_session_id)
                && process.bg_status.is_backgrounded()
                && !process.lifecycle.has_exited()
            {
                // Only reparent backgrounded, still-running tasks. Foreground
                // processes keep the child's owner_session_id so the subsequent
                // kill_foreground_commands_by_owner call can reap them.
                process.owner_session_id = Some(new_owner_session_id.to_string());
                process.notification_handle = new_handle.clone();

                // Send a synthetic backgrounded notification so the parent's
                // TUI registers this task in its bg_tasks map. For a reparented
                // monitor, recover the human description from the baked
                // "[monitor] <desc>" display command and forward the real
                // command + `monitor_description`, so the pager renders a proper
                // "Monitor" row (matching the original-spawn path) rather than a
                // bash-highlighted "[monitor] …".
                let is_monitor = process.kind == crate::computer::types::TaskKind::Monitor;
                // Recover monitor label once; reuse for backgrounded notify + pipeline.
                // Filter empty/whitespace the same way as spawn so `[monitor] `
                // / blank recovery does not stick as Some("") and block the
                // command fallback for the re-spawned pipeline label.
                let recovered_monitor_description = if is_monitor {
                    process
                        .display_command
                        .as_deref()
                        .and_then(|d| d.strip_prefix("[monitor] "))
                        .map(str::to_string)
                        .filter(|d| !d.trim().is_empty())
                } else {
                    None
                };
                let effective_description = process
                    .description
                    .clone()
                    .filter(|d| !d.trim().is_empty())
                    .or_else(|| recovered_monitor_description.clone());
                let reparent_command = if is_monitor {
                    process.command.clone()
                } else {
                    process
                        .display_command
                        .clone()
                        .unwrap_or_else(|| process.command.clone())
                };
                new_handle.send_backgrounded(crate::notification::BashExecutionBackgrounded {
                    base: crate::notification::BashNotificationBase {
                        tool_call_id: process.tool_call_id.clone(),
                        command: reparent_command,
                        output: Vec::new(),
                        total_bytes: 0,
                        truncated: false,
                        cwd: std::path::PathBuf::from(&process.cwd),
                    },
                    output_file: process.output_file.clone(),
                    task_id: task_id.clone(),
                    monitor_description: recovered_monitor_description,
                    description: effective_description.clone(),
                });

                // Re-spawn the monitor pipeline so events continue streaming.
                // The old pipeline died with the child's runtime.
                if process.kind == crate::computer::types::TaskKind::Monitor {
                    let pipeline_task_id = task_id.clone();
                    let pipeline_description =
                        effective_description.unwrap_or_else(|| process.command.clone());
                    // Weak so the reparented monitor doesn't pin the backend.
                    let pipeline_terminal = backend_weak.clone();
                    let pipeline_notif = new_handle.clone();
                    let pipeline_output_file = process.output_file.clone();
                    // Start from current file size so we don't re-emit
                    // events already delivered by the old pipeline.
                    let start_offset = std::fs::metadata(&pipeline_output_file)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    tokio::spawn(async move {
                        crate::implementations::echo_agent_build::monitor::tool::run_monitor_pipeline(
                            &pipeline_task_id,
                            &pipeline_description,
                            pipeline_terminal,
                            &pipeline_notif,
                            &pipeline_output_file,
                            Some("kill_command_or_subagent".to_string()),
                            start_offset,
                        )
                        .await;
                    });
                }
            }
        }
    }
}

// ============================================================================
// Handle (public API)
// ============================================================================

/// Handle to interact with the terminal actor.
///
/// This is the public API that implements `TerminalBackend`.
/// It sends commands to the actor via channels - no mutex locks needed.
#[derive(Clone)]
pub struct LocalTerminalBackend {
    cmd_tx: mpsc::Sender<TerminalCommand>,
    cancel_token: CancellationToken,
}

/// Grouped inputs for [`LocalTerminalBackend::new_inner`], so call sites read as
/// named fields instead of a telescoping list of positional `bool`s. Constructors
/// override only the fields they vary via `..Default::default()`.
struct LocalTerminalConfig {
    memory_config: Option<CgroupMemoryConfig>,
    use_spawn_local: bool,
    persistent_shell: bool,
    login_shell_capture: bool,
    search_shadows: SearchShadowConfig,
    shell_env_policy: Option<crate::util::ShellEnvironmentPolicy>,
    process_scope: Option<crate::util::ProcessScope>,
}

impl Default for LocalTerminalConfig {
    fn default() -> Self {
        Self {
            memory_config: None,
            use_spawn_local: false,
            persistent_shell: false,
            login_shell_capture: true,
            search_shadows: SearchShadowConfig::default(),
            shell_env_policy: None,
            process_scope: None,
        }
    }
}

impl LocalTerminalBackend {
    /// Create a new LocalTerminalBackend and spawn the actor task.
    ///
    /// The actor runs in a spawned task and processes commands from the channel.
    /// If `memory_config` is provided, a cgroupv2 memory limit is enforced on
    /// all spawned commands (Linux only; silently degrades to no-op elsewhere).
    pub fn new() -> Self {
        Self::new_inner(LocalTerminalConfig::default())
    }

    /// Create a new LocalTerminalBackend with persistent shell state.
    ///
    /// When enabled, environment variables, working directory, functions, aliases,
    /// and shell options persist across command invocations. The user's login shell
    /// (bash or zsh) is detected and its rc files are loaded once on first command.
    pub fn with_persistent_shell() -> Self {
        Self::new_inner(LocalTerminalConfig {
            persistent_shell: true,
            ..Default::default()
        })
    }

    /// Create a new LocalTerminalBackend with cgroup memory limits.
    ///
    /// See [`CgroupMemoryConfig`] for details on the soft/hard limit model.
    pub fn with_memory_limit(config: CgroupMemoryConfig) -> Self {
        Self::new_inner(LocalTerminalConfig {
            memory_config: Some(config),
            ..Default::default()
        })
    }

    /// Create a new LocalTerminalBackend using spawn_local (for single-threaded runtimes).
    ///
    /// `search_shadows` is the host-resolved `find`→`bfs` / `grep`→`ugrep` enable
    /// state, baked into this backend (see [`SearchShadowConfig`]).
    pub fn new_local(search_shadows: SearchShadowConfig) -> Self {
        Self::new_inner(LocalTerminalConfig {
            use_spawn_local: true,
            search_shadows,
            ..Default::default()
        })
    }

    pub fn new_local_with_login_shell_capture(
        search_shadows: SearchShadowConfig,
        login_shell_capture: bool,
        shell_env_policy: Option<crate::util::ShellEnvironmentPolicy>,
        process_scope: Option<crate::util::ProcessScope>,
    ) -> Self {
        Self::new_inner(LocalTerminalConfig {
            use_spawn_local: true,
            login_shell_capture,
            search_shadows,
            shell_env_policy,
            process_scope,
            ..Default::default()
        })
    }

    /// Create a new LocalTerminalBackend using spawn_local with persistent shell.
    ///
    /// `search_shadows` is the host-resolved `find`→`bfs` / `grep`→`ugrep` enable
    /// state, baked into this backend (see [`SearchShadowConfig`]).
    pub fn new_local_with_persistent_shell(
        search_shadows: SearchShadowConfig,
        shell_env_policy: Option<crate::util::ShellEnvironmentPolicy>,
        process_scope: Option<crate::util::ProcessScope>,
    ) -> Self {
        Self::new_inner(LocalTerminalConfig {
            use_spawn_local: true,
            persistent_shell: true,
            search_shadows,
            shell_env_policy,
            process_scope,
            ..Default::default()
        })
    }

    /// Test-only: a spawn_local backend that enrolls spawned children into
    /// `scope` instead of the process-global one, so a test can `kill_all()` in
    /// isolation without latching the global scope shared by other tests.
    #[cfg(test)]
    pub(crate) fn new_local_with_scope(
        search_shadows: SearchShadowConfig,
        scope: crate::util::ProcessScope,
        session_scope: Option<crate::util::ProcessScope>,
    ) -> Self {
        Self::new_with_ttl(
            None,
            true,
            false,
            true,
            search_shadows,
            COMPLETED_TASK_TTL,
            FOREGROUND_BLOCK_BUDGET,
            MAX_OUTPUT_FILE_BYTES,
            scope,
            session_scope,
            None,
        )
    }

    /// Create a backend with a custom completed-task TTL (for testing).
    #[cfg(test)]
    pub(crate) fn new_with_completed_task_ttl(ttl: Duration) -> Self {
        Self::new_with_ttl(
            None,
            false,
            false,
            true,
            SearchShadowConfig::default(),
            ttl,
            FOREGROUND_BLOCK_BUDGET,
            MAX_OUTPUT_FILE_BYTES,
            crate::util::global_process_scope().clone(),
            None,
            None,
        )
    }

    /// Backend with a custom foreground budget (test-only).
    #[cfg(test)]
    pub(crate) fn new_with_foreground_budget(budget: Duration) -> Self {
        Self::new_with_ttl(
            None,
            false,
            false,
            true,
            SearchShadowConfig::default(),
            COMPLETED_TASK_TTL,
            budget,
            MAX_OUTPUT_FILE_BYTES,
            crate::util::global_process_scope().clone(),
            None,
            None,
        )
    }

    /// Backend with a custom output-file size cap (test-only).
    #[cfg(test)]
    pub(crate) fn new_with_output_cap(output_file_cap: u64) -> Self {
        Self::new_with_ttl(
            None,
            false,
            false,
            true,
            SearchShadowConfig::default(),
            COMPLETED_TASK_TTL,
            FOREGROUND_BLOCK_BUDGET,
            output_file_cap,
            crate::util::global_process_scope().clone(),
            None,
            None,
        )
    }

    fn new_inner(config: LocalTerminalConfig) -> Self {
        let LocalTerminalConfig {
            memory_config,
            use_spawn_local,
            persistent_shell,
            login_shell_capture,
            search_shadows,
            shell_env_policy,
            process_scope,
        } = config;
        Self::new_with_ttl(
            memory_config,
            use_spawn_local,
            persistent_shell,
            login_shell_capture,
            search_shadows,
            COMPLETED_TASK_TTL,
            foreground_block_budget_from_env(),
            output_file_cap_from_env(),
            crate::util::global_process_scope().clone(),
            process_scope,
            shell_env_policy,
        )
    }

    fn new_with_ttl(
        memory_config: Option<CgroupMemoryConfig>,
        use_spawn_local: bool,
        persistent_shell: bool,
        login_shell_capture: bool,
        search_shadows: SearchShadowConfig,
        completed_task_ttl: Duration,
        foreground_block_budget: Duration,
        output_file_cap: u64,
        scope: crate::util::ProcessScope,
        session_scope: Option<crate::util::ProcessScope>,
        shell_env_policy: Option<crate::util::ShellEnvironmentPolicy>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CHANNEL_SIZE);
        let cancel_token = CancellationToken::new();

        let cancel_token_clone = cancel_token.clone();
        let actor_fut = async move {
            let (cgroup_guard, memory_monitor) = match memory_config {
                Some(config) => {
                    let guard = CgroupGuard::try_create(&config).await;
                    let monitor = MemoryMonitor::start(&guard, &config, use_spawn_local).await;
                    (guard, monitor)
                }
                None => (CgroupGuard::noop(), MemoryMonitor::noop()),
            };
            let actor = LocalTerminalActor::new(
                cmd_rx,
                cancel_token_clone,
                cgroup_guard,
                memory_monitor,
                persistent_shell,
                login_shell_capture,
                search_shadows,
                completed_task_ttl,
                foreground_block_budget,
                output_file_cap,
                scope,
                session_scope,
                shell_env_policy,
            );
            actor.run().await;
        };

        if use_spawn_local {
            tokio::task::spawn_local(actor_fut);
        } else {
            tokio::spawn(actor_fut);
        }

        Self {
            cmd_tx,
            cancel_token,
        }
    }

    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }
}

impl Default for LocalTerminalBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl TerminalBackend for LocalTerminalBackend {
    async fn run(&self, request: TerminalRunRequest) -> Result<TerminalRunResult, ComputerError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.cmd_tx
            .send(TerminalCommand::Run {
                request,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ComputerError::io("terminal actor shut down"))?;

        reply_rx
            .await
            .map_err(|_| ComputerError::io("terminal actor dropped reply channel"))?
    }

    async fn run_background(
        &self,
        request: TerminalRunRequest,
    ) -> Result<BackgroundHandle, ComputerError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.cmd_tx
            .send(TerminalCommand::RunBackground {
                request,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ComputerError::io("terminal actor shut down"))?;

        reply_rx
            .await
            .map_err(|_| ComputerError::io("terminal actor dropped reply channel"))?
    }

    async fn get_task(&self, task_id: &str) -> Option<TaskSnapshot> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(TerminalCommand::GetTask {
                task_id: task_id.to_string(),
                reply: reply_tx,
            })
            .await
            .ok()?;
        reply_rx.await.ok().flatten()
    }

    async fn kill_task(&self, task_id: &str) -> KillOutcome {
        self.kill_task_with_source(task_id, KillSource::ModelTool)
            .await
    }

    async fn kill_task_with_source(&self, task_id: &str, source: KillSource) -> KillOutcome {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(TerminalCommand::Kill {
                task_id: task_id.to_string(),
                source,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return KillOutcome::NotFound;
        }
        reply_rx.await.unwrap_or(KillOutcome::NotFound)
    }

    async fn wait_for_completion(
        &self,
        task_id: &str,
        timeout: Option<Duration>,
    ) -> Option<TaskSnapshot> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(TerminalCommand::WaitForCompletion {
                task_id: task_id.to_string(),
                timeout,
                reply: reply_tx,
            })
            .await
            .ok()?;
        reply_rx.await.ok().flatten()
    }

    async fn list_tasks(&self) -> Vec<TaskSnapshot> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(TerminalCommand::ListTasks { reply: reply_tx })
            .await
            .is_err()
        {
            return vec![];
        }
        reply_rx.await.unwrap_or_default()
    }

    async fn get_shell_cwd(&self) -> Option<PathBuf> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(TerminalCommand::GetShellCwd { reply: reply_tx })
            .await
            .ok()?;
        reply_rx.await.ok().flatten()
    }

    async fn warm_shell(&self, cwd: &std::path::Path) {
        let _ = self
            .cmd_tx
            .send(TerminalCommand::WarmShell {
                cwd: cwd.to_path_buf(),
            })
            .await;
    }

    async fn kill_foreground_commands(&self) {
        let _ = self
            .cmd_tx
            .send(TerminalCommand::KillForegroundCommands)
            .await;
    }

    async fn kill_all_background_tasks(&self) {
        let tasks = self.list_tasks().await;
        for task in tasks {
            if task.exit_code.is_none() && task.signal.is_none() {
                self.kill_task_with_source(&task.task_id, KillSource::Teardown)
                    .await;
            }
        }
    }

    async fn kill_foreground_commands_by_owner(&self, owner_session_id: &str) {
        let _ = self
            .cmd_tx
            .send(TerminalCommand::KillForegroundCommandsByOwner {
                owner_session_id: owner_session_id.to_string(),
            })
            .await;
    }

    async fn kill_all_background_tasks_by_owner(&self, owner_session_id: &str) {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(TerminalCommand::KillTasksByOwner {
                owner_session_id: owner_session_id.to_string(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return;
        }
        let _ = reply_rx.await;
    }

    async fn reparent_notifications(
        &self,
        old_owner_session_id: &str,
        new_owner_session_id: &str,
        new_handle: crate::notification::types::ToolNotificationHandle,
        backend_weak: std::sync::Weak<dyn crate::computer::types::TerminalBackend>,
    ) {
        let (reply_tx, reply_rx) = oneshot::channel();
        // `backend_weak` (anchored by the parent's `Arc`) drives re-spawned
        // pipelines without keeping the backend alive.
        if self
            .cmd_tx
            .send(TerminalCommand::ReparentNotifications {
                old_owner_session_id: old_owner_session_id.to_string(),
                new_owner_session_id: new_owner_session_id.to_string(),
                new_handle,
                backend_weak,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return;
        }
        // Wait for the actor to process the reparent before returning,
        // so the caller can safely shut down the old session without
        // risking notification loss.
        let _ = reply_rx.await;
    }

    async fn background_foreground_command(&self, tool_call_id: &str) -> bool {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(TerminalCommand::BackgroundForeground {
                tool_call_id: tool_call_id.to_string(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        reply_rx.await.unwrap_or(false)
    }

    async fn background_foreground_commands(
        &self,
        owner_session_id: Option<&str>,
    ) -> Vec<BackgroundedForeground> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(TerminalCommand::BackgroundForegroundCommands {
                owner_session_id: owner_session_id.map(str::to_string),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return Vec::new();
        }
        reply_rx.await.unwrap_or_default()
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Non-blocking read: returns `Some(Ok(n))` if data is available,
/// `Some(Err(e))` on I/O error, `Some(Ok(0))` on EOF, or `None` if
/// no data is ready right now.
///
/// Uses `Waker::noop()` — safe because the actor runs a periodic
/// polling loop and doesn't need wake-up notifications from the pipe.
/// This eliminates the 10 ms timeout-per-read that previously caused
/// O(N × 20 ms) per-tick overhead for N processes.
fn try_read_nonblocking(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
    buf: &mut [u8],
) -> Option<std::io::Result<usize>> {
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};
    use tokio::io::ReadBuf;

    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut read_buf = ReadBuf::new(buf);
    match Pin::new(reader).poll_read(&mut cx, &mut read_buf) {
        Poll::Ready(Ok(())) => Some(Ok(read_buf.filled().len())),
        Poll::Ready(Err(e)) => Some(Err(e)),
        Poll::Pending => None,
    }
}
/// Soft-kill the process tree. Non-blocking, fire-and-forget.
/// The actor's poll loop will reap the process on the next tick.
fn send_sigterm_to_group(process: &ProcessState) {
    // Unix: skip if child already reaped (avoid pid-reuse race on the
    // stored leader_pid). Windows JobObjects don't have this issue.
    #[cfg(unix)]
    if process.child.id().is_none() {
        return;
    }
    if let Some(pg) = process.process_group.as_ref() {
        let _ = pg.terminate();
    }
}

/// Hard-kill the process tree, plus `start_kill` on the immediate child
/// as a fallback when group teardown is degraded.
fn send_sigkill_to_group(process: &mut ProcessState) {
    // Unix: skip if child already reaped (see send_sigterm_to_group).
    #[cfg(unix)]
    if process.child.id().is_none() {
        let _ = process.child.start_kill();
        return;
    }
    if let Some(pg) = process.process_group.as_ref() {
        let _ = pg.kill();
    }
    let _ = process.child.start_kill();
}

/// Drain remaining stdout/stderr into the output buffer and file.
/// Bounded by `DRAIN_TIMEOUT` so a backgrounded child holding the
/// pipe open cannot block the actor loop indefinitely.
async fn drain_remaining_output(process: &mut ProcessState) {
    let timed_out = tokio::time::timeout(DRAIN_TIMEOUT, async {
        if let Some(stdout) = process.child.stdout.as_mut() {
            let mut buf = [0u8; READ_BUFFER_SIZE];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        process.output_buffer.extend_from_slice(&buf[..n]);
                        process.total_bytes += n;
                        if let Some(ref mut file) = process.file_handle {
                            let _ = file.write_all(&buf[..n]).await;
                        }
                    }
                }
            }
        }

        if let Some(stderr) = process.child.stderr.as_mut() {
            let mut buf = [0u8; READ_BUFFER_SIZE];
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        process.output_buffer.extend_from_slice(&buf[..n]);
                        process.total_bytes += n;
                        if let Some(ref mut file) = process.file_handle {
                            let _ = file.write_all(&buf[..n]).await;
                        }
                    }
                }
            }
        }
    })
    .await;

    if timed_out.is_err() {
        tracing::debug!(
            command = %process.command,
            "drain timed out after {:?}, a backgrounded child may be holding the pipe open",
            DRAIN_TIMEOUT,
        );
    }

    // Drop the pipe handles so orphaned children holding them open cannot
    // cause repeated drain timeouts on subsequent poll ticks.
    process.child.stdout.take();
    process.child.stderr.take();

    process.maybe_truncate();
}

/// Take the output already sitting in the pipes, then drop the handles.
/// Never waits: a live pipe would hold the single threaded actor for the
/// full drain timeout, so this is safe on a process that is still running.
async fn take_available_output(process: &mut ProcessState) {
    let mut collected = Vec::new();
    if let Some(stdout) = process.child.stdout.as_mut() {
        read_available(stdout, &mut collected);
    }
    if let Some(stderr) = process.child.stderr.as_mut() {
        read_available(stderr, &mut collected);
    }
    process.child.stdout.take();
    process.child.stderr.take();

    if collected.is_empty() {
        return;
    }
    process.output_buffer.extend_from_slice(&collected);
    process.total_bytes += collected.len();
    if let Some(file) = process.file_handle.as_mut() {
        let _ = file.write_all(&collected).await;
    }
    process.maybe_truncate();
}

fn read_available(reader: &mut (impl tokio::io::AsyncRead + Unpin), out: &mut Vec<u8>) {
    let mut buf = [0u8; READ_BUFFER_SIZE];
    loop {
        match try_read_nonblocking(reader, &mut buf) {
            Some(Ok(0)) | Some(Err(_)) | None => return,
            Some(Ok(n)) => out.extend_from_slice(&buf[..n]),
        }
    }
}

/// Two-phase kill that synchronously waits for the process to exit.
/// Used ONLY by `kill_and_finalize` (the explicit kill_task API) where
/// the caller expects the process to be dead when the call returns.
///
/// Every `.await` is bounded by a timeout so the actor loop is never
/// blocked indefinitely.
async fn graceful_kill_and_wait(process: &mut ProcessState) {
    send_sigterm_to_group(process);

    // Wait up to SIGTERM_GRACE (1s) for graceful exit
    if tokio::time::timeout(SIGTERM_GRACE, process.child.wait())
        .await
        .is_ok()
    {
        drain_remaining_output(process).await;
        return; // exited cleanly
    }

    // Escalate to SIGKILL
    send_sigkill_to_group(process);

    // Wait for reap — bounded at 5s. SIGKILL is unconditional so this
    // almost always returns instantly. The cap protects against D-state
    // (uninterruptible kernel I/O, e.g. NFS hang). If it times out,
    // abandon — poll_process will pick it up later.
    const SIGKILL_REAP_TIMEOUT: Duration = Duration::from_secs(5);
    if tokio::time::timeout(SIGKILL_REAP_TIMEOUT, process.child.wait())
        .await
        .is_err()
    {
        tracing::warn!(
            pid = ?process.child.id(),
            "Process did not exit after SIGKILL within {:?}, \
             abandoning reap — poll loop will pick it up",
            SIGKILL_REAP_TIMEOUT
        );
    }

    drain_remaining_output(process).await;
}

#[tracing::instrument(
    name = "terminal.kill_and_finalize",
    skip_all,
    fields(pid = process.child.id())
)]
async fn kill_and_finalize(process: &mut ProcessState) -> KillOutcome {
    // Already reaped between the caller's check and here (race with poll_process)
    if process.lifecycle.has_exited() {
        return KillOutcome::AlreadyExited;
    }

    // Fast path: process already exited on its own.
    match process.child.try_wait() {
        Ok(Some(status)) => {
            drain_remaining_output(process).await;
            finalize_process(process, Some(status)).await;
            return KillOutcome::AlreadyExited;
        }
        Err(_) => {
            drain_remaining_output(process).await;
            finalize_process(process, None).await;
            return KillOutcome::AlreadyExited;
        }
        Ok(None) => {} // still running, proceed to kill
    }

    // Two-phase kill: SIGTERM → 1s grace → SIGKILL (bounded waits)
    graceful_kill_and_wait(process).await;

    // The child is reaped now, so drop the scope's reaping handle immediately
    // rather than waiting for the next poll sweep — closes the window where a
    // racing kill_all() could killpg the (now reused) pid. Guarded like the
    // sweep: if a D-state reap was abandoned (child.id() still Some), keep the
    // Arc so the poll loop can still reap it later.
    #[cfg(unix)]
    if process.child.id().is_none() {
        process.process_group = None;
    }

    // Abort the state dump reader so its spawn_blocking thread doesn't
    // leak (same rationale as kill_foreground_commands).
    if let Some(handle) = process.state_dump_handle.take() {
        handle.abort();
    }

    finalize_process(process, None).await;
    KillOutcome::Killed
}

/// Mark the task exited, flush the output file, and notify foreground
/// waiters. Callers read the remaining output first. A process that could
/// not be collected stays unsettled, so the poll loop keeps trying.
async fn finalize_process(process: &mut ProcessState, status: Option<std::process::ExitStatus>) {
    if process.lifecycle.has_exited() {
        return;
    }

    process.mark_exited(match status {
        Some(s) => extract_exit_status(s),
        None => ExitStatus {
            exit_code: None,
            signal: Some("killed".to_owned()),
        },
    });
    if process.end_wall_time.is_none() {
        process.end_wall_time = Some(std::time::SystemTime::now());
    }

    process.flush_and_truncate_output_file().await;
    process.finish_output(Collection::of(&process.child));

    let result = Ok(process.to_result());
    process.notify_waiters(result);
}

/// Open an output file for writing, creating parent directories if needed.
#[tracing::instrument(name = "fs.open_output_file", skip_all)]
async fn open_output_file(path: &std::path::Path) -> std::io::Result<File> {
    // Create parent directories if they don't exist
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .await
}

#[cfg(unix)]
const ENV_LOGIN_ENV: &str = "ECHO_AGENT_LOGIN_ENV";

#[cfg(unix)]
fn login_env_capture_enabled() -> bool {
    !matches!(
        std::env::var(ENV_LOGIN_ENV).as_deref(),
        Ok("0") | Ok("false")
    )
}

#[cfg(unix)]
fn login_env_var_excluded(key: &str) -> bool {
    matches!(
        key,
        "PWD"
            | "OLDPWD"
            | "SHLVL"
            | "_"
            | "TERM"
            | "ECHO_AGENT_AGENT"
            | "SUDO_ASKPASS"
            | "ECHO_AGENT_ASKPASS"
            | "ELECTRON_RUN_AS_NODE"
            | "SSH_AUTH_SOCK"
            | "DBUS_SESSION_BUS_ADDRESS"
            | "XDG_RUNTIME_DIR"
            | "WAYLAND_DISPLAY"
            | "GPG_TTY"
    ) || key.to_ascii_lowercase().ends_with("_proxy")
        || key.starts_with("ECHO_AGENT_SANDBOX")
}

#[cfg(unix)]
fn parse_login_env_capture(stdout: &str) -> (Option<String>, HashMap<String, String>) {
    let parts: Vec<&str> = stdout.split('\x01').collect();
    let login_path = parts
        .get(1)
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    let mut env_map = HashMap::new();
    if let Some(blob) = parts.get(2) {
        for pair in blob.split('\0') {
            if let Some((key, value)) = pair.split_once('=')
                && !key.is_empty()
                && key != "PATH"
                && !login_env_var_excluded(key)
            {
                env_map.insert(key.to_string(), value.to_string());
            }
        }
    }
    (login_path, env_map)
}

#[cfg(unix)]
async fn capture_login_env() -> HashMap<String, String> {
    use tokio::io::AsyncReadExt;

    let shell = shell_state::ShellKind::detect();
    let rc_file = shell.rc_file_name();

    // Use $HOME inside the script (not interpolated from Rust) to avoid
    // shell injection if HOME contains special characters.
    let script = format!(
        "source \"$HOME/{rc_file}\" 2>/dev/null; printf '\\x01%s\\x01' \"$PATH\"; command env -0 2>/dev/null; printf '\\x01'"
    );

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut cmd = tokio::process::Command::new(shell.binary_path());
        cmd.args(["-lc", &script])
            .stdin(echo_agent_tty_utils::null_stdio())
            .stdout(Stdio::piped())
            .stderr(echo_agent_tty_utils::null_stdio())
            .kill_on_drop(true);
        crate::util::detach_command(&mut cmd);
        cmd.envs(crate::util::pager_env());
        #[allow(clippy::disallowed_methods)] // probe killed on drop
        let mut child = cmd.spawn().ok()?;

        let mut stdout_buf = Vec::new();
        if let Some(ref mut stdout) = child.stdout {
            stdout.read_to_end(&mut stdout_buf).await.ok();
        }

        let status = child.wait().await.ok()?;
        if !status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&stdout_buf);
        let (login_path, mut env_map) = parse_login_env_capture(&stdout);
        let login_path = login_path?;

        if !login_env_capture_enabled() {
            env_map.clear();
        }

        // Merge: login PATH first, then current-process entries not already present.
        let current_path = std::env::var("PATH").unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let merged: Vec<&str> = login_path
            .split(':')
            .chain(current_path.split(':'))
            .filter(|e| !e.is_empty() && seen.insert(*e))
            .collect();
        env_map.insert("PATH".to_string(), merged.join(":"));

        Some(env_map)
    })
    .await;

    match result {
        Ok(Some(env_map)) => env_map,
        Ok(None) => HashMap::new(),
        Err(_) => {
            tracing::warn!("login-shell env capture timed out after 5s");
            HashMap::new()
        }
    }
}

/// Layer login-shell captured vars (except `PATH`) onto `cmd`, dropping those the
/// active policy filters out and those already set in echoagent's own environment.
#[cfg(unix)]
fn layer_login_env_vars(
    cmd: &mut tokio::process::Command,
    login_env: Option<&HashMap<String, String>>,
    active_policy: Option<&crate::util::ShellEnvironmentPolicy>,
) {
    if let Some(login) = login_env {
        for (key, value) in login {
            // `var_os` reads echoagent's own process env (not the possibly cleared
            // child env): a login var already present in echoagent's environment is
            // left alone. Capture is filtered through the policy so an rc export
            // cannot bypass it.
            if key != "PATH"
                && std::env::var_os(key).is_none()
                && active_policy.is_none_or(|p| p.allows_with_inherit(key))
            {
                cmd.env(key, value);
            }
        }
    }
}

/// Layer per-request env (`.envrc`, ACP, session settings) onto `cmd`, dropping
/// names the active policy excludes so a request-supplied secret cannot bypass
/// it. Honors `exclude`/`include_only`/default excludes, not `inherit`, since
/// request env is provided explicitly rather than inherited.
fn layer_request_env(
    cmd: &mut tokio::process::Command,
    env: &HashMap<String, String>,
    active_policy: Option<&crate::util::ShellEnvironmentPolicy>,
) {
    for (key, value) in env {
        if active_policy.is_none_or(|p| p.allows(key)) {
            cmd.env(key, value);
        }
    }
}

/// Re-inject the login-shell `PATH` last (so rc-file additions win), unless the
/// active policy filters `PATH` out.
#[cfg(unix)]
fn layer_login_path(
    cmd: &mut tokio::process::Command,
    login_env: Option<&HashMap<String, String>>,
    active_policy: Option<&crate::util::ShellEnvironmentPolicy>,
) {
    if let Some(path) = login_env.and_then(|l| l.get("PATH"))
        && active_policy.is_none_or(|p| p.allows_with_inherit("PATH"))
    {
        cmd.env("PATH", path);
    }
}

/// Compose the child environment on `cmd` in one place, in a fixed order:
/// policy base, login-shell capture, echoagent control vars, request env, pager
/// vars, login `PATH` last, then the agent marker. Untrusted layers (login
/// capture and request env) pass through the policy name filter so an excluded
/// name cannot re-enter; echoagent's own control vars, login `PATH`, and the marker
/// are applied unfiltered and last. `login_env` is `None` for the persistent
/// backend, which restores login state from its own snapshot.
///
/// Layers are applied incrementally rather than composed into one map and
/// installed via `env_clear`: the default policy is a no-op, and the common
/// path must inherit echoagent's environment untouched (including non-UTF-8 vars).
/// A base env is cleared and rebuilt only when a policy is active. Request env
/// is filtered by name only, so `inherit = none` still admits explicitly
/// provided `.envrc`/ACP vars.
///
/// Unix only: the Windows spawn path applies the policy inline (it has no
/// login-shell capture and uses the shell-invocation env instead of overrides).
#[cfg(unix)]
fn apply_child_env(
    cmd: &mut tokio::process::Command,
    policy: Option<&crate::util::ShellEnvironmentPolicy>,
    login_env: Option<&HashMap<String, String>>,
    request_env: &HashMap<String, String>,
) {
    let active_policy = policy.filter(|p| !p.is_noop());
    // 1. Base env: cleared and rebuilt from the policy only when one is active.
    crate::util::shell_env_policy::install_policy_base_env(cmd, active_policy);
    // 2. Login-shell capture (filtered). 3. EchoAgent control vars. 4. Request env
    // (filtered). 5. Pager vars. 6. Login PATH last. 7. Agent marker wins.
    layer_login_env_vars(cmd, login_env, active_policy);
    cmd.envs(shell_state::shell_env_overrides());
    layer_request_env(cmd, request_env, active_policy);
    cmd.envs(crate::util::pager_env());
    layer_login_path(cmd, login_env, active_policy);
    crate::util::apply_echo_agent_agent_marker(cmd);
}

/// Spawn the shell command and attach the child to a [`ProcessGroup`] for
/// grandchild teardown (`killpg` on Unix, `TerminateJobObject` on Windows).
fn spawn_shell_command(
    command: &str,
    cwd: &std::path::Path,
    env: &HashMap<String, String>,
    login_env: Option<&HashMap<String, String>>,
    search_shadows: SearchShadowConfig,
    shell_env_policy: Option<&crate::util::ShellEnvironmentPolicy>,
) -> std::io::Result<(tokio::process::Child, crate::util::ProcessGroup)> {
    // `login_env` and `search_shadows` are only consumed by the `#[cfg(unix)]`
    // shell wrapper below; keep them live on Windows to avoid unused-arg warnings.
    #[cfg(not(unix))]
    let _ = (&login_env, &search_shadows);
    #[cfg(unix)]
    let mut cmd = {
        let shell = shell_state::ShellKind::detect();
        let wrapped_command = {
            let inject = super::embedded_search_tools::search_injection(search_shadows);
            if inject.is_empty() {
                command.to_string()
            } else {
                format!("{inject}{command}")
            }
        };
        let mut cmd = tokio::process::Command::new(shell.binary_path());
        // Non-interactive zsh still defaults to NOMATCH; pass via argv like init's -o extendedglob.
        if matches!(shell, shell_state::ShellKind::Zsh) {
            cmd.arg("-o").arg("nonomatch");
        }
        cmd.arg("-c")
            .arg(&wrapped_command)
            .current_dir(cwd)
            .stdin(echo_agent_tty_utils::null_stdio())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // NOTE: do NOT set .process_group(0) here — std runs setpgid()
            // BEFORE pre_exec hooks, which would make the child a process
            // group leader and cause setsid() to fail with EPERM.
            // detach_from_tty() handles both session and process group creation.
            .kill_on_drop(true);

        apply_child_env(&mut cmd, shell_env_policy, login_env, env);

        // Detach from the controlling terminal so subprocesses cannot open
        // /dev/tty and compete with the TUI for terminal input.
        crate::util::detach_command(&mut cmd);

        // If the sandbox profile restricts network, install a seccomp BPF
        // filter on the child that blocks connect/bind/sendto/listen/accept.
        // The parent (echoagent) process retains network for the LLM API.
        // Filesystem restrictions are already inherited from the process-level
        // Landlock/Seatbelt sandbox — no action needed here for FS.
        #[cfg(target_os = "linux")]
        if echo_agent_sandbox::should_restrict_child_network() {
            unsafe {
                cmd.pre_exec(|| echo_agent_sandbox::child_net::install_child_network_filter());
            }
        }
        cmd
    };

    #[cfg(not(unix))]
    let mut build_cmd = |with_breakaway: bool| {
        use windows::Win32::System::Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
        };

        let inv = echo_agent_config::shell::shell_command_argv(command);
        let mut cmd = tokio::process::Command::new(&inv.program);
        cmd.args(&inv.args)
            .current_dir(cwd)
            .stdin(echo_agent_tty_utils::null_stdio())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Policy base first (cleared + rebuilt only when a policy is active), then
        // the shell-invocation env, the filtered request env, pager vars, and the
        // agent marker last. Mirrors the unix ordering in `apply_child_env`;
        // `inv.env` is echoagent's trusted shell setup, so it is not filtered.
        let active_policy = shell_env_policy.filter(|p| !p.is_noop());
        crate::util::shell_env_policy::install_policy_base_env(&mut cmd, active_policy);
        cmd.envs(inv.env);
        layer_request_env(&mut cmd, env, active_policy);
        cmd.envs(crate::util::pager_env());
        crate::util::apply_echo_agent_agent_marker(&mut cmd);

        // Set creation flags inline rather than via crate::util::detach_command
        // + new_process_group: tokio's creation_flags is a SET, not OR, so
        // the helpers don't compose.
        //   - CREATE_NO_WINDOW: no console window pops up.
        //   - CREATE_NEW_PROCESS_GROUP: child can receive its own Ctrl+Break.
        //   - CREATE_BREAKAWAY_FROM_JOB: lets the child escape any inherited
        //     job so we can assign it to our own ProcessGroup. Per Microsoft
        //     docs this *fails CreateProcess with ERROR_ACCESS_DENIED* (os
        //     error 5) when the parent process is in a job that does not
        //     have JOB_OBJECT_LIMIT_BREAKAWAY_OK set — common when echo-agent-agent
        //     is launched under a Windows service, scheduled task, or some
        //     ACP host wrappers. The caller below retries without this flag
        //     on os error 5.
        let mut flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
        if with_breakaway {
            flags |= CREATE_BREAKAWAY_FROM_JOB;
        }
        cmd.creation_flags(flags.0);
        cmd
    };

    #[cfg(unix)]
    let mut group = crate::util::ProcessGroup::new()?;
    #[cfg(unix)]
    #[allow(clippy::disallowed_methods)] // attached to the process group built above
    let child = cmd.spawn().map_err(|e| {
        std::io::Error::new(e.kind(), format!("spawn shell in {}: {e}", cwd.display()))
    })?;

    #[cfg(not(unix))]
    #[allow(clippy::disallowed_methods)] // attached to the process group built in this block
    let (child, mut group) = {
        let group = crate::util::ProcessGroup::new()?;
        let mut cmd = build_cmd(true);
        match cmd.spawn() {
            Ok(child) => (child, group),
            Err(e) if e.raw_os_error() == Some(5) => {
                // Parent's containing Job Object does not allow breakaway.
                // Retry without CREATE_BREAKAWAY_FROM_JOB. We lose the
                // ability to assign the child to our own job (so the
                // attach() below will also fail), but the command runs.
                // kill_on_drop + child.kill() still terminate the immediate
                // child via TerminateProcess.
                tracing::debug!(
                    "spawn with CREATE_BREAKAWAY_FROM_JOB returned ERROR_ACCESS_DENIED; \
                     retrying without breakaway (process-tree teardown disabled for this child)"
                );
                drop(cmd);
                let mut cmd = build_cmd(false);
                let child = cmd.spawn()?;
                (child, group)
            }
            Err(e) => return Err(e),
        }
    };

    if let Err(e) = group.attach(&child) {
        tracing::debug!("Failed to attach child to ProcessGroup: {e}");
    }
    Ok((child, group))
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer::types::TaskKind;
    use std::path::PathBuf;

    fn make_request(command: &str) -> TerminalRunRequest {
        // Use a unique temp file for each test
        let output_file = std::env::temp_dir().join(format!(
            "terminal-test-{}-{}.out",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        TerminalRunRequest {
            command: command.to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            output_byte_limit: 10000,
            output_file,
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        }
    }

    #[tokio::test]
    async fn run_background_preserves_description_on_snapshot() {
        let backend = LocalTerminalBackend::new();
        let mut with_desc = make_request("sleep 30");
        with_desc.description = Some("build frontend".to_string());
        let handle = backend.run_background(with_desc).await.unwrap();
        let snap = backend
            .get_task(&handle.task_id)
            .await
            .expect("running task snapshot");
        assert_eq!(snap.description.as_deref(), Some("build frontend"));
        let listed = backend.list_tasks().await;
        let listed_snap = listed
            .iter()
            .find(|t| t.task_id == handle.task_id)
            .expect("task listed");
        assert_eq!(listed_snap.description.as_deref(), Some("build frontend"));
        let _ = backend.kill_task(&handle.task_id).await;

        let without = make_request("sleep 30");
        let handle = backend.run_background(without).await.unwrap();
        let snap = backend
            .get_task(&handle.task_id)
            .await
            .expect("running task snapshot");
        assert!(
            snap.description.is_none(),
            "absent description must stay None"
        );
        let _ = backend.kill_task(&handle.task_id).await;
    }

    /// Poll `get_task` every 25ms until the task reports `completed`, returning
    /// `false` if `timeout` elapses first. Lets callers keep a bespoke assert
    /// message while sharing the poll-until-reaped boilerplate.
    async fn poll_until_task_completed(
        backend: &LocalTerminalBackend,
        task_id: &str,
        timeout: Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if backend
                .get_task(task_id)
                .await
                .map(|s| s.completed)
                .unwrap_or(false)
            {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[test]
    fn layer_request_env_drops_names_the_policy_excludes() {
        use crate::util::{EnvironmentVariablePattern, ShellEnvironmentPolicy};

        let glob = EnvironmentVariablePattern::new_case_insensitive;
        let policy = ShellEnvironmentPolicy {
            exclude: vec![glob("AWS_*")],
            include_only: vec![glob("PATH"), glob("SAFE_*")],
            ..Default::default()
        };
        let env = HashMap::from([
            ("PATH".to_string(), "/bin".to_string()),
            ("SAFE_FLAG".to_string(), "1".to_string()),
            ("AWS_SECRET".to_string(), "leak".to_string()),
            ("OTHER".to_string(), "x".to_string()),
        ]);

        let mut cmd = tokio::process::Command::new("true");
        layer_request_env(&mut cmd, &env, Some(&policy));
        let applied: HashMap<String, String> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();

        assert_eq!(applied.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(applied.get("SAFE_FLAG").map(String::as_str), Some("1"));
        assert!(!applied.contains_key("AWS_SECRET"));
        assert!(!applied.contains_key("OTHER"));
    }

    #[cfg(unix)]
    #[test]
    fn apply_child_env_layers_in_fixed_order() {
        use crate::util::{EnvironmentVariablePattern, ShellEnvironmentPolicy};

        let policy = ShellEnvironmentPolicy {
            exclude: vec![EnvironmentVariablePattern::new_case_insensitive("*SECRET*")],
            set: HashMap::from([("ECHO_AGENT_TEST_BASE".to_string(), "1".to_string())]),
            ..Default::default()
        };
        let login = HashMap::from([
            ("ECHO_AGENT_TEST_LOGIN".to_string(), "l".to_string()),
            ("PATH".to_string(), "/login/bin".to_string()),
        ]);
        let request = HashMap::from([
            ("ECHO_AGENT_TEST_REQ".to_string(), "r".to_string()),
            ("PATH".to_string(), "/req/bin".to_string()),
            ("ECHO_AGENT_TEST_SECRET".to_string(), "s".to_string()),
        ]);

        let mut cmd = tokio::process::Command::new("true");
        apply_child_env(&mut cmd, Some(&policy), Some(&login), &request);
        let env: HashMap<String, String> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();

        assert_eq!(
            env.get("ECHO_AGENT_TEST_BASE").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.get("ECHO_AGENT_TEST_LOGIN").map(String::as_str),
            Some("l")
        );
        assert_eq!(
            env.get("ECHO_AGENT_TEST_REQ").map(String::as_str),
            Some("r")
        );
        // Request env is filtered by the policy.
        assert!(!env.contains_key("ECHO_AGENT_TEST_SECRET"));
        // Login PATH is applied last and wins over the request PATH.
        assert_eq!(env.get("PATH").map(String::as_str), Some("/login/bin"));
        // The agent marker wins over every layer.
        assert_eq!(
            env.get(crate::util::ECHO_AGENT_AGENT_ENV)
                .map(String::as_str),
            Some(crate::util::ECHO_AGENT_AGENT_ENV_VALUE)
        );
    }

    #[tokio::test]
    #[ignore = "flaky: combined_output is sometimes empty in CI"]
    async fn test_simple_command() {
        let backend = LocalTerminalBackend::new();
        let request = make_request("echo hello");
        let output_file = request.output_file.clone();

        let result = backend.run(request).await.unwrap();

        assert_eq!(result.combined_output.trim(), "hello");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);

        // Verify output was written to file
        let file_content = tokio::fs::read_to_string(&output_file).await.unwrap();
        assert_eq!(file_content.trim(), "hello");

        // Cleanup
        let _ = tokio::fs::remove_file(&output_file).await;
    }

    #[tokio::test]
    async fn test_command_with_exit_code() {
        let backend = LocalTerminalBackend::new();
        let result = backend.run(make_request("exit 42")).await.unwrap();

        assert_eq!(result.exit_code, Some(42));
    }

    #[tokio::test]
    async fn test_timeout() {
        let backend = LocalTerminalBackend::new();

        let output_file =
            std::env::temp_dir().join(format!("terminal-test-timeout-{}.out", std::process::id()));

        let request = TerminalRunRequest {
            command: "sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_millis(200),
            output_byte_limit: 10000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-timeout".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();
        assert!(result.timed_out);

        // Cleanup
        let _ = tokio::fs::remove_file(&output_file).await;
    }

    #[tokio::test]
    async fn test_auto_background_on_timeout() {
        let backend = LocalTerminalBackend::new();

        let output_file =
            std::env::temp_dir().join(format!("terminal-test-auto-bg-{}.out", std::process::id()));
        let tool_call_id = "test-auto-bg";

        let request = TerminalRunRequest {
            command: "sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_millis(500),
            output_byte_limit: 10000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: tool_call_id.to_string(),
            display_command: None,
            auto_background_on_timeout: true,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();

        // Should NOT be marked timed_out — it was auto-backgrounded instead.
        assert!(
            !result.timed_out,
            "auto-backgrounded result must not be timed_out"
        );
        assert_eq!(
            result.signal.as_deref(),
            Some("auto_backgrounded"),
            "signal should be auto_backgrounded, got {:?}",
            result.signal
        );

        // The process should be accessible via get_task under the tool_call_id.
        let snapshot = backend
            .get_task(tool_call_id)
            .await
            .expect("auto-backgrounded task should be queryable by tool_call_id");
        assert!(
            !snapshot.completed,
            "auto-backgrounded process should still be running"
        );

        // Cleanup: kill the background task so the test doesn't leak.
        let outcome = backend.kill_task(tool_call_id).await;
        assert!(
            matches!(outcome, KillOutcome::Killed | KillOutcome::AlreadyExited),
            "kill after auto-bg should succeed: {outcome:?}"
        );
        let _ = tokio::fs::remove_file(&output_file).await;
    }

    // A mid-turn redirect backgrounds a running foreground command instead of killing it, and the blocking `run` returns a "backgrounded" signal.
    #[tokio::test]
    async fn test_background_foreground_commands_keeps_process_alive() {
        let backend = std::sync::Arc::new(LocalTerminalBackend::new());

        let output_file =
            std::env::temp_dir().join(format!("terminal-test-bg-all-{}.out", std::process::id()));
        let tool_call_id = "test-bg-all";

        let request = TerminalRunRequest {
            command: "sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(60),
            output_byte_limit: 10000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: tool_call_id.to_string(),
            display_command: None,
            // Not auto-backgroundable: this must be backgrounded on demand, not by a timeout, and must never be killed.
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        // Run the foreground command on a task; `run` blocks until the command is backgrounded (or finishes).
        let run_backend = backend.clone();
        let run = tokio::spawn(async move { run_backend.run(request).await });

        // Give the process time to spawn and register as a foreground process.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let backgrounded = backend.background_foreground_commands(None).await;

        assert_eq!(
            backgrounded.len(),
            1,
            "the running command was backgrounded"
        );
        assert_eq!(backgrounded[0].tool_call_id, tool_call_id);

        // The blocking run returns a backgrounded (not killed/cancelled) result.
        let result = run.await.unwrap().unwrap();
        assert_eq!(
            result.signal.as_deref(),
            Some("backgrounded"),
            "run must return a backgrounded signal, got {:?}",
            result.signal
        );

        // The process is still alive and queryable under its tool_call_id.
        let snapshot = backend
            .get_task(tool_call_id)
            .await
            .expect("backgrounded task should be queryable by tool_call_id");
        assert!(
            !snapshot.completed,
            "the backgrounded process must still be running, not killed"
        );

        // A second call is a no-op (nothing left in the foreground).
        assert!(
            backend
                .background_foreground_commands(None)
                .await
                .is_empty(),
            "no foreground commands remain after backgrounding"
        );

        let _ = backend.kill_task(tool_call_id).await;
        let _ = tokio::fs::remove_file(&output_file).await;
    }

    // Huge timeout (1h) + tiny budget: the budget, not the timeout, backgrounds
    // an auto-backgroundable command.
    #[tokio::test]
    async fn test_foreground_block_budget_backgrounds_before_timeout() {
        // Serialize flag-asserting tests; opt into the guards for this one.
        let backend = LocalTerminalBackend::new_with_foreground_budget(Duration::from_millis(300));

        let output_file = std::env::temp_dir().join(format!(
            "terminal-test-fg-budget-{}.out",
            std::process::id()
        ));
        let tool_call_id = "test-fg-budget";

        let request = TerminalRunRequest {
            command: "sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            // 1h timeout, far longer than the 300ms budget.
            timeout: Duration::from_secs(3600),
            output_byte_limit: 10000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: tool_call_id.to_string(),
            display_command: None,
            auto_background_on_timeout: true,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let start = Instant::now();
        let result = backend.run(request).await.unwrap();
        let elapsed = start.elapsed();

        // Backgrounded by the budget, not killed by the timeout.
        assert!(
            !result.timed_out,
            "budget-backgrounded result must not be timed_out"
        );
        assert_eq!(
            result.signal.as_deref(),
            Some("auto_backgrounded"),
            "signal should be auto_backgrounded, got {:?}",
            result.signal
        );
        // It returned on the budget (~300ms), nowhere near the 1h timeout.
        assert!(
            elapsed < Duration::from_secs(10),
            "should background on the budget, not block on the timeout (took {elapsed:?})"
        );

        // Still running in the background under the tool_call_id.
        let snapshot = backend
            .get_task(tool_call_id)
            .await
            .expect("budget-backgrounded task should be queryable by tool_call_id");
        assert!(
            !snapshot.completed,
            "budget-backgrounded process should still be running"
        );

        // Cleanup: kill the background task so the test doesn't leak.
        let outcome = backend.kill_task(tool_call_id).await;
        assert!(
            matches!(outcome, KillOutcome::Killed | KillOutcome::AlreadyExited),
            "kill after budget-bg should succeed: {outcome:?}"
        );
        let _ = tokio::fs::remove_file(&output_file).await;
    }

    // A non-backgroundable command must NOT be affected by the budget — it
    // keeps its requested `timeout` as the sole (kill) deadline.
    #[tokio::test]
    async fn test_foreground_block_budget_skips_non_backgroundable() {
        // Serialize flag-asserting tests; opt in so the "skip" is attributable
        // to the command being non-backgroundable, not to the master switch.
        let backend = LocalTerminalBackend::new_with_foreground_budget(Duration::from_millis(300));

        let output_file = std::env::temp_dir().join(format!(
            "terminal-test-fg-budget-skip-{}.out",
            std::process::id()
        ));

        let request = TerminalRunRequest {
            command: "sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            // Short timeout so the test is fast; auto_bg is OFF so the budget
            // must be ignored and the command killed on the timeout instead.
            timeout: Duration::from_millis(500),
            output_byte_limit: 10000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-fg-budget-skip".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();

        // Killed by the timeout — NOT backgrounded by the budget.
        assert!(
            result.timed_out,
            "non-backgroundable command should time out, not background"
        );
        assert_ne!(
            result.signal.as_deref(),
            Some("auto_backgrounded"),
            "non-backgroundable command must not be auto-backgrounded by the budget"
        );

        let _ = tokio::fs::remove_file(&output_file).await;
    }

    // Per-request FG budget overrides the backend default (and can disable the
    // short budget via Duration::MAX so only `timeout` auto-bgs).
    #[tokio::test]
    async fn test_per_request_foreground_block_budget_overrides_backend() {
        // Backend default is 10s — request overrides to 300ms.
        let backend = LocalTerminalBackend::new_with_foreground_budget(Duration::from_secs(10));

        let output_file = std::env::temp_dir().join(format!(
            "terminal-test-per-req-budget-{}.out",
            std::process::id()
        ));
        let tool_call_id = "test-per-req-budget";

        let request = TerminalRunRequest {
            command: "sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(3600),
            output_byte_limit: 10000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: tool_call_id.to_string(),
            display_command: None,
            auto_background_on_timeout: true,
            // Per-request 300ms budget wins over backend's 10s.
            foreground_block_budget: Some(Duration::from_millis(300)),
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let start = Instant::now();
        let result = backend.run(request).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            !result.timed_out,
            "per-request budget should auto-bg, not kill"
        );
        assert_eq!(result.signal.as_deref(), Some("auto_backgrounded"));
        assert!(
            elapsed < Duration::from_secs(5),
            "should fire on per-request 300ms budget, not backend 10s (took {elapsed:?})"
        );

        let outcome = backend.kill_task(tool_call_id).await;
        assert!(matches!(
            outcome,
            KillOutcome::Killed | KillOutcome::AlreadyExited
        ));
        let _ = tokio::fs::remove_file(&output_file).await;
    }

    // `foreground_block_budget: Some(Duration::MAX)` disables the short budget:
    // auto-bg only when the request timeout elapses.
    #[tokio::test]
    async fn test_duration_max_budget_waits_for_timeout_only() {
        let backend = LocalTerminalBackend::new_with_foreground_budget(Duration::from_millis(100));

        let output_file = std::env::temp_dir().join(format!(
            "terminal-test-max-budget-{}.out",
            std::process::id()
        ));
        let tool_call_id = "test-max-budget";

        let request = TerminalRunRequest {
            command: "sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            // ~800ms kill/auto-bg timeout — short budget is disabled.
            timeout: Duration::from_millis(800),
            output_byte_limit: 10000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: tool_call_id.to_string(),
            display_command: None,
            auto_background_on_timeout: true,
            foreground_block_budget: Some(Duration::MAX),
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let start = Instant::now();
        let result = backend.run(request).await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(result.signal.as_deref(), Some("auto_backgrounded"));
        // Must wait past the short backend budget (100ms) and near the timeout.
        assert!(
            elapsed >= Duration::from_millis(500),
            "Duration::MAX budget must not short-circuit at 100ms backend default (took {elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "should auto-bg around the 800ms timeout (took {elapsed:?})"
        );

        let outcome = backend.kill_task(tool_call_id).await;
        assert!(matches!(
            outcome,
            KillOutcome::Killed | KillOutcome::AlreadyExited
        ));
        let _ = tokio::fs::remove_file(&output_file).await;
    }

    // Runaway-output guard: a stdout flood is killed once the output file
    // passes the size cap, independent of its timeout.
    #[tokio::test]
    async fn test_output_size_guard_kills_runaway() {
        // Serialize flag-asserting tests; opt into the guards for this one.
        // Tiny cap so `yes` trips it within a tick or two.
        let backend = LocalTerminalBackend::new_with_output_cap(2_000);

        let output_file =
            std::env::temp_dir().join(format!("terminal-test-size-{}.out", std::process::id()));

        let request = TerminalRunRequest {
            command: "yes".to_string(), // floods stdout forever
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            // Long timeout: the SIZE guard, not the timeout, must fire.
            timeout: Duration::from_secs(30),
            output_byte_limit: 10_000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-size".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();

        assert_eq!(
            result.signal.as_deref(),
            Some("output_limit"),
            "runaway output should be killed by the size guard, got {:?}",
            result.signal
        );
        assert!(!result.timed_out, "size kill is not a timeout");

        let _ = tokio::fs::remove_file(&output_file).await;
    }

    #[tokio::test]
    async fn test_stderr_captured() {
        let backend = LocalTerminalBackend::new();
        let result = backend.run(make_request("echo error >&2")).await.unwrap();

        assert!(result.combined_output.contains("error"));
        assert_eq!(result.exit_code, Some(0));
    }

    #[tokio::test]
    #[ignore = "flaky: combined_output is sometimes empty in CI"]
    async fn test_multiple_commands() {
        let backend = LocalTerminalBackend::new();

        let result1 = backend.run(make_request("echo first")).await.unwrap();
        let result2 = backend.run(make_request("echo second")).await.unwrap();

        assert_eq!(result1.combined_output.trim(), "first");
        assert_eq!(result2.combined_output.trim(), "second");
    }

    #[tokio::test]
    #[ignore = "flaky: output file content sometimes not flushed in CI"]
    async fn test_output_file_written() {
        let backend = LocalTerminalBackend::new();

        let output_file =
            std::env::temp_dir().join(format!("terminal-test-file-{}.out", std::process::id()));

        let request = TerminalRunRequest {
            command: "echo 'line1'; echo 'line2'; echo 'line3'".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            output_byte_limit: 10000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-output-file".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.exit_code, Some(0));

        // Verify file contains all output
        let file_content = tokio::fs::read_to_string(&output_file).await.unwrap();
        assert!(file_content.contains("line1"));
        assert!(file_content.contains("line2"));
        assert!(file_content.contains("line3"));

        // Cleanup
        let _ = tokio::fs::remove_file(&output_file).await;
    }

    #[tokio::test]
    async fn test_run_background_and_get_task() {
        let backend = LocalTerminalBackend::new();

        let output_file =
            std::env::temp_dir().join(format!("terminal-test-bg-{}.out", std::process::id()));

        let request = TerminalRunRequest {
            command: "echo background_test && sleep 0.1".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            output_byte_limit: 10000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-bg".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        // Start background task
        let handle = backend.run_background(request).await.unwrap();
        assert!(!handle.task_id.is_empty());

        // Wait for completion
        let snapshot = backend
            .wait_for_completion(&handle.task_id, Some(Duration::from_secs(5)))
            .await;
        assert!(snapshot.is_some());
        let snapshot = snapshot.unwrap();
        assert!(snapshot.completed);
        assert_eq!(snapshot.exit_code, Some(0));

        // Cleanup
        let _ = tokio::fs::remove_file(&output_file).await;
    }

    #[tokio::test]
    async fn test_kill_background_task() {
        let backend = LocalTerminalBackend::new();

        let output_file =
            std::env::temp_dir().join(format!("terminal-test-kill-{}.out", std::process::id()));

        let request = TerminalRunRequest {
            command: "sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(300),
            output_byte_limit: 10000,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-kill".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let handle = backend.run_background(request).await.unwrap();

        // Kill it
        let outcome = backend.kill_task(&handle.task_id).await;
        assert_eq!(outcome, KillOutcome::Killed);

        // Cleanup
        let _ = tokio::fs::remove_file(&output_file).await;
    }

    // -----------------------------------------------------------------------
    // BashOutputChunk streaming tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn chunk_notifications_sent_during_execution() {
        // Create a real notification channel (not noop)
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = ToolNotificationHandle::from_sender(tx);

        let backend = LocalTerminalBackend::new();
        let tmp = tempfile::TempDir::new().unwrap();

        let request = TerminalRunRequest {
            // Command that produces output over time (not all at once)
            command: "for i in 1 2 3; do echo chunk_$i; sleep 0.15; done".to_string(),
            working_directory: tmp.path().to_path_buf(),
            env: HashMap::new(),
            timeout: Duration::from_secs(5),
            output_byte_limit: 1024 * 1024,
            output_file: tmp.path().join("output.log"),
            notification_handle: handle,
            tool_call_id: "test-call-123".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.exit_code, Some(0));

        // Drain the notification channel
        let mut chunks = vec![];
        while let Ok(notification) = rx.try_recv() {
            if let crate::notification::types::ToolNotification::BashOutputChunk(chunk) =
                notification
            {
                chunks.push(chunk);
            }
        }

        // Should have received at least 2 chunks: the initial empty notification
        // plus at least one with output (command runs ~450ms with 100ms ticks)
        assert!(
            chunks.len() >= 2,
            "Expected at least 2 BashOutputChunk notifications (initial + output), got {}",
            chunks.len()
        );

        // The first chunk is the initial empty notification (sent before any output)
        let initial = &chunks[0];
        assert_eq!(initial.base.tool_call_id, "test-call-123");
        assert!(!initial.base.command.is_empty());
        assert!(
            initial.base.output.is_empty(),
            "Initial chunk should have empty output"
        );

        // Subsequent chunks should have output
        let first_with_output = &chunks[1];
        assert!(!first_with_output.base.output.is_empty());

        // Later chunks should have more output than earlier ones
        assert!(
            chunks.last().unwrap().base.output.len() >= first_with_output.base.output.len(),
            "Output should accumulate across chunks"
        );

        // The final output should contain all chunks
        assert!(result.combined_output.contains("chunk_1"));
        assert!(result.combined_output.contains("chunk_3"));
    }

    #[tokio::test]
    async fn chunk_notifications_keep_flowing_after_truncation() {
        // Regression guard for the emission gate fix: the gate is keyed off the
        // monotonic `total_bytes`, not `output_buffer.len()`. Before the fix the
        // length-based gate went false once `maybe_truncate` started shrinking
        // the tail, starving the stream of chunks for the rest of a long command.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = ToolNotificationHandle::from_sender(tx);

        let backend = LocalTerminalBackend::new();
        let tmp = tempfile::TempDir::new().unwrap();

        let request = TerminalRunRequest {
            // ~1.8 KB of ASCII over ~1.8s; far exceeds the 200-char limit so
            // truncation fires early and keeps firing on the shrinking tail.
            command: "for i in $(seq 1 60); do printf 'LINE%03d-XXXXXXXXXXXXXXXXXXXX\\n' \"$i\"; sleep 0.03; done".to_string(),
            working_directory: tmp.path().to_path_buf(),
            env: HashMap::new(),
            timeout: Duration::from_secs(10),
            output_byte_limit: 200,
            output_file: tmp.path().join("output.log"),
            notification_handle: handle,
            tool_call_id: "trunc-call".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(result.truncated, "expected output to be truncated");

        let mut chunks = vec![];
        while let Ok(notification) = rx.try_recv() {
            if let crate::notification::types::ToolNotification::BashOutputChunk(chunk) =
                notification
            {
                chunks.push(chunk);
            }
        }

        // total_bytes is monotonically non-decreasing across all chunks.
        for w in chunks.windows(2) {
            assert!(
                w[1].base.total_bytes >= w[0].base.total_bytes,
                "total_bytes regressed: {} < {}",
                w[1].base.total_bytes,
                w[0].base.total_bytes
            );
        }

        // Truncation must surface, and chunks must keep arriving afterwards.
        let first_truncated = chunks
            .iter()
            .position(|c| c.base.truncated)
            .expect("expected at least one truncated chunk past the byte limit");
        assert!(
            first_truncated < chunks.len() - 1,
            "chunks stopped after truncation (first_truncated={first_truncated}, total={})",
            chunks.len()
        );
        // And the buffer length oscillates around the limit while total_bytes
        // keeps climbing — proving the gate is total-based, not length-based.
        assert!(
            chunks.last().unwrap().base.total_bytes > 200,
            "expected total_bytes to exceed the byte limit"
        );
    }

    /// Size guard must kill runaway writers and bound the output file.
    #[tokio::test]
    async fn output_file_capped_by_size_guard() {
        let cap: u64 = 5000;
        let output_amount = cap * 1000;
        let backend = LocalTerminalBackend::new_with_output_cap(cap);
        let tmp = tempfile::TempDir::new().unwrap();
        let output_file = tmp.path().join("output.log");

        let request = TerminalRunRequest {
            command: format!("head -c {output_amount} /dev/zero | tr '\\0' 'x'"),
            working_directory: tmp.path().to_path_buf(),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            output_byte_limit: 1024,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "cap-test".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.signal.as_deref(), Some("output_limit"));

        let file_size = tokio::fs::metadata(&output_file).await.unwrap().len();
        // Guard fires on 100ms tick — some overshoot expected (up to ~512 KB
        // on arm64 CI). Without the guard the file would be `output_amount`.
        assert!(
            file_size < output_amount / 2,
            "output file should be bounded by size guard, got {file_size} bytes (cap={cap})"
        );
    }

    /// Verify retention cap constant and that small output is not truncated.
    #[tokio::test]
    async fn output_file_truncated_after_exit() {
        let backend = LocalTerminalBackend::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let output_file = tmp.path().join("output.log");

        let request = TerminalRunRequest {
            command: "head -c 200000 /dev/zero | tr '\\0' 'x'".to_string(),
            working_directory: tmp.path().to_path_buf(),
            env: HashMap::new(),
            timeout: Duration::from_secs(10),
            output_byte_limit: 1024,
            output_file: output_file.clone(),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "trunc-exit-test".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.exit_code, Some(0));

        // Output is under 64 MB, so file should have full content (no truncation).
        let file_size = tokio::fs::metadata(&output_file).await.unwrap().len();
        assert!(
            file_size >= 190_000,
            "output file should have full ~200 KB, got {file_size}"
        );
        // Verify the retention cap constant is reasonable.
        assert_eq!(MAX_RETAINED_OUTPUT_FILE_BYTES, 64 * 1024 * 1024);
    }

    #[tokio::test]
    async fn no_chunks_when_noop_handle() {
        let backend = LocalTerminalBackend::new();
        let tmp = tempfile::TempDir::new().unwrap();

        let request = TerminalRunRequest {
            command: "echo hello".to_string(),
            working_directory: tmp.path().to_path_buf(),
            env: HashMap::new(),
            timeout: Duration::from_secs(5),
            output_byte_limit: 1024 * 1024,
            output_file: tmp.path().join("output.log"),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-call".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        // noop() drops the receiver — send() is a silent no-op.
        // This confirms no panic/crash when nobody is listening.
    }

    #[tokio::test]
    async fn chunk_tool_call_id_matches_request() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = ToolNotificationHandle::from_sender(tx);

        let backend = LocalTerminalBackend::new();
        let tmp = tempfile::TempDir::new().unwrap();

        let request = TerminalRunRequest {
            command: "echo test; sleep 0.15; echo done".to_string(),
            working_directory: tmp.path().to_path_buf(),
            env: HashMap::new(),
            timeout: Duration::from_secs(5),
            output_byte_limit: 1024 * 1024,
            output_file: tmp.path().join("output.log"),
            notification_handle: handle,
            tool_call_id: "unique-id-abc".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        backend.run(request).await.unwrap();

        while let Ok(notification) = rx.try_recv() {
            if let crate::notification::types::ToolNotification::BashOutputChunk(chunk) =
                notification
            {
                assert_eq!(
                    chunk.base.tool_call_id, "unique-id-abc",
                    "Every chunk must carry the request's tool_call_id"
                );
            }
        }
    }

    #[tokio::test]
    async fn no_chunk_sent_when_no_new_output() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = ToolNotificationHandle::from_sender(tx);

        let backend = LocalTerminalBackend::new();
        let tmp = tempfile::TempDir::new().unwrap();

        // Command that outputs once then sleeps (3 idle ticks with no new output)
        let request = TerminalRunRequest {
            command: "echo once; sleep 0.5".to_string(),
            working_directory: tmp.path().to_path_buf(),
            env: HashMap::new(),
            timeout: Duration::from_secs(5),
            output_byte_limit: 1024 * 1024,
            output_file: tmp.path().join("output.log"),
            notification_handle: handle,
            tool_call_id: "test-idle".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        backend.run(request).await.unwrap();

        let mut chunks = vec![];
        while let Ok(notification) = rx.try_recv() {
            if let crate::notification::types::ToolNotification::BashOutputChunk(chunk) =
                notification
            {
                chunks.push(chunk);
            }
        }

        // Should have chunks, but NOT one per tick — idle ticks should be skipped.
        // The command outputs "once\n" early then sleeps 500ms (5 ticks).
        // We should see 1-2 chunks, not 5.
        assert!(
            chunks.len() <= 3,
            "Expected at most 3 chunks (not one per idle tick), got {}",
            chunks.len()
        );
    }

    // -----------------------------------------------------------------------
    // CS2: Graceful kill tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_timeout_uses_sigterm_then_sigkill() {
        // Run a command with a short timeout. Verify the result has timed_out == true
        // and the process is dead.
        let backend = LocalTerminalBackend::new();
        let tmp = tempfile::TempDir::new().unwrap();

        let request = TerminalRunRequest {
            command: "sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_millis(200),
            output_byte_limit: 10000,
            output_file: tmp.path().join("timeout-test.out"),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-timeout-graceful".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();
        assert!(result.timed_out);
        assert_eq!(result.signal.as_deref(), Some("timeout"));
    }

    // -----------------------------------------------------------------------
    // CS3: Output drain tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[ignore = "flaky: output buffer sometimes not flushed before kill completes"]
    async fn test_output_preserved_on_kill() {
        // Spawn a command that produces output then sleeps.
        // Kill it and verify the output before the kill is preserved.
        let backend = LocalTerminalBackend::new();
        let tmp = tempfile::TempDir::new().unwrap();

        let request = TerminalRunRequest {
            command: "echo before_kill; sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            output_byte_limit: 10000,
            output_file: tmp.path().join("kill-output.out"),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-kill-output".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let handle = backend.run_background(request).await.unwrap();

        // Give it time to produce output
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Kill it
        let outcome = backend.kill_task(&handle.task_id).await;
        assert_eq!(outcome, KillOutcome::Killed);

        // Check the snapshot has the output
        let snapshot = backend.get_task(&handle.task_id).await;
        if let Some(snapshot) = snapshot {
            assert!(
                snapshot.output.contains("before_kill"),
                "Output should contain 'before_kill', got: {:?}",
                snapshot.output
            );
        }
    }

    #[tokio::test]
    async fn test_output_preserved_on_timeout() {
        // Verify output captured before timeout is preserved in the result.
        // Uses 2s timeout (not 500ms) so the poll loop has enough ticks to
        // read the echo before the timeout handler snapshots the buffer.
        let backend = LocalTerminalBackend::new();
        let tmp = tempfile::TempDir::new().unwrap();

        let request = TerminalRunRequest {
            command: "echo before_timeout; sleep 60".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(2),
            output_byte_limit: 10000,
            output_file: tmp.path().join("timeout-output.out"),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-timeout-output".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let result = backend.run(request).await.unwrap();
        assert!(result.timed_out);
        assert!(
            result.combined_output.contains("before_timeout"),
            "Timed-out output should contain 'before_timeout', got: {:?}",
            result.combined_output
        );
    }

    #[tokio::test]
    async fn test_background_child_with_inherited_pipe_does_not_block() {
        // `sleep 300 &` inherits the pipe — without drain timeout this blocks forever.
        let backend = LocalTerminalBackend::new();
        let request = TerminalRunRequest {
            command: "sleep 300 &\nsleep 1\necho done".to_string(),
            working_directory: PathBuf::from("/tmp"),
            env: HashMap::new(),
            timeout: Duration::from_secs(30),
            output_byte_limit: 10000,
            output_file: std::env::temp_dir().join(format!(
                "terminal-test-drain-timeout-{}.out",
                std::process::id()
            )),
            notification_handle: ToolNotificationHandle::noop(),
            tool_call_id: "test-drain-timeout".to_string(),
            display_command: None,
            auto_background_on_timeout: false,
            foreground_block_budget: None,
            kind: TaskKind::Bash,
            owner_session_id: None,
            description: None,
        };

        let start = Instant::now();
        let result = backend.run(request).await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "Command should complete in ~3s (1s sleep + 2s drain timeout), got {:?}",
            elapsed
        );
        assert_eq!(result.exit_code, Some(0));
        assert!(
            result.combined_output.contains("done"),
            "Output should contain 'done', got: {:?}",
            result.combined_output
        );
    }

    #[tokio::test]
    async fn test_drain_timeout_constant() {
        assert_eq!(DRAIN_TIMEOUT, Duration::from_secs(2));
    }

    // -----------------------------------------------------------------------
    // CS4: Background guardrails tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_sigterm_grace_is_one_second() {
        // Static assertion: guard against someone changing the constant
        assert_eq!(SIGTERM_GRACE, Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_background_max_runtime_constant() {
        // Static assertion: guard against accidental changes
        assert_eq!(BACKGROUND_MAX_RUNTIME, Duration::from_secs(36_000));
    }

    // -----------------------------------------------------------------------
    // CS1: Process group tests (pre-existing)
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[ignore = "flaky: pgrep sees sleep processes from other sandbox co-tenants"]
    async fn test_kill_kills_child_processes() {
        // Spawn a background command that creates multiple child processes.
        // Kill it, then verify no sleep processes remain.
        let backend = LocalTerminalBackend::new();
        let request = make_request("bash -c 'sleep 60 & sleep 60 & wait'");

        // Run in background
        let handle = backend.run_background(request).await.unwrap();

        // Give it a moment to spawn children
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Kill the background task
        let _ = backend.kill_task(&handle.task_id).await;

        // Wait for processes to be reaped
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify no sleep processes remain owned by this test process
        #[cfg(unix)]
        {
            let pgrep = std::process::Command::new("pgrep")
                .arg("-P")
                .arg(std::process::id().to_string())
                .arg("sleep")
                .output()
                .expect("pgrep should run");

            // No sleep processes should be found
            assert!(
                pgrep.stdout.is_empty(),
                "Expected no sleep processes to remain, but pgrep found: {:?}",
                String::from_utf8_lossy(&pgrep.stdout)
            );
        }
    }

    #[test]
    fn new_local_runs_on_current_thread_runtime() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let backend = LocalTerminalBackend::new_local(SearchShadowConfig::default());
            let result = backend
                .run(make_request("echo hello"))
                .await
                .expect("command should succeed");
            assert_eq!(result.exit_code, Some(0));
            assert!(result.combined_output.contains("hello"));
        });
    }

    #[test]
    fn new_local_sequential_commands_dont_stall() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let backend = LocalTerminalBackend::new_local(SearchShadowConfig::default());
            for i in 0..5 {
                let result = backend
                    .run(make_request(&format!("echo run_{i}")))
                    .await
                    .expect("command should succeed");
                assert_eq!(result.exit_code, Some(0));
                assert!(result.combined_output.contains(&format!("run_{i}")));
            }
        });
    }

    #[test]
    fn new_local_background_task_lifecycle() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let backend = LocalTerminalBackend::new_local(SearchShadowConfig::default());

            let mut bg_req = make_request("sleep 60");
            bg_req.tool_call_id = "bg-1".to_string();
            let bg = backend
                .run_background(bg_req)
                .await
                .expect("background spawn should succeed");

            let snap = backend
                .get_task(&bg.task_id)
                .await
                .expect("task should exist");
            assert!(!snap.completed);

            let outcome = backend.kill_task(&bg.task_id).await;
            assert!(
                matches!(outcome, KillOutcome::Killed | KillOutcome::AlreadyExited),
                "kill should succeed: {outcome:?}"
            );
        });
    }

    /// A background command must be reaped by the `ProcessScope` the backend
    /// enrolled it into -- the same handle the TUI exit paths `kill_all()` on
    /// process exit. The kill is driven through the scope (not the actor's
    /// `kill_task`), so this fails if the spawn site stops enrolling children.
    /// An injected scope keeps the process-global one un-latched for other tests.
    #[test]
    fn background_child_is_reaped_via_process_scope() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let scope = crate::util::ProcessScope::new();
            let backend = LocalTerminalBackend::new_local_with_scope(
                SearchShadowConfig::default(),
                scope.clone(),
                None,
            );

            let mut bg_req = make_request("sleep 120");
            bg_req.tool_call_id = "bg-scope-1".to_string();
            let bg = backend
                .run_background(bg_req)
                .await
                .expect("background spawn should succeed");

            assert!(
                !backend
                    .get_task(&bg.task_id)
                    .await
                    .expect("task should exist")
                    .completed,
                "task should be running before kill_all"
            );

            // Reap via the scope, exactly like the TUI exit handlers do.
            scope.kill_all();

            // The actor observes the externally-killed child and marks the task
            // complete. If enrollment regressed, the `sleep 120` would outlive
            // the scope kill and this would time out.
            assert!(
                poll_until_task_completed(&backend, &bg.task_id, Duration::from_secs(10)).await,
                "kill_all did not reap the enrolled background child"
            );
        });
    }

    /// A session-scoped command stays enrolled in the base scope too, so the TUI
    /// exit paths (which `kill_all()` only the process-global scope, and reach
    /// `process::exit` without running `Drop`) still reap it.
    #[test]
    fn session_scoped_child_is_still_reaped_via_base_scope() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let base = crate::util::ProcessScope::new();
            let session = crate::util::ProcessScope::new();
            let backend = LocalTerminalBackend::new_local_with_scope(
                SearchShadowConfig::default(),
                base.clone(),
                Some(session),
            );

            let mut bg_req = make_request("sleep 120");
            bg_req.tool_call_id = "bg-dual-scope".to_string();
            let bg = backend
                .run_background(bg_req)
                .await
                .expect("background spawn should succeed");

            base.kill_all();

            assert!(
                poll_until_task_completed(&backend, &bg.task_id, Duration::from_secs(10)).await,
                "base-scope kill_all did not reap a session-scoped child"
            );
        });
    }

    /// Once a background child is reaped, the actor must drop its
    /// `Arc<ProcessGroup>` so the scope's `Weak` dies. The completed task lingers
    /// in `self.processes` for `COMPLETED_TASK_TTL`; if the actor kept the `Arc`
    /// that long, a `kill_all()` on exit could `killpg` a pid the OS recycled.
    /// Asserts the injected scope's live-group count goes 1 -> 0 across the reap.
    #[test]
    fn reaped_background_child_leaves_scope_empty() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let scope = crate::util::ProcessScope::new();
            let backend = LocalTerminalBackend::new_local_with_scope(
                SearchShadowConfig::default(),
                scope.clone(),
                None,
            );

            // A brief sleep (not `true`): it must still be running when we read
            // live_count below so we can observe the `1` end of the transition.
            // The actor's first poll tick fires right after spawn, and `true`
            // could already be reaped by then, making the `== 1` check racy.
            let mut bg_req = make_request("sleep 1");
            bg_req.tool_call_id = "bg-reap-1".to_string();
            let bg = backend
                .run_background(bg_req)
                .await
                .expect("background spawn should succeed");

            // Enrollment is synchronous with the reply and the still-running
            // child keeps the Arc alive, so exactly one group is live here.
            assert_eq!(
                scope.live_count(),
                1,
                "spawn must enroll exactly one live group"
            );

            // Drive the actor until it observes the exit and reaps the child.
            assert!(
                poll_until_task_completed(&backend, &bg.task_id, Duration::from_secs(10)).await,
                "background `sleep 1` was never reaped"
            );

            // The reap sweep runs in the same poll tick that sets exit_status, so
            // by the time get_task reports completed the Arc is already dropped —
            // kill_all() would be a no-op and can't killpg a reused pid.
            assert_eq!(
                scope.live_count(),
                0,
                "a reaped child must leave no live group enrolled in the scope"
            );
        });
    }

    // ================================================================
    // Persistent shell tests
    // ================================================================

    #[tokio::test]
    async fn test_persistent_shell_cd_persists() {
        let backend = LocalTerminalBackend::with_persistent_shell();

        // cd to /tmp
        let result = backend.run(make_request("cd /tmp")).await.unwrap();
        assert_eq!(result.exit_code, Some(0));

        // pwd should return /tmp (macOS resolves to /private/tmp)
        let result = backend.run(make_request("pwd")).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        let pwd = result.combined_output.trim();
        assert!(
            pwd == "/tmp" || pwd == "/private/tmp",
            "cwd should persist across commands, got: {pwd}"
        );
    }

    #[tokio::test]
    async fn test_persistent_shell_env_var_persists() {
        let backend = LocalTerminalBackend::with_persistent_shell();

        let result = backend
            .run(make_request("export ECHO_AGENT_PERSIST_TEST=hello123"))
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));

        let result = backend
            .run(make_request("echo $ECHO_AGENT_PERSIST_TEST"))
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(
            result.combined_output.trim(),
            "hello123",
            "env var should persist across commands"
        );
    }

    #[tokio::test]
    async fn test_persistent_shell_clears_gpg_tty() {
        // GPG_TTY must be forced empty on the live path even when supplied via the request env.
        let backend = LocalTerminalBackend::with_persistent_shell();

        let mut req = make_request("echo \"[$GPG_TTY]\"");
        req.env.insert(
            "GPG_TTY".to_string(),
            "/echo-agent-sentinel-tty".to_string(),
        );

        let result = backend.run(req).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(
            result.combined_output.trim(),
            "[]",
            "GPG_TTY must be empty on the live tool path, got: {:?}",
            result.combined_output
        );
    }

    #[tokio::test]
    async fn test_persistent_shell_function_persists() {
        let backend = LocalTerminalBackend::with_persistent_shell();

        let result = backend
            .run(make_request("myfunc() { echo \"called with $1\"; }"))
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));

        let result = backend.run(make_request("myfunc test_arg")).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(
            result.combined_output.trim(),
            "called with test_arg",
            "function should persist across commands"
        );
    }

    #[tokio::test]
    async fn test_persistent_shell_variable_capture() {
        let backend = LocalTerminalBackend::with_persistent_shell();

        // Set a variable via command substitution
        let result = backend
            .run(make_request("export CAPTURED=$(echo captured_value)"))
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));

        // Read it back
        let result = backend.run(make_request("echo $CAPTURED")).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(
            result.combined_output.trim(),
            "captured_value",
            "variable from command substitution should persist"
        );
    }

    #[tokio::test]
    async fn test_persistent_shell_deleted_cwd_falls_back_to_request_cwd() {
        let backend = LocalTerminalBackend::with_persistent_shell();

        let scratch = tempfile::TempDir::new().unwrap();
        let result = backend
            .run(make_request(&format!("cd {}", scratch.path().display())))
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));
        drop(scratch);

        let result = backend.run(make_request("pwd")).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        let output = &result.combined_output;
        assert!(
            output.contains("no longer exists"),
            "fallback warning must be in the command output, got: {output:?}"
        );
        let pwd = output.lines().last().unwrap_or_default().trim();
        assert!(
            pwd == "/tmp" || pwd == "/private/tmp",
            "command must run in the request working directory, got: {pwd:?}"
        );

        let result = backend.run(make_request("pwd")).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(
            !result.combined_output.contains("no longer exists"),
            "state must heal after the fallback, got: {:?}",
            result.combined_output
        );
    }

    #[tokio::test]
    async fn test_persistent_shell_spawn_error_names_missing_cwd() {
        let backend = LocalTerminalBackend::with_persistent_shell();

        let scratch = tempfile::TempDir::new().unwrap();
        let result = backend
            .run(make_request(&format!("cd {}", scratch.path().display())))
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));
        drop(scratch);

        let gone = tempfile::TempDir::new().unwrap();
        let gone_path = gone.path().to_path_buf();
        drop(gone);
        let mut req = make_request("pwd");
        req.working_directory = gone_path.clone();

        let Err(err) = backend.run(req).await else {
            panic!("spawn must fail when both directories are missing");
        };
        let msg = err.to_string();
        assert!(
            msg.contains("spawn shell in") && msg.contains(&gone_path.display().to_string()),
            "error must name the spawn directory, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_persistent_shell_does_not_inherit_dump_errexit() {
        let backend = LocalTerminalBackend::with_persistent_shell();

        let result = backend.run(make_request("true")).await.unwrap();
        assert_eq!(result.exit_code, Some(0));

        let result = backend
            .run(make_request("false; echo STILL_ALIVE"))
            .await
            .unwrap();
        assert_eq!(
            result.exit_code,
            Some(0),
            "a failing statement must not abort the command: {:?}",
            result.combined_output
        );
        assert!(
            result.combined_output.contains("STILL_ALIVE"),
            "execution must continue past a failing statement: {:?}",
            result.combined_output
        );
    }

    #[tokio::test]
    async fn test_non_persistent_shell_unaffected_by_deleted_cd_target() {
        let backend = LocalTerminalBackend::new();

        let scratch = tempfile::TempDir::new().unwrap();
        let result = backend
            .run(make_request(&format!("cd {}", scratch.path().display())))
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));
        drop(scratch);

        let result = backend.run(make_request("pwd")).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        let pwd = result.combined_output.trim();
        assert!(
            pwd == "/tmp" || pwd == "/private/tmp",
            "spawns must use the request cwd, got: {pwd:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_parse_login_env_capture() {
        let stdout = "motd noise\n\x01/opt/rc/bin:/usr/bin\x01\
                      XDG_CONFIG_HOME=/Users/u/.config\0\
                      GH_CONFIG_DIR=/Users/u/.config/gh\0\
                      MULTILINE=a\nb\0\
                      PATH=/login/path\0\
                      PWD=/somewhere\0\
                      SHLVL=2\0\
                      GPG_TTY=/dev/ttys001\0\
                      http_proxy=http://p:3128\0\x01";
        let (path, env) = parse_login_env_capture(stdout);
        assert_eq!(path.as_deref(), Some("/opt/rc/bin:/usr/bin"));
        assert_eq!(
            env.get("XDG_CONFIG_HOME").map(String::as_str),
            Some("/Users/u/.config")
        );
        assert_eq!(
            env.get("GH_CONFIG_DIR").map(String::as_str),
            Some("/Users/u/.config/gh")
        );
        assert_eq!(env.get("MULTILINE").map(String::as_str), Some("a\nb"));
        for excluded in ["PATH", "PWD", "SHLVL", "GPG_TTY", "http_proxy"] {
            assert!(
                !env.contains_key(excluded),
                "{excluded} must be filtered from the captured login env"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_parse_login_env_capture_path_only() {
        let (path, env) = parse_login_env_capture("\x01/usr/bin\x01");
        assert_eq!(path.as_deref(), Some("/usr/bin"));
        assert!(env.is_empty());
    }

    #[tokio::test]
    async fn test_non_persistent_shell_no_state() {
        // Verify the default (non-persistent) mode doesn't carry state.
        let backend = LocalTerminalBackend::new();

        let result = backend
            .run(make_request("export SHOULD_NOT_PERSIST=yes"))
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));

        let result = backend
            .run(make_request("echo ${SHOULD_NOT_PERSIST:-empty}"))
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(
            result.combined_output.trim(),
            "empty",
            "non-persistent mode should not carry state"
        );
    }

    /// End-to-end test: completed background task remains queryable after
    /// its ProcessState is evicted from the actor's process map.
    ///
    /// Uses a 200ms TTL so we don't have to wait 5 minutes.
    #[tokio::test]
    async fn completed_bg_task_queryable_after_eviction() {
        let ttl = Duration::from_millis(200);
        let backend = LocalTerminalBackend::new_with_completed_task_ttl(ttl);

        // 1. Start a fast background command.
        let mut req = make_request("echo eviction_test_output");
        req.tool_call_id = "evict-test".to_string();
        let bg = backend
            .run_background(req)
            .await
            .expect("background spawn should succeed");

        // 2. Wait for completion.
        let snap = backend
            .wait_for_completion(&bg.task_id, Some(Duration::from_secs(5)))
            .await
            .expect("task should complete");
        assert!(snap.completed, "task should be completed");
        assert_eq!(snap.exit_code, Some(0));

        // 3. Sleep past the TTL so the actor evicts the ProcessState.
        tokio::time::sleep(ttl + Duration::from_millis(200)).await;

        // 4. get_task should still return the snapshot (from the tombstone map).
        let snap_after = backend
            .get_task(&bg.task_id)
            .await
            .expect("task should still be queryable after eviction");
        assert!(snap_after.completed);
        assert_eq!(snap_after.exit_code, Some(0));
        assert_eq!(snap_after.task_id, bg.task_id);
        // The tombstone drops the output but still reports the size the task
        // produced, so it has to say the output is incomplete.
        assert!(snap_after.output.is_empty());
        assert!(snap_after.output_total_bytes > 0);
        assert!(
            snap_after.truncated,
            "a tombstone that reports bytes must not claim complete output"
        );

        // 5. list_tasks should include the evicted task.
        let all = backend.list_tasks().await;
        assert!(
            all.iter().any(|t| t.task_id == bg.task_id),
            "evicted task should appear in list_tasks"
        );

        // 6. wait_for_completion should also return it (already done).
        let snap_wait = backend
            .wait_for_completion(&bg.task_id, Some(Duration::from_millis(100)))
            .await
            .expect("wait_for_completion should return evicted task");
        assert!(snap_wait.completed);
    }

    // -----------------------------------------------------------------------
    // Auto-wake suppression tests (TOCTOU race fixes)
    // -----------------------------------------------------------------------

    /// Fix 3 — when wait_for_completion is called AFTER the task was
    /// evicted from `processes` (snapshot-only branch), the returned
    /// snapshot must reflect `block_waited=true` AND the in-place
    /// tombstone in `completed_task_snapshots` must also be updated.
    ///
    /// IMPORTANT: this test must NOT call `wait_for_completion` before
    /// eviction, because that would set `process.block_waited=true` on
    /// the live process and the eviction snapshot at
    /// `poll_all_processes` step 4 would copy `block_waited: true` into
    /// the tombstone (so the late-wait assertion would trivially pass
    /// even with Fix 3 reverted). Sequence below keeps the tombstone
    /// born with `block_waited=false` so the test actually exercises
    /// the in-place mutation in `handle_wait_for_completion`'s
    /// already-evicted branch.
    #[tokio::test]
    async fn wait_after_eviction_returns_snapshot_with_block_waited_true() {
        let ttl = Duration::from_millis(100);
        let backend = LocalTerminalBackend::new_with_completed_task_ttl(ttl);

        // 1. Spawn a fast-exit background task. DO NOT call wait_for_completion
        //    so process.block_waited stays false.
        let mut req = make_request("echo evict_and_wait");
        req.tool_call_id = "evict-wait".to_string();
        let bg = backend
            .run_background(req)
            .await
            .expect("background spawn should succeed");

        // 2. Poll `get_task` until the task completes.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut completed = false;
        while Instant::now() < deadline {
            if let Some(snap) = backend.get_task(&bg.task_id).await
                && snap.completed
            {
                completed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(completed, "task should complete within deadline");

        // 3. Sleep past the TTL so eviction triggers.
        tokio::time::sleep(ttl + Duration::from_millis(250)).await;

        // 4. Assert the tombstone was born with `block_waited == false`.
        //    This pins the precondition: the tombstone has NOT inherited
        //    block_waited from the process, so a passing assertion below
        //    can only come from Fix 3's in-place mutation.
        let snap_pre_wait = backend
            .get_task(&bg.task_id)
            .await
            .expect("evicted task should still be queryable via tombstone");
        assert!(
            snap_pre_wait.completed,
            "tombstone should reflect completion"
        );
        assert!(
            !snap_pre_wait.block_waited,
            "tombstone must be born with block_waited=false (no wait_for_completion \
             was called before eviction); got block_waited=true which would make \
             the late-wait assertion below trivial"
        );

        // 5. Late wait_for_completion against the tombstone.
        let snap_after = backend
            .wait_for_completion(&bg.task_id, Some(Duration::from_millis(100)))
            .await
            .expect("late wait should still return evicted snapshot");
        assert!(snap_after.completed);
        assert!(
            snap_after.block_waited,
            "late wait must set block_waited=true on the returned snapshot"
        );

        // 6. A subsequent get_task must see the imprinted flag too,
        //    confirming the mutation is in-place and observable to
        //    other readers of the tombstone (Fix 3's reason for using
        //    get_mut over get + clone).
        let snap_via_get = backend
            .get_task(&bg.task_id)
            .await
            .expect("get_task should still return after eviction");
        assert!(
            snap_via_get.block_waited,
            "get_task after late wait must see the persisted block_waited flag \
             — Fix 3's in-place mutation must be observable to other readers"
        );
    }

    #[tokio::test]
    async fn wait_on_already_completed_task_returns_immediately() {
        let backend = LocalTerminalBackend::new();
        let bg = backend
            .run_background(make_request("echo already-done"))
            .await
            .expect("spawn");
        assert!(
            poll_until_task_completed(&backend, &bg.task_id, Duration::from_secs(5)).await,
            "echo should finish"
        );

        let started = Instant::now();
        let snap = backend
            .wait_for_completion(&bg.task_id, Some(Duration::from_secs(600)))
            .await
            .expect("completed snapshot");
        assert!(snap.completed);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "already-completed wait must not burn the 600s cap; elapsed {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn wait_on_already_killed_task_returns_immediately() {
        let backend = LocalTerminalBackend::new();
        let bg = backend
            .run_background(make_request("sleep 60"))
            .await
            .expect("spawn");
        assert_eq!(backend.kill_task(&bg.task_id).await, KillOutcome::Killed);
        assert!(
            poll_until_task_completed(&backend, &bg.task_id, Duration::from_secs(5)).await,
            "kill should complete the task"
        );

        let started = Instant::now();
        let snap = backend
            .wait_for_completion(&bg.task_id, Some(Duration::from_secs(600)))
            .await
            .expect("killed snapshot");
        assert!(snap.completed);
        assert!(snap.explicitly_killed);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "already-killed wait must not burn the 600s cap; elapsed {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn wait_on_tombstoned_task_returns_immediately() {
        let ttl = Duration::from_millis(100);
        let backend = LocalTerminalBackend::new_with_completed_task_ttl(ttl);
        let bg = backend
            .run_background(make_request("echo tombstone-wait"))
            .await
            .expect("spawn");
        assert!(
            poll_until_task_completed(&backend, &bg.task_id, Duration::from_secs(5)).await,
            "echo should finish"
        );
        tokio::time::sleep(ttl + Duration::from_millis(250)).await;

        let started = Instant::now();
        let snap = backend
            .wait_for_completion(&bg.task_id, Some(Duration::from_secs(600)))
            .await
            .expect("tombstone snapshot");
        assert!(snap.completed);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "tombstone wait must not burn the 600s cap; elapsed {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn wait_on_unknown_task_returns_immediately() {
        let backend = LocalTerminalBackend::new();
        let started = Instant::now();
        let snap = backend
            .wait_for_completion("never-existed", Some(Duration::from_secs(600)))
            .await;
        assert!(snap.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "not-found wait must not burn the 600s cap; elapsed {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn wait_on_still_running_task_times_out() {
        let backend = LocalTerminalBackend::new();
        let bg = backend
            .run_background(make_request("sleep 60"))
            .await
            .expect("spawn");

        let started = Instant::now();
        let snap = backend
            .wait_for_completion(&bg.task_id, Some(Duration::from_millis(200)))
            .await
            .expect("timeout snapshot");
        assert!(
            !snap.completed,
            "still-running wait must not take the already-terminal path"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "still-running wait must block until its timeout; elapsed {:?}",
            started.elapsed()
        );
        let _ = backend.kill_task(&bg.task_id).await;
    }

    /// A blocking wait whose receiver is dropped before the task completes
    /// (the awaiting turn was cancelled, e.g. Ctrl+C mid
    /// `get_command_or_subagent_output`) must NOT leave `block_waited=true`
    /// imprinted on the task. The model never received the result, so the
    /// completion must still auto-wake it.
    ///
    /// Regression test for the cancelled-wait race:
    /// wait(timeout_ms=180000) → Ctrl+C → task exits before the wait
    /// deadline → completion delivered to a dead oneshot → `block_waited`
    /// stayed true → auto-wake suppressed → agent slept until the user
    /// manually typed "continue".
    #[tokio::test]
    async fn cancelled_wait_does_not_suppress_auto_wake_on_completion() {
        let backend = LocalTerminalBackend::new();

        // 1. Background task that outlives the cancelled waiter below.
        let mut req = make_request("sleep 1; echo done");
        req.tool_call_id = "cancelled-wait".to_string();
        let bg = backend
            .run_background(req)
            .await
            .expect("background spawn should succeed");

        // 2. Register a blocking wait with a deadline far beyond the task's
        //    runtime, then cancel it (drop the future — and with it the
        //    oneshot receiver) long before the task exits. This mirrors a
        //    turn abort while `get_task_output` blocks.
        let cancelled = tokio::time::timeout(
            Duration::from_millis(100),
            backend.wait_for_completion(&bg.task_id, Some(Duration::from_secs(120))),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "wait must still be pending when the caller is cancelled"
        );

        // 3. Let the task complete and its completion be processed.
        assert!(
            poll_until_task_completed(&backend, &bg.task_id, Duration::from_secs(10)).await,
            "task should complete within deadline"
        );

        // 4. The completion was never delivered to any waiter, so
        //    block_waited must be false — otherwise the notification
        //    bridge suppresses the TaskCompleted auto-wake.
        let snap = backend
            .get_task(&bg.task_id)
            .await
            .expect("completed task should be queryable");
        assert!(
            !snap.block_waited,
            "a cancelled (never-delivered) blocking wait must not leave \
             block_waited=true — that suppresses the completion auto-wake"
        );
    }

    /// If one waiter is cancelled but another live waiter consumes the
    /// completion, `block_waited` must stay true: the model received the
    /// result through the surviving call, so the auto-wake would be
    /// redundant noise.
    #[tokio::test]
    async fn cancelled_wait_alongside_live_waiter_keeps_block_waited() {
        let backend = LocalTerminalBackend::new();

        let mut req = make_request("sleep 1; echo done");
        req.tool_call_id = "mixed-waiters".to_string();
        let bg = backend
            .run_background(req)
            .await
            .expect("background spawn should succeed");

        // Dead waiter: cancelled before completion.
        let cancelled = tokio::time::timeout(
            Duration::from_millis(100),
            backend.wait_for_completion(&bg.task_id, Some(Duration::from_secs(120))),
        )
        .await;
        assert!(cancelled.is_err(), "first wait should be cancelled");

        // Live waiter: rides until completion.
        let snap = backend
            .wait_for_completion(&bg.task_id, Some(Duration::from_secs(30)))
            .await
            .expect("live wait should return the completed snapshot");
        assert!(snap.completed, "live wait should observe completion");
        assert!(
            snap.block_waited,
            "delivered wait must keep block_waited=true on the returned snapshot"
        );

        let snap_via_get = backend
            .get_task(&bg.task_id)
            .await
            .expect("completed task should be queryable");
        assert!(
            snap_via_get.block_waited,
            "block_waited must remain true when at least one waiter received \
             the completion — auto-wake would be redundant"
        );
    }

    #[tokio::test]
    async fn kill_with_live_waiter_marks_result_delivered() {
        for source in [KillSource::ClientUi, KillSource::ModelTool] {
            let backend = LocalTerminalBackend::new();
            let mut req = make_request("sleep 60");
            req.tool_call_id = format!("live-waiter-{source:?}");
            let bg = backend.run_background(req).await.expect("spawn");

            let waiter_backend = backend.clone();
            let task_id = bg.task_id.clone();
            let wait = tokio::spawn(async move {
                waiter_backend
                    .wait_for_completion(&task_id, Some(Duration::from_secs(30)))
                    .await
            });
            let waiter_deadline = std::time::Instant::now() + Duration::from_secs(2);
            let waiter_ready = loop {
                if backend
                    .get_task(&bg.task_id)
                    .await
                    .is_some_and(|s| s.block_waited)
                {
                    break true;
                }
                if std::time::Instant::now() >= waiter_deadline {
                    break false;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            assert!(
                waiter_ready,
                "waiter must register before kill ({source:?})"
            );

            assert_eq!(
                backend.kill_task_with_source(&bg.task_id, source).await,
                KillOutcome::Killed
            );
            let snap = backend.get_task(&bg.task_id).await.expect("killed task");
            assert!(snap.explicitly_killed, "{source:?}");
            assert!(
                snap.kill_result_delivered,
                "live waiter must mark kill_result_delivered ({source:?})"
            );
            assert!(
                snap.block_waited,
                "live waiter must keep block_waited ({source:?})"
            );
            assert!(
                snap.is_auto_wake_suppressed(),
                "delivered kill must suppress ({source:?})"
            );
            let waited = wait.await.expect("join").expect("wait snapshot");
            assert!(waited.completed, "{source:?}");
        }
    }

    #[tokio::test]
    async fn kill_with_dropped_waiter_clears_block_waited() {
        for source in [KillSource::ClientUi, KillSource::ModelTool] {
            let backend = LocalTerminalBackend::new();
            let mut req = make_request("sleep 60");
            req.tool_call_id = format!("dropped-waiter-{source:?}");
            let bg = backend.run_background(req).await.expect("spawn");

            let cancelled = tokio::time::timeout(
                Duration::from_millis(100),
                backend.wait_for_completion(&bg.task_id, Some(Duration::from_secs(120))),
            )
            .await;
            assert!(cancelled.is_err(), "wait must be cancelled ({source:?})");

            assert_eq!(
                backend.kill_task_with_source(&bg.task_id, source).await,
                KillOutcome::Killed
            );
            let snap = backend.get_task(&bg.task_id).await.expect("killed task");
            assert!(snap.explicitly_killed, "{source:?}");
            assert!(
                !snap.block_waited,
                "dropped waiter must clear block_waited ({source:?})"
            );
            let expect_delivered = source.marks_result_delivered(false);
            assert_eq!(snap.kill_result_delivered, expect_delivered, "{source:?}");
            assert_eq!(
                snap.is_auto_wake_suppressed(),
                expect_delivered,
                "ClientUi + dropped waiter must wake; ModelTool still suppresses ({source:?})"
            );
        }
    }

    #[tokio::test]
    async fn kill_without_waiter_delivery_depends_on_source() {
        // Hardcoded per-source so a no-op handle_kill or a formula change fails.
        for (source, expect_delivered) in [
            (KillSource::ClientUi, false),
            (KillSource::ModelTool, true),
            (KillSource::Teardown, true),
        ] {
            let backend = LocalTerminalBackend::new();
            let mut req = make_request("sleep 60");
            req.tool_call_id = format!("no-waiter-{source:?}");
            let bg = backend.run_background(req).await.expect("spawn");

            assert_eq!(
                backend.kill_task_with_source(&bg.task_id, source).await,
                KillOutcome::Killed
            );
            let snap = backend.get_task(&bg.task_id).await.expect("killed task");
            assert!(snap.explicitly_killed, "{source:?}");
            assert!(!snap.block_waited, "{source:?}");
            assert_eq!(snap.kill_result_delivered, expect_delivered, "{source:?}");
            assert_eq!(
                snap.is_auto_wake_suppressed(),
                expect_delivered,
                "{source:?}"
            );
        }
    }

    #[tokio::test]
    async fn kill_task_defaults_to_model_tool_source() {
        let backend = LocalTerminalBackend::new();
        let mut req = make_request("sleep 60");
        req.tool_call_id = "default-model-tool".into();
        let bg = backend.run_background(req).await.expect("spawn");
        assert_eq!(backend.kill_task(&bg.task_id).await, KillOutcome::Killed);
        let snap = backend.get_task(&bg.task_id).await.expect("killed task");
        assert!(snap.explicitly_killed);
        assert!(
            snap.kill_result_delivered,
            "bare kill_task is a model-tool kill"
        );
        assert!(snap.is_auto_wake_suppressed());
    }

    #[tokio::test]
    async fn teardown_sweeps_mark_result_delivered() {
        let backend = LocalTerminalBackend::new();
        let mut owned = make_request("sleep 60");
        owned.tool_call_id = "teardown-owned".into();
        owned.owner_session_id = Some("session-a".into());
        let owned = backend.run_background(owned).await.expect("spawn");

        let mut unowned = make_request("sleep 60");
        unowned.tool_call_id = "teardown-all".into();
        let unowned = backend.run_background(unowned).await.expect("spawn");

        backend
            .kill_all_background_tasks_by_owner("session-a")
            .await;
        let snap = backend.get_task(&owned.task_id).await.expect("owned");
        assert!(snap.completed && snap.explicitly_killed);
        assert!(
            snap.kill_result_delivered && snap.is_auto_wake_suppressed(),
            "owner teardown must suppress auto-wake"
        );

        backend.kill_all_background_tasks().await;
        let snap = backend.get_task(&unowned.task_id).await.expect("unowned");
        assert!(snap.completed && snap.explicitly_killed);
        assert!(
            snap.kill_result_delivered && snap.is_auto_wake_suppressed(),
            "global teardown must suppress auto-wake"
        );
    }

    #[tokio::test]
    async fn kill_bits_survive_ttl_eviction() {
        let ttl = Duration::from_millis(100);
        let backend = LocalTerminalBackend::new_with_completed_task_ttl(ttl);
        for (source, expect_delivered) in
            [(KillSource::ModelTool, true), (KillSource::ClientUi, false)]
        {
            let mut req = make_request("sleep 60");
            req.tool_call_id = format!("evict-kill-{source:?}");
            let bg = backend.run_background(req).await.expect("spawn");
            assert_eq!(
                backend.kill_task_with_source(&bg.task_id, source).await,
                KillOutcome::Killed
            );
            assert!(
                poll_until_task_completed(&backend, &bg.task_id, Duration::from_secs(5)).await,
                "killed task should complete ({source:?})"
            );
            tokio::time::sleep(ttl + Duration::from_millis(250)).await;
            let snap = backend
                .get_task(&bg.task_id)
                .await
                .expect("tombstone must remain queryable");
            assert!(snap.completed, "{source:?}");
            assert!(snap.explicitly_killed, "{source:?}");
            assert_eq!(snap.kill_result_delivered, expect_delivered, "{source:?}");
            assert_eq!(
                snap.is_auto_wake_suppressed(),
                expect_delivered,
                "{source:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Owner-scoped kill and reparent tests
    // -----------------------------------------------------------------------

    /// Helper: create a request owned by a specific session.
    fn make_owned_request(command: &str, owner: &str) -> TerminalRunRequest {
        let mut req = make_request(command);
        req.owner_session_id = Some(owner.to_string());
        req
    }

    #[tokio::test]
    async fn kill_foreground_by_owner_only_kills_matching_session() {
        let backend = LocalTerminalBackend::new();

        // Spawn two foreground long-running commands owned by different sessions.
        // Use auto_background_on_timeout with a long timeout so they stay foreground.
        let mut req_a = make_owned_request("sleep 60", "session-a");
        req_a.tool_call_id = "fg-a".to_string();
        req_a.timeout = Duration::from_secs(300);

        let mut req_b = make_owned_request("sleep 60", "session-b");
        req_b.tool_call_id = "fg-b".to_string();
        req_b.timeout = Duration::from_secs(300);

        // Run both as background tasks first (so they don't block this test),
        // then we'll test the scoped kill via the backend trait method.
        let handle_a = backend.run_background(req_a).await.unwrap();
        let handle_b = backend.run_background(req_b).await.unwrap();

        // Both should be running
        let snap_a = backend.get_task(&handle_a.task_id).await;
        let snap_b = backend.get_task(&handle_b.task_id).await;
        assert!(snap_a.is_some(), "task A should exist");
        assert!(snap_b.is_some(), "task B should exist");

        // Kill only session-a's tasks
        backend
            .kill_all_background_tasks_by_owner("session-a")
            .await;

        // Give the actor a moment to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Task A should be killed (completed with explicitly_killed)
        let snap_a = backend.get_task(&handle_a.task_id).await;
        assert!(
            snap_a.is_some_and(|s| s.completed && s.explicitly_killed),
            "task A should be killed"
        );

        // Task B should still be running
        let snap_b = backend.get_task(&handle_b.task_id).await;
        assert!(
            snap_b.is_some_and(|s| !s.completed),
            "task B should still be running"
        );

        // Cleanup
        backend.kill_task(&handle_b.task_id).await;
    }

    #[tokio::test]
    async fn kill_by_owner_ignores_unowned_tasks() {
        let backend = LocalTerminalBackend::new();

        // Spawn a task with no owner (None)
        let mut req = make_request("sleep 60");
        req.tool_call_id = "fg-none".to_string();
        let handle = backend.run_background(req).await.unwrap();

        // Killing by a specific owner should NOT affect unowned tasks
        backend
            .kill_all_background_tasks_by_owner("some-session")
            .await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        let snap = backend.get_task(&handle.task_id).await;
        assert!(
            snap.is_some_and(|s| !s.completed),
            "unowned task should NOT be killed by owner-scoped kill"
        );

        // Cleanup
        backend.kill_task(&handle.task_id).await;
    }

    #[tokio::test]
    async fn reparent_notifications_changes_owner_and_sends_synthetic() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let new_handle = ToolNotificationHandle::from_sender(tx);

        let backend: std::sync::Arc<dyn crate::computer::types::TerminalBackend> =
            std::sync::Arc::new(LocalTerminalBackend::new());

        // Spawn a background task owned by "child-session"
        let mut req = make_owned_request("sleep 60", "child-session");
        req.tool_call_id = "reparent-test".to_string();
        let bg = backend.run_background(req).await.unwrap();

        // Verify it's running and owned by child-session
        let snap = backend.get_task(&bg.task_id).await.unwrap();
        assert!(!snap.completed);
        assert_eq!(snap.owner_session_id.as_deref(), Some("child-session"));

        // Reparent from child-session to parent-session
        backend
            .reparent_notifications(
                "child-session",
                "parent-session",
                new_handle,
                std::sync::Arc::downgrade(&backend),
            )
            .await;

        // Verify owner changed
        let snap = backend.get_task(&bg.task_id).await.unwrap();
        assert_eq!(
            snap.owner_session_id.as_deref(),
            Some("parent-session"),
            "owner should be reparented to parent-session"
        );

        // Verify synthetic BashExecutionBackgrounded notification was sent
        let mut found_backgrounded = false;
        while let Ok(notification) = rx.try_recv() {
            if let crate::notification::types::ToolNotification::BashExecutionBackgrounded(
                bg_notif,
            ) = notification
            {
                assert_eq!(bg_notif.task_id, bg.task_id);
                found_backgrounded = true;
            }
        }
        assert!(
            found_backgrounded,
            "reparent should send a synthetic BashExecutionBackgrounded notification"
        );

        // Now killing by parent-session should kill the reparented task
        backend
            .kill_all_background_tasks_by_owner("parent-session")
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let snap = backend.get_task(&bg.task_id).await;
        assert!(
            snap.is_some_and(|s| s.completed),
            "reparented task should be killed when parent-session tasks are killed"
        );
    }

    #[tokio::test]
    async fn reparent_skips_tasks_owned_by_other_sessions() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let new_handle = ToolNotificationHandle::from_sender(tx);

        let backend: std::sync::Arc<dyn crate::computer::types::TerminalBackend> =
            std::sync::Arc::new(LocalTerminalBackend::new());

        // Spawn tasks owned by different sessions
        let mut req_child = make_owned_request("sleep 60", "child-session");
        req_child.tool_call_id = "reparent-child".to_string();
        let bg_child = backend.run_background(req_child).await.unwrap();

        let mut req_sibling = make_owned_request("sleep 60", "sibling-session");
        req_sibling.tool_call_id = "reparent-sibling".to_string();
        let bg_sibling = backend.run_background(req_sibling).await.unwrap();

        // Reparent only child-session
        backend
            .reparent_notifications(
                "child-session",
                "parent-session",
                new_handle,
                std::sync::Arc::downgrade(&backend),
            )
            .await;

        // Child's owner should change
        let snap_child = backend.get_task(&bg_child.task_id).await.unwrap();
        assert_eq!(
            snap_child.owner_session_id.as_deref(),
            Some("parent-session")
        );

        // Sibling's owner should NOT change
        let snap_sibling = backend.get_task(&bg_sibling.task_id).await.unwrap();
        assert_eq!(
            snap_sibling.owner_session_id.as_deref(),
            Some("sibling-session"),
            "sibling task should not be reparented"
        );

        // Only one synthetic notification (for the child, not the sibling)
        let mut bg_count = 0;
        while let Ok(notification) = rx.try_recv() {
            if matches!(
                notification,
                crate::notification::types::ToolNotification::BashExecutionBackgrounded(_)
            ) {
                bg_count += 1;
            }
        }
        assert_eq!(
            bg_count, 1,
            "only the child task should produce a synthetic notification"
        );

        // Cleanup
        backend.kill_task(&bg_child.task_id).await;
        backend.kill_task(&bg_sibling.task_id).await;
    }

    #[tokio::test]
    async fn owner_session_id_propagated_through_run_and_snapshot() {
        let backend = LocalTerminalBackend::new();

        let mut req = make_owned_request("echo owned", "test-owner");
        req.tool_call_id = "owned-test".to_string();
        let bg = backend.run_background(req).await.unwrap();

        // Wait for completion
        let snap = backend
            .wait_for_completion(&bg.task_id, Some(Duration::from_secs(5)))
            .await
            .expect("task should complete");

        assert!(snap.completed);
        assert_eq!(
            snap.owner_session_id.as_deref(),
            Some("test-owner"),
            "owner_session_id should propagate from request to snapshot"
        );
    }
}
