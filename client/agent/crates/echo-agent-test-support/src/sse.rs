//! SSE stream generators for mock inference endpoints.
//!
//! These produce the exact wire format that the echoagent sampling client expects,
//! validated against the real sampling client.

use axum::response::sse::Event;
use serde_json::json;

use crate::scripted::SseEvent;

/// Generate Anthropic Messages SSE events: one text block streamed as a
/// single delta, terminated by a `message_delta` carrying `stop_reason`.
pub fn messages_api_events(text: &str, model: &str, stop_reason: &str) -> Vec<Event> {
    vec![
        Event::default().data(
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test", "type": "message", "role": "assistant",
                    "content": [], "model": model, "stop_reason": null,
                    "usage": {
                        "input_tokens": 10, "output_tokens": 0,
                        "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
        Event::default().data(
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})
                .to_string(),
        ),
        Event::default().data(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}})
                .to_string(),
        ),
        Event::default().data(json!({"type":"content_block_stop","index":0}).to_string()),
        Event::default().data(
            json!({"type":"message_delta","delta":{"stop_reason":stop_reason},"usage":{"output_tokens":5,"input_tokens":10}})
                .to_string(),
        ),
        Event::default().data(json!({"type":"message_stop"}).to_string()),
    ]
}

/// Generate ChatCompletions SSE events that stream `text` word-by-word
/// (whitespace-collapsing; use [`chat_completion_events_exact`] when the
/// receiver must reconstruct `text` byte-for-byte).
pub fn chat_completion_events(text: &str, model: &str) -> Vec<Event> {
    scripted_to_axum(chat_completion_script_from_deltas(
        &space_prefixed_deltas(text.split_whitespace()),
        model,
    ))
}

/// Like [`chat_completion_events`] but byte-exact: concatenating the deltas
/// reproduces `text` byte-for-byte (newlines and whitespace runs preserved).
/// Fenced code blocks (mermaid etc.) need their newlines to parse as a block,
/// which `split_whitespace` would destroy.
pub fn chat_completion_events_exact(text: &str, model: &str) -> Vec<Event> {
    scripted_to_axum(chat_completion_script_exact(text, model))
}

/// Byte-exact Chat Completions events for a [`crate::ScriptedResponse`].
pub fn chat_completion_script_exact(text: &str, model: &str) -> Vec<SseEvent> {
    chat_completion_script_from_deltas(&chat_completion_deltas(text), model)
}

/// Split `text` into deltas that reconstruct it byte-for-byte: the first
/// carries no leading space; each subsequent one is ` {word}` (split on
/// single spaces only, so newlines/tabs stay inside the words).
fn chat_completion_deltas(text: &str) -> Vec<String> {
    space_prefixed_deltas(text.split(' '))
}

/// Shape words into chat deltas: first word bare, each subsequent one ` {word}`
/// — the source iterator decides collapsing (echo) vs byte-exact (fixed).
fn space_prefixed_deltas<'a>(words: impl Iterator<Item = &'a str>) -> Vec<String> {
    words
        .enumerate()
        .map(|(i, word)| {
            if i == 0 {
                word.to_owned()
            } else {
                format!(" {word}")
            }
        })
        .collect()
}

fn chat_completion_script_from_deltas(deltas: &[String], model: &str) -> Vec<SseEvent> {
    let n = deltas.len();
    let mut events = Vec::new();

    for (i, content) in deltas.iter().enumerate() {
        let finish_reason = if i + 1 == n {
            json!("stop")
        } else {
            json!(null)
        };

        let chunk = if i == 0 {
            json!({
                "id": "chatcmpl-test",
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant", "content": content },
                    "finish_reason": finish_reason
                }]
            })
        } else {
            json!({
                "id": "chatcmpl-test",
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": { "content": content },
                    "finish_reason": finish_reason
                }]
            })
        };
        events.push(SseEvent::data(chunk.to_string()));
    }

    events.push(SseEvent::data(
        json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": model,
            "choices": [],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": n,
                "total_tokens": 10 + n
            }
        })
        .to_string(),
    ));
    events.push(SseEvent::data("[DONE]"));
    events
}

/// Generate Responses API SSE events that stream `text` word-by-word
/// (whitespace-collapsing; use [`responses_api_events_exact`] when the
/// receiver must reconstruct `text` byte-for-byte).
pub fn responses_api_events(text: &str, model: &str) -> Vec<Event> {
    let deltas: Vec<String> = text
        .split_whitespace()
        .map(|word| format!("{word} "))
        .collect();
    scripted_to_axum(responses_api_script_from_deltas(&deltas, text, model))
}

/// Like [`responses_api_events`] but byte-exact: concatenating the deltas
/// reproduces `text` byte-for-byte (newlines and whitespace runs preserved).
pub fn responses_api_events_exact(text: &str, model: &str) -> Vec<Event> {
    scripted_to_axum(responses_api_script_exact(text, model))
}

/// Byte-exact Responses API events for a [`crate::ScriptedResponse`].
pub fn responses_api_script_exact(text: &str, model: &str) -> Vec<SseEvent> {
    responses_api_script_from_deltas(&responses_api_deltas(text), text, model)
}

/// `split_inclusive(' ')` keeps each chunk's trailing space, so concatenating
/// the chunks reconstructs `text` byte-for-byte (newlines included).
fn responses_api_deltas(text: &str) -> Vec<String> {
    text.split_inclusive(' ').map(str::to_owned).collect()
}

// `deltas` and `text` deliberately disagree in echo mode: collapsed deltas, uncollapsed
// `response.completed` text — inherited load-bearing shell behavior, do not unify.
fn responses_api_script_from_deltas(deltas: &[String], text: &str, model: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut seq = 0;

    events.push(SseEvent::data(
        json!({
            "type": "response.created",
            "sequence_number": seq,
            "response": {
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "model": model,
                "status": "in_progress",
                "output": []
            }
        })
        .to_string(),
    ));
    seq += 1;

    for chunk in deltas {
        events.push(SseEvent::data(
            json!({
                "type": "response.output_text.delta",
                "sequence_number": seq,
                "item_id": "item_test",
                "output_index": 0,
                "content_index": 0,
                "delta": chunk
            })
            .to_string(),
        ));
        seq += 1;
    }

    events.push(SseEvent::data(
        json!({
            "type": "response.completed",
            "sequence_number": seq,
            "response": {
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "model": model,
                "status": "completed",
                "output": [{
                    "type": "message",
                    "id": "msg_test",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": text,
                        "annotations": []
                    }]
                }],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "total_tokens": 15,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens_details": { "reasoning_tokens": 0 }
                }
            }
        })
        .to_string(),
    ));
    events.push(SseEvent::data("[DONE]"));
    events
}

fn scripted_to_axum(events: Vec<SseEvent>) -> Vec<Event> {
    events
        .into_iter()
        .map(|scripted| {
            let event = Event::default().data(scripted.data);
            match scripted.event {
                Some(name) => event.event(name),
                None => event,
            }
        })
        .collect()
}

/// Generate Responses API SSE events for a reasoning-only completion: the
/// model streams reasoning summary deltas and finishes with a `reasoning`
/// output item but NO message / output-text and no tool call. The shell's
/// collector synthesizes an empty assistant, so the response classifies as
/// `EmptyReason::ReasoningOnly` — the trigger that makes the sampler resample
/// (the model doomloop).
///
/// Returns [`SseEvent`]s (not axum `Event`s) for direct use with
/// [`crate::ScriptedResponse::sse`] / `enqueue_response`: reasoning-only is a
/// scripted scenario, not an echo/fixed response mode, so it is not wired into
/// the `mock_server` mode handlers.
pub fn responses_api_reasoning_only_events(reasoning: &str, model: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut seq = 0;

    // response.created
    events.push(SseEvent::data(
        json!({
            "type": "response.created",
            "sequence_number": seq,
            "response": {
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "model": model,
                "status": "in_progress",
                "output": []
            }
        })
        .to_string(),
    ));
    seq += 1;

    // Reasoning summary deltas — the only content the model streams.
    for word in reasoning.split_whitespace() {
        events.push(SseEvent::data(
            json!({
                "type": "response.reasoning_summary_text.delta",
                "sequence_number": seq,
                "item_id": "reasoning_item_1",
                "output_index": 0,
                "summary_index": 0,
                "delta": format!("{word} ")
            })
            .to_string(),
        ));
        seq += 1;
    }

    // response.completed: a single `reasoning` output item carrying the full
    // summary and NO message item. `response_to_conversation_items` appends an
    // empty assistant, yielding `[Reasoning, Assistant("")]` → reasoning_only.
    events.push(SseEvent::data(
        json!({
            "type": "response.completed",
            "sequence_number": seq,
            "response": {
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "model": model,
                "status": "completed",
                "output": [{
                    "type": "reasoning",
                    "id": "reasoning_item_1",
                    "summary": [{
                        "type": "summary_text",
                        "text": reasoning
                    }],
                    "status": "completed"
                }],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "total_tokens": 15,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens_details": { "reasoning_tokens": 5 }
                }
            }
        })
        .to_string(),
    ));
    events.push(SseEvent::data("[DONE]"));
    events
}

/// Generate Responses API SSE events for a completion that streams reasoning
/// summary deltas FIRST and then a normal text answer: the shape a
/// reasoning-capable model produces on an ordinary turn. `response.completed`
/// carries both output items (`reasoning` + `message`), so the collector
/// yields `[Reasoning, Assistant(text)]` — a full, non-empty turn.
///
/// Returns [`SseEvent`]s for direct use with [`crate::ScriptedResponse::sse`]
/// / `enqueue_response`, mirroring [`responses_api_reasoning_only_events`].
pub fn responses_api_reasoning_and_text_events(
    reasoning: &str,
    text: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut seq = 0;

    // response.created
    events.push(SseEvent::data(
        json!({
            "type": "response.created",
            "sequence_number": seq,
            "response": {
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "model": model,
                "status": "in_progress",
                "output": []
            }
        })
        .to_string(),
    ));
    seq += 1;

    // Reasoning summary deltas stream before any answer text.
    for word in reasoning.split_whitespace() {
        events.push(SseEvent::data(
            json!({
                "type": "response.reasoning_summary_text.delta",
                "sequence_number": seq,
                "item_id": "reasoning_item_1",
                "output_index": 0,
                "summary_index": 0,
                "delta": format!("{word} ")
            })
            .to_string(),
        ));
        seq += 1;
    }

    // Then the visible answer.
    for word in text.split_whitespace() {
        events.push(SseEvent::data(
            json!({
                "type": "response.output_text.delta",
                "sequence_number": seq,
                "item_id": "item_test",
                "output_index": 1,
                "content_index": 0,
                "delta": format!("{word} ")
            })
            .to_string(),
        ));
        seq += 1;
    }

    // response.completed with BOTH items: reasoning + the assistant message.
    events.push(SseEvent::data(
        json!({
            "type": "response.completed",
            "sequence_number": seq,
            "response": {
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "model": model,
                "status": "completed",
                "output": [
                    {
                        "type": "reasoning",
                        "id": "reasoning_item_1",
                        "summary": [{
                            "type": "summary_text",
                            "text": reasoning
                        }],
                        "status": "completed"
                    },
                    {
                        "type": "message",
                        "id": "msg_test",
                        "role": "assistant",
                        "status": "completed",
                        "content": [{
                            "type": "output_text",
                            "text": text,
                            "annotations": []
                        }]
                    }
                ],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 10,
                    "total_tokens": 20,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens_details": { "reasoning_tokens": 5 }
                }
            }
        })
        .to_string(),
    ));
    events.push(SseEvent::data("[DONE]"));
    events
}

/// SSE `event:` name and payload `type` of the non-standard doom-loop check
/// event (`echo_agent_sampling_types::DOOM_LOOP_CHECK_EVENT_TYPE`). Hardcoded
/// like every other wire string in this file; the shell integration tests
/// pin the two spellings against each other by absorbing built frames
/// through the real client.
const DOOM_LOOP_CHECK_EVENT: &str = "response.doom_loop_check";

/// One named `response.doom_loop_check` frame carrying the (cumulative)
/// trigger set, in the inference API's wire shape.
fn doom_loop_check_frame(triggers: &[&str], seq: u64) -> SseEvent {
    SseEvent::with_event(
        DOOM_LOOP_CHECK_EVENT,
        json!({
            "sequence_number": seq,
            "type": DOOM_LOOP_CHECK_EVENT,
            "doom_loop_check": { "triggers": triggers }
        })
        .to_string(),
    )
}

/// Inject `doom_loop_check.triggers` into a turn's terminal
/// `response.completed` object — the single home for terminal-field emission,
/// the dual of the mid-stream [`doom_loop_check_frame`]. Composes over any
/// turn builder (re-serialization may reorder JSON keys; clients and shape
/// tests parse, never byte-compare, these frames). Panics when the turn has
/// no completed frame: every builder emits one, so a miss is a script bug.
fn with_terminal_doom_loop_field(mut events: Vec<SseEvent>, triggers: &[&str]) -> Vec<SseEvent> {
    let patched = events.iter_mut().any(|e| {
        if e.data == "[DONE]" {
            return false;
        }
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&e.data) else {
            return false;
        };
        if value["type"] != "response.completed" {
            return false;
        }
        value["response"]["doom_loop_check"] = json!({ "triggers": triggers });
        e.data = value.to_string();
        true
    });
    assert!(
        patched,
        "turn builders always emit a response.completed frame"
    );
    events
}

/// Generate Responses API SSE events for a server-detected doom loop: a
/// reasoning-only stream (the doomed signature — the model loops in its
/// thinking and never answers) followed by named `response.doom_loop_check`
/// frames re-sent with the growing **cumulative** trigger set (one frame per
/// prefix of `triggers`, mirroring how the server re-emits as new triggers
/// appear), and a terminal `response.completed` whose response object carries
/// the full set under `doom_loop_check.triggers`.
///
/// Returns [`SseEvent`]s for direct use with [`crate::ScriptedResponse::sse`]
/// / `enqueue_response`, mirroring [`responses_api_reasoning_only_events`].
pub fn responses_api_doom_loop_check_events(
    triggers: &[&str],
    reasoning: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = responses_api_reasoning_only_events(reasoning, model);
    // Cumulative frames land between the deltas and the terminal event; the
    // frame seq roughly continues the stream (clients never validate it).
    for prefix_len in 1..=triggers.len() {
        let at = events.len() - 2;
        events.insert(
            at,
            doom_loop_check_frame(&triggers[..prefix_len], at as u64),
        );
    }
    with_terminal_doom_loop_field(events, triggers)
}

/// Generate Responses API SSE events for an ordinary reasoning + text turn
/// (mirroring [`responses_api_reasoning_and_text_events`]) whose terminal
/// `response.completed` object carries `doom_loop_check.triggers` with NO
/// mid-stream check frame — the terminal-only copy of the signal.
pub fn responses_api_doom_loop_terminal_only_events(
    triggers: &[&str],
    reasoning: &str,
    text: &str,
    model: &str,
) -> Vec<SseEvent> {
    with_terminal_doom_loop_field(
        responses_api_reasoning_and_text_events(reasoning, text, model),
        triggers,
    )
}

/// Splice ONE named `response.doom_loop_check` frame with an arbitrary
/// `data:` payload — a byte-exact wire fixture or a malformed variant — into
/// an otherwise-normal reasoning + text turn, right after `response.created`.
/// The payload's own `sequence_number` (if any) is its business; clients
/// never validate sequence continuity.
pub fn responses_api_with_doom_loop_frame(
    check_frame_data: &str,
    reasoning: &str,
    text: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = responses_api_reasoning_and_text_events(reasoning, text, model);
    events.insert(
        1,
        SseEvent::with_event(DOOM_LOOP_CHECK_EVENT, check_frame_data),
    );
    events
}

/// Replace the `output` list of a turn's terminal `response.completed` frame,
/// composing over any turn builder. The deltas the turn streamed are left
/// alone, so a caller can script a terminal shape that deliberately differs
/// from them — a reasoning item carrying `encrypted_content`, or a tool item
/// (`mcp_call`) the conversation form does not model.
pub fn with_terminal_output_items(
    mut events: Vec<SseEvent>,
    output: Vec<serde_json::Value>,
) -> Vec<SseEvent> {
    let at = completed_frame_index(&events);
    let mut value: serde_json::Value =
        serde_json::from_str(&events[at].data).expect("the completed frame is valid JSON");
    value["response"]["output"] = json!(output);
    events[at].data = value.to_string();
    events
}

/// Index of a turn's terminal `response.completed` frame.
fn completed_frame_index(events: &[SseEvent]) -> usize {
    events
        .iter()
        .position(|event| {
            serde_json::from_str::<serde_json::Value>(&event.data)
                .ok()
                .is_some_and(|value| value["type"] == "response.completed")
        })
        .expect("turn builders always emit a response.completed frame")
}

/// Splice ONE named `response.doom_loop_check` frame in just before the first
/// frame of `before_type`, composing over any turn builder. An armed client
/// observes the signal and aborts on that next frame, so the caller chooses
/// which frame the abort lands on — `response.function_call_arguments.delta`
/// to abort on tool activity, for instance. Panics when the turn has no such
/// frame, since that is a script bug.
pub fn with_doom_loop_frame_before_type(
    mut events: Vec<SseEvent>,
    check_frame_data: &str,
    before_type: &str,
) -> Vec<SseEvent> {
    let at = events
        .iter()
        .position(|event| {
            serde_json::from_str::<serde_json::Value>(&event.data)
                .ok()
                .is_some_and(|value| value["type"] == before_type)
        })
        .unwrap_or_else(|| panic!("the turn emits no {before_type} frame"));
    events.insert(
        at,
        SseEvent::with_event(DOOM_LOOP_CHECK_EVENT, check_frame_data),
    );
    events
}

/// Splice ONE named `response.doom_loop_check` frame in just before a turn's
/// terminal `response.completed`, composing over any turn builder (the
/// think-then-call turn, for instance). The frame is the last thing an armed
/// client sees before the terminal frame, so the signal lands with the turn's
/// items complete — the terminal-detection lane. Append a non-terminal event
/// after it (as
/// [`responses_api_with_doom_loop_frame_after_text`] does) to exercise the
/// mid-stream abort instead.
pub fn with_doom_loop_frame_before_completed(
    events: Vec<SseEvent>,
    check_frame_data: &str,
) -> Vec<SseEvent> {
    with_doom_loop_frame_before_type(events, check_frame_data, "response.completed")
}

/// A reasoning + text turn whose check frame arrives after all of its text,
/// followed by an empty typed delta — a non-terminal event that observes the
/// signal, so the mid-stream abort fires with the whole streamed turn already
/// captured. Exercises the exact text a client retains before detection.
pub fn responses_api_with_doom_loop_frame_after_text(
    check_frame_data: &str,
    reasoning: &str,
    text: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = with_doom_loop_frame_before_completed(
        responses_api_reasoning_and_text_events(reasoning, text, model),
        check_frame_data,
    );
    let at = completed_frame_index(&events);
    events.insert(
        at,
        SseEvent::data(
            json!({
                "type": "response.output_text.delta",
                "sequence_number": at,
                "item_id": "item_test",
                "output_index": 1,
                "content_index": 0,
                "delta": "",
                "logprobs": []
            })
            .to_string(),
        ),
    );
    events
}

/// Generate Responses API SSE events for a turn that streams reasoning
/// summary deltas FIRST and then issues one `function_call` — the shape a
/// reasoning-capable model produces when it thinks before its first tool
/// call. `response.completed` carries both output items (`reasoning` +
/// `function_call`) and no message, so the collector yields
/// `[Reasoning, ToolCall]` (tool calls keep the turn non-empty — no
/// `EmptyReason::ReasoningOnly` resample).
///
/// Returns [`SseEvent`]s for direct use with [`crate::ScriptedResponse::sse`]
/// / `enqueue_response`, mirroring [`responses_api_reasoning_only_events`].
pub fn responses_api_reasoning_then_tool_call_events(
    reasoning: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut seq = 0;

    // response.created
    events.push(SseEvent::data(
        json!({
            "type": "response.created",
            "sequence_number": seq,
            "response": {
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "model": model,
                "status": "in_progress",
                "output": []
            }
        })
        .to_string(),
    ));
    seq += 1;

    // Reasoning summary deltas stream before the tool call.
    for word in reasoning.split_whitespace() {
        events.push(SseEvent::data(
            json!({
                "type": "response.reasoning_summary_text.delta",
                "sequence_number": seq,
                "item_id": "reasoning_item_1",
                "output_index": 0,
                "summary_index": 0,
                "delta": format!("{word} ")
            })
            .to_string(),
        ));
        seq += 1;
    }

    // Then the tool invocation.
    events.push(SseEvent::data(
        json!({
            "type": "response.function_call_arguments.delta",
            "sequence_number": seq,
            "item_id": call_id,
            "output_index": 1,
            "delta": arguments
        })
        .to_string(),
    ));
    seq += 1;

    // response.completed: the reasoning item plus the function_call item.
    events.push(SseEvent::data(
        json!({
            "type": "response.completed",
            "sequence_number": seq,
            "response": {
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "model": model,
                "status": "completed",
                "output": [
                    {
                        "type": "reasoning",
                        "id": "reasoning_item_1",
                        "summary": [{
                            "type": "summary_text",
                            "text": reasoning
                        }],
                        "status": "completed"
                    },
                    {
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments
                    }
                ],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 20,
                    "total_tokens": 30,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens_details": { "reasoning_tokens": 5 }
                }
            }
        })
        .to_string(),
    ));
    events.push(SseEvent::data("[DONE]"));
    events
}

/// Chat Completions twin of [`responses_api_reasoning_then_tool_call_events`]:
/// `reasoning_content` deltas, then one `tool_calls` delta, then a
/// `finish_reason: "tool_calls"` chunk with usage.
pub fn chat_completions_reasoning_then_tool_call_events(
    reasoning: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = Vec::new();
    for word in reasoning.split_whitespace() {
        events.push(SseEvent::data(
            json!({
                "id": "chatcmpl-test",
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": { "reasoning_content": format!("{word} ") },
                    "finish_reason": null
                }]
            })
            .to_string(),
        ));
    }
    events.push(SseEvent::data(
        json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "index": 0,
                        "id": call_id,
                        "type": "function",
                        "function": { "name": name, "arguments": arguments }
                    }]
                },
                "finish_reason": null
            }]
        })
        .to_string(),
    ));
    events.push(SseEvent::data(
        json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        })
        .to_string(),
    ));
    events.push(SseEvent::data("[DONE]"));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both byte-exact delta encoders must reconstruct a multi-line response
    /// (incl. a ```mermaid fence) byte-for-byte. This is load-bearing:
    /// `split_whitespace` would collapse the fence's newlines onto one line,
    /// so a client would never parse it as a code block and diagram detection
    /// would silently fail.
    #[test]
    fn deltas_reconstruct_multiline_response_byte_for_byte() {
        let text = "Here is a flow:\n\n```mermaid\nflowchart TD\n  A --> B\n  B --> C\n```\n\nDone rendering.\n";

        assert_eq!(chat_completion_deltas(text).concat(), text);
        assert_eq!(responses_api_deltas(text).concat(), text);

        // The reconstruction preserves the fence as a real, newline-delimited
        // code block (the property diagram detection depends on).
        assert!(
            chat_completion_deltas(text)
                .concat()
                .contains("```mermaid\nflowchart TD\n")
        );
    }

    /// Multiple consecutive spaces and a trailing newline survive too (no
    /// `split_whitespace`-style collapsing).
    #[test]
    fn deltas_preserve_runs_of_whitespace() {
        let text = "a  b\tc\n";
        assert_eq!(chat_completion_deltas(text).concat(), text);
        assert_eq!(responses_api_deltas(text).concat(), text);
    }

    /// Shape guard for the reasoning-only builder: parse each event back to JSON
    /// and assert the structural tags/fields the shell collector keys on — at
    /// least one `response.reasoning_summary_text.delta` carrying text, no
    /// `response.output_text.delta`, and a `response.completed` whose output
    /// holds a `reasoning` item (with summary text) and no `message` item,
    /// terminated by `[DONE]`. A full round-trip through `rs::ResponseStreamEvent`
    /// would pin the async-openai types directly, but that crate is not a
    /// dependency here; the integration test deserializes these events through
    /// the real client, covering the wire contract end-to-end.
    #[test]
    fn reasoning_only_events_carry_reasoning_and_no_output_text() {
        let events = responses_api_reasoning_only_events("alpha beta gamma", "m");
        assert_eq!(events.last().map(|e| e.data.as_str()), Some("[DONE]"));

        // Parse every non-terminal event into JSON and key off the `type` tag.
        let parsed: Vec<serde_json::Value> = events
            .iter()
            .filter(|e| e.data != "[DONE]")
            .map(|e| serde_json::from_str(&e.data).expect("each event is valid JSON"))
            .collect();
        let types: Vec<&str> = parsed
            .iter()
            .map(|v| v["type"].as_str().expect("each event has a type tag"))
            .collect();

        let reasoning_delta = parsed
            .iter()
            .find(|v| v["type"] == "response.reasoning_summary_text.delta")
            .expect("must stream a reasoning summary delta");
        assert!(
            !reasoning_delta["delta"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "the reasoning delta must carry text"
        );
        assert!(
            !types.contains(&"response.output_text.delta"),
            "reasoning-only must not stream output text"
        );

        let completed = parsed
            .iter()
            .find(|v| v["type"] == "response.completed")
            .expect("must emit a completed event");
        let output = completed["response"]["output"]
            .as_array()
            .expect("completed carries an output array");
        let reasoning_item = output
            .iter()
            .find(|o| o["type"] == "reasoning")
            .expect("completed output must carry a reasoning item");
        assert!(
            !reasoning_item["summary"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "the reasoning item must carry summary text"
        );
        assert!(
            !output.iter().any(|o| o["type"] == "message"),
            "completed output must have no message item (no visible text)"
        );
    }

    /// Shape guard for the reasoning+text builder: reasoning summary deltas
    /// stream before the output-text deltas, and `response.completed` carries
    /// BOTH a `reasoning` item and a `message` item, terminated by `[DONE]` —
    /// the ordinary reasoning-model turn (never `EmptyReason::ReasoningOnly`).
    #[test]
    fn reasoning_and_text_events_carry_both_items() {
        let events = responses_api_reasoning_and_text_events("alpha beta", "the answer", "m");
        assert_eq!(events.last().map(|e| e.data.as_str()), Some("[DONE]"));

        let parsed: Vec<serde_json::Value> = events
            .iter()
            .filter(|e| e.data != "[DONE]")
            .map(|e| serde_json::from_str(&e.data).expect("each event is valid JSON"))
            .collect();
        let types: Vec<&str> = parsed
            .iter()
            .map(|v| v["type"].as_str().expect("each event has a type tag"))
            .collect();

        // Reasoning streams strictly before the visible answer.
        let first_reasoning = types
            .iter()
            .position(|t| *t == "response.reasoning_summary_text.delta")
            .expect("must stream reasoning summary deltas");
        let first_text = types
            .iter()
            .position(|t| *t == "response.output_text.delta")
            .expect("must stream output text deltas");
        assert!(
            first_reasoning < first_text,
            "reasoning deltas must precede text deltas"
        );

        let completed = parsed
            .iter()
            .find(|v| v["type"] == "response.completed")
            .expect("must emit a completed event");
        let output = completed["response"]["output"]
            .as_array()
            .expect("completed carries an output array");
        assert_eq!(
            output[0]["summary"][0]["text"].as_str(),
            Some("alpha beta"),
            "completed output must carry the reasoning item first"
        );
        assert_eq!(
            output[1]["content"][0]["text"].as_str(),
            Some("the answer"),
            "completed output must carry the assistant message"
        );
    }

    /// Shape guard for the reasoning+tool-call builder: reasoning summary
    /// deltas stream before the function-call args delta, no output text
    /// anywhere, and `response.completed` carries a `reasoning` item plus a
    /// `function_call` item (no `message`), terminated by `[DONE]` — the
    /// think-then-call turn whose tool call keeps it non-empty.
    #[test]
    fn reasoning_then_tool_call_events_carry_reasoning_and_function_call() {
        let events = responses_api_reasoning_then_tool_call_events(
            "alpha beta",
            "call_1",
            "read_file",
            "{\"target_file\":\"a.rs\"}",
            "m",
        );
        assert_eq!(events.last().map(|e| e.data.as_str()), Some("[DONE]"));

        let parsed: Vec<serde_json::Value> = events
            .iter()
            .filter(|e| e.data != "[DONE]")
            .map(|e| serde_json::from_str(&e.data).expect("each event is valid JSON"))
            .collect();
        let types: Vec<&str> = parsed
            .iter()
            .map(|v| v["type"].as_str().expect("each event has a type tag"))
            .collect();

        // Reasoning streams strictly before the tool invocation; no text.
        let first_reasoning = types
            .iter()
            .position(|t| *t == "response.reasoning_summary_text.delta")
            .expect("must stream reasoning summary deltas");
        let args_delta = types
            .iter()
            .position(|t| *t == "response.function_call_arguments.delta")
            .expect("must stream a function-call arguments delta");
        assert!(
            first_reasoning < args_delta,
            "reasoning deltas must precede the tool call"
        );
        assert!(
            !types.contains(&"response.output_text.delta"),
            "a think-then-call turn must not stream output text"
        );

        let completed = parsed
            .iter()
            .find(|v| v["type"] == "response.completed")
            .expect("must emit a completed event");
        let output = completed["response"]["output"]
            .as_array()
            .expect("completed carries an output array");
        assert_eq!(
            output[0]["summary"][0]["text"].as_str(),
            Some("alpha beta"),
            "completed output must carry the reasoning item first"
        );
        assert_eq!(output[1]["type"].as_str(), Some("function_call"));
        assert_eq!(output[1]["call_id"].as_str(), Some("call_1"));
        assert_eq!(output[1]["name"].as_str(), Some("read_file"));
        assert!(
            !output.iter().any(|o| o["type"] == "message"),
            "completed output must have no message item (no visible text)"
        );
    }

    /// Shape guard for the Chat Completions twin: `reasoning_content` deltas
    /// stream first, then exactly one `tool_calls` delta carrying the call
    /// id/name/arguments, then a `finish_reason: "tool_calls"` chunk, with no
    /// visible `content` anywhere, terminated by `[DONE]`.
    #[test]
    fn chat_reasoning_then_tool_call_events_carry_reasoning_then_tool_call() {
        let events = chat_completions_reasoning_then_tool_call_events(
            "alpha beta",
            "call_1",
            "read_file",
            "{\"target_file\":\"a.rs\"}",
            "m",
        );
        assert_eq!(events.last().map(|e| e.data.as_str()), Some("[DONE]"));

        let parsed: Vec<serde_json::Value> = events
            .iter()
            .filter(|e| e.data != "[DONE]")
            .map(|e| serde_json::from_str(&e.data).expect("each event is valid JSON"))
            .collect();
        let delta_at = |v: &serde_json::Value| v["choices"][0]["delta"].clone();

        let first_reasoning = parsed
            .iter()
            .position(|v| !delta_at(v)["reasoning_content"].is_null())
            .expect("must stream reasoning_content deltas");
        let tool_call = parsed
            .iter()
            .position(|v| !delta_at(v)["tool_calls"].is_null())
            .expect("must stream a tool_calls delta");
        assert!(
            first_reasoning < tool_call,
            "reasoning deltas must precede the tool call"
        );
        let call = delta_at(&parsed[tool_call])["tool_calls"][0].clone();
        assert_eq!(call["id"].as_str(), Some("call_1"));
        assert_eq!(call["function"]["name"].as_str(), Some("read_file"));
        assert!(
            parsed.iter().all(|v| delta_at(v)["content"]
                .as_str()
                .unwrap_or_default()
                .is_empty()),
            "a think-then-call turn must not stream visible content"
        );
        assert!(
            parsed
                .iter()
                .any(|v| v["choices"][0]["finish_reason"] == "tool_calls"),
            "the stream must finish with finish_reason tool_calls"
        );
    }

    /// Shape guard for the doom-loop builder: one NAMED check frame per
    /// cumulative prefix of `triggers` (each frame re-sends every trigger so
    /// far), a reasoning-only output (no message item — the doomed
    /// signature), and the terminal response object carrying the full set.
    #[test]
    fn doom_loop_check_events_send_growing_named_frames_and_terminal_field() {
        let events = responses_api_doom_loop_check_events(
            &["tail_repetition:4@response", "tail_repetition:2@response"],
            "looping thought",
            "m",
        );
        assert_eq!(events.last().map(|e| e.data.as_str()), Some("[DONE]"));

        let frames: Vec<&SseEvent> = events
            .iter()
            .filter(|e| e.event.as_deref() == Some(DOOM_LOOP_CHECK_EVENT))
            .collect();
        assert_eq!(frames.len(), 2, "one frame per cumulative prefix");
        let first: serde_json::Value = serde_json::from_str(&frames[0].data).unwrap();
        assert_eq!(first["type"], DOOM_LOOP_CHECK_EVENT);
        assert!(first["sequence_number"].is_u64());
        assert_eq!(
            first["doom_loop_check"]["triggers"],
            json!(["tail_repetition:4@response"])
        );
        let second: serde_json::Value = serde_json::from_str(&frames[1].data).unwrap();
        assert_eq!(
            second["doom_loop_check"]["triggers"],
            json!(["tail_repetition:4@response", "tail_repetition:2@response"])
        );

        let completed = events
            .iter()
            .filter(|e| e.data != "[DONE]")
            .map(|e| serde_json::from_str::<serde_json::Value>(&e.data).unwrap())
            .find(|v| v["type"] == "response.completed")
            .expect("must emit a completed event");
        assert_eq!(
            completed["response"]["doom_loop_check"]["triggers"],
            json!(["tail_repetition:4@response", "tail_repetition:2@response"])
        );
        let output = completed["response"]["output"].as_array().unwrap();
        assert!(
            !output.iter().any(|o| o["type"] == "message"),
            "a doomed turn is reasoning-only (no message item)"
        );
    }

    /// Shape guard for the terminal-only variant: no named check frame
    /// anywhere; the completed response carries both output items (the turn
    /// is a normal answer) plus `doom_loop_check.triggers`.
    #[test]
    fn doom_loop_terminal_only_events_carry_field_without_mid_stream_frame() {
        let events = responses_api_doom_loop_terminal_only_events(
            &["low_logprob@thinking"],
            "brief thought",
            "the answer",
            "m",
        );
        assert!(
            events.iter().all(|e| e.event.is_none()),
            "terminal-only variant must not emit a named check frame"
        );

        let completed = events
            .iter()
            .filter(|e| e.data != "[DONE]")
            .map(|e| serde_json::from_str::<serde_json::Value>(&e.data).unwrap())
            .find(|v| v["type"] == "response.completed")
            .expect("must emit a completed event");
        assert_eq!(
            completed["response"]["doom_loop_check"]["triggers"],
            json!(["low_logprob@thinking"])
        );
        let output = completed["response"]["output"].as_array().unwrap();
        assert!(output.iter().any(|o| o["type"] == "message"));
        assert!(output.iter().any(|o| o["type"] == "reasoning"));
    }

    /// Shape guard for the splice helper: the named frame lands right after
    /// `response.created` with the caller's payload byte-for-byte (this is
    /// how byte-exact fixtures and malformed variants ride a normal turn).
    #[test]
    fn with_doom_loop_frame_splices_payload_verbatim() {
        let payload = r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":42}}"#;
        let events = responses_api_with_doom_loop_frame(payload, "hm", "hi", "m");
        assert_eq!(events[1].event.as_deref(), Some(DOOM_LOOP_CHECK_EVENT));
        assert_eq!(events[1].data, payload);
        let created: serde_json::Value = serde_json::from_str(&events[0].data).unwrap();
        assert_eq!(created["type"], "response.created");
    }

    /// Shape guard for the positional composer: the named frame lands
    /// immediately before the first frame of the requested type, so an armed
    /// client aborts on that frame.
    #[test]
    fn with_doom_loop_frame_before_type_lands_before_the_named_frame() {
        let payload = r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":["tail_repetition:8@thinking"]}}"#;
        let events = with_doom_loop_frame_before_type(
            responses_api_reasoning_then_tool_call_events("hm", "call-1", "read_file", "{}", "m"),
            payload,
            "response.function_call_arguments.delta",
        );

        let at = events
            .iter()
            .position(|e| e.event.as_deref() == Some(DOOM_LOOP_CHECK_EVENT))
            .expect("the named frame is spliced in");
        assert_eq!(events[at].data, payload);
        let next: serde_json::Value = serde_json::from_str(&events[at + 1].data).unwrap();
        assert_eq!(next["type"], "response.function_call_arguments.delta");
    }

    /// Shape guard for the terminal-output composer: the completed frame
    /// carries exactly the caller's items (including wire shapes the
    /// conversation form does not model), the rest of the frame survives, and
    /// the streamed deltas are untouched.
    #[test]
    fn with_terminal_output_items_replaces_only_the_completed_output() {
        let events = with_terminal_output_items(
            responses_api_reasoning_and_text_events("thinking", "the answer", "m"),
            vec![json!({
                "type": "mcp_call",
                "id": "mcp-1",
                "name": "search",
                "server_label": "docs",
                "arguments": "{}"
            })],
        );

        let parsed: Vec<serde_json::Value> = events
            .iter()
            .filter(|e| e.data != "[DONE]")
            .map(|e| serde_json::from_str(&e.data).expect("each event is valid JSON"))
            .collect();
        assert!(
            parsed
                .iter()
                .any(|v| v["type"] == "response.output_text.delta"),
            "the streamed deltas are left alone"
        );
        let completed = parsed
            .iter()
            .find(|v| v["type"] == "response.completed")
            .expect("must emit a completed event");
        let output = completed["response"]["output"].as_array().unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["type"], "mcp_call");
        assert_eq!(
            completed["response"]["model"], "m",
            "the rest of the frame survives"
        );
    }

    /// Shape guard for the terminal-side composer: the caller's payload rides
    /// verbatim in the slot immediately before `response.completed`, over an
    /// arbitrary turn builder (here the think-then-call turn).
    #[test]
    fn with_doom_loop_frame_before_completed_lands_last_before_the_terminal_frame() {
        let payload = r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":["tail_repetition:8@thinking"]}}"#;
        let events = with_doom_loop_frame_before_completed(
            responses_api_reasoning_then_tool_call_events("hm", "call-1", "read_file", "{}", "m"),
            payload,
        );

        let at = events
            .iter()
            .position(|e| e.event.as_deref() == Some(DOOM_LOOP_CHECK_EVENT))
            .expect("the named frame is spliced in");
        assert_eq!(events[at].data, payload);
        let next: serde_json::Value = serde_json::from_str(&events[at + 1].data).unwrap();
        assert_eq!(
            next["type"], "response.completed",
            "the frame is the last event before the terminal frame"
        );
        let output = next["response"]["output"].as_array().unwrap();
        assert!(
            output.iter().any(|o| o["type"] == "function_call"),
            "the composed turn keeps its tool call"
        );
    }

    /// Shape guard for the mid-stream variant: the check frame follows every
    /// text delta and is itself followed by one empty typed delta — the
    /// non-terminal event an armed client aborts on — before the terminal
    /// frame.
    #[test]
    fn doom_loop_frame_after_text_is_followed_by_an_empty_delta() {
        let payload = r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":["tail_repetition:8@thinking"]}}"#;
        let events =
            responses_api_with_doom_loop_frame_after_text(payload, "hm", "the answer", "m");

        let at = events
            .iter()
            .position(|e| e.event.as_deref() == Some(DOOM_LOOP_CHECK_EVENT))
            .expect("the named frame is spliced in");
        let text_delta_before = events[..at]
            .iter()
            .filter(|e| e.data != "[DONE]")
            .filter_map(|e| serde_json::from_str::<serde_json::Value>(&e.data).ok())
            .any(|v| v["type"] == "response.output_text.delta");
        assert!(
            text_delta_before,
            "the frame arrives after the turn's visible text"
        );

        let next: serde_json::Value = serde_json::from_str(&events[at + 1].data).unwrap();
        assert_eq!(next["type"], "response.output_text.delta");
        assert_eq!(next["delta"], "", "the abort rides an empty typed delta");
        let terminal: serde_json::Value = serde_json::from_str(&events[at + 2].data).unwrap();
        assert_eq!(terminal["type"], "response.completed");
    }
}
