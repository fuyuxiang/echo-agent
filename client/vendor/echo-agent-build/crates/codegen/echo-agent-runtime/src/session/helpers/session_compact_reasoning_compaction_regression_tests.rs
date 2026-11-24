use super::*;
use crate::sampling::{Client, ContentPart, ConversationItem, SamplerConfig, ToolCall, rs};
use axum::Router;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::post;
use futures_util::stream;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Minimal ChatCompletions SSE stream: one content token, `stop`, then `[DONE]`.
fn summary_stream() -> Vec<Event> {
    vec![
        Event::default().data(
            json!({
                "id": "chatcmpl-test",
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant", "content": "<summary>ok</summary>" },
                    "finish_reason": "stop"
                }]
            })
            .to_string(),
        ),
        Event::default().data("[DONE]"),
    ]
}

/// SSE stream: a reasoning delta (no content), then a content delta + `stop`.
fn reasoning_then_summary_stream() -> Vec<Event> {
    vec![
        Event::default().data(
            json!({
                "id": "chatcmpl-test",
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "delta": { "role": "assistant", "reasoning_content": "let me think about the summary" },
                    "finish_reason": null
                }]
            })
            .to_string(),
        ),
        Event::default().data(
            json!({
                "id": "chatcmpl-test",
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "delta": { "content": "<summary>ok</summary>" },
                    "finish_reason": "stop"
                }]
            })
            .to_string(),
        ),
        Event::default().data("[DONE]"),
    ]
}

/// A reasoning delta that precedes the content delta must not break
/// summary extraction.
#[tokio::test]
async fn chat_completions_compaction_extracts_summary_after_reasoning_delta() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let stream = stream::iter(
                reasoning_then_summary_stream()
                    .into_iter()
                    .map(Ok::<_, std::convert::Infallible>),
            );
            Sse::new(stream).keep_alive(KeepAlive::default())
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let base_url = format!("http://{addr}/v1");
    let config = test_config(&base_url);
    let client = Client::new(config.clone()).unwrap();

    let chat_history = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("<user_query>\nfix the bug\n</user_query>"),
        ConversationItem::assistant("I fixed it."),
        ConversationItem::user("Summarize the conversation so far."),
    ];

    let output = generate_session_compact(
        chat_history,
        0,
        vec![],
        vec![],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_secs(30),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|_| panic!("compaction must succeed"));

    // A reasoning delta arriving before the content delta must not break
    // summary extraction — the content channel is returned as the summary.
    assert_eq!(output.content, "<summary>ok</summary>");

    let _ = shutdown_tx.send(());
}

const USER_IMAGE_SENTINEL: &str = "data:image/png;base64,user-image-sentinel";
const TOOL_IMAGE_SENTINEL: &str = "data:image/png;base64,tool-image-sentinel";
const FINAL_PROMPT_SENTINEL: &str = "FINAL_COMPACTION_PROMPT_SENTINEL";

fn image_compaction_history() -> Vec<ConversationItem> {
    vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user_with_parts(vec![
            ContentPart::Text {
                text: "user text sentinel".into(),
            },
            ContentPart::Image {
                url: USER_IMAGE_SENTINEL.into(),
            },
        ]),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call-image-sentinel".into(),
            name: "read_file".into(),
            arguments: r#"{"target_file":"image.png"}"#.into(),
        }]),
        ConversationItem::tool_result_with_images(
            "call-image-sentinel",
            "tool result text sentinel",
            vec![ContentPart::Image {
                url: TOOL_IMAGE_SENTINEL.into(),
            }],
        ),
        ConversationItem::user(FINAL_PROMPT_SENTINEL),
    ]
}

fn assert_preserved_compaction_body(body: &serde_json::Value) {
    let serialized = body.to_string();
    assert!(serialized.contains(USER_IMAGE_SENTINEL));
    assert!(serialized.contains(TOOL_IMAGE_SENTINEL));
    assert!(serialized.contains("user text sentinel"));
    assert!(serialized.contains("tool result text sentinel"));
    assert!(serialized.contains("call-image-sentinel"));
    assert!(serialized.contains(FINAL_PROMPT_SENTINEL));
}

fn test_config(base_url: &str) -> SamplerConfig {
    SamplerConfig {
        api_key: Some("test-api-key".to_string()),
        base_url: base_url.to_string(),
        model: "test-model".to_string(),
        max_completion_tokens: Some(1000),
        temperature: Some(0.7),
        top_p: None,
        api_backend: ApiBackend::ChatCompletions,
        auth_scheme: Default::default(),
        extra_headers: Default::default(),
        extra_response_includes: Vec::new(),
        query_params: Default::default(),
        env_http_headers: Default::default(),
        context_window: 256_000,
        client_version: None,
        force_http1: false,
        max_retries: None,
        stream_tool_calls: false,
        idle_timeout_secs: None,
        client_identifier: None,
        reasoning_effort: None,
        deployment_id: None,
        user_id: None,
        origin_client: None,
        attribution_callback: None,
        bearer_resolver: None,
        supports_backend_search: false,
        compactions_remaining: None,
        compaction_at_tokens: None,
        doom_loop_recovery: None,
        header_injector: None,
    }
}

#[tokio::test]
async fn chat_completions_compaction_does_not_panic_on_reasoning_sibling() {
    // Mock ChatCompletions endpoint returning a tiny summary stream.
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let stream = stream::iter(
                summary_stream()
                    .into_iter()
                    .map(Ok::<_, std::convert::Infallible>),
            );
            Sse::new(stream).keep_alive(KeepAlive::default())
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let base_url = format!("http://{addr}/v1");
    let config = test_config(&base_url);
    let client = Client::new(config.clone()).unwrap();

    // Responses-API session resumed onto ChatCompletions: a standalone `Reasoning` sibling.
    let chat_history = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("<user_query>\nfix the bug\n</user_query>"),
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: "r1".to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: "thinking about the bug".to_string(),
            })],
            content: None,
            encrypted_content: None,
            status: None,
        }),
        ConversationItem::assistant("I fixed it."),
        ConversationItem::user("Summarize the conversation so far."),
    ];

    let result = generate_session_compact(
        chat_history,
        0,
        vec![],
        vec![],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_secs(30),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    let output = result
        .unwrap_or_else(|_| panic!("compaction must succeed for a Reasoning-bearing history"));
    assert_eq!(output.content, "<summary>ok</summary>");

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn chat_completions_below_trigger_preserves_images_and_tools() {
    use std::sync::{Arc, Mutex};

    // Mock ChatCompletions endpoint that captures each request body.
    let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let cap = captured.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |body: axum::Json<serde_json::Value>| {
            let cap = cap.clone();
            async move {
                cap.lock().unwrap().push(body.0);
                let stream = stream::iter(
                    summary_stream()
                        .into_iter()
                        .map(Ok::<_, std::convert::Infallible>),
                );
                Sse::new(stream).keep_alive(KeepAlive::default())
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let base_url = format!("http://{addr}/v1");
    let config = test_config(&base_url);

    let chat_history = image_compaction_history();
    let tools = vec![ToolSpec {
        name: "read_file".to_string(),
        description: Some("Reads a file".to_string()),
        parameters: json!({"type": "object", "properties": {}}),
    }];
    let client = Client::new(config.clone()).unwrap();
    generate_session_compact(
        chat_history.clone(),
        0,
        tools,
        vec![],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_secs(30),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|_| panic!("compaction with tools must succeed"));

    // Without tools: neither key present.
    let client = Client::new(config.clone()).unwrap();
    generate_session_compact(
        chat_history,
        0,
        vec![],
        vec![],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_secs(30),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|_| panic!("compaction without tools must succeed"));

    let bodies = captured.lock().unwrap();
    assert_eq!(bodies.len(), 2, "mock must have served both requests");

    let with_tools = &bodies[0];
    assert_eq!(
        with_tools["tool_choice"],
        json!("auto"),
        "default compaction tool_choice is auto"
    );
    let sent_tools = with_tools["tools"]
        .as_array()
        .expect("tools must be attached for prefix-cache alignment");
    assert_eq!(sent_tools.len(), 1);
    assert_eq!(sent_tools[0]["function"]["name"], json!("read_file"));
    assert_preserved_compaction_body(with_tools);

    let without_tools = &bodies[1];
    assert!(
        without_tools.get("tools").is_none(),
        "no tools key when none are passed"
    );
    assert!(
        without_tools.get("tool_choice").is_none(),
        "tool_choice without tools is rejected by OpenAI-compat backends"
    );

    let _ = shutdown_tx.send(());
}

fn responses_summary_stream() -> Vec<Event> {
    vec![
        Event::default().data(
            json!({
                "type": "response.created",
                "sequence_number": 0,
                "response": {
                    "id": "resp_test",
                    "object": "response",
                    "created_at": 1234567890,
                    "model": "test-model",
                    "status": "in_progress",
                    "output": []
                }
            })
            .to_string(),
        ),
        Event::default().data(
            json!({
                "type": "response.output_text.delta",
                "sequence_number": 1,
                "item_id": "msg_test",
                "output_index": 0,
                "content_index": 0,
                "delta": "<summary>ok</summary>"
            })
            .to_string(),
        ),
        Event::default().data(
            json!({
                "type": "response.completed",
                "sequence_number": 2,
                "response": {
                    "id": "resp_test",
                    "object": "response",
                    "created_at": 1234567890,
                    "model": "test-model",
                    "status": "completed",
                    "output": []
                }
            })
            .to_string(),
        ),
    ]
}

fn test_config_responses(base_url: &str) -> SamplerConfig {
    let mut config = test_config(base_url);
    config.api_backend = ApiBackend::Responses;
    config
}

async fn sample_typed_compaction(
    backend: ApiBackend,
    frames: Vec<Event>,
    keep_open: bool,
) -> Result<CompactOutput, CompactFailure> {
    let path = match backend {
        ApiBackend::Responses => "/v1/responses",
        ApiBackend::Messages => "/v1/messages",
        ApiBackend::ChatCompletions => unreachable!(),
    };
    let app = Router::new().route(
        path,
        post(move || {
            let frames = frames.clone();
            async move {
                let events =
                    stream::iter(frames.into_iter().map(Ok::<_, std::convert::Infallible>));
                let events = if keep_open {
                    events.chain(stream::pending()).boxed()
                } else {
                    events.boxed()
                };
                Sse::new(events)
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut config = test_config(&base_url);
    config.api_backend = backend;
    let result = generate_session_compact(
        vec![ConversationItem::user("Summarize the conversation so far.")],
        0,
        vec![],
        vec![],
        Client::new(config.clone()).unwrap(),
        acp::SessionId::new("typed-compaction-test"),
        &config,
        std::time::Duration::from_secs(2),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;
    server.abort();
    result
}

#[tokio::test]
async fn responses_compaction_requires_typed_terminal() {
    let mut draft = responses_summary_stream();
    let completed = draft.pop().unwrap();
    for tail in [None, Some(Event::default().data("[DONE]"))] {
        let mut frames = draft.clone();
        frames.extend(tail);
        let result = sample_typed_compaction(ApiBackend::Responses, frames, false).await;
        assert!(matches!(result, Err(CompactFailure::Transient(err))
            if err.data.as_ref().and_then(|data| data.as_str())
                .is_some_and(|text| text.contains("before response.completed or response.incomplete"))));
    }

    let mut frames = draft.clone();
    frames.push(completed);
    let output = sample_typed_compaction(ApiBackend::Responses, frames, true)
        .await
        .unwrap();
    assert_eq!(output.content, "<summary>ok</summary>");
    assert_eq!(output.stop_reason.as_deref(), Some("stop"));
    assert!(!output.truncated);

    draft.push(
        Event::default().data(
            json!({
                "type": "response.incomplete",
                "sequence_number": 2,
                "response": {
                    "id": "resp_test", "object": "response", "created_at": 1234567890,
                    "model": "test-model", "status": "incomplete", "output": [],
                    "incomplete_details": { "reason": "max_output_tokens" }
                }
            })
            .to_string(),
        ),
    );
    let output = sample_typed_compaction(ApiBackend::Responses, draft, false)
        .await
        .unwrap();
    assert_eq!(output.content, "<summary>ok</summary>");
    assert_eq!(output.stop_reason.as_deref(), Some("max_output_tokens"));
    assert!(output.truncated);
}

#[tokio::test]
async fn messages_compaction_requires_stop_after_stop_reason() {
    use echo_agent_test_support::sse::messages_api_events;

    let summary = "<summary>ok</summary>";
    let mut draft = messages_api_events(summary, "test-model", "end_turn");
    let message_stop = draft.pop().unwrap();
    let result = sample_typed_compaction(ApiBackend::Messages, draft.clone(), false).await;
    assert!(matches!(result, Err(CompactFailure::Transient(err))
        if err.data.as_ref().and_then(|data| data.as_str())
            .is_some_and(|text| text.contains("before message_stop"))));

    // A typed stop without a stop reason is a malformed terminal, even if
    // the summary itself has already been streamed.
    draft.pop();
    draft.push(message_stop);
    let result = sample_typed_compaction(ApiBackend::Messages, draft, false).await;
    assert!(matches!(result, Err(CompactFailure::Deterministic(err))
        if err.data.as_ref().and_then(|data| data.as_str())
            .is_some_and(|text| text.contains("message_stop without stop_reason"))));

    for (reason, truncated) in [("end_turn", false), ("max_tokens", true)] {
        let mut frames = messages_api_events("ok</summary>", "test-model", reason);
        frames[1] = Event::default().data(
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"<summary>"}})
                .to_string(),
        );
        let output = sample_typed_compaction(ApiBackend::Messages, frames, true)
            .await
            .unwrap();
        assert_eq!(output.content, summary);
        assert_eq!(output.stop_reason.as_deref(), Some(reason));
        assert_eq!(output.truncated, truncated);
    }
}

#[tokio::test]
async fn messages_compaction_rejects_unframed_or_unfinished_content() {
    use echo_agent_test_support::sse::messages_api_events;

    for (missing, expected) in [
        (0, "unexpected content_block_start"),
        (1, "content_block_delta without an open block"),
        (3, "stop_reason before content_block_stop"),
    ] {
        let mut frames =
            messages_api_events("<summary>partial</summary>", "test-model", "end_turn");
        let _ = frames.remove(missing);
        let result = sample_typed_compaction(ApiBackend::Messages, frames, false).await;
        assert!(matches!(result, Err(CompactFailure::Deterministic(err))
            if err.data.as_ref().and_then(|data| data.as_str())
                .is_some_and(|text| text.contains(expected))));
    }
}

#[tokio::test]
async fn responses_below_trigger_preserves_images_and_tools() {
    use std::sync::{Arc, Mutex};

    let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let cap = captured.clone();
    let app = Router::new().route(
        "/v1/responses",
        post(move |body: axum::Json<serde_json::Value>| {
            let cap = cap.clone();
            async move {
                cap.lock().unwrap().push(body.0);
                let stream = stream::iter(
                    responses_summary_stream()
                        .into_iter()
                        .map(Ok::<_, std::convert::Infallible>),
                );
                Sse::new(stream).keep_alive(KeepAlive::default())
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let base_url = format!("http://{addr}/v1");
    let config = test_config_responses(&base_url);

    let chat_history = image_compaction_history();
    let tools = vec![ToolSpec {
        name: "read_file".to_string(),
        description: Some("Reads a file".to_string()),
        parameters: json!({"type": "object", "properties": {}}),
    }];
    let hosted = vec![HostedTool::WebSearch { options: None }];
    let client = Client::new(config.clone()).unwrap();
    generate_session_compact(
        chat_history.clone(),
        0,
        tools,
        hosted,
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_secs(30),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|_| panic!("Responses compaction with tools must succeed"));

    let client = Client::new(config.clone()).unwrap();
    generate_session_compact(
        chat_history,
        0,
        vec![],
        vec![],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_secs(30),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|_| panic!("Responses compaction without tools must succeed"));

    let bodies = captured.lock().unwrap();
    assert_eq!(bodies.len(), 2, "mock must have served both requests");

    let with_tools = &bodies[0];
    assert_eq!(
        with_tools["tool_choice"],
        json!("auto"),
        "default Responses compaction tool_choice is auto"
    );
    let sent_tools = with_tools["tools"]
        .as_array()
        .expect("tools must be attached for prefix-cache alignment");
    let has_read_file = sent_tools.iter().any(|t| {
        t.get("name") == Some(&json!("read_file"))
            || t.pointer("/name") == Some(&json!("read_file"))
    });
    assert!(
        has_read_file,
        "client function tool must be present: {sent_tools:?}"
    );
    assert!(
        sent_tools
            .iter()
            .any(|t| t.get("type") == Some(&json!("web_search"))),
        "hosted web_search must be present for prefix alignment: {sent_tools:?}"
    );
    assert_preserved_compaction_body(with_tools);

    let without_tools = &bodies[1];
    assert!(
        without_tools
            .get("tools")
            .map(|t| t.as_array().is_none_or(|a| a.is_empty()))
            .unwrap_or(true),
        "no tools when none are passed"
    );
    assert!(
        without_tools.get("tool_choice").is_none(),
        "tool_choice without tools should be omitted"
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn stalled_compaction_stream_times_out_as_transient() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let stream = stream::pending::<Result<Event, std::convert::Infallible>>();
            Sse::new(stream)
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let base_url = format!("http://{addr}/v1");
    let config = test_config(&base_url);
    let client = Client::new(config.clone()).unwrap();

    let chat_history = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Summarize the conversation so far."),
    ];

    let result = generate_session_compact(
        chat_history,
        0,
        vec![],
        vec![],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_millis(150),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    match result {
        Err(CompactFailure::Transient(err)) => {
            let data = err
                .data
                .as_ref()
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            assert!(
                data.contains("idle timeout"),
                "expected an idle-timeout transient failure, got: {data}"
            );
        }
        Err(CompactFailure::Deterministic(_) | CompactFailure::Cancelled) => {
            panic!("a stalled stream must be retryable (Transient), not Deterministic/Cancelled")
        }
        Ok(_) => panic!("a stalled stream must not produce a summary"),
    }

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn completed_then_stalled_stream_errors_no_salvage() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let events = stream::iter(vec![Ok::<_, std::convert::Infallible>(
                Event::default().data(
                    json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion.chunk",
                        "created": 1234567890,
                        "model": "test-model",
                        "choices": [{
                            "index": 0,
                            "delta": { "role": "assistant", "content": "<summary>ok</summary>" },
                            "finish_reason": "stop"
                        }]
                    })
                    .to_string(),
                ),
            )])
            .chain(stream::pending::<Result<Event, std::convert::Infallible>>());
            Sse::new(events)
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let base_url = format!("http://{addr}/v1");
    let config = test_config(&base_url);
    let client = Client::new(config.clone()).unwrap();

    let chat_history = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Summarize the conversation so far."),
    ];

    let result = generate_session_compact(
        chat_history,
        0,
        vec![],
        vec![],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_millis(150),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    match result {
        Err(CompactFailure::Transient(err)) => {
            let data = err
                .data
                .as_ref()
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            assert!(
                data.contains("idle timeout"),
                "expected an idle-timeout transient failure, got: {data}"
            );
        }
        Err(CompactFailure::Deterministic(_) | CompactFailure::Cancelled) => {
            panic!("a stalled stream must be retryable (Transient), not Deterministic")
        }
        Ok(_) => panic!(
            "salvage removed: a completed-but-unterminated stream must error, not return a summary"
        ),
    }

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn substantial_partial_errors_no_salvage() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let body = "x".repeat(2500);
            let events = stream::iter(vec![Ok::<_, std::convert::Infallible>(
                Event::default().data(
                    json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion.chunk",
                        "created": 1234567890,
                        "model": "test-model",
                        "choices": [{
                            "index": 0,
                            "delta": { "role": "assistant", "content": body }
                        }]
                    })
                    .to_string(),
                ),
            )])
            .chain(stream::pending::<Result<Event, std::convert::Infallible>>());
            Sse::new(events)
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let base_url = format!("http://{addr}/v1");
    let config = test_config(&base_url);
    let client = Client::new(config.clone()).unwrap();

    let chat_history = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Summarize the conversation so far."),
    ];

    let result = generate_session_compact(
        chat_history,
        0,
        vec![],
        vec![],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_millis(150),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    match result {
        Err(CompactFailure::Transient(err)) => {
            let data = err
                .data
                .as_ref()
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            assert!(
                data.contains("idle timeout"),
                "expected an idle-timeout transient failure, got: {data}"
            );
        }
        Err(CompactFailure::Deterministic(_) | CompactFailure::Cancelled) => {
            panic!("a stalled stream must be retryable (Transient), not Deterministic")
        }
        Ok(_) => panic!("salvage removed: a substantial partial must error, not be returned"),
    }

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn thin_partial_retries_on_stall() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let events = stream::iter(vec![Ok::<_, std::convert::Infallible>(
                Event::default().data(
                    json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion.chunk",
                        "created": 1234567890,
                        "model": "test-model",
                        "choices": [{
                            "index": 0,
                            "delta": { "role": "assistant", "content": "partial" }
                        }]
                    })
                    .to_string(),
                ),
            )])
            .chain(stream::pending::<Result<Event, std::convert::Infallible>>());
            Sse::new(events)
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });

    let base_url = format!("http://{addr}/v1");
    let config = test_config(&base_url);
    let client = Client::new(config.clone()).unwrap();

    let chat_history = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Summarize the conversation so far."),
    ];

    let result = generate_session_compact(
        chat_history,
        0,
        vec![],
        vec![],
        client,
        acp::SessionId::new("test-session"),
        &config,
        std::time::Duration::from_millis(150),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;

    match result {
        Err(CompactFailure::Transient(_)) => {}
        _ => panic!("a thin stalled body must retry (Transient), not salvage"),
    }

    let _ = shutdown_tx.send(());
}
