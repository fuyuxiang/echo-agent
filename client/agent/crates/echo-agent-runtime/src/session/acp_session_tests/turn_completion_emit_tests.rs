//! Leader-side emission of the durable `TurnCompleted` terminal.
//!
//! These drive the SHIPPED handlers (`handle_completion` for normal + error
//! completions, `cancel_running_task` for cancellation) and assert on the
//! notification the real `send_echo_agent_notification` persists — not a
//! re-implementation. The terminal is the persisted + replayed twin of the
//! fire-and-forget `prompt_complete`, so a re-attaching viewer can finalize
//! from replay.

use super::support::*;
use super::*;

use tokio::sync::mpsc;

/// Drain every persistence message queued so far.
fn drain_persistence(rx: &mut mpsc::UnboundedReceiver<PersistenceMsg>) -> Vec<PersistenceMsg> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        out.push(msg);
    }
    out
}

fn is_turn_completed(m: &PersistenceMsg) -> bool {
    matches!(
        m,
        PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(n))
            if matches!(n.update, EchoAgentSessionUpdate::TurnCompleted { .. })
    )
}

fn is_agent_message_delta(m: &PersistenceMsg) -> bool {
    matches!(
        m,
        PersistenceMsg::Update(crate::session::storage::SessionUpdate::Acp(n))
            if matches!(n.update, acp::SessionUpdate::AgentMessageChunk(_))
    )
}

/// Pull the `(prompt_id, stop_reason, agent_result)` of the first persisted
/// `TurnCompleted`, if any.
fn turn_completed_fields(msgs: &[PersistenceMsg]) -> Option<(String, String, Option<String>)> {
    msgs.iter().find_map(|m| match m {
        PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(n)) => {
            match &n.update {
                EchoAgentSessionUpdate::TurnCompleted {
                    prompt_id,
                    stop_reason,
                    agent_result,
                    ..
                } => Some((prompt_id.clone(), stop_reason.clone(), agent_result.clone())),
                _ => None,
            }
        }
        _ => None,
    })
}

/// A minimal front pending input matching `prompt_id`. `queue_meta` is `None`
/// so `handle_completion` does not also broadcast a `queue/changed`. The
/// completion receiver is returned so the caller keeps it alive.
pub(super) fn pending_input(prompt_id: &str) -> (InputItem, oneshot::Receiver<PromptTurnResult>) {
    let (respond_to, rx) = oneshot::channel();
    let item = InputItem {
        prompt_id: prompt_id.to_string(),
        prompt_blocks: vec![],
        prompt_mode: PromptMode::Agent,
        trace_gcs_config: None,
        artifact_tracker: None,
        client_identifier: None,
        screen_mode: None,
        verbatim: false,
        json_schema: None,
        input_origin: InputOrigin::new(crate::session::PromptOrigin::User),
        task_wake_fallback: None,
        tool_overrides_update: None,
        respond_to,
        persist_ack: None,
        parsed_prompt_tx: None,
        queue_meta: None,
        queue_mutation_policy: QueueMutationPolicy::hidden(),
        send_now: false,
    };
    (item, rx)
}

fn agent_msg_update(text: &str) -> acp::SessionUpdate {
    acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
        acp::TextContent::new(text.to_string()),
    )))
}

#[tokio::test(flavor = "current_thread")]
async fn normal_completion_persists_turn_completed_after_buffered_delta_flush() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            // Buffering enabled with a long window so a streamed delta is HELD
            // in the replay buffer until an explicit flush — the exact state the
            // actor loop is in when a turn's completion arrives.
            let (mut actor, mut event_rx) =
                create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.buffering_settings = Some(BufferingSettings {
                max_items: 100,
                max_bytes: 1_000_000,
                max_duration_ms: 3_600_000,
            });

            // A running turn with its prompt queued at the front.
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("p1".to_string());
            let (item, _rx) = pending_input("p1");
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(item);
            }

            // Stream the turn's last delta through the BUFFERED path: `send_update`
            // enqueues it on `event_tx`, and the replay buffer merges/HOLDS it —
            // it is NOT persisted yet.
            actor
                .send_update(agent_msg_update("last delta"), Some(1))
                .await;

            // Mirror the actor-owned replay buffer: drain the queued event(s)
            // into it so the delta is stranded exactly as it is when a completion
            // reaches `run_session`'s completion branch.
            let mut replay_buffer = ReplayBuffer::new(actor.buffering_settings.clone());
            while let Ok(event) = event_rx.try_recv() {
                if let SessionEvent::Notification(notification) = event {
                    let _ = replay_buffer.consume_chunk(notification);
                }
            }
            assert!(
                persistence_rx.try_recv().is_err(),
                "the buffered delta must not be persisted before the flush"
            );

            // This is the exact flush `run_session`'s completion branch performs
            // before calling `handle_completion` (mirrors the Cancel/Shutdown
            // arms). Removing it leaves the held delta stranded and the terminal
            // would be the only persisted update — the ordering this PR fixes.
            if let Some(notification) = replay_buffer.flush() {
                actor.emit_buffered(notification).await;
            }

            actor
                .handle_completion(
                    "p1".to_string(),
                    Ok(PromptTurnOk {
                        stop_reason: acp::StopReason::EndTurn,
                        total_tokens: 0,
                        turn_snapshot: None,
                        completion_kind: PromptCompletionKind::Completed,
                        structured_output: None,
                        usage: None,
                        tool_overrides: None,
                    }),
                )
                .await;

            let msgs = drain_persistence(&mut persistence_rx);

            // The terminal is persisted with the right fields...
            let (prompt_id, stop_reason, agent_result) = turn_completed_fields(&msgs)
                .expect("a normal completion must persist a TurnCompleted");
            assert_eq!(prompt_id, "p1");
            assert_eq!(stop_reason, "end_turn");
            assert_eq!(agent_result, None);

            // ...after the flushed buffered delta on the same persistence stream.
            let delta_idx = msgs
                .iter()
                .position(is_agent_message_delta)
                .expect("the flushed buffered delta must be persisted");
            let terminal_idx = msgs
                .iter()
                .position(is_turn_completed)
                .expect("the terminal must be persisted");
            assert!(
                delta_idx < terminal_idx,
                "TurnCompleted must land in updates.jsonl after the flushed buffered delta"
            );

            // Limitation: this drives `handle_completion` directly and mirrors
            // the completion-branch flush rather than driving the full
            // `run_session` loop (injecting a real completion would need a
            // mock-model turn). The negative case — a buffered delta NEVER
            // reaching persistence without the flush — is covered by
            // `buffered_chunk_does_not_reach_persistence_without_explicit_flush`
            // in `replay_buffer_send_update_tests`.
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn error_completion_persists_turn_completed_with_error_detail() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("p-err".to_string());
            let (item, _rx) = pending_input("p-err");
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(item);
            }

            actor
                .handle_completion(
                    "p-err".to_string(),
                    Err(acp::Error::internal_error().data("boom")),
                )
                .await;

            let msgs = drain_persistence(&mut persistence_rx);
            let (prompt_id, stop_reason, agent_result) = turn_completed_fields(&msgs)
                .expect("a failed completion must persist a TurnCompleted");
            assert_eq!(prompt_id, "p-err");
            assert_eq!(stop_reason, "error");
            assert_eq!(agent_result.as_deref(), Some("boom"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_persists_turn_completed_cancelled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // A running turn in flight.
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".to_string());
            let (item, _rx) = pending_input("running");
            {
                let mut state = actor.state.lock().await;
                state.running_task = Some(AgentTask {
                    prompt_id: "running".into(),
                    handle: tokio::task::spawn_local(async {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    })
                    .abort_handle(),
                });
                state.pending_inputs.push_back(item);
            }

            let _ = actor
                .cancel_running_task(crate::session::CancelOptions {
                    cancel_subagents: true,
                    trigger: Some(crate::session::CancelTrigger::CtrlC),
                    user_initiated: true,
                    ..Default::default()
                })
                .await;

            let msgs = drain_persistence(&mut persistence_rx);
            let (prompt_id, stop_reason, agent_result) =
                turn_completed_fields(&msgs).expect("a cancel must persist a TurnCompleted");
            assert_eq!(prompt_id, "running");
            assert_eq!(stop_reason, "cancelled");
            assert_eq!(agent_result, None);
            // Ctrl+C also stamps a trigger (informational; only send_now changes client behavior).
            assert_eq!(
                turn_completed_meta(&msgs).and_then(|m| m
                    .get("cancelTrigger")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)),
                Some("ctrl_c".to_string()),
                "a Ctrl+C cancel stamps its trigger on the terminal meta"
            );
        })
        .await;
}

/// Pull the first persisted `TurnCompleted`'s notification `_meta`, if any.
fn turn_completed_meta(msgs: &[PersistenceMsg]) -> Option<serde_json::Value> {
    msgs.iter().find_map(|m| match m {
        PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(n))
            if matches!(n.update, EchoAgentSessionUpdate::TurnCompleted { .. }) =>
        {
            n.meta.clone()
        }
        _ => None,
    })
}

/// Completion-race window: `current_prompt_id` is already cleared (scope-guard
/// drop / `handle_completion` ran first) while the finished front and its task
/// slot are still queued. The cancel identity must come from
/// `running_task.prompt_id`, so the durable `TurnCompleted` (with
/// `cancelTrigger=send_now`) is still persisted — otherwise viewers strand on
/// "Waiting…" with no terminal at all.
#[tokio::test(flavor = "current_thread")]
async fn send_now_cancel_in_completion_race_window_still_persists_turn_completed() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // The pin is already cleared — only the task slot knows the turn.
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = None;
            let (item, _rpc_rx) = pending_input("running");
            {
                let mut state = actor.state.lock().await;
                state.running_task = Some(AgentTask {
                    prompt_id: "running".into(),
                    handle: tokio::task::spawn_local(async {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    })
                    .abort_handle(),
                });
                state.pending_inputs.push_back(item);
            }

            let mut replay_buffer = ReplayBuffer::new(None);
            actor.cancel_turn_for_send_now(&mut replay_buffer).await;

            let msgs = drain_persistence(&mut persistence_rx);
            let (prompt_id, stop_reason, _) = turn_completed_fields(&msgs)
                .expect("a send-now cancel with a cleared pin must still persist a TurnCompleted");
            assert_eq!(prompt_id, "running");
            assert_eq!(stop_reason, "cancelled");
            let meta = turn_completed_meta(&msgs).expect("terminal must carry _meta");
            assert_eq!(
                meta.get("cancelTrigger").and_then(|v| v.as_str()),
                Some("send_now"),
            );
        })
        .await;
}

/// A send-now cancel stamps `_meta.cancelTrigger == "send_now"` on both the
/// durable `TurnCompleted` terminal and the cancelled turn's resolved RPC.
#[tokio::test(flavor = "current_thread")]
async fn send_now_cancel_stamps_cancel_trigger_on_turn_end() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".to_string());
            let (item, rpc_rx) = pending_input("running");
            {
                let mut state = actor.state.lock().await;
                state.running_task = Some(AgentTask {
                    prompt_id: "running".into(),
                    handle: tokio::task::spawn_local(async {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    })
                    .abort_handle(),
                });
                state.pending_inputs.push_back(item);
            }

            // The shipped send-now cancel path.
            let mut replay_buffer = ReplayBuffer::new(None);
            actor.cancel_turn_for_send_now(&mut replay_buffer).await;

            let msgs = drain_persistence(&mut persistence_rx);
            let (prompt_id, stop_reason, _) = turn_completed_fields(&msgs)
                .expect("a send-now cancel must persist a TurnCompleted");
            assert_eq!(prompt_id, "running");
            assert_eq!(stop_reason, "cancelled");
            let meta = turn_completed_meta(&msgs).expect("terminal must carry _meta");
            assert_eq!(
                meta.get("cancelTrigger").and_then(|v| v.as_str()),
                Some("send_now"),
                "the terminal `_meta` must carry cancelTrigger=send_now"
            );

            let mut wire_meta = None;
            while let Ok(msg) = gateway_rx.try_recv() {
                if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
                    && args.request.method.as_ref() == "echo.agent/session_notification"
                    && let Ok(v) =
                        serde_json::from_str::<serde_json::Value>(args.request.params.get())
                    && v["update"]["sessionUpdate"] == "turn_completed"
                {
                    wire_meta = Some(v["_meta"].clone());
                }
            }
            let wire_meta = wire_meta.expect("the TurnCompleted terminal must reach the wire");
            assert_eq!(
                wire_meta["cancelTrigger"], "send_now",
                "wire `_meta.cancelTrigger` must be send_now"
            );

            let result = rpc_rx.await.expect("running turn RPC must resolve");
            match result {
                Ok(PromptTurnOk {
                    stop_reason: acp::StopReason::Cancelled,
                    completion_kind: PromptCompletionKind::Cancelled { context, .. },
                    ..
                }) => {
                    assert_eq!(
                        context.and_then(|c| c.trigger).as_deref(),
                        Some("send_now"),
                        "the running turn's completion context must carry the send-now trigger"
                    );
                }
                other => panic!("expected a Cancelled completion, got {other:?}"),
            }
        })
        .await;
}

/// A hook-denied cancel stamps `_meta.cancellationCategory == "HookDenied"`
/// on the durable `TurnCompleted` terminal — the wire value shipped clients
/// match to render the blocked-by-a-hook copy.
#[tokio::test(flavor = "current_thread")]
async fn hook_denied_cancel_stamps_cancellation_category_on_turn_end() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // A running turn with its prompt queued at the front (owned).
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("p-hook".to_string());
            let (item, _rx) = pending_input("p-hook");
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(item);
            }

            // The completion a `UserPromptSubmit` hook denial resolves the
            // turn with (`TurnOutcome::Cancelled { category: HookDenied }`).
            actor
                .handle_completion(
                    "p-hook".to_string(),
                    Ok(PromptTurnOk {
                        stop_reason: acp::StopReason::Cancelled,
                        total_tokens: 0,
                        turn_snapshot: None,
                        completion_kind: PromptCompletionKind::Cancelled {
                            category: Some(
                                crate::session::events::CancellationCategory::HookDenied,
                            ),
                            context: None,
                        },
                        structured_output: None,
                        usage: None,
                        tool_overrides: None,
                    }),
                )
                .await;

            let msgs = drain_persistence(&mut persistence_rx);
            let (prompt_id, stop_reason, _) = turn_completed_fields(&msgs)
                .expect("a hook-denied cancel must persist a TurnCompleted");
            assert_eq!(prompt_id, "p-hook");
            assert_eq!(stop_reason, "cancelled");
            let meta = turn_completed_meta(&msgs).expect("terminal must carry _meta");
            assert_eq!(
                meta.get("cancellationCategory").and_then(|v| v.as_str()),
                Some("HookDenied"),
                "the terminal `_meta` must pin the hook-denied category encoding"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn no_output_rewind_cancel_emits_no_turn_completed() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // An in-flight turn with no output yet at the front of the queue.
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("rw".to_string());
            let (item, _rx) = pending_input("rw");
            {
                let mut state = actor.state.lock().await;
                state.rewindable = true;
                state.running_task = Some(AgentTask {
                    prompt_id: "rw".into(),
                    handle: tokio::task::spawn_local(async {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    })
                    .abort_handle(),
                });
                state.pending_inputs.push_back(item);
            }

            // A RewindIfNoOutput cancel on a rewindable turn takes the rewind
            // path: the turn is treated as UNSENT, so — in lock-step with the
            // legacy emit_turn_ended — NO durable terminal is emitted (else
            // replay would finalize a turn that was rewound, not completed).
            let _ = actor.cancel_running_task(crate::session::CancelOptions { history: crate::session::CancelHistoryDisposition::RewindIfNoOutput { prompt_id: None }, user_initiated: true, ..Default::default() }).await;

            let msgs = drain_persistence(&mut persistence_rx);
            assert!(
                turn_completed_fields(&msgs).is_none(),
                "a rewind cancel before any output treats the turn as unsent and must persist no TurnCompleted"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn removed_from_queue_completion_emits_no_turn_completed() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("p-removed".to_string());
            let (item, _rx) = pending_input("p-removed");
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(item);
            }

            // A removed queued prompt never started a turn — it must emit no
            // durable terminal even though it resolves with `Cancelled`.
            actor
                .handle_completion(
                    "p-removed".to_string(),
                    Ok(PromptTurnOk {
                        stop_reason: acp::StopReason::Cancelled,
                        total_tokens: 0,
                        turn_snapshot: None,
                        completion_kind: PromptCompletionKind::RemovedFromQueue,
                        structured_output: None,
                        usage: None,
                        tool_overrides: None,
                    }),
                )
                .await;

            let msgs = drain_persistence(&mut persistence_rx);
            assert!(
                turn_completed_fields(&msgs).is_none(),
                "a RemovedFromQueue completion never ran a turn and must emit no terminal"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_prompt_completion_emits_no_turn_completed() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) = mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // Reproduce the cancel race: the Cancel path already finalized and
            // dequeued the turn (current_prompt_id cleared, queue empty), so a
            // stale `(prompt, EndTurn)` completion now lands on the unknown-prompt
            // branch of handle_completion. It must NOT emit a second terminal —
            // the Cancel path already emitted TurnCompleted{cancelled} for it.
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = None;

            actor
                .handle_completion(
                    "already-finalized".to_string(),
                    Ok(PromptTurnOk {
                        stop_reason: acp::StopReason::EndTurn,
                        total_tokens: 0,
                        turn_snapshot: None,
                        completion_kind: PromptCompletionKind::Completed,
                        structured_output: None,
                        usage: None,
                        tool_overrides: None,
                    }),
                )
                .await;

            let msgs = drain_persistence(&mut persistence_rx);
            assert!(
                turn_completed_fields(&msgs).is_none(),
                "a stale completion for a prompt the Cancel path already finalized must NOT \
                 emit a second TurnCompleted (the double-emit bug)"
            );
        })
        .await;
}
