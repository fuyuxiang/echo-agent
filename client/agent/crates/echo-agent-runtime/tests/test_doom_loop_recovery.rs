//! Mock-HTTP integration suite for the server-side doom-loop check on the
//! Responses API wire: trigger parsing/dedup onto
//! `ConversationResponse.doom_loop_signals`, the recovery/resample
//! contract, and a headless config-to-header lane.
//!
//! Scripts use `MockInferenceServer`'s FIFO `enqueue_response`: request N
//! consumes script N, so "turn 1 doomed, request 2 is the resample" needs no
//! content-keyed dispatch. Parse tests use non-confident triggers (channel
//! `response`, `low_logprob`, or over-threshold) so they stay orthogonal to
//! the recovery, which acts only on confident signals.

mod common;

use common::{create_test_client, test_sampler_config};
use echo_agent_runtime::sampling::{
    ApiBackend, Client, ConversationItem, ConversationRequest, RequestId, SamplerActor,
    SamplerHandle,
};
use echo_agent_sampler::RetryPolicy;
use echo_agent_sampling_types::doom_loop::{
    DoomLoopSignalKind, SAMPLE_CHECK_EVENT_DATA_CUMULATIVE,
};
use echo_agent_test_support::sse::{
    responses_api_doom_loop_check_events, responses_api_doom_loop_terminal_only_events,
    responses_api_reasoning_and_text_events, responses_api_reasoning_only_events,
    responses_api_reasoning_then_tool_call_events, responses_api_with_doom_loop_frame,
    responses_api_with_doom_loop_frame_after_text, with_doom_loop_frame_before_completed,
    with_doom_loop_frame_before_type, with_terminal_output_items,
};
use echo_agent_test_support::{MockInferenceServer, MockModelEntry, ScriptedResponse};

const MODEL: &str = "test-model";

/// The retry closes with a synthetic user-role reminder. The wording is
/// prompt copy, not a contract — only the role and the envelope are asserted.
#[track_caller]
fn assert_recovery_reminder(item: &serde_json::Value) {
    assert_eq!(item["role"], "user", "the reminder is user-role");
    let text = item["content"].as_str().expect("the reminder is text");
    assert!(
        text.starts_with("<system_reminder>") && text.ends_with("</system_reminder>"),
        "the reminder rides a system-reminder envelope: {text}"
    );
}

fn doom_loop_client(base_url: &str) -> Client {
    let mut config = test_sampler_config(base_url, ApiBackend::Responses, &[]);
    config.doom_loop_recovery = Some(Default::default());
    Client::new(config).unwrap()
}

/// A sampler actor (the rung that owns retry/recovery) with the given
/// doom-loop policy. Events are fire-and-forget, so the receiver is dropped.
fn spawn_actor(base_url: &str, doom_loop_enabled: bool) -> SamplerHandle {
    let mut config = test_sampler_config(base_url, ApiBackend::Responses, &[]);
    if doom_loop_enabled {
        config.doom_loop_recovery = Some(Default::default());
    }
    // Small transport budget so a broken spec fails fast instead of spinning.
    let retry = RetryPolicy {
        max_retries: 2,
        rate_limit_retry_threshold: 2,
        ..RetryPolicy::default()
    };
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    SamplerActor::spawn(config, retry, event_tx)
}

fn user_request(text: &str) -> ConversationRequest {
    ConversationRequest::from_items(vec![ConversationItem::user(text)])
}

fn responses_request_count(server: &MockInferenceServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|e| e.method == "POST" && e.path.contains("/responses"))
        .count()
}

// ---------------------------------------------------------------------------
// Trigger parsing (live)
// ---------------------------------------------------------------------------

/// Mid-stream check frames populate `doom_loop_signals`, deduplicated across
/// the cumulative re-sends, with the label grammar fully parsed.
#[tokio::test]
async fn mid_stream_check_frames_populate_and_dedupe_signals() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_doom_loop_check_events(
            &["tail_repetition:4@response", "tail_repetition:2@response"],
            "around and around we go",
            MODEL,
        )),
    );
    let client = doom_loop_client(&server.url());

    let response = client
        .conversation_collect(user_request("hello"))
        .await
        .expect("a doomed stream still completes");

    let signals = &response.doom_loop_signals;
    assert_eq!(signals.len(), 2, "cumulative re-sends dedupe by raw label");
    assert_eq!(signals[0].kind, DoomLoopSignalKind::TailRepetition(4));
    assert_eq!(signals[0].channel, "response");
    assert_eq!(signals[0].raw, "tail_repetition:4@response");
    assert_eq!(signals[1].kind, DoomLoopSignalKind::TailRepetition(2));
    // The doomed turn shape itself is preserved: reasoning-only.
    assert!(response.assistant_text().is_empty());
}

/// The inference API's byte-exact cumulative frame parses into both signals
/// through the full HTTP/SSE client path.
#[tokio::test]
async fn byte_exact_cumulative_frame_parses_through_the_client() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_with_doom_loop_frame(
            SAMPLE_CHECK_EVENT_DATA_CUMULATIVE,
            "thinking",
            "the answer",
            MODEL,
        )),
    );
    let client = doom_loop_client(&server.url());

    let response = client
        .conversation_collect(user_request("hello"))
        .await
        .unwrap();

    let raws: Vec<&str> = response
        .doom_loop_signals
        .iter()
        .map(|s| s.raw.as_str())
        .collect();
    assert_eq!(
        raws,
        vec!["tail_repetition:4@response", "tail_repetition:2@response"]
    );
    assert!(response.assistant_text().contains("the answer"));
}

/// The terminal-only copy of the signal (no mid-stream frame) also lands on
/// the response.
#[tokio::test]
async fn terminal_only_field_populates_signals() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_doom_loop_terminal_only_events(
            &["low_logprob@thinking"],
            "brief thought",
            "an ordinary answer",
            MODEL,
        )),
    );
    let client = doom_loop_client(&server.url());

    let response = client
        .conversation_collect(user_request("hello"))
        .await
        .unwrap();

    assert_eq!(response.doom_loop_signals.len(), 1);
    assert_eq!(
        response.doom_loop_signals[0].kind,
        DoomLoopSignalKind::LowLogprob
    );
    assert_eq!(response.doom_loop_signals[0].channel, "thinking");
    assert!(response.empty_reason().is_none(), "a normal answer turn");
}

/// Malformed check frames are swallowed: the stream completes normally, the
/// answer is intact, and no signal is recorded.
#[tokio::test]
async fn malformed_check_frames_complete_cleanly_without_signals() {
    let malformed = [
        // triggers as a string
        r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":"tail_repetition:8@thinking"}}"#,
        // triggers as a number
        r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":8}}"#,
        // triggers as an array of objects
        r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":[{"kind":"tail_repetition"}]}}"#,
        // missing doom_loop_check key entirely
        r#"{"type":"response.doom_loop_check","sequence_number":9}"#,
        // not JSON at all (only the SSE event name identifies it)
        "definitely not json",
    ];

    let server = MockInferenceServer::start().await.unwrap();
    for payload in malformed {
        let events = responses_api_with_doom_loop_frame(payload, "hm", "fine", MODEL);
        server.enqueue_response("/v1/responses", ScriptedResponse::sse(events));
    }
    let client = doom_loop_client(&server.url());

    for payload in malformed {
        let response = client
            .conversation_collect(user_request("hello"))
            .await
            .unwrap_or_else(|e| panic!("stream must survive malformed frame {payload}: {e}"));
        assert!(
            response.doom_loop_signals.is_empty(),
            "no signal from malformed frame {payload}"
        );
        assert!(response.assistant_text().contains("fine"));
    }
}

/// Unknown extra keys on a well-formed frame do not impede parsing.
#[tokio::test]
async fn unknown_extra_keys_still_parse() {
    let payload = r#"{"sequence_number":7,"type":"response.doom_loop_check","doom_loop_check":{"triggers":["tail_repetition:4@response"]},"future_field":true}"#;
    let server = MockInferenceServer::start().await.unwrap();
    let events = responses_api_with_doom_loop_frame(payload, "hm", "fine", MODEL);
    server.enqueue_response("/v1/responses", ScriptedResponse::sse(events));

    let response = doom_loop_client(&server.url())
        .conversation_collect(user_request("hello"))
        .await
        .unwrap();
    assert_eq!(response.doom_loop_signals.len(), 1);
    assert_eq!(
        response.doom_loop_signals[0].raw,
        "tail_repetition:4@response"
    );
}

/// No check frame and no terminal field: the signal set stays empty.
#[tokio::test]
async fn absent_field_leaves_signals_empty() {
    let server = MockInferenceServer::start().await.unwrap();
    let events = responses_api_reasoning_and_text_events("thinking", "hello", MODEL);
    server.enqueue_response("/v1/responses", ScriptedResponse::sse(events));

    let response = doom_loop_client(&server.url())
        .conversation_collect(user_request("hello"))
        .await
        .unwrap();
    assert!(response.doom_loop_signals.is_empty());
}

/// Label kinds this client version does not know are preserved verbatim as
/// `Unknown` (never dropped, never an error).
#[tokio::test]
async fn unknown_label_kinds_preserved_as_unknown() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_doom_loop_check_events(
            &["novel_detector:9@thinking"],
            "hmmm",
            MODEL,
        )),
    );

    let response = doom_loop_client(&server.url())
        .conversation_collect(user_request("hello"))
        .await
        .unwrap();
    assert_eq!(response.doom_loop_signals.len(), 1);
    assert_eq!(
        response.doom_loop_signals[0].kind,
        DoomLoopSignalKind::Unknown("novel_detector:9".to_string())
    );
    assert_eq!(
        response.doom_loop_signals[0].raw,
        "novel_detector:9@thinking"
    );
}

/// With the check disabled, the terminal field is never even parsed — the
/// policy gates all signal work, not just the header.
#[tokio::test]
async fn disabled_policy_leaves_terminal_field_unparsed() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_doom_loop_terminal_only_events(
            &["tail_repetition:8@thinking"],
            "thinking",
            "an answer",
            MODEL,
        )),
    );
    let client = create_test_client(&server.url(), ApiBackend::Responses);

    let response = client
        .conversation_collect(user_request("hello"))
        .await
        .unwrap();
    assert!(response.doom_loop_signals.is_empty());
    assert!(response.assistant_text().contains("an answer"));
}

// ---------------------------------------------------------------------------
// Recovery contract (the acceptance spec for the resample behavior)
// ---------------------------------------------------------------------------

/// A confident signal (`tail_repetition:8@thinking` is confident under
/// default `max_threshold` 64) on a completed turn is resampled once: the
/// clean second script is accepted after receiving the complete failed turn
/// followed by the user-role recovery reminder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confident_signal_retries_with_failed_turn_and_reminder() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_doom_loop_terminal_only_events(
            &["tail_repetition:8@thinking"],
            "loop loop loop",
            "poisoned answer",
            MODEL,
        )),
    );
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_and_text_events(
            "fresh thought",
            "clean answer",
            MODEL,
        )),
    );
    let handle = spawn_actor(&server.url(), true);

    let (response, _metrics) = handle
        .submit_and_collect(RequestId::from("doom-confident"), user_request("hello"))
        .await
        .expect("recovery accepts the clean resample");

    assert_eq!(responses_request_count(&server), 2);
    assert_eq!(response.assistant_text(), "clean answer");
    assert!(
        response.doom_loop_signals.is_empty(),
        "the accepted response is the clean resample, not the poisoned turn"
    );
    let bodies = server.request_bodies();
    let retry_input = bodies[1]["input"].as_array().expect("input array");
    assert_eq!(
        retry_input.len(),
        4,
        "original user + failed turn + reminder"
    );
    assert_eq!(retry_input[1]["type"], "reasoning");
    assert_eq!(retry_input[1]["id"], "reasoning_item_1");
    assert_eq!(retry_input[1]["summary"][0]["text"], "loop loop loop");
    assert_eq!(retry_input[2]["role"], "assistant");
    assert_eq!(retry_input[2]["content"], "poisoned answer");
    assert_recovery_reminder(&retry_input[3]);
}

/// Budget exhaustion: with `max_retries` 2, three consecutively doomed turns
/// consume the budget and the LAST doomed response is accepted as-is — the
/// turn still succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budget_exhaustion_accepts_last_doomed_response() {
    let server = MockInferenceServer::start().await.unwrap();
    for attempt in 1..=3 {
        server.enqueue_response(
            "/v1/responses",
            ScriptedResponse::sse(responses_api_doom_loop_terminal_only_events(
                &["tail_repetition:8@thinking"],
                &format!("loop reasoning {attempt}"),
                &format!("looping answer {attempt}"),
                MODEL,
            )),
        );
    }
    let handle = spawn_actor(&server.url(), true);

    let (response, _metrics) = handle
        .submit_and_collect(RequestId::from("doom-budget"), user_request("hello"))
        .await
        .expect("an exhausted budget accepts the response instead of erroring");

    assert_eq!(
        responses_request_count(&server),
        3,
        "initial attempt + max_retries (2) resamples"
    );
    assert_eq!(response.assistant_text(), "looping answer 3");
    assert!(
        !response.doom_loop_signals.is_empty(),
        "the accepted doomed response keeps its signals (warn-only fallback)"
    );

    let bodies = server.request_bodies();
    let second = bodies[1]["input"].as_array().expect("second input");
    assert_eq!(second.len(), 4);
    assert_eq!(second[1]["summary"][0]["text"], "loop reasoning 1");
    assert_eq!(second[2]["content"], "looping answer 1");
    assert_recovery_reminder(&second[3]);

    let third = bodies[2]["input"].as_array().expect("third input");
    assert_eq!(third.len(), 7);
    assert_eq!(third[1]["summary"][0]["text"], "loop reasoning 1");
    assert_eq!(third[2]["content"], "looping answer 1");
    assert_recovery_reminder(&third[3]);
    assert_eq!(third[4]["summary"][0]["text"], "loop reasoning 2");
    assert_eq!(third[5]["content"], "looping answer 2");
    assert_recovery_reminder(&third[6]);
}

/// Non-confident signals never resample: threshold above `max_threshold`,
/// a non-thinking channel, and `low_logprob` are warn-only. The
/// misclassification fence for the recovery's confidence rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn not_confident_signals_do_not_resample() {
    for trigger in [
        "tail_repetition:65@thinking",
        "tail_repetition:2@response",
        "low_logprob@thinking",
    ] {
        let server = MockInferenceServer::start().await.unwrap();
        server.enqueue_response(
            "/v1/responses",
            ScriptedResponse::sse(responses_api_doom_loop_terminal_only_events(
                &[trigger],
                "some thought",
                "kept answer",
                MODEL,
            )),
        );
        let handle = spawn_actor(&server.url(), true);

        let (response, _metrics) = handle
            .submit_and_collect(RequestId::from("doom-lax"), user_request("hello"))
            .await
            .unwrap();

        assert_eq!(
            responses_request_count(&server),
            1,
            "{trigger} is not confident and must not resample"
        );
        assert_eq!(response.assistant_text(), "kept answer");
        assert_eq!(response.doom_loop_signals[0].raw, trigger);
    }
}

/// A disabled policy ignores even a confident signal end-to-end through the
/// actor: one request, field unparsed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_policy_ignores_confident_signal() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_doom_loop_terminal_only_events(
            &["tail_repetition:8@thinking"],
            "loop loop loop",
            "accepted anyway",
            MODEL,
        )),
    );
    let handle = spawn_actor(&server.url(), false);

    let (response, _metrics) = handle
        .submit_and_collect(RequestId::from("doom-disabled"), user_request("hello"))
        .await
        .unwrap();

    assert_eq!(responses_request_count(&server), 1);
    assert_eq!(response.assistant_text(), "accepted anyway");
    assert!(response.doom_loop_signals.is_empty());
}

/// A confident signal arriving mid-stream aborts the attempt and resamples.
/// The early abort itself is not externally assertable; the contract is two
/// requests and the clean final response. The poisoned script carries a
/// visible answer so the existing empty-response retry cannot mask the
/// doom-loop path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mid_stream_signal_aborts_and_resamples() {
    let confident_frame = r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":["tail_repetition:8@thinking"]}}"#;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_with_doom_loop_frame_after_text(
            confident_frame,
            "loop loop loop",
            "poisoned answer",
            MODEL,
        )),
    );
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_and_text_events(
            "fresh thought",
            "clean answer",
            MODEL,
        )),
    );
    let handle = spawn_actor(&server.url(), true);

    let (response, _metrics) = handle
        .submit_and_collect(RequestId::from("doom-midstream"), user_request("hello"))
        .await
        .unwrap();

    assert_eq!(responses_request_count(&server), 2);
    assert_eq!(response.assistant_text(), "clean answer");
    let bodies = server.request_bodies();
    let retry_input = bodies[1]["input"].as_array().expect("input array");
    assert_eq!(
        retry_input.len(),
        4,
        "original user + failed turn + reminder"
    );
    assert_eq!(retry_input[1]["type"], "reasoning");
    assert_eq!(retry_input[1]["id"], "reasoning_item_1");
    assert_eq!(retry_input[1]["content"][0]["text"], "loop loop loop ");
    assert_eq!(retry_input[2]["role"], "assistant");
    assert_eq!(retry_input[2]["content"], "poisoned answer ");
    assert_recovery_reminder(&retry_input[3]);
}

/// A doomed turn that called a tool is dropped whole: Responses reasoning
/// items are bound to the function call that follows them, so replaying the
/// reasoning without the call would send an orphaned item the API rejects.
/// The retry therefore carries the reminder alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doomed_tool_call_turn_retries_with_the_reminder_alone() {
    let confident_frame = r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":["tail_repetition:8@thinking"]}}"#;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(with_doom_loop_frame_before_completed(
            responses_api_reasoning_then_tool_call_events(
                "loop loop loop",
                "call-1",
                "read_file",
                r#"{"path":"a.txt"}"#,
                MODEL,
            ),
            confident_frame,
        )),
    );
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_and_text_events(
            "fresh thought",
            "clean answer",
            MODEL,
        )),
    );
    let handle = spawn_actor(&server.url(), true);

    let (response, _metrics) = handle
        .submit_and_collect(RequestId::from("doom-tool-call"), user_request("hello"))
        .await
        .expect("recovery accepts the clean resample");

    assert_eq!(responses_request_count(&server), 2);
    assert_eq!(response.assistant_text(), "clean answer");
    let bodies = server.request_bodies();
    let retry_input = bodies[1]["input"].as_array().expect("input array");
    assert_eq!(
        retry_input.len(),
        2,
        "original user + reminder, with no orphaned reasoning: {retry_input:?}"
    );
    assert_recovery_reminder(&retry_input[1]);
}

/// The terminal `output` list is what the retry replays, through the real
/// stream: a doomed reasoning item is resent under its own id with its
/// `encrypted_content` intact, not as a synthetic plaintext copy rebuilt from
/// the streamed deltas (which here say something different on purpose).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doomed_turn_replays_the_terminal_reasoning_item_verbatim() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(with_terminal_output_items(
            responses_api_doom_loop_terminal_only_events(
                &["tail_repetition:8@thinking"],
                "streamed approximation",
                "poisoned answer",
                MODEL,
            ),
            vec![
                serde_json::json!({
                    "type": "reasoning",
                    "id": "reasoning_authoritative",
                    "summary": [{ "type": "summary_text", "text": "the real thought" }],
                    "encrypted_content": "cipher-blob",
                    "status": "completed"
                }),
                serde_json::json!({
                    "type": "message",
                    "id": "msg_test",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{ "type": "output_text", "text": "poisoned answer", "annotations": [] }]
                }),
            ],
        )),
    );
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_and_text_events(
            "fresh thought",
            "clean answer",
            MODEL,
        )),
    );
    let handle = spawn_actor(&server.url(), true);

    let (response, _metrics) = handle
        .submit_and_collect(RequestId::from("doom-encrypted"), user_request("hello"))
        .await
        .expect("recovery accepts the clean resample");

    assert_eq!(responses_request_count(&server), 2);
    assert_eq!(response.assistant_text(), "clean answer");
    let bodies = server.request_bodies();
    let retry_input = bodies[1]["input"].as_array().expect("input array");
    assert_eq!(retry_input[1]["type"], "reasoning");
    assert_eq!(
        retry_input[1]["id"], "reasoning_authoritative",
        "the wire's own item id survives the replay"
    );
    assert_eq!(
        retry_input[1]["encrypted_content"], "cipher-blob",
        "the opaque reasoning state is replayed, not dropped for a plaintext copy"
    );
    assert_eq!(retry_input[1]["summary"][0]["text"], "the real thought");
    assert_eq!(retry_input[2]["role"], "assistant");
    assert_eq!(retry_input[2]["content"], "poisoned answer");
    assert_recovery_reminder(&retry_input[3]);
}

/// The tool-call veto reads the raw terminal items, so an MCP turn — which
/// the conversation form drops entirely — is dropped whole rather than
/// replayed without its call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doomed_mcp_turn_retries_with_the_reminder_alone() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(with_terminal_output_items(
            responses_api_doom_loop_terminal_only_events(
                &["tail_repetition:8@thinking"],
                "loop loop loop",
                "poisoned answer",
                MODEL,
            ),
            vec![
                serde_json::json!({
                    "type": "reasoning",
                    "id": "reasoning_item_1",
                    "summary": [{ "type": "summary_text", "text": "loop loop loop" }],
                    "status": "completed"
                }),
                serde_json::json!({
                    "type": "mcp_call",
                    "id": "mcp-1",
                    "name": "search",
                    "server_label": "docs",
                    "arguments": "{}"
                }),
            ],
        )),
    );
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_and_text_events(
            "fresh thought",
            "clean answer",
            MODEL,
        )),
    );
    let handle = spawn_actor(&server.url(), true);

    let (response, _metrics) = handle
        .submit_and_collect(RequestId::from("doom-mcp"), user_request("hello"))
        .await
        .expect("recovery accepts the clean resample");

    assert_eq!(responses_request_count(&server), 2);
    assert_eq!(response.assistant_text(), "clean answer");
    let bodies = server.request_bodies();
    let retry_input = bodies[1]["input"].as_array().expect("input array");
    assert_eq!(
        retry_input.len(),
        2,
        "original user + reminder, with no reasoning orphaned from its MCP call: {retry_input:?}"
    );
    assert_recovery_reminder(&retry_input[1]);
}

/// The abort itself must not lose the tool-call veto: a confident signal that
/// lands on a tool frame drops the stream before the main event handling
/// runs, so the frame is the only notice that a call was in flight. The retry
/// carries the reminder alone rather than reasoning orphaned from its call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mid_stream_abort_on_a_tool_frame_still_vetoes_the_replay() {
    let confident_frame = r#"{"type":"response.doom_loop_check","doom_loop_check":{"triggers":["tail_repetition:8@thinking"]}}"#;
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(with_doom_loop_frame_before_type(
            responses_api_reasoning_then_tool_call_events(
                "loop loop loop",
                "call-1",
                "read_file",
                r#"{"path":"a.txt"}"#,
                MODEL,
            ),
            confident_frame,
            "response.function_call_arguments.delta",
        )),
    );
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_and_text_events(
            "fresh thought",
            "clean answer",
            MODEL,
        )),
    );
    let handle = spawn_actor(&server.url(), true);

    let (response, _metrics) = handle
        .submit_and_collect(RequestId::from("doom-abort-tool"), user_request("hello"))
        .await
        .expect("recovery accepts the clean resample");

    assert_eq!(responses_request_count(&server), 2);
    assert_eq!(response.assistant_text(), "clean answer");
    let bodies = server.request_bodies();
    let retry_input = bodies[1]["input"].as_array().expect("input array");
    assert_eq!(
        retry_input.len(),
        2,
        "original user + reminder, with no reasoning orphaned from the in-flight call: {retry_input:?}"
    );
    assert_recovery_reminder(&retry_input[1]);
}

/// The doom-loop budget and the existing empty-response retry class coexist,
/// one debit each: turn 1 is doomed but NON-empty (confident trigger plus a
/// visible answer), so only the doom class can advance past it; turn 2 is
/// reasoning-only without a trigger, so only the empty class fires; turn 3
/// is the clean accept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doomed_then_reasoning_only_empty_coexist() {
    let server = MockInferenceServer::start().await.unwrap();
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_doom_loop_terminal_only_events(
            &["tail_repetition:8@thinking"],
            "loop loop loop",
            "poisoned answer",
            MODEL,
        )),
    );
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_only_events(
            "empty but not doomed",
            MODEL,
        )),
    );
    server.enqueue_response(
        "/v1/responses",
        ScriptedResponse::sse(responses_api_reasoning_and_text_events(
            "fresh thought",
            "clean answer",
            MODEL,
        )),
    );
    let handle = spawn_actor(&server.url(), true);

    let (response, _metrics) = handle
        .submit_and_collect(RequestId::from("doom-coexist"), user_request("hello"))
        .await
        .expect("both retry classes stay within their budgets");

    assert_eq!(responses_request_count(&server), 3);
    assert_eq!(response.assistant_text(), "clean answer");
    assert!(response.doom_loop_signals.is_empty());
}

// ---------------------------------------------------------------------------
// Headless lifecycle lane
// ---------------------------------------------------------------------------

/// `[doom_loop_recovery] enabled = true` in `config.toml` reaches the wire
/// through the real binary: the session TURN request (marked by
/// `x-echo-agent-turn-idx`) carries the opt-in header. Aux side-queries the binary
/// also fires at `/v1/responses` (e.g. session-title generation) must NOT
/// carry it — they collect without the actor's retry loop, so an armed
/// abort there could only fail them, never resample. The recovery behavior
/// itself is covered by the mock-HTTP suite above.
///
/// `#[ignore]` (needs a built binary). Run locally (auto-builds the pager):
/// ```bash
/// cargo test -p echo-agent-runtime --test test_doom_loop_recovery -- --ignored
/// ```
#[tokio::test]
#[ignore] // requires pre-built binary; run with --ignored
async fn headless_config_enables_doom_loop_check_header() {
    let models = vec![MockModelEntry::new(MODEL).with_api_backend("responses")];
    let server = MockInferenceServer::start_with_models(models)
        .await
        .expect("start mock server");
    let workdir = echo_agent_test_support::git_workdir();
    let sandbox = echo_agent_test_support::TestSandbox::builder()
        .mock_url(server.url())
        .build();

    let echo_agent_home = sandbox.echo_agent_home().to_path_buf();
    std::fs::write(
        echo_agent_home.join("config.toml"),
        "[doom_loop_recovery]\nenabled = true\n",
    )
    .expect("write config.toml");

    let mut cmd = tokio::process::Command::new(echo_agent_test_support::echo_agent_binary());
    cmd.args(["-p", "say hi", "--yolo", "--output-format", "json"])
        .arg("--cwd")
        .arg(workdir.workspace())
        .current_dir(workdir.workspace())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let result = echo_agent_test_support::run_headless_in_sandbox(cmd, sandbox).await;
    echo_agent_test_support::assert_headless_success(
        &result,
        "doom-loop header e2e",
        Some(&server),
    );

    let requests = server.requests();
    let responses_posts: Vec<_> = requests
        .iter()
        .filter(|e| e.method == "POST" && e.path.contains("/responses"))
        .collect();
    // The session turn carries `x-echo-agent-turn-idx`; aux side-queries (session
    // title, etc.) do not.
    let (turns, aux): (Vec<_>, Vec<_>) = responses_posts
        .into_iter()
        .partition(|e| e.header("x-echo-agent-turn-idx").is_some());
    assert!(
        !turns.is_empty(),
        "no session turn POST /v1/responses logged; requests:\n{}",
        server.request_log_summary()
    );
    for turn in turns {
        assert_eq!(
            turn.header("x-echo-agent-doom-loop-check"),
            Some("1024"),
            "[doom_loop_recovery] enabled must reach the turn request header; requests:\n{}",
            server.request_log_summary()
        );
    }
    for side_query in aux {
        assert_eq!(
            side_query.header("x-echo-agent-doom-loop-check"),
            None::<&str>,
            "the session policy must not leak into aux side-query clients; requests:\n{}",
            server.request_log_summary()
        );
    }
}
