//! Tool notifications — typed messages a running tool emits to subscribers
//! (TUI, gateway, audit log, ...) for live visibility into execution.
//!
//! The enum and its payload structs use unconditional serde derives so wire
//! adapters can serialise them without enabling additional features.
//!
//! Each `ToolNotification` variant has a parallel `send_*` convenience on
//! [`ToolNotificationHandle`]. The two surfaces are kept in lockstep — when
//! adding a variant here, add the `send_*` constructor too.
//!
//! The handle is built on `futures::channel::mpsc` so it is runtime-neutral:
//! the trait crate doesn't pin a particular async executor on its
//! consumers.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use futures::channel::mpsc;
use serde::{Deserialize, Serialize};

/// Common fields shared by every bash notification variant. Hoisted into a
/// dedicated struct so the variants stay in lockstep on tool_call_id /
/// command / output / cwd, and so payload-shape changes only need to be
/// made once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashNotificationBase {
    /// Tool call id, used to correlate with the originating tool call.
    pub tool_call_id: String,

    /// The command being executed.
    pub command: String,

    /// Captured output bytes. May be truncated; use `output_lossy` for a
    /// `String` rendering that handles invalid UTF-8.
    pub output: Vec<u8>,

    /// Total bytes received before any truncation.
    pub total_bytes: usize,

    /// Whether `output` was truncated to fit a size cap.
    pub truncated: bool,

    /// Working directory the command ran in.
    pub cwd: PathBuf,
}

impl BashNotificationBase {
    /// Lossy UTF-8 rendering of `output`. Invalid bytes become U+FFFD.
    pub fn output_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.output)
    }
}

/// Incremental output chunk streamed during a bash command. Sent
/// periodically while the process is still running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashOutputChunk {
    #[serde(flatten)]
    pub base: BashNotificationBase,
}

/// Sent when a bash process exits. Carries the exit status (or the killing
/// signal name when the process didn't exit normally).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashExecutionComplete {
    #[serde(flatten)]
    pub base: BashNotificationBase,

    /// `Some(code)` for a normal exit; `None` when the process was killed
    /// by a signal before reaching `exit(2)`.
    pub exit_code: Option<i32>,

    /// Signal that terminated the process (e.g. `"SIGKILL"`). `None` when
    /// the process exited normally.
    pub signal: Option<String>,
}

impl BashExecutionComplete {
    /// `true` when termination was triggered by a signal.
    pub fn was_signaled(&self) -> bool {
        self.signal.is_some()
    }
}

/// Sent when a bash command exceeded its configured timeout and was
/// killed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashExecutionTimeout {
    #[serde(flatten)]
    pub base: BashNotificationBase,

    /// Wall time the command ran for before being killed.
    pub elapsed: Duration,

    /// Configured timeout that was exceeded.
    pub timeout: Duration,
}

/// Sent when a foreground bash command was moved to the background. The
/// process keeps running; a downstream task monitor emits the eventual
/// [`BashExecutionComplete`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashExecutionBackgrounded {
    #[serde(flatten)]
    pub base: BashNotificationBase,

    /// File the full output stream is being written to. Background tasks
    /// always tee to disk so consumers can fetch the rest later.
    pub output_file: PathBuf,

    /// Background task registry id. Distinct from `base.tool_call_id`:
    /// the task id is generated when backgrounding, the tool call id was
    /// assigned when the originating tool was invoked.
    pub task_id: String,
}

/// Sent when a bash command failed to spawn. Distinct from
/// [`BashExecutionComplete`] with a non-zero `exit_code` because the
/// process never started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashExecutionFailed {
    pub tool_call_id: String,
    pub command: String,
    pub cwd: PathBuf,
    /// Error message describing the spawn / IO failure.
    pub error: String,
}

/// Emitted when a tool reads a file. Subscribers use this for state
/// snapshotting (rewind, audit) of accessed files.
///
/// **Reserved for a future `ToolNotification::FileRead` variant.** The
/// struct is kept in the public API so adapters can construct it ahead of
/// time, but it is not currently dispatched by any
/// [`ToolNotificationHandle`] helper. Adding the enum variant here is a
/// breaking change for exhaustive `match` consumers, so the variant is
/// deferred until a downstream crate has a real consumer wired up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRead {
    pub tool_call_id: String,
    /// Absolute filesystem path of the file that was read.
    pub absolute_path: PathBuf,
}

/// Emitted when a tool writes a file. Carries the full pre- and post-edit
/// content so subscribers can rewind without re-reading the disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWritten {
    pub tool_call_id: String,
    /// Absolute filesystem path of the file that was written.
    pub absolute_path: PathBuf,
    /// Full file content after the write.
    pub content: String,
    /// Full file content before the write. `None` for a fresh file.
    pub previous_content: Option<String>,
    /// Whether the write created a new file.
    pub is_new_file: bool,
}

/// Sent when the agent transitions into plan mode. Subscribers use this to
/// enforce the read-only constraint and switch UI affordances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanModeEntered {
    pub tool_call_id: String,
}

/// Sent when the agent transitions out of plan mode. Carries the plan
/// document so subscribers can present it for approval without an extra
/// file read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanModeExited {
    pub tool_call_id: String,
    /// Plan content as captured at exit time. `None` when the plan file
    /// did not exist or was empty.
    pub plan_content: Option<String>,
    /// Path the plan file lives at.
    pub plan_file_path: String,
}

/// Sent when the agent issues a structured question to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestionAsked {
    pub tool_call_id: String,
    /// Serialised question payload. Subscribers render it directly; the
    /// runtime does not introspect its shape.
    pub questions_json: serde_json::Value,
}

/// LSP server is being spawned and is waiting for the initialise
/// handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspServerStarting {
    pub server_name: String,
    pub command: String,
}

/// LSP server completed initialisation and is ready to serve requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspServerReady {
    pub server_name: String,
}

/// LSP server process died unexpectedly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspServerCrashed {
    pub server_name: String,
}

/// LSP server is being retried after a crash. Carries the retry attempt
/// count and computed backoff so subscribers can render progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspServerRetrying {
    pub server_name: String,
    pub attempt: u32,
    pub max_restarts: u32,
    pub backoff_ms: u64,
}

/// LSP server is permanently dead. Either init failed (`attempts == 0`)
/// or the configured retry budget was exhausted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspServerFailed {
    pub server_name: String,
    pub error: String,
    /// `0` for init failure, `> 0` when the retry budget was exhausted.
    pub attempts: u32,
}

/// Sent when a scheduled task fired and its prompt should be executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTaskFired {
    pub task_id: String,
    pub prompt: String,
    pub human_schedule: String,
    /// RFC 3339 timestamp of the next fire, when the task is recurring.
    pub next_fire_at: Option<String>,
}

/// Sent when a scheduled task is removed (deleted, expired, or one-shot
/// completed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTaskRemoved {
    pub task_id: String,
}

/// Sent when a scheduled task is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTaskCreated {
    pub task_id: String,
    pub prompt: String,
    pub human_schedule: String,
    /// RFC 3339 timestamp of the upcoming first fire.
    pub next_fire_at: Option<String>,
}

/// Streaming event from a Monitor tool background process. Each event is
/// already XML-wrapped for direct injection into the conversation; the
/// raw text is preserved for plain-text consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorEvent {
    pub task_id: String,
    pub description: String,
    /// XML-wrapped event text, ready for conversation injection.
    pub event_text: String,
    /// Raw text without XML wrapping.
    pub raw_text: String,
}

/// Snapshot of a background task's state. Identical shape to the EchoAgent
/// Build `TaskSnapshot` so subscribers can decode without per-source
/// adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub task_id: String,
    /// Actual command that was executed (may be wrapped by an isolation
    /// harness).
    pub command: String,
    /// Original user-provided command before isolation wrapping. When
    /// present, model- and user-facing surfaces should prefer it over
    /// `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_command: Option<String>,
    pub cwd: String,
    pub start_time: SystemTime,
    pub end_time: Option<SystemTime>,
    pub output: String,
    pub output_file: PathBuf,
    pub truncated: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub completed: bool,
    /// Distinguishes monitor tasks from regular bash tasks.
    #[serde(default)]
    pub kind: TaskKind,
    /// Total bytes the task has written, when the source tracks it.
    #[serde(default)]
    pub output_total_bytes: usize,
}

impl TaskSnapshot {
    /// Wall-time duration in seconds. Falls back to `now` for tasks that
    /// haven't completed.
    pub fn duration_secs(&self) -> f64 {
        let end = self.end_time.unwrap_or_else(SystemTime::now);
        end.duration_since(self.start_time)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}

/// Distinguishes background-task kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Regular bash command.
    #[default]
    Bash,
    /// Monitor tool — streams stdout events with rate limiting.
    Monitor,
}

/// A typed notification a tool emits during or after execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolNotification {
    BashOutputChunk(BashOutputChunk),
    BashExecutionComplete(BashExecutionComplete),
    BashExecutionTimeout(BashExecutionTimeout),
    BashExecutionBackgrounded(BashExecutionBackgrounded),
    BashExecutionFailed(BashExecutionFailed),
    FileWritten(FileWritten),
    TaskCompleted(TaskSnapshot),
    PlanModeEntered(PlanModeEntered),
    PlanModeExited(PlanModeExited),
    UserQuestionAsked(UserQuestionAsked),
    LspServerStarting(LspServerStarting),
    LspServerReady(LspServerReady),
    LspServerCrashed(LspServerCrashed),
    LspServerRetrying(LspServerRetrying),
    LspServerFailed(LspServerFailed),
    ScheduledTaskFired(ScheduledTaskFired),
    ScheduledTaskRemoved(ScheduledTaskRemoved),
    ScheduledTaskCreated(ScheduledTaskCreated),
    MonitorEvent(MonitorEvent),
}

impl ToolNotification {
    /// Stable `PascalCase` name of the active variant. Mirrors the serde
    /// `tag = "type"` discriminator used on the wire.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::BashOutputChunk(_) => "BashOutputChunk",
            Self::BashExecutionComplete(_) => "BashExecutionComplete",
            Self::BashExecutionTimeout(_) => "BashExecutionTimeout",
            Self::BashExecutionBackgrounded(_) => "BashExecutionBackgrounded",
            Self::BashExecutionFailed(_) => "BashExecutionFailed",
            Self::FileWritten(_) => "FileWritten",
            Self::TaskCompleted(_) => "TaskCompleted",
            Self::PlanModeEntered(_) => "PlanModeEntered",
            Self::PlanModeExited(_) => "PlanModeExited",
            Self::UserQuestionAsked(_) => "UserQuestionAsked",
            Self::LspServerStarting(_) => "LspServerStarting",
            Self::LspServerReady(_) => "LspServerReady",
            Self::LspServerCrashed(_) => "LspServerCrashed",
            Self::LspServerRetrying(_) => "LspServerRetrying",
            Self::LspServerFailed(_) => "LspServerFailed",
            Self::ScheduledTaskFired(_) => "ScheduledTaskFired",
            Self::ScheduledTaskRemoved(_) => "ScheduledTaskRemoved",
            Self::ScheduledTaskCreated(_) => "ScheduledTaskCreated",
            Self::MonitorEvent(_) => "MonitorEvent",
        }
    }
}

/// Cloneable handle for emitting [`ToolNotification`]s.
///
/// Built on `futures::channel::mpsc::UnboundedSender` so the sender side
/// is runtime-neutral — the trait crate does not pin tokio (or any other
/// executor) on its callers. Sends are non-blocking and best-effort:
/// errors (a closed receiver) are silently dropped, matching the
/// established convention for fire-and-forget notification streams.
#[derive(Clone)]
pub struct ToolNotificationHandle {
    sender: mpsc::UnboundedSender<ToolNotification>,
}

impl ToolNotificationHandle {
    /// Wrap a sender obtained elsewhere.
    pub fn new(sender: mpsc::UnboundedSender<ToolNotification>) -> Self {
        Self { sender }
    }

    /// Alias for [`Self::new`]. Tests and consumers that own the receiver
    /// half use this for symmetry.
    pub fn from_sender(sender: mpsc::UnboundedSender<ToolNotification>) -> Self {
        Self { sender }
    }

    /// Build both halves of a fresh channel and return them paired.
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<ToolNotification>) {
        let (sender, receiver) = mpsc::unbounded();
        (Self { sender }, receiver)
    }

    /// Build a handle whose sends are silently dropped. Use for callers
    /// that don't care about notifications (smoke tests, dry-run
    /// utilities). NOT a sensible default for production paths — the
    /// silent-drop behaviour makes notification bugs invisible.
    pub fn noop() -> Self {
        let (sender, _receiver) = mpsc::unbounded();
        Self { sender }
    }

    /// Send a fully-built notification. Errors are deliberately swallowed;
    /// notifications are best-effort.
    pub fn send(&self, notification: ToolNotification) {
        let _ = self.sender.unbounded_send(notification);
    }

    /// Send a [`ToolNotification::BashOutputChunk`]: an incremental
    /// stdout/stderr chunk while a bash command is still running.
    pub fn send_bash_output_chunk(&self, chunk: BashOutputChunk) {
        self.send(ToolNotification::BashOutputChunk(chunk));
    }

    /// Send a [`ToolNotification::BashExecutionComplete`]: a bash command
    /// exited (normally or via signal).
    pub fn send_bash_complete(&self, complete: BashExecutionComplete) {
        self.send(ToolNotification::BashExecutionComplete(complete));
    }

    /// Send a [`ToolNotification::BashExecutionTimeout`]: a bash command
    /// exceeded its configured timeout and was killed.
    pub fn send_bash_timeout(&self, timeout: BashExecutionTimeout) {
        self.send(ToolNotification::BashExecutionTimeout(timeout));
    }

    /// Send a [`ToolNotification::BashExecutionBackgrounded`]: a
    /// foreground bash command was moved to the background.
    pub fn send_bash_backgrounded(&self, backgrounded: BashExecutionBackgrounded) {
        self.send(ToolNotification::BashExecutionBackgrounded(backgrounded));
    }

    /// Send a [`ToolNotification::BashExecutionFailed`]: a bash command
    /// could not be spawned.
    pub fn send_bash_failed(&self, failed: BashExecutionFailed) {
        self.send(ToolNotification::BashExecutionFailed(failed));
    }

    /// Send a [`ToolNotification::FileWritten`]: a tool wrote to a file
    /// on disk.
    pub fn send_file_written(&self, written: FileWritten) {
        self.send(ToolNotification::FileWritten(written));
    }

    /// Send a [`ToolNotification::TaskCompleted`]: a background task
    /// transitioned to a terminal state.
    pub fn send_task_complete(&self, task_completed: TaskSnapshot) {
        self.send(ToolNotification::TaskCompleted(task_completed));
    }

    /// Send a [`ToolNotification::PlanModeEntered`]: the agent
    /// transitioned into plan mode.
    pub fn send_plan_mode_entered(&self, entered: PlanModeEntered) {
        self.send(ToolNotification::PlanModeEntered(entered));
    }

    /// Send a [`ToolNotification::PlanModeExited`]: the agent transitioned
    /// out of plan mode and the captured plan is attached.
    pub fn send_plan_mode_exited(&self, exited: PlanModeExited) {
        self.send(ToolNotification::PlanModeExited(exited));
    }

    /// Send a [`ToolNotification::UserQuestionAsked`]: the agent issued a
    /// structured question payload to the user.
    pub fn send_user_question_asked(&self, asked: UserQuestionAsked) {
        self.send(ToolNotification::UserQuestionAsked(asked));
    }

    /// Send a [`ToolNotification::LspServerStarting`]: an LSP server is
    /// being spawned.
    pub fn send_lsp_starting(&self, starting: LspServerStarting) {
        self.send(ToolNotification::LspServerStarting(starting));
    }

    /// Send a [`ToolNotification::LspServerReady`]: an LSP server
    /// finished its initialise handshake.
    pub fn send_lsp_ready(&self, ready: LspServerReady) {
        self.send(ToolNotification::LspServerReady(ready));
    }

    /// Send a [`ToolNotification::LspServerCrashed`]: an LSP server
    /// process died unexpectedly.
    pub fn send_lsp_crashed(&self, crashed: LspServerCrashed) {
        self.send(ToolNotification::LspServerCrashed(crashed));
    }

    /// Send a [`ToolNotification::LspServerRetrying`]: an LSP server is
    /// being restarted after a crash.
    pub fn send_lsp_retrying(&self, retrying: LspServerRetrying) {
        self.send(ToolNotification::LspServerRetrying(retrying));
    }

    /// Send a [`ToolNotification::LspServerFailed`]: an LSP server is
    /// permanently dead (init failure or retry budget exhausted).
    pub fn send_lsp_failed(&self, failed: LspServerFailed) {
        self.send(ToolNotification::LspServerFailed(failed));
    }

    /// Send a [`ToolNotification::ScheduledTaskFired`]: a recurring or
    /// one-shot scheduled task fired and its prompt should be executed.
    pub fn send_scheduled_task_fired(&self, fired: ScheduledTaskFired) {
        self.send(ToolNotification::ScheduledTaskFired(fired));
    }

    /// Send a [`ToolNotification::ScheduledTaskRemoved`]: a scheduled task
    /// was deleted, expired, or a one-shot variant completed.
    pub fn send_scheduled_task_removed(&self, removed: ScheduledTaskRemoved) {
        self.send(ToolNotification::ScheduledTaskRemoved(removed));
    }

    /// Send a [`ToolNotification::ScheduledTaskCreated`]: a new scheduled
    /// task was registered and should appear in subscriber views.
    pub fn send_scheduled_task_created(&self, created: ScheduledTaskCreated) {
        self.send(ToolNotification::ScheduledTaskCreated(created));
    }

    /// Send a [`ToolNotification::MonitorEvent`]: a streaming event from
    /// a Monitor background process, ready for conversation injection.
    pub fn send_monitor_event(&self, event: MonitorEvent) {
        self.send(ToolNotification::MonitorEvent(event));
    }
}
