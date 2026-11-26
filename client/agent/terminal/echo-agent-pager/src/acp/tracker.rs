//! AcpUpdateTracker — converts ACP SessionUpdate events into scrollback mutations.
//!
//! This is a stateful streaming machine: it tracks which entries are currently
//! being streamed to (agent message, thinking) and which tool calls are pending.
//! Each `handle_update()` call processes one event and mutates the scrollback.
use crate::acp::meta::{NotificationMeta, user_message_chunk_meta, user_prompt_meta};
use crate::scrollback::block::RenderBlock;
use crate::scrollback::blocks::SessionEvent;
use crate::scrollback::blocks::tool::list_dir::ListDirToolCallBlock;
use crate::scrollback::blocks::tool::search::{
    SearchFileMatch, SearchInputMeta, SearchLineMatch, SearchOutputMode, SearchToolCallBlock,
};
use crate::scrollback::blocks::tool::{
    DiscoveredTool, EditHighlightPhase, EditToolCallBlock, ExecuteToolCallBlock,
    IntegrationSearchToolCallBlock, LineRange, MemorySearchToolCallBlock, OtherToolCallBlock,
    ReadMediaKind, ReadToolCallBlock, ToolCallBlock, UseToolCallBlock, WebFetchToolCallBlock,
    WebSearchToolCallBlock,
};
use crate::scrollback::entry::{EntryId, ScrollbackEntry};
use crate::scrollback::state::ScrollbackState;
use crate::scrollback::state::verb_group::verb_group_kind_changed;
use agent_client_protocol as acp;
use chrono::{DateTime, Local, TimeZone};
use echo_agent_tools::types::output::{BashOutput, ToolOutput};
use echo_agent_tools::types::output::{ReadFileOutput, SearchToolOutput, WebFetchOutput};
use echo_agent_tools::util::strip_redundant_session_cd;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::debug;
/// Convert a UTC millisecond timestamp to local time.
fn utc_ms_to_local(ms: i64) -> DateTime<Local> {
    chrono::Utc
        .timestamp_millis_opt(ms)
        .single()
        .map(|utc| utc.with_timezone(&Local))
        .unwrap_or_else(Local::now)
}
/// What the agent is currently doing within a turn.
///
/// Derived from the tracker's internal state by [`AcpUpdateTracker::activity()`].
/// Used by the turn status line widget to show context-appropriate indicators.
///
/// Note: `Idle` here means "the tracker has no in-flight work". The caller
/// should check `TurnState` to distinguish true idle (no turn) from waiting
/// (turn started, but no chunks received yet).
/// Why a turn is open but nothing is streaming right now.
///
/// Replaces the old single, opaque "Waiting…" placeholder: instead of treating
/// the absence of activity as one undifferentiated state, the turn-status line
/// names *what* the agent is blocked on. Resolved partly by the tracker (the
/// blocking tool waits it suppresses — see [`AcpUpdateTracker::activity`]) and
/// partly at the view boundary (`Model`/`Subagent`, which need turn-state and
/// the subagent registry the tracker doesn't own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitingReason {
    /// Waiting for the model to (re)start streaming — the first token after the
    /// prompt is sent, or the gap after a tool completes before the next
    /// inference step begins.
    Model,
    /// Blocked on a running foreground subagent (`task` / `spawn_subagent`).
    /// `display` is the fully composed, pre-budgeted spinner phrase
    /// (`Subagent (<desc>): <activity>` / `<N> subagents: …`) — unlike
    /// `TaskOutput.subject`, which holds a bare subject that `label()`
    /// decorates. View-resolved; the tracker always leaves it `None`.
    Subagent { display: Option<String> },
    /// Blocked polling/awaiting a background task's output
    /// (`get_command_or_subagent_output` / `get_task_output`).
    ///
    /// `task_ids` come from the tool's `raw_input` (empty until it arrives).
    /// `subject` is an optional display name (description preferred, else
    /// command) filled in by the view from live task state — the tracker
    /// itself always leaves it `None`.
    TaskOutput {
        task_ids: Vec<String>,
        subject: Option<String>,
        /// True when the call blocks (`timeout_ms > 0` in raw_input); an
        /// instant poll (0/missing) can't be shortened by interjecting.
        /// Defaults to false until raw_input arrives.
        waits: bool,
    },
    /// Blocked until one or more background tasks finish
    /// (`wait_commands_or_subagents` / `wait_tasks`).
    TasksComplete,
    /// Explicit sleep / await (`Await` / `Sleep …`).
    Sleep,
}
/// Max chars for wait/tool *description* subjects in status UI (matches
/// tool-title truncation in `format_activity_label`).
pub const MAX_ACTIVITY_SUBJECT_CHARS: usize = 40;
/// First non-empty trimmed line, clamped to [`MAX_ACTIVITY_SUBJECT_CHARS`].
pub fn clamp_activity_subject(s: &str) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_else(|| s.trim());
    if line.chars().count() <= MAX_ACTIVITY_SUBJECT_CHARS {
        line.to_string()
    } else {
        line.chars().take(MAX_ACTIVITY_SUBJECT_CHARS).collect()
    }
}
/// Shared in-progress subject label (clamped description/command) used by
/// turn-status, title bar, and dashboard/subagent activity columns.
///
/// Renders as `{subject}…` — no "Waiting for" prefix or quotes — so a
/// description like `Wait 5 seconds` reads cleanly next to the spinner.
pub fn format_waiting_for_subject(subject: &str) -> String {
    let clamped = clamp_activity_subject(subject);
    if clamped.is_empty() {
        "Waiting on task output…".to_string()
    } else {
        format!("{clamped}…")
    }
}
impl WaitingReason {
    /// Unit constructor for a task-output wait with no known ids/subject yet.
    /// A known-blocking task-output wait (the only kind `activity()` shows).
    pub fn task_output() -> Self {
        Self::TaskOutput {
            task_ids: Vec::new(),
            subject: None,
            waits: true,
        }
    }
    pub fn subagent() -> Self {
        Self::Subagent { display: None }
    }
    /// User-facing spinner label.
    pub fn label(&self) -> String {
        match self {
            Self::Model => "Waiting for response…".to_string(),
            Self::Subagent { display } => match display.as_deref().map(clamp_activity_subject) {
                Some(display) if !display.is_empty() => format!("{display}…"),
                _ => "Waiting on subagent…".to_string(),
            },
            Self::TaskOutput {
                subject: Some(subject),
                ..
            } => format_waiting_for_subject(subject),
            Self::TaskOutput { .. } => "Waiting on task output…".to_string(),
            Self::TasksComplete => "Waiting on tasks…".to_string(),
            Self::Sleep => "Sleeping…".to_string(),
        }
    }
    /// Short, stable snake_case label for telemetry / phase-transition logs.
    pub fn as_telemetry_label(&self) -> &'static str {
        match self {
            Self::Model => "waiting_model",
            Self::Subagent { .. } => "waiting_subagent",
            Self::TaskOutput { .. } => "waiting_task_output",
            Self::TasksComplete => "waiting_tasks_complete",
            Self::Sleep => "waiting_sleep",
        }
    }
}
/// A suppressed blocking tool's wait, tagged with the stream it was registered
/// under (drives `drop_stale_blocking_waits`).
#[derive(Debug, Clone)]
struct BlockingWait {
    reason: WaitingReason,
    stream_start_ms: Option<i64>,
}
/// Deltas stream continuously during a live write — silence this long means the stream is dead.
pub(crate) const WRITING_DELTA_STALE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(10);
/// The model is streaming tool-call arguments (EchoAgent `tool_call_delta_chunk`),
/// which reach no scrollback until the canonical `ToolCall` lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritingToolCall {
    /// `None` until a chunk carries the name (only the first per-tool one does).
    pub tool_name: Option<String>,
    /// 1-based position within the sample's tool calls.
    pub ordinal: std::num::NonZeroU32,
}
impl WritingToolCall {
    /// User-facing spinner label.
    pub fn label(&self) -> String {
        let ordinal = match self.ordinal.get() {
            1 => String::new(),
            n => format!(" ({n})"),
        };
        match self.tool_name.as_deref() {
            Some(name) if echo_agent_tools::is_task_tool_id(name) => {
                format!("Writing subagent prompt{ordinal}…")
            }
            Some(echo_agent_tools::USE_TOOL_NAME) => {
                format!("Preparing MCP tool{ordinal}…")
            }
            Some(echo_agent_tools::SEARCH_TOOL_NAME) => {
                format!("Searching MCP tools{ordinal}…")
            }
            Some(name) => {
                use echo_agent_tools::types::tool::ToolKind;
                let copy =
                    echo_agent_tools::tool_taxonomy::writing_tool_kind(name).and_then(|kind| {
                        match kind {
                            ToolKind::Write => Some("Writing file"),
                            ToolKind::Edit => Some("Writing edit"),
                            ToolKind::Execute => Some("Writing command"),
                            ToolKind::Plan => Some("Updating todo list"),
                            ToolKind::Workflow => Some("Writing workflow"),
                            ToolKind::ImageGen => Some("Writing image prompt"),
                            ToolKind::ImageToVideo | ToolKind::ReferenceToVideo => {
                                Some("Writing video prompt")
                            }
                            ToolKind::AskUser => Some("Preparing question"),
                            _ => None,
                        }
                    });
                match copy {
                    Some(copy) => format!("{copy}{ordinal}…"),
                    None => {
                        let name =
                            echo_agent_workspace::permission::mcp_pretty_name_if_qualified(name);
                        format!("Preparing {}{ordinal}…", clamp_activity_subject(&name))
                    }
                }
            }
            None => format!("Preparing tool call{ordinal}…"),
        }
    }
}
/// Cap on remembered per-index tool names per sample (model-driven input).
const MAX_WRITING_TOOL_NAMES: usize = 64;
/// `strings`-greppable marker proving a binary carries this fix (kept by `#[used]`).
#[used]
static PAGER_IMPL_WAIT_STATUS_MIDTURN: &str = "PAGER_IMPL_wait_status_midturn";
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnActivity {
    /// Agent is streaming thinking/chain-of-thought content.
    Thinking,
    /// Agent is streaming response text.
    Responding,
    /// A tool is executing.
    ToolRunning {
        /// Tool title (e.g., command name, file path). Used for `Run …`
        /// when no human description is available.
        title: String,
        /// Optional human description from tool input (e.g. bash
        /// `description`). Prefer this over `Run <command>` when set
        /// (renders as `{desc}…`).
        description: Option<String>,
    },
    /// Auto-compaction in progress (mid-turn, agent-initiated).
    AutoCompacting,
    /// A retry is in progress (transient error, empty response, etc.).
    Retrying {
        /// Current retry attempt number (1-indexed).
        attempt: u32,
        /// Maximum number of retries allowed.
        max_retries: u32,
        /// Human-readable reason for the retry.
        reason: String,
    },
    /// The model is streaming tool-call arguments; see [`WritingToolCall`].
    WritingToolCall(WritingToolCall),
    /// Turn is open but nothing is streaming; `reason` says what we're waiting
    /// on. Replaces the implicit "no activity == generic Waiting…" placeholder.
    Waiting(WaitingReason),
}
/// A spinner phase's identity: the activity discriminant plus only the payload
/// that names a different unit of work. Payload the view or late-arriving
/// input fills in mid-phase (wait subjects/ids, writing name/ordinal) is
/// display churn, not a new phase — a long wait or write stays one timed
/// phase. Exhaustive on both enums so a new variant must decide its identity
/// here instead of silently regaining the per-frame timer reset.
#[derive(PartialEq)]
enum PhaseKey<'a> {
    Thinking,
    Responding,
    /// A different tool call is a new phase; description churn is not.
    ToolRunning(&'a str),
    AutoCompacting,
    /// Each retry attempt (or new cause) restarts the phase timer.
    Retrying {
        attempt: u32,
        reason: &'a str,
    },
    WritingToolCall,
    Waiting(&'static str),
}
fn phase_key(activity: &TurnActivity) -> PhaseKey<'_> {
    match activity {
        TurnActivity::Thinking => PhaseKey::Thinking,
        TurnActivity::Responding => PhaseKey::Responding,
        TurnActivity::ToolRunning { title, .. } => PhaseKey::ToolRunning(title),
        TurnActivity::AutoCompacting => PhaseKey::AutoCompacting,
        TurnActivity::Retrying {
            attempt, reason, ..
        } => PhaseKey::Retrying {
            attempt: *attempt,
            reason,
        },
        TurnActivity::WritingToolCall(_) => PhaseKey::WritingToolCall,
        TurnActivity::Waiting(reason) => PhaseKey::Waiting(reason.as_telemetry_label()),
    }
}
/// Whether `prev` → `next` starts a new spinner phase — see [`PhaseKey`].
pub(crate) fn is_phase_transition(
    prev: Option<&TurnActivity>,
    next: Option<&TurnActivity>,
) -> bool {
    prev.map(phase_key) != next.map(phase_key)
}
impl TurnActivity {
    /// Short, stable label for telemetry / profiling logs.
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::Thinking => "thinking",
            Self::Responding => "responding",
            Self::ToolRunning { .. } => "tool_running",
            Self::AutoCompacting => "compacting",
            Self::Retrying { .. } => "retrying",
            Self::WritingToolCall(_) => "writing_tool_call",
            Self::Waiting(reason) => reason.as_telemetry_label(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct PendingCompaction {
    pub tokens_before: Option<u64>,
    pub estimate_after: u64,
    pub elapsed_ms: Option<i64>,
    pub last_used: Option<u64>,
}
/// Tracks in-flight streaming state for one agent's turn.
///
/// Converts ACP `SessionUpdate` variants into scrollback entry mutations.
/// Does nothing else — no UI, no networking, just data transformation.
#[derive(Debug, Default)]
pub struct AcpUpdateTracker {
    /// Entry currently receiving AgentMessageChunk deltas.
    /// None between turns or before first message chunk.
    current_agent_msg: Option<EntryId>,
    /// Entry currently receiving AgentThoughtChunk deltas.
    /// None when agent isn't thinking.
    current_thinking: Option<EntryId>,
    /// Tool calls in flight, keyed by ACP tool call ID string.
    /// Stores the base ToolCall for field merging with ToolCallUpdate.
    pending_tools: HashMap<String, PendingTool>,
    /// ToolCallUpdates that arrived before their ToolCall (race condition).
    /// When the ToolCall arrives, we merge and create the entry immediately
    /// as completed.
    orphan_updates: HashMap<String, acp::ToolCallUpdate>,
    /// Last computed thinking elapsed (ms) from server timestamps.
    /// Updated on every thought chunk as `agentTimestampMs - streamStartMs`.
    /// Frozen when thinking ends (passed to `finish_running_with_time`).
    last_thinking_elapsed_ms: Option<i64>,
    /// When true, the next UserMessageChunk will be silently ignored
    /// because we already pushed the user prompt entry directly from
    /// `dispatch_send_prompt`. Reset after one skip.
    skip_next_user_echo: bool,
    /// When true, the next UserMessageChunk is a skill body that follows
    /// a skill metadata chunk. It should be silently absorbed so the
    /// raw skill instructions don't appear in scrollback.
    skip_next_skill_body: bool,
    /// Tool call IDs suppressed from scrollback (e.g. TodoWrite).
    /// Their ToolCallUpdate counterparts are silently dropped too.
    suppressed_tools: std::collections::HashSet<String>,
    /// Suppressed-but-blocking tool calls, keyed by tool-call ID → the reason
    /// the turn is waiting. These tools (`get_command_or_subagent_output`,
    /// `wait_tasks`, `Sleep`, …) are kept out of `pending_tools` (so they never
    /// hit scrollback) but the turn *is* blocked on them — without this the
    /// spinner falls back to a generic "Waiting…". Populated in
    /// `handle_tool_call`, cleared on the suppressed tool's completion update
    /// and in `finish_turn`.
    blocking_waits: std::collections::HashMap<String, BlockingWait>,
    /// Task tool `run_in_background` flags, keyed by `task_id` (subagent_id).
    /// Populated when a task tool call is detected (variant == "Task"),
    /// consumed by the acp_handler when `SubagentSpawned` arrives.
    pub(crate) task_tool_background: std::collections::HashMap<String, bool>,
    /// Tool call IDs marked as background (`is_background=true`).
    ///
    /// First-detection (no scrollback entry yet): defers entry creation until
    /// `echo.agent/task_backgrounded` creates a `BgTask` block.
    /// Late-detection (Execute block already exists): suppresses further output
    /// streaming; the existing block is demoted by `handle_task_backgrounded`.
    ///
    /// Value is the optional description from `raw_input.description`.
    pub(crate) bg_deferred_tools: std::collections::HashMap<String, Option<String>>,
    /// Last seen `stream_start_ms` from notification meta.
    /// When this changes, a new LLM streaming response has started — we
    /// finish any in-flight thinking/agent-message entries so the next
    /// chunks create fresh ones instead of appending to stale entries.
    last_stream_start_ms: Option<i64>,
    /// Monotonic count of live parent-agent updates that changed scrollback.
    agent_output_epoch: u64,
    epoch_at_last_finish: u64,
    /// Session project cwd for display-only redundant-`cd` stripping.
    /// Set from [`AgentSession::cwd`]; not used for execution.
    session_cwd: Option<PathBuf>,
    /// Compaction-related activity override.
    /// Set by `set_compaction_activity()` from ExtNotification events,
    /// cleared by `finish_turn()`.
    compaction_activity: Option<TurnActivity>,
    pending_compaction: Option<PendingCompaction>,
    /// Retry-related activity override.
    /// Set by `set_retry_activity()` from ExtNotification `RetryState::Retrying`,
    /// auto-cleared when normal streaming data resumes (in `handle_update` and
    /// `note_tool_call_arguments_delta`) and on `finish_turn()`.
    retry_activity: Option<TurnActivity>,
    /// Set per `ToolCallDeltaChunk` (streaming-only, never persisted — cannot
    /// replay); cleared by the canonical `ToolCall` / text / thought chunks
    /// (not `ToolCallUpdate` — see `handle_update`) and by `finish_turn()`.
    /// The instant is the last delta's arrival; expiry lives in the accessors
    /// ([`Self::fresh_writing_tool_call`] / [`Self::has_stale_tool_call_write`]).
    writing_tool_call: Option<(WritingToolCall, std::time::Instant)>,
    /// Per-`tool_index` names so interleaved deltas restore a call's name on
    /// switch-back; `None` marks an index observed before its name arrived
    /// (it still ranks for ordinals). Cleared together with `writing_tool_call`.
    writing_tool_names: HashMap<u32, Option<String>>,
    /// Pending ACP commands from the most recent `AvailableCommandsUpdate`.
    /// Consumed by the caller via `take_pending_acp_commands()`. The caller
    /// is responsible for copying to `AgentSession.available_commands` and
    /// bumping `available_commands_generation`.
    pending_acp_commands: Option<Vec<acp::AvailableCommand>>,
    /// Pending agent toolset from the most recent `AvailableCommandsUpdate.meta`.
    /// Format on the wire: `{"tools": ["read_file", ...]}`.
    /// `Some(_)` only if the shell included a tools list this round.
    /// Consumed by the caller via `take_pending_acp_tools()`.
    ///
    /// Invariant: drained synchronously by
    /// `acp_handler::handle_session_notification` immediately after each
    /// `handle_update` call -- so this field never accumulates across
    /// notifications. A meta-less follow-up update intentionally
    /// preserves the previous `Some` (see the assignment in
    /// `handle_update`) so a partial replay can't silently regress the
    /// registry to the unknown-toolset state.
    pending_acp_tools: Option<Vec<String>>,
    /// Live Edit completions awaiting full-file HL (drained via [`Self::take_pending_edit_hl`]).
    pending_edit_hl: Vec<EntryId>,
}
/// A tool call that's been started but not yet completed.
#[derive(Debug)]
struct PendingTool {
    /// Scrollback entry ID, or None if the entry hasn't been created yet.
    /// The entry is deferred until we receive the real tool kind from the
    /// first in-progress update. The initial ToolCall message often has
    /// kind=Other with no useful metadata — creating an entry from it
    /// would show a wrong block type briefly before the real kind arrives.
    entry_id: Option<EntryId>,
    base: acp::ToolCall,
    /// Streaming UTF-8 decoder for incremental bash output deltas.
    utf8_decoder: Utf8Decoder,
    /// Stashed `started_at` from eager creation. The eagerly-created block
    /// is `ToolCallBlock::Other`; when the refinement arrives with the real
    /// kind, `transfer_timing_from` can't cross variant boundaries
    /// (Other → Search, etc.) and would silently drop the timing. This
    /// field preserves the instant so `set_started_at` can apply it to
    /// whatever variant the refined block becomes.
    started_at: Option<std::time::Instant>,
}
/// Streaming UTF-8 decoder for incremental byte deltas.
///
/// When output is split at arbitrary byte offsets, a multi-byte UTF-8
/// character can land across two deltas. Without buffering, both halves
/// would be replaced with U+FFFD by `from_utf8_lossy`, permanently
/// corrupting the character.
///
/// This decoder buffers trailing incomplete bytes from each delta and
/// prepends them to the next one. Only genuinely invalid sequences
/// (not just incomplete ones at the end) produce U+FFFD.
#[derive(Debug, Default)]
struct Utf8Decoder {
    /// Trailing bytes from the last delta that didn't form a complete
    /// UTF-8 character. At most 3 bytes (max continuation length).
    buffer: Vec<u8>,
    /// Reusable output buffer — avoids allocating a new String per delta.
    /// Cleared on each `decode()` call, grows to high-water mark and stays.
    decoded: String,
}
impl Utf8Decoder {
    /// Feed raw bytes and return the decoded string slice.
    ///
    /// Any trailing incomplete UTF-8 sequence is held back in the internal
    /// buffer and will be prepended to the next `decode()` call. Genuinely
    /// invalid byte sequences produce U+FFFD.
    ///
    /// The returned `&str` is valid until the next `decode()` call.
    fn decode(&mut self, piece: &[u8]) -> &str {
        self.decoded.clear();
        self.buffer.extend_from_slice(piece);
        let mut last_invalid_len = None;
        for chunk in self.buffer.utf8_chunks() {
            if let Some(prev) = last_invalid_len.replace(chunk.invalid().len())
                && prev > 0
            {
                self.decoded.push(char::REPLACEMENT_CHARACTER);
            }
            self.decoded.push_str(chunk.valid());
        }
        match last_invalid_len {
            Some(0) => self.buffer.clear(),
            Some(n) => {
                let keep_from = self.buffer.len() - n;
                self.buffer.drain(..keep_from);
            }
            None => self.buffer.clear(),
        }
        &self.decoded
    }
}
impl AcpUpdateTracker {
    pub fn new() -> Self {
        Self::default()
    }
    pub(crate) fn output_since_last_finish(&self) -> bool {
        self.agent_output_epoch != self.epoch_at_last_finish
    }
    /// Mark all output so far as accounted for without finishing the turn —
    /// for terminals that must be skipped while a client command owns the
    /// screen (a full `finish_turn` would flush mid-command state such as
    /// `pending_compaction`).
    pub(crate) fn snapshot_output_epoch(&mut self) {
        self.epoch_at_last_finish = self.agent_output_epoch;
    }
    fn bump_agent_output_epoch(&mut self) {
        self.agent_output_epoch = self.agent_output_epoch.wrapping_add(1);
    }
    /// Record session cwd used when stripping redundant `cd` prefixes in chrome.
    /// No-op when the path is already stored (avoids cloning on every update).
    pub fn set_session_cwd(&mut self, cwd: impl AsRef<Path>) {
        let cwd = cwd.as_ref();
        if self.session_cwd.as_deref() != Some(cwd) {
            self.session_cwd = Some(cwd.to_path_buf());
        }
    }
    /// Current activity within the turn, derived from in-flight state.
    ///
    /// Priority order (highest first):
    /// 1. External overrides: Retrying, AutoCompacting (from ExtNotification)
    /// 2. Known-blocking wait (task output / wait / sleep / foreground
    ///    subagent) — outranks Thinking, ToolRunning, and Responding.
    /// 3. WritingToolCall — outranks Thinking: the first delta means reasoning
    ///    ended (the thinking scrollback block stays open until the `ToolCall`).
    /// 4. Thinking (agent is in chain-of-thought)
    /// 5. ToolRunning (a tool call is pending / executing)
    /// 6. Responding (agent is streaming text)
    /// 7. None (nothing in-flight; the view turns this into Waiting(Model) or
    ///    Waiting(Subagent) while a turn is running)
    ///
    /// Retry and compaction states are set externally via
    /// `set_retry_activity()` / `set_compaction_activity()` since they
    /// come from ExtNotification, not from standard ACP SessionUpdate messages.
    ///
    /// When [`Self::session_cwd`] is set, execute activity titles omit a leading
    /// `cd <cwd> &&` / `;` that only restates the session working directory.
    pub fn activity(&self) -> Option<TurnActivity> {
        if self.retry_activity.is_some() {
            return self.retry_activity.clone();
        }
        if self.compaction_activity.is_some() {
            return self.compaction_activity.clone();
        }
        if let Some(waiting) = self.activity_known_blocking_wait() {
            return Some(waiting);
        }
        if let Some(writing) = self.fresh_writing_tool_call() {
            return Some(TurnActivity::WritingToolCall(writing.clone()));
        }
        if self.current_thinking.is_some() {
            return Some(TurnActivity::Thinking);
        }
        if let Some(tool) = self.pending_tools.values().next() {
            let description = tool
                .base
                .raw_input
                .as_ref()
                .and_then(|v| v.get("description"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(clamp_activity_subject);
            let title = tool
                .base
                .raw_input
                .as_ref()
                .and_then(|v| v.get("command"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| tool.base.title.clone());
            let title = peeled_if_changed(&title, self.session_cwd.as_deref()).unwrap_or(title);
            return Some(TurnActivity::ToolRunning { title, description });
        }
        if self.current_agent_msg.is_some() {
            return Some(TurnActivity::Responding);
        }
        None
    }
    /// Spinner activity for a suppressed blocking tool, or `None`. Instant
    /// task-output polls (`timeout_ms` 0/missing) are excluded.
    fn activity_known_blocking_wait(&self) -> Option<TurnActivity> {
        let reason = self.blocking_wait()?;
        if matches!(reason, WaitingReason::TaskOutput { waits: false, .. }) {
            return None;
        }
        Some(TurnActivity::Waiting(reason))
    }
    /// Highest-priority blocking-tool wait currently in flight, if any.
    ///
    /// `blocking_waits` is a map (non-deterministic iteration order), so
    /// collapse it to a single reason by a fixed priority. In practice at most
    /// one blocking tool runs at a time; the ordering only matters for the
    /// degenerate multi-tool case.
    fn blocking_wait(&self) -> Option<WaitingReason> {
        self.blocking_waits
            .values()
            .min_by_key(|w| match &w.reason {
                WaitingReason::TaskOutput { .. } => 0,
                WaitingReason::TasksComplete => 1,
                WaitingReason::Sleep => 2,
                WaitingReason::Subagent { .. } => 3,
                WaitingReason::Model => 4,
            })
            .map(|w| w.reason.clone())
    }
    /// Drop waits not registered under `current_stream` (stale earlier rounds,
    /// or an unknown `None` stream); co-batched same-stream waits survive.
    fn drop_stale_blocking_waits(&mut self, current_stream: Option<i64>) {
        self.blocking_waits
            .retain(|_, w| current_stream.is_some() && w.stream_start_ms == current_stream);
    }
    pub fn tool_title(&self, tool_call_id: &str) -> Option<&str> {
        self.pending_tools
            .get(tool_call_id)
            .map(|pending| pending.base.title.as_str())
    }
    /// Get the scrollback entry_id for a pending tool by tool_call_id.
    ///
    /// Used by demotion to find the execute block to swap.
    pub fn pending_tool_entry_id(&self, tool_call_id: &str) -> Option<EntryId> {
        self.pending_tools
            .get(tool_call_id)
            .and_then(|t| t.entry_id)
    }
    /// Remove a tool from pending_tools (for demotion swap).
    ///
    /// Called when an execute block is being swapped to a BgTask block.
    pub fn remove_pending_tool(&mut self, tool_call_id: &str) {
        self.pending_tools.remove(tool_call_id);
    }
    /// Get the tool_call_id of the currently running Execute tool, if any.
    ///
    /// Used by demotion (Ctrl+B) to know which tool to background.
    /// Returns None if no Execute tool is currently pending.
    pub fn running_execute_tool_call_id(&self) -> Option<&str> {
        self.pending_tools
            .iter()
            .find(|(_, tool)| tool.base.kind == acp::ToolKind::Execute && tool.entry_id.is_some())
            .map(|(id, _)| id.as_str())
    }
    /// Set a compaction-related activity override.
    ///
    /// Called by the ACP handler when `ExtNotification` compaction events
    /// arrive. Cleared automatically by `finish_turn()`.
    pub fn set_compaction_activity(&mut self, activity: Option<TurnActivity>) {
        self.compaction_activity = activity;
    }
    pub fn defer_compaction(
        &mut self,
        tokens_before: Option<u64>,
        estimate_after: u64,
        elapsed_ms: Option<i64>,
    ) {
        self.pending_compaction = Some(PendingCompaction {
            tokens_before,
            estimate_after,
            elapsed_ms,
            last_used: None,
        });
    }
    pub fn note_context_used(&mut self, used: u64) {
        if let Some(pending) = self.pending_compaction.as_mut() {
            pending.last_used = Some(used);
        }
    }
    /// Set a retry-related activity override.
    ///
    /// Called by the ACP handler when `ExtNotification` `RetryState::Retrying`
    /// arrives. Auto-cleared when normal streaming data resumes (in
    /// `handle_update` and `note_tool_call_arguments_delta`) and on
    /// `finish_turn()`.
    pub fn set_retry_activity(&mut self, activity: Option<TurnActivity>) {
        self.retry_activity = activity;
    }
    /// Record a `ToolCallDeltaChunk`; returns `true` only when the visible
    /// label changed (continuation deltas need no redraw).
    pub fn note_tool_call_arguments_delta(&mut self, name: Option<&str>, tool_index: u32) -> bool {
        let now = std::time::Instant::now();
        let retry_cleared = self.retry_activity.take().is_some();
        let expired = self.has_stale_tool_call_write();
        if self.writing_tool_names.len() < MAX_WRITING_TOOL_NAMES
            || self.writing_tool_names.contains_key(&tool_index)
        {
            let entry = self.writing_tool_names.entry(tool_index).or_insert(None);
            if let Some(name) = name {
                *entry = Some(name.to_string());
            }
        }
        let observed_before = self
            .writing_tool_names
            .keys()
            .filter(|&&i| i < tool_index)
            .count() as u32;
        let ordinal = std::num::NonZeroU32::new(observed_before.saturating_add(1))
            .unwrap_or(std::num::NonZeroU32::MIN);
        let next = WritingToolCall {
            tool_name: self.writing_tool_names.get(&tool_index).cloned().flatten(),
            ordinal,
        };
        let changed =
            expired || self.writing_tool_call.as_ref().map(|(writing, _)| writing) != Some(&next);
        self.writing_tool_call = Some((next, now));
        retry_cleared || changed
    }
    /// The in-flight write while its deltas are fresh; a stream silent past
    /// [`WRITING_DELTA_STALE_AFTER`] is treated as no longer writing.
    fn fresh_writing_tool_call(&self) -> Option<&WritingToolCall> {
        self.writing_tool_call
            .as_ref()
            .filter(|(_, at)| at.elapsed() < WRITING_DELTA_STALE_AFTER)
            .map(|(writing, _)| writing)
    }
    /// A write whose delta stream went silent past the cutoff — positive
    /// evidence of a dead stream (canonical output would have cleared it).
    pub(crate) fn has_stale_tool_call_write(&self) -> bool {
        self.writing_tool_call
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= WRITING_DELTA_STALE_AFTER)
    }
    /// Backdate the write's delta stamp (staleness tests).
    #[cfg(test)]
    pub(crate) fn backdate_last_tool_call_delta(&mut self, age: std::time::Duration) {
        if let Some((_, at)) = &mut self.writing_tool_call {
            *at = std::time::Instant::now() - age;
        }
    }
    /// Take pending ACP commands, if any. Returns `None` if no update arrived
    /// since the last drain.
    ///
    /// The caller is the single drain site: it copies the commands to
    /// `AgentSession.available_commands` and bumps the generation counter.
    pub fn take_pending_acp_commands(&mut self) -> Option<Vec<acp::AvailableCommand>> {
        self.pending_acp_commands.take()
    }
    /// Take the agent's most recently advertised tool list, if any.
    ///
    /// Drained alongside `take_pending_acp_commands()` -- the same
    /// `AvailableCommandsUpdate` carries both. `None` means the shell
    /// didn't include a `meta.tools` field (older shell, or no update
    /// since last drain).
    pub fn take_pending_acp_tools(&mut self) -> Option<Vec<String>> {
        self.pending_acp_tools.take()
    }
    /// Drain Edit entry ids that need a background full-file HL job.
    pub fn take_pending_edit_hl(&mut self) -> Vec<EntryId> {
        std::mem::take(&mut self.pending_edit_hl)
    }
    /// Whether `block` is a successful Edit with hunks (worth a full-file HL job).
    fn edit_wants_file_hl(block: &RenderBlock) -> bool {
        matches!(
            block,
            RenderBlock::ToolCall(ToolCallBlock::Edit(edit))
                if edit.error.is_none() && !edit.hunks.is_empty()
        )
    }
    /// Stash `entry_id` for live successful Edits with hunks. Skips replay
    /// because a resume replays every historical edit at once — queueing them
    /// would thundering-herd N full-file jobs — and replayed edits' files may
    /// have changed on disk since, so the styles would not match the hunks.
    fn queue_edit_hl_if_needed(&mut self, entry_id: EntryId, block: &RenderBlock, is_replay: bool) {
        if !is_replay && Self::edit_wants_file_hl(block) {
            self.pending_edit_hl.push(entry_id);
        }
    }
    /// Push a completed tool block, queue its edit-HL upgrade if warranted, and
    /// clear the running state — the shared tail of every completed-tool path.
    /// Evaluates the predicate before `push_block` consumes the block, so the
    /// entry needs no re-fetch.
    ///
    /// The returned id may no longer be in the scrollback: a completed Edit
    /// can coalesce into an adjacent earlier Edit of the same file.
    fn finish_completed_tool(
        &mut self,
        block: RenderBlock,
        scrollback: &mut ScrollbackState,
        is_replay: bool,
    ) -> EntryId {
        let wants_hl = Self::edit_wants_file_hl(&block);
        let id = scrollback.push_block(block);
        if !is_replay && wants_hl {
            self.pending_edit_hl.push(id);
        }
        scrollback.finish_running(id);
        self.try_coalesce_edit(id, scrollback, is_replay);
        id
    }
    /// The Edit block of `entry` if it qualifies for coalescing with an
    /// adjacent same-file Edit: completed successfully with hunks, a
    /// trustworthy one-liner summary, and free of per-entry attachments a
    /// merge would misplace.
    fn coalescable_edit(entry: &ScrollbackEntry) -> Option<&EditToolCallBlock> {
        if entry.is_running || entry.is_pending_user_input || entry.hook_data.is_some() {
            return None;
        }
        let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &entry.block else {
            return None;
        };
        (edit.error.is_none() && !edit.hunks.is_empty() && !edit.summary_untrusted).then_some(edit)
    }
    /// Whether the completed Edit entries `earlier` and `later` target the
    /// same file and may merge into one block.
    fn edits_can_merge(
        &self,
        scrollback: &ScrollbackState,
        earlier: EntryId,
        later: EntryId,
    ) -> bool {
        if scrollback.is_committed(earlier) || scrollback.is_committed(later) {
            return false;
        }
        let (Some(a), Some(b)) = (
            scrollback
                .get_by_id(earlier)
                .and_then(Self::coalescable_edit),
            scrollback.get_by_id(later).and_then(Self::coalescable_edit),
        ) else {
            return false;
        };
        if a.prefix != b.prefix {
            return false;
        }
        let cwd = self.session_cwd.as_deref();
        let resolve = |p: &str| crate::render::tool_paths::resolve_tool_path_target(p, cwd);
        match (resolve(&a.path), resolve(&b.path)) {
            (Some(pa), Some(pb)) => pa == pb,
            (None, None) => a.path == b.path,
            _ => false,
        }
    }
    /// Coalesce the just-completed Edit at `entry_id` with strictly adjacent
    /// completed Edits of the same file, so back-to-back edits render as one
    /// block with a summed diffstat. The earlier entry always survives.
    ///
    /// Checks the previous neighbor (sequential completions) and the next one
    /// (parallel calls can complete out of push order, so the pair only
    /// becomes mergeable when the earlier call lands). Loops so runs of 3+
    /// collapse pairwise.
    ///
    /// Ingestion-time only: a later `collapsed_edit_blocks` flip never
    /// merges or unmerges rows that already landed.
    fn try_coalesce_edit(
        &mut self,
        entry_id: EntryId,
        scrollback: &mut ScrollbackState,
        is_replay: bool,
    ) {
        if !crate::appearance::cache::load_collapsed_edit_blocks() {
            return;
        }
        if scrollback
            .get_by_id(entry_id)
            .and_then(Self::coalescable_edit)
            .is_none()
        {
            return;
        }
        let mut survivor = entry_id;
        loop {
            let Some(idx) = scrollback.index_of_id(survivor) else {
                return;
            };
            let prev_id = idx
                .checked_sub(1)
                .and_then(|i| scrollback.get(i))
                .map(|e| e.id);
            if let Some(prev_id) = prev_id
                && self.edits_can_merge(scrollback, prev_id, survivor)
            {
                self.merge_edit_entries(prev_id, survivor, scrollback, is_replay);
                survivor = prev_id;
                continue;
            }
            let next_id = scrollback.get(idx + 1).map(|e| e.id);
            if let Some(next_id) = next_id
                && self.edits_can_merge(scrollback, survivor, next_id)
            {
                self.merge_edit_entries(survivor, next_id, scrollback, is_replay);
                continue;
            }
            return;
        }
    }
    /// Append `removed`'s hunks onto `survivor` (the earlier entry) —
    /// stitching overlapping/adjacent ones into unified hunks — and drop
    /// `removed` from the scrollback and the edit-HL queue.
    fn merge_edit_entries(
        &mut self,
        survivor: EntryId,
        removed: EntryId,
        scrollback: &mut ScrollbackState,
        is_replay: bool,
    ) {
        let (removed_hunks, removed_edit_count) =
            match scrollback.get_by_id(removed).map(|e| &e.block) {
                Some(RenderBlock::ToolCall(ToolCallBlock::Edit(edit))) => {
                    (edit.hunks.clone(), edit.edit_count)
                }
                _ => return,
            };
        if let Some(entry) = scrollback.get_by_id_mut(survivor) {
            if let RenderBlock::ToolCall(ToolCallBlock::Edit(edit)) = &mut entry.block {
                let merged_edit_count = edit.edit_count + removed_edit_count;
                let mut hunks = std::mem::take(&mut edit.hunks);
                hunks.extend(removed_hunks);
                edit.set_hunks(echo_agent_pager_diff::stitch_overlapping_hunks(hunks));
                edit.edit_count = merged_edit_count;
                edit.highlight = EditHighlightPhase::HunkOnly;
            }
            entry.invalidate_cache();
        }
        scrollback.mark_structurally_dirty(survivor);
        scrollback.remove_entry(removed);
        self.pending_edit_hl.retain(|id| *id != removed);
        if !is_replay && !self.pending_edit_hl.contains(&survivor) {
            self.pending_edit_hl.push(survivor);
        }
    }
    /// Process a single SessionUpdate, mutating the scrollback.
    ///
    /// The `meta` carries server-side timestamps used for thinking elapsed time.
    /// Returns true if the scrollback was modified (needs redraw).
    pub fn handle_update(
        &mut self,
        update: acp::SessionUpdate,
        meta: &NotificationMeta,
        scrollback: &mut ScrollbackState,
    ) -> bool {
        if !meta.is_replay {
            debug!(
                target: crate::tracing::ACP_UPDATE_TARGET,
                "[acp] {} | {}",
                update_summary(&update),
                meta_summary(meta),
            );
        }
        if self.retry_activity.is_some() {
            self.retry_activity = None;
        }
        if let Some(new_start) = meta.stream_start_ms {
            if self
                .last_stream_start_ms
                .is_some_and(|prev| prev != new_start)
            {
                let thinking_has_content = self
                    .current_thinking
                    .and_then(|id| scrollback.get_by_id(id))
                    .is_some_and(|e| {
                        if let RenderBlock::Thinking(t) = &e.block {
                            !t.text().is_empty()
                        } else {
                            false
                        }
                    });
                if thinking_has_content {
                    self.finish_thinking(scrollback);
                }
                if let Some(agent_id) = self.current_agent_msg.take() {
                    scrollback.finish_running(agent_id);
                }
                if !meta.is_replay
                    && self.current_thinking.is_none()
                    && self.activity_known_blocking_wait().is_none()
                {
                    self.pre_create_thinking(scrollback);
                }
            }
            self.last_stream_start_ms = Some(new_start);
        }
        let is_agent_output = matches!(
            &update,
            acp::SessionUpdate::AgentMessageChunk(_)
                | acp::SessionUpdate::AgentThoughtChunk(_)
                | acp::SessionUpdate::ToolCall(_)
                | acp::SessionUpdate::ToolCallUpdate(_)
        );
        if is_agent_output && !matches!(&update, acp::SessionUpdate::ToolCallUpdate(_)) {
            self.writing_tool_call = None;
            self.writing_tool_names.clear();
        }
        let changed = match update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => {
                self.blocking_waits.clear();
                self.handle_agent_chunk(chunk, meta, scrollback)
            }
            acp::SessionUpdate::AgentThoughtChunk(thought) => {
                self.drop_stale_blocking_waits(meta.stream_start_ms);
                self.handle_thought_chunk(thought, meta, scrollback)
            }
            acp::SessionUpdate::ToolCall(tc) => {
                self.handle_tool_call(tc, scrollback, meta.is_replay)
            }
            acp::SessionUpdate::ToolCallUpdate(tcu) => {
                self.handle_tool_call_update(tcu, scrollback, meta.is_replay)
            }
            acp::SessionUpdate::UserMessageChunk(chunk) => {
                self.handle_user_message(chunk, meta, scrollback)
            }
            acp::SessionUpdate::AvailableCommandsUpdate(update) => {
                if let Some(t) = parse_tools_meta(update.meta.as_ref()) {
                    self.pending_acp_tools = Some(t);
                }
                self.pending_acp_commands = Some(update.available_commands);
                true
            }
            acp::SessionUpdate::Plan(_) | acp::SessionUpdate::CurrentModeUpdate(_) => false,
            _ => false,
        };
        if is_agent_output && changed && !meta.is_replay {
            self.bump_agent_output_epoch();
        }
        changed
    }
    /// Called when PromptResponse is received (turn complete).
    pub fn finish_turn(&mut self, scrollback: &mut ScrollbackState) {
        self.epoch_at_last_finish = self.agent_output_epoch;
        self.finish_thinking(scrollback);
        scrollback.note_pin_reserve_turn_finished();
        if let Some(agent_id) = self.current_agent_msg.take() {
            scrollback.finish_running(agent_id);
        }
        for (_, pending) in self.pending_tools.drain() {
            if let Some(entry_id) = pending.entry_id {
                scrollback.finish_running(entry_id);
            }
        }
        if let Some(pending) = self.pending_compaction.take() {
            scrollback.push_block(RenderBlock::session_event(
                SessionEvent::CompactionCompleted {
                    tokens_before: pending.tokens_before,
                    tokens_after: pending.last_used.unwrap_or(pending.estimate_after),
                    elapsed_ms: pending.elapsed_ms,
                },
            ));
        }
        self.last_thinking_elapsed_ms = None;
        self.last_stream_start_ms = None;
        self.compaction_activity = None;
        self.retry_activity = None;
        self.writing_tool_call = None;
        self.writing_tool_names.clear();
        self.suppressed_tools.clear();
        self.blocking_waits.clear();
        self.orphan_updates.clear();
        self.skip_next_skill_body = false;
    }
    /// Finish the current thinking block, passing elapsed time to the entry.
    ///
    /// Empty thinking blocks (pre-created but never received content) are
    /// removed from scrollback — they'd show a misleading "Thought for 0.0s".
    /// Only blocks that received actual thinking tokens are kept.
    fn finish_thinking(&mut self, scrollback: &mut ScrollbackState) {
        if let Some(thinking_id) = self.current_thinking.take() {
            let is_empty = scrollback.get_by_id(thinking_id).is_some_and(
                |e| matches!(&e.block, RenderBlock::Thinking(t) if t.text().is_empty()),
            );
            if is_empty {
                scrollback.remove_entry(thinking_id);
            } else {
                scrollback.finish_running_with_time(thinking_id, self.last_thinking_elapsed_ms);
            }
            self.last_thinking_elapsed_ms = None;
        }
    }
    /// Pre-create a thinking block so "Thinking…" appears immediately
    /// when the turn starts, before the first ThinkingDelta arrives.
    ///
    /// The tracker's `current_thinking` is set so subsequent ThinkingDelta
    /// chunks append to this entry instead of creating a new one.
    /// No-op when `show_thinking_blocks` is off.
    pub fn pre_create_thinking(&mut self, scrollback: &mut ScrollbackState) {
        if !crate::appearance::cache::load_show_thinking_blocks() {
            return;
        }
        if self.current_thinking.is_none() {
            let block = RenderBlock::thinking_streaming();
            let entry_id = scrollback.push_block(block);
            scrollback.set_last_running(true);
            self.current_thinking = Some(entry_id);
        }
    }
    /// Mark that the next UserMessageChunk should be silently dropped.
    ///
    /// Call this from `dispatch_send_prompt` after pushing the user entry
    /// directly, so the ACP echo doesn't produce a duplicate.
    pub fn expect_user_echo(&mut self) {
        self.skip_next_user_echo = true;
    }
    /// Reset stale skip state when no local user block was rendered, so the
    /// agent's user-message broadcast is the one source of the user echo
    /// (e.g. the synthetic cron/bash adoption path) instead of being dropped.
    pub fn clear_user_echo_skip(&mut self) {
        self.skip_next_user_echo = false;
        self.skip_next_skill_body = false;
    }
    /// Whether [`expect_user_echo`] is pending (subagent replay tests).
    #[cfg(test)]
    pub fn expects_user_echo(&self) -> bool {
        self.skip_next_user_echo
    }
    /// Handle an agent message chunk (streaming text).
    fn handle_agent_chunk(
        &mut self,
        chunk: acp::ContentChunk,
        meta: &NotificationMeta,
        scrollback: &mut ScrollbackState,
    ) -> bool {
        self.finish_thinking(scrollback);
        let text = extract_text_from_content(&chunk.content);
        if text.is_empty() {
            return false;
        }
        if self.current_agent_msg.is_none() && text.trim().is_empty() {
            tracing::warn!(
                text = %text.escape_debug(),
                "ignoring whitespace-only agent message chunk (no prior content)"
            );
            return false;
        }
        let is_new = self.current_agent_msg.is_none();
        let id = *self.current_agent_msg.get_or_insert_with(|| {
            let entry_id = scrollback.start_streaming_agent();
            scrollback.set_last_running(true);
            entry_id
        });
        if is_new
            && let Some(ts_ms) = meta.agent_timestamp_ms
            && let Some(entry) = scrollback.get_by_id_mut(id)
        {
            entry.created_at = Some(utc_ms_to_local(ts_ms));
        }
        if meta.is_replay {
            scrollback.push_chunk_to_agent_deferred(id, &text)
        } else {
            scrollback.push_chunk_to_agent(id, &text)
        }
    }
    /// Handle an agent thought chunk (streaming thinking).
    fn handle_thought_chunk(
        &mut self,
        thought: acp::ContentChunk,
        meta: &NotificationMeta,
        scrollback: &mut ScrollbackState,
    ) -> bool {
        if !crate::appearance::cache::load_show_thinking_blocks() {
            return false;
        }
        let text = match &thought.content {
            acp::ContentBlock::Text(t) => &t.text,
            _ => return false,
        };
        if text.is_empty() {
            return false;
        }
        let is_replay = meta.is_replay;
        let id = *self.current_thinking.get_or_insert_with(|| {
            let block = if is_replay {
                RenderBlock::thinking_streaming_replay()
            } else {
                RenderBlock::thinking_streaming()
            };
            let entry_id = scrollback.push_block(block);
            scrollback.set_last_running(true);
            entry_id
        });
        if let (Some(agent_ts), Some(stream_start)) =
            (meta.agent_timestamp_ms, meta.stream_start_ms)
        {
            self.last_thinking_elapsed_ms = Some(agent_ts - stream_start);
        }
        if meta.is_replay {
            scrollback.push_chunk_to_thinking_deferred(id, text)
        } else {
            scrollback.push_chunk_to_thinking(id, text)
        }
    }
    /// Handle a tool call start.
    fn handle_tool_call(
        &mut self,
        tc: acp::ToolCall,
        scrollback: &mut ScrollbackState,
        is_replay: bool,
    ) -> bool {
        self.finish_thinking(scrollback);
        self.current_agent_msg = None;
        if is_todo_tool(&tc)
            || is_bg_plumbing_tool(&tc)
            || is_task_tool(&tc)
            || is_goal_tool(&tc)
            || is_scheduler_tool(&tc)
            || is_workflow_tool(&tc)
        {
            if is_task_tool(&tc) {
                let is_background = tc
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("subagentBackground"))
                    .and_then(serde_json::Value::as_bool);
                if is_background != Some(true) {
                    self.blocking_waits.insert(
                        tc.tool_call_id.0.to_string(),
                        BlockingWait {
                            reason: WaitingReason::subagent(),
                            stream_start_ms: self.last_stream_start_ms,
                        },
                    );
                }
            } else if let Some(reason) = blocking_wait_reason(&tc) {
                self.blocking_waits.insert(
                    tc.tool_call_id.0.to_string(),
                    BlockingWait {
                        reason,
                        stream_start_ms: self.last_stream_start_ms,
                    },
                );
            }
            self.suppressed_tools.insert(tc.tool_call_id.0.to_string());
            return false;
        }
        let tc_id = tc.tool_call_id.0.to_string();
        if let Some(orphan) = self.orphan_updates.remove(&tc_id) {
            let merged = merge_tool_call_update(tc, orphan);
            let block = tool_call_to_block(&merged, self.session_cwd.as_deref());
            self.finish_completed_tool(block, scrollback, is_replay);
            return true;
        }
        let is_completed = matches!(
            tc.status,
            acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed
        );
        if is_completed {
            let block = tool_call_to_block(&tc, self.session_cwd.as_deref());
            self.finish_completed_tool(block, scrollback, is_replay);
        } else {
            let block = tool_call_to_block(&tc, self.session_cwd.as_deref());
            let id = scrollback.push_block(block);
            scrollback.set_last_running(true);
            let started_at = Some(std::time::Instant::now());
            self.pending_tools.insert(
                tc_id,
                PendingTool {
                    entry_id: Some(id),
                    base: tc,
                    utf8_decoder: Utf8Decoder::default(),
                    started_at,
                },
            );
        }
        true
    }
    /// Handle a tool call update (streaming output or completion).
    fn handle_tool_call_update(
        &mut self,
        tcu: acp::ToolCallUpdate,
        scrollback: &mut ScrollbackState,
        is_replay: bool,
    ) -> bool {
        let tc_id_str = tcu.tool_call_id.0.to_string();
        if self.bg_deferred_tools.contains_key(&tc_id_str) {
            return false;
        }
        if self.suppressed_tools.contains(&tc_id_str) {
            if let Some(ref raw_input) = tcu.fields.raw_input {
                let variant = raw_input.get("variant").and_then(|v| v.as_str());
                if is_task_variant(variant) {
                    let run_in_bg = raw_input
                        .get("run_in_background")
                        .or_else(|| raw_input.get("background"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    if variant == Some("Task")
                        && let Some(task_id) = raw_input.get("task_id").and_then(|v| v.as_str())
                    {
                        self.task_tool_background
                            .insert(task_id.to_string(), run_in_bg);
                    }
                    if run_in_bg {
                        self.blocking_waits.remove(&tc_id_str);
                    }
                }
                if let Some(WaitingReason::TaskOutput {
                    task_ids, waits, ..
                }) = self
                    .blocking_waits
                    .get_mut(&tc_id_str)
                    .map(|w| &mut w.reason)
                {
                    let extracted = task_ids_from_raw_input(raw_input);
                    if !extracted.is_empty() {
                        *task_ids = extracted;
                    }
                    if raw_input.get("timeout_ms").is_some() {
                        *waits = timeout_waits(Some(raw_input));
                    }
                }
            }
            let status = tcu.fields.status.unwrap_or_default();
            if matches!(
                status,
                acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed
            ) {
                self.suppressed_tools.remove(&tc_id_str);
                self.blocking_waits.remove(&tc_id_str);
            }
            return false;
        }
        let status = tcu.fields.status.unwrap_or_default();
        let is_completed = matches!(
            status,
            acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed
        );
        let tc_id = tcu.tool_call_id.0.to_string();
        if !is_completed {
            let defer_as_bg = if let Some(pending) = self.pending_tools.get_mut(&tc_id) {
                let bash_output = extract_bash_output_from_value(&tcu.fields.raw_output);
                pending.base.update(tcu.fields);
                if pending.entry_id.is_none() && is_bg_tool(&pending.base) {
                    let desc = extract_raw_field(&pending.base, "description");
                    Some((tc_id.clone(), desc, false))
                } else if pending.entry_id.is_some() && is_bg_tool(&pending.base) {
                    let eid = pending.entry_id;
                    let has_real_command = raw_input_command(&pending.base).is_some();
                    let entry_placeholder = eid
                        .and_then(|id| scrollback.get_by_id(id))
                        .is_some_and(entry_is_execute_placeholder);
                    let drop_placeholder = entry_placeholder && !has_real_command;
                    let desc = extract_raw_field(&pending.base, "description");
                    if drop_placeholder {
                        if let Some(id) = pending.entry_id.take() {
                            scrollback.remove_entry(id);
                        }
                        Some((tc_id.clone(), desc, false))
                    } else {
                        if let Some(entry_id) = pending.entry_id {
                            let mut block =
                                tool_call_to_block(&pending.base, self.session_cwd.as_deref());
                            let mut kind_changed = false;
                            if let Some(entry) = scrollback.get_by_id_mut(entry_id) {
                                if let RenderBlock::ToolCall(new_tc) = &mut block
                                    && let Some(t) = pending.started_at
                                {
                                    new_tc.set_started_at(t);
                                }
                                kind_changed = verb_group_kind_changed(&entry.block, &block);
                                entry.block = block;
                                entry.invalidate_cache();
                            }
                            if kind_changed {
                                scrollback.mark_structurally_dirty(entry_id);
                            }
                        }
                        Some((tc_id.clone(), desc, true))
                    }
                } else {
                    let entry_id = if let Some(entry_id) = pending.entry_id {
                        let block = tool_call_to_block(&pending.base, self.session_cwd.as_deref());
                        scrollback.replace_tool_block(entry_id, block, pending.started_at);
                        entry_id
                    } else {
                        let block = tool_call_to_block(&pending.base, self.session_cwd.as_deref());
                        let id = scrollback.push_block(block);
                        scrollback.set_last_running(true);
                        pending.entry_id = Some(id);
                        id
                    };
                    if let Some(bash_output) = bash_output {
                        if let Some(delta) = &bash_output.output_delta {
                            let text = pending.utf8_decoder.decode(delta);
                            return scrollback.append_execute_output(entry_id, text);
                        }
                        let output_str = String::from_utf8_lossy(&bash_output.output);
                        return scrollback.set_execute_output(entry_id, &output_str);
                    }
                    return true;
                }
            } else {
                return false;
            };
            if let Some((deferred_id, description, keep_in_pending)) = defer_as_bg {
                tracing::debug!(
                    tool_call_id = %deferred_id,
                    keep_in_pending,
                    "Deferring is_background=true tool to bg_deferred_tools"
                );
                if !keep_in_pending {
                    self.pending_tools.remove(&deferred_id);
                }
                self.bg_deferred_tools.insert(deferred_id, description);
                return false;
            }
            unreachable!("both branches above return");
        }
        if let Some(pending) = self.pending_tools.remove(&tc_id) {
            let merged = merge_tool_call_update(pending.base, tcu);
            let block = tool_call_to_block(&merged, self.session_cwd.as_deref());
            if let Some(entry_id) = pending.entry_id {
                if scrollback.replace_tool_block(entry_id, block, pending.started_at)
                    && let Some(entry) = scrollback.get_by_id(entry_id)
                {
                    self.queue_edit_hl_if_needed(entry_id, &entry.block, is_replay);
                }
                scrollback.finish_running(entry_id);
                self.try_coalesce_edit(entry_id, scrollback, is_replay);
            } else {
                self.finish_completed_tool(block, scrollback, is_replay);
            }
            true
        } else {
            self.orphan_updates.insert(tc_id, tcu);
            false
        }
    }
    /// Handle a user message chunk (session replay or live followup).
    ///
    /// If `skip_next_user_echo` is set, this is the ACP echo of a prompt
    /// we already added to scrollback — drop it but still reset tracking
    /// state so the agent's response creates fresh entries.
    fn handle_user_message(
        &mut self,
        chunk: acp::ContentChunk,
        meta: &NotificationMeta,
        scrollback: &mut ScrollbackState,
    ) -> bool {
        let text = extract_text_from_content(&chunk.content);
        if self.skip_next_skill_body {
            self.skip_next_skill_body = false;
            return false;
        }
        if text.is_empty() {
            return false;
        }
        self.finish_thinking(scrollback);
        if let Some(agent_id) = self.current_agent_msg.take() {
            scrollback.finish_running(agent_id);
        }
        for (_, pending) in self.pending_tools.drain() {
            if let Some(entry_id) = pending.entry_id {
                scrollback.finish_running(entry_id);
            }
        }
        if self.skip_next_user_echo {
            self.skip_next_user_echo = false;
            if text.contains("<command-name>") {
                self.skip_next_skill_body = true;
            }
            let prompt_index = chunk
                .meta
                .as_ref()
                .and_then(|m| m.get(user_message_chunk_meta::PROMPT_INDEX))
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let replay_ts = meta
                .is_replay
                .then(|| meta.turn_start_ms.or(meta.agent_timestamp_ms))
                .flatten();
            if prompt_index.is_some() || replay_ts.is_some() {
                for idx in (0..scrollback.len()).rev() {
                    if let Some(entry) = scrollback.get_mut(idx)
                        && let RenderBlock::UserPrompt(ref mut block) = entry.block
                    {
                        if block.is_interjection {
                            continue;
                        }
                        if let Some(pi) = prompt_index
                            && block.prompt_index.is_none()
                        {
                            block.prompt_index = Some(pi);
                        }
                        if let Some(ms) = replay_ts {
                            entry.created_at = Some(utc_ms_to_local(ms));
                        }
                        break;
                    }
                }
            }
            return false;
        }
        let prompt_index = chunk
            .meta
            .as_ref()
            .and_then(|m| m.get(user_message_chunk_meta::PROMPT_INDEX))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        if let Some(segments) = combined_display_texts_from_chunk(&chunk) {
            let mut last_id = None;
            for seg in &segments {
                last_id = Some(scrollback.push_block(RenderBlock::UserPrompt(
                    crate::scrollback::blocks::UserPromptBlock::new(seg.clone()),
                )));
            }
            if let (Some(pi), Some(id)) = (prompt_index, last_id)
                && let Some(entry) = scrollback.get_by_id_mut(id)
                && let RenderBlock::UserPrompt(ref mut block) = entry.block
            {
                block.prompt_index = Some(pi);
            }
            return true;
        }
        let display_override = match &chunk.content {
            acp::ContentBlock::Text(t) => t
                .meta
                .as_ref()
                .and_then(|m| m.get(user_prompt_meta::DISPLAY_TEXT))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            _ => None,
        };
        let skill_token_ranges = match &chunk.content {
            acp::ContentBlock::Text(t) => t
                .meta
                .as_ref()
                .and_then(|m| m.get(user_prompt_meta::SKILL_TOKEN_RANGES))
                .map(parse_skill_token_ranges)
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut block = if let Some(dt) = display_override {
            if text.contains("<command-name>") {
                self.skip_next_skill_body = true;
            }
            let (as_skill, as_cron) = match &chunk.content {
                acp::ContentBlock::Text(t) => {
                    let m = t.meta.as_ref();
                    let skill = m
                        .and_then(|m| m.get(user_prompt_meta::DISPLAY_AS_SKILL))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let cron = m
                        .and_then(|m| m.get(user_prompt_meta::DISPLAY_AS_CRON))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    (skill, cron)
                }
                _ => (false, false),
            };
            if as_cron {
                crate::scrollback::blocks::UserPromptBlock::cron(dt)
            } else if as_skill {
                crate::scrollback::blocks::UserPromptBlock::skill(dt)
            } else {
                crate::scrollback::blocks::UserPromptBlock::new(dt)
            }
        } else if !skill_token_ranges.is_empty() {
            crate::scrollback::blocks::UserPromptBlock::with_skill_tokens(text, skill_token_ranges)
        } else {
            let skill_display =
                echo_agent_tools::implementations::skills::skill::extract_skill_display_text(&text);
            if let Some(display_text) = skill_display {
                self.skip_next_skill_body = true;
                crate::scrollback::blocks::UserPromptBlock::skill(display_text)
            } else if text.starts_with('/') && !text.starts_with("//") {
                crate::scrollback::blocks::UserPromptBlock::skill(text)
            } else if let Some(cmd) = extract_skill_header_command(&text) {
                crate::scrollback::blocks::UserPromptBlock::new(cmd)
            } else if let Some(prompt) = extract_cron_prompt_body(&text) {
                crate::scrollback::blocks::UserPromptBlock::cron(prompt)
            } else if user_message_hidden_from_scrollback(&chunk, meta, &text) {
                return false;
            } else {
                crate::scrollback::blocks::UserPromptBlock::new(text)
            }
        };
        block.prompt_index = prompt_index;
        let entry_id = scrollback.push_block(RenderBlock::UserPrompt(block));
        let ts_ms = meta.turn_start_ms.or(meta.agent_timestamp_ms);
        if let Some(ms) = ts_ms
            && let Some(entry) = scrollback.get_by_id_mut(entry_id)
        {
            entry.created_at = Some(utc_ms_to_local(ms));
        }
        true
    }
}
/// Per-prompt display strings from combine ([`user_prompt_meta::COMBINED_DISPLAY_TEXTS`]).
fn combined_display_texts_from_chunk(chunk: &acp::ContentChunk) -> Option<Vec<String>> {
    let acp::ContentBlock::Text(t) = &chunk.content else {
        return None;
    };
    let arr = t
        .meta
        .as_ref()?
        .get(user_prompt_meta::COMBINED_DISPLAY_TEXTS)?
        .as_array()?;
    let segs: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .filter(|s| !s.is_empty())
        .collect();
    (segs.len() >= 2).then_some(segs)
}
/// Parse `skillTokenRanges` content-block meta (`[[start, end], …]`) into
/// byte ranges. Malformed entries are skipped; bounds/boundary validation
/// happens in `UserPromptBlock::with_skill_tokens`.
fn parse_skill_token_ranges(v: &serde_json::Value) -> Vec<std::ops::Range<usize>> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|pair| {
                    let pair = pair.as_array()?;
                    let start = pair.first()?.as_u64()? as usize;
                    let end = pair.get(1)?.as_u64()? as usize;
                    Some(start..end)
                })
                .collect()
        })
        .unwrap_or_default()
}
/// Extract a slash command name from a skill instruction markdown header.
///
/// Matches text starting with `# /command -- ` (the format used by
/// `InjectSkill`). Returns the `## Input` section's content if present,
/// prefixed with the command name. Falls back to just the command name.
///
/// Example: `"# /loop -- schedule a recurring prompt\n\n...\n## Input\n5m check deploy"`
/// → `"/loop 5m check deploy"`
fn extract_skill_header_command(text: &str) -> Option<String> {
    let text = text.strip_prefix("# ")?;
    if !text.starts_with('/') {
        return None;
    }
    let cmd_name = text.split(&[' ', '\n'][..]).next()?;
    if let Some(input_idx) = text.find("## Input\n") {
        let args = text[input_idx + "## Input\n".len()..].trim();
        if !args.is_empty() {
            return Some(format!("{cmd_name} {args}"));
        }
    }
    Some(cmd_name.to_string())
}
/// Whether a `UserMessageChunk` must stay out of scrollback.
///
/// Type-driven (preferred):
/// 1. `ContentChunk._meta.hideFromScrollback` stamped by the shell from
///    [`PromptOrigin::hide_user_echo_from_scrollback`]
/// 2. `SessionNotification._meta.promptId` classified via
///    [`PromptOrigin::from_prompt_id`]
///
/// Legacy fallback (pre-meta sessions only): bare auto-wake text that used to
/// be gated by the system-reminder prefix. Cron is handled earlier by
/// [`extract_cron_prompt_body`].
fn user_message_hidden_from_scrollback(
    chunk: &acp::ContentChunk,
    meta: &NotificationMeta,
    text: &str,
) -> bool {
    if chunk
        .meta
        .as_ref()
        .and_then(|m| m.get(user_message_chunk_meta::HIDE_FROM_SCROLLBACK))
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        return true;
    }
    if let Some(pid) = meta.prompt_id.as_deref()
        && echo_agent_runtime::session::PromptOrigin::from_prompt_id(pid)
            .hide_user_echo_from_scrollback()
    {
        return true;
    }
    let t = text.trim_start();
    t.starts_with("<system-reminder>")
        || t.starts_with("<monitor-event")
        || t.trim() == "---"
        || t.lines().next().is_some_and(|first| {
            first.starts_with(|c: char| c.is_ascii_digit())
                && first.contains(" monitor events from ")
                && first.contains(" (use ")
        })
}
/// Extract the user's prompt from `<system-reminder>` cron framing.
///
/// Matches the format produced by `format_scheduled_task_prompt`:
/// `"<system-reminder>\nThis is a scheduled task execution...\n</system-reminder>\n\n<prompt>"`
///
/// Returns the prompt text after the closing tag, or `None` if the text
/// doesn't match the cron framing pattern.
fn extract_cron_prompt_body(text: &str) -> Option<String> {
    if !text.starts_with("<system-reminder>") {
        return None;
    }
    let end_tag = "</system-reminder>";
    let close = text.find(end_tag)?;
    let header = &text[..close];
    if !header.contains("scheduled task execution") {
        return None;
    }
    let body = text[close + end_tag.len()..].trim();
    if body.is_empty() {
        return None;
    }
    Some(body.to_string())
}
/// Merge ToolCallUpdate fields with the base ToolCall.
/// Update fields take precedence when present.
fn merge_tool_call_update(base: acp::ToolCall, update: acp::ToolCallUpdate) -> acp::ToolCall {
    acp::ToolCall::new(
        update.tool_call_id,
        update.fields.title.unwrap_or(base.title),
    )
    .kind(update.fields.kind.unwrap_or(base.kind))
    .status(update.fields.status.unwrap_or(base.status))
    .content(update.fields.content.unwrap_or(base.content))
    .raw_input(update.fields.raw_input.or(base.raw_input))
    .raw_output(update.fields.raw_output.or(base.raw_output))
    .locations(update.fields.locations.unwrap_or(base.locations))
    .meta(base.meta)
}
/// Peeled display form when a redundant leading `cd <cwd>` was stripped, else None.
fn peeled_if_changed(command: &str, session_cwd: Option<&Path>) -> Option<String> {
    let cwd = session_cwd?;
    let stripped = strip_redundant_session_cd(command, cwd);
    (stripped.as_ref() != command).then(|| stripped.into_owned())
}
/// True when `s` is an ACP/function tool id rather than a shell command.
///
/// Eager ToolCall messages often set `title` to the function name
/// (`run_terminal_command`) before `raw_input.command` arrives — using that as
/// the execute header flashes the internal tool name in the TUI.
fn is_execute_tool_function_name(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "run_terminal_command"
            | "run_terminal_cmd"
            | "bash"
            | "shell"
            | "execute"
            | "run_command"
            | "terminal"
    )
}
/// Eager execute-related placeholder that should not be shown to the user.
///
/// Only **empty** execute commands count as placeholders. A real shell
/// invocation of `bash` / `shell` / etc. must not be dropped on late
/// `is_background` (would lose demotion + stdout). Other blocks still
/// matching the tool function name are placeholders.
fn entry_is_execute_placeholder(entry: &crate::scrollback::entry::ScrollbackEntry) -> bool {
    match &entry.block {
        RenderBlock::ToolCall(ToolCallBlock::Execute(ex)) => ex.command.trim().is_empty(),
        RenderBlock::ToolCall(ToolCallBlock::Other(o)) => is_execute_tool_function_name(&o.name),
        _ => false,
    }
}
/// Non-empty `raw_input.command` if present (empty / whitespace treated as missing).
fn raw_input_command(tc: &acp::ToolCall) -> Option<String> {
    extract_raw_field(tc, "command").and_then(|c| {
        let t = c.trim();
        if t.is_empty() { None } else { Some(c) }
    })
}
/// Resolve the shell command for an execute tool call.
///
/// Prefer `raw_input.command`. Do **not** fall back to a title that is only the
/// tool function name (that produces the "Run run_terminal_command" flash).
fn execute_command_from_tool_call(tc: &acp::ToolCall) -> String {
    if let Some(cmd) = raw_input_command(tc) {
        return cmd;
    }
    if !tc.title.is_empty() && !is_execute_tool_function_name(&tc.title) {
        return tc.title.clone();
    }
    String::new()
}
/// Convert an ACP ToolCall to a RenderBlock.
///
/// Parses `tool_call.kind` to create the appropriate block type,
/// extracting fields from `raw_input` JSON when available. `session_cwd` sets
/// execute `header_display` when a leading `cd <cwd>` is redundant.
fn tool_call_to_block(tc: &acp::ToolCall, session_cwd: Option<&Path>) -> RenderBlock {
    let success = !matches!(tc.status, acp::ToolCallStatus::Failed);
    match tc.kind {
        acp::ToolKind::Execute => {
            let command = execute_command_from_tool_call(tc);
            let header_display = peeled_if_changed(&command, session_cwd);
            let description = extract_raw_field(tc, "description");
            let is_bash_mode = tc
                .meta
                .as_ref()
                .and_then(|m| m.get("bash_mode"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if let Some(bash) = extract_bash_output_from_value(&tc.raw_output) {
                let output_str = String::from_utf8_lossy(&bash.output);
                let mut block = ExecuteToolCallBlock::new(command).with_output(output_str.as_ref());
                block.bash_mode = is_bash_mode;
                block.header_display = header_display;
                if let Some(desc) = description {
                    block = block.with_description(desc);
                }
                if !success || bash.exit_code != 0 {
                    let error_msg = if let Some(sig) = &bash.signal {
                        sig.clone()
                    } else if bash.exit_code != 0 {
                        format!("exit code {}", bash.exit_code)
                    } else {
                        "Command failed".into()
                    };
                    block = block.with_error(error_msg);
                }
                RenderBlock::ToolCall(ToolCallBlock::Execute(block))
            } else {
                let mut block = ExecuteToolCallBlock::new(command);
                block.bash_mode = is_bash_mode;
                block.header_display = header_display;
                if let Some(desc) = description {
                    block = block.with_description(desc);
                }
                if !success {
                    let text = content_text(tc);
                    let error_msg = if text.is_empty() {
                        "Command failed".to_string()
                    } else {
                        text
                    };
                    block = block.with_error(error_msg);
                }
                RenderBlock::ToolCall(ToolCallBlock::Execute(block))
            }
        }
        acp::ToolKind::Read => {
            let path = extract_raw_field(tc, "file_path")
                .or_else(|| extract_raw_field(tc, "target_file"))
                .or_else(|| extract_raw_field(tc, "path"))
                .unwrap_or_else(|| tc.title.clone());
            let mut block = ReadToolCallBlock::new(&path);
            if let Some(ref raw) = tc.raw_output
                && let Ok(ToolOutput::ReadFile(read_output)) =
                    serde_json::from_value::<ToolOutput>(raw.clone())
            {
                match read_output {
                    ReadFileOutput::FileContent(fc) => {
                        if fc.offset.is_some() || fc.limit.is_some() {
                            let off = fc.offset.unwrap_or(0);
                            let start = off + 1;
                            let end = fc
                                .limit
                                .map_or(fc.total_lines, |lim| (off + lim).min(fc.total_lines));
                            block = block.with_line_range(LineRange::new(start, end));
                        }
                        block = block.with_content(fc.raw_output, fc.total_lines);
                    }
                    ReadFileOutput::FileNotFound(msg)
                    | ReadFileOutput::IsADirectory(msg)
                    | ReadFileOutput::PermissionDenied(msg)
                    | ReadFileOutput::FileTooLarge(msg)
                    | ReadFileOutput::FileReadError(msg)
                    | ReadFileOutput::ImageSizeError(msg) => {
                        block = block.with_error(msg);
                    }
                    ReadFileOutput::ImageContent(_) => {
                        block.media_kind = Some(ReadMediaKind::Image);
                        block.image_ref =
                            crate::prompt_images::ScrollbackImageRef::from_path(&path);
                    }
                    ReadFileOutput::PdfPageImages(pdf) => {
                        block.media_kind = Some(ReadMediaKind::Pdf {
                            pages: pdf.total_pages,
                        });
                    }
                }
            } else if !success {
                let text = content_text(tc);
                block = block.with_error(if text.is_empty() {
                    "Read failed".to_string()
                } else {
                    text
                });
            }
            RenderBlock::ToolCall(ToolCallBlock::Read(block))
        }
        acp::ToolKind::Edit => {
            let raw_path = extract_raw_field(tc, "file_path")
                .or_else(|| extract_raw_field(tc, "filePath"))
                .or_else(|| extract_raw_field(tc, "target_file"))
                .or_else(|| extract_raw_field(tc, "path"));
            let path_from_title = raw_path.is_none();
            let path = raw_path.unwrap_or_else(|| tc.title.clone());
            let untrusted_summary = path_from_title
                || tc
                    .content
                    .iter()
                    .filter(|c| matches!(c, acp::ToolCallContent::Diff(_)))
                    .count()
                    > 1;
            let is_write = is_write_tool(tc);
            let mut block = if success {
                let (hunks, _count) = echo_agent_pager_diff::extract_edit_hunks(tc);
                EditToolCallBlock::new(path, hunks)
            } else {
                let error_msg = extract_edit_error(tc);
                EditToolCallBlock::new(path, vec![]).with_error(error_msg)
            };
            if untrusted_summary {
                block = block.with_untrusted_summary();
            }
            if is_write {
                block = block.with_prefix("Creating ");
            }
            RenderBlock::ToolCall(ToolCallBlock::Edit(block))
        }
        acp::ToolKind::Search
            if matches!(
                extract_raw_field(tc, "variant").as_deref(),
                Some("WebSearch") | Some("XSearch")
            ) || tc.title.starts_with("Web search:")
                || tc.title.starts_with("X search:") =>
        {
            let is_backend = extract_raw_field(tc, "backend").as_deref() == Some("true")
                || tc
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("backend"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            let query = extract_raw_field(tc, "query")
                .or_else(|| {
                    tc.title
                        .strip_prefix("Web search: ")
                        .map(|q| q.trim_matches('"').to_owned())
                })
                .unwrap_or_else(|| {
                    if is_backend {
                        String::new()
                    } else {
                        tc.title.clone()
                    }
                });
            let mut block = WebSearchToolCallBlock::new(query);
            if is_backend {
                let variant = extract_raw_field(tc, "variant").unwrap_or_default();
                if variant == "XSearch" {
                    block.label = Some("X Search ".to_string());
                    block.is_x_search = true;
                }
                if let Some(ref raw) = tc.raw_output {
                    if variant == "XSearch" && raw.get("name").is_some() {
                        let tool_name = raw
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("x_search");
                        let short_type = match tool_name {
                            "x_keyword_search" => "keyword",
                            "x_semantic_search" => "semantic",
                            "x_user_search" => "users",
                            "x_thread_fetch" => "thread",
                            other => other,
                        };
                        let input_str = raw.get("input").and_then(|v| v.as_str()).unwrap_or("{}");
                        let query_text = serde_json::from_str::<serde_json::Value>(input_str)
                            .ok()
                            .and_then(|v| {
                                v.get("query").and_then(|q| q.as_str()).map(String::from)
                            });
                        if let Some(ref q) = query_text {
                            block.query = format!("{short_type}({q})");
                        } else {
                            block.query = short_type.to_string();
                        }
                    } else if variant == "WebSearch" && raw.pointer("/action/type").is_some() {
                        let action_type = raw
                            .pointer("/action/type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        match action_type {
                            "search" => {
                                if let Some(q) =
                                    raw.pointer("/action/query").and_then(|v| v.as_str())
                                {
                                    block.query = q.to_string();
                                }
                                if let Some(arr) =
                                    raw.pointer("/action/sources").and_then(|v| v.as_array())
                                {
                                    block.citations = arr
                                        .iter()
                                        .filter_map(|s| s.get("url").and_then(|u| u.as_str()))
                                        .map(|u| u.to_string())
                                        .collect();
                                }
                            }
                            "open_page" => {
                                if let Some(url) =
                                    raw.pointer("/action/url").and_then(|v| v.as_str())
                                {
                                    block.query = format!("open {url}");
                                    block.citations = vec![url.to_string()];
                                }
                            }
                            "find" | "find_in_page" => {
                                let pattern = raw
                                    .pointer("/action/pattern")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?");
                                let url = raw
                                    .pointer("/action/url")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                block.query = format!("find \"{pattern}\"");
                                if !url.is_empty() {
                                    block.citations = vec![url.to_string()];
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if !block.citations.is_empty() {
                    let sources_list: Vec<String> = block
                        .citations
                        .iter()
                        .enumerate()
                        .map(|(i, url)| format!("{}. {}", i + 1, url))
                        .collect();
                    block.content = Some(sources_list.join("\n"));
                }
            } else {
                if let Some(ref raw) = tc.raw_output
                    && let Ok(ToolOutput::WebSearch(ws)) =
                        serde_json::from_value::<ToolOutput>(raw.clone())
                {
                    if !ws.content.is_empty() {
                        block.content = Some(ws.content);
                    }
                    block.citations = ws.citations;
                }
                if block.content.is_none() {
                    let text = content_text(tc);
                    if !text.is_empty() {
                        block.content = Some(text);
                    }
                }
            }
            if !success {
                block = block.with_error("Web search failed");
            }
            RenderBlock::ToolCall(ToolCallBlock::WebSearch(block))
        }
        acp::ToolKind::Search => {
            let pattern = extract_raw_field(tc, "pattern")
                .or_else(|| extract_raw_field(tc, "glob_pattern"))
                .unwrap_or_else(|| tc.title.clone());
            let meta = extract_search_meta(tc);
            let grep = extract_grep_output(&tc.raw_output).unwrap_or_default();
            let mut block = SearchToolCallBlock::new(pattern);
            block.meta = meta;
            block.match_count = grep.match_count;
            block.file_matches = grep.file_matches;
            block.file_paths = grep.file_paths;
            if !success {
                block.error = Some("Search failed".into());
            }
            RenderBlock::ToolCall(ToolCallBlock::Search(block))
        }
        acp::ToolKind::Fetch => {
            let url = extract_raw_field(tc, "url")
                .or_else(|| tc.title.strip_prefix("Fetch: ").map(str::to_owned))
                .unwrap_or_else(|| tc.title.clone());
            let mut block = WebFetchToolCallBlock::new(url);
            if let Some(ref raw) = tc.raw_output
                && let Ok(ToolOutput::WebFetch(WebFetchOutput::Content(content))) =
                    serde_json::from_value::<ToolOutput>(raw.clone())
            {
                block.status_code = Some(content.status_code);
                block.content_type = Some(content.content_type);
                block.bytes = Some(content.bytes);
            }
            let text = content_text(tc);
            if !text.is_empty() {
                block.output = Some(text);
            }
            if !success {
                block = block.with_error("Fetch failed");
            }
            RenderBlock::ToolCall(ToolCallBlock::WebFetch(block))
        }
        _ if extract_raw_field(tc, "target_directory").is_some() => {
            let path = extract_raw_field(tc, "target_directory").unwrap();
            let mut block = ListDirToolCallBlock::new(make_relative_path(&path));
            if let Some(content) = extract_listdir_content(&tc.raw_output) {
                block = block.with_output(content);
            }
            if !success {
                block = block.with_error("List directory failed");
            }
            RenderBlock::ToolCall(ToolCallBlock::ListDir(block))
        }
        _ if extract_raw_field(tc, "variant").as_deref() == Some("SearchTool") => {
            let query = extract_raw_field(tc, "query").unwrap_or_default();
            let mut block = IntegrationSearchToolCallBlock::new(query);
            block.limit = tc
                .raw_input
                .as_ref()
                .and_then(|v| v.get("limit"))
                .and_then(|v| v.as_u64())
                .map(|n| n as u8);
            if let Some(ref raw) = tc.raw_output
                && let Ok(ToolOutput::SearchTool(SearchToolOutput {
                    result_count,
                    content,
                })) = serde_json::from_value::<ToolOutput>(raw.clone())
            {
                block.result_count = result_count;
                block.results = parse_search_tool_results(&content);
                block.content = Some(content);
            }
            if !success {
                block = block.with_error("Search failed");
            }
            RenderBlock::ToolCall(ToolCallBlock::IntegrationSearch(block))
        }
        _ if extract_raw_field(tc, "variant").as_deref() == Some("UseTool") => {
            let tool_name = extract_raw_field(tc, "tool_name").unwrap_or_else(|| tc.title.clone());
            let mut block = UseToolCallBlock::new(tool_name);
            block.input_args = extract_use_tool_args(tc);
            let text = content_text(tc);
            if !text.is_empty() {
                block.output = Some(text);
            } else if let Some(extracted) = extract_use_tool_output(&tc.raw_output) {
                block.output = Some(extracted);
            }
            if !success {
                block.error = Some(
                    block
                        .output
                        .take()
                        .unwrap_or_else(|| "Tool call failed".into()),
                );
            }
            RenderBlock::ToolCall(ToolCallBlock::UseTool(block))
        }
        _ if matches!(
            extract_raw_field(tc, "variant").as_deref(),
            Some("ImageGen") | Some("ImageToVideo") | Some("ReferenceToVideo") | Some("ImageEdit")
        ) =>
        {
            media_gen_block(tc, success)
        }
        _ if tc.title.starts_with("Memory search:") => {
            let query = tc
                .title
                .strip_prefix("Memory search: ")
                .map(|q| q.trim_matches('"').to_owned())
                .unwrap_or_else(|| tc.title.clone());
            let mut block = MemorySearchToolCallBlock::new(query);
            let text = content_text(tc);
            if !text.is_empty() {
                block.results =
                    crate::scrollback::blocks::tool::memory_search::parse_memory_results(&text);
            }
            if !success {
                block.error = Some("Memory search failed".into());
            }
            RenderBlock::ToolCall(ToolCallBlock::MemorySearch(block))
        }
        _ => {
            if is_execute_tool_function_name(&tc.title) {
                let command = execute_command_from_tool_call(tc);
                let header_display = peeled_if_changed(&command, session_cwd);
                if let Some(bash) = extract_bash_output_from_value(&tc.raw_output) {
                    let output_str = String::from_utf8_lossy(&bash.output);
                    let mut block =
                        ExecuteToolCallBlock::new(command).with_output(output_str.as_ref());
                    block.header_display = header_display;
                    if let Some(desc) = extract_raw_field(tc, "description") {
                        block = block.with_description(desc);
                    }
                    if !success || bash.exit_code != 0 {
                        let error_msg = if let Some(sig) = &bash.signal {
                            sig.clone()
                        } else if bash.exit_code != 0 {
                            format!("exit code {}", bash.exit_code)
                        } else {
                            "Command failed".into()
                        };
                        block = block.with_error(error_msg);
                    }
                    return RenderBlock::ToolCall(ToolCallBlock::Execute(block));
                }
                let mut block = ExecuteToolCallBlock::new(command);
                block.header_display = header_display;
                if let Some(desc) = extract_raw_field(tc, "description") {
                    block = block.with_description(desc);
                }
                if !success {
                    let text = content_text(tc);
                    block = block.with_error(if text.is_empty() {
                        "Command failed".to_string()
                    } else {
                        text
                    });
                }
                return RenderBlock::ToolCall(ToolCallBlock::Execute(block));
            }
            let name = tool_call_title(tc);
            let summary = if tc.title.is_empty() {
                extract_raw_field(tc, "path")
                    .or_else(|| extract_raw_field(tc, "url"))
                    .or_else(|| extract_raw_field(tc, "query"))
                    .unwrap_or_default()
            } else if tc.kind == acp::ToolKind::Other {
                String::new()
            } else {
                format!("{:?}", tc.kind).to_lowercase()
            };
            let (label, ctor): (String, fn(OtherToolCallBlock) -> ToolCallBlock) = if name
                .eq_ignore_ascii_case("skill")
                || name.to_ascii_lowercase().starts_with("skill:")
            {
                let label = match name.find(':') {
                    Some(i) => format!("Skill{}", &name[i..]),
                    None => "Skill".into(),
                };
                (label, ToolCallBlock::Skill)
            } else {
                (name.into_owned(), ToolCallBlock::Other)
            };
            let mut block = OtherToolCallBlock::new(label, summary);
            let ct = content_text(tc);
            if !success {
                block.error = Some(if ct.is_empty() {
                    "Failed".into()
                } else {
                    ct.clone()
                });
            }
            if !ct.is_empty() {
                block.set_output_text(ct);
            }
            RenderBlock::ToolCall(ctor(block))
        }
    }
}
/// Display title for a tool call: its title, or the kind name when empty.
fn tool_call_title(tc: &acp::ToolCall) -> Cow<'_, str> {
    if tc.title.is_empty() {
        Cow::Owned(format!("{:?}", tc.kind))
    } else {
        Cow::Borrowed(&tc.title)
    }
}
/// Build the media block from the typed `raw_output` path.
fn media_gen_block(tc: &acp::ToolCall, success: bool) -> RenderBlock {
    let mut block = OtherToolCallBlock::new(tool_call_title(tc), String::new());
    if !success {
        let err = content_text(tc);
        block.error = Some(if err.is_empty() { "Failed".into() } else { err });
    } else if let Some((path, is_video)) = media_gen_ref(tc) {
        block = block.with_media_ref(path, is_video);
    } else if let Some(text) = media_gen_text(tc) {
        block.set_output_text(text);
    }
    RenderBlock::ToolCall(ToolCallBlock::Other(block))
}
/// Plain-text body of a media-variant tool that returned `ToolOutput::Text`
/// rather than a media file (the free / X Basic EchoAgent-upsell short-circuit).
/// `None` for real media outputs — including ZDR upload-only results — so their
/// typed rendering is untouched.
fn media_gen_text(tc: &acp::ToolCall) -> Option<String> {
    match serde_json::from_value::<ToolOutput>(tc.raw_output.clone()?).ok()? {
        ToolOutput::Text(t) => (!t.text.is_empty()).then_some(t.text),
        _ => None,
    }
}
/// Local `(path, is_video)` from typed `raw_output`.
///
/// Returns `None` when `raw_output` is missing/unparseable, not a media
/// variant, or has no openable local file (ZDR `uploaded_url` / empty path).
fn media_gen_ref(tc: &acp::ToolCall) -> Option<(std::path::PathBuf, bool)> {
    let (media, is_video) =
        match serde_json::from_value::<ToolOutput>(tc.raw_output.clone()?).ok()? {
            ToolOutput::ImageGen(m) | ToolOutput::ImageEdit(m) => (m, false),
            ToolOutput::ImageToVideo(m) | ToolOutput::ReferenceToVideo(m) => (m, true),
            _ => return None,
        };
    if media.uploaded_url.is_some() || media.path.as_os_str().is_empty() {
        return None;
    }
    Some((media.path, is_video))
}
/// Extract text content from a ContentBlock.
fn extract_text_from_content(content: &acp::ContentBlock) -> String {
    match content {
        acp::ContentBlock::Text(t) => t.text.clone(),
        _ => String::new(),
    }
}
/// Extract text from tool call content blocks.
fn content_text(tc: &acp::ToolCall) -> String {
    tc.content
        .iter()
        .filter_map(|c| match c {
            acp::ToolCallContent::Content(acp::Content {
                content: acp::ContentBlock::Text(t),
                ..
            }) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
/// Check if a tool call is bg-task internal plumbing
/// (get_command_or_subagent_output, kill_command_or_subagent,
/// wait_commands_or_subagents, and the external background-await tool).
///
/// These are suppressed from scrollback because the bg task pane provides
/// visibility into task status and output.
fn is_bg_plumbing_tool(tc: &acp::ToolCall) -> bool {
    matches!(
        tc.title.as_str(),
        // Current names (post-rename)
        "get_command_or_subagent_output" | "kill_command_or_subagent" | "wait_commands_or_subagents"
        // Old names (persisted sessions / replay)
        | "get_task_output" | "kill_task" | "wait_tasks"
        // Intermediate names (mid-rename sessions)
        | "get_task_or_subagent_output" | "kill_task_or_subagent" | "wait_tasks_or_subagents"
        | "AwaitShell" | "Await"
    ) || tc.title.starts_with("Await:")
        || tc.title.starts_with("Sleep ")
        || tc.title.starts_with("Wait tasks:")
        || tc.title.starts_with("Kill task:")
        || tc
            .raw_input
            .as_ref()
            .and_then(|v| v.get("variant"))
            .and_then(|v| v.as_str())
            .is_some_and(|v| matches!(v, "TaskOutput" | "KillTask" | "WaitTasks"))
}
/// Classify a *blocking* suppressed tool into the [`WaitingReason`] the turn is
/// waiting on, or `None` for suppressed tools that don't block the turn (e.g.
/// `kill_*`, todo/goal/scheduler). Mirrors the title/variant matches in
/// [`is_bg_plumbing_tool`] so the spinner can name the wait instead of falling
/// back to a generic "Waiting…".
fn blocking_wait_reason(tc: &acp::ToolCall) -> Option<WaitingReason> {
    let title = tc.title.as_str();
    let variant = tc
        .raw_input
        .as_ref()
        .and_then(|v| v.get("variant"))
        .and_then(|v| v.as_str());
    if matches!(
        title,
        "get_command_or_subagent_output" | "get_task_output" | "get_task_or_subagent_output"
    ) || variant == Some("TaskOutput")
    {
        let task_ids = tc
            .raw_input
            .as_ref()
            .map(task_ids_from_raw_input)
            .unwrap_or_default();
        return Some(WaitingReason::TaskOutput {
            task_ids,
            subject: None,
            waits: timeout_waits(tc.raw_input.as_ref()),
        });
    }
    if matches!(
        title,
        "wait_commands_or_subagents" | "wait_tasks" | "wait_tasks_or_subagents"
    ) || title.starts_with("Wait tasks:")
        || variant == Some("WaitTasks")
    {
        return Some(WaitingReason::TasksComplete);
    }
    if matches!(title, "Await" | "AwaitShell")
        || title.starts_with("Await:")
        || title.starts_with("Sleep ")
    {
        return Some(WaitingReason::Sleep);
    }
    None
}
/// Whether the wait tool call actually blocks: `timeout_ms > 0` in raw_input.
/// Missing input / missing field / 0 all mean an instant poll.
fn timeout_waits(raw: Option<&serde_json::Value>) -> bool {
    raw.and_then(|v| v.get("timeout_ms"))
        .and_then(|v| v.as_u64())
        .is_some_and(|t| t > 0)
}
/// Extract `task_ids` from a `get_task_output` / wait tool's raw_input JSON.
fn task_ids_from_raw_input(raw: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();
    if let Some(arr) = raw.get("task_ids").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(id) = v.as_str() {
                let id = id.trim();
                if !id.is_empty() && seen.insert(id.to_string()) {
                    out.push(id.to_string());
                }
            }
        }
    }
    if out.is_empty()
        && let Some(id) = raw.get("task_id").and_then(|v| v.as_str())
    {
        let id = id.trim();
        if !id.is_empty() {
            out.push(id.to_string());
        }
    }
    out
}
/// Check if a tool call is a background execute (`is_background=true`).
///
/// These are deferred from scrollback — the `echo.agent/task_backgrounded`
/// notification creates a `BgTask` block instead of an `Execute` block.
///
/// Eager ACP messages often use `kind=Other` with `title=run_terminal_command`
/// before the kind is refined to Execute — still treat those as execute tools
/// when `raw_input` requests background so we don't flash the function name.
fn is_bg_tool(tc: &acp::ToolCall) -> bool {
    let looks_like_execute =
        tc.kind == acp::ToolKind::Execute || is_execute_tool_function_name(&tc.title);
    looks_like_execute
        && tc
            .raw_input
            .as_ref()
            .and_then(|v| v.get("is_background").or_else(|| v.get("background")))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
}
/// Check if an Edit-kind tool call is a whole-file write (write)
/// rather than a targeted replacement (search_replace / edit).
///
/// Detection: a Write-family `rawInput.variant` tag.
fn is_write_tool(tc: &acp::ToolCall) -> bool {
    is_write_variant(
        tc.raw_input
            .as_ref()
            .and_then(|v| v.get("variant"))
            .and_then(|v| v.as_str()),
    )
}
/// Extract the serde variant tag from a tool call's `raw_input.variant`.
///
/// Shared helper for all `is_*_tool` suppression checks — avoids
/// duplicating the `.as_ref()?.get("variant")?.as_str()` chain.
fn extract_variant(tc: &acp::ToolCall) -> Option<&str> {
    tc.raw_input.as_ref()?.get("variant")?.as_str()
}
/// Twin without the optional-toolset spelling.
fn is_task_variant(variant: Option<&str>) -> bool {
    matches!(variant, Some("Task"))
}
/// Twin without the optional-toolset spelling.
fn is_write_variant(variant: Option<&str>) -> bool {
    matches!(variant, Some("Write"))
}
/// Twin without the optional-toolset spelling.
fn is_todo_variant(variant: Option<&str>) -> bool {
    matches!(variant, Some("TodoWrite"))
}
/// Check if a tool call is a todo-related tool.
///
/// Suppressed from scrollback because the dedicated todo pane provides
/// better visibility. Covers the `todo_write` / `TodoWrite` ids, the
/// `Updating plan` title, and TodoWrite-family variant tags.
fn is_todo_tool(tc: &acp::ToolCall) -> bool {
    matches!(
        tc.title.as_str(),
        "todo_write" | "TodoWrite" | "Updating plan"
    ) || is_todo_variant(extract_variant(tc))
}
/// Check if a tool call is a task tool (subagent spawn).
///
/// Suppressed from scrollback because the SubagentBlock (created from
/// SubagentSpawned notification) provides better visibility. Covers the
/// `task` / `Task` / `spawn_subagent` ids and Task-family variant tags.
fn is_task_tool(tc: &acp::ToolCall) -> bool {
    echo_agent_tools::is_task_tool_id(&tc.title) || is_task_variant(extract_variant(tc))
}
fn is_goal_tool(tc: &acp::ToolCall) -> bool {
    tc.title == "update_goal"
        || tc.title.starts_with("Goal:")
        || matches!(extract_variant(tc), Some("UpdateGoal" | "WorkflowSignal"))
}
fn is_workflow_tool(tc: &acp::ToolCall) -> bool {
    let is_workflow = tc.title == "workflow" || matches!(extract_variant(tc), Some("Workflow"));
    if !is_workflow {
        return false;
    }
    let validate_only = tc.title.starts_with("Validating workflow")
        || tc
            .raw_input
            .as_ref()
            .and_then(|v| v.get("validate_only"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    !validate_only
}
/// Check if a tool call is a scheduler tool (scheduler_create/delete/list).
///
/// Suppressed from scrollback because the tasks pane provides visibility.
/// Uses convention-based prefixes rather than exhaustive names.
fn is_scheduler_tool(tc: &acp::ToolCall) -> bool {
    tc.title.starts_with("scheduler_")
        || extract_variant(tc).is_some_and(|v| v.starts_with("Scheduler"))
}
/// Extract a string field from raw_input JSON.
fn extract_raw_field(tc: &acp::ToolCall, field: &str) -> Option<String> {
    tc.raw_input
        .as_ref()
        .and_then(|v| v.get(field))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}
/// Extract a short, user-friendly error label from a failed Edit tool call.
fn extract_edit_error(tc: &acp::ToolCall) -> String {
    use echo_agent_tools::types::output::SearchReplaceOutput;
    if let Some(ref raw) = tc.raw_output
        && let Ok(ToolOutput::SearchReplace(sr)) = serde_json::from_value::<ToolOutput>(raw.clone())
    {
        return match sr {
            SearchReplaceOutput::InvalidInput(_) => "Invalid input".to_owned(),
            SearchReplaceOutput::FileNotFound(_) => "File not found".to_owned(),
            SearchReplaceOutput::MultipleMatchesFound(_) => "Multiple matches found".to_owned(),
            SearchReplaceOutput::FileAlreadyExists(_) => "File already exists".to_owned(),
            SearchReplaceOutput::FilenameTooLong(_) => "Filename too long".to_owned(),
            SearchReplaceOutput::NoMatchesFound(_) => "No matches found".to_owned(),
            SearchReplaceOutput::EditsApplied(_) => "Edit failed".to_owned(),
        };
    }
    "Edit failed".to_owned()
}
/// Extract search input metadata from a tool call's rawInput.
fn extract_search_meta(tc: &acp::ToolCall) -> SearchInputMeta {
    let raw = match tc.raw_input.as_ref() {
        Some(v) => v,
        None => return SearchInputMeta::default(),
    };
    let str_field = |name: &str| -> Option<String> {
        raw.get(name)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    };
    let bool_field =
        |name: &str| -> bool { raw.get(name).and_then(|v| v.as_bool()).unwrap_or(false) };
    let output_mode_str = raw.get("output_mode").and_then(|v| v.as_str());
    let path = str_field("path")
        .or_else(|| str_field("target_directory"))
        .map(|p| make_relative_path(&p))
        .filter(|p| p != ".");
    SearchInputMeta {
        path,
        glob: str_field("glob"),
        output_mode: SearchOutputMode::from_str_opt(output_mode_str),
        case_insensitive: bool_field("-i"),
        file_type: str_field("type"),
        multiline: bool_field("multiline"),
    }
}
/// Extract BashOutput from a serde_json::Value containing ToolOutput::Bash.
fn extract_bash_output_from_value(raw: &Option<serde_json::Value>) -> Option<BashOutput> {
    let val = raw.as_ref()?;
    match serde_json::from_value::<ToolOutput>(val.clone()) {
        Ok(ToolOutput::Bash(bash)) => Some(bash),
        _ => None,
    }
}
/// Extracted grep search results.
#[derive(Default)]
struct GrepResult {
    match_count: usize,
    file_matches: Vec<SearchFileMatch>,
    /// File paths only (for files_with_matches output mode).
    file_paths: Vec<String>,
}
/// Extract grep search results from raw_output.
fn extract_grep_output(raw: &Option<serde_json::Value>) -> Option<GrepResult> {
    let val = raw.as_ref()?;
    match serde_json::from_value::<ToolOutput>(val.clone()) {
        Ok(ToolOutput::GrepSearch(grep)) => {
            let file_matches: Vec<SearchFileMatch> = grep
                .file_matches
                .into_iter()
                .map(|fm| SearchFileMatch {
                    path: make_relative_path(&fm.path),
                    matches: fm
                        .matches
                        .into_iter()
                        .map(|m| SearchLineMatch {
                            line_number: m.line_number,
                            content: m.content,
                        })
                        .collect(),
                })
                .collect();
            let file_paths = if file_matches.is_empty() && grep.match_count > 0 {
                let stdout_str = String::from_utf8_lossy(&grep.stdout);
                parse_file_paths_from_stdout(&stdout_str)
            } else {
                vec![]
            };
            Some(GrepResult {
                match_count: grep.match_count,
                file_matches,
                file_paths,
            })
        }
        _ => None,
    }
}
/// Parse file paths from grep stdout in workspace_result XML format.
///
/// The stdout format is:
/// ```text
/// <workspace_result workspace_path="/path">
/// Found N files
/// /path/to/file1.rs
/// /path/to/file2.rs
/// </workspace_result>
/// ```
fn parse_file_paths_from_stdout(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('<') && !line.starts_with("Found "))
        .map(make_relative_path)
        .collect()
}
/// Extract directory listing content from rawOutput.
fn extract_listdir_content(raw: &Option<serde_json::Value>) -> Option<String> {
    let val = raw.as_ref()?;
    match serde_json::from_value::<ToolOutput>(val.clone()) {
        Ok(ToolOutput::ListDir(echo_agent_tools::types::output::ListDirOutput::Content(c))) => {
            Some(c.content)
        }
        _ => None,
    }
}
/// Extract the agent's advertised toolset from
/// `AvailableCommandsUpdate.meta`.
///
/// Wire format set by the shell: `{"tools": ["read_file", ...]}`.
/// Returns `None` if `meta` is absent, has no `tools` array, or the
/// array contains no string entries (defensive against future shape
/// drift). An empty `Vec` would mean "the shell told us there are zero
/// tools" -- pager `CommandRegistry::set_available_tools(empty)` then
/// hides every tool-gated command.
fn parse_tools_meta(meta: Option<&acp::Meta>) -> Option<Vec<String>> {
    let arr = meta?.get("tools")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
    )
}
/// Compact one-line description of a `SessionUpdate` for the always-on
/// `acp_update` log target.
///
/// Deliberately avoids serializing payloads: emits variant names, ids,
/// statuses, and *sizes* only, so the line stays O(100B) no matter how large
/// the update is. Full payloads go to the opt-in `acp_update_payload` target.
fn update_summary(update: &acp::SessionUpdate) -> String {
    match update {
        acp::SessionUpdate::UserMessageChunk(chunk) => {
            format!(
                "user_message_chunk {}",
                content_block_summary(&chunk.content)
            )
        }
        acp::SessionUpdate::AgentMessageChunk(chunk) => {
            format!(
                "agent_message_chunk {}",
                content_block_summary(&chunk.content)
            )
        }
        acp::SessionUpdate::AgentThoughtChunk(chunk) => {
            format!(
                "agent_thought_chunk {}",
                content_block_summary(&chunk.content)
            )
        }
        acp::SessionUpdate::ToolCall(tc) => {
            format!(
                "tool_call id={} kind={:?} status={:?} title={:?} content={} raw_input={}",
                tc.tool_call_id.0,
                tc.kind,
                tc.status,
                tc.title,
                tc.content.len(),
                tc.raw_input
                    .as_ref()
                    .map_or_else(|| "none".to_string(), json_size_hint),
            )
        }
        acp::SessionUpdate::ToolCallUpdate(tcu) => {
            let f = &tcu.fields;
            format!(
                "tool_call_update id={} status={:?} title={:?} content={} raw_output={}",
                tcu.tool_call_id.0,
                f.status,
                f.title,
                f.content
                    .as_ref()
                    .map_or_else(|| "none".to_string(), |c| c.len().to_string()),
                f.raw_output
                    .as_ref()
                    .map_or_else(|| "none".to_string(), json_size_hint),
            )
        }
        acp::SessionUpdate::Plan(plan) => format!("plan entries={}", plan.entries.len()),
        acp::SessionUpdate::AvailableCommandsUpdate(u) => {
            format!(
                "available_commands_update commands={}",
                u.available_commands.len()
            )
        }
        acp::SessionUpdate::CurrentModeUpdate(u) => {
            format!("current_mode_update mode={}", u.current_mode_id.0)
        }
        _ => "unknown_update".to_string(),
    }
}
/// Compact description of a `ContentBlock`: type plus payload size in bytes.
fn content_block_summary(content: &acp::ContentBlock) -> String {
    match content {
        acp::ContentBlock::Text(t) => format!("text={}B", t.text.len()),
        acp::ContentBlock::Image(i) => {
            format!("image={}B mime={}", i.data.len(), i.mime_type)
        }
        acp::ContentBlock::Audio(a) => {
            format!("audio={}B mime={}", a.data.len(), a.mime_type)
        }
        acp::ContentBlock::ResourceLink(r) => format!("resource_link={}", r.uri),
        acp::ContentBlock::Resource(_) => "resource".to_string(),
        _ => "unknown_content".to_string(),
    }
}
/// Cheap size descriptor for a `serde_json::Value` without serializing it.
///
/// Strings report byte length; arrays report element count (bash raw_output
/// is a `Vec<u8>`, so element count == output bytes); objects report key
/// count plus the summed size of direct string/array members (one level, no
/// recursion). This keeps the cost O(top-level members), never O(payload).
fn json_size_hint(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::String(s) => format!("str({}B)", s.len()),
        Value::Array(a) => format!("arr({})", a.len()),
        Value::Object(o) => {
            let inner: usize = o
                .values()
                .map(|m| match m {
                    Value::String(s) => s.len(),
                    Value::Array(a) => a.len(),
                    _ => 0,
                })
                .sum();
            format!("obj({} keys, ~{}B)", o.len(), inner)
        }
    }
}
/// Compact rendering of the interesting `NotificationMeta` fields.
fn meta_summary(meta: &NotificationMeta) -> String {
    format!(
        "seq={} tokens={} prompt={} stream_start={}",
        meta.event_seq
            .map_or_else(|| "-".to_string(), |v| v.to_string()),
        meta.total_tokens
            .map_or_else(|| "-".to_string(), |v| v.to_string()),
        meta.prompt_id.as_deref().unwrap_or("-"),
        meta.stream_start_ms
            .map_or_else(|| "-".to_string(), |v| v.to_string()),
    )
}
/// Parse the JSON content from a SearchToolOutput into DiscoveredTool entries.
///
/// Results are grouped by server: `{"results": [{"server": "...", "tools": [...]}]}`.
/// Each tool has `tool_name`, `description`, `score`, and `input_schema`.
fn parse_search_tool_results(content: &str) -> Vec<DiscoveredTool> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(content) else {
        return Vec::new();
    };
    let Some(groups) = val.get("results").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for group in groups {
        let server = group
            .get("server")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let Some(tools) = group.get("tools").and_then(|v| v.as_array()) else {
            continue;
        };
        for r in tools {
            let Some(name) = r.get("tool_name").and_then(|v| v.as_str()) else {
                continue;
            };
            let description = r
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            out.push(DiscoveredTool {
                name: name.to_owned(),
                server: server.clone(),
                description,
                score,
            });
        }
    }
    out
}
/// Extract output text from a use_tool's raw_output.
///
/// MCP tools don't put content in ACP content blocks — they only set raw_output.
/// This extracts the text from ToolOutput::MCP, ToolOutput::Text, or
/// ToolOutput::Dynamic variants.
fn extract_use_tool_output(raw: &Option<serde_json::Value>) -> Option<String> {
    let val = raw.as_ref()?;
    if let Ok(output) = serde_json::from_value::<ToolOutput>(val.clone()) {
        let text = match output {
            ToolOutput::MCP(mcp) => {
                use echo_agent_tools::types::output::MCPOutputDetails;
                match mcp.output() {
                    MCPOutputDetails::OkayOutput(s) | MCPOutputDetails::Error(s) => s.clone(),
                }
            }
            ToolOutput::Text(text) => text.text,
            ToolOutput::Dynamic(v) => {
                return Some(serde_json::to_string_pretty(&v).unwrap_or_default());
            }
            _ => return None,
        };
        return Some(maybe_pretty_json(&text));
    }
    val.as_str().map(maybe_pretty_json)
}
/// If the string is valid JSON, pretty-print it. Otherwise return as-is.
fn maybe_pretty_json(s: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| s.to_owned())
    } else {
        s.to_owned()
    }
}
/// Extract input arguments from a use_tool call's raw_input.tool_input.
///
/// Flattens the top-level JSON object into key-value string pairs for display.
/// Nested objects/arrays are rendered as compact JSON strings.
fn extract_use_tool_args(tc: &acp::ToolCall) -> Vec<(String, String)> {
    let Some(raw) = tc.raw_input.as_ref() else {
        return Vec::new();
    };
    let Some(tool_input) = raw.get("tool_input") else {
        return Vec::new();
    };
    let Some(obj) = tool_input.as_object() else {
        return Vec::new();
    };
    obj.iter()
        .map(|(k, v)| {
            let display = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => "null".to_owned(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            (k.clone(), display)
        })
        .collect()
}
/// Convert an absolute path to relative by stripping the current working directory.
fn make_relative_path(path: &str) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = cwd.to_string_lossy();
        if let Some(rel) = path.strip_prefix(cwd_str.as_ref()) {
            let rel = rel.strip_prefix('/').unwrap_or(rel);
            return if rel.is_empty() {
                ".".to_string()
            } else {
                rel.to_string()
            };
        }
    }
    path.to_string()
}
#[cfg(test)]
#[path = "tracker_tests.rs"]
mod tests;
