//! Layer-2 stream transform for the Chat Completions API.
//!
//! Consumes a raw `ChatCompletionChunk` stream and produces
//! [`SamplingEvent`]s. Pure: no I/O, no shell coupling.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::{BoxStream, Stream};

use echo_agent_sampling_types::{
    AssistantItem, ChatChunkChoice, ChatCompletionChunk, ConversationItem, ConversationResponse,
    FinishReason, ResponseModelMetadata, SamplingError, StopReason, TokenUsage, ToolCall,
};

use crate::events::{SamplingChannel, SamplingErrorInfo, SamplingEvent};
use crate::metrics::InferenceLatencyStats;
use crate::types::RequestId;

/// Stable prefix used by the request actor to recognize the one class of
/// stream failure that can be recovered safely by disabling parallel tools.
pub(crate) const AMBIGUOUS_PARALLEL_TOOL_STREAM: &str = "模型服务返回的并行工具调用缺少稳定标识";

/// Select the first wire choice and keep its terminal reason immutable.
#[derive(Default)]
pub(crate) struct ChatChoiceState {
    selected_index: Option<u32>,
    pub(crate) finish_reason: Option<FinishReason>,
    pub(crate) saw_finish_reason: bool,
}

impl ChatChoiceState {
    pub(crate) fn observe(&mut self, choice: &ChatChunkChoice) -> Result<bool, SamplingError> {
        self.saw_finish_reason |= choice.finish_reason.is_some();
        if choice.index != *self.selected_index.get_or_insert(choice.index) {
            return Ok(false);
        }
        if let Some(previous) = self.finish_reason {
            let delta = &choice.delta;
            let has_output = delta.content.as_ref().is_some_and(|text| !text.is_empty())
                || delta
                    .reasoning_content
                    .as_ref()
                    .is_some_and(|text| !text.is_empty())
                || !delta.tool_calls.is_empty();
            if has_output
                || choice
                    .finish_reason
                    .is_some_and(|reason| reason != previous)
            {
                return Err(SamplingError::ChatStreamInterrupted {
                    detail:
                        "Chat Completions sent conflicting data after the selected choice finished"
                            .into(),
                    retryable: false,
                });
            }
        } else {
            self.finish_reason = choice.finish_reason;
        }
        Ok(true)
    }
}

fn next_tool_slot(preferred: Option<u32>, calls: &BTreeMap<u32, (String, String, String)>) -> u32 {
    if let Some(preferred) = preferred
        && !calls.contains_key(&preferred)
    {
        return preferred;
    }
    calls
        .last_key_value()
        .map_or(0, |(index, _)| index.saturating_add(1))
}

/// Resolve unreliable OpenAI-compatible wire indices to stable local slots.
/// IDs are authoritative. Duplicate/missing indices are recovered only when
/// the chunk shape makes the association unambiguous; otherwise the stream is
/// failed rather than attaching arguments to the wrong tool call.
fn resolve_tool_slot(
    wire_index: Option<u32>,
    id: Option<&str>,
    calls: &BTreeMap<u32, (String, String, String)>,
    wire_slots: &mut HashMap<u32, Vec<u32>>,
    id_slots: &mut HashMap<String, u32>,
) -> Result<u32, String> {
    if let Some(id) = id {
        if let Some(slot) = id_slots.get(id) {
            return Ok(*slot);
        }

        let slot = if let Some(wire_index) = wire_index {
            let candidates = wire_slots.entry(wire_index).or_default();
            if let Some(slot) = candidates.iter().copied().find(|slot| {
                calls
                    .get(slot)
                    .is_some_and(|(known_id, _, _)| known_id.is_empty() || known_id == id)
            }) {
                slot
            } else {
                let slot = next_tool_slot(Some(wire_index), calls);
                candidates.push(slot);
                slot
            }
        } else {
            next_tool_slot(None, calls)
        };
        id_slots.insert(id.to_string(), slot);
        return Ok(slot);
    }

    if let Some(wire_index) = wire_index {
        let candidates = wire_slots
            .entry(wire_index)
            .or_insert_with(|| vec![wire_index]);
        if let [slot] = candidates.as_slice() {
            return Ok(*slot);
        }
        return match candidates.as_slice() {
            [slot] => Ok(*slot),
            slots => Err(format!(
                "{AMBIGUOUS_PARALLEL_TOOL_STREAM}：索引 {wire_index} 对应 {} 个调用，但当前片段没有调用 ID。",
                slots.len()
            )),
        };
    }

    match calls.len() {
        0 => Ok(0),
        1 => Ok(*calls.first_key_value().expect("one call exists").0),
        len => Err(format!(
            "{AMBIGUOUS_PARALLEL_TOOL_STREAM}：当前参数片段同时省略了索引和调用 ID，无法安全区分 {len} 个调用。"
        )),
    }
}

fn validate_parallel_tool_delta_batch(
    deltas: &[echo_agent_sampling_types::ToolCallDelta],
) -> Result<(), String> {
    if deltas.len() <= 1 {
        return Ok(());
    }
    let mut indices = std::collections::HashSet::with_capacity(deltas.len());
    for delta in deltas {
        let Some(index) = delta.index else {
            return Err(format!(
                "{AMBIGUOUS_PARALLEL_TOOL_STREAM}：同一片段包含 {} 个调用，但有调用没有索引。",
                deltas.len()
            ));
        };
        if !indices.insert(index) {
            return Err(format!(
                "{AMBIGUOUS_PARALLEL_TOOL_STREAM}：同一片段内的多个调用重复使用索引 {index}。"
            ));
        }
    }
    Ok(())
}

/// Transform a raw Chat Completions chunk stream into a stream of
/// [`SamplingEvent`]s.
///
/// The output stream emits exactly one terminal event per request:
/// [`SamplingEvent::Completed`] on normal stream end, or
/// [`SamplingEvent::Failed`] on error / idle timeout. Callers must not
/// consume past the terminal event (the implementation `return`s after
/// yielding it).
///
/// `idle_timeout` covers two cases:
/// 1. The transport stops yielding chunks at all (`tokio::time::timeout`).
/// 2. The transport keeps yielding empty / keepalive chunks but no
///    meaningful content (separate `last_content_chunk_at` timer).
///
/// Both produce `SamplingEvent::Failed { kind: IdleTimeout }`.
pub fn stream_chat_completions<'a>(
    raw_stream: BoxStream<'a, Result<ChatCompletionChunk, SamplingError>>,
    model_metadata: Option<ResponseModelMetadata>,
    request_id: RequestId,
    idle_timeout: Duration,
) -> impl Stream<Item = SamplingEvent> + Send + 'a {
    async_stream::stream! {
        let stream_start = Instant::now();
        let mut chunk_timestamps: Vec<Instant> = Vec::new();

        // Emit StreamStarted before reading any chunks so subscribers
        // can record TTFB / TTLB baselines.
        yield SamplingEvent::StreamStarted {
            request_id: request_id.clone(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        if let Some(metadata) = model_metadata {
            yield SamplingEvent::ModelMetadata {
                request_id: request_id.clone(),
                metadata,
            };
        }

        // Per-response accumulators
        let mut first_chunk_seen = false;
        let mut first_choice_seen = false;
        let mut choices = ChatChoiceState::default();
        let mut first_token_emitted = false;
        let mut model: String = String::new();
        let mut model_fingerprint: Option<String> = None;
        let mut usage: Option<TokenUsage> = None;
        let mut cost_usd_ticks: Option<i64> = None;

        let mut content_acc = String::new();
        let mut reasoning_acc = String::new();
        // Tool call deltas keyed by positional index. Each entry is
        // (id, name, arguments_buffer); the first chunk for an index
        // carries id+name and starts the arguments buffer, subsequent
        // chunks append to arguments only.
        let mut tool_call_acc: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
        let mut tool_wire_slots: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut tool_id_slots: HashMap<String, u32> = HashMap::new();
        // Do not expose provisional tool deltas until the complete stream has
        // proven that every wire index maps unambiguously. Some compatible
        // gateways introduce a second call with the same index in a later SSE
        // chunk, then omit IDs from argument continuations. Forwarding earlier
        // deltas would make the request look externally observed and prevent
        // the actor from safely retrying with parallel tools disabled.
        let mut buffered_tool_deltas: Vec<(
            u32,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = Vec::new();

        // Index counter spanning text + reasoning chunks (matches the
        // shell's chunk_index used for notification correlation).
        let mut chunk_index: u64 = 0;
        // Separate counter for AgentMessageChunk (text-only) emissions;
        // mirrored onto ConversationResponse.message_chunks_emitted so
        // downstream can detect lost-streaming-events scenarios.
        let mut message_chunk_count: u64 = 0;

        // Content-aware idle timer: the outer
        // `tokio::time::timeout(idle_timeout, stream.next())` already
        // catches "transport stops yielding chunks". This second timer
        // catches the more subtle case where the model keeps emitting
        // keepalive / empty-delta SSE events that satisfy the outer
        // timer but make no real progress -- some inference engines
        // do exactly that.
        let mut last_content_chunk_at = Instant::now();

        let mut stream = raw_stream;
        loop {
            let next = match tokio::time::timeout(idle_timeout, stream.next()).await {
                Ok(Some(next)) => next,
                Ok(None) => break, // stream ended normally
                Err(_) if choices.finish_reason.is_some() => break, // the optional trailer stalled
                Err(_elapsed) => {
                    let err = SamplingError::IdleTimeout {
                        elapsed_secs: idle_timeout.as_secs(),
                    };
                    yield SamplingEvent::Failed {
                        request_id: request_id.clone(),
                        error: SamplingErrorInfo::from(&err),
                    };
                    return;
                }
            };
            let chunk = match next {
                Ok(chunk) => chunk,
                Err(err) => {
                    yield SamplingEvent::Failed {
                        request_id: request_id.clone(),
                        error: SamplingErrorInfo::from(&err),
                    };
                    return;
                }
            };

            if !first_chunk_seen {
                model = chunk.model.clone();
                model_fingerprint = chunk
                    .system_fingerprint
                    .clone()
                    .filter(|s| !s.is_empty());
                first_chunk_seen = true;
            }

            if let Some(u) = chunk.usage.clone() {
                // Wire cost is cumulative for the response, so last-write-wins.
                // Never clobber a known cost with missing/unreported.
                let chunk_cost = echo_agent_sampling_types::reported_cost_ticks(u.cost_in_usd_ticks);
                cost_usd_ticks = match (cost_usd_ticks, chunk_cost) {
                    (_, Some(n)) => Some(n),
                    (prev, None) => prev,
                };
                usage = Some(u.into());
            }

            // Track whether this chunk carried meaningful content.
            // Set inside the choices loop and checked at the end.
            let mut chunk_has_content = false;

            for choice in chunk.choices.into_iter() {
                match choices.observe(&choice) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        yield SamplingEvent::Failed {
                            request_id: request_id.clone(),
                            error: SamplingErrorInfo::from(&error),
                        };
                        return;
                    }
                }
                first_choice_seen = true;
                if choice.finish_reason.is_some() {
                    chunk_has_content = true;
                }

                let delta = choice.delta;

                if let Some(text) = delta.content
                    && !text.is_empty()
                {
                    if !first_token_emitted {
                        first_token_emitted = true;
                        yield SamplingEvent::FirstToken {
                            request_id: request_id.clone(),
                        };
                    }
                    chunk_has_content = true;
                    chunk_timestamps.push(Instant::now());
                    chunk_index += 1;
                    message_chunk_count += 1;
                    content_acc.push_str(&text);
                    yield SamplingEvent::ChannelToken {
                        request_id: request_id.clone(),
                        channel: SamplingChannel::Text,
                        text,
                        chunk_index,
                    };
                }

                if let Some(thought) = delta.reasoning_content
                    && !thought.is_empty()
                {
                    if !first_token_emitted {
                        first_token_emitted = true;
                        yield SamplingEvent::FirstToken {
                            request_id: request_id.clone(),
                        };
                    }
                    chunk_has_content = true;
                    chunk_index += 1;
                    reasoning_acc.push_str(&thought);
                    yield SamplingEvent::ChannelToken {
                        request_id: request_id.clone(),
                        channel: SamplingChannel::Reasoning,
                        text: thought,
                        chunk_index,
                    };
                }

                if let Err(message) = validate_parallel_tool_delta_batch(&delta.tool_calls) {
                    let error = SamplingError::EventStreamError(message);
                    yield SamplingEvent::Failed {
                        request_id: request_id.clone(),
                        error: SamplingErrorInfo::from(&error),
                    };
                    return;
                }
                for tc_delta in delta.tool_calls {
                    chunk_has_content = true;

                    let incoming_id = tc_delta.id.filter(|id| !id.trim().is_empty());
                    let tool_index = match resolve_tool_slot(
                        tc_delta.index,
                        incoming_id.as_deref(),
                        &tool_call_acc,
                        &mut tool_wire_slots,
                        &mut tool_id_slots,
                    ) {
                        Ok(index) => index,
                        Err(message) => {
                            let error = SamplingError::EventStreamError(message);
                            yield SamplingEvent::Failed {
                                request_id: request_id.clone(),
                                error: SamplingErrorInfo::from(&error),
                            };
                            return;
                        }
                    };

                    let entry = tool_call_acc
                        .entry(tool_index)
                        .or_insert_with(|| (String::new(), String::new(), String::new()));

                    let mut id_for_event: Option<String> = None;
                    let mut name_for_event: Option<String> = None;
                    let mut args_for_event: Option<String> = None;

                    if let Some(id) = incoming_id {
                        if entry.0.is_empty() {
                            entry.0 = id.clone();
                        }
                        id_for_event = Some(id);
                    }
                    if let Some(func) = tc_delta.function {
                        if let Some(name) = func.name.filter(|name| !name.trim().is_empty()) {
                            // OpenAI-compatible providers are inconsistent here: some repeat
                            // function.name in later argument chunks, and some send an empty
                            // string instead of omitting it. The first non-blank name is the
                            // authoritative one. Never let a malformed trailing delta erase or
                            // replace it; doing so leaves valid arguments attached to tool `""`.
                            if entry.1.is_empty() {
                                entry.1 = name.clone();
                                name_for_event = Some(name);
                            } else if entry.1 == name {
                                name_for_event = Some(name);
                            } else {
                                let error = SamplingError::EventStreamError(format!(
                                    "模型服务在同一工具调用中将名称从“{}”更改为“{name}”，无法安全归属参数片段。",
                                    entry.1
                                ));
                                yield SamplingEvent::Failed {
                                    request_id: request_id.clone(),
                                    error: SamplingErrorInfo::from(&error),
                                };
                                return;
                            }
                        }
                        if let Some(args) = func.arguments {
                            entry.2.push_str(&args);
                            args_for_event = Some(args);
                        }
                    }

                    buffered_tool_deltas.push((
                        tool_index,
                        id_for_event,
                        name_for_event,
                        args_for_event,
                    ));
                }
            }

            if chunk_has_content {
                last_content_chunk_at = Instant::now();
            } else if last_content_chunk_at.elapsed() > idle_timeout {
                if choices.finish_reason.is_some() {
                    break;
                }
                let err = SamplingError::IdleTimeout {
                    elapsed_secs: idle_timeout.as_secs(),
                };
                yield SamplingEvent::Failed {
                    request_id: request_id.clone(),
                    error: SamplingErrorInfo::from(&err),
                };
                return;
            }
        }

        let mut finish_reason = choices.finish_reason.map(StopReason::from);
        // Explicit truncation and filtering take precedence over the
        // presence of tool fragments, even if their JSON happens to parse.
        if finish_reason == Some(StopReason::Length) {
            yield SamplingEvent::Failed {
                request_id: request_id.clone(),
                error: SamplingErrorInfo::from(&SamplingError::MaxTokensTruncation),
            };
            return;
        }
        if finish_reason == Some(StopReason::ContentFilter) {
            buffered_tool_deltas.clear();
            tool_call_acc.clear();
        }

        // The stream completed without an ambiguous association. Only now is
        // it safe to publish the tool-call deltas to downstream state/UI.
        for (tool_index, id, name, arguments_delta) in buffered_tool_deltas {
            yield SamplingEvent::ToolCallDelta {
                request_id: request_id.clone(),
                tool_index,
                id,
                name,
                arguments_delta,
            };
        }

        // ── Build the final response ─────────────────────────────────
        let tool_calls: Vec<ToolCall> = tool_call_acc
            .into_values()
            .map(|(id, name, arguments)| ToolCall {
                id: std::sync::Arc::<str>::from(id),
                name,
                arguments: std::sync::Arc::<str>::from(arguments),
            })
            .collect();

        // Honor tool calls by overriding the stop reason if the model
        // forgot to set it (mirrors the shell's behavior).
        if !tool_calls.is_empty() {
            finish_reason = Some(StopReason::ToolCalls);
        }

        // Build the trailing Assistant + any reasoning sibling.
        let mut items: Vec<ConversationItem> = Vec::new();
        if first_choice_seen {
            if !reasoning_acc.is_empty() {
                items.push(ConversationItem::Reasoning(
                    echo_agent_sampling_types::synthesized_reasoning_item(reasoning_acc),
                ));
            }
            items.push(ConversationItem::Assistant(AssistantItem {
                content: std::sync::Arc::<str>::from(content_acc),
                tool_calls,
                model_id: Some(model),
                model_fingerprint,
                // Chat Completions does not echo the applied reasoning effort.
                reasoning_effort: None,
            }));
        } else {
            items.push(ConversationItem::assistant(""));
        }

        let stream_end = Instant::now();
        let metrics =
            InferenceLatencyStats::from_timestamps(stream_start, &chunk_timestamps, stream_end);

        let response = ConversationResponse {
            items,
            stop_reason: finish_reason,
            usage,
            cost_usd_ticks,
            message_chunks_emitted: message_chunk_count,
            doom_loop_signals: Vec::new(),
            stop_message: None,
            message_id: None,
            raw_stop_reason: None,
            stop_sequence: None,
        };

        yield SamplingEvent::Completed {
            request_id: request_id.clone(),
            response: Box::new(response),
            metrics,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_sampling_types::{
        ChatChunkChoice, ChatChunkDelta, FinishReason, Role, ToolCallDelta as ChunkToolCallDelta,
        ToolCallFunctionDelta, Usage, rs,
    };
    use futures_util::stream;
    use std::pin::pin;

    fn rid() -> RequestId {
        RequestId::from("test-req")
    }

    fn make_chunk(deltas: Vec<ChatChunkDelta>) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "chunk-1".into(),
            object: "chat.completion.chunk".into(),
            created: 0,
            model: "test-model".into(),
            choices: deltas
                .into_iter()
                .enumerate()
                .map(|(i, delta)| ChatChunkChoice {
                    index: i as u32,
                    delta,
                    finish_reason: None,
                })
                .collect(),
            usage: None,
            system_fingerprint: None,
        }
    }

    fn text_chunk(text: &str) -> ChatCompletionChunk {
        make_chunk(vec![ChatChunkDelta {
            role: Some(Role::Assistant),
            content: Some(text.to_string()),
            reasoning_content: None,
            tool_calls: vec![],
            tool_call_id: None,
        }])
    }

    fn final_chunk(reason: FinishReason) -> ChatCompletionChunk {
        let mut chunk = make_chunk(vec![ChatChunkDelta::default()]);
        chunk.choices[0].finish_reason = Some(reason);
        chunk
    }

    async fn collect(s: impl Stream<Item = SamplingEvent>) -> Vec<SamplingEvent> {
        let mut out = Vec::new();
        let mut s = pin!(s);
        while let Some(ev) = s.next().await {
            out.push(ev);
        }
        out
    }

    #[tokio::test]
    async fn empty_stream_yields_started_then_completed() {
        let raw = stream::iter(Vec::<Result<ChatCompletionChunk, SamplingError>>::new()).boxed();
        let events = collect(stream_chat_completions(
            raw,
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], SamplingEvent::StreamStarted { .. }));
        match &events[1] {
            SamplingEvent::Completed { response, .. } => {
                assert!(response.is_empty());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn text_only_stream_emits_first_token_then_channel_tokens_then_completed() {
        let chunks: Vec<Result<ChatCompletionChunk, SamplingError>> = vec![
            Ok(text_chunk("Hello, ")),
            Ok(text_chunk("world!")),
            Ok(final_chunk(FinishReason::Stop)),
        ];
        let raw = stream::iter(chunks).boxed();
        let events = collect(stream_chat_completions(
            raw,
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        // Expected sequence: StreamStarted, FirstToken, ChannelToken(Text)
        // x 2, Completed.
        assert!(matches!(events[0], SamplingEvent::StreamStarted { .. }));
        assert!(matches!(events[1], SamplingEvent::FirstToken { .. }));

        let text_tokens: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                SamplingEvent::ChannelToken {
                    channel: SamplingChannel::Text,
                    text,
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text_tokens, vec!["Hello, ", "world!"]);

        match events.last().unwrap() {
            SamplingEvent::Completed { response, .. } => {
                let a = response.assistant().expect("assistant item present");
                assert_eq!(a.content.as_ref(), "Hello, world!");
                assert_eq!(response.stop_reason, Some(StopReason::Stop));
                assert_eq!(response.message_chunks_emitted, 2);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reasoning_chunk_emits_reasoning_channel_and_first_token_once() {
        let mut reasoning_chunk = make_chunk(vec![ChatChunkDelta {
            role: Some(Role::Assistant),
            content: None,
            reasoning_content: Some("thinking...".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }]);
        reasoning_chunk.choices[0].finish_reason = None;

        let chunks: Vec<Result<ChatCompletionChunk, SamplingError>> = vec![
            Ok(reasoning_chunk),
            Ok(text_chunk("done")),
            Ok(final_chunk(FinishReason::Stop)),
        ];
        let raw = stream::iter(chunks).boxed();
        let events = collect(stream_chat_completions(
            raw,
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        // FirstToken should appear exactly once.
        let first_token_count = events
            .iter()
            .filter(|e| matches!(e, SamplingEvent::FirstToken { .. }))
            .count();
        assert_eq!(first_token_count, 1);

        let mut saw_reasoning = false;
        let mut saw_text = false;
        for e in &events {
            if let SamplingEvent::ChannelToken { channel, text, .. } = e {
                match channel {
                    SamplingChannel::Reasoning => {
                        assert_eq!(text, "thinking...");
                        saw_reasoning = true;
                    }
                    SamplingChannel::Text => {
                        assert_eq!(text, "done");
                        saw_text = true;
                    }
                }
            }
        }
        assert!(saw_reasoning && saw_text);

        match events.last().unwrap() {
            SamplingEvent::Completed { response, .. } => {
                let r = response
                    .reasoning_items()
                    .next()
                    .expect("reasoning sibling preserved");
                let rs::SummaryPart::SummaryText(t) = &r.summary[0];
                assert_eq!(t.text, "thinking...");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_call_stream_emits_deltas_and_assembles_final_call() {
        // First chunk has id + name + part of arguments.
        let chunk1 = make_chunk(vec![ChatChunkDelta {
            role: None,
            content: None,
            reasoning_content: None,
            tool_calls: vec![ChunkToolCallDelta {
                index: Some(0),
                id: Some("call_abc".into()),
                kind: Some("function".into()),
                function: Some(ToolCallFunctionDelta {
                    name: Some("do_thing".into()),
                    arguments: Some("{\"x\":".into()),
                }),
            }],
            tool_call_id: None,
        }]);
        // Second chunk has only argument fragment.
        let chunk2 = make_chunk(vec![ChatChunkDelta {
            role: None,
            content: None,
            reasoning_content: None,
            tool_calls: vec![ChunkToolCallDelta {
                index: Some(0),
                id: None,
                kind: None,
                function: Some(ToolCallFunctionDelta {
                    name: None,
                    arguments: Some("1}".into()),
                }),
            }],
            tool_call_id: None,
        }]);

        let raw = stream::iter::<Vec<Result<ChatCompletionChunk, SamplingError>>>(vec![
            Ok(chunk1),
            Ok(chunk2),
        ])
        .boxed();
        let events = collect(stream_chat_completions(
            raw,
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        let deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                SamplingEvent::ToolCallDelta {
                    tool_index,
                    id,
                    name,
                    arguments_delta,
                    ..
                } => Some((
                    *tool_index,
                    id.clone(),
                    name.clone(),
                    arguments_delta.clone(),
                )),
                _ => None,
            })
            .collect();

        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].0, 0);
        assert_eq!(deltas[0].1.as_deref(), Some("call_abc"));
        assert_eq!(deltas[0].2.as_deref(), Some("do_thing"));
        assert_eq!(deltas[0].3.as_deref(), Some("{\"x\":"));
        assert_eq!(deltas[1].1, None);
        assert_eq!(deltas[1].2, None);
        assert_eq!(deltas[1].3.as_deref(), Some("1}"));

        match events.last().unwrap() {
            SamplingEvent::Completed { response, .. } => {
                let calls = response.tool_calls();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id.as_ref(), "call_abc");
                assert_eq!(calls[0].name, "do_thing");
                assert_eq!(calls[0].arguments.as_ref(), "{\"x\":1}");
                // Tool calls force ToolCalls stop reason.
                assert_eq!(response.stop_reason, Some(StopReason::ToolCalls));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    fn parallel_tool_chunk(
        calls: Vec<(Option<u32>, Option<&str>, Option<&str>, &str)>,
    ) -> ChatCompletionChunk {
        make_chunk(vec![ChatChunkDelta {
            role: None,
            content: None,
            reasoning_content: None,
            tool_calls: calls
                .into_iter()
                .map(|(index, id, name, arguments)| ChunkToolCallDelta {
                    index,
                    id: id.map(str::to_string),
                    kind: Some("function".into()),
                    function: Some(ToolCallFunctionDelta {
                        name: name.map(str::to_string),
                        arguments: Some(arguments.to_string()),
                    }),
                })
                .collect(),
            tool_call_id: None,
        }])
    }

    #[tokio::test]
    async fn parallel_tool_calls_with_distinct_indices_do_not_cross_arguments() {
        let chunks = vec![
            Ok(parallel_tool_chunk(vec![
                (Some(0), Some("call_a"), Some("grep"), "{\"pattern\":\"a\""),
                (Some(1), Some("call_b"), Some("grep"), "{\"pattern\":\"b\""),
            ])),
            Ok(parallel_tool_chunk(vec![
                (Some(0), None, None, "}"),
                (Some(1), None, None, "}"),
            ])),
        ];
        let events = collect(stream_chat_completions(
            stream::iter(chunks).boxed(),
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        match events.last().expect("terminal event") {
            SamplingEvent::Completed { response, .. } => {
                let calls = response.tool_calls();
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].id.as_ref(), "call_a");
                assert_eq!(calls[0].arguments.as_ref(), r#"{"pattern":"a"}"#);
                assert_eq!(calls[1].id.as_ref(), "call_b");
                assert_eq!(calls[1].arguments.as_ref(), r#"{"pattern":"b"}"#);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn duplicate_wire_indices_fail_before_any_tool_delta_is_forwarded() {
        let chunks = vec![
            Ok(parallel_tool_chunk(vec![
                (Some(0), Some("call_a"), Some("grep"), "{\"pattern\":\"a\""),
                (Some(0), Some("call_b"), Some("grep"), "{\"pattern\":\"b\""),
            ])),
            Ok(parallel_tool_chunk(vec![
                (Some(0), None, None, "}"),
                (Some(0), None, None, "}"),
            ])),
        ];
        let events = collect(stream_chat_completions(
            stream::iter(chunks).boxed(),
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        assert!(matches!(events.last(), Some(SamplingEvent::Failed { .. })));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SamplingEvent::ToolCallDelta { .. }))
        );
    }

    #[tokio::test]
    async fn duplicate_wire_indices_without_ids_fail_instead_of_guessing_by_position() {
        let chunks = vec![
            Ok(parallel_tool_chunk(vec![
                (Some(0), None, Some("grep"), "{\"pattern\":\"a\""),
                (Some(0), None, Some("read_file"), "{\"path\":\"b\""),
            ])),
            Ok(parallel_tool_chunk(vec![
                (Some(0), None, None, "}"),
                (Some(0), None, None, "}"),
            ])),
        ];
        let events = collect(stream_chat_completions(
            stream::iter(chunks).boxed(),
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        assert!(matches!(events.last(), Some(SamplingEvent::Failed { .. })));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SamplingEvent::ToolCallDelta { .. }))
        );
    }

    #[tokio::test]
    async fn missing_wire_indices_fail_instead_of_guessing_by_position() {
        let chunks = vec![
            Ok(parallel_tool_chunk(vec![
                (None, Some("call_a"), Some("grep"), "{\"pattern\":\"a\""),
                (None, Some("call_b"), Some("grep"), "{\"pattern\":\"b\""),
            ])),
            Ok(parallel_tool_chunk(vec![
                (None, None, None, "}"),
                (None, None, None, "}"),
            ])),
        ];
        let events = collect(stream_chat_completions(
            stream::iter(chunks).boxed(),
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        assert!(matches!(events.last(), Some(SamplingEvent::Failed { .. })));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SamplingEvent::ToolCallDelta { .. }))
        );
    }

    #[tokio::test]
    async fn ambiguous_parallel_tool_delta_fails_instead_of_corrupting_calls() {
        let chunks = vec![
            Ok(parallel_tool_chunk(vec![
                (None, Some("call_a"), Some("grep"), "{"),
                (None, Some("call_b"), Some("grep"), "{"),
            ])),
            Ok(parallel_tool_chunk(vec![(None, None, None, "}")])),
        ];
        let events = collect(stream_chat_completions(
            stream::iter(chunks).boxed(),
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        assert!(matches!(events.last(), Some(SamplingEvent::Failed { .. })));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SamplingEvent::Completed { .. }))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SamplingEvent::ToolCallDelta { .. }))
        );
    }

    #[tokio::test]
    async fn duplicate_wire_index_across_chunks_is_buffered_then_fails_safely() {
        let chunks = vec![
            Ok(parallel_tool_chunk(vec![(
                Some(0),
                Some("call_a"),
                Some("grep"),
                "{",
            )])),
            Ok(parallel_tool_chunk(vec![(
                Some(0),
                Some("call_b"),
                Some("read_file"),
                "{",
            )])),
            Ok(parallel_tool_chunk(vec![(Some(0), None, None, "}")])),
        ];
        let events = collect(stream_chat_completions(
            stream::iter(chunks).boxed(),
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        assert!(matches!(events.last(), Some(SamplingEvent::Failed { .. })));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SamplingEvent::ToolCallDelta { .. }))
        );
    }

    #[tokio::test]
    async fn trailing_blank_tool_name_does_not_erase_first_non_blank_name() {
        let chunk1 = make_chunk(vec![ChatChunkDelta {
            role: None,
            content: None,
            reasoning_content: None,
            tool_calls: vec![ChunkToolCallDelta {
                index: Some(0),
                id: Some("call_read".into()),
                kind: Some("function".into()),
                function: Some(ToolCallFunctionDelta {
                    name: Some("read_file".into()),
                    arguments: Some("{\"target_file\":".into()),
                }),
            }],
            tool_call_id: None,
        }]);
        let chunk2 = make_chunk(vec![ChatChunkDelta {
            role: None,
            content: None,
            reasoning_content: None,
            tool_calls: vec![ChunkToolCallDelta {
                index: Some(0),
                id: Some(String::new()),
                kind: None,
                function: Some(ToolCallFunctionDelta {
                    name: Some(String::new()),
                    arguments: Some("\"README.md\"}".into()),
                }),
            }],
            tool_call_id: None,
        }]);

        let raw = stream::iter::<Vec<Result<ChatCompletionChunk, SamplingError>>>(vec![
            Ok(chunk1),
            Ok(chunk2),
        ])
        .boxed();
        let events = collect(stream_chat_completions(
            raw,
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        match events.last().unwrap() {
            SamplingEvent::Completed { response, .. } => {
                let calls = response.tool_calls();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id.as_ref(), "call_read");
                assert_eq!(calls[0].name, "read_file");
                assert_eq!(
                    calls[0].arguments.as_ref(),
                    r#"{"target_file":"README.md"}"#
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn changed_tool_name_fails_instead_of_appending_arguments_to_the_wrong_tool() {
        let chunks = vec![
            Ok(parallel_tool_chunk(vec![(
                Some(0),
                None,
                Some("grep"),
                "{\"pattern\":",
            )])),
            Ok(parallel_tool_chunk(vec![(
                Some(0),
                None,
                Some("read_file"),
                "\"README.md\"}",
            )])),
        ];
        let events = collect(stream_chat_completions(
            stream::iter(chunks).boxed(),
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        assert!(matches!(events.last(), Some(SamplingEvent::Failed { .. })));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SamplingEvent::Completed { .. }))
        );
    }

    #[tokio::test]
    async fn mid_stream_error_yields_failed_no_completed() {
        let chunks: Vec<Result<ChatCompletionChunk, SamplingError>> = vec![
            Ok(text_chunk("hi")),
            Err(SamplingError::EventStreamError("conn reset".into())),
        ];
        let raw = stream::iter(chunks).boxed();
        let events = collect(stream_chat_completions(
            raw,
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, SamplingEvent::Failed { .. }))
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SamplingEvent::Completed { .. }))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_timeout_when_stream_stalls() {
        // A stream that yields one chunk then hangs forever.
        let raw = stream::iter(vec![Ok(text_chunk("hello"))])
            .chain(stream::pending())
            .boxed();
        let events = collect(stream_chat_completions(
            raw,
            None,
            rid(),
            Duration::from_millis(100),
        ))
        .await;

        // Stream should emit StreamStarted, FirstToken, ChannelToken
        // then Failed(IdleTimeout) when the stall hits the deadline.
        match events.last().unwrap() {
            SamplingEvent::Failed { error, .. } => {
                assert_eq!(error.kind, crate::events::SamplingErrorKind::IdleTimeout);
            }
            other => panic!("expected Failed(IdleTimeout), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_metadata_yielded_after_stream_started() {
        let raw = stream::iter(Vec::<Result<ChatCompletionChunk, SamplingError>>::new()).boxed();
        let metadata = ResponseModelMetadata {
            context_window: Some(8192),
            max_completion_tokens: Some(4096),
            models_etag: None,
        };
        let events = collect(stream_chat_completions(
            raw,
            Some(metadata.clone()),
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        assert!(matches!(events[0], SamplingEvent::StreamStarted { .. }));
        match &events[1] {
            SamplingEvent::ModelMetadata { metadata: m, .. } => {
                assert_eq!(m.context_window, Some(8192));
                assert_eq!(m.max_completion_tokens, Some(4096));
            }
            other => panic!("expected ModelMetadata second, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn usage_is_extracted_from_chunk() {
        let mut chunk_with_usage = make_chunk(vec![ChatChunkDelta::default()]);
        chunk_with_usage.usage = Some(Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            prompt_tokens_details: None,
            completion_tokens_details: None,
            cost_in_usd_ticks: None,
        });

        let chunks: Vec<Result<ChatCompletionChunk, SamplingError>> = vec![
            Ok(text_chunk("ok")),
            Ok(chunk_with_usage),
            Ok(final_chunk(FinishReason::Stop)),
        ];
        let raw = stream::iter(chunks).boxed();
        let events = collect(stream_chat_completions(
            raw,
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;

        match events.last().unwrap() {
            SamplingEvent::Completed { response, .. } => {
                let u = response.usage.as_ref().expect("usage extracted");
                assert_eq!(u.prompt_tokens, 100);
                assert_eq!(u.completion_tokens, 50);
                assert_eq!(u.total_tokens, 150);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    /// Server-reported cost lands on the response; the REST mapper's `0`
    /// backfill means "unreported" and must yield `None`.
    #[tokio::test]
    async fn cost_is_extracted_and_zero_is_unreported() {
        for (wire, expected) in [(Some(78), Some(78)), (Some(0), None), (None, None)] {
            let mut chunk_with_usage = make_chunk(vec![ChatChunkDelta::default()]);
            chunk_with_usage.usage = Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                prompt_tokens_details: None,
                completion_tokens_details: None,
                cost_in_usd_ticks: wire,
            });
            let chunks: Vec<Result<ChatCompletionChunk, SamplingError>> = vec![
                Ok(text_chunk("ok")),
                Ok(chunk_with_usage),
                Ok(final_chunk(FinishReason::Stop)),
            ];
            let raw = stream::iter(chunks).boxed();
            let events = collect(stream_chat_completions(
                raw,
                None,
                rid(),
                Duration::from_secs(60),
            ))
            .await;
            match events.last().unwrap() {
                SamplingEvent::Completed { response, .. } => {
                    assert_eq!(response.cost_usd_ticks, expected, "wire {wire:?}");
                }
                other => panic!("expected Completed, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn later_missing_cost_does_not_clobber_earlier_ticks() {
        let mut first = make_chunk(vec![ChatChunkDelta::default()]);
        first.usage = Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            prompt_tokens_details: None,
            completion_tokens_details: None,
            cost_in_usd_ticks: Some(99),
        });
        let mut second = make_chunk(vec![ChatChunkDelta::default()]);
        second.usage = Some(Usage {
            prompt_tokens: 12,
            completion_tokens: 6,
            total_tokens: 18,
            prompt_tokens_details: None,
            completion_tokens_details: None,
            cost_in_usd_ticks: Some(0),
        });
        let chunks: Vec<Result<ChatCompletionChunk, SamplingError>> = vec![
            Ok(text_chunk("ok")),
            Ok(first),
            Ok(second),
            Ok(final_chunk(FinishReason::Stop)),
        ];
        let raw = stream::iter(chunks).boxed();
        let events = collect(stream_chat_completions(
            raw,
            None,
            rid(),
            Duration::from_secs(60),
        ))
        .await;
        match events.last().unwrap() {
            SamplingEvent::Completed { response, .. } => {
                assert_eq!(response.cost_usd_ticks, Some(99));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }
}
