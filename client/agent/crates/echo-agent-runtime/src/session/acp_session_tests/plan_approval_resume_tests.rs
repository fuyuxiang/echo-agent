//! Resume re-park of the `exit_plan_mode` approval + the mid-turn
//! disconnect handling.
//!
//! On resume the shell re-issues the `echo.agent/exit_plan_mode` reverse-request when
//! `awaiting_plan_approval` was persisted, recreating a real live waiter so the
//! pager's existing approve/revise/abandon path works unchanged. These tests
//! pin the reverse-request shape, the awaiting-bit lifecycle, and the mid-turn
//! disconnect path — a graceful client disconnect must NOT auto-approve.

use super::support::*;
use super::*;

/// Build the typed approval response the pager would send back.
fn ext_response(outcome: &str) -> Arc<serde_json::value::RawValue> {
    serde_json::value::to_raw_value(&serde_json::json!({ "outcome": outcome }))
        .unwrap()
        .into()
}

/// Actor with both gateway and persistence receivers retained (the shared
/// `build_actor` drops persistence).
async fn actor_with_channels() -> (
    std::sync::Arc<SessionActor>,
    tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
    tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>,
) {
    let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let (actor, _ev) = create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
    (std::sync::Arc::new(actor), gateway_rx, persistence_rx)
}

/// Latest persisted `awaiting_plan_approval` value, or `None` if plan-mode state
/// was never persisted.
fn last_persisted_awaiting(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>,
) -> Option<bool> {
    let mut last = None;
    while let Ok(msg) = rx.try_recv() {
        if let PersistenceMsg::PlanModeState(snapshot) = msg {
            last = Some(snapshot.awaiting_plan_approval);
        }
    }
    last
}

#[tokio::test(flavor = "current_thread")]
async fn request_plan_approval_issues_reverse_request_and_clears_flag() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut gateway_rx) = build_actor().await;

            // Stand in for the pager: answer the exit_plan_mode reverse-request
            // with "approved"; ack the fire-and-forget pending broadcasts.
            let responder = tokio::task::spawn_local(async move {
                let mut seen_method = None;
                let mut seen_session = None;
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            seen_method = Some(args.request.method.to_string());
                            let req: serde_json::Value =
                                serde_json::from_str(args.request.params.get()).unwrap();
                            seen_session = req
                                .get("sessionId")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            let _ = args
                                .response_tx
                                .send(Ok(acp::ExtResponse::new(ext_response("approved"))));
                            break;
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
                (seen_method, seen_session)
            });

            let tool_call_id = acp::ToolCallId::new(Arc::from("tc-resume"));
            let parsed = actor
                .request_plan_approval(&tool_call_id, Some("# Plan".into()))
                .await
                .expect("approval round-trip should succeed");

            assert_eq!(parsed.outcome, "approved");
            assert!(
                !actor.plan_mode.lock().is_awaiting_plan_approval(),
                "awaiting flag must clear once the approval is answered"
            );

            let (method, session_id) = responder.await.unwrap();
            assert_eq!(method.as_deref(), Some("echo.agent/exit_plan_mode"));
            assert_eq!(
                session_id.as_deref(),
                Some("test-actor"),
                "reverse-request must carry a non-empty sessionId (design §5.4)"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn request_plan_approval_clears_flag_on_request_changes() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut gateway_rx) = build_actor().await;

            let responder = tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            let _ = args
                                .response_tx
                                .send(Ok(acp::ExtResponse::new(ext_response("cancelled"))));
                            break;
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let tool_call_id = acp::ToolCallId::new(Arc::from("tc-resume-revise"));
            let parsed = actor
                .request_plan_approval(&tool_call_id, Some("# Plan".into()))
                .await
                .expect("approval round-trip should succeed");

            assert_eq!(parsed.outcome, "cancelled");
            // Request-changes leaves plan mode active but never strands the bit.
            assert!(!actor.plan_mode.lock().is_awaiting_plan_approval());
            responder.await.unwrap();
        })
        .await;
}

/// An unparseable approval response must fail CLOSED to `"cancelled"` (stay in
/// plan mode), never fall open to `"approved"`.
#[tokio::test(flavor = "current_thread")]
async fn request_plan_approval_parse_fallback_fails_closed() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut gateway_rx) = build_actor().await;

            let responder = tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            // Garbage payload that does not deserialize to ExitPlanModeExtResponse.
                            let garbage: Arc<serde_json::value::RawValue> =
                                serde_json::value::to_raw_value(&serde_json::json!(
                                    "not-an-object"
                                ))
                                .unwrap()
                                .into();
                            let _ = args.response_tx.send(Ok(acp::ExtResponse::new(garbage)));
                            break;
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let tool_call_id = acp::ToolCallId::new(Arc::from("tc-garbage"));
            let parsed = actor
                .request_plan_approval(&tool_call_id, Some("# Plan".into()))
                .await
                .expect("round-trip returns Ok even for a garbage payload");
            assert_eq!(parsed.outcome, "cancelled");
            responder.await.unwrap();
        })
        .await;
}

/// REAL park (not seeded): drive a genuine `exit_plan_mode` tool call through
/// `prepare_tool_call`, simulate the client disconnecting mid-approval, and
/// assert the tool is NOT auto-executed, plan mode stays Active, and
/// `awaiting_plan_approval=true` is PERSISTED (what would land in
/// `plan_mode.json`) so a fresh resume re-parks.
#[tokio::test(flavor = "current_thread")]
async fn real_exit_plan_mode_disconnect_keeps_awaiting_persisted() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut gateway_rx, mut persistence_rx) = actor_with_channels().await;
            // Real enter/exit_plan_mode tools so prepare_tool_call parses a genuine call.
            *actor.agent.borrow_mut() = test_agent_with_plan_tools().await;
            // Plan mode Active with a real plan.md at the tracker path.
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("plan.md"), "# Plan\n- step 1\n").unwrap();
            {
                let mut tracker = actor.plan_mode.lock();
                *tracker =
                    crate::session::plan_mode::PlanModeTracker::new(dir.path().to_path_buf());
                tracker.activate_from_tool();
            }

            // Pager receives the gate then disconnects (drops the request).
            let responder = tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            drop(args); // no response -> "unable to receive response"
                            break;
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let call = crate::sampling::types::ToolCallResponse {
                id: "call-exit".to_string(),
                kind: "function".to_string(),
                function: crate::sampling::types::ToolCallFunction::new("exit_plan_mode", "{}"),
            };
            let mut deferred = Vec::new();
            let outcome = actor
                .prepare_tool_call(call, &mut deferred)
                .await
                .expect("prepare_tool_call should not error");

            // Disconnect must NOT auto-approve: the tool is not prepared/executed.
            match outcome {
                Err(ToolLoop::Cancelled) => {}
                other => panic!("expected ToolLoop::Cancelled on disconnect, got {other:?}"),
            }
            assert!(
                actor.plan_mode.lock().is_active(),
                "plan mode must remain Active after a disconnect (no auto-approve)"
            );
            assert!(
                actor.plan_mode.lock().is_awaiting_plan_approval(),
                "awaiting flag must remain set after a disconnect"
            );
            assert_eq!(
                last_persisted_awaiting(&mut persistence_rx),
                Some(true),
                "plan_mode.json must persist awaiting_plan_approval=true after disconnect"
            );
            responder.await.unwrap();
        })
        .await;
}

/// Headless / no UI client wired: the reverse-request can't be delivered, so
/// `exit_plan_mode` falls through and executes (original behavior) — verified by
/// `prepare_tool_call` returning a prepared call rather than Cancelled.
#[tokio::test(flavor = "current_thread")]
async fn real_exit_plan_mode_no_client_executes_tool() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, gateway_rx, _persistence_rx) = actor_with_channels().await;
            // Drop the gateway receiver: the reverse-request enqueue fails
            // ("unable to send"), the headless branch.
            drop(gateway_rx);
            *actor.agent.borrow_mut() = test_agent_with_plan_tools().await;
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("plan.md"), "# Plan\n- step 1\n").unwrap();
            {
                let mut tracker = actor.plan_mode.lock();
                *tracker =
                    crate::session::plan_mode::PlanModeTracker::new(dir.path().to_path_buf());
                tracker.activate_from_tool();
            }

            let call = crate::sampling::types::ToolCallResponse {
                id: "call-exit-headless".to_string(),
                kind: "function".to_string(),
                function: crate::sampling::types::ToolCallFunction::new("exit_plan_mode", "{}"),
            };
            let mut deferred = Vec::new();
            let outcome = actor
                .prepare_tool_call(call, &mut deferred)
                .await
                .expect("prepare_tool_call should not error");
            // Headless: the tool is prepared (it will execute and exit plan mode).
            assert!(
                outcome.is_ok(),
                "headless exit_plan_mode should fall through to execute the tool"
            );
        })
        .await;
}

/// A quit-while-parked (disconnect) keeps `awaiting_plan_approval` set so the
/// next resume re-parks the gate. Not seeded: `request_plan_approval` sets the
/// bit itself.
#[tokio::test(flavor = "current_thread")]
async fn request_plan_approval_keeps_flag_when_client_disconnects() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut gateway_rx, mut persistence_rx) = actor_with_channels().await;

            let responder = tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            drop(args); // no response -> ext_method sees Err
                            break;
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let tool_call_id = acp::ToolCallId::new(Arc::from("tc-resume-quit"));
            let result = actor
                .request_plan_approval(&tool_call_id, Some("# Plan".into()))
                .await;

            assert!(result.is_err(), "disconnect should surface as an error");
            assert!(
                actor.plan_mode.lock().is_awaiting_plan_approval(),
                "awaiting flag must survive a client disconnect"
            );
            assert_eq!(
                last_persisted_awaiting(&mut persistence_rx),
                Some(true),
                "the parked approval must be persisted as awaiting=true"
            );
            responder.await.unwrap();
        })
        .await;
}

/// Dropping the `request_plan_approval` future mid-await (the turn-cancel path)
/// must clear the awaiting bit via `AwaitingApprovalGuard`.
#[tokio::test(flavor = "current_thread")]
async fn request_plan_approval_future_drop_clears_flag() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut gateway_rx) = build_actor().await;

            // Pager receives the request but never answers (keeps the parked
            // await pending) so we can drop the future while it is in flight.
            let responder = tokio::task::spawn_local(async move {
                while let Some(msg) = gateway_rx.recv().await {
                    match msg {
                        echo_agent_acp::AcpClientMessage::ExtMethod(args) => {
                            // Hold the response sender open: never resolve.
                            std::mem::forget(args);
                            break;
                        }
                        echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                            let _ = args.response_tx.send(Ok(()));
                        }
                        _ => {}
                    }
                }
            });

            let tool_call_id = acp::ToolCallId::new(Arc::from("tc-drop"));
            let mut fut =
                Box::pin(actor.request_plan_approval(&tool_call_id, Some("# Plan".into())));
            // Poll until the request is parked (awaiting flag set), then drop.
            tokio::select! {
                _ = &mut fut => panic!("request should still be parked (no answer)"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
            assert!(actor.plan_mode.lock().is_awaiting_plan_approval());
            drop(fut); // turn cancelled -> guard runs

            assert!(
                !actor.plan_mode.lock().is_awaiting_plan_approval(),
                "dropping the parked future must clear the awaiting bit"
            );
            responder.abort();
        })
        .await;
}

/// Resume with the flag set but no `plan.md` on disk: clear the bit and issue NO
/// reverse-request.
#[tokio::test(flavor = "current_thread")]
async fn resume_no_plan_md_clears_flag_without_request() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut gateway_rx, _persistence_rx) = actor_with_channels().await;
            // Point the tracker at an empty dir (no plan.md) and arm the flag.
            let dir = tempfile::tempdir().unwrap();
            {
                let mut tracker = actor.plan_mode.lock();
                *tracker =
                    crate::session::plan_mode::PlanModeTracker::new(dir.path().to_path_buf());
                tracker.activate_from_tool();
                tracker.set_awaiting_plan_approval(true);
            }

            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.clone().resume_plan_approval(completion_tx).await;

            assert!(
                !actor.plan_mode.lock().is_awaiting_plan_approval(),
                "missing plan.md must clear the stuck awaiting bit"
            );
            assert!(
                gateway_rx.try_recv().is_err(),
                "no reverse-request should be sent when plan.md is missing"
            );
        })
        .await;
}
