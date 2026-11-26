//! Regression tests for the session-recap display-only invariant.
//!
//! A recap must NEVER mutate the model conversation — it is generated from a
//! read-only snapshot and surfaced as a notification only. These tests lock
//! that contract: after `handle_recap` returns, `get_conversation()` must be
//! byte-identical to what it was before.

use super::support::*;
use super::*;
use echo_agent_sampling_types::ConversationItem;

/// Serializes `items` the way a main turn would, so auxiliary calls can be compared against the real wire shape.
fn main_turn_input(items: Vec<ConversationItem>) -> Vec<serde_json::Value> {
    let request = echo_agent_sampling_types::ConversationRequest {
        items: echo_agent_chat_state::compaction_utils::ModelRequestHistory::from_raw(items)
            .into_items(),
        model: Some("test-model".to_string()),
        ..Default::default()
    };
    let mapped = async_openai::types::responses::CreateResponse::from(&request);
    serde_json::to_value(&mapped).expect("request serializes")["input"]
        .as_array()
        .expect("input is an array")
        .clone()
}

/// Checks that an auxiliary call replays the parent conversation verbatim and appends one instruction. A prefix that shifts cannot hit the cache.
fn assert_rides_parent_prefix(
    body: &serde_json::Value,
    parent: Vec<ConversationItem>,
    label: &str,
) {
    let expected = main_turn_input(parent);
    let actual = body["input"].as_array().expect("input must be present");
    assert!(
        actual.len() > expected.len(),
        "{label}: auxiliary input ({}) must extend the parent ({})",
        actual.len(),
        expected.len()
    );
    assert_eq!(
        &actual[..expected.len()],
        expected.as_slice(),
        "{label}: prefix diverges from the main turn"
    );
    assert_eq!(
        actual.len(),
        expected.len() + 1,
        "{label}: exactly one appended instruction turn"
    );
}

fn without_cache_control(mut value: serde_json::Value) -> serde_json::Value {
    match &mut value {
        serde_json::Value::Object(fields) => {
            fields.remove("cache_control");
            for value in fields.values_mut() {
                *value = without_cache_control(value.take());
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                *value = without_cache_control(value.take());
            }
        }
        _ => {}
    }
    value
}

fn assert_messages_rides_parent_prefix(
    body: &serde_json::Value,
    parent: Vec<ConversationItem>,
    label: &str,
) {
    let request = echo_agent_sampling_types::ConversationRequest {
        items: echo_agent_chat_state::compaction_utils::ModelRequestHistory::from_raw(parent)
            .into_items(),
        model: Some("test".to_string()),
        reasoning_effort: Some(echo_agent_sampling_types::ReasoningEffort::High),
        ..Default::default()
    };
    let expected =
        serde_json::to_value(echo_agent_sampling_types::build_messages_request(&request))
            .expect("main Messages request serializes");
    let expected_messages = without_cache_control(expected["messages"].clone());
    let actual_messages = without_cache_control(body["messages"].clone());
    let expected = expected_messages
        .as_array()
        .expect("main Messages request has messages");
    let actual = actual_messages
        .as_array()
        .expect("side-call Messages request has messages");

    assert!(
        actual.len() > expected.len(),
        "{label}: side-call Messages request must extend the parent"
    );
    assert_eq!(
        &actual[..expected.len()],
        expected.as_slice(),
        "{label}: Messages prefix diverges from the main turn"
    );
    assert_eq!(
        actual.len(),
        expected.len() + 1,
        "{label}: exactly one instruction message must be appended"
    );
    assert_eq!(body["thinking"]["type"], "adaptive", "{label}: {body:#}");
    assert_eq!(body["output_config"]["effort"], "high", "{label}: {body:#}");
}

fn assert_messages_reasoning_stripped(body: &serde_json::Value, label: &str) {
    assert!(
        body.get("thinking").is_none() || body["thinking"].is_null(),
        "{label}: top-level thinking must be absent: {body:#}"
    );
    for message in body["messages"]
        .as_array()
        .expect("Messages request has messages")
    {
        let Some(blocks) = message["content"].as_array() else {
            continue;
        };
        assert!(
            blocks.iter().all(|block| {
                !matches!(
                    block["type"].as_str(),
                    Some("thinking" | "redacted_thinking")
                )
            }),
            "{label}: replayed thinking must be absent: {body:#}"
        );
    }
}

/// Reasoning effort sits ahead of the conversation in the prompt, so an auxiliary call that drops it diverges from the main turn right away.
#[tokio::test(flavor = "current_thread")]
async fn side_question_projects_agent_messages_without_mutating_history() {
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("an answer");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            let raw = vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::agent_message("agent context"),
            ];
            let raw_bytes = serde_json::to_vec(&raw).unwrap();
            actor.chat_state_handle.replace_conversation(raw);

            actor
                .handle_side_question("what context matters?")
                .await
                .expect("side question must succeed");

            let requests = server.requests();
            let body = requests
                .iter()
                .rev()
                .find(|request| request.path.contains("responses"))
                .and_then(|request| request.body.as_ref())
                .expect("btw body must be JSON")
                .to_string();
            assert!(
                body.contains(echo_agent_chat_state::compaction_utils::AGENT_MESSAGE_MODEL_LABEL)
            );
            assert_eq!(
                serde_json::to_vec(&actor.chat_state_handle.get_conversation().await).unwrap(),
                raw_bytes
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn auxiliary_calls_send_the_session_reasoning_effort() {
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("an answer");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
            // Low is not the model default, so a fallback would show up in the assert below.
            cfg.reasoning_effort = Some(echo_agent_sampling_types::ReasoningEffort::Low);
            actor.chat_state_handle.update_sampling_config(cfg);

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor
                .handle_side_question("what does xor mean here?")
                .await
                .expect("side question must succeed");

            let requests = server.requests();
            let body = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .and_then(|r| r.body.as_ref())
                .expect("btw body must be JSON");
            assert_eq!(
                body["reasoning"]["effort"].as_str(),
                Some("low"),
                "side question must send the session's effort, not the model default: {}",
                body["reasoning"]
            );
        })
        .await;
}

/// When a backend drops `prompt_cache_key`, the conv id is all that ties the call to its conversation, so it must be the parent session id.
#[tokio::test(flavor = "current_thread")]
async fn side_question_routes_on_the_session_id_when_the_key_is_not_forwarded() {
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("an answer");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::ChatCompletions;
            actor.chat_state_handle.update_sampling_config(cfg);

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor
                .handle_side_question("what does xor mean here?")
                .await
                .expect("side question must succeed");

            let requests = server.requests();
            let req = requests.last().expect("a request must be recorded");
            let session_id = actor.session_info.id.to_string();
            assert_eq!(
                req.header("x-echo-agent-conv-id"),
                Some(session_id.as_str()),
                "on a backend that drops the cache key the conv id must be the parent session id"
            );
            let req_id = req
                .header("x-echo-agent-req-id")
                .expect("req id must still be sent");
            assert!(
                req_id.starts_with("echo-agent-btw-"),
                "the btw label moves to the req id: {req_id}"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn new_prompt_cancels_in_flight_recap_epoch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let epoch0 = actor.recap_epoch.get();
            assert!(!actor.recap_was_cancelled(epoch0));

            actor.invalidate_side_calls_for_new_prompt();
            assert!(
                actor.recap_was_cancelled(epoch0),
                "bumping epoch cancels a recap that captured the prior value"
            );
            let epoch1 = actor.recap_epoch.get();
            assert_eq!(epoch1, epoch0.wrapping_add(1));
            assert!(
                !actor.recap_was_cancelled(epoch1),
                "a recap that captures after the bump is still live"
            );
        })
        .await;
}

/// `queue_input` for a real user prompt bumps epoch before any await so a
/// LocalSet recap cannot commit after Prompt accept but before handle_prompt.
#[tokio::test(flavor = "current_thread")]
async fn queue_input_user_prompt_bumps_recap_epoch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let epoch0 = actor.recap_epoch.get();
            let (respond_to, _rx) = tokio::sync::oneshot::channel();
            let _ = actor
                .queue_input(queue_input_request(vec![], "user-next", respond_to))
                .await;
            assert!(
                actor.recap_was_cancelled(epoch0),
                "user queue_input must invalidate in-flight recap epoch"
            );
        })
        .await;
}

/// Synthetic auto-wake must not cancel an in-flight recap.
#[tokio::test(flavor = "current_thread")]
async fn queue_input_synthetic_does_not_bump_recap_epoch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let epoch0 = actor.recap_epoch.get();
            let (respond_to, _rx) = tokio::sync::oneshot::channel();
            let _ = actor
                .queue_input(queue_input_request(
                    vec![],
                    "task-completed-bg-1",
                    respond_to,
                ))
                .await;
            assert_eq!(
                actor.recap_epoch.get(),
                epoch0,
                "synthetic queue_input must leave recap epoch alone"
            );
            assert!(!actor.recap_was_cancelled(epoch0));
        })
        .await;
}

/// A second auto recap must not clear another recap's in-flight claim.
#[tokio::test(flavor = "current_thread")]
async fn skipped_auto_recap_leaves_in_flight_claim() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.recap_in_flight.set(true);
            actor.handle_recap(true).await;
            assert!(
                actor.recap_in_flight.get(),
                "skipped auto recap must not clear another recap's in-flight claim"
            );
        })
        .await;
}

/// Production commit branch: epoch bump mid-flight → no watermark, in-flight cleared.
#[tokio::test(flavor = "current_thread")]
async fn try_commit_recap_cancelled_clears_in_flight_without_watermark() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.last_recap_main_turn.set(2);
            actor.recap_in_flight.set(true);
            let epoch = actor.recap_epoch.get();
            actor.invalidate_side_calls_for_new_prompt();

            assert!(
                !actor.try_commit_recap(epoch, 7),
                "stale epoch must not commit"
            );
            assert_eq!(
                actor.last_recap_main_turn.get(),
                2,
                "cancelled recap must not advance watermark"
            );
            assert!(
                !actor.recap_in_flight.get(),
                "cancelled recap must clear recap_in_flight"
            );
        })
        .await;
}

/// Live epoch commits watermark and clears in-flight (emit path may proceed).
#[tokio::test(flavor = "current_thread")]
async fn try_commit_recap_live_advances_watermark() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.last_recap_main_turn.set(2);
            actor.recap_in_flight.set(true);
            let epoch = actor.recap_epoch.get();

            assert!(actor.try_commit_recap(epoch, 7));
            assert_eq!(actor.last_recap_main_turn.get(), 7);
            assert!(!actor.recap_in_flight.get());
        })
        .await;
}

/// Auto cancel is silent; manual cancel emits SessionRecapUnavailable.
#[tokio::test(flavor = "current_thread")]
async fn drop_recap_after_cancel_auto_silent_manual_unavailable() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.recap_in_flight.set(true);
            actor.drop_recap_after_cancel(true).await;
            assert!(!actor.recap_in_flight.get());
            assert!(
                !drained_recap_unavailable(&mut persistence_rx),
                "auto cancel must not emit SessionRecapUnavailable"
            );
            assert!(
                !drained_session_recap(&mut persistence_rx),
                "auto cancel must not emit SessionRecap"
            );

            actor.recap_in_flight.set(true);
            actor.drop_recap_after_cancel(false).await;
            assert!(!actor.recap_in_flight.get());
            assert!(
                drained_recap_unavailable(&mut persistence_rx),
                "manual cancel must emit SessionRecapUnavailable"
            );
            assert!(
                !drained_session_recap(&mut persistence_rx),
                "manual cancel must not emit SessionRecap"
            );
        })
        .await;
}

/// Drain whether a `SessionRecap` update was emitted.
fn drained_session_recap(rx: &mut tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>) -> bool {
    let mut saw = false;
    while let Ok(msg) = rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(n)) = msg
            && matches!(
                n.update,
                crate::extensions::notification::SessionUpdate::SessionRecap { .. }
            )
        {
            saw = true;
        }
    }
    saw
}

/// Auto recap below `MIN_TURNS_FOR_AUTO_RECAP` is a no-op and display-only.
#[tokio::test(flavor = "current_thread")]
async fn auto_recap_below_min_turns_is_noop_and_display_only() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);
            let before = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                before.len(),
                2,
                "seed must be applied before the recap call"
            );

            actor.handle_recap(true).await;

            let after = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                serde_json::to_string(&before).unwrap(),
                serde_json::to_string(&after).unwrap(),
                "a gated auto recap must not mutate the conversation"
            );
            assert!(
                persistence_rx.try_recv().is_err(),
                "a gated auto recap must emit no notification"
            );
        })
        .await;
}

/// A manual `/recap` passes the gate (when a new main turn exists) and attempts
/// generation; the test's base_url is unreachable so the model call fails.
/// Either way the conversation must be byte-identical afterwards — display-only.
#[tokio::test(flavor = "current_thread")]
async fn manual_recap_never_mutates_conversation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);
            let before = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                before.len(),
                3,
                "seed must be applied before the recap call"
            );

            actor.handle_recap(false).await;

            let after = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                serde_json::to_string(&before).unwrap(),
                serde_json::to_string(&after).unwrap(),
                "manual recap must be display-only"
            );
        })
        .await;
}

/// Drain the persistence channel and report whether a `SessionRecapUnavailable`
/// EchoAgent update was emitted.
fn drained_recap_unavailable(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>,
) -> bool {
    let mut saw = false;
    while let Ok(msg) = rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(n)) = msg
            && matches!(
                n.update,
                crate::extensions::notification::SessionUpdate::SessionRecapUnavailable
            )
        {
            saw = true;
        }
    }
    saw
}

/// A manual `/recap` on a brand-new session (no main turns yet) must NOT strand
/// the client's loading spinner: the recap gate skips before any model call, but
/// instead of silently dropping, the shell emits `SessionRecapUnavailable` so
/// the client can clear it. Deterministic (no-network) repro of the
/// forever-spinner bug.
#[tokio::test(flavor = "current_thread")]
async fn manual_recap_with_no_turns_emits_unavailable() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // No main (user) turns — the gate skips before any model call.
            actor.chat_state_handle.replace_conversation(vec![]);

            actor.handle_recap(false).await;

            assert!(
                drained_recap_unavailable(&mut persistence_rx),
                "manual recap with no turns must emit SessionRecapUnavailable"
            );
        })
        .await;
}

/// A manual `/recap` whose generation fails (the test's base_url is
/// unreachable, so the prepare/model call errors) must also emit
/// `SessionRecapUnavailable` rather than leaving the spinner running.
#[tokio::test(flavor = "current_thread")]
async fn manual_recap_generation_failure_emits_unavailable() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // One main (user) turn clears the recap gate, so the failure comes
            // from the (unreachable) prepare/model call rather than the gate.
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor.handle_recap(false).await;

            assert!(
                drained_recap_unavailable(&mut persistence_rx),
                "a failed manual recap must emit SessionRecapUnavailable"
            );
        })
        .await;
}

/// When the recap model call is attempted (gate passes) but fails, we still
/// persist a `RecapRequest` artifact (with `error` set) for offline replay —
/// same idea as compaction request artifacts on failure.
#[tokio::test(flavor = "current_thread")]
async fn manual_recap_generation_failure_persists_request_artifact() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor.handle_recap(false).await;

            let mut saw_recap_request = false;
            while let Ok(msg) = persistence_rx.try_recv() {
                if let PersistenceMsg::RecapRequest(artifact) = msg {
                    assert_eq!(artifact.trigger, "manual");
                    assert!(
                        artifact.error.is_some(),
                        "failed recap must record error on the artifact"
                    );
                    assert!(
                        artifact.summary.is_none(),
                        "failed recap must not invent a summary"
                    );
                    assert!(
                        !artifact.chat_history.is_empty(),
                        "artifact must include the recap request items"
                    );
                    assert!(
                        artifact
                            .x_echo_agent_req_id
                            .starts_with("echo-agent-recap-"),
                        "req id: {}",
                        artifact.x_echo_agent_req_id
                    );
                    saw_recap_request = true;
                }
            }
            assert!(
                saw_recap_request,
                "failed recap must enqueue PersistenceMsg::RecapRequest"
            );
        })
        .await;
}

/// An automatic recap below the turn gate stays silent — it shows no spinner,
/// so it must NOT emit `SessionRecapUnavailable` (which would be wasted wire
/// traffic and could clear an unrelated manual spinner on another client).
#[tokio::test(flavor = "current_thread")]
async fn auto_recap_gated_does_not_emit_unavailable() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // One main turn (below the auto min-turns gate): the auto path is
            // gated and shows no spinner, so it must stay silent.
            actor
                .chat_state_handle
                .replace_conversation(vec![ConversationItem::user("hi, nothing yet")]);

            actor.handle_recap(true).await;

            assert!(
                !drained_recap_unavailable(&mut persistence_rx),
                "a gated auto recap must not emit SessionRecapUnavailable"
            );
        })
        .await;
}

/// Over-budget recap: the persisted `RecapRequest` is trimmed within budget and
/// the conversation is left unmutated (display-only). Seeds an oversized item so
/// the over-budget branch runs deterministically with no network.
#[tokio::test(flavor = "current_thread")]
async fn manual_recap_over_budget_trims_persisted_request_and_is_display_only() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            // window 8_000 => prompt_budget = 8_000 * 85 / 100 - 4_000 = 2_800.
            const PROMPT_BUDGET: u64 = 8_000 * 85 / 100 - 4_000;
            let actor = create_test_actor(0, 8_000, 85, gateway_tx, persistence_tx).await;

            // An oversized real user turn (~40 KB => ~10k est tokens) forces the
            // over-budget branch regardless of the harness `total_tokens` arg.
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("x".repeat(40_000)),
            ]);
            let before = actor.chat_state_handle.get_conversation().await;

            actor.handle_recap(false).await;

            // Display-only: the conversation is byte-identical afterwards.
            let after = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                serde_json::to_string(&before).unwrap(),
                serde_json::to_string(&after).unwrap(),
                "an over-budget recap must not mutate the conversation"
            );

            // The model call fails (unreachable base_url) → the error arm persists
            // the (trimmed) request artifact.
            let mut saw_recap_request = false;
            while let Ok(msg) = persistence_rx.try_recv() {
                if let PersistenceMsg::RecapRequest(artifact) = msg {
                    let est =
                        echo_agent_chat_state::estimate_conversation_tokens(&artifact.chat_history);
                    assert!(
                        est <= PROMPT_BUDGET,
                        "persisted recap request must be within budget: {est} > {PROMPT_BUDGET}"
                    );
                    assert!(
                        !artifact.chat_history.is_empty(),
                        "the trimmed recap request must be non-empty"
                    );
                    assert!(
                        matches!(
                            artifact.chat_history.last(),
                            Some(ConversationItem::User(_))
                        ),
                        "the recap request must end with the appended User instruction"
                    );
                    saw_recap_request = true;
                }
            }
            assert!(
                saw_recap_request,
                "an over-budget recap must still enqueue a trimmed RecapRequest artifact"
            );
        })
        .await;
}

/// Over-budget recap serializes to a well-formed Anthropic Messages payload:
/// system preserved, reasoning stripped, no dangling `tool_use`/`tool_result`, no
/// `tool_result` before the appended instruction. (Messages is the strictest
/// shape, so it also covers the laxer echoagent ChatCompletions/Responses shapes.)
#[test]
fn over_budget_recap_serializes_to_well_formed_messages_request() {
    use crate::session::helpers::session_recap;
    use echo_agent_sampling_types::messages::{ContentBlock, MessageContent, MessageRole};
    use echo_agent_sampling_types::{ConversationRequest, ToolCall, rs};

    let mk_reasoning = |id: &str| {
        ConversationItem::Reasoning(rs::ReasoningItem {
            id: id.to_string(),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: format!("secret thinking {id}"),
            })],
            content: None,
            encrypted_content: None,
            status: None,
        })
    };
    let mk_call = |id: &str| ToolCall {
        id: std::sync::Arc::from(id),
        name: "read_file".into(),
        arguments: std::sync::Arc::from("{}"),
    };

    // Over-budget (window 8_000) conversation that ENDS in a tool run and carries
    // reasoning; a valid interior tool pair sits behind a non-tool barrier so it
    // survives the trim.
    let conv = vec![
        ConversationItem::system("you are a coding agent"),
        ConversationItem::user("o".repeat(60_000)), // oldest, dropped by trim
        mk_reasoning("r1"),
        ConversationItem::assistant_tool_calls(vec![mk_call("c1")]),
        ConversationItem::tool_result("c1", "fn main() {}"),
        ConversationItem::assistant("done reading the parser"), // non-tool barrier
        ConversationItem::user("what did you change?"),
        ConversationItem::assistant_tool_calls(vec![mk_call("c2")]), // trailing run
        ConversationItem::tool_result("c2", "z".repeat(40_000)),     // trailing run
    ];

    // echoagent backend => strip_reasoning=false; the over-budget branch strips anyway.
    let items = session_recap::budget_recap_items(conv, "system-reminder", false, 8_000);
    let req = ConversationRequest::from_items(items);
    let msg = echo_agent_sampling_types::build_messages_request(&req);

    assert!(msg.system.is_some(), "system prompt must be preserved");

    // Flatten every content block across all messages (each message's content is
    // a `Blocks` vec here).
    let all_blocks: Vec<ContentBlock> = msg
        .messages
        .iter()
        .flat_map(|m| match &m.content {
            MessageContent::Blocks(b) => b.clone(),
            MessageContent::Text(_) => Vec::new(),
        })
        .collect();

    // Reasoning stripped: no thinking block anywhere.
    assert!(
        !all_blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Thinking { .. })),
        "over-budget branch must strip reasoning (no thinking blocks)"
    );

    // Last message is the appended user instruction: role user, text-only.
    let last = msg.messages.last().expect("messages must be non-empty");
    assert!(matches!(last.role, MessageRole::User));
    assert!(
        matches!(&last.content, MessageContent::Blocks(b)
            if b.iter().all(|blk| matches!(blk, ContentBlock::Text { .. }))),
        "the appended instruction message must be text-only (no tool_result/tool_use)"
    );

    // No dangling tool_use: every tool_use id has a matching tool_result id.
    let mut tool_use_ids = std::collections::HashSet::new();
    let mut tool_result_ids = std::collections::HashSet::new();
    for b in &all_blocks {
        match b {
            ContentBlock::ToolUse { id, .. } => {
                tool_use_ids.insert(id.clone());
            }
            ContentBlock::ToolResult { tool_use_id, .. } => {
                tool_result_ids.insert(tool_use_id.clone());
            }
            _ => {}
        }
    }
    assert!(
        tool_use_ids.is_subset(&tool_result_ids),
        "no dangling tool_use: uses={tool_use_ids:?} results={tool_result_ids:?}"
    );
}

/// Recap wire shape: main-turn tools + `prompt_cache_key` = session id, so the
/// request rides the parent turn's prefix cache instead of cold-prefilling.
#[tokio::test(flavor = "current_thread")]
async fn recap_request_rides_parent_prompt_cache() {
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            // Register a real tool so the "recap sends the main turn's tools"
            // assertion is non-vacuous.
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("You asked about the borrow checker.");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor.handle_recap(false).await;

            assert!(
                server.has_responses_request(),
                "recap must hit /v1/responses"
            );
            let requests = server.requests();
            let recap_req = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .expect("a responses request must be recorded");

            let conv_id = recap_req
                .header("x-echo-agent-conv-id")
                .expect("recap must send x-echo-agent-conv-id");
            assert!(
                conv_id.starts_with("recap-"),
                "conv id keeps the recap-* label: {conv_id}"
            );

            let body = recap_req.body.as_ref().expect("recap body must be JSON");
            assert_eq!(
                body["prompt_cache_key"].as_str(),
                Some(actor.session_info.id.to_string().as_str()),
                "prompt_cache_key must be the parent session id for sticky routing"
            );
            let main_turn_specs =
                actor.turn_base_tool_specs(&actor.prepare_tool_definitions().await);
            assert!(!main_turn_specs.is_empty(), "test env must expose tools");
            let tools = body["tools"].as_array().expect("tools must be present");
            assert_eq!(
                tools.len(),
                main_turn_specs.len(),
                "recap must send exactly the main turn's tool specs"
            );
        })
        .await;
}

/// Hosted tools serialize into the token prefix on the Responses path, so a recap in a backend-search session must send the main turn's hosted
/// tools or its prefix diverges and cold-misses the cache.
#[tokio::test(flavor = "current_thread")]
async fn recap_request_sends_hosted_tools_under_backend_search() {
    use echo_agent_sampling_types::HostedTool;
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            // Backend-search fixture: agent carries hosted tools and both gates are on.
            {
                let mut agent_slot = actor.agent.borrow_mut();
                let agent = &*agent_slot;
                *agent_slot = echo_agent_core::Agent::new(
                    agent.definition().clone(),
                    agent.prompt_context().clone(),
                    agent.system_prompt().to_string(),
                    std::sync::Arc::clone(agent.tool_bridge()),
                    agent.reminder_policy().clone(),
                    agent.compaction_policy().clone(),
                    vec![HostedTool::WebSearch { options: None }],
                    true,
                );
            }
            actor.supports_backend_search.set(true);

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("You asked about the borrow checker.");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor.handle_recap(false).await;

            let requests = server.requests();
            let recap_req = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .expect("a responses request must be recorded");
            let body = recap_req.body.as_ref().expect("recap body must be JSON");
            let tools = body["tools"].as_array().expect("tools must be present");

            assert!(
                tools
                    .iter()
                    .any(|t| t["type"].as_str() == Some("web_search")),
                "recap must send the main turn's hosted tools: {tools:?}"
            );
            // Function tools must still match the main turn's specs exactly.
            let main_turn_specs =
                actor.turn_base_tool_specs(&actor.prepare_tool_definitions().await);
            assert!(!main_turn_specs.is_empty(), "test env must expose tools");
            let function_tools = tools
                .iter()
                .filter(|t| t["type"].as_str() == Some("function"))
                .count();
            assert_eq!(
                function_tools,
                main_turn_specs.len(),
                "hosted tools augment, not replace, the main turn's function tools"
            );
        })
        .await;
}

// ── Turn-summary task lifecycle (bail / abort-and-respawn) ──────────────

/// A queued follow-up promoted before the post-turn respawn fires is already
/// running; a snapshot taken now would contain its user message. The entry
/// gate on `current_prompt_id` bails — that turn's completion re-fires. The
/// gate also stays inert when the feature is off.
#[tokio::test(flavor = "current_thread")]
async fn turn_summary_bails_when_newer_turn_already_running() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let disabled = std::sync::Arc::new(
                create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await,
            );
            disabled.restart_turn_summary("pid-off".into());
            assert!(
                disabled.turn_summary_task.borrow().is_none(),
                "feature off: no task spawned"
            );

            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut prx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.turn_summary_enabled = true;
            let actor = std::sync::Arc::new(actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("pid-next".into());

            actor.restart_turn_summary("pid-done".into());

            assert!(
                actor.turn_summary_task.borrow().is_none(),
                "bailed before spawning: the running turn's completion re-fires"
            );
            assert!(
                prx.try_recv().is_err(),
                "no persistence write for a bailed generation"
            );
        })
        .await;
}

/// A real user prompt aborts an in-flight summary generation — its result
/// would describe a conversation the prompt is about to extend. (A newer
/// completion aborts via `restart_turn_summary` the same way.)
#[tokio::test(flavor = "current_thread")]
async fn new_prompt_aborts_in_flight_turn_summary() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.turn_summary_enabled = true;

            // Stand-in for an in-flight generation: a task parked forever.
            let task = tokio::task::spawn_local(std::future::pending::<()>());
            *actor.turn_summary_task.borrow_mut() = Some(task);

            actor.invalidate_side_calls_for_new_prompt();

            assert!(
                actor.turn_summary_task.borrow().is_none(),
                "new prompt must abort the in-flight generation"
            );
        })
        .await;
}

/// Happy path: a successful side-call persists the summary and broadcasts it
/// transiently, then clears the task slot.
#[tokio::test(flavor = "current_thread")]
async fn turn_summary_generate_persists_and_broadcasts() {
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut prx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.turn_summary_enabled = true;
            let actor = std::sync::Arc::new(actor);

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("Fixed the parser race; suite green");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("fix the flaky parser test"),
                ConversationItem::assistant("patched the race and re-ran the suite"),
            ]);

            actor.restart_turn_summary("pid-happy".into());
            assert!(
                actor.turn_summary_task.borrow().is_some(),
                "generation task must be registered"
            );

            // Drive the LocalSet until the task finishes and clears its slot.
            for _ in 0..200 {
                if actor.turn_summary_task.borrow().is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                actor.turn_summary_task.borrow().is_none(),
                "slot must clear when generation finishes"
            );

            let mut found_persist = false;
            while let Ok(msg) = prx.try_recv() {
                if let PersistenceMsg::LastTurnSummary(Some((text, prompt_id))) = msg {
                    assert_eq!(prompt_id, "pid-happy");
                    assert!(
                        text.contains("parser") || text.contains("suite") || !text.is_empty(),
                        "summary text must be non-empty cleaned model output: {text:?}"
                    );
                    found_persist = true;
                }
            }
            assert!(found_persist, "must persist LastTurnSummary with prompt_id");

            let mut found_broadcast = false;
            while let Ok(msg) = grx.try_recv() {
                let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg else {
                    continue;
                };
                if args.request.method.as_ref() != "echo.agent/session_notification" {
                    continue;
                }
                let value: serde_json::Value =
                    serde_json::from_str(args.request.params.get()).expect("params json");
                let update = value.get("update").expect("update object");
                if update.get("sessionUpdate").and_then(|v| v.as_str()) != Some("last_turn_summary")
                {
                    continue;
                }
                assert_eq!(
                    update.get("prompt_id").and_then(|v| v.as_str()),
                    Some("pid-happy")
                );
                let summary = update.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                assert!(!summary.is_empty(), "broadcast summary must be non-empty");
                // Transient path must not stamp eventId (reconnect cursor).
                let meta = value.get("meta");
                assert!(
                    meta.and_then(|m| m.get("eventId")).is_none(),
                    "transient summary must omit eventId: {meta:?}"
                );
                found_broadcast = true;
            }
            assert!(found_broadcast, "must broadcast LastTurnSummary to gateway");
        })
        .await;
}

/// A recap must serialize the main turn's *effective* hosted tools, so an active per-turn cutoff
/// reaches the recap's `x_search` entry rather than an unbounded tool.
#[tokio::test(flavor = "current_thread")]
async fn recap_hosted_tools_reflect_the_active_per_turn_override() {
    use echo_agent_sampling_types::{HostedTool, SearchDateBound, ToolOverrides, XSearchOptions};
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            // Backend-search fixture seeded with an *unbounded* x_search (options: None), so any
            // bound the recap sends can only have come from the per-turn override below.
            {
                let mut agent_slot = actor.agent.borrow_mut();
                let agent = &*agent_slot;
                *agent_slot = echo_agent_core::Agent::new(
                    agent.definition().clone(),
                    agent.prompt_context().clone(),
                    agent.system_prompt().to_string(),
                    std::sync::Arc::clone(agent.tool_bridge()),
                    agent.reminder_policy().clone(),
                    agent.compaction_policy().clone(),
                    vec![HostedTool::XSearch { options: None }],
                    true,
                );
            }
            actor.supports_backend_search.set(true);

            // A per-turn cutoff (toDate only), with no definition seed: the recap must reflect it.
            *actor.tool_overrides.borrow_mut() = Some(ToolOverrides {
                x_search: Some(XSearchOptions {
                    date_bound: Some(
                        SearchDateBound::new(None, Some("2024-03-15".to_string())).unwrap(),
                    ),
                }),
                web_search: None,
            });

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("recap summary");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor.handle_recap(false).await;

            let requests = server.requests();
            let recap_req = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .expect("a responses request must be recorded");
            let body = recap_req.body.as_ref().expect("recap body must be JSON");
            let tools = body["tools"].as_array().expect("tools must be present");
            let x_search = tools
                .iter()
                .find(|t| t["type"].as_str() == Some("x_search"))
                .expect("recap must send the x_search hosted tool");
            assert_eq!(
                x_search["to_date"].as_str(),
                Some("2024-03-15"),
                "recap must serialize the per-turn override's cutoff, not the unbounded seed: {x_search:?}"
            );
        })
        .await;
}

/// A `/btw` call sends the main turn's tools and the session id as `prompt_cache_key`, so it reuses the parent's cached prefix.
#[tokio::test(flavor = "current_thread")]
async fn side_question_request_rides_parent_prompt_cache() {
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("The borrow checker enforces shared-xor-mutable.");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            let answer = actor
                .handle_side_question("what does xor mean here?")
                .await
                .expect("side question must succeed against the mock server");
            assert!(!answer.is_empty());

            assert!(
                server.has_responses_request(),
                "side question must hit /v1/responses"
            );
            let requests = server.requests();
            let btw_req = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .expect("a responses request must be recorded");

            let conv_id = btw_req
                .header("x-echo-agent-conv-id")
                .expect("side question must send x-echo-agent-conv-id");
            assert!(
                conv_id.starts_with("btw-"),
                "conv id keeps the btw-* label: {conv_id}"
            );

            let body = btw_req.body.as_ref().expect("btw body must be JSON");
            assert_eq!(
                body["prompt_cache_key"].as_str(),
                Some(actor.session_info.id.to_string().as_str()),
                "prompt_cache_key must be the parent session id for sticky routing"
            );
            let tools = body["tools"].as_array().expect("tools must be present");

            // The fixture registers `update_goal`, so an empty or unrelated tool list cannot pass.
            let sent: Vec<&str> = tools
                .iter()
                .filter(|t| t["type"] == "function")
                .map(|t| t["name"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(
                sent,
                vec!["update_goal"],
                "side question must send the fixture's main-turn tool"
            );

            // Compare the whole array: name, description, and schema must match the main turn, in order.
            let main_turn_specs =
                actor.turn_base_tool_specs(&actor.prepare_tool_definitions().await);
            let expected: Vec<serde_json::Value> = main_turn_specs
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "type": "function",
                        "name": s.name,
                        "description": s.description,
                        "parameters": s.parameters,
                    })
                })
                .collect();
            assert_eq!(
                tools, &expected,
                "side question tools must equal the main turn's specs verbatim"
            );

            // The main turn sends no hosted search here, so the side question must not add one.
            assert!(
                actor.hosted_tools_for_turn().is_empty(),
                "fixture must have backend search off"
            );
            assert!(
                !tools.iter().any(|t| t["type"] != "function"),
                "no hosted tools may be added to a side question the main turn would not send: {tools:?}"
            );
        })
        .await;
}

/// Recap and `/btw` must replay the parent conversation verbatim and append one instruction. The cache key buys nothing if the prefix moved.
#[tokio::test(flavor = "current_thread")]
async fn auxiliary_calls_keep_the_main_turn_prefix() {
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("a summary");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            // The Responses backend keeps reasoning, so it belongs in the prefix both calls have to reproduce.
            let parent = vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::Reasoning(echo_agent_sampling_types::synthesized_reasoning_item(
                    "recalling the aliasing rules",
                )),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ];
            actor.chat_state_handle.replace_conversation(parent.clone());

            actor
                .handle_side_question("what does xor mean here?")
                .await
                .expect("side question must succeed");
            let requests = server.requests();
            let btw_body = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .and_then(|r| r.body.as_ref())
                .expect("btw body must be JSON");
            assert_rides_parent_prefix(btw_body, parent.clone(), "/btw");

            actor.handle_recap(false).await;
            let requests = server.requests();
            let recap_body = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .and_then(|r| r.body.as_ref())
                .expect("recap body must be JSON");
            assert_rides_parent_prefix(recap_body, parent, "recap");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_side_calls_preserve_completed_reasoning() {
    use echo_agent_sampling_types::{ReasoningEffort, rs};
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.turn_summary_enabled = true;
            actor.title_refresh_enabled = true;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;
            let actor = std::sync::Arc::new(actor);

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("a short summary");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Messages;
            cfg.reasoning_effort = Some(ReasoningEffort::High);
            actor.chat_state_handle.update_sampling_config(cfg);

            let reasoning = |turn: usize| {
                ConversationItem::Reasoning(rs::ReasoningItem {
                    id: String::new(),
                    summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                        text: format!("thinking for turn {turn}"),
                    })],
                    content: None,
                    encrypted_content: Some(format!("signature-{turn}")),
                    status: None,
                })
            };
            let parent = vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("first question"),
                reasoning(1),
                ConversationItem::assistant("first answer"),
                ConversationItem::user("second question"),
                reasoning(2),
                ConversationItem::assistant("second answer"),
                ConversationItem::user("third question"),
                reasoning(3),
                ConversationItem::assistant("third answer"),
            ];
            actor.chat_state_handle.replace_conversation(parent.clone());

            actor
                .handle_side_question("what matters most?")
                .await
                .expect("side question must succeed");
            let body = server
                .requests()
                .into_iter()
                .rev()
                .find(|request| request.path == "/v1/messages")
                .and_then(|request| request.body)
                .expect("/btw Messages body");
            assert_messages_rides_parent_prefix(&body, parent.clone(), "/btw");

            actor.handle_recap(false).await;
            let body = server
                .requests()
                .into_iter()
                .rev()
                .find(|request| request.path == "/v1/messages")
                .and_then(|request| request.body)
                .expect("recap Messages body");
            assert_messages_rides_parent_prefix(&body, parent.clone(), "recap");

            actor.restart_turn_summary("prompt-3".to_string());
            for _ in 0..200 {
                if actor.turn_summary_task.borrow().is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                actor.turn_summary_task.borrow().is_none(),
                "turn summary must finish"
            );
            let body = server
                .requests()
                .into_iter()
                .rev()
                .find(|request| request.path == "/v1/messages")
                .and_then(|request| request.body)
                .expect("turn-summary Messages body");
            assert_messages_rides_parent_prefix(&body, parent.clone(), "turn summary");

            actor.maybe_refresh_title();
            for _ in 0..200 {
                if actor.title_refresh_task.borrow().is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                actor.title_refresh_task.borrow().is_none(),
                "title refresh must finish"
            );
            let body = server
                .requests()
                .into_iter()
                .rev()
                .find(|request| request.path == "/v1/messages")
                .and_then(|request| request.body)
                .expect("title-refresh Messages body");
            assert_messages_rides_parent_prefix(&body, parent, "title refresh");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_side_calls_strip_reasoning_without_supported_thinking_effort() {
    use echo_agent_sampling_types::{ReasoningEffort, synthesized_reasoning_item};
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            for reasoning_effort in [
                None,
                Some(ReasoningEffort::None),
                Some(ReasoningEffort::Minimal),
            ] {
                let (gateway_tx, _grx) =
                    tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
                let (persistence_tx, _prx) =
                    tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
                let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
                actor.turn_summary_enabled = true;
                actor.title_refresh_enabled = true;
                *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;
                let actor = std::sync::Arc::new(actor);

                let server = MockInferenceServer::start().await.unwrap();
                server.set_response("a short summary");
                let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
                cfg.base_url = server.url();
                cfg.api_backend = echo_agent_sampling_types::ApiBackend::Messages;
                cfg.reasoning_effort = reasoning_effort;
                actor.chat_state_handle.update_sampling_config(cfg);

                let parent = vec![
                    ConversationItem::system("you are a coding agent"),
                    ConversationItem::user("first question"),
                    ConversationItem::Reasoning(synthesized_reasoning_item("signed thinking")),
                    ConversationItem::assistant("first answer"),
                    ConversationItem::user("second question"),
                    ConversationItem::Reasoning(synthesized_reasoning_item("more signed thinking")),
                    ConversationItem::assistant("second answer"),
                    ConversationItem::user("third question"),
                    ConversationItem::assistant("third answer"),
                ];
                actor.chat_state_handle.replace_conversation(parent);

                actor
                    .handle_side_question("what matters most?")
                    .await
                    .expect("side question must succeed");
                let body = server
                    .requests()
                    .into_iter()
                    .rev()
                    .find(|request| request.path == "/v1/messages")
                    .and_then(|request| request.body)
                    .expect("/btw Messages body");
                assert_messages_reasoning_stripped(&body, "/btw");

                actor.handle_recap(false).await;
                let body = server
                    .requests()
                    .into_iter()
                    .rev()
                    .find(|request| request.path == "/v1/messages")
                    .and_then(|request| request.body)
                    .expect("recap Messages body");
                assert_messages_reasoning_stripped(&body, "recap");

                actor.restart_turn_summary("prompt-3".to_string());
                for _ in 0..200 {
                    if actor.turn_summary_task.borrow().is_none() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert!(
                    actor.turn_summary_task.borrow().is_none(),
                    "turn summary must finish"
                );
                let body = server
                    .requests()
                    .into_iter()
                    .rev()
                    .find(|request| request.path == "/v1/messages")
                    .and_then(|request| request.body)
                    .expect("turn-summary Messages body");
                assert_messages_reasoning_stripped(&body, "turn summary");

                actor.maybe_refresh_title();
                for _ in 0..200 {
                    if actor.title_refresh_task.borrow().is_none() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert!(
                    actor.title_refresh_task.borrow().is_none(),
                    "title refresh must finish"
                );
                let body = server
                    .requests()
                    .into_iter()
                    .rev()
                    .find(|request| request.path == "/v1/messages")
                    .and_then(|request| request.body)
                    .expect("title-refresh Messages body");
                assert_messages_reasoning_stripped(&body, "title refresh");
            }
        })
        .await;
}

/// A mid-turn `/btw` must not send a reasoning item whose assistant the trim removed, or the request goes out with an unpaired prefix.
#[tokio::test(flavor = "current_thread")]
async fn side_question_trims_reasoning_orphaned_by_mid_turn_truncation() {
    use echo_agent_sampling_types::conversation::{AssistantItem, ToolCall};
    use echo_agent_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("xor means one or the other, not both.");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            // Responses backend keeps reasoning, which is what creates the orphan.
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            // Mid-turn shape: the tool call is still in flight, so the reasoning before it has no result behind it.
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::Reasoning(echo_agent_sampling_types::synthesized_reasoning_item(
                    "planning the file read",
                )),
                ConversationItem::Assistant(AssistantItem {
                    content: String::new().into(),
                    tool_calls: vec![ToolCall {
                        id: "tc1".into(),
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    }],
                    model_id: None,
                    model_fingerprint: None,
                    reasoning_effort: None,
                }),
            ]);

            actor
                .handle_side_question("what does xor mean here?")
                .await
                .expect("side question must succeed against the mock server");

            let requests = server.requests();
            let btw_req = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .expect("a responses request must be recorded");
            let body = btw_req.body.as_ref().expect("btw body must be JSON");
            let input = body["input"].as_array().expect("input must be present");

            let kinds: Vec<&str> = input
                .iter()
                .map(|i| i["type"].as_str().unwrap_or("message"))
                .collect();
            assert!(
                !kinds.contains(&"reasoning"),
                "reasoning orphaned by the mid-turn trim must not be sent: {kinds:?}"
            );
            assert!(
                !kinds.contains(&"function_call"),
                "the in-flight tool call must be trimmed: {kinds:?}"
            );
        })
        .await;
}
