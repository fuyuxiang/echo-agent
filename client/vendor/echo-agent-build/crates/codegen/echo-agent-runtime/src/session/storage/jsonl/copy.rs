//! Session fork/copy for the JSONL adapter.
//!
//! The `updates.jsonl` transcript is unbounded, so the copy streams it line by
//! line: peak memory tracks a single capped line, plus one small per-line
//! record when a prompt cut is requested. Chat history stays materialized: its
//! transforms need random access and the compacted history is bounded by the
//! context window.

use std::collections::BTreeSet;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, Write};
use std::ops::ControlFlow;
use std::path::Path;

use agent_client_protocol as acp;

use crate::sampling::{
    ConversationItem, conversation_truncate_for_prompt, fork_filter_chat,
    transform_conversation_cwd,
};
use crate::session::info::Info;
use crate::session::persistence::{CHAT_FORMAT_VERSION, Summary};
use crate::session::storage::jsonl::{JsonlStorageAdapter, transform_session_id_in_update};
use crate::session::storage::{
    CopySessionOptions, CopySessionResult, RewindStep, SessionUpdate, SessionUpdateEnvelope,
    filter_rewind_by, rewind_step_for_line, truncate_for_prompt_by,
};

#[cfg(test)]
#[path = "copy_tests.rs"]
mod tests;

fn is_orchestration_projection_update(update: &SessionUpdate) -> bool {
    matches!(
        update,
        SessionUpdate::EchoAgent(notification)
            if matches!(
                &notification.update,
                crate::extensions::notification::SessionUpdate::WorkflowUpdated { .. }
                    | crate::extensions::notification::SessionUpdate::GoalUpdated { .. }
            )
    )
}

/// Updates written plus the `compaction_checkpoints/{uuid}.json` files the
/// surviving records reference, collected in the same pass.
#[derive(Default)]
struct CopiedUpdates {
    count: usize,
    checkpoint_files: BTreeSet<String>,
}

/// Longest `updates.jsonl` line the copy will buffer; anything past it is
/// corruption (e.g. a tail that lost its newlines) and is discarded without
/// being buffered. Discarded lines consume no index in either pass, unlike
/// torn lines, which classify as [`RewindStep::Other`] and end a user run.
const MAX_UPDATE_LINE_BYTES: usize = 64 * 1024 * 1024;

/// [`for_each_jsonl_line_capped`] with the production cap.
fn for_each_jsonl_line<R: BufRead>(
    reader: R,
    f: impl FnMut(usize, &[u8]) -> io::Result<ControlFlow<()>>,
) -> io::Result<()> {
    for_each_jsonl_line_capped(reader, MAX_UPDATE_LINE_BYTES, f)
}

/// Invoke `f` with the index and bytes of each non-empty line, reusing one
/// capped line buffer. Lines over `cap` content bytes are discarded without
/// being buffered whole and consume no index. `f` returns `Break` to stop
/// early. Raw bytes rather than the typed `UpdatesIterator`: classification
/// must tolerate non-UTF-8 lines, and both copy passes need identical line
/// indexes.
fn for_each_jsonl_line_capped<R: BufRead>(
    mut reader: R,
    cap: usize,
    mut f: impl FnMut(usize, &[u8]) -> io::Result<ControlFlow<()>>,
) -> io::Result<()> {
    let mut buf = Vec::new();
    let mut index = 0;
    let mut discarded = 0usize;
    let result = loop {
        buf.clear();
        let n = reader
            .by_ref()
            .take(cap as u64 + 1)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            break Ok(());
        }
        if buf.len() > cap && buf.last() != Some(&b'\n') {
            discarded += 1;
            if discarded == 1 {
                tracing::warn!(
                    max_bytes = cap,
                    "discarding over-long updates.jsonl line during fork copy"
                );
            }
            // Drain the remainder of the line without retaining it.
            loop {
                buf.clear();
                let n = reader
                    .by_ref()
                    .take(cap as u64)
                    .read_until(b'\n', &mut buf)?;
                if n == 0 || buf.last() == Some(&b'\n') {
                    break;
                }
            }
            continue;
        }
        let line = buf.trim_ascii();
        if line.is_empty() {
            continue;
        }
        if f(index, line)?.is_break() {
            break Ok(());
        }
        index += 1;
    };
    if discarded > 1 {
        tracing::warn!(
            discarded,
            max_bytes = cap,
            "discarded over-long updates.jsonl lines during fork copy"
        );
    }
    result
}

/// Indexes (in non-empty-line order) of the source lines that survive rewind
/// filtering and the `target_prompt_index` cut, holding one classification per
/// line instead of the lines. As in replay, an unparseable line classifies as
/// [`RewindStep::Other`] (ending a user run) and is skipped later at parse.
fn surviving_line_indexes<R: BufRead>(
    reader: R,
    target_prompt_index: usize,
) -> io::Result<Vec<usize>> {
    struct LineRecord {
        index: usize,
        step: RewindStep,
    }
    let mut records = Vec::new();
    for_each_jsonl_line(reader, |index, line| {
        let step = std::str::from_utf8(line).map_or(RewindStep::Other, rewind_step_for_line);
        records.push(LineRecord { index, step });
        Ok(ControlFlow::Continue(()))
    })?;
    let mut records = filter_rewind_by(records, |record| record.step);
    let keep = truncate_for_prompt_by(&records, target_prompt_index, |record| record.step);
    records.truncate(keep);
    Ok(records.into_iter().map(|record| record.index).collect())
}

/// Streaming writer for the fork target's `updates.jsonl`. Corruption-tolerant
/// like the load path: a torn or undecodable line is skipped with a warning
/// instead of failing the fork.
struct UpdateLineWriter<'a> {
    writer: BufWriter<std::fs::File>,
    source: &'a Path,
    target_session_id: &'a acp::SessionId,
    copied: CopiedUpdates,
    skipped_lines: usize,
}

impl<'a> UpdateLineWriter<'a> {
    fn try_new(
        target: &Path,
        source: &'a Path,
        target_session_id: &'a acp::SessionId,
    ) -> io::Result<Self> {
        Ok(Self {
            writer: BufWriter::new(std::fs::File::create(target)?),
            source,
            target_session_id,
            copied: CopiedUpdates::default(),
            skipped_lines: 0,
        })
    }

    fn copy_line(&mut self, line: &[u8]) -> io::Result<()> {
        let update = match std::str::from_utf8(line).map(SessionUpdateEnvelope::from_str) {
            Ok(Ok(update)) => update,
            Ok(Err(error)) => {
                self.skip_torn_line(&error);
                return Ok(());
            }
            Err(error) => {
                self.skip_torn_line(&error);
                return Ok(());
            }
        };
        if is_orchestration_projection_update(&update) {
            return Ok(());
        }
        if let SessionUpdate::EchoAgent(notification) = &update
            && let crate::extensions::notification::SessionUpdate::CompactionCheckpoint(info) =
                &notification.update
        {
            self.copied
                .checkpoint_files
                .insert(info.checkpoint_file.clone());
        }
        let update = transform_session_id_in_update(update, self.target_session_id);
        let envelope = SessionUpdateEnvelope::from_update(&update).map_err(invalid_data)?;
        serde_json::to_writer(&mut self.writer, &envelope).map_err(invalid_data)?;
        self.writer.write_all(b"\n")?;
        self.copied.count += 1;
        Ok(())
    }

    fn skip_torn_line(&mut self, error: &dyn std::fmt::Display) {
        self.skipped_lines += 1;
        if self.skipped_lines == 1 {
            tracing::warn!(
                error = %error,
                path = %self.source.display(),
                "skipping unparseable updates.jsonl line during fork copy (torn append?)"
            );
        }
    }

    fn finish(mut self) -> io::Result<CopiedUpdates> {
        // The first skipped line already warned with its parse error.
        if self.skipped_lines > 1 {
            tracing::warn!(
                skipped = self.skipped_lines,
                copied = self.copied.count,
                path = %self.source.display(),
                "skipped unparseable session update lines during fork copy"
            );
        }
        self.writer.flush()?;
        Ok(self.copied)
    }
}

/// Copy `source` (an `updates.jsonl`) to `target` without materializing it.
/// With a `target_prompt_index`, pass one computes the surviving line set and
/// pass two writes exactly those lines; without one, every line streams
/// through, preserving rewind markers and dead branches. Both passes read one
/// pinned, rewound file handle, so their line indexes cannot skew under a
/// concurrent rename; `updates.jsonl` is append-only by contract, so lines
/// appended after pass one land past every survivor index.
fn copy_updates_streaming(
    source: &Path,
    target: &Path,
    target_session_id: &acp::SessionId,
    target_prompt_index: Option<usize>,
) -> io::Result<CopiedUpdates> {
    let mut writer = UpdateLineWriter::try_new(target, source, target_session_id)?;
    let mut file = match std::fs::File::open(source) {
        Ok(file) => file,
        // A missing source is an empty transcript; still write the target.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return writer.finish(),
        Err(error) => return Err(error),
    };
    match target_prompt_index {
        None => {
            for_each_jsonl_line(BufReader::new(file), |_, line| {
                writer.copy_line(line)?;
                Ok(ControlFlow::Continue(()))
            })?;
        }
        Some(target_idx) => {
            let survivors = surviving_line_indexes(BufReader::new(&mut file), target_idx)?;
            file.seek(io::SeekFrom::Start(0))?;
            let mut survivors = survivors.into_iter().peekable();
            for_each_jsonl_line(BufReader::new(file), |index, line| {
                if survivors.next_if_eq(&index).is_some() {
                    writer.copy_line(line)?;
                }
                Ok(if survivors.peek().is_none() {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                })
            })?;
        }
    }
    writer.finish()
}

impl JsonlStorageAdapter {
    /// Fully synchronous implementation of `copy_session_data`, for use on a
    /// blocking thread; every caller reaches it through `spawn_blocking`.
    pub(crate) fn copy_session_data_sync(
        &self,
        source_info: &Info,
        target_info: &Info,
        options: CopySessionOptions,
    ) -> io::Result<CopySessionResult> {
        // Canonical creator: the fork target chain is born owner-only.
        let target_dir = self.create_session_dir_owner_only(target_info)?;

        let source_summary = self.read_summary_sync(source_info)?;
        let chat_format_version = source_summary.chat_format_version;

        let mut chat_to_copy: Vec<ConversationItem> =
            self.read_chat_history_sync(self.chat_file(source_info), chat_format_version)?;

        if let Some(target_idx) = options.target_prompt_index {
            let updates = self.updates_file(source_info);
            if options.fresh_title
                && crate::session::helpers::replay::find_latest_compaction_checkpoint(&updates)?
                    .is_some()
            {
                // A current compacted summary can describe work AFTER the cut.
                // Use the same checkpoint-aware reconstruction as rewind.
                let replay = crate::session::helpers::replay::replay_to_prompt_with_context(
                    &updates,
                    &self.session_dir(source_info),
                    target_idx + 1,
                )?;
                if replay.prompt_index_reached != target_idx + 1 {
                    return Err(io::Error::other(
                        "无法完整恢复所选历史轮次，请刷新会话后重试",
                    ));
                }
                if matches!(
                    replay.conversation.first(),
                    Some(ConversationItem::System(_))
                ) {
                    chat_to_copy = replay.conversation;
                } else {
                    let mut prefix = Vec::new();
                    if let Some(system @ ConversationItem::System(_)) = chat_to_copy.first() {
                        prefix.push(system.clone());
                        if let Some(original) = replay.original_user_info {
                            prefix.push(ConversationItem::user(original));
                        } else if let Some(user @ ConversationItem::User(_)) = chat_to_copy.get(1) {
                            prefix.push(user.clone());
                        }
                    }
                    prefix.extend(replay.conversation);
                    chat_to_copy = prefix;
                }
            } else {
                // +1: the cut keeps the target prompt inclusive.
                let keep = conversation_truncate_for_prompt(&chat_to_copy, target_idx + 1);
                chat_to_copy.truncate(keep);
            }
        }

        if options.fork_filter {
            fork_filter_chat(&mut chat_to_copy);
        }

        for target in [
            self.workflows_dir(target_info),
            self.goal_mode_state_file(target_info)
                .parent()
                .expect("goal state has a parent")
                .to_path_buf(),
        ] {
            match std::fs::remove_dir_all(&target) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }

        // The child inherits everything below this boundary; compaction
        // preserves it.
        let inherited_prefix_len = if options.fork_filter {
            Some(chat_to_copy.len())
        } else {
            options.inherited_prefix_len
        };

        // Worktree forks skip the cwd rewrite: their display_cwd already
        // shows the model the original project path, and rewritten
        // conversation paths would contradict it.
        if !options.skip_cwd_transform && source_info.cwd != target_info.cwd {
            transform_conversation_cwd(&mut chat_to_copy, &source_info.cwd, &target_info.cwd);
        }

        if options.strip_reasoning {
            chat_to_copy =
                echo_agent_chat_state::compaction_utils::strip_reasoning_blocks(chat_to_copy);
        }

        let num_chat_messages = chat_to_copy.len();
        let cwd_switch_bookkeeping_generation = chat_to_copy
            .iter()
            .filter_map(ConversationItem::working_directory_switch_generation)
            .max()
            .unwrap_or(0);

        // Release chat history before the (typically much larger) updates copy.
        {
            let mut writer = BufWriter::new(std::fs::File::create(self.chat_file(target_info))?);
            for item in &chat_to_copy {
                serde_json::to_writer(&mut writer, item).map_err(invalid_data)?;
                writer.write_all(b"\n")?;
            }
            writer.flush()?;
        }
        drop(chat_to_copy);

        // A fork_filter copy (subagent context bootstrap) starts the child with
        // an empty replay transcript, so the source updates are never read.
        let mut copied_updates = if options.fork_filter {
            std::fs::write(self.updates_file(target_info), b"")?;
            CopiedUpdates::default()
        } else {
            copy_updates_streaming(
                &self.updates_file(source_info),
                &self.updates_file(target_info),
                &target_info.id,
                options.target_prompt_index,
            )?
        };
        if options.fresh_title && !options.copy_plan_state {
            // Retain historical events, then explicitly end their authority over
            // the child's current plan. Omitting plan.json alone cannot do this:
            // clients restore the active plan from the replay stream.
            let reset = SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
                target_info.id.clone(),
                acp::SessionUpdate::Plan(acp::Plan::new(Vec::new())),
            )));
            let envelope = SessionUpdateEnvelope::from_update(&reset).map_err(invalid_data)?;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(self.updates_file(target_info))?;
            serde_json::to_writer(&mut file, &envelope).map_err(invalid_data)?;
            file.write_all(b"\n")?;
            copied_updates.count += 1;
        }
        let checkpoint_files = copied_updates.checkpoint_files;
        let num_messages = copied_updates.count;

        let target_summary = fork_summary(
            source_summary,
            target_info,
            &options,
            ForkCounters {
                num_messages,
                num_chat_messages,
                cwd_switch_bookkeeping_generation,
                inherited_prefix_len,
            },
        );
        let summary_bytes = serde_json::to_vec_pretty(&target_summary).map_err(invalid_data)?;
        std::fs::write(self.summary_file(target_info), summary_bytes)?;

        let plan_copied = copy_sidecar_file(
            options.copy_plan_state,
            &self.plan_file(source_info),
            &self.plan_file(target_info),
        )?;
        let signals_copied = copy_sidecar_file(
            options.copy_signals,
            &self.signals_file(source_info),
            &self.signals_file(target_info),
        )?;
        let plan_mode_state_copied = copy_sidecar_file(
            options.copy_plan_mode_state,
            &self.plan_mode_state_file(source_info),
            &self.plan_mode_state_file(target_info),
        )?;
        let tool_state_copied = copy_sidecar_file(
            options.copy_tool_state,
            &self.session_dir(source_info).join("tool_state.json"),
            &self.session_dir(target_info).join("tool_state.json"),
        )?;
        let announcement_state_copied = copy_sidecar_file(
            options.copy_announcement_state,
            &self.announcement_state_file(source_info),
            &self.announcement_state_file(target_info),
        )?;

        // Title-refresh watermark: only a managed parent (one with a watermark)
        // passes managed state to the child, so a fork of a pre-feature session
        // stays unmanaged (frozen) rather than being adopted. A full fork
        // inherits the parent's checkpoint (keeping the inherited title frozen);
        // a partial fork starts fresh at `0` so it can retitle its shorter
        // conversation.
        if let Some(parent_idx) =
            crate::session::helpers::session_summary::load_title_refresh_watermark(
                &self.session_dir(source_info),
            )
        {
            let child_idx = if options.target_prompt_index.is_none() {
                parent_idx
            } else {
                0
            };
            crate::session::helpers::session_summary::save_title_refresh_watermark(
                &self.session_dir(target_info),
                child_idx,
            );
        }

        // Copied verbatim: the archive is immutable, so no cwd rewrite.
        let compaction_segments_copied = if options.copy_compaction_segments {
            let src_dir = self
                .session_dir(source_info)
                .join(echo_agent_compaction_transcript::COMPACTION_DIR);
            let mut copied = 0usize;
            if src_dir.is_dir() {
                let dst_dir = self
                    .session_dir(target_info)
                    .join(echo_agent_compaction_transcript::COMPACTION_DIR);
                std::fs::create_dir_all(&dst_dir)?;
                for entry in std::fs::read_dir(&src_dir)? {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        std::fs::copy(entry.path(), dst_dir.join(entry.file_name()))?;
                        copied += 1;
                    }
                }
            }
            copied
        } else {
            0
        };

        let compaction_checkpoints_copied = copy_referenced_checkpoints(
            &checkpoint_files,
            &self.session_dir(source_info),
            &target_dir,
            &source_info.id,
        )?;

        Ok(CopySessionResult {
            chat_messages_copied: num_chat_messages,
            updates_copied: num_messages,
            plan_state_copied: plan_copied,
            plan_mode_state_copied,
            signals_copied,
            tool_state_copied,
            announcement_state_copied,
            compaction_segments_copied,
            compaction_checkpoints_copied,
        })
    }
}

/// Counters produced by this copy that feed the fork target's summary, named
/// so the same-typed counts cannot transpose.
struct ForkCounters {
    num_messages: usize,
    num_chat_messages: usize,
    cwd_switch_bookkeeping_generation: u64,
    inherited_prefix_len: Option<usize>,
}

/// Build the fork target's summary: counters from this copy, fork identity
/// from `options`, and per field either inheritance from the source or a
/// fresh-session reset.
fn fork_summary(
    source: Summary,
    target_info: &Info,
    options: &CopySessionOptions,
    counters: ForkCounters,
) -> Summary {
    Summary {
        info: target_info.clone(),
        cwd_generation: source.cwd_generation,
        previous_cwd: source.previous_cwd,
        pending_cwd_switch_reminder: None,
        cwd_switch_bookkeeping_generation: counters.cwd_switch_bookkeeping_generation,
        session_summary: if options.fresh_title {
            String::new()
        } else {
            source.session_summary
        },
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        num_messages: counters.num_messages,
        num_chat_messages: counters.num_chat_messages,
        current_model_id: options
            .new_model_id
            .clone()
            .map(acp::ModelId::new)
            .unwrap_or(source.current_model_id),
        parent_session_id: options.parent_session_id.clone(),
        forked_at: Some(chrono::Utc::now()),
        collection_id: None,
        next_trace_turn: 0,
        chat_format_version: CHAT_FORMAT_VERSION,
        prompt_display_cwd: options.prompt_display_cwd.clone(),
        session_kind: Some(
            options
                .session_kind
                .clone()
                .unwrap_or_else(|| "fork".to_string()),
        ),
        fork_context_source: options.fork_context_source.clone(),
        fork_parent_prompt_id: options.fork_parent_prompt_id.clone(),
        inherited_prefix_len: counters.inherited_prefix_len,
        hidden: None,
        source_workspace_dir: options.source_workspace_dir.clone(),
        git_root_dir: None,
        git_remotes: Vec::new(),
        head_commit: source.head_commit,
        head_branch: source.head_branch,
        request_id: None,
        // Fresh local echo_agent_home, not inherited from source: the fork lives on this machine.
        echo_agent_home: crate::session::persistence::echo_agent_home_string(),
        last_active_at: source.last_active_at,
        generated_title: if options.fresh_title {
            None
        } else {
            source.generated_title
        },
        // A fork keeps the parent's title, so its manual-ness rides along.
        title_is_manual: !options.fresh_title && source.title_is_manual,
        worktree_label: source.worktree_label,
        agent_name: source.agent_name,
        sandbox_profile: source.sandbox_profile,
        reasoning_effort: source.reasoning_effort,
        // Full forks keep the parent's last turn. Partial forks
        // (`target_prompt_index`) may drop that turn, so clear the summary
        // rather than showing work that is not in the child conversation.
        last_turn_summary: if options.target_prompt_index.is_some() {
            None
        } else {
            source.last_turn_summary
        },
        last_turn_summary_prompt_id: if options.target_prompt_index.is_some() {
            None
        } else {
            source.last_turn_summary_prompt_id
        },
        // A recap describes the parent's whole session; a partial fork may not
        // contain that work, so clear it there and keep it for full forks.
        last_recap: if options.target_prompt_index.is_some() {
            None
        } else {
            source.last_recap
        },
    }
}

/// Copy one optional sidecar file (plan, signals, tool state, ...) when
/// enabled and present; reports whether a copy happened. A sidecar that
/// exists but is not a regular file is skipped with a warning rather than
/// failing the fork.
fn copy_sidecar_file(enabled: bool, src: &Path, dst: &Path) -> io::Result<bool> {
    if !enabled {
        return Ok(false);
    }
    if !src.is_file() {
        if src.exists() {
            tracing::warn!(
                path = %src.display(),
                "sidecar is not a regular file; skipping copy",
            );
        }
        return Ok(false);
    }
    std::fs::copy(src, dst)?;
    Ok(true)
}

/// Copy the `compaction_checkpoints/{uuid}.json` files referenced by the
/// retained records; returns how many copied. Records are user-editable data,
/// so only the exact path shape this feature writes may resolve, symlinks are
/// never followed, and dangling references are skipped rather than failing
/// the fork (otherwise every /rewind in the target session would fail).
fn copy_referenced_checkpoints(
    checkpoint_files: &BTreeSet<String>,
    source_session_dir: &Path,
    target_dir: &Path,
    source_id: &acp::SessionId,
) -> io::Result<usize> {
    if checkpoint_files.is_empty() {
        return Ok(0);
    }
    // The per-file `symlink_metadata` below only vets the final path
    // component, so the intermediate `compaction_checkpoints` dir must itself
    // be a real directory; a symlinked dir would resolve every matching name
    // outside the session.
    match std::fs::symlink_metadata(source_session_dir.join("compaction_checkpoints")) {
        Ok(meta) if meta.file_type().is_dir() => {}
        Ok(meta) => {
            tracing::warn!(
                file_type = ?meta.file_type(),
                session_id = %source_id,
                "compaction_checkpoints is not a real directory; skipping checkpoint copy",
            );
            return Ok(0);
        }
        // Dir gone means every record is dangling; same policy as a missing
        // checkpoint file.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tracing::warn!(
                session_id = %source_id,
                "compaction_checkpoints directory missing; skipping checkpoint copy",
            );
            return Ok(0);
        }
        Err(error) => return Err(error),
    }
    let mut copied = 0usize;
    for checkpoint_file in checkpoint_files {
        let relative = Path::new(checkpoint_file);
        // A doctored record path must not address other session files (e.g.
        // the fork's rewritten updates.jsonl).
        let well_formed = relative.parent() == Some(Path::new("compaction_checkpoints"))
            && relative.extension() == Some("json".as_ref());
        if !well_formed {
            tracing::warn!(
                checkpoint_file = %checkpoint_file,
                session_id = %source_id,
                "skipping compaction checkpoint with unexpected path during copy",
            );
            continue;
        }
        let src = source_session_dir.join(relative);
        match std::fs::symlink_metadata(&src) {
            Ok(meta) if meta.file_type().is_file() => {}
            Ok(meta) => {
                // This feature only ever writes regular files, so don't
                // follow symlinks planted in the source session.
                tracing::warn!(
                    path = %src.display(),
                    file_type = ?meta.file_type(),
                    session_id = %source_id,
                    "compaction checkpoint source is not a regular file; skipping copy",
                );
                continue;
            }
            // Already-dangling record (e.g. a chained fork of a broken
            // session): the copy can't invent the file, so don't fail.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                tracing::warn!(
                    path = %src.display(),
                    session_id = %source_id,
                    "compaction checkpoint file missing from source; skipping copy",
                );
                continue;
            }
            Err(error) => return Err(error),
        }
        let dst = target_dir.join(relative);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dst)?;
        copied += 1;
    }
    Ok(copied)
}

fn invalid_data(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
