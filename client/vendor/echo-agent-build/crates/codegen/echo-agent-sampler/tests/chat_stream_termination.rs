//! Exercise the real HTTP decoder and sampler actor with deterministic SSE.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Router, body::Body, response::Response, routing::post};
use echo_agent_sampler::{
    ApiBackend, RequestId, RetryPolicy, SamplerActor, SamplerConfig, SamplingClient,
    SamplingErrorKind, SamplingEvent,
};
use echo_agent_sampling_types::{ConversationItem, ConversationRequest, SamplingError, StopReason};
use echo_agent_test_support::{MockInferenceServer, ScriptedResponse, SseEvent, sse};
use serde_json::{Value, json};

const PATH: &str = "/v1/chat/completions";
const MODEL: &str = "test-model";
const DRAFT_ARGS: &str = r#"{"file_path":"summary.txt","content":"draft"}"#;
const FINAL_ARGS: &str = r#"{"file_path":"summary.txt","content":"verified"}"#;

fn tool_frames(id: &str, arguments: &str) -> Vec<SseEvent> {
    sse::chat_completions_reasoning_then_tool_call_events("", id, "write_file", arguments, MODEL)
}

fn set_finish(frame: &mut SseEvent, reason: Value, index: u32) {
    let mut frame_body: Value = serde_json::from_str(&frame.data).unwrap();
    frame_body["choices"][0]["finish_reason"] = reason;
    frame_body["choices"][0]["index"] = json!(index);
    frame.data = frame_body.to_string();
}

struct Run {
    requests: usize,
    events: Vec<SamplingEvent>,
}

impl Run {
    fn terminal(&self) -> &SamplingEvent {
        let mut terminals = self.events.iter().filter(|event| {
            matches!(
                event,
                SamplingEvent::Completed { .. } | SamplingEvent::Failed { .. }
            )
        });
        let terminal = terminals
            .next()
            .expect("sampler must emit a terminal event");
        assert!(
            terminals.next().is_none(),
            "sampler emitted multiple terminal events"
        );
        terminal
    }

    fn tool_delta_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, SamplingEvent::ToolCallDelta { .. }))
            .count()
    }

    fn tool_ids(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                SamplingEvent::ToolCallDelta { id: Some(id), .. } => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }
}

fn config(base_url: String) -> SamplerConfig {
    SamplerConfig {
        base_url,
        api_key: Some("synthetic-test-key".to_owned()),
        api_backend: ApiBackend::ChatCompletions,
        model: MODEL.to_owned(),
        max_completion_tokens: Some(1024),
        max_retries: Some(3),
        idle_timeout_secs: Some(2),
        ..Default::default()
    }
}

fn request() -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::user("Write a verified summary.")],
        ..Default::default()
    }
}

async fn collect_events(base_url: String, retry_only_before_output: bool) -> Vec<SamplingEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = SamplerActor::spawn(
        config(base_url),
        RetryPolicy {
            max_retries: 2,
            retry_only_before_output,
            ..Default::default()
        },
        tx,
    );
    // Terminal events drive the actual SessionActor. The returned completion
    // additionally fences the event drain so the measurement includes retries.
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        actor.submit_and_collect(RequestId::from("stream-termination"), request()),
    )
    .await
    .expect("sampler exceeded the test budget");

    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

async fn sample(scripts: Vec<Vec<SseEvent>>, retry_only_before_output: bool) -> Run {
    let server = MockInferenceServer::start().await.unwrap();
    for events in scripts {
        server.enqueue_response(PATH, ScriptedResponse::sse(events));
    }
    let started = std::time::Instant::now();
    let events = collect_events(server.url(), retry_only_before_output).await;
    let requests = server
        .requests()
        .iter()
        .filter(|request| request.path == PATH)
        .count();
    let run = Run { requests, events };
    let terminal = match run.terminal() {
        SamplingEvent::Completed {
            response, metrics, ..
        } => json!({
            "kind": "completed",
            "attempts": metrics.attempts,
            "stop": response.stop_reason,
            "text": response.assistant().map(|item| item.content.as_ref()),
            "tool_args": response.assistant().into_iter().flat_map(|item| &item.tool_calls)
                .map(|call| call.arguments.as_ref()).collect::<Vec<_>>(),
            "usage": response.usage,
        }),
        SamplingEvent::Failed { error, .. } => json!({"kind": "failed", "error": error.kind}),
        _ => unreachable!(),
    };
    println!(
        "{}",
        json!({
            "test": std::thread::current().name(),
            "requests": requests,
            "tool_ids": run.tool_ids(),
        "tool_deltas": run.tool_delta_count(),
            "terminal": terminal,
            "elapsed_ms": started.elapsed().as_millis(),
        })
    );
    run
}

#[tokio::test]
async fn dev_eof_discards_draft_tool_before_retry() {
    let mut interrupted = tool_frames("draft", DRAFT_ARGS);
    interrupted.truncate(1);
    let run = sample(vec![interrupted, tool_frames("verified", FINAL_ARGS)], true).await;
    assert_eq!(run.requests, 2);
    assert_eq!(run.tool_ids(), ["verified"]);
    match run.terminal() {
        SamplingEvent::Completed {
            response, metrics, ..
        } => {
            assert_eq!(metrics.attempts, 2);
            let calls = &response.assistant().unwrap().tool_calls;
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].arguments.as_ref(), FINAL_ARGS);
        }
        event => panic!("expected complete response after retry, got {event:?}"),
    }
}

#[tokio::test]
async fn empty_attempt_does_not_count_as_visible_output_for_a_later_interruption() {
    let mut interrupted = tool_frames("draft", DRAFT_ARGS);
    interrupted.truncate(1);
    let run = sample(
        vec![
            sse::chat_completion_script_exact("", MODEL),
            interrupted,
            tool_frames("verified", FINAL_ARGS),
        ],
        false,
    )
    .await;
    assert_eq!(run.requests, 3);
    assert_eq!(run.tool_ids(), ["verified"]);
    assert!(
        matches!(run.terminal(), SamplingEvent::Completed { metrics, .. } if metrics.attempts == 3)
    );
}

#[tokio::test]
async fn dev_length_has_no_dispatchable_tool() {
    let mut frames = tool_frames("draft", DRAFT_ARGS);
    set_finish(&mut frames[1], json!("length"), 0);
    let run = sample(vec![frames], false).await;
    assert_eq!(run.requests, 1);
    assert_eq!(run.tool_delta_count(), 0);
    assert!(matches!(run.terminal(), SamplingEvent::Failed { error, .. }
        if error.kind == SamplingErrorKind::MaxTokensTruncation));
}

#[tokio::test]
async fn held_alternate_choice_does_not_finalize_selected_choice() {
    let mut frames = sse::chat_completion_script_exact("First draft", MODEL);
    frames.truncate(2);
    set_finish(&mut frames[0], Value::Null, 2);
    let mut role_only: Value = serde_json::from_str(&frames[0].data).unwrap();
    role_only["choices"][0]["delta"] = json!({"role": "assistant"});
    frames[0].data = role_only.to_string();
    let run = sample(
        vec![
            frames,
            sse::chat_completion_script_exact("Verified summary", MODEL),
        ],
        false,
    )
    .await;
    assert_eq!(run.requests, 2);
    assert!(
        matches!(run.terminal(), SamplingEvent::Completed { response, .. }
        if response.assistant().unwrap().content.as_ref() == "Verified summary")
    );
}

#[tokio::test]
async fn held_content_filter_has_no_dispatchable_tool() {
    let mut frames = tool_frames("draft", DRAFT_ARGS);
    set_finish(&mut frames[1], json!("content_filter"), 0);
    let run = sample(vec![frames], false).await;
    assert_eq!(run.requests, 1);
    assert_eq!(run.tool_delta_count(), 0);
    assert!(
        matches!(run.terminal(), SamplingEvent::Completed { response, .. }
        if response.stop_reason == Some(StopReason::ContentFilter)
            && response.assistant().unwrap().tool_calls.is_empty())
    );
}

#[tokio::test]
async fn guard_accepts_either_explicit_terminal_signal() {
    let mut finish_only = tool_frames("verified", FINAL_ARGS);
    finish_only.pop();
    let mut done_only = tool_frames("verified", FINAL_ARGS);
    set_finish(&mut done_only[1], Value::Null, 0);

    for frames in [finish_only, done_only] {
        let run = sample(vec![frames], false).await;
        assert_eq!(run.requests, 1);
        assert_eq!(run.tool_ids(), ["verified"]);
        assert!(
            matches!(run.terminal(), SamplingEvent::Completed { response, .. }
            if response.assistant().unwrap().tool_calls[0].arguments.as_ref() == FINAL_ARGS
                && response.usage.as_ref().unwrap().total_tokens == 30)
        );
    }
}

#[tokio::test]
async fn guard_partial_text_never_retries_or_completes() {
    let mut frames = sse::chat_completion_script_exact("First draft", MODEL);
    frames.truncate(1);
    let ordinary = sample(vec![frames.clone()], false).await;
    let budgeted = sample(vec![frames], true).await;
    for run in [ordinary, budgeted] {
        assert_eq!(run.requests, 1);
        assert!(matches!(run.terminal(), SamplingEvent::Failed { error, .. }
            if error.kind == SamplingErrorKind::ChatStreamInterrupted));
    }
}

#[tokio::test]
async fn conflicting_selected_finish_reasons_never_release_tools() {
    for (first, second) in [
        ("content_filter", "stop"),
        ("length", "stop"),
        ("stop", "content_filter"),
    ] {
        let mut frames = tool_frames("draft", DRAFT_ARGS);
        let mut contradictory = frames[1].clone();
        set_finish(&mut frames[1], json!(first), 0);
        set_finish(&mut contradictory, json!(second), 0);
        frames.insert(2, contradictory);
        let run = sample(vec![frames], false).await;
        assert_eq!(run.requests, 1);
        assert_eq!(run.tool_delta_count(), 0);
        assert!(matches!(run.terminal(), SamplingEvent::Failed { error, .. }
            if error.kind == SamplingErrorKind::ChatStreamInterrupted && !error.is_retryable));
    }
}

#[tokio::test]
async fn done_cannot_finalize_an_unfinished_selected_choice() {
    let mut frames = tool_frames("draft", DRAFT_ARGS);
    set_finish(&mut frames[1], json!("stop"), 1);
    let run = sample(vec![frames], false).await;
    assert_eq!(run.requests, 1);
    assert_eq!(run.tool_delta_count(), 0);
    assert!(matches!(run.terminal(), SamplingEvent::Failed { error, .. }
        if error.kind == SamplingErrorKind::ChatStreamInterrupted));
}

#[tokio::test]
async fn tool_chunks_after_selected_finish_are_rejected() {
    let mut frames = tool_frames("draft", DRAFT_ARGS);
    frames.swap(0, 1);
    let run = sample(vec![frames], false).await;
    assert_eq!(run.requests, 1);
    assert_eq!(run.tool_delta_count(), 0);
    assert!(matches!(run.terminal(), SamplingEvent::Failed { error, .. }
        if error.kind == SamplingErrorKind::ChatStreamInterrupted));
}

fn encode_sse(events: &[SseEvent]) -> String {
    events
        .iter()
        .map(|event| format!("data: {}\n\n", event.data))
        .collect()
}

async fn sample_transport_error(first: Vec<SseEvent>) -> Run {
    let requests = Arc::new(AtomicUsize::new(0));
    let server_requests = Arc::clone(&requests);
    let first = encode_sse(&first);
    let app = Router::new().route(
        PATH,
        post(move || {
            let attempt = server_requests.fetch_add(1, Ordering::Relaxed);
            let first = first.clone();
            async move {
                let body = if attempt == 0 {
                    Body::from_stream(async_stream::stream! {
                        yield Ok(first);
                        // Give the HTTP writer a pending poll so it flushes the
                        // SSE frame before aborting the response body.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        yield Err(std::io::Error::other("fixture interrupted the HTTP body"));
                    })
                } else {
                    Body::from(encode_sse(&tool_frames("verified", FINAL_ARGS)))
                };
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(body)
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let events = collect_events(url, false).await;
    server.abort();
    Run {
        requests: requests.load(Ordering::Relaxed),
        events,
    }
}

#[tokio::test]
async fn broken_http_body_retries_only_before_visible_output() {
    let mut partial_text = sse::chat_completion_script_exact("First draft", MODEL);
    partial_text.truncate(1);
    let mut partial_reasoning = sse::chat_completions_reasoning_then_tool_call_events(
        "thinking",
        "draft",
        "write_file",
        DRAFT_ARGS,
        MODEL,
    );
    partial_reasoning.truncate(1);
    for frames in [partial_text, partial_reasoning] {
        let run = sample_transport_error(frames).await;
        assert_eq!(run.requests, 1);
        assert!(
            run.events
                .iter()
                .any(|event| matches!(event, SamplingEvent::ChannelToken { .. }))
        );
        assert_eq!(run.tool_delta_count(), 0);
        assert!(matches!(run.terminal(), SamplingEvent::Failed { error, .. }
            if error.kind == SamplingErrorKind::ChatStreamInterrupted));
    }

    let mut partial_tool = tool_frames("draft", DRAFT_ARGS);
    partial_tool.truncate(1);
    let run = sample_transport_error(partial_tool).await;
    assert_eq!(run.requests, 2);
    assert_eq!(run.tool_ids(), ["verified"]);
    assert!(
        matches!(run.terminal(), SamplingEvent::Completed { metrics, .. } if metrics.attempts == 2)
    );
}

#[tokio::test]
async fn transport_error_while_reading_trailer_preserves_selected_finish() {
    let mut finished = tool_frames("verified", FINAL_ARGS);
    finished.pop();
    let run = sample_transport_error(finished).await;
    assert_eq!(run.requests, 1);
    assert_eq!(run.tool_ids(), ["verified"]);
    assert!(
        matches!(run.terminal(), SamplingEvent::Completed { response, .. }
        if response.stop_reason == Some(StopReason::ToolCalls))
    );
}

#[tokio::test]
async fn collected_truncation_keeps_its_non_retryable_type() {
    let server = MockInferenceServer::start().await.unwrap();
    let mut frames = sse::chat_completion_script_exact("Partial", MODEL);
    set_finish(&mut frames[0], json!("length"), 0);
    server.enqueue_response(PATH, ScriptedResponse::sse(frames));
    let client = SamplingClient::new(config(server.url())).unwrap();
    let error = client.conversation_collect(request()).await.unwrap_err();
    assert!(matches!(error, SamplingError::MaxTokensTruncation));
    assert!(!error.is_retryable());
    assert_eq!(server.requests().len(), 1);
}
