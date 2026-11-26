//! Chat-history integrity across mid-turn user-message injection.
//!
//! A `push_user_message` (system reminder, interjection, etc.) while an
//! assistant `tool_use` is committed but unanswered makes integrity repair
//! fabricate a `"cancelled by the user"` `tool_result`. If the tool then
//! runs and appends its real result, the conversation has **two**
//! `tool_result`s for one `tool_use_id`. Providers reject that shape with
//! HTTP 400 on every subsequent request — a permanently bricked session
//! with no in-band recovery.
//!
//! The concrete injector that first hit this was the action-stationarity
//! nudge (consecutive identical tool calls). The invariant is broader:
//! **no mid-turn user injection may leave duplicate results for one id.**
//! The nudge is only the driver that reaches the vulnerable window.
//!
//! This is intentionally *not* a unit test of `IdenticalToolCallRun`'s latch
//! (see `identical_tool_call_run_tests` in `turn.rs`). It drives a real
//! turn loop against a scripted model so a reordering that pushes the
//! reminder between `record_assistant_response` and `execute_tool_calls`
//! fails here.

use super::support::*;
use super::*;
use echo_agent_test_support::sse::{
    responses_api_reasoning_then_tool_call_events, responses_api_script_exact,
};
use echo_agent_test_support::{MockInferenceServer, ScriptedResponse};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Derived from the harness's own thresholds so retuning them retunes this suite instead
/// of breaking it. The scripted tool is `todo_write`, which is in the
/// problematically-repeating tier, so this tracks that tier's constants. Script two more
/// calls than the hard stop allows, so the run always ends because the harness stopped it
/// rather than because the script ran dry.
const SCRIPTED_IDENTICAL_CALLS: usize =
    super::turn::MAX_CONSECUTIVE_IDENTICAL_PROBLEMATIC_TOOL_CALLS as usize + 2;
/// A run only reaches the injection window under test once the nudge fires.
const MIN_EXECUTED_IDENTICAL_CALLS: usize =
    super::turn::NUDGE_AFTER_IDENTICAL_PROBLEMATIC_TOOL_CALLS as usize;

const TODO_ARGS: &str = r#"{"todos":[{"id":"t1","content":"poll","status":"completed"}]}"#;

const CANCEL_MARKER: &str = "cancelled by the user";

/// Distinctive fragment of the action-stationarity reminder. Present in both
/// the rendered template and the unrendered fallback string.
const STATIONARITY_NUDGE_MARKER: &str = "stuck in a polling loop";

fn tool_call_sse(call_id: &str) -> ScriptedResponse {
    ScriptedResponse::sse(responses_api_reasoning_then_tool_call_events(
        "poll",
        call_id,
        "todo_write",
        TODO_ARGS,
        "test",
    ))
}

fn drain_gateway(mut rx: tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>) {
    tokio::task::spawn_local(async move {
        while let Some(msg) = rx.recv().await {
            if let echo_agent_acp::AcpClientMessage::SessionNotification(args) = msg {
                let _ = args.response_tx.send(Ok(()));
            }
        }
    });
}

fn drain_persistence(mut rx: tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>) {
    tokio::task::spawn_local(async move {
        while let Some(msg) = rx.recv().await {
            if let PersistenceMsg::FlushAndAck { respond_to } = msg {
                let _ = respond_to.send(Ok(()));
            }
        }
    });
}

/// Group `ToolResult` bodies by `tool_call_id` across the whole conversation
/// (not just the contiguous run after an assistant message). Provider validation
/// is global; a user row between two results for the same id still 400s.
fn tool_results_by_call_id(conv: &[ConversationItem]) -> HashMap<String, Vec<String>> {
    let mut by_id: HashMap<String, Vec<String>> = HashMap::new();
    for item in conv {
        if let ConversationItem::ToolResult(tr) = item {
            by_id
                .entry(tr.tool_call_id.clone())
                .or_default()
                .push(item.text_content());
        }
    }
    by_id
}

/// Mid-turn system reminder (stationarity nudge after 8 identical tool calls)
/// must not fabricate a phantom cancel that duplicates a live tool's result.
///
/// Pre-fix this failed: the nudge was pushed after the assistant `tool_use`
/// was committed and before `execute_tool_calls`, so integrity repair wrote
/// a cancel result and the real result landed beside it under the same id.
#[tokio::test(flavor = "current_thread")]
async fn mid_turn_user_injection_must_not_duplicate_tool_results_for_one_tool_use_id() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = MockInferenceServer::start().await.expect("mock inference server");
            for i in 1..=SCRIPTED_IDENTICAL_CALLS {
                server.enqueue_response(
                    "/v1/responses",
                    tool_call_sse(&format!("stat-call-{i}")),
                );
            }
            server.enqueue_response(
                "/v1/responses",
                ScriptedResponse::sse(responses_api_script_exact("done", "test")),
            );

            let sampling_cfg = echo_agent_sampler::SamplerConfig {
                api_key: Some("test-key".to_string()),
                base_url: server.url(),
                model: "test".to_string(),
                api_backend: echo_agent_sampler::ApiBackend::Responses,
                context_window: 256_000,
                max_retries: Some(0),
                idle_timeout_secs: Some(30),
                ..Default::default()
            };

            let (sampler_event_tx, sampler_event_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_sampler::SamplingEvent>();
            let sampler_handle = echo_agent_sampler::SamplerActor::spawn(
                sampling_cfg,
                echo_agent_sampler::RetryPolicy {
                    max_retries: 0,
                    rate_limit_retry_threshold: 0,
                    ..Default::default()
                },
                sampler_event_tx,
            );

            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            drain_gateway(gateway_rx);
            let (persistence_tx, persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            drain_persistence(persistence_rx);

            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.sampler_handle = sampler_handle;
            *actor.agent.borrow_mut() = test_echo_agent_build_agent_with_todo().await;

            let mut cfg = actor
                .chat_state_handle
                .get_sampling_config()
                .await
                .expect("test actor has sampling config");
            cfg.base_url = server.url();
            cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
            cfg.model = "test".to_string();
            actor.chat_state_handle.update_sampling_config(cfg);
            let mut creds = actor.chat_state_handle.get_credentials().await;
            creds.api_key = Some("test-key".to_string());
            actor.chat_state_handle.update_credentials(creds);

            actor
                .workspace_ops
                .bind_local_session(
                    &actor.session_id_string(),
                    actor.tool_context.cwd.as_path().to_path_buf(),
                    actor.tool_context.hunk_tracker_handle.clone(),
                    actor.agent.borrow().tool_bridge().toolset(),
                    None,
                )
                .expect("bind_local_session");

            let actor = Arc::new(actor);
            {
                let drainer = actor.clone();
                let mut sampler_event_rx = sampler_event_rx;
                tokio::task::spawn_local(async move {
                    while let Some(event) = sampler_event_rx.recv().await {
                        drainer.handle_sampling_event(event).await;
                    }
                });
            }

            let prompt_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(
                "keep polling the same todo".to_string(),
            ))];
            let outcome = tokio::time::timeout(
                Duration::from_secs(60),
                actor.handle_prompt(
                    "chat-history-integrity",
                    prompt_blocks,
                    PromptMode::Agent,
                    None,
                    None,
                    None,
                    None,
                    true,
                    /* send_now */ false,
                    None,
                    None,
                    None,
                ),
            )
            .await
            .expect("turn must finish within timeout");
            assert!(
                outcome.is_ok(),
                "turn must not error: {outcome:?}"
            );

            let conv = actor.chat_state_handle.get_conversation().await;
            let by_id = tool_results_by_call_id(&conv);

            assert!(
                by_id.len() >= MIN_EXECUTED_IDENTICAL_CALLS,
                "expected at least {MIN_EXECUTED_IDENTICAL_CALLS} executed tool calls so the \
                 identical-call run trips the nudge; got {} distinct tool_call_ids. \
                 conversation={conv:#?}",
                by_id.len()
            );

            // Fewer executed than scripted proves the harness stopped the run at the
            // tight tier's limit, i.e. `todo_write` resolved to a problematically
            // repeating kind rather than falling through to the looser thresholds.
            assert!(
                by_id.len() <= super::turn::MAX_CONSECUTIVE_IDENTICAL_PROBLEMATIC_TOOL_CALLS
                    as usize,
                "`todo_write` must be classified in the problematically-repeating tier, so \
                 the run stops at its hard limit; executed {} of {SCRIPTED_IDENTICAL_CALLS} \
                 scripted calls",
                by_id.len()
            );

            for (tool_call_id, results) in &by_id {
                let has_cancel = results.iter().any(|r| r.contains(CANCEL_MARKER));
                let has_real = results.iter().any(|r| !r.contains(CANCEL_MARKER));
                assert!(
                    !(has_cancel && has_real),
                    "tool_use_id `{tool_call_id}` has both a fabricated `{CANCEL_MARKER}` \
                     tool_result and a real execution result. The tool ran; integrity repair \
                     must not claim it was cancelled. A mid-turn user message was pushed while \
                     the tool_use was unanswered. results={results:?}"
                );
                assert_eq!(
                    results.len(),
                    1,
                    "tool_use_id `{tool_call_id}` has {} tool_results. \
                     Duplicate tool_results for one tool_use_id brick the session: the \
                     Anthropic Messages API (and siblings) reject the history with HTTP 400 \
                     on every subsequent request, and there is no in-band recovery. \
                     Mid-turn user messages (nudges, reminders, interjections) must not be \
                     pushed while a tool_use is unanswered — integrity repair fabricates a \
                     cancel result, then the live tool appends a second result. results={results:?}",
                    results.len()
                );
            }

            assert!(
                conv.iter()
                    .any(|item| item.text_content().contains(STATIONARITY_NUDGE_MARKER)),
                "action-stationarity nudge must still be delivered after the identical-call \
                 run; deleting the nudge is not a valid fix for chat-history corruption. \
                 conversation={conv:#?}"
            );
        })
        .await;
}
