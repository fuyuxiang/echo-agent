use super::support::*;
use super::*;

fn install_client_hook(
    actor: &SessionActor,
    event: echo_agent_hooks::event::HookEventName,
    callback_ids: &[&str],
) {
    let mut client_hooks = crate::extensions::hooks::ClientHooks::new();
    client_hooks.insert(
        event,
        vec![crate::extensions::hooks::ClientHookGroup {
            matcher: None,
            callback_ids: callback_ids.iter().map(|s| s.to_string()).collect(),
            timeout: None,
        }],
    );
    *actor.client_hooks.borrow_mut() = client_hooks;
}

/// Acks UI notifications so `deny_tool` cannot block the gate.
fn spawn_deny_responder(
    gateway_rx: tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
    reason: &'static str,
) {
    let mut gateway_rx = gateway_rx;
    tokio::task::spawn_local(async move {
        while let Some(msg) = gateway_rx.recv().await {
            match msg {
                echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                    let deny: Arc<serde_json::value::RawValue> =
                        serde_json::value::to_raw_value(&serde_json::json!({
                            "decision": "deny",
                            "systemMessage": reason,
                        }))
                        .unwrap()
                        .into();
                    let _ = args.response_tx.send(Ok(acp::ExtResponse::new(deny)));
                }
                echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                    let _ = args.response_tx.send(Ok(()));
                }
                _ => {}
            }
        }
    });
}

/// Client hooks fire even with no on-disk hook registry: `notify_client_hooks`
/// reads `client_hooks` (never `hook_registry`) and its call sites sit outside the
/// file-registry guard.
#[tokio::test(flavor = "current_thread")]
async fn client_hooks_fire_without_file_registry() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            assert!(
                actor.hook_registry.borrow().is_none(),
                "fixture must have no file registry for this invariant"
            );
            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::Stop,
                &["cb_0"],
            );

            actor.fire_hook(
                echo_agent_hooks::event::HookEventName::Stop,
                None,
                echo_agent_hooks::event::HookPayload::Stop {
                    reason: "end_turn".to_string(),
                    stop_hook_active: false,
                    last_assistant_message: None,
                    background_tasks: None,
                    session_crons: None,
                },
            );

            let msg = gateway_rx
                .try_recv()
                .expect("client hook must fire with no file registry");
            let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg else {
                panic!("expected an echo.agent/hooks/event ext notification");
            };
            assert_eq!(args.request.method.as_ref(), "echo.agent/hooks/event");
            let params: serde_json::Value =
                serde_json::from_str(args.request.params.get()).unwrap();
            assert_eq!(params["hookCallbackId"], "cb_0");
            assert_eq!(params["hookEventName"], "stop");
        })
        .await;
}

/// A `use_tool` call whose wire `function.name` is the dispatcher surfaces to PreToolUse
/// hooks as its resolved target, so a matcher keyed on the qualified MCP name
/// (`linear__save_issue`) gates the dispatch. Drives the real `prepare_tool_call` path;
/// the deny fires only if the resolved name reached the envelope.
#[tokio::test(flavor = "current_thread")]
async fn pre_tool_use_resolves_meta_dispatch_tool_name_end_to_end() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            // The toolset must know `use_tool` so it parses to `ToolInput::UseTool`.
            *actor.agent.borrow_mut() = test_agent_with_tools(vec![
                echo_agent_tools::registry::types::ToolConfig::for_tool::<
                    echo_agent_tools::implementations::use_tool::UseTool,
                >(),
            ])
            .await;

            let mut client_hooks = crate::extensions::hooks::ClientHooks::new();
            client_hooks.insert(
                echo_agent_hooks::event::HookEventName::PreToolUse,
                vec![crate::extensions::hooks::ClientHookGroup {
                    matcher: Some(
                        echo_agent_hooks::matcher::HookMatcher::new("linear__save_issue").unwrap(),
                    ),
                    callback_ids: vec!["cb_0".to_string()],
                    timeout: None,
                }],
            );
            *actor.client_hooks.borrow_mut() = client_hooks;
            spawn_deny_responder(gateway_rx, "nope");

            let call = ToolCallResponse {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "use_tool",
                    r#"{"tool_name":"linear__save_issue","tool_input":{}}"#,
                ),
            };

            let mut deferred = Vec::new();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.prepare_tool_call(call, &mut deferred),
            )
            .await
            .expect("prepare_tool_call must not hang")
            .expect("prepare_tool_call must not error");
            assert!(
                matches!(result, Err(ToolLoop::HookDenied { .. })),
                "a hook matched on the resolved tool must gate the use_tool dispatch; \
                 got {result:?}"
            );
        })
        .await;
}

/// Reproduces the prod inheritance seam (subagent.rs `ctx.client_hooks.clone()`) by
/// cloning the parent's hooks into a child `SessionActor`, so the subagent call hits
/// the parent's PreToolUse gate carrying the `subagentType`.
#[tokio::test(flavor = "current_thread")]
async fn subagent_inherits_parent_pre_tool_use_client_hook() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (parent_gateway_tx, _parent_gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (parent_persistence_tx, _parent_persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let parent =
                create_test_actor(0, 256_000, 85, parent_gateway_tx, parent_persistence_tx).await;

            install_client_hook(
                &parent,
                echo_agent_hooks::event::HookEventName::PreToolUse,
                &["cb_0"],
            );

            let (child_gateway_tx, mut child_gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (child_persistence_tx, _child_persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let subagent =
                create_test_actor(0, 256_000, 85, child_gateway_tx, child_persistence_tx).await;

            assert!(
                subagent.client_hooks.borrow().is_empty(),
                "the subagent starts with no hooks of its own"
            );
            *subagent.client_hooks.borrow_mut() = parent.client_hooks.borrow().clone();

            let seen_subagent_type = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
            let seen = seen_subagent_type.clone();
            tokio::task::spawn_local(async move {
                while let Some(msg) = child_gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            let params: serde_json::Value =
                                serde_json::from_str(args.request.params.get()).unwrap();
                            *seen.lock().unwrap() =
                                params["subagentType"].as_str().map(str::to_string);
                            let deny: Arc<serde_json::value::RawValue> =
                                serde_json::value::to_raw_value(&serde_json::json!({
                                    "decision": "deny",
                                }))
                                .unwrap()
                                .into();
                            let _ = args.response_tx.send(Ok(acp::ExtResponse::new(deny)));
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let call = ToolCallResponse {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "run_terminal_command",
                    "{}",
                ),
            };
            let tool_call_id = acp::ToolCallId::new("call_1");
            let envelope = subagent.make_hook_envelope(
                echo_agent_hooks::event::HookEventName::PreToolUse,
                None,
                echo_agent_hooks::event::HookPayload::PreToolUse {
                    tool_name: call.function.name.clone(),
                    tool_use_id: call.id.clone(),
                    tool_input: serde_json::json!({}),
                    tool_input_truncated: false,
                    subagent_type: Some("code-reviewer".to_string()),
                },
            );

            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                subagent.run_pre_tool_use_client_hook(&call, &tool_call_id, &envelope),
            )
            .await
            .expect("the gate must not hang")
            .expect("the gate must not error");

            assert!(
                matches!(result, Some(ToolLoop::HookDenied { .. })),
                "a subagent tool call must be blocked by the parent's inherited PreToolUse hook"
            );
            assert_eq!(
                seen_subagent_type.lock().unwrap().as_deref(),
                Some("code-reviewer"),
                "the parent's hook must observe the subagent's type on the dispatch"
            );
        })
        .await;
}

/// A slow/hung callback must not starve a later deny: with the first-registered callback
/// never replying and the second denying, the gate returns `HookDenied` quickly (a
/// sequential gate would block on the hung one's full timeout). Pins the concurrency claim.
#[tokio::test(flavor = "current_thread")]
async fn pre_tool_use_slow_callback_does_not_starve_a_deny() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::PreToolUse,
                &["slow_cb", "deny_cb"],
            );

            tokio::task::spawn_local(async move {
                let mut held = Vec::new();
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            let params: serde_json::Value =
                                serde_json::from_str(args.request.params.get()).unwrap();
                            if params["hookCallbackId"] == "deny_cb" {
                                let deny: Arc<serde_json::value::RawValue> =
                                    serde_json::value::to_raw_value(&serde_json::json!({
                                        "decision": "deny",
                                    }))
                                    .unwrap()
                                    .into();
                                let _ = args.response_tx.send(Ok(acp::ExtResponse::new(deny)));
                            } else {
                                held.push(args.response_tx);
                            }
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let call = ToolCallResponse {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "run_terminal_command",
                    "{}",
                ),
            };
            let tool_call_id = acp::ToolCallId::new("call_1");
            let envelope = actor.make_hook_envelope(
                echo_agent_hooks::event::HookEventName::PreToolUse,
                None,
                echo_agent_hooks::event::HookPayload::PreToolUse {
                    tool_name: call.function.name.clone(),
                    tool_use_id: call.id.clone(),
                    tool_input: serde_json::json!({}),
                    tool_input_truncated: false,
                    subagent_type: None,
                },
            );

            // 5s ceiling is well under the hung callback's 30s per-callback timeout, so a
            // pass proves the deny was not serialized behind the slow callback.
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_pre_tool_use_client_hook(&call, &tool_call_id, &envelope),
            )
            .await
            .expect("a deny must resolve without waiting on the hung callback")
            .expect("the gate must not error");
            assert!(matches!(result, Some(ToolLoop::HookDenied { .. })));
        })
        .await;
}

/// PostToolUse and PostToolUseFailure must never both fire for one tool call: a hard
/// dispatch error fires only PostToolUseFailure; a successful dispatch fires only
/// PostToolUse.
#[tokio::test(flavor = "current_thread")]
async fn post_tool_use_and_failure_never_double_fire() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            // The agent's tool bridge must know `todo_write` for it to parse + dispatch.
            *actor.agent.borrow_mut() = test_echo_agent_build_agent_with_todo().await;

            let mut client_hooks = crate::extensions::hooks::ClientHooks::new();
            for event in [
                echo_agent_hooks::event::HookEventName::PostToolUse,
                echo_agent_hooks::event::HookEventName::PostToolUseFailure,
            ] {
                client_hooks.insert(
                    event,
                    vec![crate::extensions::hooks::ClientHookGroup {
                        matcher: None,
                        callback_ids: vec!["cb".to_string()],
                        timeout: None,
                    }],
                );
            }
            *actor.client_hooks.borrow_mut() = client_hooks;

            let drain = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<
                echo_agent_acp::AcpClientMessage,
            >| {
                let mut events = Vec::new();
                while let Ok(msg) = rx.try_recv() {
                    if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
                        && args.request.method.as_ref() == "echo.agent/hooks/event"
                    {
                        let params: serde_json::Value =
                            serde_json::from_str(args.request.params.get()).unwrap();
                        if let Some(name) = params["hookEventName"].as_str() {
                            events.push(name.to_string());
                        }
                    }
                }
                events
            };

            let todo_call = |id: &str| crate::sampling::types::ToolCallResponse {
                id: id.to_string(),
                kind: "function".to_string(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "todo_write",
                    r#"{"todos":[{"id":"t1","content":"do","status":"completed"}]}"#,
                ),
            };

            // Failure: no workspace session is bound, so the dispatch hard-errors.
            actor
                .execute_tool_calls(vec![todo_call("call_err")])
                .await
                .expect("execute_tool_calls must not error");
            assert_eq!(
                drain(&mut gateway_rx),
                ["post_tool_use_failure"],
                "an errored tool must fire only PostToolUseFailure, never PostToolUse"
            );

            actor
                .workspace_ops
                .bind_local_session(
                    &actor.session_id_string(),
                    actor.tool_context.cwd.as_path().to_path_buf(),
                    actor.tool_context.hunk_tracker_handle.clone(),
                    actor.agent.borrow().tool_bridge().toolset(),
                    None,
                )
                .expect("bind_local_session must succeed");
            actor
                .execute_tool_calls(vec![todo_call("call_ok")])
                .await
                .expect("execute_tool_calls must not error");
            assert_eq!(
                drain(&mut gateway_rx),
                ["post_tool_use"],
                "a successful tool must fire PostToolUse exactly once, never PostToolUseFailure"
            );
        })
        .await;
}

/// A `pre_tool_use` deny must NOT cancel the turn: `execute_tool_calls` feeds the deny
/// reason back as the blocked tool's `tool_result` and returns `ToolLoop::Continue`, so
/// the model re-samples with the reason in context.
///
/// Regression guard: the deny once surfaced as `ToolLoop::HookDenied`, which
/// `execute_tool_calls` treated as a terminal result, cancelling the whole turn.
#[tokio::test(flavor = "current_thread")]
async fn pre_tool_use_deny_feeds_reason_back_and_continues_turn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            // The tool bridge must know `todo_write` so the call reaches the gate
            // rather than short-circuiting as an unknown tool.
            *actor.agent.borrow_mut() = test_echo_agent_build_agent_with_todo().await;

            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::PreToolUse,
                &["cb_0"],
            );
            spawn_deny_responder(gateway_rx, "use read_file instead");

            let call = ToolCallResponse {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: crate::sampling::types::ToolCallFunction::new(
                    "todo_write",
                    r#"{"todos":[{"id":"t1","content":"do","status":"completed"}]}"#,
                ),
            };

            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.execute_tool_calls(vec![call]),
            )
            .await
            .expect("execute_tool_calls must not hang")
            .expect("execute_tool_calls must not error");

            assert!(
                matches!(result, ToolLoop::Continue),
                "a pre_tool_use deny must continue the turn, got {result:?}"
            );

            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                conv.iter()
                    .any(|c| c.text_content().contains("use read_file instead")),
                "the deny reason must be fed back as the tool_result"
            );
        })
        .await;
}

/// The Stop client gate collects every deny as a block (no short-circuit).
#[tokio::test(flavor = "current_thread")]
async fn stop_client_gate_collects_denies() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::Stop,
                &["cb_block", "cb_allow"],
            );

            tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            let params: serde_json::Value =
                                serde_json::from_str(args.request.params.get()).unwrap();
                            let response = if params["hookCallbackId"] == "cb_block" {
                                serde_json::json!({
                                    "decision": "deny",
                                    "systemMessage": "finish the tests first",
                                })
                            } else {
                                serde_json::json!({})
                            };
                            let response_params: Arc<serde_json::value::RawValue> =
                                serde_json::value::to_raw_value(&response).unwrap().into();
                            let _ = args
                                .response_tx
                                .send(Ok(acp::ExtResponse::new(response_params)));
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let envelope = actor.make_hook_envelope(
                echo_agent_hooks::event::HookEventName::Stop,
                Some("prompt-1".to_string()),
                echo_agent_hooks::event::HookPayload::Stop {
                    reason: "end_turn".to_string(),
                    stop_hook_active: true,
                    last_assistant_message: Some("I'm done".to_string()),
                    background_tasks: None,
                    session_crons: None,
                },
            );
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_stop_client_hooks(&envelope),
            )
            .await
            .expect("the stop gate must not hang");

            assert_eq!(result.blocks.len(), 1, "only the denying callback blocks");
            assert_eq!(result.blocks[0].hook_name, "client:cb_block");
            assert_eq!(result.blocks[0].reason, "finish the tests first");
            assert!(result.prevent_continuation.is_none());
            assert!(result.additional_context.is_empty());
        })
        .await;
}

/// `continue: false` becomes a force-stop (with `stopReason`) and `additionalContext`
/// becomes non-error feedback, matching what file hooks express.
#[tokio::test(flavor = "current_thread")]
async fn stop_client_gate_carries_continue_false_and_context() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::Stop,
                &["cb_stop", "cb_ctx"],
            );

            tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            let params: serde_json::Value =
                                serde_json::from_str(args.request.params.get()).unwrap();
                            let response = if params["hookCallbackId"] == "cb_stop" {
                                serde_json::json!({ "continue": false, "stopReason": "budget" })
                            } else {
                                serde_json::json!({ "additionalContext": "run the linter" })
                            };
                            let response_params: Arc<serde_json::value::RawValue> =
                                serde_json::value::to_raw_value(&response).unwrap().into();
                            let _ = args
                                .response_tx
                                .send(Ok(acp::ExtResponse::new(response_params)));
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let envelope = actor.make_hook_envelope(
                echo_agent_hooks::event::HookEventName::Stop,
                Some("prompt-1".to_string()),
                echo_agent_hooks::event::HookPayload::Stop {
                    reason: "end_turn".to_string(),
                    stop_hook_active: false,
                    last_assistant_message: None,
                    background_tasks: None,
                    session_crons: None,
                },
            );
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_stop_client_hooks(&envelope),
            )
            .await
            .expect("the stop gate must not hang");

            assert!(result.blocks.is_empty());
            let prevent = result
                .prevent_continuation
                .expect("continue:false captured");
            assert_eq!(prevent.hook_name, "client:cb_stop");
            assert_eq!(prevent.reason, "budget");
            assert_eq!(result.additional_context, ["run the linter"]);
        })
        .await;
}

/// End-to-end through `run_stop_gate`: a client deny becomes `KeepWorking`, no hooks
/// allows the stop, and the consecutive-block cap overrides the gate.
#[tokio::test(flavor = "current_thread")]
async fn run_stop_gate_keep_working_and_cap() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let decision = actor.run_stop_gate("prompt-1", 0).await;
            assert!(matches!(decision, StopGateDecision::AllowStop));

            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::Stop,
                &["cb_0"],
            );

            spawn_deny_responder(gateway_rx, "keep working");

            let decision = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_stop_gate("prompt-1", 0),
            )
            .await
            .expect("the stop gate must not hang");
            match decision {
                StopGateDecision::KeepWorking { feedback } => {
                    assert!(
                        feedback.contains("Stop hook feedback:")
                            && feedback.contains("keep working"),
                        "feedback must carry the deny message, got: {feedback}"
                    );
                }
                _ => panic!("a client deny must keep the agent working"),
            }

            let decision = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_stop_gate("prompt-1", MAX_STOP_HOOK_CONTINUATIONS_PER_TURN),
            )
            .await
            .expect("the capped gate must not hang");
            assert!(matches!(decision, StopGateDecision::AllowStop));
        })
        .await;
}

pub(super) fn file_registry_with_spec(
    event: echo_agent_hooks::event::HookEventName,
    script: &str,
) -> echo_agent_hooks::discovery::HookRegistry {
    file_registry(event, script, true)
}

fn file_registry(
    event: echo_agent_hooks::event::HookEventName,
    script: &str,
    enabled: bool,
) -> echo_agent_hooks::discovery::HookRegistry {
    let (mut registry, _) = echo_agent_hooks::discovery::load_hooks(None, None);
    registry.append_specs(vec![echo_agent_hooks::config::HookSpec {
        name: "test/stop-hook".into(),
        event,
        handler_type: echo_agent_hooks::config::HandlerType::Command,
        configured_matcher: None,
        matcher: None,
        enabled,
        command: Some(std::path::PathBuf::from(script)),
        command_raw: Some(script.to_string()),
        url: None,
        url_raw: None,
        timeout_ms: 5000,
        source_dir: std::path::PathBuf::from("/tmp"),
        extra_env: std::collections::HashMap::new(),
        layer: echo_agent_hooks::config::HookProvenance::File,
    }]);
    registry
}

/// A file-hook force-stop skips the client run gate (its signals would be discarded)
/// but still delivers the observe `echo.agent/hooks/event` notification.
#[tokio::test(flavor = "current_thread")]
async fn file_force_stop_skips_client_gate_but_notifies() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.hook_resolved_workspace_root = "/tmp".to_string();

            *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(file_registry_with_spec(
                echo_agent_hooks::event::HookEventName::Stop,
                r#"echo '{"continue":false,"stopReason":"budget exhausted"}'"#,
            )));
            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::Stop,
                &["cb_observer"],
            );

            let run_requests = std::rc::Rc::new(std::cell::Cell::new(0u32));
            let observe_events = std::rc::Rc::new(std::cell::Cell::new(0u32));
            let (runs, observes) = (run_requests.clone(), observe_events.clone());
            tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            if args.request.method.as_ref() == "echo.agent/hooks/run" {
                                runs.set(runs.get() + 1);
                            }
                            let empty: Arc<serde_json::value::RawValue> =
                                serde_json::value::to_raw_value(&serde_json::json!({}))
                                    .unwrap()
                                    .into();
                            let _ = args.response_tx.send(Ok(acp::ExtResponse::new(empty)));
                        }
                        echo_agent_acp::AcpClientMessage::ExtNotification(args) => {
                            if args.request.method.as_ref() == "echo.agent/hooks/event" {
                                observes.set(observes.get() + 1);
                            }
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let decision = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_stop_gate("prompt-1", 0),
            )
            .await
            .expect("the stop gate must not hang");
            assert!(
                matches!(decision, StopGateDecision::AllowStop),
                "a file force-stop must end the turn"
            );
            // Yield so the fire-and-forget notification lands.
            tokio::task::yield_now().await;
            assert_eq!(run_requests.get(), 0, "the client run gate must be skipped");
            assert_eq!(
                observe_events.get(),
                1,
                "client callbacks must still see the turn end as an observe event"
            );
            // A forced stop still ran its hooks, so it spends the report: dropping the commit
            // here would let a cancel during teardown file a second one.
            assert!(actor.turn_report.claim_for_gate().is_none());
        })
        .await;
}

/// Two client callbacks both force-stop; attribution follows registration order even
/// when that callback responds last (completion order must not decide it).
#[tokio::test(flavor = "current_thread")]
async fn client_force_stop_attribution_is_registration_ordered() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::Stop,
                &["cb_first", "cb_second"],
            );

            tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            let params: serde_json::Value =
                                serde_json::from_str(args.request.params.get()).unwrap();
                            let is_first = params["hookCallbackId"] == "cb_first";
                            tokio::task::spawn_local(async move {
                                if is_first {
                                    // The registration-order winner replies last.
                                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                                }
                                let reason = if is_first {
                                    "from-first"
                                } else {
                                    "from-second"
                                };
                                let body: Arc<serde_json::value::RawValue> =
                                    serde_json::value::to_raw_value(&serde_json::json!({
                                        "continue": false,
                                        "stopReason": reason,
                                    }))
                                    .unwrap()
                                    .into();
                                let _ = args.response_tx.send(Ok(acp::ExtResponse::new(body)));
                            });
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let envelope = actor.make_hook_envelope(
                echo_agent_hooks::event::HookEventName::Stop,
                Some("prompt-1".to_string()),
                echo_agent_hooks::event::HookPayload::Stop {
                    reason: "end_turn".to_string(),
                    stop_hook_active: false,
                    last_assistant_message: None,
                    background_tasks: None,
                    session_crons: None,
                },
            );
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_stop_client_hooks(&envelope),
            )
            .await
            .expect("the stop gate must not hang");

            let prevent = result.prevent_continuation.expect("force-stop captured");
            assert_eq!(
                prevent.hook_name, "client:cb_first",
                "attribution must follow registration order, not completion order"
            );
            assert_eq!(prevent.reason, "from-first");
        })
        .await;
}

/// A subagent session gates on `SubagentStop` specs (not `Stop`), with the gate-phase
/// payload.
#[tokio::test(flavor = "current_thread")]
async fn subagent_session_gates_on_subagent_stop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.startup_hints.is_subagent = true;
            actor.hook_resolved_workspace_root = "/tmp".to_string();

            *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(file_registry_with_spec(
                echo_agent_hooks::event::HookEventName::SubagentStop,
                r#"echo '{"decision":"block","reason":"verify the summary"}'"#,
            )));

            tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    if let echo_agent_acp::AcpClientMessage::SessionNotification(args) = msg {
                        let _ = args.response_tx.send(Ok(()));
                    }
                }
            });

            let decision = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_stop_gate("prompt-1", 0),
            )
            .await
            .expect("the subagent stop gate must not hang");
            match decision {
                StopGateDecision::KeepWorking { feedback } => {
                    assert!(
                        feedback.contains("verify the summary"),
                        "the SubagentStop block reason must become feedback, got: {feedback}"
                    );
                }
                other => {
                    panic!("a SubagentStop block must keep the subagent working, got {other:?}")
                }
            }
        })
        .await;
}

/// Alias fire sites serialize the canonical event name: a `SubagentEnd` envelope reads
/// `"subagent_stop"` on the wire, matching `ECHO_AGENT_HOOK_EVENT`.
#[tokio::test(flavor = "current_thread")]
async fn alias_envelope_serializes_canonical_event_name() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let envelope = actor.make_hook_envelope(
                echo_agent_hooks::event::HookEventName::SubagentEnd,
                None,
                echo_agent_hooks::event::HookPayload::SubagentStop {
                    phase: echo_agent_hooks::event::SubagentStopPhase::Observe,
                    subagent_id: "sub-1".into(),
                    subagent_type: "explore".into(),
                    stop_hook_active: None,
                    last_assistant_message: None,
                },
            );
            let value = serde_json::to_value(&envelope).expect("envelope serializes");
            assert_eq!(value["hookEventName"], "subagent_stop");
            // The test actor runs yolo, so permissionMode pins that state.
            assert_eq!(value["permissionMode"], "bypassPermissions");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn client_force_stop_reports_the_turn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::Stop,
                &["cb_stop"],
            );

            tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            let response =
                                serde_json::json!({ "continue": false, "stopReason": "budget" });
                            let params: Arc<serde_json::value::RawValue> =
                                serde_json::value::to_raw_value(&response).unwrap().into();
                            let _ = args.response_tx.send(Ok(acp::ExtResponse::new(params)));
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let decision = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_stop_gate("prompt-1", 0),
            )
            .await
            .expect("the stop gate must not hang");
            assert!(matches!(decision, StopGateDecision::AllowStop));
            assert!(actor.turn_report.claim_for_gate().is_none());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_unanswered_client_gate_leaves_the_report_unspent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::Stop,
                &["cb_stop"],
            );

            // Drop the response channel unanswered: a transport error, not a decision.
            tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    if let echo_agent_acp::AcpClientMessage::ExtMethod(args) = msg {
                        drop(args.response_tx);
                    }
                }
            });

            let decision = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_stop_gate("prompt-1", 0),
            )
            .await
            .expect("the stop gate must not hang");

            assert!(matches!(decision, StopGateDecision::AllowStop));
            assert!(
                actor.turn_report.claim_for_gate().is_some(),
                "a gate whose only hook never answered must leave the turn reportable"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_reported_turn_skips_the_gate_entirely() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.hook_resolved_workspace_root = "/tmp".to_string();
            *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(file_registry_with_spec(
                echo_agent_hooks::event::HookEventName::Stop,
                "exit 2",
            )));

            let claim = actor
                .turn_report
                .claim_for_gate()
                .expect("the turn is fresh");
            assert_eq!(
                claim.commit(),
                super::turn_report_slot::CommitOutcome::Reported,
                "an interrupt reported this turn"
            );

            let decision = actor.run_stop_gate("prompt-1", 0).await;
            assert!(
                matches!(decision, StopGateDecision::AllowStop),
                "a hook that ran would have blocked the stop"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn only_a_completed_stop_hook_is_the_turns_report() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.hook_resolved_workspace_root = "/tmp".to_string();
            // A report only counts once it reaches the queue, so the turn needs one.
            let actor = std::sync::Arc::new(actor);
            let _queue = super::turn_end_hooks::TurnEndQueue::spawn(actor.clone());
            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::StopFailure,
                &["cb_fail"],
            );
            // Registered with nothing to deliver, so only the file hook can produce a result.
            actor.client_hooks.borrow_mut().insert(
                echo_agent_hooks::event::HookEventName::Stop,
                vec![crate::extensions::hooks::ClientHookGroup {
                    matcher: None,
                    callback_ids: vec![],
                    timeout: None,
                }],
            );

            use super::turn_end_hooks::ReportOutcome;
            for (case, script, enabled, reports_later) in [
                (
                    "disabled, so skipped",
                    "exit 0",
                    false,
                    ReportOutcome::Queued,
                ),
                ("crashed", "exit 1", true, ReportOutcome::Queued),
                ("ran", "exit 0", true, ReportOutcome::AlreadyReported),
            ] {
                actor.turn_report.start_next_turn();
                *actor.hook_registry.borrow_mut() = Some(std::sync::Arc::new(file_registry(
                    echo_agent_hooks::event::HookEventName::Stop,
                    script,
                    enabled,
                )));

                let decision = actor.run_stop_gate(case, 0).await;
                assert!(matches!(decision, StopGateDecision::AllowStop), "{case}");
                let reported = actor.claim_and_queue(
                    case,
                    actor.turn_report.epoch(),
                    super::turn_end_hooks::TurnEnd::Failed {
                        error: echo_agent_hooks::event::StopFailureKind::Unknown,
                        error_details: None,
                        last_assistant_message: None,
                    },
                );
                assert_eq!(reported, reports_later, "{case}");
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_gate_that_keeps_working_releases_the_report() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            install_client_hook(
                &actor,
                echo_agent_hooks::event::HookEventName::Stop,
                &["cb_block"],
            );
            spawn_deny_responder(gateway_rx, "keep working");

            let decision = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                actor.run_stop_gate("prompt-1", 0),
            )
            .await
            .expect("the stop gate must not hang");
            assert!(matches!(decision, StopGateDecision::KeepWorking { .. }));
            assert!(
                actor.turn_report.claim_for_gate().is_some(),
                "a gate that kept the agent working must leave the report slot free"
            );
        })
        .await;
}
