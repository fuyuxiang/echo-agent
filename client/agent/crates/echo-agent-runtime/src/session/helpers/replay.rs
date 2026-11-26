//! Replay pipeline for cross-compaction rewind.
//!
//! When rewinding to a prompt that precedes a compaction boundary, the in-memory
//! conversation (and `chat_history.jsonl`) no longer contains the original
//! messages. This module reconstructs the conversation by streaming
//! `updates.jsonl` and handling `CompactionCheckpoint` / `RewindMarker` entries.

use std::io;
use std::path::Path;

use crate::extensions::notification::{
    CompactionCheckpointFile, CompactionCheckpointInfo, SessionUpdate as EchoAgentSessionUpdate,
};
use crate::sampling::{AssistantItem, ContentPart, ConversationItem, ToolCall};
use crate::session::storage::{SessionUpdate, UpdatesIterator};
use std::collections::{HashMap, HashSet};

/// Result of replaying updates.jsonl up to a target prompt index.
#[derive(Debug)]
pub struct ReplayResult {
    /// The reconstructed conversation, suitable for replacing in-memory state.
    pub conversation: Vec<ConversationItem>,
    /// The prompt index that was reached (should equal the target).
    pub prompt_index_reached: usize,
    /// The original User(user_info) text from before the first compaction.
    /// Extracted from the checkpoint file's `original_user_info` field.
    /// `None` if no checkpoint was encountered or the checkpoint predates
    /// the field (schema_version 1 without it).
    pub original_user_info: Option<String>,
    /// Compaction marker for the rebuilt conversation: `Some(idx)` if a summary survives, else `None`.
    pub last_compaction_prompt_index: Option<usize>,
}

/// Find the most recent `CompactionCheckpoint` in `updates.jsonl`.
///
/// Uses raw-line peeking: only lines containing `"compaction_checkpoint"`
/// are parsed, skipping full typed deserialization.
pub fn find_latest_compaction_checkpoint(
    updates_path: &Path,
) -> io::Result<Option<CompactionCheckpointInfo>> {
    use crate::session::storage::RawLinePeek;

    let raw_contents = match std::fs::read_to_string(updates_path) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    if !raw_contents.contains("compaction_checkpoint") {
        return Ok(None);
    }

    let mut latest: Option<CompactionCheckpointInfo> = None;

    for line in raw_contents.lines() {
        if line.trim().is_empty() || !line.contains("compaction_checkpoint") {
            continue;
        }

        let Ok(env) = serde_json::from_str::<RawLinePeek<'_>>(line) else {
            continue;
        };

        if env.method != Some("_echo.agent/session/update") {
            continue;
        }

        let Some(raw_params) = env.params else {
            continue;
        };

        if let Ok(notification) = serde_json::from_str::<
            crate::extensions::notification::SessionNotification,
        >(raw_params.get())
            && let EchoAgentSessionUpdate::CompactionCheckpoint(info) = notification.update
        {
            latest = Some(*info);
        }
    }

    Ok(latest)
}

/// Replay `updates.jsonl` to reconstruct the conversation at `target_prompt_index`.
///
/// This handles:
/// - `RewindMarker`: discards accumulated state beyond the marker's target.
/// - `CompactionCheckpoint`: loads/ignores checkpoints based on whether the
///   target is before or after the compaction boundary.
///
/// `session_dir` is the path to the session directory (for reading checkpoint files).
pub fn replay_to_prompt(
    updates_path: &Path,
    session_dir: &Path,
    target_prompt_index: usize,
) -> io::Result<ReplayResult> {
    replay_to_prompt_impl(updates_path, session_dir, target_prompt_index, false)
}

/// Continuation snapshots retain tool calls/results and inline images as model
/// context, in addition to the text used by the existing rewind path.
pub fn replay_to_prompt_with_context(
    updates_path: &Path,
    session_dir: &Path,
    target_prompt_index: usize,
) -> io::Result<ReplayResult> {
    replay_to_prompt_impl(updates_path, session_dir, target_prompt_index, true)
}

fn replay_to_prompt_impl(
    updates_path: &Path,
    session_dir: &Path,
    target_prompt_index: usize,
    preserve_context: bool,
) -> io::Result<ReplayResult> {
    let Some(iter) = UpdatesIterator::open(updates_path)? else {
        return Ok(ReplayResult {
            conversation: vec![],
            prompt_index_reached: 0,
            original_user_info: None,
            last_compaction_prompt_index: None,
        });
    };

    let mut state = ReplayState::new(target_prompt_index);
    if preserve_context {
        state.context = Some(ReplayContext::default());
    }

    for update_result in iter {
        let update = match update_result {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(?e, "Skipping malformed update during replay");
                continue;
            }
        };

        let action = state.process_update(&update, session_dir)?;
        if action == ReplayAction::Stop {
            break;
        }
    }

    // Flush any trailing partial messages.
    state.flush_pending_user();
    state.flush_pending_agent();

    // After processing the entire file, the conversation may extend beyond
    // the target. `target_prompt_index` means "rewind to before prompt N",
    // so keep prompts 0..N-1 (N prompts total).
    if state.prompt_counter > target_prompt_index {
        if state.checkpoint_active && target_prompt_index >= state.checkpoint_prompt_index {
            let turns_to_keep = target_prompt_index - state.checkpoint_prompt_index;
            let mut seen_marker = false;
            let mut user_count = 0;
            let mut cut_pos = state.conversation.len();
            for (i, item) in state.conversation[state.checkpoint_base_len..]
                .iter()
                .enumerate()
            {
                if counts_as_replay_turn_progressive(item, &mut seen_marker) {
                    user_count += 1;
                    if user_count > turns_to_keep {
                        cut_pos = state.checkpoint_base_len + i;
                        break;
                    }
                }
            }
            state.conversation.truncate(cut_pos);
        } else if target_prompt_index == 0 {
            state.conversation.clear();
        } else {
            let truncate_at = state.truncate_target(target_prompt_index);
            let keep =
                crate::sampling::conversation_truncate_for_prompt(&state.conversation, truncate_at);
            state.conversation.truncate(keep);
        }
        state.prompt_counter = target_prompt_index;
    }

    if preserve_context {
        crate::sampling::repair_dangling_tool_calls(
            &mut state.conversation,
            crate::sampling::DanglingToolCallReason::HarnessHalted {
                class: "incomplete_history",
            },
        );
        crate::session::storage::jsonl::strip_invalid_images(&mut state.conversation);
    }
    Ok(ReplayResult {
        conversation: state.conversation,
        prompt_index_reached: state.prompt_counter,
        original_user_info: state.original_user_info,
        last_compaction_prompt_index: state
            .checkpoint_active
            .then_some(state.checkpoint_prompt_index),
    })
}

#[derive(Debug, PartialEq)]
enum ReplayAction {
    Continue,
    Stop,
}

/// Internal state machine for the replay pipeline.
struct ReplayState {
    /// The prompt index we're trying to reach.
    target: usize,

    /// Accumulated conversation items.
    conversation: Vec<ConversationItem>,

    /// Current prompt counter (how many user turns we've seen).
    prompt_counter: usize,

    /// Whether we're inside a contiguous sequence of UserMessageChunk updates
    /// (used to count user turns correctly — multiple chunks = one turn).
    in_user_message: bool,

    /// Partial text accumulator for the current user message.
    current_user_text: String,

    current_user_prompt_index: Option<usize>,

    /// True once any user chunk with `_meta.promptIndex` has been seen.
    /// Unnumbered user runs after that are mid-turn phantoms (not turns).
    seen_prompt_index_marker: bool,

    /// Partial text accumulator for the current agent message.
    current_agent_text: String,

    /// Whether there's a pending agent message to flush.
    has_pending_agent: bool,

    /// When set, the replay is operating "after" a loaded checkpoint.
    /// In this mode, only real `UserMessageChunk` turns from updates.jsonl
    /// are counted — the User messages inside the compacted history are ignored.
    checkpoint_active: bool,

    /// The conversation length right after loading a checkpoint. Items before
    /// this index are the opaque compacted history blob and must NOT be
    /// truncated or counted for prompt indexing.
    checkpoint_base_len: usize,

    /// The `prompt_index_at_compaction` from the loaded checkpoint.
    checkpoint_prompt_index: usize,

    /// The original User(user_info) text from before the first compaction.
    original_user_info: Option<String>,
    context: Option<ReplayContext>,
}

#[derive(Default)]
struct ReplayContext {
    user_parts: Vec<ContentPart>,
    tool_calls: Vec<ToolCall>,
    known_tools: HashMap<String, ToolCall>,
    completed_tools: HashSet<String>,
}

fn image_part(image: &agent_client_protocol::ImageContent) -> Option<ContentPart> {
    let url = if !image.data.is_empty() {
        format!("data:{};base64,{}", image.mime_type, image.data)
    } else {
        image.uri.clone()?
    };
    Some(ContentPart::Image { url: url.into() })
}

impl ReplayState {
    fn new(target: usize) -> Self {
        Self {
            target,
            conversation: Vec::new(),
            prompt_counter: 0,
            in_user_message: false,
            current_user_text: String::new(),
            current_user_prompt_index: None,
            seen_prompt_index_marker: false,
            current_agent_text: String::new(),
            has_pending_agent: false,
            checkpoint_active: false,
            checkpoint_base_len: 0,
            checkpoint_prompt_index: 0,
            original_user_info: None,
            context: None,
        }
    }

    fn process_update(
        &mut self,
        update: &SessionUpdate,
        session_dir: &Path,
    ) -> io::Result<ReplayAction> {
        match update {
            SessionUpdate::EchoAgent(notification) => {
                match &notification.update {
                    EchoAgentSessionUpdate::CompactionCheckpoint(info) => {
                        return self.handle_checkpoint(info, session_dir);
                    }
                    EchoAgentSessionUpdate::RewindMarker {
                        target_prompt_index,
                        ..
                    } => {
                        self.handle_rewind_marker(*target_prompt_index);
                        return Ok(ReplayAction::Continue);
                    }
                    // Other EchoAgent notifications are informational — skip them.
                    _ => {}
                }
            }
            SessionUpdate::Acp(notification) => match &notification.update {
                agent_client_protocol::SessionUpdate::UserMessageChunk(chunk) => {
                    return Ok(self.handle_user_chunk(chunk));
                }
                agent_client_protocol::SessionUpdate::AgentMessageChunk(chunk) => {
                    self.handle_agent_chunk(chunk);
                }
                agent_client_protocol::SessionUpdate::ToolCall(call) if self.context.is_some() => {
                    let name = notification
                        .meta
                        .as_ref()
                        .and_then(|meta| meta.get("echo.agent/tool"))
                        .and_then(|identity| identity.get("name"))
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| {
                            call.meta
                                .as_ref()
                                .and_then(|meta| meta.get("echo.agent/tool"))
                                .and_then(|identity| identity.get("name"))
                                .and_then(serde_json::Value::as_str)
                        })
                        .unwrap_or_else(|| {
                            // Older logs only store a human title. It must not
                            // become an invalid provider function name.
                            if !call.title.is_empty()
                                && call.title.len() <= 64
                                && call
                                    .title
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                            {
                                &call.title
                            } else {
                                "historical_tool"
                            }
                        })
                        .to_string();
                    self.handle_tool_call(call, name);
                }
                agent_client_protocol::SessionUpdate::ToolCallUpdate(call)
                    if self.context.is_some() =>
                {
                    self.handle_tool_update(call);
                }
                _ => {}
            },
        }
        Ok(ReplayAction::Continue)
    }

    fn handle_checkpoint(
        &mut self,
        info: &CompactionCheckpointInfo,
        session_dir: &Path,
    ) -> io::Result<ReplayAction> {
        if self.target < info.prompt_index_at_compaction {
            // Target is before this compaction — don't load the compacted
            // history (we'll reconstruct from raw updates). But the
            // checkpoint is still required for original_user_info — the
            // historical User(user_info) that the model saw for these
            // pre-compaction turns. Without it we'd use the post-compaction
            // rebuilt user_info, which is wrong data.
            let checkpoint_path = session_dir.join(&info.checkpoint_file);
            let bytes = match std::fs::read(&checkpoint_path) {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    tracing::error!(
                        path = %checkpoint_path.display(),
                        "Compaction checkpoint file missing, cannot restore original user_info"
                    );
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "Compaction checkpoint file missing: {}. \
                             Cannot safely rewind past the compaction point.",
                            checkpoint_path.display()
                        ),
                    ));
                }
                Err(e) => return Err(e),
            };
            match serde_json::from_slice::<CompactionCheckpointFile>(&bytes) {
                Ok(file) => {
                    if self.original_user_info.is_none() {
                        self.original_user_info = file.original_user_info;
                    }
                }
                Err(e) => {
                    tracing::error!(
                        ?e,
                        path = %checkpoint_path.display(),
                        "Compaction checkpoint file corrupt, cannot restore original user_info"
                    );
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Compaction checkpoint file corrupt: {}. \
                             Cannot safely rewind past the compaction point.",
                            checkpoint_path.display()
                        ),
                    ));
                }
            }
            tracing::debug!(
                target = self.target,
                checkpoint_at = info.prompt_index_at_compaction,
                "Replay: using raw updates (target is pre-compaction), original_user_info extracted"
            );
            Ok(ReplayAction::Continue)
        } else {
            let checkpoint_path = session_dir.join(&info.checkpoint_file);
            let bytes = match std::fs::read(&checkpoint_path) {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    tracing::error!(
                        path = %checkpoint_path.display(),
                        "Compaction checkpoint file missing, cannot reconstruct conversation"
                    );
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "Compaction checkpoint file missing: {}. \
                             Cannot safely rewind past the compaction point.",
                            checkpoint_path.display()
                        ),
                    ));
                }
                Err(e) => return Err(e),
            };
            let file: CompactionCheckpointFile = match serde_json::from_slice(&bytes) {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!(
                        ?e,
                        path = %checkpoint_path.display(),
                        "Compaction checkpoint file corrupt, cannot reconstruct conversation"
                    );
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Compaction checkpoint file corrupt: {}. \
                             Cannot safely rewind past the compaction point.",
                            checkpoint_path.display()
                        ),
                    ));
                }
            };

            if file.schema_version > 1 {
                tracing::error!(
                    schema_version = file.schema_version,
                    path = %checkpoint_path.display(),
                    "Unsupported checkpoint schema version, cannot reconstruct conversation"
                );
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Unsupported checkpoint schema version {}. \
                         Cannot safely rewind past the compaction point.",
                        file.schema_version
                    ),
                ));
            }

            // Capture original_user_info from the checkpoint (even if we
            // replace the conversation — it's needed by handle_rewind for
            // the raw-updates prefix case).
            if self.original_user_info.is_none() {
                self.original_user_info = file.original_user_info.clone();
            }

            // Replace accumulated conversation with the compacted history.
            self.conversation = file.compacted_history;
            // Checkpoints predate this binary's validation (or the API's
            // current validators), so heal them like the jsonl loader does
            // — otherwise a cross-compaction rewind re-injects a stripped
            // poison image and every turn 400s until the next restart.
            let stripped_images =
                crate::session::storage::jsonl::strip_invalid_images(&mut self.conversation);
            if stripped_images > 0 {
                tracing::warn!(
                    count = stripped_images,
                    "stripped invalid images from compaction checkpoint history"
                );
            }
            self.prompt_counter = info.prompt_index_at_compaction;
            self.checkpoint_active = true;
            self.checkpoint_base_len = self.conversation.len();
            self.checkpoint_prompt_index = info.prompt_index_at_compaction;

            // Flush any in-progress message state.
            self.in_user_message = false;
            self.current_user_text.clear();
            self.current_user_prompt_index = None;
            self.current_agent_text.clear();
            self.has_pending_agent = false;
            if let Some(context) = &mut self.context {
                *context = ReplayContext::default();
            }

            tracing::debug!(
                prompt_counter = self.prompt_counter,
                "Replay: loaded compaction checkpoint"
            );

            // If the auto-continue prompt was recorded, add it as a user turn.
            if let Some(ref ac) = info.auto_continue {
                self.conversation
                    .push(ConversationItem::user(ac.prompt_text.clone()));
                // The auto-continue prompt counts as a user turn.
                // prompt_counter already equals prompt_index_at_compaction.
            }

            if self.prompt_counter > self.target {
                // The checkpoint itself is past our target — this shouldn't
                // normally happen but we handle it gracefully.
                Ok(ReplayAction::Stop)
            } else {
                Ok(ReplayAction::Continue)
            }
        }
    }

    fn handle_rewind_marker(&mut self, marker_target: usize) {
        // Discard any in-progress partial messages — they belong to the
        // timeline being discarded, so we drop them rather than flushing.
        self.current_user_text.clear();
        self.current_user_prompt_index = None;
        self.current_agent_text.clear();
        self.has_pending_agent = false;
        self.in_user_message = false;
        if let Some(context) = &mut self.context {
            *context = ReplayContext::default();
        }

        if self.prompt_counter <= marker_target {
            return;
        }

        // `marker_target = N` means "rewind to before prompt N", keeping
        // prompts 0..N-1 (N prompts total).
        if self.checkpoint_active && marker_target >= self.checkpoint_prompt_index {
            // Post-checkpoint truncation: keep the compacted history blob intact
            // and only discard real user turns appended after it.
            let turns_to_keep = marker_target - self.checkpoint_prompt_index;
            let mut seen_marker = false;
            let mut user_count = 0;
            let mut cut_pos = self.conversation.len();
            for (i, item) in self.conversation[self.checkpoint_base_len..]
                .iter()
                .enumerate()
            {
                if counts_as_replay_turn_progressive(item, &mut seen_marker) {
                    user_count += 1;
                    if user_count > turns_to_keep {
                        cut_pos = self.checkpoint_base_len + i;
                        break;
                    }
                }
            }
            self.conversation.truncate(cut_pos);
        } else if marker_target == 0 {
            // Rewind to the very beginning — discard everything.
            self.conversation.clear();
            self.checkpoint_active = false;
        } else if self.checkpoint_active {
            // Marker target is before the checkpoint — discard the checkpoint
            // entirely and truncate the pre-checkpoint conversation.
            self.checkpoint_active = false;
            let truncate_at = self.truncate_target(marker_target);
            let keep =
                crate::sampling::conversation_truncate_for_prompt(&self.conversation, truncate_at);
            self.conversation.truncate(keep);
        } else {
            let truncate_at = self.truncate_target(marker_target);
            let keep =
                crate::sampling::conversation_truncate_for_prompt(&self.conversation, truncate_at);
            self.conversation.truncate(keep);
        }

        self.prompt_counter = marker_target;
    }

    fn handle_user_chunk(&mut self, chunk: &agent_client_protocol::ContentChunk) -> ReplayAction {
        if crate::session::storage::is_host_turn_chunk(chunk) {
            self.flush_host_turn_boundary();
            return ReplayAction::Continue;
        }
        let chunk_prompt_index = chunk
            .meta
            .as_ref()
            .and_then(|m| m.get("promptIndex"))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        if chunk_prompt_index.is_some() {
            self.seen_prompt_index_marker = true;
        }

        if !self.in_user_message {
            self.flush_pending_agent();
            self.begin_context_turn(chunk_prompt_index);
            self.in_user_message = true;
            self.current_user_text.clear();
            self.current_user_prompt_index = chunk_prompt_index;
        } else if chunk_prompt_index != self.current_user_prompt_index
            && (chunk_prompt_index.is_some() || self.current_user_prompt_index.is_some())
        {
            // New run: promptIndex changed, or transition between marked/unmarked.
            self.flush_pending_user();
            self.begin_context_turn(chunk_prompt_index);
            self.in_user_message = true;
            self.current_user_text.clear();
            self.current_user_prompt_index = chunk_prompt_index;
        } else if self.current_user_prompt_index.is_none() {
            self.current_user_prompt_index = chunk_prompt_index;
        }

        if let agent_client_protocol::ContentBlock::Text(t) = &chunk.content {
            self.current_user_text.push_str(&t.text);
        }

        if let Some(context) = &mut self.context {
            match &chunk.content {
                agent_client_protocol::ContentBlock::Text(text) => {
                    context.user_parts.push(ContentPart::Text {
                        text: text.text.clone().into(),
                    })
                }
                agent_client_protocol::ContentBlock::Image(image) => {
                    if let Some(part) = image_part(image) {
                        context.user_parts.push(part);
                    }
                }
                _ => {}
            }
        }

        // Note: we do NOT early-stop when prompt_counter > target because a
        // later RewindMarker could reset the counter back below the target.
        // The replay processes the entire file and the final conversation
        // state is correct regardless of timeline branches.
        ReplayAction::Continue
    }

    fn handle_agent_chunk(&mut self, chunk: &agent_client_protocol::ContentChunk) {
        if crate::session::storage::is_host_turn_chunk(chunk) {
            self.flush_host_turn_boundary();
            return;
        }

        // An agent chunk ends any in-progress user message.
        if self.in_user_message {
            self.flush_pending_user();
            self.in_user_message = false;
        }

        if let agent_client_protocol::ContentBlock::Text(t) = &chunk.content {
            self.current_agent_text.push_str(&t.text);
            self.has_pending_agent = true;
        }
    }

    fn begin_context_turn(&mut self, prompt_index: Option<usize>) {
        // Provider tool ids can be reused in a later turn. Unnumbered host
        // inserts inside a numbered turn do not start a new identity scope.
        if (prompt_index.is_some() || !self.seen_prompt_index_marker)
            && let Some(context) = &mut self.context
        {
            context.known_tools.clear();
            context.completed_tools.clear();
        }
    }

    fn flush_host_turn_boundary(&mut self) {
        if self.in_user_message {
            self.flush_pending_user();
            self.in_user_message = false;
        }
        self.flush_pending_agent();
    }

    fn conversation_has_markers(&self) -> bool {
        self.seen_prompt_index_marker
            || self
                .conversation
                .iter()
                .any(|i| matches!(i, ConversationItem::User(u) if u.prompt_index.is_some()))
    }

    /// Absolute `target` when items carry `prompt_index`; else `target - 1`
    /// for the preamble-aware counting fallback.
    fn truncate_target(&self, target: usize) -> usize {
        if self.conversation_has_markers() {
            target
        } else {
            target.saturating_sub(1)
        }
    }

    fn flush_pending_user(&mut self) {
        let parts = self
            .context
            .as_mut()
            .map(|context| std::mem::take(&mut context.user_parts));
        if self.current_user_text.is_empty() && parts.as_ref().is_none_or(Vec::is_empty) {
            self.current_user_prompt_index = None;
            return;
        }
        let text = std::mem::take(&mut self.current_user_text);
        let pi = self.current_user_prompt_index.take();
        let mut item = match parts {
            Some(parts) => ConversationItem::user_with_parts(parts),
            None => ConversationItem::user(text),
        };
        if let Some(pi) = pi {
            item.set_prompt_index(pi);
            self.prompt_counter += 1;
        } else if !self.seen_prompt_index_marker {
            self.prompt_counter += 1;
        }
        self.conversation.push(item);
    }

    fn flush_pending_agent(&mut self) {
        let tool_calls = self
            .context
            .as_mut()
            .map(|context| std::mem::take(&mut context.tool_calls))
            .unwrap_or_default();
        if self.has_pending_agent || !tool_calls.is_empty() {
            self.conversation
                .push(ConversationItem::Assistant(AssistantItem {
                    content: std::mem::take(&mut self.current_agent_text).into(),
                    tool_calls,
                    model_id: None,
                    model_fingerprint: None,
                    reasoning_effort: None,
                }));
            self.has_pending_agent = false;
        }
    }

    fn handle_tool_call(&mut self, call: &agent_client_protocol::ToolCall, name: String) {
        if self.in_user_message {
            self.flush_pending_user();
            self.in_user_message = false;
        }
        let id = call.tool_call_id.0.to_string();
        let context = self.context.as_mut().unwrap();
        if !context.known_tools.contains_key(&id) {
            let tool = ToolCall {
                id: id.clone().into(),
                name,
                arguments: call
                    .raw_input
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "{}".into())
                    .into(),
            };
            context.known_tools.insert(id.clone(), tool.clone());
            context.tool_calls.push(tool);
        }
        self.finish_tool(
            &id,
            Some(call.status),
            Some(&call.content),
            call.raw_output.as_ref(),
        );
    }

    fn handle_tool_update(&mut self, call: &agent_client_protocol::ToolCallUpdate) {
        let id = call.tool_call_id.0.to_string();
        if !self.context.as_ref().unwrap().known_tools.contains_key(&id) {
            return;
        }
        if let Some(raw_input) = &call.fields.raw_input {
            let arguments: std::sync::Arc<str> = raw_input.to_string().into();
            let context = self.context.as_mut().unwrap();
            if let Some(tool) = context.known_tools.get_mut(&id) {
                tool.arguments = arguments.clone();
            }
            let mut pending = false;
            for tool in &mut context.tool_calls {
                if tool.id.as_ref() == id {
                    tool.arguments = arguments.clone();
                    pending = true;
                }
            }
            if !pending {
                // Only the current call may be enriched. A provider can reuse
                // the same id later without rewriting an earlier turn's input.
                for item in self.conversation.iter_mut().rev() {
                    if let ConversationItem::Assistant(assistant) = item {
                        if let Some(tool) = assistant
                            .tool_calls
                            .iter_mut()
                            .find(|tool| tool.id.as_ref() == id)
                        {
                            tool.arguments = arguments.clone();
                            break;
                        }
                    }
                }
            }
        }
        self.finish_tool(
            &id,
            call.fields.status,
            call.fields.content.as_deref(),
            call.fields.raw_output.as_ref(),
        );
    }

    fn finish_tool(
        &mut self,
        id: &str,
        status: Option<agent_client_protocol::ToolCallStatus>,
        content: Option<&[agent_client_protocol::ToolCallContent]>,
        raw: Option<&serde_json::Value>,
    ) {
        let already_completed = self.context.as_ref().unwrap().completed_tools.contains(id);
        if !matches!(
            status,
            Some(
                agent_client_protocol::ToolCallStatus::Completed
                    | agent_client_protocol::ToolCallStatus::Failed
            )
        ) && !(status.is_none() && already_completed)
        {
            return;
        }
        let context = self.context.as_mut().unwrap();
        if !context.known_tools.contains_key(id) {
            return;
        }
        let first_result = context.completed_tools.insert(id.into());
        // Emit pending calls before their results, keeping parallel results
        // adjacent even if commentary arrived while another call was pending.
        if first_result && !context.tool_calls.is_empty() {
            self.flush_pending_agent();
        }
        let mut text = String::new();
        let mut images = Vec::new();
        for block in content.unwrap_or_default() {
            if let agent_client_protocol::ToolCallContent::Content(value) = block {
                match &value.content {
                    agent_client_protocol::ContentBlock::Text(value) => text.push_str(&value.text),
                    agent_client_protocol::ContentBlock::Image(value) => {
                        if let Some(part) = image_part(value) {
                            images.push(part);
                        }
                    }
                    _ => {}
                }
            }
        }
        let has_text_update = raw.is_some() || !text.is_empty();
        let text = raw.map(ToString::to_string).unwrap_or(text);
        if !first_result {
            if let Some(ConversationItem::ToolResult(previous)) = self.conversation.iter_mut().rev()
                .find(|item| matches!(item, ConversationItem::ToolResult(result) if result.tool_call_id == id)) {
                if has_text_update { previous.content = text.into(); }
                if !images.is_empty() { previous.images = images; }
            }
            return;
        }
        self.conversation
            .push(ConversationItem::tool_result_with_images(id, text, images));
    }
}

/// Progressive post-checkpoint turn: unmarked users count until the first
/// marker in the slice; after that only marked users count.
fn counts_as_replay_turn_progressive(item: &ConversationItem, seen_marker: &mut bool) -> bool {
    let ConversationItem::User(u) = item else {
        return false;
    };
    if u.prompt_index.is_some() {
        *seen_marker = true;
        true
    } else {
        !*seen_marker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::notification::{
        AutoContinueInfo, CompactionCheckpointFile, CompactionCheckpointInfo,
        SessionNotification as EchoAgentNotification, SessionUpdate as EchoAgentSessionUpdate,
    };
    use agent_client_protocol as acp;
    use tempfile::TempDir;

    fn make_user_update(session_id: &str, text: &str) -> SessionUpdate {
        SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
            acp::SessionId::new(session_id),
            acp::SessionUpdate::UserMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new(text.to_string()),
            ))),
        )))
    }

    fn make_user_update_pi(session_id: &str, text: &str, prompt_index: usize) -> SessionUpdate {
        SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
            acp::SessionId::new(session_id),
            acp::SessionUpdate::UserMessageChunk(
                acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
                    text.to_string(),
                )))
                .meta(
                    serde_json::json!({ "promptIndex": prompt_index })
                        .as_object()
                        .cloned(),
                ),
            ),
        )))
    }

    #[test]
    fn test_replay_consecutive_prompts_no_agent_between_are_distinct_turns() {
        let tmp = TempDir::new().unwrap();
        let updates = vec![
            make_user_update_pi("s1", "P0", 0),
            make_user_update_pi("s1", "P1", 1),
            make_user_update_pi("s1", "P2", 2),
            make_user_update_pi("s1", "P3", 3),
            make_user_update_pi("s1", "P4", 4),
            make_user_update_pi("s1", "P5", 5),
        ];
        let result = replay_updates(&updates, tmp.path(), 3);
        let user_msgs: Vec<String> = result
            .conversation
            .iter()
            .filter(|c| matches!(c, ConversationItem::User(_)))
            .map(|c| c.text_content())
            .collect();
        assert_eq!(
            user_msgs,
            vec!["P0", "P1", "P2"],
            "consecutive cancelled-turn prompts must be distinct turns and truncate correctly"
        );
        assert_eq!(result.prompt_index_reached, 3);
    }

    fn make_agent_update(session_id: &str, text: &str) -> SessionUpdate {
        SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
            acp::SessionId::new(session_id),
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new(text.to_string()),
            ))),
        )))
    }

    fn make_host_turn_update(session_id: &str, text: &str, user: bool) -> SessionUpdate {
        let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
            text.to_string(),
        )))
        .meta(serde_json::json!({ "hostTurn": true }).as_object().cloned());
        let update = if user {
            acp::SessionUpdate::UserMessageChunk(chunk)
        } else {
            acp::SessionUpdate::AgentMessageChunk(chunk)
        };
        SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
            acp::SessionId::new(session_id),
            update,
        )))
    }

    #[test]
    fn test_replay_suppresses_full_host_turn_and_flushes_preceding_agent() {
        let tmp = TempDir::new().unwrap();
        let updates = vec![
            make_user_update_pi("s1", "real user", 0),
            make_agent_update("s1", "real assistant"),
            make_host_turn_update("s1", "/workflows", true),
            make_host_turn_update("s1", "host-only slash output", false),
            make_user_update_pi("s1", "next real user", 1),
            make_agent_update("s1", "next real assistant"),
        ];

        let result = replay_updates(&updates, tmp.path(), 2);
        let texts: Vec<_> = result
            .conversation
            .iter()
            .map(ConversationItem::text_content)
            .collect();
        assert_eq!(
            texts,
            vec![
                "real user",
                "real assistant",
                "next real user",
                "next real assistant"
            ]
        );
        assert_eq!(result.prompt_index_reached, 2);
    }

    fn make_rewind_marker(target: usize) -> SessionUpdate {
        SessionUpdate::EchoAgent(Box::new(EchoAgentNotification {
            session_id: acp::SessionId::new("test"),
            update: EchoAgentSessionUpdate::RewindMarker {
                target_prompt_index: target,
                created_at: "2024-01-01T00:00:00Z".to_string(),
            },
            meta: None,
        }))
    }

    fn make_checkpoint(
        checkpoint_id: &str,
        prompt_index_at_compaction: usize,
        auto_continue: Option<AutoContinueInfo>,
    ) -> SessionUpdate {
        SessionUpdate::EchoAgent(Box::new(EchoAgentNotification {
            session_id: acp::SessionId::new("test"),
            update: EchoAgentSessionUpdate::CompactionCheckpoint(Box::new(
                CompactionCheckpointInfo {
                    checkpoint_id: checkpoint_id.to_string(),
                    prompt_index_at_compaction,
                    checkpoint_file: format!("compaction_checkpoints/{checkpoint_id}.json"),
                    auto_continue,
                    schema_version: 1,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                },
            )),
            meta: None,
        }))
    }

    fn write_checkpoint_file(
        session_dir: &Path,
        checkpoint_id: &str,
        prompt_index_at_compaction: usize,
        compacted_history: Vec<ConversationItem>,
    ) {
        let dir = session_dir.join("compaction_checkpoints");
        std::fs::create_dir_all(&dir).unwrap();
        let file = CompactionCheckpointFile {
            checkpoint_id: checkpoint_id.to_string(),
            prompt_index_at_compaction,
            compacted_history,
            schema_version: 1,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            original_user_info: None,
            reread_file_paths: vec![],
        };
        let bytes = serde_json::to_vec_pretty(&file).unwrap();
        std::fs::write(dir.join(format!("{checkpoint_id}.json")), bytes).unwrap();
    }

    /// Helper: write a sequence of updates to a JSONL file and replay to a target.
    fn replay_updates(
        updates: &[SessionUpdate],
        session_dir: &Path,
        target: usize,
    ) -> ReplayResult {
        let updates_path = session_dir.join("updates.jsonl");
        let mut content = Vec::new();
        for u in updates {
            let envelope = crate::session::storage::SessionUpdateEnvelope::from_update(u).unwrap();
            let mut line = serde_json::to_vec(&envelope).unwrap();
            line.push(b'\n');
            content.extend(line);
        }
        std::fs::write(&updates_path, content).unwrap();
        replay_to_prompt(&updates_path, session_dir, target).unwrap()
    }

    #[test]
    fn test_replay_unmarked_phantom_between_marked_prompts() {
        let tmp = TempDir::new().unwrap();
        let updates = vec![
            make_user_update_pi("s1", "P0", 0),
            make_agent_update("s1", "A0"),
            make_user_update("s1", "!pwd phantom"),
            make_agent_update("s1", "bash"),
            make_user_update_pi("s1", "P1", 1),
            make_agent_update("s1", "A1"),
            make_user_update_pi("s1", "P2", 2),
            make_agent_update("s1", "A2"),
        ];
        let result = replay_updates(&updates, tmp.path(), 2);
        let real: Vec<_> = result
            .conversation
            .iter()
            .filter_map(|c| match c {
                ConversationItem::User(u) if u.prompt_index.is_some() => Some(c.text_content()),
                _ => None,
            })
            .collect();
        assert_eq!(real, vec!["P0", "P1"]);
        assert!(
            result.conversation.iter().any(|c| {
                matches!(
                    c,
                    ConversationItem::User(u)
                        if u.prompt_index.is_none() && c.text_content().contains("pwd")
                )
            }),
            "phantom text kept for context"
        );
        assert_eq!(result.prompt_index_reached, 2);
    }

    #[test]
    fn test_replay_simple_no_compaction() {
        let tmp = TempDir::new().unwrap();
        let updates = vec![
            make_user_update("s1", "hello"),
            make_agent_update("s1", "hi there"),
            make_user_update("s1", "fix the bug"),
            make_agent_update("s1", "done"),
            make_user_update("s1", "add tests"),
            make_agent_update("s1", "tests added"),
        ];

        // Replay to prompt 1: keep prompts 0..0 = just "hello"
        let result = replay_updates(&updates, tmp.path(), 1);
        assert_eq!(result.prompt_index_reached, 1);
        assert_eq!(result.conversation.len(), 2);
        assert_eq!(result.conversation[0].text_content(), "hello");
        assert_eq!(result.conversation[1].text_content(), "hi there");
    }

    #[test]
    fn test_replay_with_rewind_marker() {
        let tmp = TempDir::new().unwrap();
        // P0, P1, P2, rewind(1) removes P1+P2, P1'
        let updates = vec![
            make_user_update("s1", "P0"),
            make_agent_update("s1", "R0"),
            make_user_update("s1", "P1"),
            make_agent_update("s1", "R1"),
            make_user_update("s1", "P2"),
            make_agent_update("s1", "R2"),
            make_rewind_marker(1), // keep P0 only
            make_user_update("s1", "P1_prime"),
            make_agent_update("s1", "R1_prime"),
        ];

        // Replay to prompt 2: keep prompts 0..1 = P0, P1'
        let result = replay_updates(&updates, tmp.path(), 2);
        // After rewind(1): P0 kept, P1+P2 discarded
        // P1_prime added as prompt 1 → [P0, R0, P1_prime, R1_prime]
        assert_eq!(result.conversation.len(), 4);
        let user_msgs: Vec<String> = result
            .conversation
            .iter()
            .filter(|c| matches!(c, ConversationItem::User(_)))
            .map(|c| c.text_content())
            .collect();
        assert_eq!(user_msgs, vec!["P0", "P1_prime"]);
    }

    #[test]
    fn test_replay_pre_compaction_target() {
        let tmp = TempDir::new().unwrap();

        // P0, P1, checkpoint(at=2), P2
        write_checkpoint_file(
            tmp.path(),
            "ckpt1",
            2,
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("compacted summary"),
            ],
        );

        let updates = vec![
            make_user_update("s1", "P0"),
            make_agent_update("s1", "R0"),
            make_user_update("s1", "P1"),
            make_agent_update("s1", "R1"),
            make_checkpoint("ckpt1", 2, None),
            make_user_update("s1", "P2"),
            make_agent_update("s1", "R2"),
        ];

        // Replay to prompt 1 (pre-compaction) — should IGNORE the checkpoint
        // Keep prompts 0..0 = just P0
        let result = replay_updates(&updates, tmp.path(), 1);
        let user_msgs: Vec<String> = result
            .conversation
            .iter()
            .filter(|c| matches!(c, ConversationItem::User(_)))
            .map(|c| c.text_content())
            .collect();
        assert_eq!(user_msgs, vec!["P0"]);
    }

    #[test]
    fn test_replay_post_compaction_target() {
        let tmp = TempDir::new().unwrap();

        // Checkpoint replaces conversation at prompt 2
        write_checkpoint_file(
            tmp.path(),
            "ckpt1",
            2,
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("compacted summary"),
            ],
        );

        let updates = vec![
            make_user_update("s1", "P0"),
            make_agent_update("s1", "R0"),
            make_user_update("s1", "P1"),
            make_agent_update("s1", "R1"),
            make_checkpoint("ckpt1", 2, None),
            make_user_update("s1", "P2"),
            make_agent_update("s1", "R2"),
            make_user_update("s1", "P3"),
            make_agent_update("s1", "R3"),
        ];

        // Replay to prompt 3 (post-compaction): keep prompts 0..2
        // Checkpoint blob + P2 only (P3 removed)
        let result = replay_updates(&updates, tmp.path(), 3);
        assert_eq!(result.conversation.len(), 4);
        assert_eq!(result.conversation[0].text_content(), "sys");
        assert_eq!(result.conversation[1].text_content(), "compacted summary");
        assert_eq!(result.conversation[2].text_content(), "P2");
        assert_eq!(result.conversation[3].text_content(), "R2");
        assert_eq!(result.prompt_index_reached, 3);
    }

    /// A checkpoint written before this binary's validation (or before the
    /// API tightened its validators) can carry an unsendable image; the
    /// splice must heal it like the jsonl loader does, or a cross-compaction
    /// rewind re-poisons a healed session.
    #[test]
    fn test_replay_checkpoint_strips_invalid_images() {
        use base64::Engine as _;
        let tmp = TempDir::new().unwrap();

        // 16×16 icon: below the API's 512-total-pixel floor.
        let mut png = Vec::new();
        image::ImageBuffer::from_pixel(16, 16, image::Rgba([9u8, 9, 9, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        let mut poisoned = ConversationItem::user("look at this icon");
        poisoned.add_image(url);

        write_checkpoint_file(
            tmp.path(),
            "ckpt1",
            1,
            vec![ConversationItem::system("sys"), poisoned],
        );

        let updates = vec![
            make_user_update("s1", "P0"),
            make_agent_update("s1", "R0"),
            make_checkpoint("ckpt1", 1, None),
            make_user_update("s1", "P1"),
            make_agent_update("s1", "R1"),
        ];

        let result = replay_updates(&updates, tmp.path(), 2);
        let ConversationItem::User(u) = &result.conversation[1] else {
            panic!("expected user item from checkpoint");
        };
        assert!(
            u.content.iter().all(|p| match p {
                crate::sampling::ContentPart::Image { url } => !url.starts_with("data:"),
                _ => true,
            }),
            "below-floor image must be stripped from the checkpoint splice"
        );
    }

    /// Auto-continue prompt is synthetic (not a real user prompt) so it must
    /// NOT increment prompt_counter. It's appended to the conversation for
    /// context but the next real prompt still gets the expected index.
    #[test]
    fn test_replay_checkpoint_with_auto_continue() {
        let tmp = TempDir::new().unwrap();

        write_checkpoint_file(
            tmp.path(),
            "ckpt1",
            2,
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("compacted"),
            ],
        );

        let updates = vec![
            make_user_update("s1", "P0"),
            make_agent_update("s1", "R0"),
            make_user_update("s1", "P1"),
            make_agent_update("s1", "R1"),
            make_checkpoint(
                "ckpt1",
                2,
                Some(AutoContinueInfo {
                    prompt_text: "Continue working".to_string(),
                }),
            ),
            make_user_update("s1", "P2"),
            make_agent_update("s1", "R2"),
        ];

        // Replay to prompt 3: keep prompts 0..2 = checkpoint blob + auto-continue + P2
        let result = replay_updates(&updates, tmp.path(), 3);
        // checkpoint sets counter to 2, auto-continue doesn't increment,
        // P2 increments to 3 → prompt_index_reached = 3
        assert_eq!(result.conversation.len(), 5);
        assert_eq!(result.conversation[0].text_content(), "sys");
        assert_eq!(result.conversation[1].text_content(), "compacted");
        assert_eq!(result.conversation[2].text_content(), "Continue working");
        assert_eq!(result.conversation[3].text_content(), "P2");
        assert_eq!(result.conversation[4].text_content(), "R2");
        assert_eq!(result.prompt_index_reached, 3);
    }

    #[test]
    fn test_find_latest_checkpoint_none() {
        let tmp = TempDir::new().unwrap();
        let updates_path = tmp.path().join("updates.jsonl");
        std::fs::write(&updates_path, "").unwrap();

        let result = find_latest_compaction_checkpoint(&updates_path).unwrap();
        assert!(result.is_none());
    }

    /// Scenario H: rewind marker after a loaded checkpoint.
    /// checkpoint(at=2), P2, P3, RewindMarker(2), P2'
    /// Replaying to prompt 2 should give checkpoint + P2' (not P2 or P3).
    #[test]
    fn test_replay_rewind_marker_after_checkpoint() {
        let tmp = TempDir::new().unwrap();

        write_checkpoint_file(
            tmp.path(),
            "ckpt1",
            2,
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("compacted summary"),
            ],
        );

        let updates = vec![
            make_user_update("s1", "P0"),
            make_agent_update("s1", "R0"),
            make_user_update("s1", "P1"),
            make_agent_update("s1", "R1"),
            make_checkpoint("ckpt1", 2, None),
            make_user_update("s1", "P2"),
            make_agent_update("s1", "R2"),
            make_user_update("s1", "P3"),
            make_agent_update("s1", "R3"),
            make_rewind_marker(2),
            make_user_update("s1", "P2_prime"),
            make_agent_update("s1", "R2_prime"),
        ];

        // Replay to prompt 3: keep 0..2 = checkpoint + P2' (after the rewind marker)
        let result = replay_updates(&updates, tmp.path(), 3);

        // The checkpoint blob has 2 items (system + user summary).
        // After the rewind marker discards P2+P3, P2' is added.
        // Result: [sys, summary, P2_prime, R2_prime]
        let user_msgs: Vec<String> = result
            .conversation
            .iter()
            .filter(|c| matches!(c, ConversationItem::User(_)))
            .map(|c| c.text_content())
            .collect();

        // The compacted summary is a synthetic User msg inside the checkpoint blob.
        // P2_prime is the real user msg appended after.
        assert!(
            user_msgs.contains(&"P2_prime".to_string()),
            "Should contain P2_prime, got: {:?}",
            user_msgs
        );
        assert!(
            !user_msgs.contains(&"P2".to_string()),
            "Should NOT contain old P2, got: {:?}",
            user_msgs
        );
        assert!(
            !user_msgs.contains(&"P3".to_string()),
            "Should NOT contain P3, got: {:?}",
            user_msgs
        );
    }

    /// Scenario E: multiple compactions, rewind to before the first.
    /// P0, P1 → checkpoint#1(at=2) → P2 → checkpoint#2(at=3) → P3
    /// Rewind to P1 should ignore both checkpoints.
    #[test]
    fn test_replay_multiple_compactions_rewind_to_before_first() {
        let tmp = TempDir::new().unwrap();

        write_checkpoint_file(
            tmp.path(),
            "ckpt1",
            2,
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("summary1"),
            ],
        );
        write_checkpoint_file(
            tmp.path(),
            "ckpt2",
            3,
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("summary2"),
            ],
        );

        let updates = vec![
            make_user_update("s1", "P0"),
            make_agent_update("s1", "R0"),
            make_user_update("s1", "P1"),
            make_agent_update("s1", "R1"),
            make_checkpoint("ckpt1", 2, None),
            make_user_update("s1", "P2"),
            make_agent_update("s1", "R2"),
            make_checkpoint("ckpt2", 3, None),
            make_user_update("s1", "P3"),
            make_agent_update("s1", "R3"),
        ];

        // Rewind to P1 — both checkpoints should be ignored
        // Keep prompts 0..0 = just P0
        let result = replay_updates(&updates, tmp.path(), 1);
        assert_eq!(result.conversation.len(), 2);
        let user_msgs: Vec<String> = result
            .conversation
            .iter()
            .filter(|c| matches!(c, ConversationItem::User(_)))
            .map(|c| c.text_content())
            .collect();
        assert_eq!(user_msgs, vec!["P0"]);
    }

    /// Scenario E variant: rewind to between two compactions.
    /// Should use checkpoint#1 and replay P2.
    #[test]
    fn test_replay_multiple_compactions_rewind_between() {
        let tmp = TempDir::new().unwrap();

        write_checkpoint_file(
            tmp.path(),
            "ckpt1",
            2,
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("summary1"),
            ],
        );
        write_checkpoint_file(
            tmp.path(),
            "ckpt2",
            3,
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("summary2"),
            ],
        );

        let updates = vec![
            make_user_update("s1", "P0"),
            make_agent_update("s1", "R0"),
            make_user_update("s1", "P1"),
            make_agent_update("s1", "R1"),
            make_checkpoint("ckpt1", 2, None),
            make_user_update("s1", "P2"),
            make_agent_update("s1", "R2"),
            make_checkpoint("ckpt2", 3, None),
            make_user_update("s1", "P3"),
            make_agent_update("s1", "R3"),
        ];

        // Replay to prompt 3: keep prompts 0..2 via ckpt1
        // ckpt1 loaded (target 3 >= 2), ckpt2 also loaded (target 3 >= 3).
        // ckpt2 replaces ckpt1. Then P3 is the first post-ckpt2 prompt.
        // Keep 3 - 3 = 0 post-ckpt2 prompts → just ckpt2 blob.
        let result = replay_updates(&updates, tmp.path(), 3);
        assert_eq!(result.conversation.len(), 2);
        assert_eq!(result.conversation[0].text_content(), "sys");
        assert_eq!(result.conversation[1].text_content(), "summary2");
    }
}
