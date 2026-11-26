use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::notification::types::ToolNotificationHandle;

// ============================================================================
// Error types
// ============================================================================

#[derive(thiserror::Error, Debug, Clone)]
pub enum ComputerError {
    #[error("IO Error: {0}")]
    IOError(String, Option<std::io::ErrorKind>),
    #[error("UnQuoted command")]
    CommandNotQuoted,
}

impl ComputerError {
    /// Create an IO error from a message string with no preserved error kind.
    pub fn io(msg: impl Into<String>) -> Self {
        Self::IOError(msg.into(), None)
    }

    pub fn io_with_kind(msg: impl Into<String>, kind: std::io::ErrorKind) -> Self {
        Self::IOError(msg.into(), Some(kind))
    }

    /// Returns the underlying `io::ErrorKind` or `None`
    pub fn io_error_kind(&self) -> Option<std::io::ErrorKind> {
        match self {
            Self::IOError(_, kind) => *kind,
            _ => None,
        }
    }
}

impl From<std::io::Error> for ComputerError {
    fn from(err: std::io::Error) -> Self {
        Self::IOError(err.to_string(), Some(err.kind()))
    }
}

// ============================================================================
// File system trait
// ============================================================================

#[async_trait::async_trait]
pub trait AsyncFileSystem: Send + Sync {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, ComputerError>;

    async fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), ComputerError>;

    async fn delete_file(&self, path: &Path) -> Result<(), ComputerError>;

    /// Whether `path` exists as a readable regular file, without reading
    /// its contents. `Ok(false)` means a definitive not-found; other probe
    /// failures surface as `Err`.
    ///
    /// The default errs with `ErrorKind::Unsupported` — callers must treat
    /// `Err` as "unknown" and fail closed. Backends opt in by overriding
    /// with a cheap stat/lookup; a full-content read is never an acceptable
    /// probe (the target may be arbitrarily large or remote).
    async fn file_exists(&self, path: &Path) -> Result<bool, ComputerError> {
        let _ = path;
        Err(ComputerError::io_with_kind(
            "file_exists is not supported by this backend",
            std::io::ErrorKind::Unsupported,
        ))
    }
}

// ============================================================================
// Terminal types
// ============================================================================

pub struct TerminalRunRequest {
    pub command: String,
    pub working_directory: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout: Duration,
    pub output_byte_limit: usize,
    /// File path to write output incrementally as it arrives.
    /// This ensures full output is always available even after in-memory buffer is truncated.
    /// For background tasks, this allows retrieval of output after the agent has moved on.
    pub output_file: PathBuf,

    /// Notification handle for streaming output chunks during execution.
    /// The backend sends `BashOutputChunk` notifications every ~100ms.
    /// Callers that don't need streaming pass `ToolNotificationHandle::noop()`
    /// — messages are silently dropped. No `Option` wrapper needed.
    pub notification_handle: ToolNotificationHandle,

    /// Tool call ID for correlating notifications with the tool invocation.
    /// Flows from `ToolContext::tool_call_id()` through the actor to
    /// `BashOutputChunk.base.tool_call_id`.
    pub tool_call_id: String,

    /// Original user command before isolation wrapping.
    ///
    /// When set, the terminal actor stores this on the `ProcessEntry` so
    /// `get_task()` returns it in `TaskSnapshot.display_command`. This
    /// ensures model-facing `get_task_output` shows the user's command
    /// instead of the `unshare`/mount wrapper.
    pub display_command: Option<String>,

    /// Auto-background on timeout instead of killing (default `false`).
    pub auto_background_on_timeout: bool,

    /// When [`Self::auto_background_on_timeout`] is true, maximum time the
    /// command may block the turn before being moved to the background (process
    /// keeps running). Independent of [`Self::timeout`].
    ///
    /// - `None` → use the terminal backend default (typically 15s).
    /// - `Some(Duration::MAX)` → no short budget; auto-bg only when `timeout` elapses.
    /// - `Some(d)` → auto-bg after `d` if still running.
    pub foreground_block_budget: Option<Duration>,

    /// Task kind for distinguishing monitor tasks from regular bash tasks.
    pub kind: TaskKind,

    /// Session that owns this process. Used to scope kill operations so
    /// `kill_all_background_tasks_by_owner` only targets the requesting
    /// session's processes — not the parent's or sibling's.
    pub owner_session_id: Option<String>,
    /// Model-supplied label for task UI / snapshots.
    pub description: Option<String>,
}

/// Distinguishes different types of background tasks.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Regular bash command.
    #[default]
    Bash,
    /// Monitor tool — streams stdout events with rate limiting.
    Monitor,
}

#[derive(Clone)]
pub struct TerminalRunResult {
    pub combined_output: String,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    pub signal: Option<String>,
    pub timed_out: bool,
    /// Path to the output file where full output is stored.
    /// Use read_file tool to retrieve full output when truncated.
    pub output_file: PathBuf,
    /// Total bytes of output (before truncation).
    /// When truncated, combined_output contains the first and last portions up to output_byte_limit chars.
    pub total_bytes: usize,
    /// PID of the spawned shell process, when available. Set by the
    /// local terminal backend at spawn time. Useful for foreground
    /// commands that auto-background on timeout: the resulting
    /// `BackgroundTaskStarted` can carry the real PID instead of a
    /// placeholder. `None` for backends that cannot surface a local
    /// PID (e.g. ACP/remote terminals) or when the process exited
    /// before `child.id()` could be queried.
    pub pid: Option<u32>,
}

/// Returned by `TerminalBackend::run_background` — gives the caller the task_id
/// to use for subsequent queries via `get_task`, `kill_task`, `wait_for_completion`.
pub struct BackgroundHandle {
    pub task_id: String,
    pub output_file: PathBuf,
    /// PID of the spawned shell process, when available. `None` for
    /// backends that do not surface a local PID (e.g. ACP/remote
    /// gateways) or when the process exited before the PID could be
    /// captured.
    pub pid: Option<u32>,
}

/// Full snapshot of a task's state.
/// Used by both local and ACP backends.
#[derive(
    Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct TaskSnapshot {
    pub task_id: String,
    /// The actual command that was executed (may be isolation-wrapped).
    pub command: String,
    /// The original user command before isolation wrapping.
    ///
    /// When set, model/user-facing output should prefer this over `command`
    /// to avoid exposing internal isolation mechanics (unshare/mount wrapper).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_command: Option<String>,
    pub cwd: String,
    pub start_time: std::time::SystemTime,
    pub end_time: Option<std::time::SystemTime>,
    pub output: String,
    pub output_file: PathBuf,
    /// `output` may not be the whole output: read `output_file` for the rest.
    /// Says the copy is partial, not how it came to be.
    pub truncated: bool,
    /// Total bytes the task has written, when the source tracks it. `output`
    /// may hold only part of that; zero means unknown.
    #[serde(default)]
    pub output_total_bytes: usize,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub completed: bool,
    /// Task kind: bash (default) or monitor.
    #[serde(default)]
    pub kind: TaskKind,
    /// Whether a block-waiter (`block=true`) consumed this task's result.
    /// When set, the notification bridge skips auto-wake synthetic prompts
    /// because the blocking caller already received the result directly.
    #[serde(default)]
    pub block_waited: bool,
    /// Whether this task was explicitly killed (kill tool, UI Stop, or
    /// teardown) rather than exiting on its own. Display/tombstone flag;
    /// auto-wake uses [`Self::is_auto_wake_suppressed`].
    #[serde(default)]
    pub explicitly_killed: bool,
    /// Model already got a kill/wait tool result, or the kill was teardown.
    /// False for UI/Stop with no live waiter. Missing on the wire is false.
    #[serde(default)]
    pub kill_result_delivered: bool,

    /// Session that owns this task. Used for scoped kill operations so
    /// subagent teardown only kills the subagent's own tasks, not
    /// the parent's or sibling's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    /// Model-supplied label for task UI / snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// True after explicit/user/auto backgrounding; false for pure foreground runs.
    #[serde(default)]
    pub is_backgrounded: bool,
}

impl TaskSnapshot {
    /// Calculate duration in seconds.
    /// If task is still running, returns time since start.
    pub fn duration_secs(&self) -> f64 {
        let end = self.end_time.unwrap_or_else(std::time::SystemTime::now);
        end.duration_since(self.start_time)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    /// True iff the task has NOT yet completed — covers bash AND
    /// monitor task kinds (the `kind` field doesn't change this
    /// predicate; the runtime turn-end TodoGate counts both as
    /// backing work).
    pub fn is_outstanding(&self) -> bool {
        !self.completed
    }

    /// The output on hand, and the size of the output it came from. Readers
    /// take the size from here so the "not tracked" case is handled once.
    pub fn output_view(&self) -> crate::util::truncate::PartialOutput<'_> {
        crate::util::truncate::PartialOutput::part_of(&self.output, self.output_total_bytes)
    }

    /// Incomplete and backgrounded — tray/`tasks_snapshot` predicate (not FG in-flight).
    pub fn is_outstanding_background(&self) -> bool {
        !self.completed && self.is_backgrounded
    }

    /// True when a TaskCompleted auto-wake would be redundant.
    pub fn is_auto_wake_suppressed(&self) -> bool {
        self.block_waited || (self.explicitly_killed && self.kill_result_delivered)
    }
}

/// Result of killing a terminal task.
///
/// Serialized over the wire in the `echo.agent/task/kill` ext response
/// (`echo-agent-runtime::extensions::task::KillTaskResponse`) and deserialized
/// by clients (echo-agent-pager), so it derives both serde directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KillOutcome {
    Killed,
    AlreadyExited,
    NotFound,
}

/// Who initiated a background-task kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSource {
    /// Model `kill_task` / `kill_command_or_subagent` tool.
    ModelTool,
    /// Single-task client UI kill (task-pane `[×]`).
    ClientUi,
    /// Bulk teardown: owner sweep, dashboard stop-all, session delete, headless reap.
    Teardown,
}

impl KillSource {
    /// Model-tool and teardown kills count as delivered; a client/UI kill
    /// counts only when a waiter actually received the result.
    pub fn marks_result_delivered(self, waiter_delivered: bool) -> bool {
        matches!(self, Self::ModelTool | Self::Teardown) || waiter_delivered
    }
}

/// One command moved to the background by [`TerminalBackend::background_foreground_commands`].
/// `tool_call_id` doubles as the background task id the model uses to retrieve output later.
#[derive(Debug, Clone)]
pub struct BackgroundedForeground {
    pub tool_call_id: String,
}

// ============================================================================
// TerminalBackend trait
// ============================================================================

/// The single abstraction over terminal execution backends.
///
/// Implemented by:
/// - `LocalTerminalBackend` (in echo-agent-tools, spawns processes)
/// - `AcpTerminalBackend` (in echo-agent-runtime, calls ACP protocol)
#[async_trait::async_trait]
pub trait TerminalBackend: Send + Sync {
    /// Run a command. Blocks until completion or timeout.
    async fn run(&self, request: TerminalRunRequest) -> Result<TerminalRunResult, ComputerError>;

    /// Start a command in the background. Returns immediately with a handle ID.
    /// The process continues running; use get_task/kill_task/wait_for_completion to manage it.
    async fn run_background(
        &self,
        request: TerminalRunRequest,
    ) -> Result<BackgroundHandle, ComputerError>;

    /// Get current snapshot of a background task.
    async fn get_task(&self, task_id: &str) -> Option<TaskSnapshot>;

    /// Kill a background task.
    ///
    /// Equivalent to [`Self::kill_task_with_source`] with
    /// [`KillSource::ModelTool`].
    async fn kill_task(&self, task_id: &str) -> KillOutcome;

    /// Kill a background task, recording who initiated the kill.
    ///
    /// The default ignores `source` and delegates to [`Self::kill_task`],
    /// which existing backends treat as a model-tool kill.
    async fn kill_task_with_source(&self, task_id: &str, source: KillSource) -> KillOutcome {
        let _ = source;
        self.kill_task(task_id).await
    }

    /// Kill all running foreground processes.
    async fn kill_foreground_commands(&self) {}

    /// Kill all running foreground processes owned by a specific session.
    /// Used on a shared terminal backend so a subagent's cancel doesn't
    /// kill the parent's foreground commands.
    async fn kill_foreground_commands_by_owner(&self, _owner_session_id: &str) {}

    /// Kill all running background tasks.
    /// Used during subagent teardown to clean up orphaned processes.
    async fn kill_all_background_tasks(&self) {}

    /// Kill all running background tasks owned by a specific session.
    /// Used during subagent teardown on a shared terminal backend so
    /// only the subagent's own tasks are killed — not the parent's.
    async fn kill_all_background_tasks_by_owner(&self, _owner_session_id: &str) {}

    async fn warm_shell(&self, _cwd: &std::path::Path) {}

    /// Reparent notification handles for all tasks owned by `old_owner_session_id`.
    /// Swaps the dead child session's notification handle with the parent's
    /// live handle so events from surviving processes route correctly.
    /// Also re-spawns monitor pipelines on the caller's runtime so monitor
    /// events continue streaming to the parent.
    ///
    /// `backend_weak` is a [`Weak`](std::sync::Weak) to *this* backend (anchored
    /// by the parent session's `Arc`); it drives re-spawned monitor pipelines
    /// without keeping the backend alive. See `run_monitor_pipeline`.
    async fn reparent_notifications(
        &self,
        _old_owner_session_id: &str,
        _new_owner_session_id: &str,
        _new_handle: crate::notification::types::ToolNotificationHandle,
        _backend_weak: std::sync::Weak<dyn TerminalBackend>,
    ) {
    }

    /// Move a foreground command to background by tool_call_id.
    /// The process keeps running but the foreground waiter is unblocked.
    /// Returns `true` if a matching foreground process was found.
    async fn background_foreground_command(&self, _tool_call_id: &str) -> bool {
        false
    }

    /// Background every running foreground command instead of killing it, scoped to `owner_session_id` when `Some`.
    /// Called on a mid-turn redirect (send-now); one returned entry per command lets the caller answer the model's dangling tool call.
    async fn background_foreground_commands(
        &self,
        _owner_session_id: Option<&str>,
    ) -> Vec<BackgroundedForeground> {
        Vec::new()
    }

    /// Wait for a background task to complete, with optional timeout.
    ///
    /// # Panics / overflow
    /// Implementations may add `timeout` to `Instant::now()`. Callers must
    /// bound `timeout` (e.g. via `capped_wait_timeout`) so the sum stays
    /// representable; unbounded model `timeout_ms` can overflow.
    async fn wait_for_completion(
        &self,
        task_id: &str,
        timeout: Option<Duration>,
    ) -> Option<TaskSnapshot>;

    /// List all known background tasks (running and completed).
    /// Used for context compaction to include task state in summaries.
    async fn list_tasks(&self) -> Vec<TaskSnapshot>;

    /// Return the persistent shell's current working directory, if persistent
    /// shell state is enabled. Returns `None` when persistence is off or the
    /// backend doesn't support it (e.g. ACP/remote).
    async fn get_shell_cwd(&self) -> Option<std::path::PathBuf> {
        None
    }
}

// ============================================================================
// Computer struct
// ============================================================================

/// Contains the computer struct which provides access to both the terminal and the fs
pub struct Computer {
    pub terminal: Arc<dyn TerminalBackend>,
    pub file_system: Arc<dyn AsyncFileSystem>,
}

impl Computer {
    pub fn new(terminal: Arc<dyn TerminalBackend>, file_system: Arc<dyn AsyncFileSystem>) -> Self {
        Self {
            terminal,
            file_system,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_kind_preserved_through_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let ce = ComputerError::from(io_err);
        assert_eq!(ce.io_error_kind(), Some(std::io::ErrorKind::NotFound));
    }

    #[test]
    fn io_error_kind_is_a_directory() {
        let io_err = std::io::Error::new(std::io::ErrorKind::IsADirectory, "it's a dir");
        let ce = ComputerError::from(io_err);
        assert_eq!(ce.io_error_kind(), Some(std::io::ErrorKind::IsADirectory));
    }

    #[test]
    fn io_error_kind_none_for_string_constructor() {
        let ce = ComputerError::io("something broke");
        assert_eq!(ce.io_error_kind(), None);
    }

    #[test]
    fn io_error_kind_none_for_non_io_variant() {
        let ce = ComputerError::CommandNotQuoted;
        assert_eq!(ce.io_error_kind(), None);
    }

    #[test]
    fn io_with_kind_preserves_not_found() {
        let ce = ComputerError::io_with_kind("Resource not found", std::io::ErrorKind::NotFound);
        assert_eq!(ce.io_error_kind(), Some(std::io::ErrorKind::NotFound));
    }

    #[test]
    fn io_with_kind_preserves_permission_denied() {
        let ce = ComputerError::io_with_kind("access denied", std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            ce.io_error_kind(),
            Some(std::io::ErrorKind::PermissionDenied)
        );
    }

    #[tokio::test]
    async fn file_exists_default_is_unsupported() {
        struct MinimalFs;

        #[async_trait::async_trait]
        impl AsyncFileSystem for MinimalFs {
            async fn read_file(&self, _path: &Path) -> Result<Vec<u8>, ComputerError> {
                panic!("the default file_exists must not read file contents");
            }
            async fn write_file(&self, _path: &Path, _data: &[u8]) -> Result<(), ComputerError> {
                unreachable!()
            }
            async fn delete_file(&self, _path: &Path) -> Result<(), ComputerError> {
                unreachable!()
            }
        }

        let err = MinimalFs
            .file_exists(Path::new("/any"))
            .await
            .expect_err("default probe must err");
        assert_eq!(err.io_error_kind(), Some(std::io::ErrorKind::Unsupported));
    }

    #[test]
    fn kill_source_marks_result_delivered() {
        assert!(KillSource::ModelTool.marks_result_delivered(false));
        assert!(KillSource::ModelTool.marks_result_delivered(true));
        assert!(KillSource::Teardown.marks_result_delivered(false));
        assert!(KillSource::Teardown.marks_result_delivered(true));
        assert!(!KillSource::ClientUi.marks_result_delivered(false));
        assert!(KillSource::ClientUi.marks_result_delivered(true));
    }

    #[test]
    fn snapshot_suppresses_auto_wake_only_when_result_was_delivered() {
        let mut snap = TaskSnapshot {
            task_id: "t".into(),
            command: "sleep 1".into(),
            display_command: None,
            cwd: "/tmp".into(),
            start_time: std::time::SystemTime::UNIX_EPOCH,
            end_time: None,
            output: String::new(),
            output_file: PathBuf::new(),
            truncated: false,
            output_total_bytes: 0,
            exit_code: None,
            signal: None,
            completed: false,
            kind: TaskKind::Bash,
            block_waited: false,
            explicitly_killed: false,
            kill_result_delivered: false,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
        };
        assert!(!snap.is_auto_wake_suppressed());

        snap.block_waited = true;
        assert!(snap.is_auto_wake_suppressed());
        snap.block_waited = false;

        snap.explicitly_killed = true;
        assert!(
            !snap.is_auto_wake_suppressed(),
            "UI/Stop kill with no delivered result must wake"
        );
        snap.kill_result_delivered = true;
        assert!(
            snap.is_auto_wake_suppressed(),
            "model-tool/teardown/delivered kill must suppress"
        );
        snap.explicitly_killed = false;
        assert!(
            !snap.is_auto_wake_suppressed(),
            "delivered bit alone is not a kill"
        );
    }

    #[test]
    fn kill_result_delivered_deserializes_default_false() {
        let mut snap = TaskSnapshot {
            task_id: "t".into(),
            command: "echo".into(),
            display_command: None,
            cwd: "/tmp".into(),
            start_time: std::time::SystemTime::UNIX_EPOCH,
            end_time: None,
            output: String::new(),
            output_file: PathBuf::from("/tmp/t.log"),
            truncated: false,
            output_total_bytes: 0,
            exit_code: None,
            signal: None,
            completed: true,
            kind: TaskKind::Bash,
            block_waited: false,
            explicitly_killed: true,
            kill_result_delivered: true,
            owner_session_id: None,
            description: None,
            is_backgrounded: false,
        };
        let mut value = serde_json::to_value(&snap).expect("serialize");
        value
            .as_object_mut()
            .expect("object")
            .remove("kill_result_delivered");
        let decoded: TaskSnapshot = serde_json::from_value(value).expect("legacy snapshot");
        assert!(!decoded.kill_result_delivered);
        assert!(decoded.explicitly_killed);
        assert!(
            !decoded.is_auto_wake_suppressed(),
            "legacy explicitly_killed without the new field is not a delivered kill"
        );
        snap.kill_result_delivered = false;
        assert_eq!(decoded, snap);
    }

    #[test]
    fn io_with_kind_matches_local_fs_dispatch_for_not_found() {
        // Simulate what LocalFs produces for a missing file
        let local_err = ComputerError::from(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No such file or directory (os error 2)",
        ));
        // Simulate what AcpFsAdapter now produces for RESOURCE_NOT_FOUND
        let acp_err =
            ComputerError::io_with_kind("Resource not found", std::io::ErrorKind::NotFound);

        // Both must produce the same io_error_kind so read_file dispatches
        // to FileNotFound("Error: {path} does not exist.") in both cases.
        assert_eq!(local_err.io_error_kind(), acp_err.io_error_kind());
    }
}
