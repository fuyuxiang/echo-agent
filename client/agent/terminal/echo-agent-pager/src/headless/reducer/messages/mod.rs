//! The `streaming-messages-json` reducer (Anthropic Messages API wire format).
//! The coordinator: owns [`MessagesReducer`] and its [`Reducer`] impl; cohesive
//! pieces live in the `wire`/`state`/`partial`/`usage`/`web_search` submodules.

use agent_client_protocol as acp;
use serde_json::{Value, json};

use super::{
    Lifecycle, Reducer, SessionContext, StreamEvent, ToolCallEvent, ToolCallUpdateEvent, TurnEnd,
    to_line,
};

mod partial;
mod state;
mod usage;
mod web_search;
mod wire;

#[cfg(test)]
mod tests;

use state::{
    PartialFraming, PendingResponse, ResponseIdentity, ResponseState, SessionState, TextKind,
};
use wire::{
    AssistantFrame, AssistantMessage, CompactBoundaryLine, CompactMetadata, ContentBlock,
    MessageUsage, MessagesLine, PartialDelta, ResultLine, SystemInitLine, SystemLine,
    ToolResultBlock, ToolResultLine, ToolResultMessage, messages_permission_mode, new_uuid,
};

/// `streaming-messages-json`: the Messages API wire format.
pub(crate) struct MessagesReducer {
    /// Session facts, populated by `begin`; `None` until then.
    session: Option<SessionState>,
    tools: Vec<String>,
    slash_commands: Vec<String>,
    /// Skill names for the Messages `init` `skills` field.
    skills: Vec<String>,
    init_emitted: bool,
    max_turns_hit: bool,
    blocks: Vec<ContentBlock>,
    open_kind: Option<TextKind>,
    open_text: String,
    msg_seq: u64,
    /// Assistant frames flushed this turn; gates the `result.result` final-text fallback.
    assistant_frames: u64,
    /// Completed responses this turn, including contentless ones; the `num_turns` fallback.
    completed_responses: u64,
    /// Current response lifecycle phase; dropped at response boundaries so it cannot leak.
    response: ResponseState,
    /// In-order signature for the currently-open thinking block, so each block keeps its own.
    open_signature: Option<String>,
    /// Terminal tool results buffered for one grouped `user` message, tagged with
    /// the `tool_use`'s emission order so the group flushes in `tool_use` order.
    pending_tool_results: Vec<(u64, ToolResultBlock)>,
    /// Monotonic order stamped on each `tool_use` so a later `tool_result` sorts back into place.
    next_tool_use_order: u64,
    /// Unmatched client `tool_use` blocks (id -> emission order); leftovers at turn
    /// end get an `is_error` `tool_result` to keep the transcript valid.
    pending_client_tool_uses: std::collections::HashMap<String, u64>,
    /// In-flight backend `web_search` calls (id -> order + call); query and results
    /// arrive only at completion, so the `ToolCall` defers here.
    backend_web_search_calls: std::collections::HashMap<String, (u64, ToolCallEvent)>,
    /// Count of successful inline backend `web_search` invocations (errored ones excluded, not billed).
    web_search_requests: u64,
    /// Text of the most recently flushed assistant frame (the `result.result` value).
    last_text: String,
    /// Typed partial-stream framing sub-state; only with `--include-partial-messages`.
    framing: PartialFraming,
    /// Monotonic counter for synthesized partial `message_start.id` placeholders.
    partial_msg_seq: u64,
}

impl MessagesReducer {
    pub(crate) fn new() -> Self {
        Self {
            session: None,
            tools: Vec::new(),
            slash_commands: Vec::new(),
            skills: Vec::new(),
            init_emitted: false,
            max_turns_hit: false,
            blocks: Vec::new(),
            open_kind: None,
            open_text: String::new(),
            msg_seq: 0,
            assistant_frames: 0,
            completed_responses: 0,
            response: ResponseState::Idle,
            open_signature: None,
            pending_tool_results: Vec::new(),
            next_tool_use_order: 0,
            pending_client_tool_uses: std::collections::HashMap::new(),
            backend_web_search_calls: std::collections::HashMap::new(),
            web_search_requests: 0,
            last_text: String::new(),
            framing: PartialFraming::Idle,
            partial_msg_seq: 0,
        }
    }

    /// The session id, or `""` before `begin` (the startup-error last resort).
    fn session_id(&self) -> &str {
        self.session.as_ref().map_or("", |s| s.session_id.as_str())
    }

    /// Whether `--include-partial-messages` framing is on; `false` before `begin`.
    fn include_partials(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.include_partials)
    }

    fn init_line(&self) -> Value {
        let session = self.session.as_ref();
        to_line(&MessagesLine::System(SystemLine::Init(SystemInitLine {
            session_id: self.session_id().to_string(),
            api_key_source: if session.is_none_or(|s| s.api_key_auth) {
                "user"
            } else {
                "oauth"
            },
            model: self.model_or_unknown(),
            cwd: session.map(|s| s.cwd.clone()).unwrap_or_default(),
            permission_mode: messages_permission_mode(
                session.and_then(|s| s.permission_mode.as_deref()),
            ),
            tools: self.tools.clone(),
            slash_commands: self.slash_commands.clone(),
            mcp_servers: session.map(|s| s.mcp_servers.clone()).unwrap_or_default(),
            skills: self.skills.clone(),
            uuid: new_uuid(),
        })))
    }

    fn ensure_init(&mut self) -> Option<Value> {
        if self.init_emitted {
            return None;
        }
        self.init_emitted = true;
        Some(self.init_line())
    }

    fn append_text(&mut self, kind: TextKind, text: &str) {
        // Finalize a differing or pending signature-only block so it keeps its position.
        if self.open_kind.is_some_and(|k| k != kind)
            || (self.open_kind.is_none() && self.open_signature.is_some())
        {
            self.finalize_open();
        }
        self.open_kind = Some(kind);
        self.open_text.push_str(text);
    }

    fn finalize_open(&mut self) {
        // Consume this block's own signature so it stamps onto THIS block, never a later one.
        let signature = self.open_signature.take();
        let Some(kind) = self.open_kind.take() else {
            if let Some(signature) = signature {
                self.blocks.push(ContentBlock::Thinking {
                    thinking: String::new(),
                    signature,
                });
            }
            return;
        };
        let text = std::mem::take(&mut self.open_text);
        match kind {
            TextKind::Text => {
                if !text.is_empty() {
                    self.blocks.push(ContentBlock::Text { text });
                }
            }
            TextKind::Thinking => {
                if !text.is_empty() || signature.is_some() {
                    self.blocks.push(ContentBlock::Thinking {
                        thinking: text,
                        signature: signature.unwrap_or_default(),
                    });
                }
            }
        }
    }

    fn add_tool_use(&mut self, tc: ToolCallEvent) {
        self.finalize_open();
        self.blocks.push(ContentBlock::ToolUse {
            id: tc.tool_call_id,
            name: tc.tool_name,
            input: normalized_tool_input(tc.raw_input),
        });
    }

    /// Add a client tool's `tool_use` block to the open frame, with partial framing when enabled.
    fn emit_client_tool_call(&mut self, out: &mut Vec<Value>, tc: ToolCallEvent) {
        // Track emission order so out-of-order `tool_result`s sort back into place.
        let order = self.take_tool_use_order();
        self.pending_client_tool_uses
            .insert(tc.tool_call_id.clone(), order);
        if self.include_partials() {
            self.partial_signature_only_block(out);
            self.partial_close_block(out);
            let id = tc.tool_call_id.clone();
            let name = tc.tool_name.clone();
            let input = normalized_tool_input(tc.raw_input.clone());
            self.add_tool_use(tc);
            let index = self.blocks.len().saturating_sub(1);
            self.partial_tool_use(out, index, &id, &name, &input);
        } else {
            self.add_tool_use(tc);
        }
    }

    // The frame and its partial `message_delta` resolve stop reason, usage, and
    // stop sequence through these three, so the two renderings never disagree.

    /// Reported reason, else `default`; a `None` default forces null so a failed turn is not mislabeled.
    fn resolved_stop_reason(&self, default: Option<&str>) -> Option<String> {
        let default = default?;
        self.response
            .pending()
            .and_then(|p| p.stop_reason.clone())
            .or_else(|| Some(default.to_string()))
    }

    /// Reported usage, else the identity's input-side usage (`output_tokens` 0).
    fn resolved_usage(&self) -> MessageUsage {
        self.response
            .pending()
            .and_then(|p| p.usage.as_ref())
            .cloned()
            .unwrap_or_else(|| self.response.identity().input_usage())
    }

    fn resolved_stop_sequence(&self) -> Option<String> {
        self.response
            .pending()
            .and_then(|p| p.stop_sequence.clone())
    }

    /// Flush the accumulated blocks as one assistant message. `default_stop_reason`
    /// applies only when no `ResponseCompleted` supplied one; `None` stamps null.
    fn flush_assistant(&mut self, default_stop_reason: Option<&str>) -> Option<Value> {
        self.finalize_open();
        if self.blocks.is_empty() {
            if self.response.started() {
                self.completed_responses += 1;
            }
            self.clear_pending();
            return None;
        }
        let identity = self.response.identity();
        let usage = self.resolved_usage();
        let stop_reason = self.resolved_stop_reason(default_stop_reason);
        let stop_sequence = self.resolved_stop_sequence();
        let pending = self.response.take_pending();
        let mut content = std::mem::take(&mut self.blocks);
        let fallback_sig = pending
            .signature
            .clone()
            .or_else(|| self.open_signature.take());
        if let Some(sig) = fallback_sig
            && let Some(ContentBlock::Thinking {
                signature: slot, ..
            }) = content
                .iter_mut()
                .rev()
                .find(|b| matches!(b, ContentBlock::Thinking { .. }))
            && slot.is_empty()
        {
            *slot = sig;
        }
        self.open_signature = None;
        let text: String = content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        self.last_text = text;
        let id = pending
            .message_id
            .clone()
            .or_else(|| identity.message_id.clone())
            .unwrap_or_else(|| {
                let id = format!("msg_{}", self.msg_seq);
                self.msg_seq += 1;
                id
            });
        let frame = AssistantFrame {
            message: AssistantMessage {
                id,
                kind: "message",
                role: "assistant",
                model: self.frame_model(&identity),
                content,
                stop_reason,
                stop_sequence,
                usage,
            },
            parent_tool_use_id: None,
            session_id: self.session_id().to_string(),
            uuid: new_uuid(),
        };
        self.assistant_frames += 1;
        self.completed_responses += 1;
        Some(to_line(&MessagesLine::Assistant(frame)))
    }

    /// Drop all per-response state so none leaks onto a later response.
    fn clear_pending(&mut self) {
        self.response.reset();
        self.open_signature = None;
    }

    /// Close the open partial message and flush the assistant frame with the same
    /// default stop reason, so the partial rebuild and frame never disagree.
    fn close_and_flush(&mut self, out: &mut Vec<Value>, default_stop_reason: Option<&str>) {
        self.partial_close_message(out, default_stop_reason);
        if let Some(assistant) = self.flush_assistant(default_stop_reason) {
            out.push(assistant);
        }
    }

    /// Shared terminal preamble for `finish`/`error`: init, reconcile deferred web
    /// searches, close+flush the open frame, then flush grouped tool results.
    fn flush_terminal_preamble(&mut self, out: &mut Vec<Value>, default_stop_reason: Option<&str>) {
        if let Some(init) = self.ensure_init() {
            out.push(init);
        }
        self.flush_unresolved_web_searches(out);
        self.close_and_flush(out, default_stop_reason);
        self.reconcile_unmatched_client_tools();
        self.flush_tool_results(out);
    }

    /// Reconcile deferred `web_search` calls that never terminated: emit each as a
    /// `server_tool_use` + `web_search_tool_result_error` pair, in invocation order.
    fn flush_unresolved_web_searches(&mut self, out: &mut Vec<Value>) {
        if self.backend_web_search_calls.is_empty() {
            return;
        }
        let mut leftovers: Vec<(u64, String)> = self
            .backend_web_search_calls
            .drain()
            .map(|(id, (order, _tc))| (order, id))
            .collect();
        leftovers.sort_by_key(|(order, _)| *order);
        let error = json!({
            "type": "web_search_tool_result_error",
            "error_code": "unavailable",
        });
        for (_order, id) in leftovers {
            // Query never arrived, so empty; the error result reflects an unresolved search.
            self.append_web_search_result(out, &id, "", &error);
        }
    }

    /// The session model for the `init` line and `result` `modelUsage`, or `"unknown"`.
    fn model_or_unknown(&self) -> String {
        self.session
            .as_ref()
            .and_then(|s| s.model.clone())
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// The model for one response's frames: its own model, then the session model, then `"unknown"`.
    fn frame_model(&self, identity: &ResponseIdentity) -> String {
        identity
            .model
            .as_deref()
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .or_else(|| {
                self.session
                    .as_ref()
                    .and_then(|s| s.model.clone())
                    .filter(|m| !m.is_empty())
            })
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Flush a completed-but-un-flushed response before new content begins.
    fn flush_prior_response(&mut self, out: &mut Vec<Value>) {
        if self.response.is_completed() {
            self.close_and_flush(out, Some("end_turn"));
        }
    }

    /// The next monotonic `tool_use` emission order.
    fn take_tool_use_order(&mut self) -> u64 {
        let order = self.next_tool_use_order;
        self.next_tool_use_order += 1;
        order
    }

    /// Buffer one terminal tool result for the next grouped `user` message, tagged
    /// with its `tool_use`'s emission order.
    fn buffer_tool_result(&mut self, u: ToolCallUpdateEvent) {
        let is_error = u.status == Some(acp::ToolCallStatus::Failed);
        let order = self
            .pending_client_tool_uses
            .remove(&u.tool_call_id)
            .unwrap_or_else(|| self.take_tool_use_order());
        self.pending_tool_results.push((
            order,
            ToolResultBlock {
                kind: "tool_result",
                tool_use_id: u.tool_call_id,
                content: tool_result_content(u.raw_output, u.content),
                is_error,
            },
        ));
    }

    /// Buffer an `is_error` `tool_result` for any client `tool_use` that never got one,
    /// so every `tool_use` is matched and the transcript stays valid.
    fn reconcile_unmatched_client_tools(&mut self) {
        if self.pending_client_tool_uses.is_empty() {
            return;
        }
        for (id, order) in std::mem::take(&mut self.pending_client_tool_uses) {
            self.pending_tool_results.push((
                order,
                ToolResultBlock {
                    kind: "tool_result",
                    tool_use_id: id,
                    content: Value::String("tool call did not complete".to_string()),
                    is_error: true,
                },
            ));
        }
    }

    /// Emit the buffered tool results as one grouped `user` message, in `tool_use` order.
    fn flush_tool_results(&mut self, out: &mut Vec<Value>) {
        if self.pending_tool_results.is_empty() {
            return;
        }
        let mut buffered = std::mem::take(&mut self.pending_tool_results);
        buffered.sort_by_key(|(order, _)| *order);
        let content = buffered.into_iter().map(|(_, block)| block).collect();
        out.push(to_line(&MessagesLine::User(ToolResultLine {
            message: ToolResultMessage {
                role: "user",
                content,
            },
            parent_tool_use_id: None,
            session_id: self.session_id().to_string(),
            uuid: new_uuid(),
        })));
    }

    /// Flush a prior response's frame and grouped tool results before new content begins.
    fn flush_boundary(&mut self, out: &mut Vec<Value>) {
        self.flush_prior_response(out);
        self.flush_tool_results(out);
    }

    /// Whether any prior-response state remains that a new `ResponseStarted` must flush first.
    fn has_unflushed_response(&self) -> bool {
        self.response.is_active()
            || !self.blocks.is_empty()
            || !self.open_text.is_empty()
            || self.open_signature.is_some()
            || !self.pending_tool_results.is_empty()
    }

    fn result_session_id<'a>(&'a self, end_session_id: &'a str) -> &'a str {
        if end_session_id.is_empty() {
            self.session_id()
        } else {
            end_session_id
        }
    }
}

impl Reducer for MessagesReducer {
    fn begin(&mut self, ctx: SessionContext) -> Vec<Value> {
        debug_assert!(
            self.session.is_none(),
            "MessagesReducer::begin called twice; the session context is set once"
        );
        self.session = Some(SessionState {
            session_id: ctx.session_id,
            model: ctx.model,
            cwd: ctx.cwd,
            permission_mode: ctx.permission_mode,
            api_key_auth: ctx.api_key_auth,
            mcp_servers: ctx.mcp_servers,
            include_partials: ctx.include_partial_messages,
            context_window: ctx.context_window,
        });
        // Init is deferred to the first output line so tool/command lists fill.
        Vec::new()
    }

    fn reduce(&mut self, event: StreamEvent) -> Vec<Value> {
        let mut out = Vec::new();
        // Metadata accumulates before init; `ResponseCompleted` must not force init.
        let is_metadata = matches!(
            event,
            StreamEvent::AvailableCommands { .. }
                | StreamEvent::ResponseStarted { .. }
                | StreamEvent::ReasoningCompleted { .. }
                | StreamEvent::ResponseCompleted { .. }
        );
        if !is_metadata && let Some(init) = self.ensure_init() {
            out.push(init);
        }
        match event {
            StreamEvent::AvailableCommands {
                tools,
                commands,
                skills,
            } => {
                if !tools.is_empty() {
                    self.tools = tools;
                }
                // Update commands and skills together so a later empty update clears neither.
                if !commands.is_empty() {
                    self.slash_commands = commands;
                    self.skills = skills;
                }
            }
            // Skip empty chunks so the partial block index can never desync.
            StreamEvent::AgentMessage(text) if text.is_empty() => {}
            StreamEvent::AgentThought(text) if text.is_empty() => {}
            StreamEvent::AgentMessage(text) => {
                self.flush_boundary(&mut out);
                self.partial_signature_only_block(&mut out);
                if self.include_partials() && self.open_kind.is_some_and(|k| k != TextKind::Text) {
                    self.partial_close_block(&mut out);
                }
                self.append_text(TextKind::Text, &text);
                if self.include_partials() {
                    let index = self.blocks.len();
                    self.partial_delta(
                        &mut out,
                        TextKind::Text,
                        index,
                        PartialDelta::Text { text },
                    );
                }
            }
            StreamEvent::AgentThought(text) => {
                self.flush_boundary(&mut out);
                self.partial_signature_only_block(&mut out);
                if self.include_partials()
                    && self.open_kind.is_some_and(|k| k != TextKind::Thinking)
                {
                    self.partial_close_block(&mut out);
                }
                self.append_text(TextKind::Thinking, &text);
                if self.include_partials() {
                    let index = self.blocks.len();
                    self.partial_delta(
                        &mut out,
                        TextKind::Thinking,
                        index,
                        PartialDelta::Thinking { thinking: text },
                    );
                }
            }
            StreamEvent::ToolCall(tc) if tc.backend_web_search => {
                // Query and results are unknown until completion, so defer; stamp invocation order.
                let order = self.take_tool_use_order();
                self.backend_web_search_calls
                    .insert(tc.tool_call_id.clone(), (order, tc));
            }
            StreamEvent::ToolCall(tc) => {
                // Flush a prior tool round's results so rounds interleave on backends without `ResponseStarted`.
                self.flush_tool_results(&mut out);
                self.emit_client_tool_call(&mut out, tc);
            }
            StreamEvent::ToolCallUpdate(u) => {
                let terminal = matches!(
                    u.status,
                    Some(acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed)
                );
                if terminal {
                    if let Some((_order, tc)) =
                        self.backend_web_search_calls.remove(&u.tool_call_id)
                    {
                        self.finish_web_search(&mut out, tc, u);
                    } else {
                        self.close_and_flush(&mut out, Some("tool_use"));
                        self.buffer_tool_result(u);
                    }
                }
            }
            StreamEvent::Lifecycle(Lifecycle::CompactCompleted { pre_tokens }) => {
                self.flush_boundary(&mut out);
                out.push(to_line(&MessagesLine::System(SystemLine::CompactBoundary(
                    CompactBoundaryLine {
                        compact_metadata: CompactMetadata {
                            trigger: "auto",
                            pre_tokens,
                        },
                        session_id: self.session_id().to_string(),
                        uuid: new_uuid(),
                    },
                ))));
            }
            StreamEvent::ResponseStarted {
                message_id,
                model,
                input_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            } => {
                // Flush any prior response before this opens so metadata is not cross-attributed.
                if self.has_unflushed_response() {
                    if let Some(init) = self.ensure_init() {
                        out.push(init);
                    }
                    self.close_and_flush(&mut out, Some("end_turn"));
                    self.flush_tool_results(&mut out);
                }
                // Adopt this response's model as the session model so `init`/`modelUsage` track a switch.
                if let Some(model) = model.clone()
                    && !model.is_empty()
                    && let Some(session) = self.session.as_mut()
                {
                    session.model = Some(model);
                }
                // Clone (never take) the identity so both the partial start and final frame read it.
                self.response.open(ResponseIdentity {
                    message_id,
                    model,
                    input_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                });
            }
            StreamEvent::ReasoningCompleted { signature } => {
                // A pending signature belongs to a new block, so finalize the current one first.
                if self.open_signature.is_some() {
                    if self.include_partials() {
                        if let Some(init) = self.ensure_init() {
                            out.push(init);
                        }
                        if self.framing.open_block().is_some() {
                            self.partial_close_block(&mut out);
                        } else {
                            self.partial_signature_only_block(&mut out);
                        }
                    }
                    self.finalize_open();
                }
                self.open_signature = signature;
            }
            StreamEvent::ResponseCompleted {
                message_id,
                stop_reason,
                usage,
                signature,
                stop_sequence,
            } => {
                self.flush_boundary(&mut out);
                // Drop a late completion for an already-flushed response (id differs), else it cross-attributes.
                let open_id = self.response.identity().message_id;
                let stale = self.response.is_started()
                    && matches!((&open_id, &message_id), (Some(o), Some(d)) if o != d);
                if stale {
                    tracing::warn!(
                        open_id = ?open_id,
                        completed_id = ?message_id,
                        "messages: dropping late ResponseCompleted for an already-flushed response"
                    );
                } else {
                    let usage: Option<MessageUsage> = usage.as_ref().map(MessageUsage::from);
                    self.response.complete(PendingResponse {
                        message_id,
                        stop_reason,
                        usage,
                        signature,
                        stop_sequence,
                    });
                }
            }
            StreamEvent::Lifecycle(_) | StreamEvent::Plan(_) => {}
        }
        out
    }

    fn max_turns(&mut self) -> Vec<Value> {
        self.max_turns_hit = true;
        Vec::new()
    }

    fn finish(&mut self, end: &TurnEnd<'_>) -> Vec<Value> {
        let mut out = Vec::new();
        let refused = end.stop_reason == "refusal";
        let cancelled = end.stop_reason == "cancelled";
        let structured_err = match &end.structured_output {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        };
        // Default stop reason when no `ResponseCompleted` supplied one; `null` when the turn did not complete normally.
        let did_not_complete_normally =
            self.max_turns_hit || refused || cancelled || structured_err.is_some();
        let flush_default = if did_not_complete_normally {
            None
        } else {
            match end.stop_reason {
                "max_tokens" => Some("max_tokens"),
                _ => Some("end_turn"),
            }
        };
        self.flush_terminal_preamble(&mut out, flush_default);
        let (subtype, is_error, errors) = if self.max_turns_hit {
            (
                "error_max_turns",
                true,
                Some(vec!["Reached the maximum number of turns".to_string()]),
            )
        } else if refused {
            (
                "error_during_execution",
                true,
                Some(vec!["The model refused to continue".to_string()]),
            )
        } else if cancelled {
            // No `cancelled` subtype in the Messages SDK, so use the catch-all `error_during_execution`.
            (
                "error_during_execution",
                true,
                Some(vec!["cancelled".to_string()]),
            )
        } else if let Some(msg) = structured_err {
            ("error_max_structured_output_retries", true, Some(vec![msg]))
        } else {
            ("success", false, None)
        };
        let structured_output = match end.structured_output.clone() {
            Some(Ok(value)) if !is_error => Some(value),
            _ => None,
        };
        let ru = self.messages_result_usage(end.usage);
        out.push(to_line(&MessagesLine::Result(Box::new(ResultLine {
            subtype,
            is_error,
            duration_ms: end.duration_ms,
            duration_api_ms: ru.duration_api_ms,
            num_turns: ru.num_turns,
            // Fall back to the caller's buffer only when no frame was flushed; else `last_text` is authoritative.
            result: (!is_error).then(|| {
                if self.assistant_frames == 0 && self.last_text.is_empty() {
                    end.result_text.to_string()
                } else {
                    self.last_text.clone()
                }
            }),
            stop_reason: Some(end.stop_reason.to_string()),
            total_cost_usd: ru.total_cost_usd,
            usage: ru.usage,
            model_usage: ru.model_usage,
            structured_output,
            errors,
            session_id: self.result_session_id(end.session_id).to_string(),
            uuid: new_uuid(),
        }))));
        out
    }

    fn error(
        &mut self,
        message: &str,
        usage: Option<&Value>,
        duration_ms: u64,
        stop_reason: Option<&str>,
    ) -> Vec<Value> {
        let mut out = Vec::new();
        // Max-tokens truncation stamps `max_tokens`; any other error falls back to `null`.
        let flush_default = match stop_reason {
            Some("max_tokens") => Some("max_tokens"),
            _ => None,
        };
        self.flush_terminal_preamble(&mut out, flush_default);
        let ru = self.messages_result_usage(usage);
        out.push(to_line(&MessagesLine::Result(Box::new(ResultLine {
            subtype: "error_during_execution",
            is_error: true,
            duration_ms,
            duration_api_ms: ru.duration_api_ms,
            num_turns: ru.num_turns,
            result: None,
            stop_reason: stop_reason.map(str::to_string),
            total_cost_usd: ru.total_cost_usd,
            usage: ru.usage,
            model_usage: ru.model_usage,
            structured_output: None,
            errors: Some(vec![message.to_string()]),
            session_id: self.session_id().to_string(),
            uuid: new_uuid(),
        }))));
        out
    }
}

/// A `tool_use.input` must be a JSON object; anything else degrades to `{}`.
fn normalized_tool_input(raw: Value) -> Value {
    if raw.is_object() { raw } else { json!({}) }
}

/// Reduce a tool result to a `tool_result.content` string (verbatim, else compact JSON).
fn tool_result_content(output: Value, content: Value) -> Value {
    match output {
        Value::String(s) => Value::String(s),
        Value::Null => match &content {
            Value::Array(items) if !items.is_empty() => Value::String(content.to_string()),
            _ => Value::String(String::new()),
        },
        other => Value::String(other.to_string()),
    }
}
