// Modified by EchoAgent for the embedded desktop runtime; see ECHOAGENT_VENDOR.md.
use chrono::{DateTime, Utc};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::StorageMode;

use crate::remote::RemoteSync;

use crate::sampling::Client as OaiCompatClient;
use crate::sampling::ConversationItem;
use crate::session::export::ExportedMetadata;
use echo_agent_workspace::session::file_state::RewindPoint;

use crate::session::signals::SessionSignals;
use crate::session::storage::relocation::{RelocationError, RelocationView};
use crate::session::storage::{JsonlStorageAdapter, StorageAdapter};
use crate::tools::todo::TodoState;
use crate::util::echo_agent_home::echo_agent_home;
use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
use echo_agent_sampling_types::ReasoningEffort;

use crate::extensions::notification::{
    DISK_FULL_ERROR_TYPE, DISK_FULL_USER_MESSAGE, RetryState,
    SessionNotification as EchoAgentSessionNotification, SessionUpdate as EchoAgentSessionUpdate,
};
use crate::session::info::Info;
use tokio::sync::{mpsc, watch};

/// Current chat history format version.
/// - Version 0: Legacy ChatRequestMessage format (default for old sessions)
/// - Version 1: ConversationItem format (used for new sessions)
pub const CHAT_FORMAT_VERSION: u8 = 1;

/// Maximum Unicode scalars in a session title (`/rename`, dashboard editor,
/// and the `echo.agent/session/rename` ext boundary). Counted after control-strip
/// and trim.
pub const MAX_TITLE_SCALARS: usize = 100;

/// UTF-8 byte ceiling before we bother stripping controls. 4 bytes/scalar
/// plus slack so a handful of C0 bytes that will be stripped don't trip a
/// false reject; anything larger is already over the scalar cap.
pub const MAX_TITLE_BYTES: usize = MAX_TITLE_SCALARS * 4 + 64;

/// C0/C1 plus the bidi/format overrides the dashboard rename editor
/// already rejects. Shared by persist-drop and display-FFFD so the
/// character class cannot drift.
#[inline]
pub fn is_forbidden_title_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

/// Drop C0/C1 and bidi/format controls, then trim. The ext boundary, pull
/// hydrate, and pager ingest share this so a title cannot carry terminal
/// escapes or RTL overrides into `display_name` / `summary.json`.
///
/// Already-clean input is borrowed (trim is a subslice); only a title
/// that actually contains forbidden chars allocates.
pub fn sanitize_rename_title(title: &str) -> Cow<'_, str> {
    if title.chars().any(is_forbidden_title_char) {
        let mut cleaned: String = title
            .chars()
            .filter(|c| !is_forbidden_title_char(*c))
            .collect();
        let trimmed = cleaned.trim();
        if trimmed.len() != cleaned.len() {
            cleaned = trimmed.to_string();
        }
        Cow::Owned(cleaned)
    } else {
        Cow::Borrowed(title.trim())
    }
}

/// Sanitize then cap. `None` when the result is blank. Overlong titles are
/// truncated (ingest/pull defense); the ext rename path rejects instead.
pub fn sanitize_and_cap_title(title: &str) -> Option<String> {
    let cleaned = sanitize_rename_title(title);
    if cleaned.is_empty() {
        return None;
    }
    if cleaned.chars().count() <= MAX_TITLE_SCALARS {
        Some(cleaned.into_owned())
    } else {
        Some(cleaned.chars().take(MAX_TITLE_SCALARS).collect())
    }
}

#[derive(Debug, Clone)]
pub struct PersistenceContentChunk {
    content_chunks: Vec<acp::ContentBlock>,
}

impl PersistenceContentChunk {
    pub(crate) fn new(content_chunks: Vec<acp::ContentBlock>) -> Self {
        Self { content_chunks }
    }

    /// Text that represents what the user actually typed. Desktop and skill
    /// clients can attach a compact `displayText` while sending expanded model
    /// context in the block body. Titles must use that display value or hidden
    /// expert/skill instructions become user-facing session metadata.
    fn title_source_text(self) -> String {
        if let Some(display) = self.content_chunks.iter().find_map(|content_chunk| {
            let acp::ContentBlock::Text(text) = content_chunk else {
                return None;
            };
            text.meta
                .as_ref()
                .and_then(|meta| meta.get("displayText").or_else(|| meta.get("display_text")))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .map(str::to_string)
        }) {
            return display;
        }

        self.content_chunks
            .into_iter()
            .filter_map(|content_chunk| match content_chunk {
                acp::ContentBlock::Text(text) => Some(text.text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Read only the opening user message from the durable ACP transcript. Unlike
/// model context, this survives compaction; unlike search prompt extraction,
/// it preserves displayText and a textless first message.
async fn load_initial_title_source(
    storage: &dyn StorageAdapter,
    info: &Info,
) -> io::Result<Option<String>> {
    let Some(path) = storage.updates_file_path(info) else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || {
        let Some(updates) = crate::session::storage::UpdatesIterator::open(&path)? else {
            return Ok(None);
        };
        let mut blocks = Vec::new();
        let mut opening_prompt = None;
        for update in updates {
            if let crate::session::storage::SessionUpdate::Acp(notification) = update? {
                if let acp::SessionUpdate::UserMessageChunk(chunk) = notification.update {
                    let prompt = (
                        chunk
                            .meta
                            .as_ref()
                            .and_then(|meta| meta.get("promptIndex"))
                            .and_then(|value| value.as_u64()),
                        notification
                            .meta
                            .as_ref()
                            .and_then(|meta| meta.get("promptId"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                    );
                    // Cancelled turns can leave consecutive user messages with
                    // no assistant chunk between them. Keep their sources apart.
                    if !blocks.is_empty() && opening_prompt.as_ref() != Some(&prompt) {
                        break;
                    }
                    opening_prompt = Some(prompt);
                    blocks.push(chunk.content);
                    continue;
                }
            }
            if !blocks.is_empty() {
                break;
            }
        }
        Ok((!blocks.is_empty()).then(|| PersistenceContentChunk::new(blocks).title_source_text()))
    })
    .await
    .map_err(io::Error::other)?
}

/// Mirrors generated titles to the session registry after local persistence succeeds.
#[derive(Clone)]
pub(crate) struct RegistryGeneratedTitleSync {
    pub client: crate::agent::session_registry_client::SessionRegistryClient,
    pub suppress_for_zdr: bool,
}

use crate::session::storage::SessionUpdate;
use serde::{Deserialize, Serialize};

// /btw side question persistence types

/// A single /btw side question entry persisted to `btw_history.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BtwEntry {
    /// Unique ID for this side question.
    pub btw_session_id: String,
    /// The parent session ID.
    pub parent_session_id: String,
    /// When the question was asked.
    pub asked_at: DateTime<Utc>,
    /// The user's question.
    pub question: String,
    /// The model's response (empty if failed).
    pub answer: String,
    /// Model used.
    pub model: String,
    /// Whether the request succeeded.
    pub success: bool,
    /// Error message if failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Model-call attempts made (1 = no retry). Entries written before this
    /// field existed deserialize as 1.
    #[serde(default = "default_btw_attempts")]
    pub attempts: u32,
}

fn default_btw_attempts() -> u32 {
    1
}

// Local feedback persistence types

/// A feedback entry persisted to `~/.echo-agent/sessions/.../feedback.jsonl`.
///
/// Uses a tagged enum so different feedback types are self-describing in the
/// JSONL file (currently only `UserFeedback`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalFeedbackEntry {
    /// Regular user feedback (spontaneous or solicited via heuristics)
    UserFeedback(UserFeedbackEntry),
}

/// A user feedback entry (thumbs, stars, text, or dismiss).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserFeedbackEntry {
    pub submitted_at: DateTime<Utc>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_number: Option<i64>,
    /// Whether this was a response to a server-initiated FeedbackRequest
    pub solicited: bool,
    /// The feedback request ID (only set for solicited feedback)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// True if the user dismissed the feedback request without responding
    #[serde(default, skip_serializing_if = "is_false")]
    pub dismissed: bool,
    /// The full submission payload (omitted when dismissed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submission: Option<echo_agent_chat_proxy_types::feedback_types::FeedbackSubmission>,
}

/// Helper for `#[serde(skip_serializing_if)]` on bool fields.
pub(crate) fn is_false(v: &bool) -> bool {
    !v
}

#[cfg(test)]
#[path = "persistence_feedback_tests.rs"]
mod feedback_tests;

#[derive(Debug, Clone)]
pub struct CopiedSessionFile {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct SessionStateCopy {
    pub files: Vec<CopiedSessionFile>,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum PersistenceMsg {
    /// A session update (ACP update or EchoAgent extension update)
    Update(SessionUpdate),
    AppendUpdateDurablyAndAck {
        update: SessionUpdate,
        respond_to:
            tokio::sync::oneshot::Sender<Result<(), crate::session::storage::AppendUpdateError>>,
    },
    ContentChunk(PersistenceContentChunk),
    Chat(ConversationItem),
    AppendCwdSwitchAndAck {
        item: ConversationItem,
        respond_to: tokio::sync::oneshot::Sender<
            Result<
                echo_agent_chat_state::StrictAppendAck,
                echo_agent_chat_state::StrictAppendError,
            >,
        >,
    },
    /// Replace the entire chat history (used for compaction)
    ReplaceChatHistory(Vec<ConversationItem>),
    /// Destructive image-strip rewrite: back up the on-disk history first,
    /// and only rewrite if the backup landed: recoverability gates the
    /// destruction. Acks the combined disk outcome.
    ReplaceChatHistoryForStripAndAck {
        messages: Vec<ConversationItem>,
        respond_to: tokio::sync::oneshot::Sender<io::Result<()>>,
    },
    CurrentModel {
        model_id: acp::ModelId,
        /// The active agent definition name (e.g. `"echo-agent-build"`).
        /// Persisted in `summary.agent_name` so session resume doesn't depend
        /// on the mutable model catalog.
        agent_name: Option<String>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
    },
    PlanState(TodoState),
    /// Plan mode lifecycle state to persist
    PlanModeState(crate::session::plan_mode::PlanModeSnapshot),
    /// A rewind point to persist
    RewindPoint(RewindPoint),
    /// Truncate rewind points from a specific prompt index (inclusive).
    /// Syncs the persisted file with the in-memory FileStateTracker after rewind.
    TruncateRewindPoints {
        from_index: usize,
    },
    /// Merge rewind points at indices >= `target_index` into the previous point
    /// (read-modify-write on disk, after a ConversationOnly rewind). Disk is
    /// authoritative, so a partial in-memory tracker can't truncate history.
    MergeRewindPointsFrom {
        target_index: usize,
    },
    /// Collection ID for telemetry tracing
    CollectionId(String),
    /// Monotonic telemetry turn counter and optional request_id for trace metadata/filenames.
    /// This is the "next turn" value (i.e., after increment).
    NextTraceTurn {
        next_trace_turn: u64,
        request_id: Option<String>,
    },
    /// Persist a snapshot of the session signals.
    Signals(SessionSignals),
    /// Persist announcement tracking state (MCP + skill announcement dedup).
    AnnouncementState(crate::session::announcement_state::AnnouncementState),
    /// Persist goal mode orchestration state.
    GoalModeState(crate::session::goal_tracker::GoalOrchestration),
    DeleteGoalModeState {
        respond_to: tokio::sync::oneshot::Sender<io::Result<()>>,
    },
    WorkflowRunState(crate::session::workflow::store::WorkflowRunManifest),
    WorkflowRunStateAndAck {
        manifest: crate::session::workflow::store::WorkflowRunManifest,
        respond_to: tokio::sync::oneshot::Sender<io::Result<()>>,
    },
    DeleteWorkflowRunState(String),
    /// Persist a local feedback entry (user feedback)
    Feedback(LocalFeedbackEntry),
    /// Persist a /btw side question entry
    Btw(BtwEntry),
    /// Persist updated HEAD commit and branch to summary.
    GitHead {
        commit: Option<String>,
        branch: Option<String>,
    },
    /// Persist a compaction checkpoint file to `compaction_checkpoints/{id}.json`.
    CompactionCheckpoint(crate::extensions::notification::CompactionCheckpointFile),
    /// Persist a compaction request+response artifact to
    /// `compaction_requests/{request_id}.json`. Used for offline prompt
    /// iteration — captures the exact ConversationItem list sent to the
    /// compaction model plus the summary it returned (or the final error).
    /// The file rides on the post-turn session archive to cloud storage automatically;
    /// no separate upload path is needed.
    CompactionRequest(crate::extensions::notification::CompactionRequestFile),
    /// Persist a recap request+response artifact to
    /// `recap_requests/{request_id}.json`. Same GCS ride-along as
    /// compaction requests; enables offline recap prompt / garble replay.
    RecapRequest(crate::extensions::notification::RecapRequestFile),
    /// Persist a compaction segment (`Segments` mode).
    CompactionSegment(crate::extensions::notification::CompactionSegmentFile),
    /// Generated session title from background LLM task.
    /// Routed back through the persistence channel so the storage write
    /// stays sequential with other summary.json mutations.
    GeneratedTitle(String),
    /// Compatibility request from the legacy refresh path. Only fills an
    /// absent title; established titles are preserved under the summary lock.
    RegenerateTitle(String),
    /// Persist a bounded preview of the latest session recap so listing
    /// surfaces can show it whenever available; `None` clears it (rewind
    /// removed the described turns).
    LastRecap(Option<String>),
    /// Manual `/rename` title. Rides this FIFO channel so the resulting
    /// `SetTitle` cannot race a `GeneratedTitle` `SetTitle` out-of-band.
    ManualTitleRenamed(String),
    /// `/rename --auto`: reset [`crate::session::summary::SummaryGenerator`]
    /// so the next content chunk regenerates. Storage is already cleared by
    /// the ext handler; remote stores stay untouched until the fresh auto
    /// title is adopted.
    ResetTitleToAuto,
    /// Per-turn dashboard summary as `(text, prompt_id)`; replaces (`Some`)
    /// or clears (`None`, on conversation rewind) the previous one in
    /// `summary.json`.
    LastTurnSummary(Option<(String, String)>),
    /// Enable remote writeback for a session created `Local` before remote
    /// settings resolved (non-blocking startup); backfills its local history.
    UpgradeToWriteback {
        auth_manager: Arc<crate::auth::AuthManager>,
    },
    Flush,
    /// Flush all pending writes, then signal the caller once the flush is complete.
    /// Unlike `Flush` (fire-and-forget), this is a **sync barrier**: the caller's
    /// oneshot only resolves after `flush_pending()` finishes writing to disk.
    FlushAndAck {
        respond_to: tokio::sync::oneshot::Sender<io::Result<()>>,
    },
    /// Checked host teardown: drain retained updates, then confirm local files
    /// reached the durable sync barrier. Existing CLI/remote flush stays intact.
    FlushAndSyncAndAck {
        respond_to: tokio::sync::oneshot::Sender<io::Result<()>>,
    },
    ProbeWritable {
        respond_to: tokio::sync::oneshot::Sender<io::Result<()>>,
    },
    /// Flush all pending writes, then copy the current session directory contents and return
    /// the in-memory snapshot to the caller (who can tar.gz + upload to GCS, etc.).
    CopyFile {
        one_shot: tokio::sync::oneshot::Sender<anyhow::Result<SessionStateCopy>>,
    },
}

pub use echo_agent_shared::session::session_dir;

type RelocationResult<T> = crate::session::storage::relocation::Result<T>;
type SummaryReader = fn(&Path) -> RelocationResult<Summary>;

fn storage_view(sessions_root: &Path) -> RelocationResult<RelocationView> {
    RelocationView::load_for_sessions_root(sessions_root)
}

/// Check if a session exists locally under the given cwd.
///
/// This is the correct check for the `-r` resume path: a session is only
/// "already local" if it lives under the **same** cwd as the current invocation.
/// A session stored under a different cwd does NOT satisfy this check — the
/// caller must still run the remote restore into the requested cwd.
pub fn session_exists_for_cwd(session_id: &str, cwd: &str) -> bool {
    let sessions_root = crate::util::echo_agent_home::echo_agent_home().join("sessions");
    session_exists_for_cwd_in_root(session_id, cwd, &sessions_root)
}

/// A directory is a resumable session only if it has a `summary.json`; this
/// skips `images/`-only stubs that would otherwise hijack `--resume`. Used by
/// the resume/restore resolution path; `find_session_dir_by_id` intentionally
/// stays dir-only for non-resume compatibility.
fn is_persisted_session_dir(session_path: &Path) -> bool {
    session_path.join("summary.json").is_file()
}

/// Inner implementation of `session_exists_for_cwd` with an injectable root.
/// Separated for deterministic tempdir-based tests.
fn session_exists_for_cwd_in_root(session_id: &str, cwd: &str, sessions_root: &Path) -> bool {
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);
    let session_path = sessions_root.join(&encoded).join(session_id);
    is_persisted_session_dir(&session_path)
}

/// Find the local child session id that was previously restored from `remote_session_id`
/// in the given `cwd`.
///
/// When a remote session is restored, a new local child is created with
/// `summary.parent_session_id == remote_session_id`.  On a second
/// `echoagent -r <remote_id>` in the same cwd, this function returns the already-restored
/// child so no duplicate restore is performed.
///
/// If multiple children match (e.g., from pre-fix duplicate restores), the
/// most recently used one is returned.  Selection is fully deterministic:
/// 1. Newest `updated_at` timestamp in `summary.json`
/// 2. Newest session directory mtime as a tie-breaker (catches equal timestamps)
/// 3. Lexicographically largest session id as the final stable tie-breaker
///
/// Returns `Some(local_child_id)` when at least one matching child is found.
/// Returns `None` when no child with `parent_session_id == remote_session_id` exists.
pub fn find_local_child_for_remote(remote_session_id: &str, cwd: &str) -> Option<String> {
    let sessions_root = crate::util::echo_agent_home::echo_agent_home().join("sessions");
    find_local_child_for_remote_in_root(remote_session_id, cwd, &sessions_root)
}

/// Resolve a session ID to one that is available locally under `cwd`.
///
/// Checks in order:
///   1. `session_id` exists directly under `cwd` → returns it as-is.
///   2. A previously restored child of `session_id` exists → returns the child ID.
///   3. Neither found → returns `None` (caller should restore from remote).
pub fn resolve_local_session(session_id: &str, cwd: &str) -> Option<String> {
    if session_exists_for_cwd(session_id, cwd) {
        return Some(session_id.to_string());
    }
    find_local_child_for_remote(session_id, cwd)
}

// Repo-wide session resolution (for worktree resume)

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LocalSessionResolutionKind {
    ExactCwd,
    RestoredChildInExactCwd,
    SameRepoDifferentCwd,
    RestoredChildInSameRepoDifferentCwd,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolvedLocalSession {
    pub session_id: String,
    pub cwd: String,
    pub resolution_kind: LocalSessionResolutionKind,
}

/// Resolve a session across multiple candidate cwds for worktree resume.
///
/// The first cwd in `candidate_cwds` should be the exact current cwd so it
/// gets priority. For each candidate, checks both direct session existence
/// and previously-restored children.
///
/// Returns `None` when no local match exists in any candidate.
pub(crate) fn resolve_local_session_for_repo(
    session_id: &str,
    candidate_cwds: &[&str],
) -> Option<ResolvedLocalSession> {
    let sessions_root = crate::util::echo_agent_home::echo_agent_home().join("sessions");
    resolve_local_session_for_repo_in_root(session_id, candidate_cwds, &sessions_root)
}

pub(crate) fn resolve_local_session_for_repo_in_root(
    session_id: &str,
    candidate_cwds: &[&str],
    sessions_root: &Path,
) -> Option<ResolvedLocalSession> {
    for (i, &cwd) in candidate_cwds.iter().enumerate() {
        let is_exact = i == 0;

        if session_exists_for_cwd_in_root(session_id, cwd, sessions_root) {
            return Some(ResolvedLocalSession {
                session_id: session_id.to_owned(),
                cwd: cwd.to_owned(),
                resolution_kind: if is_exact {
                    LocalSessionResolutionKind::ExactCwd
                } else {
                    LocalSessionResolutionKind::SameRepoDifferentCwd
                },
            });
        }

        if let Some(child_id) = find_local_child_for_remote_in_root(session_id, cwd, sessions_root)
        {
            return Some(ResolvedLocalSession {
                session_id: child_id,
                cwd: cwd.to_owned(),
                resolution_kind: if is_exact {
                    LocalSessionResolutionKind::RestoredChildInExactCwd
                } else {
                    LocalSessionResolutionKind::RestoredChildInSameRepoDifferentCwd
                },
            });
        }
    }
    None
}
fn find_local_child_for_remote_in_root(
    remote_session_id: &str,
    cwd: &str,
    sessions_root: &Path,
) -> Option<String> {
    let encoded = crate::util::echo_agent_home::encode_cwd_dirname(cwd);
    let cwd_dir = sessions_root.join(&encoded);
    if !cwd_dir.exists() {
        return None;
    }

    // Collect all matching children.  Multiple can exist when a user ran
    // `echoagent -r <remote_id>` before this fix was deployed.
    // Tuple: (updated_at, dir_mtime_nanos, session_id) — all sorted descending.
    let mut candidates: Vec<(String, u128, String)> = Vec::new();

    let entries = std::fs::read_dir(&cwd_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let summary_path = path.join("summary.json");
        if !summary_path.exists() {
            continue;
        }
        // Parse minimum fields without deserializing the full Summary,
        // so we don't fail on missing/extra fields from older/newer formats.
        if let Ok(raw) = std::fs::read_to_string(&summary_path)
            && let Ok(partial) = serde_json::from_str::<serde_json::Value>(&raw)
            && partial.get("parent_session_id").and_then(|v| v.as_str()) == Some(remote_session_id)
            && let Some(session_id) = path.file_name().and_then(|n| n.to_str())
        {
            let updated_at = partial
                .get("updated_at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Directory mtime as a tie-breaker for equal updated_at values.
            let dir_mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            candidates.push((updated_at, dir_mtime, session_id.to_string()));
        }
    }

    // Sort descending by all three keys for full determinism.
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
    candidates.into_iter().next().map(|(_, _, id)| id)
}

/// Check if a session exists locally by session ID.
/// Searches across ALL cwd directories under `~/.echo-agent/sessions/`.
///
/// Use `session_exists_for_cwd` instead when the target cwd is known
/// (e.g., the `-r` resume path) to avoid false-positive matches.
/// Find a session by ID across **all** CWD directories under `~/.echo-agent/sessions/`.
///
/// Unlike [`resolve_local_session`] which only checks a single CWD,
/// this scans every encoded-CWD subdirectory. Returns the decoded CWD path
/// that contains the session, or `None` if not found anywhere.
///
/// This is used by the pager's `--resume` to find sessions that were created
/// in a different CWD (e.g., a worktree) than the one the user is currently in.
pub fn resolve_local_session_any_cwd(session_id: &str) -> Option<String> {
    resolve_local_session_any_cwd_result(session_id)
        .ok()
        .flatten()
}

pub(crate) fn resolve_local_session_any_cwd_result(session_id: &str) -> io::Result<Option<String>> {
    resolve_local_session_any_cwd_in_root(session_id, &echo_agent_home().join("sessions"))
        .map_err(io::Error::other)
}

fn resolve_local_session_any_cwd_in_root(
    session_id: &str,
    sessions_root: &Path,
) -> Result<Option<String>, crate::session::storage::relocation::RelocationError> {
    let Some(session_path) = storage_view(sessions_root)?.find_persisted_session_dir(session_id)?
    else {
        return Ok(None);
    };
    Ok(session_path
        .parent()
        .and_then(crate::util::echo_agent_home::decode_cwd_from_dirname))
}

/// Scan all CWD directories for a session and return its directory path.
pub fn find_session_dir_by_id(session_id: &str) -> Option<PathBuf> {
    find_any_session_dir_by_id_result(session_id).ok().flatten()
}

pub(crate) fn find_persisted_session_dir_by_id_result(
    session_id: &str,
) -> io::Result<Option<PathBuf>> {
    find_persisted_session_dir_by_id_in_root_result(session_id, &echo_agent_home().join("sessions"))
}

pub(crate) fn find_persisted_session_dir_by_id_in_root_result(
    session_id: &str,
    sessions_root: &Path,
) -> io::Result<Option<PathBuf>> {
    storage_view(sessions_root)
        .and_then(|view| view.find_persisted_session_dir(session_id))
        .map_err(io::Error::other)
}

pub(crate) fn find_any_session_dir_by_id_result(session_id: &str) -> io::Result<Option<PathBuf>> {
    storage_view(&echo_agent_home().join("sessions"))
        .and_then(|view| view.find_any_session_dir(session_id))
        .map_err(io::Error::other)
}

#[cfg(test)]
fn session_exists_in_root(session_id: &str, sessions_root: &Path) -> bool {
    find_persisted_session_dir_by_id_in_root_result(session_id, sessions_root)
        .is_ok_and(|path| path.is_some())
}

/// Whether a session dir's `summary.json` records a manual `/rename`
/// (`false` if missing/unreadable). Cheap read for paths that only need the
/// manual flag without loading the full session.
pub(crate) fn title_is_manual_in_dir(session_dir: &Path) -> bool {
    std::fs::read(session_dir.join("summary.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Summary>(&bytes).ok())
        .is_some_and(|summary| summary.title_is_manual)
}

/// Find and read a session summary given only its ID (scans all CWD directories).
pub(crate) fn find_summary_by_session_id(session_id: &str) -> Option<Summary> {
    find_summary_by_session_id_in_root(session_id, &echo_agent_home().join("sessions"))
}

/// Inner implementation with injectable root for testing.
pub(crate) fn find_summary_by_session_id_in_root(
    session_id: &str,
    sessions_root: &Path,
) -> Option<Summary> {
    let path = storage_view(sessions_root)
        .ok()?
        .find_persisted_session_dir(session_id)
        .ok()
        .flatten()?;
    read_summary_from_dir(&path).ok()
}

fn read_summary_from_dir(session_dir: &Path) -> RelocationResult<Summary> {
    let path = session_dir.join("summary.json");
    let bytes = std::fs::read(&path).map_err(|error| RelocationError::Io {
        operation: "read",
        path: path.clone(),
        source: error,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| RelocationError::Json { path, source })
}

/// The most recently updated local session summary for `cwd` (by
/// `last_active_at` else `updated_at`), or `None` if there are no local sessions
/// for that cwd. Sync and local-only — suitable for the startup path that must
/// resolve the sandbox profile before the (irreversible) OS sandbox is applied.
fn most_recent_local_summary_for_cwd_in_root(cwd: &str, sessions_root: &Path) -> Option<Summary> {
    most_recent_local_summary_for_cwd_in_view(
        cwd,
        &storage_view(sessions_root).ok()?,
        read_summary_from_dir,
    )
    .ok()
    .flatten()
}

fn most_recent_local_summary_for_cwd_in_view(
    cwd: &str,
    view: &RelocationView,
    read_summary: SummaryReader,
) -> RelocationResult<Option<Summary>> {
    let mut best: Option<Summary> = None;
    for session_dir in view.session_dirs(Some(cwd))? {
        let summary = match read_summary(&session_dir) {
            Ok(summary) => summary,
            Err(RelocationError::Json { .. }) => continue,
            Err(RelocationError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => return Err(error),
        };
        if summary.is_hidden() {
            continue;
        }
        if best.as_ref().is_none_or(|current| {
            let time = summary.last_active_at.unwrap_or(summary.updated_at);
            let current_time = current.last_active_at.unwrap_or(current.updated_at);
            time > current_time
                || (time == current_time && summary.info.id.0.as_ref() < current.info.id.0.as_ref())
        }) {
            best = Some(summary);
        }
    }
    Ok(best)
}

/// Sync, local-only session summaries for `cwd` (hidden sessions filtered).
/// For startup paths that must resolve a resume target before the
/// irreversible OS sandbox is applied; async callers use [`list_summaries`].
///
/// Listing failures propagate so pre-sandbox callers can fail closed;
/// individual unreadable summaries are skipped, matching the async path's
/// tolerance for a single corrupt file.
pub fn local_summaries_for_cwd_sync(cwd: &str) -> io::Result<Vec<Summary>> {
    local_summaries_for_cwd_sync_in_root(cwd, &echo_agent_home().join("sessions"))
}

fn local_summaries_for_cwd_sync_in_root(
    cwd: &str,
    sessions_root: &Path,
) -> io::Result<Vec<Summary>> {
    let view = storage_view(sessions_root).map_err(io::Error::other)?;
    let dirs = view.session_dirs(Some(cwd)).map_err(io::Error::other)?;
    Ok(dirs
        .iter()
        .filter_map(|dir| read_summary_from_dir(dir).ok())
        .filter(|s| !s.is_hidden())
        .collect())
}

/// Best-effort lookup of the sandbox profile persisted with a session that is
/// about to be resumed, used at startup to restore the session's profile before
/// the (irreversible) OS sandbox is applied.
///
/// - `session_id`: the explicit id from `--resume <id>` / `--load <id>` /
///   `-s <id>`. Resolved directly across all cwds, then — for a remote id that
///   was restored into a local child — via that child's `parent_session_id`.
/// - `cwd`: the current working directory. Used to resolve a remote id to its
///   local child, and as the lookup key for `-c` / `--continue` and bare
///   `--resume` (most-recent-for-cwd).
///
/// Returns `None` when not resuming, the session isn't found locally, or it has
/// no persisted profile (sessions created before this was tracked) — callers
/// then fall back to the normal config/CLI resolution.
pub fn resumed_session_sandbox_profile(
    session_id: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    resumed_session_sandbox_profile_in_root(session_id, cwd, &echo_agent_home().join("sessions"))
}

fn resumed_session_sandbox_profile_in_root(
    session_id: Option<&str>,
    cwd: Option<&str>,
    sessions_root: &Path,
) -> Option<String> {
    if let Some(id) = session_id.filter(|s| !s.is_empty()) {
        // Direct match by id (across all cwds).
        if let Some(summary) = find_summary_by_session_id_in_root(id, sessions_root) {
            return summary.sandbox_profile;
        }
        // A remote id resumes into a local child (fresh id, `parent_session_id`
        // = remote id). Mirror the canonical resume path so the peek doesn't
        // miss the restored session's saved profile.
        if let Some(cwd) = cwd
            && let Some(child) = find_local_child_for_remote_in_root(id, cwd, sessions_root)
        {
            return find_summary_by_session_id_in_root(&child, sessions_root)
                .and_then(|s| s.sandbox_profile);
        }
        return None;
    }
    if let Some(cwd) = cwd {
        return most_recent_local_summary_for_cwd_in_root(cwd, sessions_root)
            .and_then(|s| s.sandbox_profile);
    }
    None
}

/// Get file path for storing a large prompt.
/// Creates the prompts subdirectory if it doesn't exist.
/// Path format: `{session_dir}/prompts/prompt_{prompt_index}.txt`
/// Owner-only session dir + `<encoded-cwd>` shield, for writers that bypass
/// a storage adapter's `init_session` (e.g. chat-kind sessions).
pub(crate) fn ensure_owner_only_session_dir(info: &Info) -> std::io::Result<PathBuf> {
    ensure_owner_only_session_dir_in(&echo_agent_home(), info)
}

/// Inner implementation with an injectable echoagent home for tests.
fn ensure_owner_only_session_dir_in(
    echo_agent_home: &Path,
    info: &Info,
) -> std::io::Result<PathBuf> {
    let _ = crate::util::echo_agent_home::ensure_sessions_cwd_dir_in(echo_agent_home, &info.cwd);
    let dir = session_dir_in(echo_agent_home, info);
    crate::util::echo_agent_home::create_dir_all_owner_only(&dir)?;
    Ok(dir)
}

/// `session_dir` with an injectable echoagent home (pure path computation).
fn session_dir_in(echo_agent_home: &Path, info: &Info) -> PathBuf {
    crate::util::echo_agent_home::sessions_cwd_dir_in(echo_agent_home, &info.cwd)
        .join(info.id.to_string())
}

pub(crate) fn get_prompt_file_path(info: &Info, prompt_index: usize) -> PathBuf {
    get_prompt_file_path_in(&echo_agent_home(), info, prompt_index)
}

/// Inner implementation with an injectable echoagent home for tests.
fn get_prompt_file_path_in(echo_agent_home: &Path, info: &Info, prompt_index: usize) -> PathBuf {
    // Best-effort; failures surface on the prompt-file write itself.
    let _ = ensure_owner_only_session_dir_in(echo_agent_home, info);
    let prompts_dir = session_dir_in(echo_agent_home, info).join("prompts");
    let _ = crate::util::echo_agent_home::create_dir_all_owner_only(&prompts_dir);
    prompts_dir.join(format!("prompt_{}.txt", prompt_index))
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingCwdSwitchReminder {
    pub cwd_generation: u64,
    pub previous_cwd: String,
    #[serde(alias = "cwd")]
    pub destination_cwd: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_project_instructions: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    pub info: Info,
    /// Monotonic generation of the authoritative cwd in `info.cwd`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cwd_generation: u64,
    /// Cwd immediately preceding the current generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_cwd: Option<String>,
    /// Reminder staged for exactly-once append during relocation completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_cwd_switch_reminder: Option<PendingCwdSwitchReminder>,
    /// Latest switch generation reflected in `num_chat_messages` bookkeeping.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cwd_switch_bookkeeping_generation: u64,
    pub session_summary: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub num_messages: usize,
    #[serde(default)]
    pub num_chat_messages: usize,
    pub current_model_id: acp::ModelId,
    /// Parent session ID if this session was forked from another session
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// Timestamp when this session was forked (only set for forked sessions)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_at: Option<DateTime<Utc>>,
    /// Collection ID for telemetry trace uploads (one per session)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
    /// Next telemetry trace turn id (monotonic, persisted).
    /// Used to generate unique turn ids for telemetry metadata/filenames even across rewinds.
    #[serde(default)]
    pub next_trace_turn: u64,
    /// Chat history format version:
    /// - 0 (default): Legacy ChatRequestMessage format
    /// - 1: ConversationItem format
    #[serde(default)]
    pub chat_format_version: u8,
    /// Stable display path for forked sessions.
    ///
    /// When set, the system prompt's `Workspace Path` and prompt metadata
    /// paths show this value instead of the real worktree/overlay path
    /// (`info.cwd`). Persisted so the override survives session
    /// restore/reload without the caller needing to resend it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_display_cwd: Option<String>,
    /// What created this session: `"fork"`, `"subagent"`, `"subagent_fork"`, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
    /// How the session's initial context was bootstrapped: `"new"` or `"forked"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_context_source: Option<String>,
    /// The parent prompt/turn ID that triggered this fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_parent_prompt_id: Option<String>,
    /// Number of conversation items inherited from the parent session.
    /// During compaction, items below this index are preserved as-is
    /// (the "inherited prefix"). Only items after this boundary are
    /// summarized. `None` means no inherited prefix (non-forked session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_prefix_len: Option<usize>,
    /// Visibility override. None = default for `session_kind`, Some = explicit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// The original workspace directory this worktree session was spawned from.
    /// Used by clients to group worktree sessions under their source workspace
    /// regardless of the worktree's actual `cwd`. Only set when
    /// `session_kind == "worktree"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_workspace_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_root_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub git_remotes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Absolute path to the `.echo-agent` directory, used by reconstruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo_agent_home: Option<String>,
    /// When the session last had content added (user or model messages).
    /// Only advanced locally by `append_update` / `append_chat_message`;
    /// never touched by remote registry operations or metadata-only writes.
    /// `None` for sessions created before this field was added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<DateTime<Utc>>,
    /// LLM-generated session title persisted separately from `session_summary`.
    /// When present, this is preferred for display over `session_summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_title: Option<String>,
    /// True when `generated_title` was set by a manual `/rename` (vs auto LLM
    /// title). Manual titles render inline in the prompt's top border on
    /// resume.
    #[serde(default, skip_serializing_if = "is_false")]
    pub title_is_manual: bool,
    /// Human-readable label for the worktree directory (e.g. "nuke-v-tables").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_label: Option<String>,
    /// The agent definition name that was active when the session was last saved.
    /// Used during session resume to avoid re-deriving from the (mutable) model
    /// catalog — if the model is removed or its `agent_type` changes between
    /// sessions, this persisted value ensures the correct harness is restored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    /// The OS sandbox profile this session ran under (e.g. "workspace",
    /// "strict", "off", or a custom name). Persisted so a resumed session is
    /// restored to the same profile instead of silently falling back to the
    /// config default — which would otherwise break commands that worked before
    /// (a stricter profile denies filesystem/network the session relied on).
    /// `None` for sessions created before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Ultra-short summary of the most recent successful turn, shown as the
    /// dashboard row's secondary line (via the roster for non-attached
    /// clients). Displayed until replaced by the next successful turn (or
    /// cleared by a conversation rewind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_summary: Option<String>,
    /// Prompt id of the turn `last_turn_summary` describes (provenance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn_summary_prompt_id: Option<String>,
    /// Bounded preview of the most recent session recap ("where was I"),
    /// persisted so listing surfaces (`/resume`, `/session-info`) can show it
    /// whenever available. Distinct from `last_turn_summary` (a summary of the
    /// final turn only). Regenerated on demand by `/recap`; this holds the last
    /// committed value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recap: Option<String>,
}

/// Current `echo_agent_home` as a UTF-8 string, or `None` if the path isn't valid UTF-8.
pub(crate) fn echo_agent_home_string() -> Option<String> {
    crate::util::echo_agent_home::echo_agent_home()
        .to_str()
        .map(String::from)
}

pub fn default_model_id() -> acp::ModelId {
    acp::ModelId::new(crate::models::default_model())
}

impl Summary {
    pub(crate) fn new(info: &Info, model_id: acp::ModelId) -> std::io::Result<Self> {
        let git_metadata =
            echo_agent_workspace::session::git::resolve_persisted_session_git_metadata_sync(
                std::path::Path::new(&info.cwd),
            );
        Ok(Self {
            info: info.clone(),
            cwd_generation: 0,
            previous_cwd: None,
            pending_cwd_switch_reminder: None,
            cwd_switch_bookkeeping_generation: 0,
            session_summary: String::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            num_messages: 0,
            num_chat_messages: 0,
            current_model_id: model_id,
            parent_session_id: None,
            forked_at: None,
            collection_id: None,
            next_trace_turn: 0,
            chat_format_version: CHAT_FORMAT_VERSION,
            prompt_display_cwd: None,
            session_kind: None,
            fork_context_source: None,
            fork_parent_prompt_id: None,
            inherited_prefix_len: None,
            hidden: None,
            source_workspace_dir: None,
            git_root_dir: git_metadata.git_root_dir,
            git_remotes: git_metadata.git_remotes,
            head_commit: git_metadata.head_commit,
            head_branch: git_metadata.head_branch,
            request_id: None,
            echo_agent_home: echo_agent_home_string(),
            last_active_at: None,
            generated_title: None,
            title_is_manual: false,
            worktree_label: crate::session::worktree::lookup_worktree_label(&info.cwd),
            agent_name: None,
            sandbox_profile: None,
            reasoning_effort: None,
            last_turn_summary: None,
            last_turn_summary_prompt_id: None,
            last_recap: None,
        })
    }

    /// Whether this session should be excluded from history listings.
    pub fn is_hidden(&self) -> bool {
        self.hidden.unwrap_or(
            self.session_kind
                .as_deref()
                .is_some_and(|k| k.starts_with("subagent")),
        )
    }

    /// Preferred display title: `generated_title` if non-empty, else `session_summary`.
    pub fn display_title(&self) -> &str {
        let generated = self
            .generated_title
            .as_deref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty());
        if let Some(title) = generated
            && (self.title_is_manual
                || (!crate::session::helpers::session_summary::contains_internal_reasoning_markup(
                    title,
                ) && !crate::session::helpers::session_summary::contains_expert_persona_markup(
                    title,
                )))
        {
            return title;
        }

        let fallback = self.session_summary.trim();
        if !self.title_is_manual
            && (crate::session::helpers::session_summary::contains_internal_reasoning_markup(
                fallback,
            ) || crate::session::helpers::session_summary::contains_expert_persona_markup(
                fallback,
            ))
        {
            ""
        } else {
            fallback
        }
    }

    /// [`Self::display_title`] as an `Option`, `None` when blank.
    pub fn display_title_opt(&self) -> Option<String> {
        let title = self.display_title().trim();
        (!title.is_empty()).then(|| title.to_string())
    }

    /// The manually-`/rename`d title (trimmed), `None` for auto-generated or
    /// blank titles. Binds to `generated_title` — the field `title_is_manual`
    /// describes — never the `session_summary` display fallback, so a stale
    /// flag over a blank manual title can't relabel an auto summary as
    /// manual. When `Some`, it equals [`Self::display_title_opt`] (a
    /// non-blank `generated_title` wins the display chain).
    pub fn manual_title_opt(&self) -> Option<String> {
        self.title_is_manual
            .then_some(self.generated_title.as_deref())
            .flatten()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    }

    /// Last-change time (unix millis): `last_active_at`, else `updated_at`.
    pub fn last_change_unix_ms(&self) -> i64 {
        self.last_active_at
            .unwrap_or(self.updated_at)
            .timestamp_millis()
    }
}

#[cfg(test)]
#[path = "persistence_is_hidden_tests.rs"]
mod is_hidden_tests;

#[cfg(test)]
#[path = "persistence_head_fields_tests.rs"]
mod head_fields_tests;

#[cfg(test)]
#[path = "persistence_generated_title_tests.rs"]
mod generated_title_tests;

#[derive(Clone)]
pub struct PersistenceHandle {
    pub tx: mpsc::UnboundedSender<PersistenceMsg>,
    noop: bool,
    disk_full_rx: watch::Receiver<bool>,
}

fn actor_channel() -> (
    PersistenceHandle,
    mpsc::UnboundedReceiver<PersistenceMsg>,
    mpsc::WeakUnboundedSender<PersistenceMsg>,
    watch::Sender<bool>,
) {
    let (tx, rx) = mpsc::unbounded_channel::<PersistenceMsg>();
    let (disk_full_tx, disk_full_rx) = watch::channel(false);
    let weak = tx.downgrade();
    let handle = PersistenceHandle {
        tx,
        noop: false,
        disk_full_rx,
    };
    (handle, rx, weak, disk_full_tx)
}

#[derive(Debug)]
pub(crate) enum DurableAppendError {
    NotCommitted(io::Error),
    Committed(io::Error),
    AcknowledgementLost(io::Error),
}

impl std::fmt::Display for DurableAppendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCommitted(error)
            | Self::Committed(error)
            | Self::AcknowledgementLost(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DurableAppendError {}

impl From<crate::session::storage::AppendUpdateError> for DurableAppendError {
    fn from(error: crate::session::storage::AppendUpdateError) -> Self {
        use crate::session::storage::AppendUpdateError;
        match error {
            AppendUpdateError::NotCommitted(error) => Self::NotCommitted(error),
            AppendUpdateError::Committed(error)
                if crate::session::storage::is_committed_barrier_error(&error) =>
            {
                Self::AcknowledgementLost(error)
            }
            AppendUpdateError::Committed(error) => Self::Committed(error),
        }
    }
}

impl PersistenceHandle {
    #[cfg(test)]
    pub(crate) fn from_sender_for_test(tx: mpsc::UnboundedSender<PersistenceMsg>) -> Self {
        Self::from_parts_for_test(tx, watch::channel(false).1)
    }

    #[cfg(test)]
    pub(crate) fn from_parts_for_test(
        tx: mpsc::UnboundedSender<PersistenceMsg>,
        disk_full_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            tx,
            noop: false,
            disk_full_rx,
        }
    }

    pub fn noop() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self {
            tx,
            noop: true,
            disk_full_rx: watch::channel(false).1,
        }
    }

    pub fn is_noop(&self) -> bool {
        self.noop
    }

    #[cfg(test)]
    pub(crate) fn is_disk_full(&self) -> bool {
        *self.disk_full_rx.borrow()
    }

    pub(crate) fn subscribe_disk_full(&self) -> watch::Receiver<bool> {
        self.disk_full_rx.clone()
    }

    /// Append after older buffered updates and wait for the durable barrier.
    ///
    /// [`DurableAppendError::NotCommitted`] is safe to retry; [`DurableAppendError::Committed`]
    /// means the replay line landed; [`DurableAppendError::AcknowledgementLost`] has unknown status.
    /// No-op handles return `Unsupported`.
    pub(crate) async fn append_update_durably(
        &self,
        update: SessionUpdate,
    ) -> Result<(), DurableAppendError> {
        if self.noop {
            return Err(DurableAppendError::NotCommitted(io::Error::new(
                io::ErrorKind::Unsupported,
                "durable session update append is unsupported by a no-op persistence handle",
            )));
        }
        let (respond_to, response) = tokio::sync::oneshot::channel();
        self.tx
            .send(PersistenceMsg::AppendUpdateDurablyAndAck { update, respond_to })
            .map_err(|_| {
                DurableAppendError::NotCommitted(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "session persistence actor stopped before durable append dispatch",
                ))
            })?;
        response
            .await
            .map_err(|_| {
                DurableAppendError::AcknowledgementLost(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "session persistence actor stopped before durable append acknowledgement",
                ))
            })?
            .map_err(DurableAppendError::from)
    }
}

#[cfg(test)]
enum PendingAppendOutcome {
    CommittedOk(acp::SessionNotification),
    CommittedErr(acp::SessionNotification, io::Error),
    NotCommittedErr(acp::SessionNotification, io::Error),
}

#[derive(Clone)]
enum PendingChatWrite {
    Append(ConversationItem),
    Replace(Vec<ConversationItem>),
    /// A line landed but its bookkeeping/sync failed. Retry the final save,
    /// never the append, before allowing a later cache operation to overtake it.
    SyncCommitted,
}

struct SessionPersistence {
    info: Info,
    storage: Arc<dyn StorageAdapter>,
    /// Pending ACP notification for merging consecutive text chunks
    pending_notification: Option<acp::SessionNotification>,
    /// Uncommitted updates retain FIFO order ahead of the newest merge buffer.
    /// A committed error is removed here, so retry never duplicates its line.
    pending_updates: VecDeque<SessionUpdate>,
    pending_chat_writes: VecDeque<PendingChatWrite>,
    pending_plan_modes: VecDeque<crate::session::plan_mode::PlanModeSnapshot>,
    rx: mpsc::UnboundedReceiver<PersistenceMsg>,
    remote_sync: Option<RemoteSync>,
    /// True only for sessions created this run (not resumed); gates the
    /// writeback backfill so a resumed, already-synced session isn't re-sent.
    created_fresh: bool,
    /// WebSocket-based relay sync for real-time session sharing.
    /// This streams updates to the relay backend in addition to local persistence.
    relay_sync: Option<crate::relay::RelaySync>,
    /// Session title generation lifecycle.
    summary: crate::session::summary::SummaryGenerator,
    registry_title_sync: Option<RegistryGeneratedTitleSync>,
    /// Client gateway for `SessionSummaryGenerated` notifications. Used to
    /// announce an auto-generated title only once it has actually been adopted
    /// (see the `GeneratedTitle` handler), so a title rejected for racing a
    /// manual `/rename` never reaches the client. `None` for the subagent
    /// variant, whose lifecycle notifications are handled by the coordinator.
    gateway: Option<GatewaySender>,
    /// Read every turn, not at construction, so a session opened before the
    /// decision landed still indexes.
    search_index: crate::session::storage::search::SharedSearchIndex,
    disk_full_tx: watch::Sender<bool>,
    disk_full_notified: bool,
}

impl SessionPersistence {
    fn try_merge_text(prev: &mut acp::ContentBlock, new: &acp::ContentBlock) -> bool {
        match (prev, new) {
            (acp::ContentBlock::Text(prev_text), acp::ContentBlock::Text(new_text))
                if prev_text.annotations.is_none()
                    && prev_text.meta.is_none()
                    && new_text.annotations.is_none()
                    && new_text.meta.is_none() =>
            {
                prev_text.text.push_str(&new_text.text);
                true
            }
            _ => false,
        }
    }

    // Empty chunks are chunks that have no content and no meta.
    fn is_empty_chunk(update: &acp::SessionUpdate) -> bool {
        match update {
            acp::SessionUpdate::AgentMessageChunk(chunk)
            | acp::SessionUpdate::AgentThoughtChunk(chunk) => {
                let empty_text =
                    matches!(&chunk.content, acp::ContentBlock::Text(t) if t.text.is_empty());
                let no_meta = chunk.meta.is_none();
                empty_text && no_meta
            }
            _ => false,
        }
    }

    /// Attempt to merge consecutive ACP text notifications to reduce storage writes.
    /// Returns Some(notification) if the pending notification should be written now.
    fn maybe_merge_notification(
        &mut self,
        incoming: &acp::SessionNotification,
    ) -> Option<acp::SessionNotification> {
        // Always skip empty chunks - don't store them at all
        if Self::is_empty_chunk(&incoming.update) {
            return None;
        }

        let Some(pending) = self.pending_notification.take() else {
            self.pending_notification = Some(incoming.clone());
            return None;
        };

        let pending_update = pending.update.clone();
        match (&incoming.update, pending_update) {
            (
                acp::SessionUpdate::AgentMessageChunk(new_chunk),
                acp::SessionUpdate::AgentMessageChunk(mut pending_chunk),
            )
            | (
                acp::SessionUpdate::AgentThoughtChunk(new_chunk),
                acp::SessionUpdate::AgentThoughtChunk(mut pending_chunk),
            ) => {
                let did_merge = pending_chunk.meta.is_none()
                    && new_chunk.meta.is_none()
                    && Self::try_merge_text(&mut pending_chunk.content, &new_chunk.content);

                if did_merge {
                    let merged_update = match &incoming.update {
                        acp::SessionUpdate::AgentMessageChunk(_) => {
                            acp::SessionUpdate::AgentMessageChunk(pending_chunk)
                        }
                        acp::SessionUpdate::AgentThoughtChunk(_) => {
                            acp::SessionUpdate::AgentThoughtChunk(pending_chunk)
                        }
                        _ => unreachable!(),
                    };
                    self.pending_notification = Some(
                        acp::SessionNotification::new(incoming.session_id.clone(), merged_update)
                            .meta(incoming.meta.clone()),
                    );
                    None
                } else {
                    self.pending_notification = Some(incoming.clone());
                    Some(pending)
                }
            }
            _ => {
                self.pending_notification = Some(incoming.clone());
                Some(pending)
            }
        }
    }

    async fn write_update(
        &mut self,
        update: &SessionUpdate,
    ) -> Result<(), crate::session::storage::AppendUpdateError> {
        let result = self
            .storage
            .append_update_commit_aware(&self.info, update)
            .await;
        self.observe_append_update(&result);
        // ContentChunk arrives before the buffered transcript reaches disk.
        // Refresh after the durable terminal so a fast first index cannot keep
        // an empty document until the next process-wide bootstrap. Failed
        // appends must not publish a new search version.
        if result.is_ok()
            && matches!(update, SessionUpdate::EchoAgent(notification)
                if matches!(&notification.update, EchoAgentSessionUpdate::TurnCompleted { .. }))
        {
            crate::session::storage::search::notify_session_updated(
                self.search_index.decision().writer(),
                &self.info.id.to_string(),
                &self.info.cwd,
            );
        }
        result
    }

    fn observe_io<T>(&mut self, result: &io::Result<T>) {
        match result {
            Ok(_) => self.clear_disk_full(),
            Err(error) if is_disk_full_io_error(error) => self.mark_disk_full(),
            Err(_) => {}
        }
    }

    fn observe_append_update(
        &mut self,
        result: &Result<(), crate::session::storage::AppendUpdateError>,
    ) {
        match result {
            Ok(()) => self.clear_disk_full(),
            Err(
                crate::session::storage::AppendUpdateError::NotCommitted(error)
                | crate::session::storage::AppendUpdateError::Committed(error),
            ) if is_disk_full_io_error(error) => self.mark_disk_full(),
            Err(_) => {}
        }
    }

    fn mark_disk_full(&mut self) {
        if !*self.disk_full_tx.borrow() {
            let _ = self.disk_full_tx.send(true);
        }
        if self.disk_full_notified {
            return;
        }
        self.disk_full_notified = true;
        self.emit_disk_full_notification();
    }

    fn clear_disk_full(&mut self) {
        if *self.disk_full_tx.borrow() {
            let _ = self.disk_full_tx.send(false);
        }
        self.disk_full_notified = false;
    }

    fn emit_disk_full_notification(&self) {
        let Some(gateway) = &self.gateway else {
            return;
        };
        let notification = EchoAgentSessionNotification {
            session_id: self.info.id.clone(),
            update: EchoAgentSessionUpdate::RetryState(RetryState::Failed {
                error_type: DISK_FULL_ERROR_TYPE.to_string(),
                message: DISK_FULL_USER_MESSAGE.to_string(),
            }),
            meta: None,
        };
        if let Ok(params) = serde_json::value::to_raw_value(&notification) {
            gateway.forward_fire_and_forget(acp::ExtNotification::new(
                "echo.agent/session_notification",
                params.into(),
            ));
        }
    }

    async fn probe_writable(&self) -> io::Result<()> {
        let dir = self
            .storage
            .updates_file_path(&self.info)
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "session directory is unknown; cannot probe disk space",
                )
            })?;
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir)?;
            let probe = dir.join(".disk_ok");
            std::fs::write(&probe, b"ok")?;
            let _ = std::fs::remove_file(&probe);
            io::Result::Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }

    fn queue_acp_sync(&self, notification: acp::SessionNotification) {
        if let Some(sync) = &self.remote_sync {
            sync.queue(notification.clone());
        }
        if let Some(relay) = &self.relay_sync {
            relay.queue(notification);
        }
    }

    /// Enable writeback for a session created `Local` before settings resolved:
    /// build the sync and (for a fresh session) backfill its local-only history.
    /// No-op once syncing, so a repeat upgrade is harmless.
    async fn upgrade_to_writeback(&mut self, auth_manager: Arc<crate::auth::AuthManager>) {
        if self.remote_sync.is_some() {
            return;
        }
        // Flush the merge-pending notification so the backfill re-reads it.
        let _ = self.flush_pending().await;
        let persisted = match self.storage.load_session(&self.info).await {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(%error, "writeback upgrade: failed to load session for backfill");
                return;
            }
        };
        let remote_sync = match init_remote_sync(
            &persisted.summary,
            StorageMode::Writeback,
            Some(auth_manager),
        ) {
            Ok(Some(remote_sync)) => remote_sync,
            // ZDR team, or nothing to do: leave the session local-only.
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "writeback upgrade: remote sync init failed");
                return;
            }
        };
        // Fresh-only backfill; see `backfill_updates_to_sync`.
        let backfilled =
            backfill_updates_to_sync(self.created_fresh, persisted.updates, &remote_sync);
        if self.created_fresh {
            tracing::info!(
                session_id = %self.info.id,
                backfilled,
                "writeback enabled after settings arrival; backfilled local-only history",
            );
        } else {
            tracing::info!(
                session_id = %self.info.id,
                "writeback enabled for resumed session; forward-only, no backfill",
            );
        }
        self.remote_sync = Some(remote_sync);
    }

    #[cfg(test)]
    fn finish_pending_append(
        notification: acp::SessionNotification,
        result: Result<(), crate::session::storage::AppendUpdateError>,
    ) -> PendingAppendOutcome {
        match result {
            Ok(()) => PendingAppendOutcome::CommittedOk(notification),
            Err(crate::session::storage::AppendUpdateError::NotCommitted(error)) => {
                PendingAppendOutcome::NotCommittedErr(notification, error)
            }
            Err(crate::session::storage::AppendUpdateError::Committed(error)) => {
                PendingAppendOutcome::CommittedErr(notification, error)
            }
        }
    }

    fn queue_pending_notification(&mut self) {
        if let Some(notification) = self.pending_notification.take() {
            self.pending_updates
                .push_back(SessionUpdate::Acp(Box::new(notification)));
        }
    }

    async fn drain_updates(&mut self) -> Result<(), crate::session::storage::AppendUpdateError> {
        while let Some(update) = self.pending_updates.front().cloned() {
            let result = self.write_update(&update).await;
            match result {
                Err(error @ crate::session::storage::AppendUpdateError::NotCommitted(_)) => {
                    // Keep this record ahead of all subsequent updates.
                    return Err(error);
                }
                committed => {
                    self.pending_updates.pop_front();
                    if let SessionUpdate::Acp(notification) = update {
                        self.queue_acp_sync(*notification);
                    }
                    // The line landed even if a derived summary write failed.
                    // Report that error, but never append this record again.
                    committed?;
                }
            }
        }
        Ok(())
    }

    /// Restore uncommitted failures; sync committed records before returning errors.
    async fn drain_pending(&mut self) -> Result<(), crate::session::storage::AppendUpdateError> {
        self.queue_pending_notification();
        self.drain_updates().await
    }

    async fn drain_chat_writes(&mut self) -> io::Result<()> {
        use crate::session::storage::AppendUpdateError;
        while let Some(operation) = self.pending_chat_writes.front().cloned() {
            let result = match operation {
                PendingChatWrite::Append(message) => {
                    self.storage
                        .append_chat_message_commit_aware(&self.info, &message)
                        .await
                }
                // Full-cache replacement is idempotent, including when its
                // summary update failed after the replacement landed.
                PendingChatWrite::Replace(messages) => self
                    .storage
                    .replace_chat_history(&self.info, &messages)
                    .await
                    .map_err(AppendUpdateError::NotCommitted),
                PendingChatWrite::SyncCommitted => self
                    .storage
                    .sync_session_files_checked(&self.info)
                    .await
                    .map_err(AppendUpdateError::NotCommitted),
            };
            self.observe_append_update(&result);
            match result {
                Ok(()) => {
                    self.pending_chat_writes.pop_front();
                }
                Err(AppendUpdateError::NotCommitted(error)) => return Err(error),
                Err(AppendUpdateError::Committed(error)) => {
                    self.pending_chat_writes.pop_front();
                    self.pending_chat_writes
                        .push_front(PendingChatWrite::SyncCommitted);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    async fn drain_plan_modes(&mut self) -> io::Result<()> {
        while let Some(snapshot) = self.pending_plan_modes.front().cloned() {
            let result = self
                .storage
                .write_plan_mode_state(&self.info, &snapshot)
                .await;
            self.observe_io(&result);
            result?;
            self.pending_plan_modes.pop_front();
        }
        Ok(())
    }

    async fn handle_durable_append(
        &mut self,
        update: SessionUpdate,
    ) -> Result<(), crate::session::storage::AppendUpdateError> {
        // A committed error from an OLDER queued update does not mean this
        // caller's new durable update was appended. Preserve safe retry semantics.
        self.drain_pending().await.map_err(|error| {
            crate::session::storage::AppendUpdateError::NotCommitted(error.into_io_error())
        })?;
        let result = self
            .storage
            .append_update_durable_commit_aware(&self.info, &update)
            .await;
        self.observe_append_update(&result);
        match (&update, &result) {
            (SessionUpdate::Acp(notification), Ok(()))
            | (
                SessionUpdate::Acp(notification),
                Err(crate::session::storage::AppendUpdateError::Committed(_)),
            ) => self.queue_acp_sync((**notification).clone()),
            _ => {}
        }
        result
    }

    /// Flush any pending merged ACP notification to disk and remote sync.
    /// A no-op drain must not clear the disk-full latch.
    async fn flush_pending(&mut self) -> io::Result<()> {
        let updates = self
            .drain_pending()
            .await
            .map_err(crate::session::storage::AppendUpdateError::into_io_error);
        let result = match updates {
            Err(error) => Err(error),
            Ok(()) => match self.drain_chat_writes().await {
                Err(error) => Err(error),
                Ok(()) => self.drain_plan_modes().await,
            },
        };
        if let Err(error) = &result {
            tracing::warn!(%error, "failed to write pending update");
        }
        if let Some(sync) = &self.remote_sync {
            sync.flush();
        }
        if let Some(relay) = &self.relay_sync {
            relay.flush();
        }
        result
    }

    /// Flush pending writes and sync all session files to disk.
    /// Called before CopyFile to ensure all data is persisted.
    async fn flush_and_sync(&mut self) {
        let _ = self.flush_pending().await;
        if let Err(e) = self.storage.sync_session_files(&self.info).await {
            tracing::warn!(?e, "Failed to sync session files to disk");
        }
    }

    /// Announce a newly adopted auto title (first generation or refresh) to the
    /// client, remote store, and session registry. Called only after the title
    /// actually landed on disk, so a title rejected for racing a manual
    /// `/rename` is never announced.
    fn announce_adopted_title(&self, title: String) {
        // The title was committed before this callback; keep FTS title/content
        // hashes aligned even when title generation finishes after the turn.
        crate::session::storage::search::notify_session_updated(
            self.search_index.decision().writer(),
            &self.info.id.to_string(),
            &self.info.cwd,
        );
        crate::session::summary::notify_client(&self.gateway, &self.info, &title);
        if let Some(sync) = &self.remote_sync {
            sync.set_title(title.clone());
        }
        if let Some(reg) = self.registry_title_sync.as_ref()
            && !reg.suppress_for_zdr
        {
            let client = reg.client.clone();
            let sid = self.info.id.to_string();
            tokio::spawn(async move {
                let req = crate::agent::session_registry_client::UpdateRequest {
                    summary: Some(title),
                    first_prompt: None,
                    last_turn_number: None,
                    repo_head_at_end: None,
                    restorable_turn_number: None,
                };
                if let Err(e) = client.update(&sid, &req).await {
                    tracing::warn!(
                        error = %e,
                        session_id = %sid,
                        "session registry summary sync failed after title update"
                    );
                }
            });
        }
    }

    async fn run(mut self) {
        // Persistence traffic counts as worktree activity; debounced so
        // long-resident sessions (leader/remote, active for days without a
        // re-open) stay out of gc expiry without per-message DB writes.
        // The constructors fire the t=0 touch, so this starts at now().
        let mut last_worktree_touch = std::time::Instant::now();
        while let Some(msg) = self.rx.recv().await {
            if last_worktree_touch.elapsed() >= WORKTREE_TOUCH_INTERVAL {
                last_worktree_touch = std::time::Instant::now();
                // Detached on purpose: opportunistic refresh, no ordering need.
                spawn_worktree_touch(&self.info);
            }
            match msg {
                PersistenceMsg::UpgradeToWriteback { auth_manager } => {
                    self.upgrade_to_writeback(auth_manager).await;
                }
                PersistenceMsg::Flush => {
                    let _ = self.flush_pending().await;
                }
                PersistenceMsg::FlushAndAck { respond_to } => {
                    let result = self.flush_pending().await;
                    let _ = respond_to.send(result);
                }
                PersistenceMsg::FlushAndSyncAndAck { respond_to } => {
                    let result = match self.flush_pending().await {
                        Ok(()) => self.storage.sync_session_files_checked(&self.info).await,
                        Err(error) => Err(error),
                    };
                    self.observe_io(&result);
                    let _ = respond_to.send(result);
                }
                PersistenceMsg::ProbeWritable { respond_to } => {
                    let result = self.probe_writable().await;
                    self.observe_io(&result);
                    let _ = respond_to.send(result);
                }
                PersistenceMsg::Update(update) => {
                    match update {
                        SessionUpdate::Acp(notification) => {
                            // ACP notifications use merging to coalesce consecutive text chunks
                            if let Some(to_write) = self.maybe_merge_notification(&notification) {
                                self.pending_updates
                                    .push_back(SessionUpdate::Acp(Box::new(to_write)));
                                if let Err(error) = self.drain_updates().await {
                                    tracing::warn!(%error, "retaining uncommitted session updates for retry");
                                }
                            }
                        }
                        SessionUpdate::EchoAgent(_) => {
                            // An extension event may not overtake an earlier
                            // buffered ACP chunk or a previously failed append.
                            self.queue_pending_notification();
                            self.pending_updates.push_back(update);
                            if let Err(error) = self.drain_updates().await {
                                tracing::warn!(%error, "retaining uncommitted session updates for retry");
                            }
                        }
                    }
                }
                PersistenceMsg::AppendUpdateDurablyAndAck { update, respond_to } => {
                    let result = self.handle_durable_append(update).await;
                    let _ = respond_to.send(result);
                }
                PersistenceMsg::Chat(chat_msg) => {
                    self.pending_chat_writes
                        .push_back(PendingChatWrite::Append(chat_msg));
                    if let Err(error) = self.drain_chat_writes().await {
                        tracing::warn!(%error, "retaining failed chat writes for retry");
                    }
                }
                PersistenceMsg::AppendCwdSwitchAndAck { item, respond_to } => {
                    if let Err(error) = self.drain_chat_writes().await {
                        let _ = respond_to.send(Err(
                            echo_agent_chat_state::StrictAppendError::NotCommitted(error),
                        ));
                        continue;
                    }
                    let result = self
                        .storage
                        .append_cwd_switch_commit_aware(&self.info, &item)
                        .await
                        .map_err(|error| match error {
                            crate::session::storage::AppendCwdSwitchError::NotCommitted(error) => {
                                echo_agent_chat_state::StrictAppendError::NotCommitted(error)
                            }
                            crate::session::storage::AppendCwdSwitchError::Committed {
                                acknowledgement,
                                source,
                            } => echo_agent_chat_state::StrictAppendError::Committed {
                                acknowledgement,
                                source,
                            },
                        });
                    let _ = respond_to.send(result);
                }
                PersistenceMsg::ReplaceChatHistory(messages) => {
                    tracing::info!(
                        num_messages = messages.len(),
                        "Replacing chat history (compaction)"
                    );
                    self.pending_chat_writes
                        .push_back(PendingChatWrite::Replace(messages));
                    if let Err(error) = self.drain_chat_writes().await {
                        tracing::warn!(%error, "retaining failed chat replacement for retry");
                    }
                }
                PersistenceMsg::ReplaceChatHistoryForStripAndAck {
                    messages,
                    respond_to,
                } => {
                    if let Err(error) = self.drain_chat_writes().await {
                        let _ = respond_to.send(Err(error));
                        continue;
                    }
                    // Backup gates the rewrite; see `strip_rewrite_gated`.
                    let result = crate::session::storage::strip_rewrite_gated(
                        self.storage.as_ref(),
                        &self.info,
                        &messages,
                    )
                    .await;
                    self.observe_io(&result);
                    if let Err(e) = &result {
                        tracing::warn!(?e, "image-strip history rewrite failed");
                    }
                    let _ = respond_to.send(result);
                }
                PersistenceMsg::CurrentModel {
                    model_id,
                    agent_name,
                    reasoning_effort,
                } => {
                    if let Err(e) = self
                        .storage
                        .update_current_model_and_agent(
                            &self.info,
                            &model_id,
                            agent_name.as_deref(),
                            reasoning_effort,
                        )
                        .await
                    {
                        tracing::warn!(?e, "failed to update current model");
                    }
                    if let Some(sync) = &self.remote_sync {
                        sync.set_model_id(model_id.0.to_string());
                    }
                }
                PersistenceMsg::PlanState(state) => {
                    if let Err(e) = self.storage.write_plan_state(&self.info, &state).await {
                        tracing::warn!(?e, "failed to write plan state");
                    }
                }
                PersistenceMsg::PlanModeState(state) => {
                    self.pending_plan_modes.push_back(state);
                    if let Err(error) = self.drain_plan_modes().await {
                        tracing::warn!(%error, "retaining failed plan mode state for retry");
                    }
                }
                PersistenceMsg::GoalModeState(state) => {
                    if let Err(e) = self.storage.write_goal_mode_state(&self.info, &state).await {
                        tracing::warn!(?e, "failed to write goal mode state");
                    }
                }
                PersistenceMsg::DeleteGoalModeState { respond_to } => {
                    let result = self.storage.delete_goal_mode_state(&self.info).await;
                    if let Err(e) = &result {
                        tracing::warn!(?e, "failed to delete goal mode state");
                    }
                    let _ = respond_to.send(result);
                }
                PersistenceMsg::WorkflowRunState(manifest) => {
                    if let Err(error) = self
                        .storage
                        .write_workflow_run_state(&self.info, &manifest)
                        .await
                    {
                        tracing::warn!(run_id = %manifest.state.run_id, ?error, "failed to write workflow run state");
                    }
                }
                PersistenceMsg::WorkflowRunStateAndAck {
                    manifest,
                    respond_to,
                } => {
                    let result = self
                        .storage
                        .write_workflow_run_state(&self.info, &manifest)
                        .await;
                    if let Err(error) = &result {
                        tracing::warn!(run_id = %manifest.state.run_id, ?error, "failed to write acknowledged workflow run state");
                    }
                    let _ = respond_to.send(result);
                }
                PersistenceMsg::DeleteWorkflowRunState(run_id) => {
                    if let Err(e) = self
                        .storage
                        .delete_workflow_run_state(&self.info, &run_id)
                        .await
                    {
                        tracing::warn!(%run_id, ?e, "failed to delete workflow run state");
                    }
                }
                PersistenceMsg::ContentChunk(content_chunks) => {
                    let content_part = content_chunks.title_source_text();
                    self.summary.update(content_part);

                    // Notify session search index so this turn becomes searchable
                    crate::session::storage::search::notify_session_updated(
                        self.search_index.decision().writer(),
                        &self.info.id.to_string(),
                        &self.info.cwd,
                    );
                }
                PersistenceMsg::GeneratedTitle(title) => {
                    // Auto-generated titles must never overwrite a title the
                    // user set via `/rename`. `set_generated_title_if_absent`
                    // writes only when the session still has no title (checked
                    // atomically under the summary lock) and reports whether it
                    // did, so a manual rename that raced this generation wins
                    // and its title is not clobbered locally or on remotes.
                    match self
                        .storage
                        .set_generated_title_if_absent(&self.info, title.clone())
                        .await
                    {
                        Ok(true) => self.announce_adopted_title(title),
                        Ok(false) => {
                            tracing::debug!(
                                "skipped auto-generated title; session already has a title"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(?e, "failed to persist generated session title");
                        }
                    }
                }
                PersistenceMsg::RegenerateTitle(title) => {
                    // An old refresh request must not rename an established
                    // conversation or announce a title the storage rejected.
                    match self
                        .storage
                        .regenerate_generated_title(&self.info, title.clone())
                        .await
                    {
                        Ok(true) => self.announce_adopted_title(title),
                        Ok(false) => {
                            tracing::debug!("skipped title refresh; session already has a title");
                        }
                        Err(e) => {
                            tracing::warn!(?e, "failed to persist refreshed session title");
                        }
                    }
                }
                PersistenceMsg::LastRecap(recap) => {
                    if let Err(e) = self.storage.set_last_recap(&self.info, recap).await {
                        tracing::warn!(?e, "failed to persist session recap");
                    }
                }
                PersistenceMsg::ManualTitleRenamed(title) => {
                    if let Some(sync) = &self.remote_sync {
                        sync.set_manual_title(title);
                    }
                }
                PersistenceMsg::ResetTitleToAuto => {
                    self.summary.reset();
                    if let Some(sync) = &self.remote_sync {
                        sync.clear_title();
                    }
                }
                PersistenceMsg::LastTurnSummary(summary) => {
                    if let Err(e) = self
                        .storage
                        .set_last_turn_summary(&self.info, summary)
                        .await
                    {
                        tracing::warn!(?e, "failed to persist last turn summary");
                    }
                }
                PersistenceMsg::RewindPoint(point) => {
                    let result = self.storage.append_rewind_point(&self.info, &point).await;
                    self.observe_io(&result);
                    if let Err(e) = result {
                        tracing::warn!(?e, "failed to write rewind point");
                    }
                }
                PersistenceMsg::TruncateRewindPoints { from_index } => {
                    if let Err(e) = self
                        .storage
                        .truncate_rewind_points_from(&self.info, from_index)
                        .await
                    {
                        tracing::warn!(?e, from_index, "failed to truncate rewind points");
                    }
                }
                PersistenceMsg::MergeRewindPointsFrom { target_index } => {
                    if let Err(e) = self
                        .storage
                        .merge_rewind_points_from(&self.info, target_index)
                        .await
                    {
                        tracing::warn!(?e, target_index, "failed to merge rewind points");
                    }
                }
                PersistenceMsg::CollectionId(collection_id) => {
                    if let Err(e) = self
                        .storage
                        .update_collection_id(&self.info, &collection_id)
                        .await
                    {
                        tracing::warn!(?e, "failed to write collection id");
                    }
                }
                PersistenceMsg::NextTraceTurn {
                    next_trace_turn,
                    request_id,
                } => {
                    if let Err(e) = self
                        .storage
                        .update_next_trace_turn(&self.info, next_trace_turn, request_id.as_deref())
                        .await
                    {
                        tracing::warn!(?e, "failed to write next trace turn");
                    }
                }
                PersistenceMsg::Signals(signals) => {
                    if let Err(e) = self.storage.write_signals(&self.info, &signals).await {
                        tracing::warn!(?e, "failed to write session signals");
                    }
                }
                PersistenceMsg::AnnouncementState(state) => {
                    if let Err(e) = self
                        .storage
                        .write_announcement_state(&self.info, &state)
                        .await
                    {
                        tracing::warn!(?e, "failed to write announcement state");
                    }
                }
                PersistenceMsg::Feedback(entry) => {
                    if let Err(e) = self.storage.append_feedback(&self.info, &entry).await {
                        tracing::warn!(?e, "failed to write feedback entry");
                    }
                }
                PersistenceMsg::Btw(entry) => {
                    if let Err(e) = self.storage.append_btw(&self.info, &entry).await {
                        tracing::warn!(?e, "failed to write btw entry");
                    }
                }
                PersistenceMsg::GitHead { commit, branch } => {
                    if let Err(e) = self
                        .storage
                        .update_git_head(&self.info, commit, branch)
                        .await
                    {
                        tracing::warn!(?e, "failed to persist git HEAD");
                    }
                }
                PersistenceMsg::CompactionCheckpoint(checkpoint) => {
                    if let Err(e) = self
                        .storage
                        .write_compaction_checkpoint(&self.info, &checkpoint)
                        .await
                    {
                        tracing::warn!(?e, "failed to write compaction checkpoint file");
                    }
                }
                PersistenceMsg::CompactionRequest(request) => {
                    if let Err(e) = self
                        .storage
                        .write_compaction_request(&self.info, &request)
                        .await
                    {
                        tracing::warn!(?e, "failed to write compaction request artifact");
                    }
                }
                PersistenceMsg::RecapRequest(request) => {
                    if let Err(e) = self.storage.write_recap_request(&self.info, &request).await {
                        tracing::warn!(?e, "failed to write recap request artifact");
                    }
                }
                PersistenceMsg::CompactionSegment(segment) => {
                    if let Err(e) = self
                        .storage
                        .write_compaction_segment(&self.info, &segment)
                        .await
                    {
                        tracing::warn!(?e, "failed to write compaction segment");
                    }
                }
                PersistenceMsg::CopyFile { one_shot } => {
                    // Flush pending writes and sync all session files to disk before copying.
                    self.flush_and_sync().await;

                    let result = self.copy_session_dir_to_memory().await;
                    let _ = one_shot.send(result);
                }
            }
        }

        // Drain the merge buffer on channel close.
        let _ = self.flush_pending().await;
    }

    async fn copy_session_dir_to_memory(&self) -> anyhow::Result<SessionStateCopy> {
        let session_dir = session_dir(&self.info);
        tokio::task::spawn_blocking(move || {
            let mut files = Vec::new();

            if !session_dir.exists() {
                return Ok(SessionStateCopy { files });
            }

            collect_session_files_recursive(&session_dir, &session_dir, &mut files);
            collect_mcp_stderr_logs(&mut files);

            Ok(SessionStateCopy { files })
        })
        .await?
    }
}

/// Collect MCP server stderr logs from `~/.echo-agent/logs/mcp/` for inclusion in the session archive.
fn collect_mcp_stderr_logs(files: &mut Vec<CopiedSessionFile>) {
    let mcp_log_dir = echo_agent_config::echo_agent_home()
        .join("logs")
        .join("mcp");
    let Ok(entries) = std::fs::read_dir(&mcp_log_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path.extension().is_some_and(|ext| ext == "log")
            && let Ok(data) = std::fs::read(&path)
            && !data.is_empty()
        {
            let name = format!(
                "mcp_stderr/{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            files.push(CopiedSessionFile { name, data });
        }
    }
}

/// Recursively collect all files from `dir` into `files`, using paths relative to `base`.
/// This captures subdirectories like `prompts/` which contain large-prompt files
/// referenced by truncated chat history entries.
fn collect_session_files_recursive(base: &Path, dir: &Path, files: &mut Vec<CopiedSessionFile>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(?dir, ?e, "Failed to read directory during session copy");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let rel_path = match path.strip_prefix(base) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let Some(name) = rel_path.to_str() else {
                continue;
            };
            let data = match std::fs::read(&path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(?e, "Failed to read session file during copy");
                    continue;
                }
            };
            files.push(CopiedSessionFile {
                name: name.to_string(),
                data,
            });
        } else if path.is_dir() {
            collect_session_files_recursive(base, &path, files);
        }
    }
}

/// Queue a fresh session's local-only ACP history to `remote_sync` (EchoAgent updates
/// are never synced), returning the count. Resumed sessions are forward-only:
/// their prior history may already be on the backend (which appends by content,
/// no per-message id), so re-sending would duplicate.
fn backfill_updates_to_sync(
    created_fresh: bool,
    updates: Vec<SessionUpdate>,
    remote_sync: &RemoteSync,
) -> usize {
    if !created_fresh {
        return 0;
    }
    let mut backfilled = 0usize;
    for update in updates {
        if let SessionUpdate::Acp(notification) = update {
            remote_sync.queue(*notification);
            backfilled += 1;
        }
    }
    remote_sync.flush();
    backfilled
}

fn init_remote_sync(
    summary: &Summary,
    storage_mode: StorageMode,
    auth_manager: Option<Arc<crate::auth::AuthManager>>,
) -> io::Result<Option<RemoteSync>> {
    match storage_mode {
        StorageMode::Local => Ok(None),
        StorageMode::Writeback => {
            let auth_manager = auth_manager.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Writeback storage mode requires authentication. Run 'echoagent login' first.",
                )
            })?;
            if let Some(auth) = auth_manager.current_or_expired() {
                if auth.is_zdr_team() {
                    tracing::debug!("ZDR team: skipping remote sync");
                    return Ok(None);
                }
            } else {
                tracing::warn!(
                    "writeback: no auth loaded yet, ZDR check skipped (backend enforces server-side)"
                );
            }
            tracing::info!("Writeback mode enabled, syncing to backend");
            let client =
                crate::remote::BackendClient::new().with_auth_manager(auth_manager.clone());
            let metadata = ExportedMetadata::from_summary(summary);
            Ok(Some(RemoteSync::new(
                summary.info.id.to_string(),
                metadata,
                client,
            )))
        }
    }
}

/// Pull a session from the backend if not found locally. Returns the pulled
/// session's [`Info`] (cwd may differ from caller's on different machines),
/// or `None` if not found or on error.
async fn try_pull_from_remote(info: &Info, client: &crate::remote::BackendClient) -> Option<Info> {
    // BackendClient resolves auth internally via its auth_manager.
    client.auth_manager.as_ref()?;

    tracing::info!(session_id = %info.id, "Session not found locally, trying backend");

    match crate::remote::pull_session_to_local(&info.id.0, client).await {
        Ok(crate::remote::PullResult::Hydrated(pulled_info)) => {
            tracing::info!(
                session_id = %info.id,
                pulled_cwd = %pulled_info.cwd,
                "Pulled session from backend"
            );
            Some(pulled_info)
        }
        Ok(crate::remote::PullResult::NotFound) => {
            tracing::debug!(session_id = %info.id, "Session not found on backend either");
            None
        }
        Err(e) => {
            tracing::warn!(session_id = %info.id, error = %e, "Backend pull failed");
            None
        }
    }
}

pub(crate) fn is_disk_full_io_error(e: &io::Error) -> bool {
    if let Some(source) = crate::session::storage::committed_barrier_source(e) {
        return is_disk_full_io_error(source);
    }
    if e.kind() == io::ErrorKind::StorageFull {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(
            e.raw_os_error(),
            Some(raw) if raw == libc::ENOSPC || raw == libc::EDQUOT
        )
    }
    #[cfg(windows)]
    {
        const ERROR_DISK_FULL: i32 = 112;
        const ERROR_HANDLE_DISK_FULL: i32 = 39;
        matches!(
            e.raw_os_error(),
            Some(ERROR_DISK_FULL | ERROR_HANDLE_DISK_FULL)
        )
    }
    #[cfg(not(any(unix, windows)))]
    false
}

/// Map a persistence `io::Error` into an `acp::Error` with a human-friendly
/// `message` and a stable `data.code` for log aggregation.
pub(crate) fn io_error_to_acp(e: &io::Error) -> acp::Error {
    let (message, code) = if is_disk_full_io_error(e) {
        ("No space left on device", "FS_DISK_QUOTA_EXCEEDED")
    } else {
        match e.kind() {
            io::ErrorKind::NotFound => ("Path not found.", "FS_NOT_FOUND"),
            io::ErrorKind::PermissionDenied => ("Permission denied.", "FS_PERMISSION_DENIED"),
            _ => {
                tracing::warn!(error = %e, kind = ?e.kind(), raw_os = ?e.raw_os_error(), "unclassified persistence I/O error");
                ("An unexpected I/O error occurred.", "FS_OTHER")
            }
        }
    };
    acp::Error::new(acp::ErrorCode::InternalError.into(), message.to_string()).data(Some(
        serde_json::json!({
            "code": code,
            "detail": e.to_string(),
        }),
    ))
}

#[cfg(test)]
#[path = "persistence_io_error_to_acp_tests.rs"]
mod io_error_to_acp_tests;

/// Best-effort worktree liveness touch: stamp `last_accessed_at` on the
/// worktree containing this session's cwd so `echoagent worktree gc` expires by
/// last use, not creation time. Lives here — not in a `StorageAdapter` —
/// so every session create/load path shares it regardless of backend.
fn spawn_worktree_touch(info: &Info) -> tokio::task::JoinHandle<()> {
    let cwd = info.cwd.clone();
    tokio::task::spawn_blocking(move || {
        crate::session::worktree::touch_worktree_for_cwd(&cwd);
    })
}

/// Bound on how long session open waits for the liveness touch to commit —
/// generous vs the DB's 5s busy_timeout without letting a pathologically
/// locked worktrees.db stall init.
const WORKTREE_TOUCH_INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Touch the worktree and wait (bounded) for the write to commit before the
/// session open completes: a detached touch can land after gc's pre-removal
/// re-check reads the row, letting gc delete a worktree that is actively
/// being opened or resumed. Awaiting a blocking-pool task does not block the
/// runtime; on timeout the task keeps running detached (the old
/// fire-and-forget behavior) and init proceeds.
async fn touch_worktree_for_session(info: &Info) {
    if tokio::time::timeout(WORKTREE_TOUCH_INIT_TIMEOUT, spawn_worktree_touch(info))
        .await
        .is_err()
    {
        tracing::debug!(
            cwd = %info.cwd,
            "worktree liveness touch still pending at session open"
        );
    }
}

/// Floor between activity-driven worktree touches from the persistence actor.
const WORKTREE_TOUCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// What the actor is handed once and holds for the life of the session.
pub(crate) struct SessionDeps {
    pub(crate) sampling_client: OaiCompatClient,
    pub(crate) storage_mode: StorageMode,
    pub(crate) auth_manager: Option<Arc<crate::auth::AuthManager>>,
    pub(crate) relay_sync: Option<crate::relay::RelaySync>,
    pub(crate) gateway: Option<GatewaySender>,
    pub(crate) session_summary_model: String,
    pub(crate) registry_title_sync: Option<RegistryGeneratedTitleSync>,
    pub(crate) search_index: crate::session::storage::search::SharedSearchIndex,
}

pub(crate) async fn new(
    info: &Info,
    model_id: acp::ModelId,
    deps: SessionDeps,
) -> io::Result<PersistenceHandle> {
    let SessionDeps {
        sampling_client,
        storage_mode,
        auth_manager,
        relay_sync,
        gateway,
        session_summary_model,
        registry_title_sync,
        search_index,
    } = deps;
    let root_dir = echo_agent_home();
    let storage: Box<dyn StorageAdapter> = Box::new(JsonlStorageAdapter::with_root(root_dir));

    // Initialize session in storage
    let mut summary = storage.init_session(info, model_id.clone()).await?;
    touch_worktree_for_session(info).await;

    // Update model if different
    if summary.current_model_id != model_id {
        storage.update_current_model(info, &model_id).await?;
        summary.current_model_id = model_id;
    }

    let (handle, rx, summary_tx, disk_full_tx) = actor_channel();

    let info_clone = info.clone();
    let storage: Arc<dyn StorageAdapter> = Arc::from(storage);
    let remote_sync = init_remote_sync(&summary, storage_mode, auth_manager)?;
    tokio::task::spawn(async move {
        let persistence = SessionPersistence {
            info: info_clone,
            storage: storage.clone(),
            pending_notification: None,
            pending_updates: VecDeque::new(),
            pending_chat_writes: VecDeque::new(),
            pending_plan_modes: VecDeque::new(),
            rx,
            remote_sync: remote_sync.clone(),
            created_fresh: true,
            relay_sync,
            summary: crate::session::summary::SummaryGenerator::new(
                crate::session::summary::SummaryConfig {
                    sampling_client,
                    model: session_summary_model,
                    persistence_tx: summary_tx,
                },
            ),
            registry_title_sync,
            gateway,
            search_index,
            disk_full_tx,
            disk_full_notified: false,
        };
        persistence.run().await;
    });

    Ok(handle)
}

/// Create a persistence handle that writes to an explicit directory on disk.
/// Used for subagent child sessions (top-level `sessions/<cwd>/<id>` dirs;
/// only their metadata nests under the parent's session dir).
///
/// Unlike [`new()`], this:
/// - Uses `JsonlStorageAdapter::with_explicit_session_dir()` to bypass
///   the standard `{root}/sessions/{cwd}/{id}/` path computation.
/// - Skips remote sync (subagent sessions are not synced to cloud).
/// - Skips relay sync (subagent sessions are not shared).
/// - Skips gateway (lifecycle notifications are handled by the coordinator).
pub(crate) async fn new_with_explicit_dir(
    info: &Info,
    target_dir: PathBuf,
    model_id: acp::ModelId,
    sampling_client: OaiCompatClient,
    session_summary_model: String,
) -> io::Result<PersistenceHandle> {
    let summary_path = target_dir.join("summary.json");
    let storage: Box<dyn StorageAdapter> =
        Box::new(JsonlStorageAdapter::with_explicit_session_dir(target_dir));

    // Initialize session in storage (creates summary.json, etc.)
    let mut summary = storage.init_session(info, model_id.clone()).await?;
    touch_worktree_for_session(info).await;
    if summary.session_kind.is_none() {
        summary.session_kind = Some("subagent".to_string());
    }
    let summary_json = serde_json::to_vec_pretty(&summary)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(&summary_path, summary_json)?;

    if summary.current_model_id != model_id {
        storage.update_current_model(info, &model_id).await?;
        summary.current_model_id = model_id;
    }

    let (handle, rx, summary_tx, disk_full_tx) = actor_channel();

    let info_clone = info.clone();
    let storage: Arc<dyn StorageAdapter> = Arc::from(storage);
    tokio::task::spawn(async move {
        let persistence = SessionPersistence {
            info: info_clone,
            storage: storage.clone(),
            pending_notification: None,
            pending_updates: VecDeque::new(),
            pending_chat_writes: VecDeque::new(),
            pending_plan_modes: VecDeque::new(),
            rx,
            remote_sync: None,
            created_fresh: false,
            relay_sync: None,
            summary: crate::session::summary::SummaryGenerator::new(
                crate::session::summary::SummaryConfig {
                    sampling_client,
                    model: session_summary_model,
                    persistence_tx: summary_tx,
                },
            ),
            registry_title_sync: None,
            gateway: None,
            // A bootstrap never sees a subagent session: `list_sessions_sync`
            // drops hidden summaries, and a subagent kind is hidden. Skip it here too.
            search_index: crate::session::storage::search::SharedSearchIndex::never_indexed(),
            disk_full_tx,
            disk_full_notified: false,
        };
        persistence.run().await;
    });

    Ok(handle)
}

pub struct PersistedInfo {
    pub summary: Summary,
    pub chat_history: Vec<ConversationItem>,
    /// All session updates (ACP updates and EchoAgent extension updates) in chronological order
    pub updates: Vec<SessionUpdate>,
    pub plan_state: Option<TodoState>,
    pub rewind_points: Vec<RewindPoint>,
    /// Persisted session signals (None for old sessions without signals file)
    pub signals: Option<SessionSignals>,
    pub workflow_runs: Vec<crate::session::workflow::store::RestoredWorkflowRun>,
}

/// Same as PersistedInfo but without updates - for memory efficiency when streaming
pub struct PersistedInfoLight {
    pub summary: Summary,
    pub chat_history: Vec<ConversationItem>,
    pub plan_state: Option<TodoState>,
    pub plan_mode_state: Option<crate::session::plan_mode::PlanModeSnapshot>,
    /// Path to updates file for streaming reads
    pub updates_file_path: Option<std::path::PathBuf>,
    /// Adapter-owned path to `rewind_points.jsonl` for the session's
    /// `FileStateTracker` to load lazily. `None` if the backend doesn't persist
    /// rewind points to a streamable file.
    pub rewind_points_file_path: Option<std::path::PathBuf>,
    /// Persisted session signals (None for old sessions without signals file)
    pub signals: Option<SessionSignals>,
    /// Persisted announcement tracking state (None for sessions before this feature)
    pub announcement_state: Option<crate::session::announcement_state::AnnouncementState>,
    /// Persisted goal mode orchestration state (None for sessions without goal mode)
    pub goal_mode_state: Option<crate::session::goal_tracker::GoalOrchestration>,
    pub workflow_runs: Vec<crate::session::workflow::store::RestoredWorkflowRun>,
}

/// On NotFound, try pulling from backend. Returns pulled info or the original error.
async fn pull_on_miss(
    info: &Info,
    client: &crate::remote::BackendClient,
    err: io::Error,
) -> io::Result<Info> {
    if err.kind() != io::ErrorKind::NotFound {
        return Err(err);
    }
    try_pull_from_remote(info, client).await.ok_or(err)
}

#[expect(dead_code, reason = "wired when session restore flow calls load")]
pub(crate) async fn load(
    info: &Info,
    backend: Option<&crate::remote::BackendClient>,
    deps: SessionDeps,
) -> io::Result<(PersistedInfo, PersistenceHandle)> {
    let SessionDeps {
        sampling_client,
        storage_mode,
        auth_manager,
        relay_sync,
        gateway,
        session_summary_model,
        registry_title_sync,
        search_index,
    } = deps;
    let root_dir = echo_agent_home();
    let storage: Box<dyn StorageAdapter> = Box::new(JsonlStorageAdapter::with_root(root_dir));

    let (persisted, loaded_info) = match storage.load_session(info).await {
        Ok(p) => (p, info.clone()),
        Err(e) => match backend {
            Some(client) => {
                let pulled = pull_on_miss(info, client, e).await?;
                let p = storage.load_session(&pulled).await?;
                (p, pulled)
            }
            None => return Err(e),
        },
    };
    // Touch on load too: resuming must reset the worktree's gc expiry clock.
    touch_worktree_for_session(&loaded_info).await;

    let persisted_info = PersistedInfo {
        summary: persisted.summary,
        chat_history: persisted.chat_history,
        updates: persisted.updates,
        plan_state: persisted.plan_state,
        rewind_points: persisted.rewind_points,
        signals: persisted.signals,
        workflow_runs: persisted.workflow_runs,
    };

    let (handle, rx, summary_tx, disk_full_tx) = actor_channel();

    let storage: Arc<dyn StorageAdapter> = Arc::from(storage);
    let remote_sync = init_remote_sync(&persisted_info.summary, storage_mode, auth_manager)?;

    let has_title = !persisted_info.summary.display_title().is_empty();
    let mut had_history = persisted_info.chat_history.iter().any(
        |item| matches!(item, ConversationItem::User(user) if user.synthetic_reason.is_none()),
    );
    // ACP prompt history survives model-context compaction. Recover the opening
    // instead of taking the next follow-up when an earlier title call was lost.
    let first_prompt = if has_title {
        None
    } else {
        match load_initial_title_source(storage.as_ref(), &loaded_info).await {
            Ok(source) => source,
            Err(error) => {
                tracing::warn!(%error, "could not recover first prompt for session title");
                // An unreadable transcript is not evidence of a fresh session.
                had_history = true;
                None
            }
        }
    };
    tokio::task::spawn(async move {
        let mut summary_gen = crate::session::summary::SummaryGenerator::new(
            crate::session::summary::SummaryConfig {
                sampling_client,
                model: session_summary_model,
                persistence_tx: summary_tx,
            },
        );
        summary_gen.restore(has_title, had_history, first_prompt);
        let persistence = SessionPersistence {
            info: loaded_info,
            storage: storage.clone(),
            pending_notification: None,
            pending_updates: VecDeque::new(),
            pending_chat_writes: VecDeque::new(),
            pending_plan_modes: VecDeque::new(),
            rx,
            remote_sync: remote_sync.clone(),
            created_fresh: false,
            relay_sync,
            summary: summary_gen,
            registry_title_sync,
            gateway,
            search_index,
            disk_full_tx,
            disk_full_notified: false,
        };
        persistence.run().await;
    });

    Ok((persisted_info, handle))
}

/// Like `load`, but doesn't load updates into memory.
/// Instead, provides the path to the updates file for streaming reads.
/// Use this for memory-efficient session loading when replaying updates.
pub(crate) async fn load_light(
    info: &Info,
    backend: Option<&crate::remote::BackendClient>,
    deps: SessionDeps,
) -> io::Result<(PersistedInfoLight, PersistenceHandle)> {
    let SessionDeps {
        sampling_client,
        storage_mode,
        auth_manager,
        relay_sync,
        gateway,
        session_summary_model,
        registry_title_sync,
        search_index,
    } = deps;
    let root_dir = echo_agent_home();
    let storage: Box<dyn StorageAdapter> =
        Box::new(JsonlStorageAdapter::with_root(root_dir.clone()));

    let (persisted, loaded_info) = match storage.load_session_without_updates(info).await {
        Ok(p) => (p, info.clone()),
        Err(e) => match backend {
            Some(client) => {
                let pulled = pull_on_miss(info, client, e).await?;
                let p = storage.load_session_without_updates(&pulled).await?;
                (p, pulled)
            }
            None => return Err(e),
        },
    };
    // Touch on load too: resuming must reset the worktree's gc expiry clock.
    touch_worktree_for_session(&loaded_info).await;

    let updates_file_path = storage.updates_file_path(&loaded_info);
    let rewind_points_file_path = storage.rewind_points_file_path(&loaded_info);

    let persisted_info = PersistedInfoLight {
        summary: persisted.summary,
        chat_history: persisted.chat_history,
        plan_state: persisted.plan_state,
        plan_mode_state: persisted.plan_mode_state,
        updates_file_path,
        rewind_points_file_path,
        signals: persisted.signals,
        announcement_state: persisted.announcement_state,
        goal_mode_state: persisted.goal_mode_state,
        workflow_runs: persisted.workflow_runs,
    };

    let (handle, rx, summary_tx, disk_full_tx) = actor_channel();

    let storage: Arc<dyn StorageAdapter> = Arc::from(storage);
    let remote_sync = init_remote_sync(&persisted_info.summary, storage_mode, auth_manager)?;

    let has_title = !persisted_info.summary.display_title().is_empty();
    let mut had_history = persisted_info.chat_history.iter().any(
        |item| matches!(item, ConversationItem::User(user) if user.synthetic_reason.is_none()),
    );
    // ACP prompt history survives model-context compaction. Recover the opening
    // instead of taking the next follow-up when an earlier title call was lost.
    let first_prompt = if has_title {
        None
    } else {
        match load_initial_title_source(storage.as_ref(), &loaded_info).await {
            Ok(source) => source,
            Err(error) => {
                tracing::warn!(%error, "could not recover first prompt for session title");
                // An unreadable transcript is not evidence of a fresh session.
                had_history = true;
                None
            }
        }
    };
    tokio::task::spawn(async move {
        let mut summary_gen = crate::session::summary::SummaryGenerator::new(
            crate::session::summary::SummaryConfig {
                sampling_client,
                model: session_summary_model,
                persistence_tx: summary_tx,
            },
        );
        summary_gen.restore(has_title, had_history, first_prompt);
        let persistence = SessionPersistence {
            info: loaded_info,
            storage: storage.clone(),
            pending_notification: None,
            pending_updates: VecDeque::new(),
            pending_chat_writes: VecDeque::new(),
            pending_plan_modes: VecDeque::new(),
            rx,
            remote_sync: remote_sync.clone(),
            created_fresh: false,
            relay_sync,
            summary: summary_gen,
            registry_title_sync,
            gateway,
            search_index,
            disk_full_tx,
            disk_full_notified: false,
        };
        persistence.run().await;
    });

    Ok((persisted_info, handle))
}

/// List session summaries, optionally filtered by cwd (absolute path string).
/// Returns summaries sorted by `last_active_at` (else `updated_at`) descending.
fn recover_session_relocations_in(root: &Path) -> crate::session::storage::relocation::Result<()> {
    crate::session::storage::relocation::RelocationStorage::new(root.into()).recover_all()
}

pub async fn list_summaries(cwd: Option<&str>) -> io::Result<Vec<Summary>> {
    let root_dir = crate::util::echo_agent_home::echo_agent_home();
    let recovery_root = root_dir.clone();
    tokio::task::spawn_blocking(move || recover_session_relocations_in(&recovery_root))
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    let storage: Box<dyn StorageAdapter> = Box::new(JsonlStorageAdapter::with_root(root_dir));
    storage.list_sessions(cwd).await
}

/// Failure modes of [`delete_session_history`].
///
/// Kept distinct so callers can surface a precise message: a remote
/// failure is reported separately from a local-disk failure because the
/// remote delete runs first and aborts the whole operation (see the doc
/// on [`delete_session_history`]).
#[derive(Debug, thiserror::Error)]
pub enum DeleteSessionError {
    /// Listing local summaries (to resolve the on-disk session dir) failed.
    #[error("failed to list sessions: {0}")]
    List(#[source] io::Error),
    /// The remote (writeback) copy could not be deleted; local bits were
    /// left untouched so the operation can be retried.
    #[error("failed to delete remote session data: {0}")]
    Remote(#[source] crate::remote::client::BackendError),
    /// The local on-disk session directory could not be removed.
    #[error("failed to delete session: {0}")]
    Local(#[source] io::Error),
}

/// Where a session copy was actually removed by [`delete_session_history`].
///
/// Both fields are `false` when nothing existed to delete (still a
/// success). Callers use [`Self::any_removed`] to decide between a
/// "deleted" and a "not found" message without conflating a remote-only
/// delete with a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionDeletion {
    /// A local on-disk session directory was found and removed.
    pub local_removed: bool,
    /// A remote (writeback) copy was found and removed. `false` when
    /// `needs_remote` was not set, or the remote copy was already absent
    /// (the backend returned `404`).
    pub remote_removed: bool,
}

impl SessionDeletion {
    /// `true` when a copy was removed from at least one location.
    pub fn any_removed(self) -> bool {
        self.local_removed || self.remote_removed
    }
}

/// Exact storage lookup for deletion, independent of readable summaries and UI
/// catalog limits. Never follow a redirected storage directory.
pub fn session_history_paths(session_id: &str, cwd: Option<&str>) -> io::Result<Vec<PathBuf>> {
    session_history_paths_in(&echo_agent_home().join("sessions"), session_id, cwd)
}

fn session_history_paths_in(
    root: &Path,
    session_id: &str,
    cwd: Option<&str>,
) -> io::Result<Vec<PathBuf>> {
    let mut components = Path::new(session_id).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
        || session_id.contains(['/', '\\'])
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid session id",
        ));
    }
    let inspect = |path: &Path| -> io::Result<bool> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(true),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsafe session directory: {}", path.display()),
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    };
    if !inspect(root)? {
        return Ok(vec![]);
    }
    let scope = cwd.map(crate::util::echo_agent_home::encode_cwd_dirname);
    let mut paths = vec![];
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if scope
            .as_ref()
            .is_some_and(|scope| entry.file_name() != std::ffi::OsStr::new(scope))
        {
            continue;
        }
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let parent = entry.path();
        if !inspect(&parent)? {
            continue;
        }
        let path = parent.join(session_id);
        if inspect(&path)? {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Permanently delete a session's history: the remote (writeback) copy
/// when `needs_remote`, the local on-disk session directory, and the
/// FTS search-index entry.
///
/// Idempotent: a session that is missing locally (e.g. remote-only)
/// still succeeds, and a remote `404` (copy already gone) is treated as
/// success rather than an error. When `needs_remote` is set the remote
/// delete runs *first* and is authoritative — only on its success (or a
/// `404`) are the local bits removed. This ordering prevents a partial
/// delete where the local copy is nuked but the remote copy lingers and
/// re-appears on the next session list.
///
/// Returns a [`SessionDeletion`] recording which copies (local / remote)
/// were actually removed; both fields `false` means nothing existed
/// (still `Ok`).
pub async fn delete_session_history(
    session_id: &str,
    cwd: Option<&str>,
    needs_remote: bool,
    auth_manager: Arc<crate::auth::AuthManager>,
    search_index: Option<&echo_agent_session_search::SearchIndexManager>,
) -> Result<SessionDeletion, DeleteSessionError> {
    let sid = acp::SessionId::new(Arc::from(session_id));

    // Resolve the local session info, scoping to cwd if provided. A
    // remote-only session won't be found here — that's fine, the remote
    // delete (if applicable) still runs.
    let summaries = list_summaries(cwd)
        .await
        .map_err(DeleteSessionError::List)?;
    let local_info = summaries
        .iter()
        .find(|s| s.info.id == sid)
        .map(|s| s.info.clone());
    let paths = session_history_paths(session_id, cwd).map_err(DeleteSessionError::Local)?;

    // Remote delete first (authoritative for cloud history). A genuine
    // failure aborts before any local mutation so the row does not
    // reappear; a `404` means the copy is already gone, so deletion stays
    // idempotent and falls through to local cleanup.
    let remote_removed = if needs_remote {
        let result = crate::remote::client::BackendClient::new()
            .with_auth_manager(auth_manager)
            .delete_session_data(session_id)
            .await;
        classify_remote_delete(result)?
    } else {
        false
    };

    for path in &paths {
        match tokio::fs::remove_dir_all(path).await {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(DeleteSessionError::Local(e)),
        }
    }
    if !session_history_paths(session_id, cwd)
        .map_err(DeleteSessionError::Local)?
        .is_empty()
    {
        return Err(DeleteSessionError::Local(io::Error::other(
            "session history survived deletion",
        )));
    }
    let local_removed = !paths.is_empty();

    // A stale search row can outlive an absent or damaged summary.
    crate::session::storage::search::evict_session(
        &crate::util::echo_agent_home::echo_agent_home(),
        session_id,
    )
    .await;
    // The eviction above is a point in time. Queue the indexer as well so an upsert already under
    // way, which would otherwise write the row back, is followed by a re-read that finds nothing.
    if let Some(info) = local_info {
        crate::session::storage::search::notify_session_updated(
            search_index,
            &info.id.to_string(),
            &info.cwd,
        );
    }

    Ok(SessionDeletion {
        local_removed,
        remote_removed,
    })
}

/// Classify a remote `delete_session_data` result, reporting whether a
/// remote copy was actually removed: a `2xx` means a copy was deleted
/// (`Ok(true)`), a `404` means it was already gone so deletion stays
/// idempotent (`Ok(false)`), and any other backend error aborts the
/// delete (`Err`) so local bits are left untouched and it can be retried.
fn classify_remote_delete(
    result: Result<(), crate::remote::client::BackendError>,
) -> Result<bool, DeleteSessionError> {
    use crate::remote::client::BackendError;
    match result {
        Ok(()) => Ok(true),
        Err(BackendError::RequestFailed { status: 404, .. }) => Ok(false),
        Err(e) => Err(DeleteSessionError::Remote(e)),
    }
}

#[cfg(test)]
#[path = "persistence_tests.rs"]
mod durable_update_tests;

#[cfg(test)]
#[path = "persistence_chat_retry_tests.rs"]
mod chat_retry_tests;

#[cfg(test)]
#[path = "persistence_delete_session_history_tests.rs"]
mod delete_session_history_tests;

/// List the `limit` most recently modified session summaries across all
/// workspaces. Uses stat-based mtime sorting to avoid reading every
/// summary file on disk; final order uses `last_active_at` else `updated_at`.
pub async fn list_recent_summaries(limit: usize) -> io::Result<Vec<Summary>> {
    let root_dir = crate::util::echo_agent_home::echo_agent_home();
    let recovery_root = root_dir.clone();
    tokio::task::spawn_blocking(move || recover_session_relocations_in(&recovery_root))
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    let storage = JsonlStorageAdapter::with_root(root_dir);
    storage.list_sessions_recent(limit).await
}

// Session folder TTL cleanup

/// Guard ensuring session cleanup runs at most once per process.
static CLEANUP_SESSIONS_ONCE: std::sync::Once = std::sync::Once::new();

/// Default TTL for stale session files (30 days).
const DEFAULT_CLEANUP_TTL_DAYS: u32 = 30;

/// Walk `~/.echo-agent/sessions/` and delete files with mtime older than `ttl_days`.
/// Removes empty session directories after file cleanup.
/// Skips `skip_session_dir` if provided (current session).
///
/// This is a **synchronous** function intended to be called via
/// `tokio::task::spawn_blocking` so it runs on the thread pool and
/// never competes with the agent's single-threaded `LocalSet`.
pub(crate) fn cleanup_stale_sessions(skip_session_dir: Option<&Path>) {
    CLEANUP_SESSIONS_ONCE.call_once(|| {
        let ttl_days = resolve_cleanup_ttl_days();
        let root = echo_agent_home();
        if let Err(error) = recover_session_relocations_in(&root) {
            tracing::error!(%error, "session relocation recovery failed before TTL cleanup");
            return;
        }
        let sessions_root = root.join("sessions");
        let relocation_view = match storage_view(&sessions_root) {
            Ok(view) => view,
            Err(error) => {
                tracing::error!(%error, "session relocation snapshot failed before TTL cleanup");
                return;
            }
        };

        tracing::info!(
            target: "echo_agent_runtime::session::persistence",
            sessions_root = %sessions_root.display(),
            ttl_days,
            skip = ?skip_session_dir.map(|p| p.display().to_string()),
            "SESSION_CLEANUP_START: scanning for stale session files"
        );

        let stats = cleanup_stale_sessions_inner(
            &sessions_root,
            ttl_days,
            skip_session_dir,
            &relocation_view,
            &root,
            CleanupLevel::SessionsRoot,
        );

        tracing::info!(
            target: "echo_agent_runtime::session::persistence",
            sessions_root = %sessions_root.display(),
            files_deleted = stats.files_deleted,
            dirs_removed = stats.dirs_removed,
            errors = stats.errors,
            "SESSION_CLEANUP_DONE"
        );
    });
}

/// Resolve TTL from config.toml `[storage] cleanup_ttl_days`, falling back to 30.
fn resolve_cleanup_ttl_days() -> u32 {
    // Try to load config and read [storage] section
    if let Ok(layers) = crate::config::ConfigLayers::load() {
        let effective = layers.effective_config_disk_only();
        if let Some(storage) = effective.get("storage")
            && let Some(ttl) = storage.get("cleanup_ttl_days")
            && let Some(days) = ttl.as_integer()
            && days > 0
        {
            return days as u32;
        }
    }
    DEFAULT_CLEANUP_TTL_DAYS
}

#[derive(Default)]
struct CleanupStats {
    files_deleted: u32,
    dirs_removed: u32,
    errors: u32,
}

#[derive(Clone, Copy)]
enum CleanupLevel {
    SessionsRoot,
    Cwd,
    Session,
}

/// Recursive cleanup: delete stale files, then rmdir empty dirs (post-order).
fn cleanup_stale_sessions_inner(
    root: &Path,
    ttl_days: u32,
    skip: Option<&Path>,
    relocation_view: &crate::session::storage::relocation::RelocationView,
    echo_agent_home: &Path,
    level: CleanupLevel,
) -> CleanupStats {
    let mut stats = CleanupStats::default();

    if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
    {
        return stats;
    }
    if let Some(skip_dir) = skip
        && root == skip_dir
    {
        return stats;
    }

    let Ok(entries) = std::fs::read_dir(root) else {
        return stats;
    };

    for entry_result in entries {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!(
                    target: "echo_agent_runtime::session::persistence",
                    error = %e,
                    "SESSION_CLEANUP_READ_ERROR"
                );
                stats.errors += 1;
                continue;
            }
        };
        let path = entry.path();

        if let Some(skip_dir) = skip
            && path == skip_dir
        {
            continue;
        }

        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            if matches!(level, CleanupLevel::SessionsRoot)
                && relocation_view.protects_cwd_dir(&path)
            {
                continue;
            }
            let lease = if matches!(level, CleanupLevel::Cwd) {
                let summary = path.join("summary.json");
                let summary_type = match std::fs::symlink_metadata(&summary) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        let child_stats = cleanup_stale_sessions_inner(
                            &path,
                            ttl_days,
                            skip,
                            relocation_view,
                            echo_agent_home,
                            CleanupLevel::Session,
                        );
                        stats.files_deleted += child_stats.files_deleted;
                        stats.dirs_removed += child_stats.dirs_removed;
                        stats.errors += child_stats.errors;
                        if child_stats.files_deleted > 0 && std::fs::remove_dir(&path).is_ok() {
                            stats.dirs_removed += 1;
                        }
                        continue;
                    }
                    Err(error) => {
                        stats.errors += 1;
                        tracing::debug!(
                            target: "echo_agent_runtime::session::persistence",
                            path = %summary.display(),
                            %error,
                            "SESSION_CLEANUP_METADATA_ERROR"
                        );
                        continue;
                    }
                };
                if !summary_type.file_type().is_file() || summary_type.file_type().is_symlink() {
                    continue;
                }
                let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let storage = crate::session::storage::relocation::RelocationStorage::new(
                    echo_agent_home.to_path_buf(),
                );
                let Ok(lease) = storage.acquire(id) else {
                    continue;
                };
                match storage.read_journal(id) {
                    Err(crate::session::storage::relocation::RelocationError::JournalMissing(
                        _,
                    )) => Some(lease),
                    _ => continue,
                }
            } else {
                None
            };
            let next = match level {
                CleanupLevel::SessionsRoot => CleanupLevel::Cwd,
                CleanupLevel::Cwd | CleanupLevel::Session => CleanupLevel::Session,
            };
            let child_stats = cleanup_stale_sessions_inner(
                &path,
                ttl_days,
                skip,
                relocation_view,
                echo_agent_home,
                next,
            );
            stats.files_deleted += child_stats.files_deleted;
            stats.dirs_removed += child_stats.dirs_removed;
            stats.errors += child_stats.errors;

            // Only attempt remove_dir if this subtree actually had stale
            // files deleted in this pass. Otherwise we risk removing dirs
            // that were deliberately created for use by concurrent sessions.
            if child_stats.files_deleted > 0 && std::fs::remove_dir(&path).is_ok() {
                stats.dirs_removed += 1;
                tracing::debug!(
                    target: "echo_agent_runtime::session::persistence",
                    dir = %path.display(),
                    "SESSION_CLEANUP_RMDIR"
                );
            }
            drop(lease);
        } else if let Ok(mtime) = metadata.modified()
            && is_stale(mtime, ttl_days)
        {
            if std::fs::remove_file(&path).is_ok() {
                stats.files_deleted += 1;
                tracing::debug!(
                    target: "echo_agent_runtime::session::persistence",
                    file = %path.display(),
                    "SESSION_CLEANUP_DELETE"
                );
            } else {
                stats.errors += 1;
            }
        }
    }

    stats
}

fn is_stale(mtime: std::time::SystemTime, ttl_days: u32) -> bool {
    let ttl = std::time::Duration::from_secs(u64::from(ttl_days) * 86400);
    mtime.elapsed().is_ok_and(|age| age > ttl)
}

#[cfg(test)]
#[path = "persistence_agent_name_persistence_tests.rs"]
mod agent_name_persistence_tests;

#[cfg(test)]
#[path = "persistence_collect_session_files_tests.rs"]
mod collect_session_files_tests;

#[cfg(test)]
#[path = "persistence_session_exists_tests.rs"]
mod session_exists_tests;

#[cfg(test)]
#[path = "persistence_find_summary_by_session_id_tests.rs"]
mod find_summary_by_session_id_tests;

#[cfg(test)]
#[path = "persistence_resumed_sandbox_profile_tests.rs"]
mod resumed_sandbox_profile_tests;

#[cfg(test)]
#[path = "persistence_session_exists_for_cwd_tests.rs"]
mod session_exists_for_cwd_tests;

#[cfg(test)]
#[path = "persistence_find_local_child_tests.rs"]
mod find_local_child_tests;

#[cfg(test)]
#[path = "persistence_resolve_local_session_tests.rs"]
mod resolve_local_session_tests;

#[cfg(test)]
#[path = "persistence_repo_wide_resolution_tests.rs"]
mod repo_wide_resolution_tests;

#[cfg(test)]
#[path = "persistence_actor_lifetime_tests.rs"]
mod actor_lifetime_tests;
