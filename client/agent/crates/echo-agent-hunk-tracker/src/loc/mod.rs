//! LOC (Lines of Code) tracking — hunk-level attribution records.
//!
//! This module provides:
//! - [`HunkRecord`]: a serializable attribution record derived from a [`Hunk`].
//! - [`HunkRecordWriter`] / [`JsonlHunkRecordWriter`]: append-only JSONL persistence.
//! - [`run_loc_sink`]: an async task that consumes [`HunkEvent`]s and writes records.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;

use crate::events::{HunkEvent, HunkRemovalReason};
use crate::types::{Hunk, HunkId, HunkSource};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Who authored a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthorType {
    Agent,
    Human,
}

impl std::fmt::Display for AuthorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Agent => f.write_str("agent"),
            Self::Human => f.write_str("human"),
        }
    }
}

/// Mirror of [`HunkSource`] for serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceType {
    AgentEdit,
    ExternalEditOnAgentFile,
    External,
}

impl std::fmt::Display for SourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AgentEdit => f.write_str("agent_edit"),
            Self::ExternalEditOnAgentFile => f.write_str("external_edit_on_agent_file"),
            Self::External => f.write_str("external"),
        }
    }
}

/// Whether a record represents a new hunk or an in-place update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventType {
    /// A new hunk was created.
    Added,
    /// An existing hunk's content changed in place.
    Updated,
    /// A hunk was removed. `lines_added` / `lines_removed` are negated
    /// so that `SUM` zeroes out the hunk's accumulated contribution.
    Removed,
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added => f.write_str("added"),
            Self::Updated => f.write_str("updated"),
            Self::Removed => f.write_str("removed"),
        }
    }
}

// ---------------------------------------------------------------------------
// HunkRecord
// ---------------------------------------------------------------------------

/// A single LOC attribution record derived from a [`Hunk`].
///
/// Each record captures who authored a hunk (agent vs human), along with
/// enough context (session, file, line range) for downstream analytics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HunkRecord {
    /// Stable hunk identifier (UUID).
    pub hunk_id: HunkId,
    /// Absolute file path.
    pub file_path: PathBuf,
    /// Start line of the hunk in the new file (1-indexed).
    pub hunk_start: usize,
    /// End line of the hunk in the new file (inclusive).
    pub hunk_end: usize,
    /// Lines added. For [`EventType::Added`] this is the full count (≥ 0).
    /// For [`EventType::Updated`] this is the delta from the previous state
    /// and may be negative (hunk shrank).
    pub lines_added: i64,
    /// Lines removed. For [`EventType::Added`] this is the full count (≥ 0).
    /// For [`EventType::Updated`] this is the delta and may be negative.
    pub lines_removed: i64,
    /// Who authored this change. `None` for [`EventType::Removed`] records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_type: Option<AuthorType>,
    /// For agent edits: the agent id. For human edits: the user id (if known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
    /// Machine-level agent identifier.
    pub agent_id: String,
    /// Session that produced this hunk.
    pub session_id: String,
    /// When the hunk was first detected.
    pub timestamp: DateTime<Utc>,
    /// Prompt index for agent edits, `None` for human edits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_index: Option<usize>,
    /// Which [`HunkSource`] variant produced this change. `None` for [`EventType::Removed`] records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_type: Option<SourceType>,
    /// Whether this is a new hunk or an in-place update.
    pub event_type: EventType,
    /// Why the hunk was removed. Only set for [`EventType::Removed`] records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removal_reason: Option<HunkRemovalReason>,
}

impl HunkRecord {
    /// Derive a [`HunkRecord`] from a [`Hunk`].
    ///
    /// `agent_id` is the stable machine-level identifier.
    /// `user_id` is the authenticated user id (used for human-attributed hunks).
    /// `event_type` distinguishes new hunks from in-place updates.
    ///
    /// `attribution_source` controls which [`HunkSource`] is used for author
    /// attribution. For `HunkAdded` events this is `hunk.source`. For
    /// `HunkContentChanged` events this should be the *trigger* source
    /// (the source of the edit that caused the change), not the hunk's
    /// preserved source, since source-preservation logic may have kept the
    /// original agent attribution even though a human made the edit.
    pub fn from_hunk(
        hunk: &Hunk,
        session_id: &str,
        agent_id: &str,
        user_id: Option<&str>,
        event_type: EventType,
        attribution_source: &HunkSource,
    ) -> Self {
        let (author_type, author_id, prompt_index, source_type) = match *attribution_source {
            HunkSource::AgentEdit { prompt_index } => (
                AuthorType::Agent,
                Some(agent_id.to_owned()),
                Some(prompt_index),
                SourceType::AgentEdit,
            ),
            HunkSource::ExternalEditOnAgentFile => (
                AuthorType::Human,
                user_id.map(str::to_owned),
                None,
                SourceType::ExternalEditOnAgentFile,
            ),
            HunkSource::External => (
                AuthorType::Human,
                user_id.map(str::to_owned),
                None,
                SourceType::External,
            ),
        };

        // For pure deletions (new_count == 0) use old_start/old_count.
        let (start, count) = if hunk.line_info.new_count == 0 {
            (hunk.line_info.old_start, hunk.line_info.old_count)
        } else {
            (hunk.line_info.new_start, hunk.line_info.new_count)
        };
        let end = if count == 0 { start } else { start + count - 1 };

        Self {
            hunk_id: hunk.id.clone(),
            file_path: hunk.path.clone(),
            hunk_start: start,
            hunk_end: end,
            lines_added: hunk.line_info.new_count as i64,
            lines_removed: hunk.line_info.old_count as i64,
            author_type: Some(author_type),
            author_id,
            agent_id: agent_id.to_owned(),
            session_id: session_id.to_owned(),
            timestamp: hunk.created_at,
            prompt_index,
            source_type: Some(source_type),
            event_type,
            removal_reason: None,
        }
    }
}

// ---------------------------------------------------------------------------
// HunkRecordWriter
// ---------------------------------------------------------------------------

/// Trait for persisting [`HunkRecord`]s.
///
/// Implementations may write to JSONL files, databases, etc.
///
/// All returned futures must be `Send` because `run_loc_sink` is spawned
/// via `tokio::spawn` (which may run the task on any thread in the pool).
pub trait HunkRecordWriter: Send {
    /// Write a single record. Errors are non-fatal; callers log and continue.
    fn write(
        &mut self,
        record: &HunkRecord,
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send;

    /// Flush any buffered data. Called during shutdown.
    fn flush(&mut self) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
}

/// Append-only JSONL writer for [`HunkRecord`]s.
///
/// The file is opened lazily on the first write so that sessions that produce
/// no hunk events never create an empty file on disk.
pub struct JsonlHunkRecordWriter {
    path: PathBuf,
    file: Option<tokio::fs::File>,
}

impl JsonlHunkRecordWriter {
    /// Create a writer that will append to the given path.
    ///
    /// The parent directory is created on the first write if it does not exist.
    pub fn new(path: PathBuf) -> Self {
        Self { path, file: None }
    }

    /// Lazily open (or create) the file in append mode.
    async fn ensure_open(&mut self) -> std::io::Result<&mut tokio::fs::File> {
        if self.file.is_none() {
            if let Some(parent) = self.path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .await?;
            self.file = Some(file);
        }
        // The `if` block above guarantees `self.file` is `Some` at this point.
        Ok(self.file.as_mut().unwrap())
    }
}

impl HunkRecordWriter for JsonlHunkRecordWriter {
    async fn write(&mut self, record: &HunkRecord) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;

        let file = self.ensure_open().await?;
        let mut line = serde_json::to_string(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        file.write_all(line.as_bytes()).await?;
        Ok(())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;

        if let Some(file) = self.file.as_mut() {
            file.flush().await?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// LocAggregate (channel-based bridge to signals)
// ---------------------------------------------------------------------------

/// Lightweight aggregate update emitted by the LOC sink for consumption by
/// an external bridge (e.g., the signals system in `echo-agent-runtime`).
///
/// The sink sends one of these per processed `HunkEvent` that affects LOC.
/// The bridge task translates them into `SignalEvent` variants.
#[derive(Debug, Clone)]
pub enum LocAggregate {
    /// Lines were added or changed (from HunkAdded or HunkContentChanged).
    LinesChanged {
        author_type: AuthorType,
        lines_added: i64,
        lines_removed: i64,
        file_path: PathBuf,
    },
    /// A hunk was reverted (rejected or superseded). The values are the
    /// accumulated totals that were zeroed out — always non-negative.
    LinesReverted {
        lines_added_reverted: i64,
        lines_removed_reverted: i64,
    },
}

// ---------------------------------------------------------------------------
// Sink configuration
// ---------------------------------------------------------------------------

/// Context passed to the LOC sink at spawn time.
pub struct LocSinkContext {
    /// Session identifier.
    pub session_id: String,
    /// Stable machine-level agent identifier.
    pub agent_id: String,
    /// Authenticated user id (if available). Used for human-attributed records.
    pub user_id: Option<String>,
    /// Optional channel for emitting LOC aggregates to an external consumer
    /// (e.g., the session signals system). When `None`, only JSONL is written.
    pub aggregate_tx: Option<mpsc::UnboundedSender<LocAggregate>>,
}

// ---------------------------------------------------------------------------
// run_loc_sink
// ---------------------------------------------------------------------------

/// Consume [`HunkEvent`]s and write LOC attribution records.
///
/// This is the main entry point for the LOC tracking pipeline. It runs as a
/// long-lived async task and should be spawned via `tokio::spawn`.
///
/// The sink maintains a `HashMap<HunkId, (i64, i64)>` tracking accumulated
/// `(lines_added, lines_removed)` per hunk. When a `HunkRemoved` event
/// arrives, the accumulated total is negated and written as a `Removed`
/// record, zeroing out the hunk's contribution in SUM-based totals.
///
/// On cancellation the task drains any remaining events from the channel so
/// that no in-flight records are lost.
pub async fn run_loc_sink(
    mut event_rx: mpsc::UnboundedReceiver<HunkEvent>,
    mut writer: impl HunkRecordWriter,
    ctx: LocSinkContext,
    cancellation_token: tokio_util::sync::CancellationToken,
) {
    // Accumulated (lines_added, lines_removed) per hunk_id.
    // Used to emit negating records when hunks are rejected/superseded.
    let mut acc: HashMap<HunkId, (i64, i64)> = HashMap::new();

    loop {
        tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => {
                tracing::debug!("LOC sink: cancellation received, draining remaining events");
                drain_remaining(&mut event_rx, &mut writer, &ctx, &mut acc).await;
                break;
            }
            event = event_rx.recv() => {
                let Some(event) = event else {
                    // Channel closed — sender dropped.
                    tracing::debug!("LOC sink: event channel closed");
                    break;
                };
                handle_event(event, &mut writer, &ctx, &mut acc).await;
            }
        }
    }

    if let Err(e) = writer.flush().await {
        tracing::warn!(error = %e, "LOC sink: failed to flush writer on shutdown");
    }
    tracing::debug!("LOC sink: exiting");
}

/// Process a single [`HunkEvent`].
async fn handle_event(
    event: HunkEvent,
    writer: &mut impl HunkRecordWriter,
    ctx: &LocSinkContext,
    acc: &mut HashMap<HunkId, (i64, i64)>,
) {
    match event {
        HunkEvent::HunkAdded { path: _, ref hunk } => {
            // For new hunks, the hunk's own source is the correct attribution.
            // lines_added/lines_removed are the full counts (no prior state).
            let record = HunkRecord::from_hunk(
                hunk,
                &ctx.session_id,
                &ctx.agent_id,
                ctx.user_id.as_deref(),
                EventType::Added,
                &hunk.source,
            );
            let entry = acc.entry(hunk.id.clone()).or_insert((0, 0));
            entry.0 += record.lines_added;
            entry.1 += record.lines_removed;
            // Emit aggregate for signals bridge
            if let Some(tx) = &ctx.aggregate_tx {
                let _ = tx.send(LocAggregate::LinesChanged {
                    author_type: record.author_type.unwrap_or(AuthorType::Agent),
                    lines_added: record.lines_added,
                    lines_removed: record.lines_removed,
                    file_path: record.file_path.clone(),
                });
            }
            write_record(&record, writer).await;
        }
        HunkEvent::HunkContentChanged {
            path: _,
            ref hunk,
            trigger_source,
            prev_lines_added,
            prev_lines_removed,
        } => {
            // For in-place changes, use the trigger source for attribution
            // and record only the delta (new - prev) so LOC totals can be
            // computed with a simple SUM grouped by author_type.
            let mut record = HunkRecord::from_hunk(
                hunk,
                &ctx.session_id,
                &ctx.agent_id,
                ctx.user_id.as_deref(),
                EventType::Updated,
                &trigger_source,
            );
            // Replace full counts with signed deltas so shrinking hunks
            // (e.g., human deletes 3 of 10 agent lines) produce negative
            // values that correctly reduce the total on SUM.
            record.lines_added = hunk.line_info.new_count as i64 - prev_lines_added as i64;
            record.lines_removed = hunk.line_info.old_count as i64 - prev_lines_removed as i64;
            let entry = acc.entry(hunk.id.clone()).or_insert((0, 0));
            entry.0 += record.lines_added;
            entry.1 += record.lines_removed;
            // Emit aggregate for signals bridge
            if let Some(tx) = &ctx.aggregate_tx {
                let _ = tx.send(LocAggregate::LinesChanged {
                    author_type: record.author_type.unwrap_or(AuthorType::Human),
                    lines_added: record.lines_added,
                    lines_removed: record.lines_removed,
                    file_path: record.file_path.clone(),
                });
            }
            write_record(&record, writer).await;
        }
        HunkEvent::HunkRemoved {
            path,
            hunk_id,
            reason,
        } => {
            match reason {
                HunkRemovalReason::Accepted => {
                    // Accepted hunks keep their LOC contribution — just
                    // clear the accumulated state without writing a
                    // negating record.
                    acc.remove(&hunk_id);
                }
                HunkRemovalReason::Rejected | HunkRemovalReason::Superseded => {
                    // Rejected/superseded hunks lose their LOC — negate
                    // the accumulated totals so SUM zeroes them out.
                    if let Some((total_added, total_removed)) = acc.remove(&hunk_id)
                        && (total_added != 0 || total_removed != 0)
                    {
                        // Emit revert aggregate for signals bridge
                        if let Some(tx) = &ctx.aggregate_tx {
                            let _ = tx.send(LocAggregate::LinesReverted {
                                lines_added_reverted: total_added.max(0),
                                lines_removed_reverted: total_removed.max(0),
                            });
                        }
                        let record = HunkRecord {
                            hunk_id: hunk_id.clone(),
                            file_path: path,
                            hunk_start: 0,
                            hunk_end: 0,
                            lines_added: -total_added,
                            lines_removed: -total_removed,
                            author_type: None,
                            author_id: None,
                            agent_id: ctx.agent_id.clone(),
                            session_id: ctx.session_id.clone(),
                            timestamp: Utc::now(),
                            prompt_index: None,
                            source_type: None,
                            event_type: EventType::Removed,
                            removal_reason: Some(reason),
                        };
                        write_record(&record, writer).await;
                    }
                }
            }
        }
        HunkEvent::HunkMoved { .. } => {
            tracing::trace!("LOC sink: ignoring HunkMoved event");
        }
        HunkEvent::FileAdded { .. } => {
            tracing::trace!("LOC sink: ignoring FileAdded event");
        }
        HunkEvent::FileRemoved { .. } => {
            tracing::trace!("LOC sink: ignoring FileRemoved event");
        }
        HunkEvent::BaselineUpdated { .. } => {
            tracing::trace!("LOC sink: ignoring BaselineUpdated event");
        }
    }
}

/// Write a [`HunkRecord`], logging on success/failure.
async fn write_record(record: &HunkRecord, writer: &mut impl HunkRecordWriter) {
    if let Err(e) = writer.write(record).await {
        tracing::warn!(
            error = %e,
            hunk_id = %record.hunk_id,
            file_path = %record.file_path.display(),
            "LOC sink: failed to write hunk record, dropping"
        );
    } else {
        tracing::debug!(
            hunk_id = %record.hunk_id,
            file_path = %record.file_path.display(),
            author_type = ?record.author_type,
            "LOC sink: wrote hunk record"
        );
    }
}

/// Drain remaining events after cancellation.
async fn drain_remaining(
    event_rx: &mut mpsc::UnboundedReceiver<HunkEvent>,
    writer: &mut impl HunkRecordWriter,
    ctx: &LocSinkContext,
    acc: &mut HashMap<HunkId, (i64, i64)>,
) {
    let mut count = 0usize;
    while let Ok(event) = event_rx.try_recv() {
        handle_event(event, writer, ctx, acc).await;
        count += 1;
    }
    if count > 0 {
        tracing::debug!(
            count,
            "LOC sink: drained remaining events after cancellation"
        );
    }
}
