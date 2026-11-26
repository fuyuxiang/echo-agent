//! Integration tests for the sampling client with mock HTTP servers.
//!
//! These tests verify the full streaming flow for both:
//! - Chat Completions API (`/v1/chat/completions`)
//! - Responses API (`/v1/responses`)
//!
//! Each test spawns a temporary mock server that returns SSE streams,
//! allowing us to test the client without real API credentials.

use futures_util::stream::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};

use echo_agent_runtime::sampling::{
    ApiBackend, Client, ConversationItem, ConversationRequest, ConversationToolChoice,
    SamplingError, ToolCall, ToolSpec, rs,
};
use echo_agent_runtime::session::storage::JsonlStorageAdapter;
use echo_agent_test_support::sse::responses_api_reasoning_and_text_events;
use echo_agent_test_support::{MockInferenceServer, ScriptedResponse, SseEvent};

mod common;

use common::{create_test_client, create_test_client_with_extra_headers, test_sampler_config};

// ============================================================================
// Mock Response Generators
// ============================================================================

/// Generate a Chat Completions SSE stream with tool calls.
fn chat_completion_tool_call_stream(tool_calls: Vec<Value>, model: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();

    // First chunk with role
    let first_chunk = json!({
        "id": "chatcmpl-test123",
        "object": "chat.completion.chunk",
        "created": 1234567890,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "content": null,
                "tool_calls": tool_calls
            },
            "finish_reason": null
        }]
    });
    events.push(SseEvent::data(first_chunk.to_string()));

    // Final chunk with finish reason
    let final_chunk = json!({
        "id": "chatcmpl-test123",
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
    });
    events.push(SseEvent::data(final_chunk.to_string()));
    events.push(SseEvent::data("[DONE]"));

    events
}

/// Generate a Chat Completions SSE stream with reasoning content.
fn chat_completion_with_reasoning_stream(
    reasoning: &str,
    content: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = Vec::new();

    // Reasoning chunks
    for word in reasoning.split_whitespace() {
        let chunk = json!({
            "id": "chatcmpl-test123",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {
                    "reasoning_content": format!("{} ", word)
                },
                "finish_reason": null
            }]
        });
        events.push(SseEvent::data(chunk.to_string()));
    }

    // Content chunks
    for word in content.split_whitespace() {
        let chunk = json!({
            "id": "chatcmpl-test123",
            "object": "chat.completion.chunk",
            "created": 1234567890,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {
                    "content": format!("{} ", word)
                },
                "finish_reason": null
            }]
        });
        events.push(SseEvent::data(chunk.to_string()));
    }

    // Final chunk
    let final_chunk = json!({
        "id": "chatcmpl-test123",
        "object": "chat.completion.chunk",
        "created": 1234567890,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 20,
            "total_tokens": 30
        }
    });
    events.push(SseEvent::data(final_chunk.to_string()));
    events.push(SseEvent::data("[DONE]"));

    events
}

/// Generate a Responses API SSE stream with function calls.
fn responses_api_tool_call_stream(
    call_id: &str,
    name: &str,
    arguments: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut seq = 0;

    // response.created event
    let created = json!({
        "type": "response.created",
        "sequence_number": seq,
        "response": {
            "id": "resp_test123",
            "object": "response",
            "created_at": 1234567890,
            "model": model,
            "status": "in_progress",
            "output": []
        }
    });
    events.push(SseEvent::data(created.to_string()));
    seq += 1;

    // Function call arguments delta
    let args_delta = json!({
        "type": "response.function_call_arguments.delta",
        "sequence_number": seq,
        "item_id": call_id,
        "output_index": 0,
        "delta": arguments
    });
    events.push(SseEvent::data(args_delta.to_string()));
    seq += 1;

    // response.completed event with function call
    let completed = json!({
        "type": "response.completed",
        "sequence_number": seq,
        "response": {
            "id": "resp_test123",
            "object": "response",
            "created_at": 1234567890,
            "model": model,
            "status": "completed",
            "output": [{
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": arguments
            }],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20,
                "total_tokens": 30,
                "input_tokens_details": {
                    "cached_tokens": 0
                },
                "output_tokens_details": {
                    "reasoning_tokens": 0
                }
            }
        }
    });
    events.push(SseEvent::data(completed.to_string()));
    events.push(SseEvent::data("[DONE]"));

    events
}

// ============================================================================
// Chat Completions API Tests
// ============================================================================

#[tokio::test]
async fn test_chat_completions_streaming_text() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("Hello world from the assistant!");
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let request = ConversationRequest::from_items(vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Say hello"),
    ]);

    let (mut stream, _metadata) = client.conversation_stream(request).await.unwrap();

    let mut content = String::new();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        for choice in chunk.choices {
            if let Some(ref text) = choice.delta.content {
                content.push_str(text);
            }
        }
    }

    assert_eq!(content, "Hello world from the assistant!");
}

#[tokio::test]
async fn test_chat_completions_streaming_tool_calls() {
    let tool_calls = vec![json!({
        "id": "call_abc123",
        "type": "function",
        "function": {
            "name": "read_file",
            "arguments": r#"{"path": "/test.txt"}"#
        }
    })];

    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::sse(chat_completion_tool_call_stream(
            tool_calls,
            "echo-agent-test",
        )),
    );
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Read a file")])
        .with_tools(vec![ToolSpec {
            name: "read_file".to_string(),
            description: Some("Read a file".to_string()),
            parameters: json!({"type": "object"}),
        }]);

    let (mut stream, _metadata) = client.conversation_stream(request).await.unwrap();

    let mut tool_calls_received = Vec::new();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        for choice in chunk.choices {
            for tc in choice.delta.tool_calls {
                tool_calls_received.push(tc);
            }
        }
    }

    assert_eq!(tool_calls_received.len(), 1);
    assert_eq!(
        tool_calls_received[0]
            .function
            .as_ref()
            .and_then(|f| f.name.as_deref()),
        Some("read_file")
    );
}

#[tokio::test]
async fn test_chat_completions_with_reasoning() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::sse(chat_completion_with_reasoning_stream(
            "Let me think about this",
            "The answer is 42",
            "echo-agent-test",
        )),
    );
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let request = ConversationRequest::from_items(vec![ConversationItem::user(
        "What is the meaning of life?",
    )]);

    let (mut stream, _metadata) = client.conversation_stream(request).await.unwrap();

    let mut content = String::new();
    let mut reasoning = String::new();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        for choice in chunk.choices {
            if let Some(ref text) = choice.delta.content {
                content.push_str(text);
            }
            if let Some(ref thought) = choice.delta.reasoning_content {
                reasoning.push_str(thought);
            }
        }
    }

    assert!(reasoning.contains("Let me think"));
    assert!(content.contains("42"));
}

// ============================================================================
// Reasoning-as-sibling — chat completions, both paths
// ============================================================================

/// All-new path: a chat-completions stream carrying `reasoning_content`
/// deltas must be collected into a sibling `ConversationItem::Reasoning`
/// that *precedes* the assistant — the exact shape currently persisted to
/// chat_history.jsonl. Unlike `test_chat_completions_with_reasoning`
/// (which only inspects raw SSE deltas), this drives the high-level
/// `conversation_collect` path so it exercises
/// stream_chat_completions → collect_response → `ConversationResponse.items`.
#[tokio::test]
async fn chat_completions_collect_synthesizes_reasoning_sibling() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::sse(chat_completion_with_reasoning_stream(
            "Let me think about this",
            "The answer is 42",
            "echo-agent-test",
        )),
    );
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let request = ConversationRequest::from_items(vec![ConversationItem::user(
        "What is the meaning of life?",
    )]);

    let response = client.conversation_collect(request).await.unwrap();

    // Items must be exactly [Reasoning, Assistant] — the sibling shape.
    assert!(
        matches!(
            response.items.as_slice(),
            [
                ConversationItem::Reasoning(_),
                ConversationItem::Assistant(_)
            ]
        ),
        "expected [Reasoning, Assistant], got {:?}",
        response.items
    );

    let reasoning = response
        .reasoning_items()
        .next()
        .expect("a reasoning sibling must be synthesized from reasoning_content");
    let rs::SummaryPart::SummaryText(summary) = &reasoning.summary[0];
    assert!(
        summary.text.contains("Let me think"),
        "reasoning sibling should carry the streamed reasoning_content, got: {:?}",
        summary.text
    );

    let assistant = response.assistant().expect("assistant item present");
    assert!(
        assistant.content.contains("42"),
        "assistant content should carry the streamed text, got: {:?}",
        assistant.content
    );
}

/// Upgrade path: a legacy chat-completions session on disk —
/// an assistant carrying inline `reasoning: {text}` — must, when loaded,
/// reconstruct a sibling Reasoning item, which then folds into
/// `reasoning_content` on the *correct* assistant message in the outgoing
/// chat-completions request body. This ties the whole chain together:
/// read_chat_history_sync (upgrade_legacy_reasoning) → ConversationRequest →
/// `From<ConversationRequest> for ChatCompletionRequest`
/// (conversation_to_chat_messages) → wire.
#[tokio::test]
async fn chat_completions_upgrade_folds_reconstructed_reasoning_into_request() {
    // 1. Seed a legacy chat-completions chat_history.jsonl (inline reasoning
    //    on the assistant — the shape an older binary wrote).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("chat_history.jsonl"),
        concat!(
            r#"{"type":"system","content":"You are helpful."}"#,
            "\n",
            r#"{"type":"user","content":[{"type":"text","text":"q1"}]}"#,
            "\n",
            r#"{"type":"assistant","content":"a1","reasoning":{"text":"legacy chain of thought","id":"rs_legacy"}}"#,
            "\n",
        ),
    )
    .unwrap();

    // 2. Load through the real adapter — applies the in-memory upgrade.
    let adapter = JsonlStorageAdapter::with_root(dir.path().to_path_buf());
    let mut items = adapter.load_chat_history_from_dir(dir.path()).unwrap();
    assert!(
        items
            .iter()
            .any(|i| matches!(i, ConversationItem::Reasoning(_))),
        "legacy inline reasoning must be reconstructed as a sibling on load, got {:?}",
        items
    );

    // 3. Continue the conversation and send it over chat-completions,
    //    capturing the outgoing request body.
    items.push(ConversationItem::user("q2"));

    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("ok");
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let _ = client
        .conversation_collect(ConversationRequest::from_items(items))
        .await
        .unwrap();

    // 4. The reconstructed reasoning must land on the assistant's
    //    reasoning_content in the wire request — not be dropped.
    let body = server.request_bodies().pop().unwrap();
    let messages = body.get("messages").unwrap().as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("assistant message present in request");
    assert_eq!(
        assistant.get("reasoning_content").and_then(Value::as_str),
        Some("legacy chain of thought"),
        "reconstructed reasoning must fold onto the assistant's reasoning_content; \
         full messages: {messages:#?}"
    );
}

/// Upgrade path, echo-agent-build / Responses API: a legacy session whose
/// assistant carries inline `reasoning: {text, encrypted, id}` must, on
/// load, reconstruct a sibling Reasoning item that round-trips back to
/// the Responses API as a **typed** `reasoning` input item — `summary`,
/// `encrypted_content`, and `id` all preserved — NOT flattened to a
/// string. This is the byte-stable SGLang-prefix path; it must not go
/// through `reasoning_item_text`.
#[tokio::test]
async fn responses_upgrade_roundtrips_reconstructed_reasoning_as_typed_input() {
    // 1. Seed a legacy echo-agent-build chat_history.jsonl (inline reasoning
    //    with encrypted_content + id — the older shape).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("chat_history.jsonl"),
        concat!(
            r#"{"type":"system","content":"You are helpful."}"#,
            "\n",
            r#"{"type":"user","content":[{"type":"text","text":"q1"}]}"#,
            "\n",
            r#"{"type":"assistant","content":"a1","reasoning":{"text":"legacy echo-agent-build reasoning","encrypted":"ENC_BLOB_xyz","id":"rs_echo_agentbuild_legacy"},"model_id":"echo-agent-build"}"#,
            "\n",
        ),
    )
    .unwrap();

    // 2. Load through the real adapter — applies the upgrade.
    let adapter = JsonlStorageAdapter::with_root(dir.path().to_path_buf());
    let mut items = adapter.load_chat_history_from_dir(dir.path()).unwrap();
    assert!(
        items
            .iter()
            .any(|i| matches!(i, ConversationItem::Reasoning(_))),
        "legacy inline reasoning must be reconstructed as a sibling on load, got {items:?}"
    );

    // 3. Continue and send over the Responses API, capturing the body.
    items.push(ConversationItem::user("q2"));

    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("ok");
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let _ = client
        .conversation_collect(ConversationRequest::from_items(items))
        .await
        .unwrap();

    // 4. The reconstructed reasoning must appear as a typed `reasoning`
    //    input item with its fields preserved verbatim — the byte-stable
    //    typed round-trip, not a flattened string.
    let body = server.request_bodies().pop().unwrap();
    let input = body.get("input").unwrap().as_array().unwrap();
    let reasoning = input
        .iter()
        .find(|i| i.get("type").and_then(Value::as_str) == Some("reasoning"))
        .unwrap_or_else(|| {
            panic!("reconstructed reasoning must round-trip as a typed Responses input item; input: {input:#?}")
        });
    assert_eq!(
        reasoning.get("id").and_then(Value::as_str),
        Some("rs_echo_agentbuild_legacy"),
        "reasoning id preserved"
    );
    assert_eq!(
        reasoning.get("encrypted_content").and_then(Value::as_str),
        Some("ENC_BLOB_xyz"),
        "encrypted_content preserved verbatim (this is what restores exact tokens server-side)"
    );
    let summary = reasoning.get("summary").and_then(Value::as_array).unwrap();
    assert_eq!(
        summary[0].get("type").and_then(Value::as_str),
        Some("summary_text")
    );
    assert_eq!(
        summary[0].get("text").and_then(Value::as_str),
        Some("legacy echo-agent-build reasoning")
    );
}

/// Upgrade path, Anthropic Messages API: a legacy session whose assistant
/// carries inline `reasoning: {text, encrypted, id}` (text = thinking,
/// encrypted = signature) must, on load, reconstruct a sibling Reasoning
/// item that emits a Anthropic Messages `thinking` content block (with `thinking`
/// + `signature`) on the outgoing `/v1/messages` request.
#[tokio::test]
async fn messages_upgrade_emits_reconstructed_reasoning_as_thinking_block() {
    // 1. Seed a legacy Anthropic Messages-origin chat_history.jsonl. Anthropic Messages
    //    thinking blocks never carried an id (stream/messages.rs sets
    //    id=""), and the signature lives in `encrypted`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("chat_history.jsonl"),
        concat!(
            r#"{"type":"system","content":"You are helpful."}"#,
            "\n",
            r#"{"type":"user","content":[{"type":"text","text":"q1"}]}"#,
            "\n",
            r#"{"type":"assistant","content":"a1","reasoning":{"text":"legacy anthropic thinking","encrypted":"SIGNATURE_abc","id":""},"model_id":"echo-agent-4.5"}"#,
            "\n",
        ),
    )
    .unwrap();

    // 2. Load + upgrade.
    let adapter = JsonlStorageAdapter::with_root(dir.path().to_path_buf());
    let mut items = adapter.load_chat_history_from_dir(dir.path()).unwrap();
    assert!(
        items
            .iter()
            .any(|i| matches!(i, ConversationItem::Reasoning(_))),
        "legacy inline reasoning must be reconstructed as a sibling on load, got {items:?}"
    );

    // 3. Continue and send over the Messages API, capturing the body.
    items.push(ConversationItem::user("q2"));

    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("ok");
    let client = create_test_client(&server.url(), ApiBackend::Messages);

    let _ = client
        .conversation_collect(ConversationRequest::from_items(items))
        .await
        .unwrap();

    // 4. The reconstructed reasoning must emit a Anthropic Messages `thinking`
    //    content block carrying the thinking text + signature.
    let body = server.request_bodies().pop().unwrap();
    let messages = body.get("messages").unwrap().as_array().unwrap();
    let thinking_block = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .flat_map(|m| {
            m.get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .find(|b| b.get("type").and_then(Value::as_str) == Some("thinking"))
        .unwrap_or_else(|| {
            panic!("reconstructed reasoning must emit an Anthropic thinking block; messages: {messages:#?}")
        });
    assert_eq!(
        thinking_block.get("thinking").and_then(Value::as_str),
        Some("legacy anthropic thinking"),
        "thinking text preserved"
    );
    assert_eq!(
        thinking_block.get("signature").and_then(Value::as_str),
        Some("SIGNATURE_abc"),
        "signature (encrypted) preserved — required to reuse the thought server-side"
    );
}

// ============================================================================
// Responses API Tests
// ============================================================================

/// Generate a Responses API SSE stream with reasoning (including encrypted content).
fn responses_api_with_reasoning_stream(
    reasoning_summary: &str,
    encrypted_content: Option<&str>,
    content: &str,
    model: &str,
) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut seq = 0;

    // response.created event
    let created = json!({
        "type": "response.created",
        "sequence_number": seq,
        "response": {
            "id": "resp_reason123",
            "object": "response",
            "created_at": 1234567890,
            "model": model,
            "status": "in_progress",
            "output": []
        }
    });
    events.push(SseEvent::data(created.to_string()));
    seq += 1;

    // Build reasoning output item
    let mut reasoning_item = json!({
        "type": "reasoning",
        "id": "reasoning_item_1",
        "summary": [{
            "type": "summary_text",
            "text": reasoning_summary
        }],
        "status": "completed"
    });

    if let Some(enc) = encrypted_content {
        reasoning_item["encrypted_content"] = json!(enc);
    }

    // response.completed event with reasoning
    let completed = json!({
        "type": "response.completed",
        "sequence_number": seq,
        "response": {
            "id": "resp_reason123",
            "object": "response",
            "created_at": 1234567890,
            "model": model,
            "status": "completed",
            "output": [
                reasoning_item,
                {
                    "type": "message",
                    "id": "msg_reason123",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": content,
                        "annotations": []
                    }]
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20,
                "total_tokens": 30,
                "input_tokens_details": {
                    "cached_tokens": 0
                },
                "output_tokens_details": {
                    "reasoning_tokens": 50
                }
            }
        }
    });
    events.push(SseEvent::data(completed.to_string()));
    events.push(SseEvent::data("[DONE]"));

    events
}

#[tokio::test]
async fn test_responses_api_streaming_text() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("Hello from Responses API!");
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let request = ConversationRequest::from_items(vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Say hello"),
    ]);

    let (mut stream, _metadata, _) = client.conversation_stream_responses(request).await.unwrap();

    let mut content = String::new();
    let mut completed = false;
    while let Some(event_result) = stream.next().await {
        let event = event_result.unwrap();
        use echo_agent_runtime::sampling::rs::ResponseStreamEvent;
        match event {
            ResponseStreamEvent::ResponseOutputTextDelta(delta) => {
                content.push_str(&delta.delta);
            }
            ResponseStreamEvent::ResponseCompleted(_) => {
                completed = true;
            }
            _ => {}
        }
    }

    assert!(content.contains("Hello"));
    assert!(content.contains("Responses"));
    assert!(completed);
}

#[tokio::test]
async fn test_responses_api_streaming_tool_call() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_tool_call_stream(
            "call_xyz789",
            "bash",
            r#"{"command": "ls -la"}"#,
            "echo-agent-test",
        )),
    );
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let request = ConversationRequest::from_items(vec![ConversationItem::user("List files")])
        .with_tools(vec![ToolSpec {
            name: "bash".to_string(),
            description: Some("Run a command".to_string()),
            parameters: json!({"type": "object"}),
        }]);

    let (mut stream, _metadata, _) = client.conversation_stream_responses(request).await.unwrap();

    let mut function_call_found = false;
    while let Some(event_result) = stream.next().await {
        let event = event_result.unwrap();
        use echo_agent_runtime::sampling::rs::ResponseStreamEvent;
        if let ResponseStreamEvent::ResponseCompleted(completed) = event {
            for output in completed.response.output {
                use echo_agent_runtime::sampling::rs::OutputItem;
                if let OutputItem::FunctionCall(fc) = output {
                    assert_eq!(fc.call_id, "call_xyz789");
                    assert_eq!(fc.name, "bash");
                    assert!(fc.arguments.contains("ls -la"));
                    function_call_found = true;
                }
            }
        }
    }

    assert!(function_call_found);
}

#[tokio::test]
async fn test_responses_api_with_reasoning_and_encrypted_content() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_with_reasoning_stream(
            "Let me think step by step about this problem.",
            Some("enc_base64_encrypted_reasoning_chain_data"),
            "The answer based on my reasoning is 42.",
            "echo-agent-test",
        )),
    );
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let request = ConversationRequest::from_items(vec![ConversationItem::user(
        "What is the meaning of life?",
    )]);

    let (mut stream, _metadata, _) = client.conversation_stream_responses(request).await.unwrap();

    let mut found_reasoning = false;
    let mut found_encrypted = false;
    let mut found_content = false;

    while let Some(event_result) = stream.next().await {
        let event = event_result.unwrap();
        use echo_agent_runtime::sampling::rs::ResponseStreamEvent;
        if let ResponseStreamEvent::ResponseCompleted(completed) = event {
            for output in &completed.response.output {
                use echo_agent_runtime::sampling::rs::OutputItem;
                match output {
                    OutputItem::Reasoning(r) => {
                        // Check summary text
                        if !r.summary.is_empty() {
                            found_reasoning = true;
                        }
                        // Check encrypted content
                        if let Some(encrypted_content) = &r.encrypted_content {
                            found_encrypted = true;
                            assert!(encrypted_content.contains("enc_base64"));
                        }
                    }
                    OutputItem::Message(msg) => {
                        for content in &msg.content {
                            use echo_agent_runtime::sampling::rs::OutputMessageContent;
                            if let OutputMessageContent::OutputText(text) = content
                                && text.text.contains("42")
                            {
                                found_content = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    assert!(found_reasoning, "Should have found reasoning summary");
    assert!(
        found_encrypted,
        "Should have found encrypted reasoning content"
    );
    assert!(found_content, "Should have found message content");
}

#[tokio::test]
async fn test_responses_api_reasoning_without_encrypted() {
    // Test reasoning with only visible summary, no encrypted content
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_with_reasoning_stream(
            "I need to analyze the code carefully.",
            None, // No encrypted content
            "Here is my analysis.",
            "echo-agent-test",
        )),
    );
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let request =
        ConversationRequest::from_items(vec![ConversationItem::user("Analyze this code")]);

    let (mut stream, _metadata, _) = client.conversation_stream_responses(request).await.unwrap();

    let mut found_reasoning = false;
    while let Some(event_result) = stream.next().await {
        let event = event_result.unwrap();
        use echo_agent_runtime::sampling::rs::ResponseStreamEvent;
        if let ResponseStreamEvent::ResponseCompleted(completed) = event {
            for output in &completed.response.output {
                use echo_agent_runtime::sampling::rs::OutputItem;
                if let OutputItem::Reasoning(r) = output {
                    found_reasoning = true;
                    // Should have summary but no encrypted content
                    assert!(!r.summary.is_empty());
                    assert!(r.encrypted_content.is_none());
                }
            }
        }
    }

    assert!(found_reasoning);
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[tokio::test]
async fn test_chat_completions_401_unauthorized() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::text(401, "Unauthorized"),
    );
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);

    let result = client.conversation_stream(request).await;
    assert!(result.is_err());

    if let Err(SamplingError::Auth { .. }) = result {
        // Expected
    } else {
        panic!("Expected Auth error");
    }
}

#[tokio::test]
async fn test_chat_completions_500_server_error() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::json(500, json!({"error": {"message": "Internal server error"}})),
    );
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);

    let result = client.conversation_stream(request).await;
    assert!(result.is_err());

    if let Err(SamplingError::Api {
        status, message, ..
    }) = result
    {
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(message.contains("Internal server error"));
    } else {
        panic!("Expected Api error");
    }
}

#[tokio::test]
async fn test_responses_api_401_unauthorized() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/responses", ScriptedResponse::text(401, "Unauthorized"));
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);

    let result = client.conversation_stream_responses(request).await;
    assert!(result.is_err());

    if let Err(SamplingError::Auth { .. }) = result {
        // Expected
    } else {
        panic!("Expected Auth error");
    }
}

#[tokio::test]
async fn test_stream_error_during_streaming() {
    // Simulate a stream error mid-response
    let events = vec![
        SseEvent::data(
            json!({
                "id": "chatcmpl-test123",
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": "echo-agent-test",
                "choices": [{
                    "index": 0,
                    "delta": {"role": "assistant", "content": "Hello"},
                    "finish_reason": null
                }]
            })
            .to_string(),
        ),
        // Stream error
        SseEvent::data(
            json!({
                "error": {
                    "message": "Stream interrupted",
                    "type": "stream_error"
                }
            })
            .to_string(),
        ),
    ];

    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/chat/completions", ScriptedResponse::sse(events));
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);

    let (mut stream, _metadata) = client.conversation_stream(request).await.unwrap();

    let mut got_content = false;
    let mut got_error = false;

    while let Some(result) = stream.next().await {
        match result {
            Ok(chunk) => {
                for choice in chunk.choices {
                    if choice.delta.content.is_some() {
                        got_content = true;
                    }
                }
            }
            Err(SamplingError::StreamError { .. }) => {
                got_error = true;
            }
            Err(e) => {
                panic!("Unexpected error: {:?}", e);
            }
        }
    }

    assert!(got_content, "Should have received content before error");
    assert!(got_error, "Should have received stream error");
}

#[tokio::test]
async fn test_stream_error_during_responses_streaming() {
    // Simulate a stream error mid-response on the Responses API path.
    // This mirrors test_stream_error_during_streaming but exercises the
    // Responses API stream-error detection (the second call site for the
    // fast-path contains("error") guard).
    let events = vec![
        SseEvent::with_event(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "sequence_number": 0,
                "item_id": "item_test",
                "output_index": 0,
                "content_index": 0,
                "delta": "Hello"
            })
            .to_string(),
        ),
        // Stream error
        SseEvent::data(
            json!({
                "error": {
                    "message": "Stream interrupted",
                    "type": "stream_error"
                }
            })
            .to_string(),
        ),
    ];

    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response("/v1/responses", ScriptedResponse::sse(events));
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);

    let (mut stream, _metadata, _) = client.conversation_stream_responses(request).await.unwrap();

    let mut got_event = false;
    let mut got_error = false;

    while let Some(result) = stream.next().await {
        match result {
            Ok(_) => {
                got_event = true;
            }
            Err(SamplingError::StreamError { .. }) => {
                got_error = true;
            }
            Err(e) => {
                panic!("Unexpected error: {:?}", e);
            }
        }
    }

    assert!(got_event, "Should have received an event before error");
    assert!(got_error, "Should have received stream error");
}

// ============================================================================
// Request Validation Tests
// ============================================================================

#[tokio::test]
async fn test_request_includes_headers() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("OK");
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")])
        .with_conv_id("conv-12345")
        .with_req_id("req-67890");

    let (mut stream, _metadata) = client.conversation_stream(request).await.unwrap();
    while stream.next().await.is_some() {}

    let request = server.requests().pop().unwrap();

    assert_eq!(request.header("authorization"), Some("Bearer test-api-key"));
    assert_eq!(request.header("x-echo-agent-conv-id"), Some("conv-12345"));
    assert_eq!(request.header("x-echo-agent-req-id"), Some("req-67890"));
}

/// The session writes the resolved `x-compaction-at` value into
/// `SamplerConfig::extra_headers`; this proves the client forwards it on the wire.
#[tokio::test]
async fn test_request_forwards_compaction_at_header() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("OK");
    let client = create_test_client_with_extra_headers(
        &server.url(),
        ApiBackend::ChatCompletions,
        &[("x-compaction-at", "217600")],
    );

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);
    let (mut stream, _metadata) = client.conversation_stream(request).await.unwrap();
    while stream.next().await.is_some() {}

    let request = server.requests().pop().unwrap();
    assert_eq!(request.header("x-compaction-at"), Some("217600"));
}

#[tokio::test]
async fn test_request_includes_tools() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("OK");
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")])
        .with_tools(vec![
            ToolSpec {
                name: "read_file".to_string(),
                description: Some("Read a file".to_string()),
                parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            },
            ToolSpec {
                name: "bash".to_string(),
                description: Some("Run a command".to_string()),
                parameters: json!({"type": "object"}),
            },
        ])
        .with_tool_choice(ConversationToolChoice::Auto);

    let (mut stream, _metadata) = client.conversation_stream(request).await.unwrap();
    while stream.next().await.is_some() {}

    let body = server.request_bodies().pop().unwrap();

    // Verify tools were included
    let tools = body.get("tools").unwrap().as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["function"]["name"], "read_file");
    assert_eq!(tools[1]["function"]["name"], "bash");

    // Verify tool_choice
    assert_eq!(body.get("tool_choice").unwrap(), "auto");
}

#[tokio::test]
async fn test_responses_api_request_format() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("OK");
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let request = ConversationRequest::from_items(vec![
        ConversationItem::system("You are helpful"),
        ConversationItem::user("Hello"),
    ])
    .with_temperature(0.5)
    .with_max_output_tokens(500);

    let (mut stream, _metadata, _) = client.conversation_stream_responses(request).await.unwrap();
    while stream.next().await.is_some() {}

    let body = server.request_bodies().pop().unwrap();

    // Verify Responses API format
    assert!(body.get("input").is_some());
    assert_eq!(body.get("temperature").unwrap(), 0.5);
    assert_eq!(body.get("max_output_tokens").unwrap(), 500);
    assert_eq!(body.get("stream").unwrap(), true);

    // Verify input items format
    let input = body.get("input").unwrap().as_array().unwrap();
    assert!(input.len() >= 2);
}

/// The sampler owns the doom-loop opt-in: setting
/// `SamplerConfig::doom_loop_recovery` puts `x-echo-agent-doom-loop-check` on the
/// wire AND arms the collector, and the server's named check event is
/// absorbed mid-stream without disturbing the typed event flow.
#[tokio::test]
async fn test_doom_loop_check_enabled_sends_header_and_absorbs_check_event() {
    use echo_agent_sampling_types::doom_loop::{
        DOOM_LOOP_CHECK_EVENT_TYPE, SAMPLE_CHECK_EVENT_DATA,
    };

    let server = MockInferenceServer::start().await.unwrap();
    let mut events =
        responses_api_reasoning_and_text_events("pondering", "Hello there", "test-model");
    // Splice the named check event right after `response.created`.
    events.insert(
        1,
        SseEvent::with_event(DOOM_LOOP_CHECK_EVENT_TYPE, SAMPLE_CHECK_EVENT_DATA),
    );
    server.enqueue_response("/v1/responses", ScriptedResponse::sse(events));

    let mut config = test_sampler_config(&server.url(), ApiBackend::Responses, &[]);
    config.doom_loop_recovery = Some(Default::default());
    let client = Client::new(config).unwrap();

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);
    let (mut stream, _metadata, collector) =
        client.conversation_stream_responses(request).await.unwrap();
    assert!(collector.is_some(), "set policy must arm a collector");

    let mut completed = false;
    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("absorbed check event must not fail the typed stream");
        if matches!(event, rs::ResponseStreamEvent::ResponseCompleted(_)) {
            completed = true;
        }
    }
    assert!(completed);

    let logged = server.requests().pop().unwrap();
    assert!(logged.path.contains("/responses"));
    assert_eq!(logged.header("x-echo-agent-doom-loop-check"), Some("1024"));
}

/// With the check disabled no header goes on the wire, and check frames from
/// a misbehaving server (rollout skew) are dropped instead of failing the
/// typed stream — a named frame even with a garbage payload, and an unnamed
/// frame identified only by its payload `type` tag.
#[tokio::test]
async fn test_doom_loop_check_disabled_sends_no_header_and_drops_check_frames() {
    use echo_agent_sampling_types::doom_loop::{
        DOOM_LOOP_CHECK_EVENT_TYPE, SAMPLE_CHECK_EVENT_DATA,
    };

    let server = MockInferenceServer::start().await.unwrap();
    let mut events =
        responses_api_reasoning_and_text_events("pondering", "Hello there", "test-model");
    events.insert(
        1,
        SseEvent::with_event(DOOM_LOOP_CHECK_EVENT_TYPE, "this is not json"),
    );
    events.insert(2, SseEvent::data(SAMPLE_CHECK_EVENT_DATA));
    server.enqueue_response("/v1/responses", ScriptedResponse::sse(events));

    let client = create_test_client(&server.url(), ApiBackend::Responses);
    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);
    let (mut stream, _metadata, collector) =
        client.conversation_stream_responses(request).await.unwrap();
    assert!(
        collector.is_none(),
        "disabled policy must not arm a collector"
    );

    let mut completed = false;
    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("check frames must be dropped, not fail the stream");
        if matches!(event, rs::ResponseStreamEvent::ResponseCompleted(_)) {
            completed = true;
        }
    }
    assert!(completed);

    let logged = server.requests().pop().unwrap();
    assert!(logged.path.contains("/responses"));
    assert_eq!(logged.header("x-echo-agent-doom-loop-check"), None);
}

// ============================================================================
// Multi-turn Conversation Tests
// ============================================================================

#[tokio::test]
async fn test_multi_turn_conversation_with_tool_calls() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("I've read the file for you.");
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    // Simulate a multi-turn conversation with tool call and result
    let request = ConversationRequest::from_items(vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Read the README file"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "README.md"}"#.into(),
        }]),
        ConversationItem::tool_result("call_1", "# My Project\n\nThis is a test project."),
        // The model should now respond based on the file content
    ]);

    let (mut stream, _metadata) = client.conversation_stream(request).await.unwrap();

    let mut content = String::new();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.unwrap();
        for choice in chunk.choices {
            if let Some(ref text) = choice.delta.content {
                content.push_str(text);
            }
        }
    }

    assert!(!content.is_empty());
}

#[tokio::test]
async fn test_responses_api_multi_turn_with_tool_calls() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("Done with the file.");
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    // Multi-turn with tool call and result
    let request = ConversationRequest::from_items(vec![
        ConversationItem::user("Read the config"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_abc".into(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "config.json"}"#.into(),
        }]),
        ConversationItem::tool_result("call_abc", r#"{"key": "value"}"#),
    ]);

    let (mut stream, _metadata, _) = client.conversation_stream_responses(request).await.unwrap();

    let mut completed = false;
    while let Some(event_result) = stream.next().await {
        let event = event_result.unwrap();
        use echo_agent_runtime::sampling::rs::ResponseStreamEvent;
        if let ResponseStreamEvent::ResponseCompleted(_) = event {
            completed = true;
        }
    }

    assert!(completed);
}

// ============================================================================
// Request Counter Tests (verify retry behavior, etc.)
// ============================================================================

#[tokio::test]
async fn test_single_request_per_stream() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("Hello");
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);

    let (mut stream, _metadata) = client.conversation_stream(request).await.unwrap();
    while stream.next().await.is_some() {}

    assert_eq!(server.request_count(), 1);
}

// ============================================================================
// API Backend Routing Tests
// ============================================================================

#[tokio::test]
async fn test_api_backend_getter_returns_configured_value() {
    // Verify that the client correctly reports its configured API backend
    let client_responses = create_test_client("http://localhost/v1", ApiBackend::Responses);
    assert_eq!(client_responses.api_backend(), ApiBackend::Responses);

    let client_chat = create_test_client("http://localhost/v1", ApiBackend::ChatCompletions);
    assert_eq!(client_chat.api_backend(), ApiBackend::ChatCompletions);
}

#[tokio::test]
async fn test_responses_backend_hits_responses_endpoint_not_chat_completions() {
    // This test verifies that when ApiBackend::Responses is configured,
    // the client hits /v1/responses and NOT /v1/chat/completions.
    // The session-level dispatch in `echo-agent-sampler` selects the
    // backend stream based on `SamplingClient::api_backend()`.

    let server = MockInferenceServer::start().await.unwrap();
    // A request to the wrong endpoint fails loudly instead of streaming.
    server.enqueue_response(
        "/v1/chat/completions",
        ScriptedResponse::text(500, "Wrong endpoint!"),
    );
    server.set_response("OK");
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    // Simulate the routing logic from acp_session.rs
    match client.api_backend() {
        ApiBackend::Responses => {
            let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);
            let (mut stream, _metadata, _) =
                client.conversation_stream_responses(request).await.unwrap();
            while stream.next().await.is_some() {}
        }
        ApiBackend::ChatCompletions => {
            panic!("Expected Responses backend but got ChatCompletions");
        }
        ApiBackend::Messages => {
            panic!("Expected Responses backend but got Messages");
        }
    }

    assert!(
        !server.has_chat_completion_request(),
        "Should NOT have called /v1/chat/completions"
    );
    assert!(
        server.has_responses_request(),
        "Should have called /v1/responses"
    );
}

#[tokio::test]
async fn test_chat_completions_backend_hits_chat_endpoint_not_responses() {
    // Verify the inverse: ChatCompletions backend hits /v1/chat/completions

    let server = MockInferenceServer::start().await.unwrap();
    // A request to the wrong endpoint fails loudly instead of streaming.
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::text(500, "Wrong endpoint!"),
    );
    server.set_response("OK");
    let client = create_test_client(&server.url(), ApiBackend::ChatCompletions);

    // Simulate the routing logic from acp_session.rs
    match client.api_backend() {
        ApiBackend::ChatCompletions => {
            let request = ConversationRequest::from_items(vec![ConversationItem::user("Hello")]);
            let (mut stream, _metadata) = client.conversation_stream(request).await.unwrap();
            while stream.next().await.is_some() {}
        }
        ApiBackend::Responses => {
            panic!("Expected ChatCompletions backend but got Responses");
        }
        ApiBackend::Messages => {
            panic!("Expected ChatCompletions backend but got Messages");
        }
    }

    assert!(
        server.has_chat_completion_request(),
        "Should have called /v1/chat/completions"
    );
    assert!(
        !server.has_responses_request(),
        "Should NOT have called /v1/responses"
    );
}
