//! Session transcript replay: line peeks, rewind-aware prepare, and bounded
//! streaming load.
//!
//! Invariants:
//! - InProgress `tool_call_update` peeks are typed serde (unknown fields
//!   ignored) so `content` / `rawOutput` are not allocated.
//! - Production child/fork replay streams one typed ACP update at a time
//!   ([`stream_replay_updates_at`]); [`load_updates_for_replay_at`] stays a
//!   typed materialize-all reference for tests.

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use agent_client_protocol as acp;

use super::relocation::RelocationView;
use super::{
    RawLinePeek, RawParamsPeek, RawUpdatePeek, SessionUpdate, SessionUpdateEnvelope,
    filter_rewind_lines, replay_updates_path_in_dir, strip_context_wrappers,
};
use crate::extensions::notification::SessionNotification;
use crate::extensions::notification::SessionUpdate as EchoAgentUpdate;
use crate::session::wire_tags::{
    AVAILABLE_COMMANDS_UPDATE, TOOL_CALL_STATUS_IN_PROGRESS, TOOL_CALL_UPDATE,
};

// `_meta` protocol field names (not enum discriminants).
/// `_meta` key holding the running token count. The serde `rename` below must
/// match it by hand (serde attrs can't reference a const).
const TOTAL_TOKENS_KEY: &str = "totalTokens";
/// `_meta` key holding the per-event id used for cursor-based reconnect.
const EVENT_ID_KEY: &str = "eventId";

/// What to do when cwd hints miss `updates.jsonl`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplayLookupFallback {
    #[default]
    Relocation,
    HintedOnly,
}

/// Optional location hints so child `updates.jsonl` lookup can skip a full
/// `~/.echo-agent/sessions` RelocationView scan.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReplayPathHint<'a> {
    /// Parent session working directory; tried as
    /// `<sessions>/<encoded_cwd>/<child_id>/updates.jsonl`.
    pub parent_cwd: Option<&'a Path>,
    /// Child working directory when it differs from the parent (worktree /
    /// custom cwd). Tried before [`Self::parent_cwd`].
    pub child_cwd: Option<&'a Path>,
    /// When cwd hints miss: scan via [`RelocationView`], or return None.
    pub fallback: ReplayLookupFallback,
}

#[doc(hidden)]
pub struct PreparedReplay<'a> {
    /// Rewind-filtered replay lines, each borrowed from the input transcript.
    pub lines: Vec<&'a str>,
    pub(crate) mark_replay: bool,
    pub(crate) last_tokens: u64,
    /// Highest `eventId` counter across all live (rewind-filtered) lines, used
    /// to re-seed the process-global event counter on resume so post-load live
    /// events keep monotonically increasing ids (see
    /// [`crate::util::event_id::ensure_event_counter_at_least`]). `None` when no
    /// line carried a parseable `eventId` (older shell).
    pub(crate) max_event_seq: Option<u64>,
    pub(crate) total_live: usize,
    /// Replayed spawns with no matching finish (a rewind can drop the finish):
    /// `(subagent_id, child_session_id)`, reconciled on load.
    pub(crate) unfinished_subagents: Vec<(String, String)>,
}

/// Whether a replay stream forwarded any ACP update. Gates the caller's
/// post-replay memory purge (`Empty` means nothing was reclaimable) and, for
/// child hydrate, whether disk proved it can rebuild the transcript. EchoAgent
/// events alone never count as `Emitted`, so an eviction decision can't
/// settle on a file the client cannot rebuild content from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum ReplayEmission {
    Emitted,
    Empty,
}

/// One update forwarded by the streaming child replay: the ACP transcript
/// stream plus persisted EchoAgent child events (compaction, retry, memory
/// lifecycle) in file order, so a rebuilt view keeps its non-ACP markers.
#[derive(Debug)]
pub enum ReplayedUpdate {
    /// The second field is the persisted line's `_meta` (original
    /// `agentTimestampMs`/`turnStartMs`, so rebuilt entries keep their run-time
    /// timestamps). For a collapsed ToolCall it is the completing line's meta;
    /// `None` for start-only tools flushed at EOF.
    Acp(acp::SessionUpdate, Option<acp::Meta>),
    EchoAgent(EchoAgentUpdate),
}

/// Collapses ToolCall + ToolCallUpdates into one ToolCall during replay.
/// Parent forward never flushes leftovers (cursor/`_meta.eventId` contract).
/// Child stream EOF calls [`Self::take_pending`] so start-only tools still hydrate.
pub(crate) struct ReplayToolCollapser {
    pending: HashMap<acp::ToolCallId, acp::ToolCall>,
}

impl ReplayToolCollapser {
    pub(crate) fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    /// Ingest one ACP update. `None` means hold or drop (do not forward yet).
    pub(crate) fn push(&mut self, update: acp::SessionUpdate) -> Option<acp::SessionUpdate> {
        match update {
            acp::SessionUpdate::AvailableCommandsUpdate(_) => None,
            acp::SessionUpdate::ToolCall(tc) => {
                if matches!(
                    tc.status,
                    acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed
                ) {
                    return Some(acp::SessionUpdate::ToolCall(tc));
                }
                self.pending.insert(tc.tool_call_id.clone(), tc);
                None
            }
            acp::SessionUpdate::ToolCallUpdate(mut u) => match u.fields.status {
                Some(acp::ToolCallStatus::Completed) | Some(acp::ToolCallStatus::Failed) => {
                    if let Some(mut base) = self.pending.remove(&u.tool_call_id) {
                        base.update(std::mem::take(&mut u.fields));
                        return Some(acp::SessionUpdate::ToolCall(base));
                    }
                    Some(acp::SessionUpdate::ToolCallUpdate(u))
                }
                None => {
                    if let Some(base) = self.pending.get_mut(&u.tool_call_id) {
                        base.update(std::mem::take(&mut u.fields));
                    }
                    None
                }
                Some(_) => None,
            },
            other => Some(other),
        }
    }

    /// Child-stream EOF: emit ToolCalls that never saw Completed/Failed.
    pub(crate) fn take_pending(&mut self) -> impl Iterator<Item = acp::SessionUpdate> {
        std::mem::take(&mut self.pending)
            .into_values()
            .map(acp::SessionUpdate::ToolCall)
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

/// Load replay-ready typed ACP updates for a session, or `None` when the
/// session or its `updates.jsonl` is missing.
pub fn load_updates_for_replay(
    session_id: &str,
) -> std::io::Result<Option<Vec<acp::SessionUpdate>>> {
    let Some(session_dir) =
        crate::session::persistence::find_persisted_session_dir_by_id_result(session_id)?
    else {
        return Ok(None);
    };
    let Some(updates_path) = replay_updates_path_in_dir(&session_dir) else {
        return Ok(None);
    };
    Ok(Some(collect_replay_updates(&updates_path)?))
}

/// Like [`load_updates_for_replay`], but resolves the session under a specific
/// echoagent home. Typed, materialize-all replay reader: collects every update into
/// owned `Vec`s. Production forwards replay through [`stream_replay_updates_at`]
/// to bound peak memory, so this has no production caller and is compiled only
/// for tests: the `testkit_synth_roundtrip` and `session_load_perf` parity
/// references and the in-crate relocation tests.
#[cfg(any(test, feature = "test-support"))]
pub fn load_updates_for_replay_at(
    session_id: &str,
    echo_agent_home: &std::path::Path,
) -> std::io::Result<Option<Vec<acp::SessionUpdate>>> {
    let Some(updates_path) =
        resolve_replay_updates_path(session_id, echo_agent_home, ReplayPathHint::default())?
    else {
        return Ok(None);
    };
    Ok(Some(collect_replay_updates(&updates_path)?))
}

/// Collect every replay-ready ACP update from `updates_path` into a `Vec`, the
/// materializing counterpart of the streaming [`for_each_replay_update_in_file`].
fn collect_replay_updates(
    updates_path: &std::path::Path,
) -> std::io::Result<Vec<acp::SessionUpdate>> {
    let mut acp_updates: Vec<acp::SessionUpdate> = Vec::new();
    for_each_replay_update_in_file(updates_path, |u| acp_updates.push(u))?;
    Ok(acp_updates)
}

fn is_safe_session_id_component(session_id: &str) -> bool {
    let mut parts = Path::new(session_id).components();
    matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
}

fn try_fast_replay_updates_path(
    session_id: &str,
    echo_agent_home: &Path,
    hint: ReplayPathHint<'_>,
) -> Option<PathBuf> {
    if !is_safe_session_id_component(session_id) {
        return None;
    }
    for cwd in [hint.child_cwd, hint.parent_cwd].into_iter().flatten() {
        let encoded = echo_agent_config::encode_cwd_dirname(&cwd.to_string_lossy());
        let candidate = echo_agent_home
            .join("sessions")
            .join(encoded)
            .join(session_id);
        if let Some(path) = replay_updates_path_in_dir(&candidate) {
            return Some(path);
        }
    }
    None
}

/// Resolve `updates.jsonl` for `session_id` under `echo_agent_home`, or `None` when
/// the session directory or the file is missing. Shared by the typed
/// `load_updates_for_replay_at` and the streaming [`stream_replay_updates_at`].
pub(crate) fn resolve_replay_updates_path(
    session_id: &str,
    echo_agent_home: &std::path::Path,
    hint: ReplayPathHint<'_>,
) -> std::io::Result<Option<std::path::PathBuf>> {
    match try_fast_replay_updates_path(session_id, echo_agent_home, hint) {
        Some(path) if !super::relocation::has_relocation_journal(echo_agent_home, session_id) => {
            return Ok(Some(path));
        }
        Some(_journaled) => {
            // Journal is authority; hinted source may be stale.
        }
        None if hint.fallback == ReplayLookupFallback::HintedOnly => {
            // Hinted miss: skip RelocationView (UI-thread scan).
            return Ok(None);
        }
        None => {}
    }
    let sessions_root = echo_agent_home.join("sessions");
    let view = RelocationView::load_for_sessions_root(&sessions_root).map_err(io::Error::other)?;
    let Some(session_dir) = view
        .find_persisted_session_dir(session_id)
        .map_err(io::Error::other)?
    else {
        return Ok(None);
    };
    Ok(replay_updates_path_in_dir(&session_dir))
}

/// Invoke `f` once per client-replay ACP update for a session under `echo_agent_home`,
/// never building the full typed `Vec`. Skips ACU / InProgress lines before
/// full notification serde and collapses ToolCall+updates. The typed
/// [`load_updates_for_replay_at`] reference does not skip those.
///
/// `Empty` folds missing-session, missing-file, and no-ACP-updates.
/// I/O errors from reading the file still propagate.
pub fn stream_replay_updates_at<F: FnMut(acp::SessionUpdate)>(
    session_id: &str,
    echo_agent_home: &std::path::Path,
    mut f: F,
) -> std::io::Result<ReplayEmission> {
    stream_replay_updates_at_hinted(
        session_id,
        echo_agent_home,
        ReplayPathHint::default(),
        |update| {
            if let ReplayedUpdate::Acp(update, _) = update {
                f(update);
            }
        },
    )
}

/// Whether replaying `session_id`'s persisted transcript would emit at least
/// one ACP update ([`ReplayEmission::Emitted`]), without applying anything.
/// For callers deciding whether an in-memory copy can be dropped and rebuilt
/// later: a non-empty file is NOT proof (EchoAgent-only, torn, or catalog-only
/// content replays `Empty`), so this mirrors
/// [`stream_replay_updates_at_hinted`]'s per-line emission exactly — same
/// rewind and drop filters, same envelope parse — but stops at the first
/// emitting line (typically the first line, the prompt echo). `false` and
/// `Err` both mean "keep the in-memory copy".
pub fn replay_would_emit(
    session_id: &str,
    echo_agent_home: &std::path::Path,
    hint: ReplayPathHint<'_>,
) -> std::io::Result<bool> {
    let Some(path) = resolve_replay_updates_path(session_id, echo_agent_home, hint)? else {
        return Ok(false);
    };
    let raw_contents = std::fs::read_to_string(&path)?;
    for line in rewind_filtered_live(&raw_contents) {
        if line_is_dropped_on_replay(line) {
            continue;
        }
        let Ok(SessionUpdate::Acp(notif)) = SessionUpdateEnvelope::from_str(line) else {
            continue;
        };
        // Mirrors [`ReplayToolCollapser`]: every parsed ACP line reaches the
        // stream (directly, merged into its ToolCall, or via the EOF pending
        // flush) except a command catalog and a ToolCallUpdate that never
        // completes and has no base to merge into.
        match notif.update {
            acp::SessionUpdate::AvailableCommandsUpdate(_) => {}
            acp::SessionUpdate::ToolCallUpdate(u) => {
                if matches!(
                    u.fields.status,
                    Some(acp::ToolCallStatus::Completed) | Some(acp::ToolCallStatus::Failed)
                ) {
                    return Ok(true);
                }
            }
            _ => return Ok(true),
        }
    }
    Ok(false)
}

/// [`stream_replay_updates_at`] with parent/child cwd hints so child hydrate
/// can skip a full sessions-root scan on the common encoded-cwd path, and
/// with persisted EchoAgent child events forwarded in file order (see
/// [`ReplayedUpdate`]).
pub fn stream_replay_updates_at_hinted<F: FnMut(ReplayedUpdate)>(
    session_id: &str,
    echo_agent_home: &std::path::Path,
    hint: ReplayPathHint<'_>,
    mut f: F,
) -> std::io::Result<ReplayEmission> {
    let Some(updates_path) = resolve_replay_updates_path(session_id, echo_agent_home, hint)? else {
        return Ok(ReplayEmission::Empty);
    };
    let raw_contents = std::fs::read_to_string(&updates_path)?;
    let live = rewind_filtered_live(&raw_contents);
    let mut collapser = ReplayToolCollapser::new();
    let mut forwarded = false;
    for line in live {
        if line_is_dropped_on_replay(line) {
            continue;
        }
        match SessionUpdateEnvelope::from_str(line) {
            Ok(SessionUpdate::Acp(notif)) => {
                let notif = *notif;
                let update = strip_context_wrappers(notif.update);
                if let Some(update) = collapser.push(update) {
                    forwarded = true;
                    f(ReplayedUpdate::Acp(update, notif.meta));
                }
            }
            Ok(SessionUpdate::EchoAgent(notif)) => f(ReplayedUpdate::EchoAgent(notif.update)),
            Err(e) => tracing::debug!(error = %e, "skipping unparseable replay line"),
        }
    }
    for update in collapser.take_pending() {
        forwarded = true;
        f(ReplayedUpdate::Acp(update, None));
    }
    Ok(if forwarded {
        ReplayEmission::Emitted
    } else {
        ReplayEmission::Empty
    })
}

/// Typed-load core: rewind-filter and forward every ACP update (including ACU).
/// Not used by [`stream_replay_updates_at`] (that path peeks + collapses).
pub(crate) fn for_each_replay_update_in_file<F: FnMut(acp::SessionUpdate)>(
    updates_path: &std::path::Path,
    mut f: F,
) -> std::io::Result<bool> {
    let raw_contents = std::fs::read_to_string(updates_path)?;
    let live = rewind_filtered_live(&raw_contents);
    let mut forwarded = false;
    for line in live {
        match SessionUpdateEnvelope::from_str(line) {
            Ok(SessionUpdate::Acp(notif)) => {
                forwarded = true;
                f(strip_context_wrappers(notif.update));
            }
            Ok(SessionUpdate::EchoAgent(_)) => {}
            Err(e) => tracing::debug!(error = %e, "skipping unparseable replay line"),
        }
    }
    Ok(forwarded)
}

fn rewind_filtered_live(raw: &str) -> Vec<&str> {
    filter_rewind_lines(raw.lines().filter(|l| !l.trim().is_empty()).collect())
}

/// Unpaired spawns across the rewind-filtered timeline. Substring pre-filter
/// keeps non-subagent lines off the JSON path.
pub(crate) fn collect_unfinished_subagents(filtered: &[&str]) -> Vec<(String, String)> {
    let mut pending: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for line in filtered {
        if !line.contains("subagent_spawned") && !line.contains("subagent_finished") {
            continue;
        }
        let raw = serde_json::from_str::<RawLinePeek<'_>>(line)
            .ok()
            .and_then(|e| e.params.map(|p| p.get()))
            .unwrap_or(line);
        let Ok(notification) = serde_json::from_str::<SessionNotification>(raw) else {
            continue;
        };
        match notification.update {
            EchoAgentUpdate::SubagentSpawned {
                subagent_id,
                child_session_id,
                ..
            } => {
                pending.insert(subagent_id, child_session_id);
            }
            EchoAgentUpdate::SubagentFinished { subagent_id, .. } => {
                pending.remove(&subagent_id);
            }
            _ => {}
        }
    }
    pending.into_iter().collect()
}

/// The raw `_meta` object of a persisted line, if any, without allocating a
/// `serde_json::Value`. Handles both the enveloped (`{method,params}`) and legacy
/// (params-at-top-level) on-disk formats.
fn line_meta(line: &str) -> Option<&serde_json::value::RawValue> {
    let env = serde_json::from_str::<RawLinePeek<'_>>(line).ok()?;
    let raw = env.params.map(|p| p.get()).unwrap_or(line);
    serde_json::from_str::<RawParamsPeek<'_>>(raw).ok()?.meta
}

/// Catalog lines stay on disk but are re-advertised after every `session/load`,
/// so replay skips them. Typed peek ignores the huge `availableCommands` array.
pub(crate) fn line_is_available_commands_update(line: &str) -> bool {
    line.contains(&*AVAILABLE_COMMANDS_UPDATE)
        && peek_line_update(line).is_some_and(|u| u.session_update == *AVAILABLE_COMMANDS_UPDATE)
}

/// Fat 100ms bash `tool_call_update`s: typed peek of `sessionUpdate` + `status`
/// only (unknown fields ignored). Completed/Failed and `status: None` stay.
pub(crate) fn line_is_in_progress_tool_call_update(line: &str) -> bool {
    if !line.contains(&*TOOL_CALL_UPDATE) || !line.contains(&*TOOL_CALL_STATUS_IN_PROGRESS) {
        return false;
    }
    peek_line_update(line).is_some_and(|u| {
        u.session_update == *TOOL_CALL_UPDATE
            && u.status == Some(TOOL_CALL_STATUS_IN_PROGRESS.as_str())
    })
}

fn peek_line_update(line: &str) -> Option<RawUpdatePeek<'_>> {
    let env = serde_json::from_str::<RawLinePeek<'_>>(line).ok()?;
    let raw = env.params.map(|p| p.get()).unwrap_or(line);
    serde_json::from_str::<RawParamsPeek<'_>>(raw).ok()?.update
}

pub(crate) fn line_is_dropped_on_replay(line: &str) -> bool {
    line_is_available_commands_update(line) || line_is_in_progress_tool_call_update(line)
}

/// Extract `_meta.totalTokens` from a persisted update line without allocating a
/// `serde_json::Value`. Returns `None` when the line carries no token count.
fn line_total_tokens(line: &str) -> Option<u64> {
    if !line.contains(TOTAL_TOKENS_KEY) {
        return None;
    }
    #[derive(serde::Deserialize)]
    struct TokensPeek {
        #[serde(rename = "totalTokens")]
        total_tokens: Option<u64>,
    }
    serde_json::from_str::<TokensPeek>(line_meta(line)?.get())
        .ok()
        .and_then(|t| t.total_tokens)
}

/// This line's `_meta.eventId`, if any. Cheap peek (no `Value`).
fn line_event_id(line: &str) -> Option<std::borrow::Cow<'_, str>> {
    if !line.contains(EVENT_ID_KEY) {
        return None;
    }
    #[derive(serde::Deserialize)]
    struct EventIdPeek<'a> {
        #[serde(rename = "eventId", borrow)]
        event_id: Option<std::borrow::Cow<'a, str>>,
    }
    serde_json::from_str::<EventIdPeek<'_>>(line_meta(line)?.get())
        .ok()
        .and_then(|e| e.event_id)
}

/// Does this line's `_meta.eventId` equal `cursor_id`?
fn line_has_event_id(line: &str, cursor_id: &str) -> bool {
    line_event_id(line).as_deref() == Some(cursor_id)
}

/// Rewind-filter, resolve the reconnect cursor, drop redundant command
/// catalogs and InProgress tool_call_updates, and scan `totalTokens`. Pure
/// data processing, no I/O.
///
/// The cursor is resolved before dropping ACUs / InProgress lines, because an
/// idle client often reconnects with one of those `eventId`s as its cursor;
/// resolving against the inclusive set keeps reconnect incremental instead of
/// a full replay.
///
/// `#[doc(hidden)] pub` (not stable API): production replay uses it, and the
/// session-load memory test drives it to check the peek stays zero-copy.
#[doc(hidden)]
pub fn prepare_replay_lines<'a>(contents: &'a str, cursor: Option<&str>) -> PreparedReplay<'a> {
    let filtered = filter_rewind_lines(contents.lines().filter(|l| !l.trim().is_empty()).collect());

    let mut max_event_seq: Option<u64> = None;
    for line in &filtered {
        if line.contains("eventId")
            && let Ok(env) = serde_json::from_str::<RawLinePeek<'_>>(line)
            && let Some(raw) = env.params.map(|p| p.get())
            && let Ok(pp) = serde_json::from_str::<RawParamsPeek<'_>>(raw)
            && let Some(meta_raw) = pp.meta
            && let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_raw.get())
            && let Some(seq) = meta
                .get("eventId")
                .and_then(|v| v.as_str())
                .and_then(|s| s.rsplit('-').next())
                .and_then(|c| c.parse::<u64>().ok())
        {
            max_event_seq = Some(max_event_seq.map_or(seq, |m| m.max(seq)));
        }
    }

    let last_tokens = filtered
        .iter()
        .rev()
        .find_map(|l| line_total_tokens(l))
        .unwrap_or(0);

    let cursor_pos = cursor
        .and_then(|id| filtered.iter().rposition(|l| line_has_event_id(l, id)))
        .filter(|&pos| {
            let bounded = filtered[pos + 1..]
                .iter()
                .all(|l| line_is_dropped_on_replay(l) || line_event_id(l).is_some());
            if !bounded {
                tracing::warn!(
                    "replay: post-cursor tail contains eventId-less lines; full replay instead"
                );
            }
            bounded
        });
    let mark_replay = cursor_pos.is_none();
    let start = cursor_pos.map_or(0, |pos| pos + 1);

    let mut lines: Vec<&str> = Vec::with_capacity(filtered.len().saturating_sub(start));
    let mut total_live = 0usize;
    for (i, &line) in filtered.iter().enumerate() {
        if line_is_dropped_on_replay(line) {
            continue;
        }
        total_live += 1;
        if i >= start {
            lines.push(line);
        }
    }

    PreparedReplay {
        lines,
        mark_replay,
        last_tokens,
        max_event_seq,
        total_live,
        unfinished_subagents: collect_unfinished_subagents(&filtered),
    }
}

/// Blank-strip, drop redundant command catalogs and InProgress tool updates,
/// and rewind-filter a raw `updates.jsonl` segment. Shared by the delta-replay
/// path (which has no reconnect cursor); the initial replay path is
/// [`prepare_replay_lines`], which additionally resolves a cursor (and so must
/// see ACUs / InProgress lines) before dropping them.
pub(crate) fn filter_delta_replay_lines(contents: &str) -> Vec<&str> {
    let live: Vec<&str> = contents
        .lines()
        .filter(|l| !l.trim().is_empty() && !line_is_dropped_on_replay(l))
        .collect();
    filter_rewind_lines(live)
}
