use super::*;
use crate::sampling::{Client, ContentPart, ConversationItem, SamplerConfig, ToolCall, rs};
use crate::session::helpers::prepared_compaction_history::build_compaction_chat_history;
use axum::Router;
use axum::body::Bytes;
use axum::extract::DefaultBodyLimit;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::post;
use echo_agent_chat_state::image_budget::{
    IMAGE_COMPACT_RECLAIM_TARGET_BYTES, IMAGE_COMPACT_TRIGGER_BYTES,
};
use futures_util::stream;
use serde_json::json;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const MIB: usize = 1024 * 1024;
// Mirrors the 50 MiB ingress limit that rejected the incident request.
const TRANSPORT_LIMIT_BYTES: usize = 50 * MIB;
const ENVELOPE_ALLOWANCE_BYTES: usize = MIB;
const IMAGE_PAYLOAD_BYTES: usize = 9 * MIB;
const LARGE_CONTEXT: &str = "large-body-context-sentinel";
const EVICTED_MARKERS: [&str; 4] = ["OLD0", "IMG1", "IMG2", "IMG3"];
const RETAINED_MARKERS: [&str; 2] = ["IMG4", "NEW5"];
const MARKERS: [&str; 6] = ["OLD0", "IMG1", "IMG2", "IMG3", "IMG4", "NEW5"];

#[derive(Default)]
struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn responses_summary_stream() -> Vec<Event> {
    vec![
        Event::default().data(
            json!({
                "type": "response.created",
                "sequence_number": 0,
                "response": {
                    "id": "resp_test", "object": "response", "created_at": 1234567890,
                    "model": "test-model", "status": "in_progress", "output": []
                }
            })
            .to_string(),
        ),
        Event::default().data(
            json!({
                "type": "response.output_text.delta", "sequence_number": 1,
                "item_id": "msg_test", "output_index": 0, "content_index": 0,
                "delta": "<summary>ok</summary>"
            })
            .to_string(),
        ),
        Event::default().data(
            json!({
                "type":"response.completed",
                "sequence_number":2,
                "response": {
                    "id": "resp_test", "object": "response", "created_at": 1234567890,
                    "model": "test-model", "status": "completed", "output": []
                }
            })
            .to_string(),
        ),
    ]
}

fn test_config(base_url: &str) -> SamplerConfig {
    SamplerConfig {
        api_key: Some("test-api-key".into()),
        base_url: base_url.into(),
        model: "test-model".into(),
        max_completion_tokens: Some(1000),
        temperature: Some(0.7),
        api_backend: ApiBackend::Responses,
        context_window: 256_000,
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "large compaction body: allocates and serializes over 50 MiB"]
async fn responses_large_tool_result_images_fit_transport_limit() {
    let tools = vec![ToolSpec {
        name: "read_file".into(),
        description: Some("Reads a file".into()),
        parameters: json!({"type":"object","properties":{}}),
    }];
    let mut source = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("inspect the generated images"),
    ];
    for (index, marker) in MARKERS.iter().enumerate() {
        let call_id = format!("call-{index}");
        source.push(ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: call_id.as_str().into(),
            name: "read_file".into(),
            arguments: format!(r#"{{"target_file":"image-{index}.png"}}"#).into(),
        }]));
        source.push(ConversationItem::tool_result_with_images(
            call_id,
            format!("tool-result-text-{index}"),
            vec![ContentPart::Image {
                url: format!(
                    "data:image/png;base64,{marker}{}",
                    "A".repeat(IMAGE_PAYLOAD_BYTES - marker.len())
                )
                .into(),
            }],
        ));
    }

    let mut unbudgeted_items = source.clone();
    unbudgeted_items.push(ConversationItem::user(build_compaction_prompt(
        Some(LARGE_CONTEXT),
        true,
    )));
    let unbudgeted_request = ConversationRequest {
        items: unbudgeted_items,
        tools: tools.clone(),
        model: Some("test-model".into()),
        ..Default::default()
    };
    let mut unbudgeted_bytes = ByteCounter::default();
    serde_json::to_writer(
        &mut unbudgeted_bytes,
        &rs::CreateResponse::from(&unbudgeted_request),
    )
    .unwrap();
    assert!(unbudgeted_bytes.0 > TRANSPORT_LIMIT_BYTES);
    drop(unbudgeted_request);

    let prepared = build_compaction_chat_history(source.clone(), Some(LARGE_CONTEXT), true, 0);
    let repeated = build_compaction_chat_history(source.clone(), Some(LARGE_CONTEXT), true, 0);
    assert!(prepared.image_budget.body_bytes >= IMAGE_COMPACT_TRIGGER_BYTES);
    assert!(prepared.image_budget.body_bytes_after <= IMAGE_COMPACT_RECLAIM_TARGET_BYTES);
    assert_eq!(prepared.image_budget.evicted, EVICTED_MARKERS.len());
    assert_eq!(
        serde_json::to_value(&prepared.items).unwrap(),
        serde_json::to_value(&repeated.items).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(&prepared.items).unwrap(),
        serde_json::to_vec(&repeated.items).unwrap()
    );

    let captured = Arc::new(Mutex::new(None::<(usize, serde_json::Value)>));
    let cap = captured.clone();
    let app = Router::new().route(
        "/v1/responses",
        post(move |body: Bytes| {
            let cap = cap.clone();
            async move {
                let body_len = body.len();
                if body_len > TRANSPORT_LIMIT_BYTES {
                    return (StatusCode::PAYLOAD_TOO_LARGE, "request too large").into_response();
                }
                let parsed = serde_json::from_slice(&body).unwrap();
                *cap.lock().unwrap() = Some((body_len, parsed));
                let stream = stream::iter(
                    responses_summary_stream()
                        .into_iter()
                        .map(Ok::<_, std::convert::Infallible>),
                );
                Sse::new(stream)
                    .keep_alive(KeepAlive::default())
                    .into_response()
            }
        })
        .layer(DefaultBodyLimit::max(64 * MIB)),
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
    let output = generate_session_compact(
        prepared,
        0,
        tools,
        vec![],
        client,
        acp::SessionId::new("large-body-test"),
        &config,
        std::time::Duration::from_secs(30),
        0,
        crate::util::config::CompactionToolChoice::Auto,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|_| panic!("budgeted compaction request must succeed"));
    assert_eq!(output.content, "<summary>ok</summary>");

    let (wire_bytes, body) = captured.lock().unwrap().take().unwrap();
    assert!(wire_bytes < TRANSPORT_LIMIT_BYTES);
    assert!(wire_bytes <= IMAGE_COMPACT_RECLAIM_TARGET_BYTES + ENVELOPE_ALLOWANCE_BYTES);
    let wire = body.to_string();
    for marker in EVICTED_MARKERS {
        assert!(!wire.contains(marker));
    }
    for marker in RETAINED_MARKERS {
        assert!(wire.contains(marker));
    }
    assert!(wire.contains("tool-result-text-0"));
    assert!(wire.contains("tool-result-text-5"));
    assert!(wire.contains("call-0"));
    assert!(wire.contains("call-5"));
    assert!(wire.contains(LARGE_CONTEXT));
    let tools = body["tools"].as_array().expect("tools must be attached");
    assert!(tools.iter().any(|tool| tool["name"] == "read_file"));
    assert_eq!(
        source
            .iter()
            .filter_map(|item| match item {
                ConversationItem::ToolResult(result) => Some(result.images.len()),
                _ => None,
            })
            .sum::<usize>(),
        MARKERS.len()
    );

    let _ = shutdown_tx.send(());
}
