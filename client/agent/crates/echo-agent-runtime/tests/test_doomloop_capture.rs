//! Mock-HTTP integration test for reasoning-only detection on the Responses
//! API wire — the trigger that drives the model doomloop.
//!
//! Spawns a `MockInferenceServer` that serves a `/v1/responses` SSE stream
//! carrying only reasoning (reasoning summary deltas, no output text, no tool
//! call) and asserts the shell sampling client classifies the collected
//! response as `EmptyReason::ReasoningOnly`. This is the exact check the
//! sampler's retry loop runs on every completed turn to decide to resample
//! (and accumulate the out-of-band streaming-capture segments verified by the
//! actor-level capture test).

mod common;

use common::create_test_client;
use echo_agent_runtime::sampling::{ApiBackend, ConversationItem, ConversationRequest};
use echo_agent_sampling_types::EmptyReason;
use echo_agent_test_support::sse::responses_api_reasoning_only_events;
use echo_agent_test_support::{MockInferenceServer, ScriptedResponse};

/// A `/v1/responses` stream that streams only reasoning and finishes with no
/// visible content must be collected into a response the client classifies as
/// `EmptyReason::ReasoningOnly` — the detection that makes the shell resample
/// and spin the doomloop. Exercises the real SSE/HTTP path
/// (`conversation_collect` -> `stream_responses` -> `collect_response`).
#[tokio::test]
async fn responses_api_reasoning_only_is_classified_as_reasoning_only() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_only_events(
            "let me think carefully about this",
            "echo-agent-test",
        )),
    );
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let request = ConversationRequest::from_items(vec![ConversationItem::user(
        "Solve this without writing anything",
    )]);

    // The stream completes normally — it is just empty — so collect succeeds.
    let response = client
        .conversation_collect(request)
        .await
        .expect("collect must succeed: the stream completes, the response is empty");

    assert_eq!(
        response.empty_reason(),
        Some(EmptyReason::ReasoningOnly),
        "a reasoning-only Responses stream must classify as reasoning_only",
    );
    assert!(response.is_empty());
    // The reasoning sibling survived; the assistant is present but empty.
    assert!(
        response.reasoning_items().next().is_some(),
        "the reasoning item must be collected as a sibling",
    );
    let assistant = response
        .assistant()
        .expect("an empty assistant is synthesized for the turn");
    assert!(
        assistant.content.is_empty(),
        "reasoning-only means the assistant carried no visible content",
    );
}

/// Negative control for the classifier: a normal text `/v1/responses` stream
/// carrying visible assistant content must NOT be classified empty —
/// `empty_reason()` is `None`, distinguishing real content from the
/// reasoning-only case above. (No recovery/resample loop is exercised here.)
#[tokio::test]
async fn normal_text_response_is_not_classified_reasoning_only() {
    let server = MockInferenceServer::start().await.unwrap();
    server.set_response("The answer is 42.");
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let response = client
        .conversation_collect(ConversationRequest::from_items(vec![
            ConversationItem::user("What is the answer?"),
        ]))
        .await
        .unwrap();

    assert!(
        response.empty_reason().is_none(),
        "a normal text turn must not be classified empty",
    );
    let assistant = response
        .assistant()
        .expect("assistant present on a text turn");
    assert!(
        assistant.content.contains("42"),
        "the text turn must carry the model's content, got: {:?}",
        assistant.content,
    );
}
