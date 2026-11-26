use super::support::*;
use super::*;
use echo_agent_test_support::sse::{
    responses_api_reasoning_then_tool_call_events, responses_api_script_exact,
};
use echo_agent_test_support::{MockInferenceServer, ScriptedResponse};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// `SessionActor` turn futures overflow the default test thread stack.
fn block_on_session(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(f)
        .expect("spawn large-stack test thread")
        .join()
        .expect("test thread");
}

fn current_thread_local<F>(f: F)
where
    F: Future<Output = ()> + 'static,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    tokio::task::LocalSet::new().block_on(&rt, f);
}

const TODO_ARGS: &str = r#"{"todos":[{"id":"t1","content":"poll","status":"completed"}]}"#;

fn drain_gateway(mut rx: tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>) {
    tokio::task::spawn_local(async move {
        while let Some(msg) = rx.recv().await {
            if let echo_agent_acp::AcpClientMessage::SessionNotification(args) = msg {
                let _ = args.response_tx.send(Ok(()));
            }
        }
    });
}

/// Acks like [`drain_gateway`] but keeps the hook events for the one test that asserts on them.
fn capture_hook_events(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
) -> std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>> {
    let fired = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = fired.clone();
    tokio::task::spawn_local(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                    let _ = args.response_tx.send(Ok(()));
                }
                echo_agent_acp::AcpClientMessage::ExtNotification(args)
                    if args.request.method.as_ref() == "echo.agent/hooks/event" =>
                {
                    sink.borrow_mut()
                        .push(serde_json::from_str(args.request.params.get()).unwrap());
                }
                _ => {}
            }
        }
    });
    fired
}

fn drain_persistence_flush_enospc(mut rx: tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>) {
    tokio::task::spawn_local(async move {
        while let Some(msg) = rx.recv().await {
            if let PersistenceMsg::FlushAndAck { respond_to } = msg {
                let _ = respond_to.send(Err(std::io::Error::from(std::io::ErrorKind::StorageFull)));
            }
        }
    });
}

async fn actor_with_mock_sampler(
    server: &MockInferenceServer,
    persistence_tx: tokio::sync::mpsc::UnboundedSender<PersistenceMsg>,
    gateway_tx: tokio::sync::mpsc::UnboundedSender<echo_agent_acp::AcpClientMessage>,
    max_turns: Option<usize>,
) -> Arc<SessionActor> {
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

    let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    actor.sampler_handle = sampler_handle;
    actor.max_turns = max_turns;
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
    actor
}

async fn run_prompt(
    actor: &Arc<SessionActor>,
    prompt_id: &str,
) -> Result<crate::session::commands::PromptTurnOk, acp::Error> {
    let prompt_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(
        "hello".to_string(),
    ))];
    tokio::time::timeout(
        Duration::from_secs(60),
        actor.handle_prompt(
            prompt_id,
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
    .expect("turn must finish within timeout")
}

/// Also the one test that drives a real turn into `StopFailure`: deleting the report in the
/// turn's error arm leaves a host that watched the turn start waiting forever.
#[test]
fn completed_turn_flush_enospc_returns_error_and_reports_stop_failure() {
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start()
                .await
                .expect("mock inference server");
            server.enqueue_response(
                "/v1/responses",
                ScriptedResponse::sse(responses_api_script_exact("done", "test")),
            );

            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let fired = capture_hook_events(gateway_rx);
            let (persistence_tx, persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            drain_persistence_flush_enospc(persistence_rx);

            let actor = actor_with_mock_sampler(&server, persistence_tx, gateway_tx, None).await;
            let mut hooks = crate::extensions::hooks::ClientHooks::new();
            hooks.insert(
                echo_agent_hooks::event::HookEventName::StopFailure,
                vec![crate::extensions::hooks::ClientHookGroup {
                    matcher: None,
                    callback_ids: vec!["cb".to_string()],
                    timeout: None,
                }],
            );
            *actor.client_hooks.borrow_mut() = hooks;
            let queue = super::turn_end_hooks::TurnEndQueue::spawn(actor.clone());

            let error = run_prompt(&actor, "disk-full-completed")
                .await
                .expect_err("completed turn must fail when flush hits ENOSPC");
            queue.drain().await;

            assert_eq!(error.message, "No space left on device");
            let fired = fired.borrow();
            assert_eq!(fired.len(), 1);
            assert_eq!(fired[0]["hookEventName"], "stop_failure");
        });
    });
}

#[test]
fn cancelled_turn_flush_enospc_still_reports_cancellation() {
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start()
                .await
                .expect("mock inference server");
            server.enqueue_response(
                "/v1/responses",
                ScriptedResponse::sse(responses_api_reasoning_then_tool_call_events(
                    "poll",
                    "disk-full-cancel-call",
                    "todo_write",
                    TODO_ARGS,
                    "test",
                )),
            );

            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            drain_gateway(gateway_rx);
            let (persistence_tx, persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            drain_persistence_flush_enospc(persistence_rx);

            let actor = actor_with_mock_sampler(&server, persistence_tx, gateway_tx, Some(0)).await;
            let ok = run_prompt(&actor, "disk-full-cancelled")
                .await
                .expect("cancel/max-turns must not become a disk-full error");
            assert_eq!(ok.stop_reason, acp::StopReason::Cancelled);
        });
    });
}
