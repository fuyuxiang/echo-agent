//! The turn-end hooks, driven through the real cancel and dispatch paths.

use super::support::*;
use super::turn_end_hooks::ReportOutcome;
use super::turn_end_hooks::TurnEnd;
use super::turn_report_slot::{CommitOutcome, TurnReportState};
use super::*;
use crate::session::CancelTrigger;
use echo_agent_hooks::event::{HookEventName, StopCancelledReason, StopFailureKind};

#[derive(Default)]
struct RecordingLifecycle {
    aborts: std::cell::Cell<usize>,
    idles: std::cell::Cell<usize>,
}

#[async_trait::async_trait(?Send)]
impl echo_agent_lifecycle::LocalTurnLifecycleContributor for RecordingLifecycle {
    async fn on_turn_abort(&self, _input: &echo_agent_lifecycle::TurnAbortInput) {
        self.aborts.set(self.aborts.get() + 1);
    }
}

#[async_trait::async_trait(?Send)]
impl echo_agent_lifecycle::LocalSessionLifecycleContributor for RecordingLifecycle {
    async fn on_session_idle(&self, _input: &echo_agent_lifecycle::SessionIdleInput) {
        self.idles.set(self.idles.get() + 1);
    }
}

struct Harness {
    actor: Arc<SessionActor>,
    lifecycle: std::rc::Rc<RecordingLifecycle>,
    gateway: Option<tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>>,
    events: Option<tokio::sync::mpsc::UnboundedReceiver<SessionEvent>>,
    /// Stands in for the one `run_session` owns; [`Self::spawn_loop`] retires it.
    queue: Option<super::turn_end_hooks::TurnEndQueue>,
    /// Held only so the loop does not see its chat channel close.
    chat: Option<tokio::sync::mpsc::UnboundedSender<echo_agent_chat_state::ChatStateEvent>>,
}

impl Harness {
    async fn new() -> Self {
        Self::build(false).await
    }

    async fn subagent() -> Self {
        Self::build(true).await
    }

    async fn build(is_subagent: bool) -> Self {
        let (gateway_tx, gateway) = tokio::sync::mpsc::unbounded_channel();
        let (persistence_tx, mut persistence) =
            tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
        // Drop each message rather than hold the queue: that drops any ack channel inside it, so
        // a writer waiting on one fails fast instead of waiting forever.
        tokio::task::spawn_local(async move { while persistence.recv().await.is_some() {} });
        let (mut actor, events) =
            create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
        actor.startup_hints.is_subagent = is_subagent;
        if is_subagent {
            actor.startup_hints.subagent_type = Some("explore".into());
        }
        let lifecycle = std::rc::Rc::new(RecordingLifecycle::default());
        let mut builder = echo_agent_lifecycle::LocalExtensionRegistryBuilder::default();
        builder.turn_lifecycle_contributor(lifecycle.clone());
        builder.session_lifecycle_contributor(lifecycle.clone());
        actor.extension_registry = builder.build();
        let actor = Arc::new(actor);
        Self {
            queue: Some(super::turn_end_hooks::TurnEndQueue::spawn(actor.clone())),
            actor,
            lifecycle,
            gateway: Some(gateway),
            events: Some(events),
            chat: None,
        }
    }

    /// Drains without re-arming, leaving the next report nowhere to go.
    async fn close_turn_end_queue(&mut self) {
        if let Some(queue) = self.queue.take() {
            queue.drain().await;
        }
    }

    /// Runs every queued report, then re-arms, so a test never races the worker.
    async fn drain_turn_ends(&mut self) {
        if let Some(queue) = self.queue.take() {
            queue.drain().await;
        }
        self.queue = Some(super::turn_end_hooks::TurnEndQueue::spawn(
            self.actor.clone(),
        ));
    }

    /// Drives the real `run_session`. Takes the gateway, whose notifications must be
    /// acknowledged or the actor blocks; hook events land in the returned buffer.
    async fn spawn_loop(
        &mut self,
    ) -> (
        tokio::sync::mpsc::UnboundedSender<SessionCommand>,
        std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>,
    ) {
        if let Some(queue) = self.queue.take() {
            queue.drain().await;
        }
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (chat_tx, chat_rx) = tokio::sync::mpsc::unbounded_channel();
        self.chat = Some(chat_tx);
        let events = self.events.take().expect("one loop per harness");
        let fired = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = fired.clone();
        let mut gateway = self.gateway.take().expect("the loop owns the gateway");
        tokio::task::spawn_local(async move {
            while let Some(msg) = gateway.recv().await {
                match msg {
                    echo_agent_acp::AcpClientMessage::ExtNotification(args) => {
                        if args.request.method.as_ref() == "echo.agent/hooks/event" {
                            sink.borrow_mut()
                                .push(serde_json::from_str(args.request.params.get()).unwrap());
                        }
                    }
                    echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                        let _ = args.response_tx.send(Ok(()));
                    }
                    _ => {}
                }
            }
        });
        tokio::task::spawn_local(super::run_session(
            self.actor.clone(),
            cmd_rx,
            chat_rx,
            events,
            None,
            Arc::new(parking_lot::Mutex::new(
                echo_agent_workspace::file_system::CodebaseIndexManager::new(),
            )),
            std::path::PathBuf::from("/tmp"),
            crate::session::fs_watch::FsWatchCapabilities::none(),
        ));
        (cmd_tx, fired)
    }

    fn listen(&self, events: &[HookEventName]) {
        let mut hooks = crate::extensions::hooks::ClientHooks::new();
        for event in events {
            hooks.insert(
                *event,
                vec![crate::extensions::hooks::ClientHookGroup {
                    matcher: None,
                    callback_ids: vec!["cb".to_string()],
                    timeout: None,
                }],
            );
        }
        *self.actor.client_hooks.borrow_mut() = hooks;
    }

    /// A running turn whose prompt is queued at the front, which is what makes a completion
    /// this actor's own.
    async fn queue_turn(
        &self,
        prompt_id: &str,
    ) -> tokio::sync::oneshot::Receiver<PromptTurnResult> {
        self.start_turn(prompt_id).await;
        let (item, rx) = super::turn_completion_emit_tests::pending_input(prompt_id);
        self.actor.state.lock().await.pending_inputs.push_back(item);
        rx
    }

    async fn start_turn(&self, prompt_id: &str) {
        let handle = tokio::task::spawn_local(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        })
        .abort_handle();
        self.run_turn(prompt_id, handle).await;
    }

    async fn run_turn(&self, prompt_id: &str, handle: tokio::task::AbortHandle) {
        *self
            .actor
            .current_prompt_id
            .lock()
            .expect("current_prompt_id mutex poisoned") = Some(prompt_id.to_string());
        self.actor.state.lock().await.running_task = Some(AgentTask {
            prompt_id: prompt_id.into(),
            handle,
        });
    }

    async fn cancel(&self, trigger: CancelTrigger) -> super::tasks_cancel::CancelOutcome {
        self.cancel_with(trigger, true).await
    }

    async fn cancel_with(
        &self,
        trigger: CancelTrigger,
        cancel_subagents: bool,
    ) -> super::tasks_cancel::CancelOutcome {
        self.actor
            .cancel_running_task(crate::session::CancelOptions {
                cancel_subagents,
                trigger: Some(trigger),
                user_initiated: true,
                ..Default::default()
            })
            .await
    }

    fn fired(&mut self) -> Vec<String> {
        self.fired_payloads()
            .iter()
            .filter_map(|p| p["hookEventName"].as_str().map(str::to_string))
            .collect()
    }

    fn fired_payloads(&mut self) -> Vec<serde_json::Value> {
        let gateway = self
            .gateway
            .as_mut()
            .expect("the loop owns the gateway; assert on the sink it returns");
        let mut events = Vec::new();
        while let Ok(msg) = gateway.try_recv() {
            if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
                && args.request.method.as_ref() == "echo.agent/hooks/event"
            {
                events.push(serde_json::from_str(args.request.params.get()).unwrap());
            }
        }
        events
    }
}

async fn run(test: impl std::future::Future<Output = ()>) {
    tokio::task::LocalSet::new().run_until(test).await;
}

#[tokio::test(flavor = "current_thread")]
async fn interrupting_a_turn_reports_stop_cancelled_once() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled, HookEventName::StopFailure]);
        h.actor
            .chat_state_handle
            .push_assistant_response(ConversationItem::assistant("partway through"));
        h.start_turn("p1").await;

        let outcome = h.cancel(CancelTrigger::CtrlC).await;
        assert!(outcome.turn_stopped);
        h.drain_turn_ends().await;
        let fired = h.fired_payloads();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0]["hookEventName"], "stop_cancelled");
        assert_eq!(fired[0]["reason"], "user_interrupt");
        assert_eq!(fired[0]["cancelTrigger"], "ctrl_c");
        assert_eq!(fired[0]["lastAssistantMessage"], "partway through");

        let second = h.actor.claim_and_queue(
            "p1",
            h.actor.turn_report.epoch(),
            TurnEnd::Failed {
                error: StopFailureKind::Unknown,
                error_details: None,
                last_assistant_message: None,
            },
        );
        assert_eq!(
            second,
            ReportOutcome::AlreadyReported,
            "the turn's one report is used up"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn interrupting_a_parked_stop_gate_still_reports() {
    for cancel_subagents in [true, false] {
        run(async move {
            let mut h = Harness::new().await;
            h.listen(&[HookEventName::Stop, HookEventName::StopCancelled]);

            let actor = h.actor.clone();
            let gate = tokio::task::spawn_local(async move { actor.run_stop_gate("p1", 0).await });
            h.run_turn("p1", gate.abort_handle()).await;
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !matches!(h.actor.turn_report.state(), TurnReportState::Held { .. }) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the gate must reach its claim");
            assert!(!gate.is_finished());

            let _ = h.cancel_with(CancelTrigger::CtrlC, cancel_subagents).await;
            h.drain_turn_ends().await;
            assert_eq!(h.fired(), ["stop_cancelled"], "{cancel_subagents}");
        })
        .await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_cancel_after_a_committed_gate_reports_nothing() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;
        let claim = h
            .actor
            .turn_report
            .claim_for_gate()
            .expect("the gate claims");
        assert_eq!(claim.commit(), CommitOutcome::Reported, "the gate commits");

        let _ = h.cancel(CancelTrigger::CtrlC).await;
        h.drain_turn_ends().await;
        assert!(h.fired_payloads().is_empty(), "the turn already reported");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn interrupting_the_turn_after_a_reported_one_still_reports() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;

        let _ = h.cancel(CancelTrigger::CtrlC).await;
        h.drain_turn_ends().await;
        assert_eq!(h.fired(), ["stop_cancelled"]);

        {
            let mut state = h.actor.state.lock().await;
            state.pending_inputs.push_back(user_item("p2", "alice"));
        }
        let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
        h.actor
            .clone()
            .maybe_start_running_task(completion_tx)
            .await;

        let _ = h.cancel(CancelTrigger::CtrlC).await;
        h.drain_turn_ends().await;
        assert_eq!(
            h.fired(),
            ["stop_cancelled"],
            "a turn interrupted before its first poll still reports"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_late_report_cannot_spend_the_next_turns_slot() {
    run(async {
        let h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);

        let epoch = h.actor.turn_report.epoch();
        h.actor.turn_report.start_next_turn();

        let late = h.actor.claim_and_queue(
            "p1",
            epoch,
            TurnEnd::Cancelled {
                reason: StopCancelledReason::UserInterrupt,
                trigger: None,
                reason_details: None,
                last_assistant_message: None,
            },
        );
        assert_eq!(late, ReportOutcome::AlreadyReported);
        assert_eq!(h.actor.turn_report.state(), TurnReportState::Free);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_rewind_reports_nothing_but_still_settles_the_session() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        {
            let mut state = h.actor.state.lock().await;
            state.rewindable = true;
            state.pending_inputs.push_back(user_item("p1", "alice"));
        }
        h.start_turn("p1").await;

        let outcome = h
            .actor
            .cancel_running_task(crate::session::CancelOptions {
                history: crate::session::CancelHistoryDisposition::RewindIfNoOutput {
                    prompt_id: None,
                },
                trigger: Some(CancelTrigger::Esc),
                user_initiated: true,
                ..Default::default()
            })
            .await;
        assert!(outcome.turn_stopped);
        assert_eq!(h.lifecycle.aborts.get(), 1);
        h.drain_turn_ends().await;
        assert!(h.fired_payloads().is_empty(), "a rewind ends no turn");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_turn_announces_its_abort_once() {
    run(async {
        let h = Harness::new().await;
        let interrupted = echo_agent_lifecycle::TurnAbortReason::Interrupted;
        let first = h.actor.turn_report.epoch();
        h.actor.notify_turn_abort(first, interrupted).await;
        h.actor.notify_turn_abort(first, interrupted).await;
        assert_eq!(h.lifecycle.aborts.get(), 1);

        h.actor.turn_report.start_next_turn();
        let second = h.actor.turn_report.epoch();
        h.actor.notify_turn_abort(first, interrupted).await;
        assert_eq!(h.lifecycle.aborts.get(), 1, "the old turn does not repeat");
        h.actor.notify_turn_abort(second, interrupted).await;
        assert_eq!(
            h.lifecycle.aborts.get(),
            2,
            "the new turn announces its own"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_subagent_session_end_names_the_child() {
    run(async {
        let events = [HookEventName::SessionEnd, HookEventName::Stop];

        let mut parent = Harness::new().await;
        parent.listen(&events);
        super::run_loop::fire_session_end_hooks(&parent.actor, "shutdown").await;
        assert_eq!(parent.fired(), vec!["session_end", "stop"]);

        let mut child = Harness::subagent().await;
        child.listen(&events);
        super::run_loop::fire_session_end_hooks(&child.actor, "shutdown").await;
        let fired = child.fired_payloads();
        assert_eq!(
            fired.len(),
            1,
            "the session-end `Stop` stays subagent-guarded"
        );
        assert_eq!(fired[0]["hookEventName"], "session_end");
        assert_eq!(fired[0]["subagentType"], "explore");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_hook_on_another_event_leaves_the_slot_free() {
    run(async {
        let h = Harness::new().await;
        h.listen(&[HookEventName::PreToolUse]);
        // A file registry too, so only the per-event check holds the slot back.
        *h.actor.hook_registry.borrow_mut() = Some(Arc::new(
            super::client_hooks_tests::file_registry_with_spec(HookEventName::PreToolUse, "true"),
        ));
        h.start_turn("p1").await;

        let _ = h.cancel(CancelTrigger::CtrlC).await;
        assert_eq!(h.actor.turn_report.state(), TurnReportState::Free);
        assert_eq!(
            h.actor.report_turn_end(
                "p1",
                TurnEnd::Failed {
                    error: StopFailureKind::Unknown,
                    error_details: None,
                    last_assistant_message: None,
                },
            ),
            ReportOutcome::NoListener
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn only_a_settled_main_session_offers_an_idle_ping() {
    run(async {
        let parent = Harness::new().await;
        parent.start_turn("p1").await;
        parent.actor.emit_session_idle_if_idle().await;
        assert_eq!(
            parent.lifecycle.idles.get(),
            0,
            "a running turn is not idle"
        );

        parent.actor.state.lock().await.running_task = None;
        // Suppressed by the interrupt that ended the turn, which must not swallow the ping.
        parent.actor.state.lock().await.notifications_suppressed = true;
        parent.actor.emit_session_idle_if_idle().await;
        assert_eq!(parent.lifecycle.idles.get(), 1);

        let child = Harness::subagent().await;
        child.actor.emit_session_idle_if_idle().await;
        assert_eq!(child.lifecycle.idles.get(), 0);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_subagent_reports_only_its_own_turn_ends() {
    run(async {
        let mut h = Harness::subagent().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;
        let epoch = h.actor.turn_report.epoch();
        let end = |reason| TurnEnd::Cancelled {
            reason,
            trigger: None,
            reason_details: None,
            last_assistant_message: None,
        };

        let inherited =
            h.actor
                .claim_and_queue("p1", epoch, end(StopCancelledReason::UserInterrupt));
        assert_eq!(
            inherited,
            ReportOutcome::InheritedInterrupt,
            "a child inherits the parent's interrupt"
        );

        let own = h
            .actor
            .claim_and_queue("p1", epoch, end(StopCancelledReason::MaxTurns));
        assert_eq!(
            own,
            ReportOutcome::Queued,
            "a subagent's own max-turns is its own report"
        );

        h.listen(&[HookEventName::StopCancelled, HookEventName::StopFailure]);
        h.actor.turn_report.start_next_turn();
        let failed = h.actor.claim_and_queue(
            "p2",
            h.actor.turn_report.epoch(),
            TurnEnd::Failed {
                error: StopFailureKind::ServerError,
                error_details: Some("y".repeat(2000)),
                last_assistant_message: None,
            },
        );
        assert_eq!(
            failed,
            ReportOutcome::Queued,
            "a subagent's own failure is its own report"
        );
        h.drain_turn_ends().await;

        let fired = h.fired_payloads();
        assert_eq!(fired[0]["reason"], "max_turns");
        assert_eq!(fired[0]["subagentType"], "explore");
        assert_eq!(fired[1]["hookEventName"], "stop_failure");
        assert_eq!(fired[1]["subagentType"], "explore");
        let details = fired[1]["errorDetails"].as_str().expect("a detail");
        assert!(details.ends_with("… [+1000 chars]"), "{details}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_loop_reports_and_settles_a_cancelled_turn() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;
        let (cmd, fired) = h.spawn_loop().await;

        cmd.send(SessionCommand::Cancel(crate::session::CancelOptions {
            cancel_subagents: true,
            trigger: Some(CancelTrigger::CtrlC),
            user_initiated: true,
            ..Default::default()
        }))
        .expect("the loop is running");

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while fired.borrow().is_empty() || h.lifecycle.idles.get() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the loop must report and settle");

        let fired = fired.borrow();
        assert_eq!(fired.len(), 1, "the loop must dispatch the queued report");
        assert_eq!(fired[0]["hookEventName"], "stop_cancelled");
        assert_eq!(h.lifecycle.idles.get(), 1, "the loop must settle the host");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn stale_named_cancel_acknowledges_without_stopping_the_next_prompt() {
    run(async {
        let mut h = Harness::new().await;
        let response = h.queue_turn("next-prompt").await;
        let (cmd, fired) = h.spawn_loop().await;

        let send_cancel = |prompt_id: &str| {
            let acknowledged = Arc::new(crate::session::CancelAcknowledgement::default());
            cmd.send(SessionCommand::Cancel(crate::session::CancelOptions {
                cancel_subagents: true,
                expected_prompt_id: Some(prompt_id.into()),
                trigger: Some(CancelTrigger::Client("stop".into())),
                user_initiated: true,
                acknowledged: Some(acknowledged.clone()),
                ..Default::default()
            }))
            .expect("the loop is running");
            acknowledged
        };
        let ignored_stale = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            send_cancel("previous-prompt").ignored_stale(),
        )
        .await
        .expect("acknowledge a stale request");
        assert!(ignored_stale);
        {
            let state = h.actor.state.lock().await;
            assert_eq!(state.running_prompt_id(), Some("next-prompt"));
            assert!(!state.notifications_suppressed);
        }
        assert!(fired.borrow().is_empty());
        assert_eq!(h.lifecycle.aborts.get(), 0);

        let ignored_stale = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            send_cancel("next-prompt").ignored_stale(),
        )
        .await
        .expect("acknowledge matching cancellation");
        assert!(!ignored_stale);
        assert!(h.actor.state.lock().await.running_prompt_id().is_none());
        assert_eq!(h.lifecycle.aborts.get(), 1);
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), response)
            .await
            .expect("the named prompt must settle")
            .expect("the prompt response channel stays open")
            .expect("the prompt returns its cancellation outcome");
        assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_closed_queue_leaves_the_turn_reportable() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;
        let end = || TurnEnd::Cancelled {
            reason: StopCancelledReason::UserInterrupt,
            trigger: None,
            reason_details: None,
            last_assistant_message: None,
        };

        h.close_turn_end_queue().await;
        assert_eq!(
            h.actor.report_turn_end("p1", end()),
            ReportOutcome::QueueClosed,
            "a report nobody can dispatch is not this turn's report"
        );
        assert_eq!(h.actor.turn_report.state(), TurnReportState::Free);

        h.drain_turn_ends().await;
        assert_eq!(h.actor.report_turn_end("p1", end()), ReportOutcome::Queued);
        h.drain_turn_ends().await;
        assert_eq!(h.fired(), ["stop_cancelled"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn draining_runs_the_worker_down_instead_of_timing_out() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;
        let _ = h.cancel(CancelTrigger::CtrlC).await;

        let queue = h.queue.take().expect("a live queue");
        let started = std::time::Instant::now();
        queue.drain().await;

        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "drain waited {:?}, so it never saw the worker exit",
            started.elapsed()
        );
        assert_eq!(h.fired(), ["stop_cancelled"], "and it still dispatched");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_the_queue_leaves_nothing_reportable() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.start_turn("p1").await;

        drop(h.queue.take().expect("a live queue"));

        assert_eq!(
            h.actor.report_turn_end(
                "p1",
                TurnEnd::Cancelled {
                    reason: StopCancelledReason::UserInterrupt,
                    trigger: None,
                    reason_details: None,
                    last_assistant_message: None,
                },
            ),
            ReportOutcome::QueueClosed
        );
        assert_eq!(h.actor.turn_report.state(), TurnReportState::Free);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_long_assistant_message_is_clipped() {
    run(async {
        let over = echo_agent_hooks::event::MAX_ASSISTANT_MESSAGE_CHARS + 500;
        // Four bytes a char, the UTF-8 worst case the char budget is derived from.
        let long = || ConversationItem::assistant("\u{1f642}".repeat(over));
        let assert_clipped = |text: &str| {
            assert!(
                text.ends_with(" chars]"),
                "unclipped at {} chars",
                text.chars().count()
            );
            assert!(
                text.chars().count() <= echo_agent_hooks::event::MAX_ASSISTANT_MESSAGE_CHARS + 32,
                "{} chars",
                text.chars().count()
            );
        };

        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        h.actor.chat_state_handle.push_assistant_response(long());
        h.start_turn("p1").await;
        let _ = h.cancel(CancelTrigger::CtrlC).await;
        h.drain_turn_ends().await;
        let fired = h.fired_payloads();
        assert_clipped(
            fired[0]["lastAssistantMessage"]
                .as_str()
                .expect("a last message"),
        );

        // Driven as a subagent because that branch of the gate's builder skips the work
        // snapshot, which would need a live tool bridge to answer.
        let sub = Harness::subagent().await;
        sub.actor.chat_state_handle.push_assistant_response(long());
        sub.start_turn("p1").await;
        let echo_agent_hooks::event::HookPayload::SubagentStop {
            last_assistant_message,
            ..
        } = sub.actor.stop_payload_for_test().await
        else {
            panic!("a subagent gate builds SubagentStop")
        };
        assert_clipped(&last_assistant_message.expect("a last message"));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn teardown_runs_queued_reports_before_the_session_end_hooks() {
    for shutdown in [true, false] {
        run(async move {
            let mut h = Harness::new().await;
            h.listen(&[
                HookEventName::StopCancelled,
                HookEventName::SessionEnd,
                HookEventName::Stop,
            ]);
            h.start_turn("p1").await;
            let (cmd, fired) = h.spawn_loop().await;

            cmd.send(SessionCommand::Cancel(crate::session::CancelOptions {
                cancel_subagents: true,
                trigger: Some(CancelTrigger::CtrlC),
                user_initiated: true,
                ..Default::default()
            }))
            .expect("the loop is running");
            if shutdown {
                cmd.send(SessionCommand::Shutdown(
                    crate::session::ShutdownKind::CancelRunningTurn,
                ))
                .expect("the loop is running");
            }
            // The cancel is buffered, so it is handled before `recv` sees the close.
            drop(cmd);

            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while fired.borrow().len() < 3 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("teardown must run the queued report and the session-end hooks");

            let names: Vec<String> = fired
                .borrow()
                .iter()
                .filter_map(|p| p["hookEventName"].as_str().map(str::to_string))
                .collect();
            assert_eq!(
                names,
                ["stop_cancelled", "session_end", "stop"],
                "{shutdown}"
            );
        })
        .await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_completion_arriving_after_its_cancel_reports_nothing() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        let _p1 = h.queue_turn("p1").await;

        let _ = h.cancel(CancelTrigger::CtrlC).await;
        h.actor
            .handle_completion(
                "p1".into(),
                Ok(PromptTurnOk {
                    stop_reason: acp::StopReason::Cancelled,
                    total_tokens: 0,
                    turn_snapshot: None,
                    completion_kind: PromptCompletionKind::Cancelled {
                        category: Some(crate::session::events::CancellationCategory::MidTurnAbort),
                        context: None,
                    },
                    structured_output: None,
                    usage: None,
                    tool_overrides: None,
                }),
            )
            .await;
        h.drain_turn_ends().await;

        assert_eq!(h.fired(), ["stop_cancelled"], "one turn, one report");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_completion_reports_its_own_cancel_reason() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        let ok = |kind| {
            Ok(PromptTurnOk {
                stop_reason: acp::StopReason::EndTurn,
                total_tokens: 0,
                turn_snapshot: None,
                completion_kind: kind,
                structured_output: None,
                usage: None,
                tool_overrides: None,
            })
        };

        let _p1 = h.queue_turn("p1").await;
        h.actor
            .handle_completion(
                "p1".into(),
                ok(PromptCompletionKind::MaxTurnsReached { limit: 1 }),
            )
            .await;

        h.actor.turn_report.start_next_turn();
        let _p2 = h.queue_turn("p2").await;
        h.actor
            .handle_completion(
                "p2".into(),
                ok(PromptCompletionKind::Cancelled {
                    category: Some(
                        crate::session::events::CancellationCategory::PermissionRejected,
                    ),
                    context: Some(crate::session::CancellationContext {
                        tool_name: Some("read_file".into()),
                        reason: Some("x".repeat(2000)),
                        hook_name: None,
                        trigger: Some(format!("ctrl_c{}", "y".repeat(2000))),
                    }),
                }),
            )
            .await;
        h.drain_turn_ends().await;

        let fired = h.fired_payloads();
        assert_eq!(fired.len(), 2);
        assert_eq!(fired[0]["reason"], "max_turns");
        assert_eq!(fired[0]["promptId"], "p1");
        assert_eq!(fired[1]["reason"], "permission_rejected");
        for (field, prefix, max) in [
            (
                "cancelTrigger",
                "ctrl_c",
                echo_agent_hooks::event::MAX_CANCEL_TRIGGER_CHARS,
            ),
            (
                "reasonDetails",
                "read_file: ",
                echo_agent_hooks::event::MAX_STOP_ENTRY_TEXT_CHARS,
            ),
        ] {
            let text = fired[1][field]
                .as_str()
                .unwrap_or_else(|| panic!("{field}"));
            assert!(text.starts_with(prefix), "{field}: {text}");
            assert!(text.ends_with(" chars]"), "{field}: {text}");
            assert!(text.chars().count() < max + 32, "{field}: {}", text.len());
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_flush_leaves_the_queue_open() {
    run(async {
        let mut h = Harness::new().await;
        h.listen(&[HookEventName::StopCancelled]);
        for prompt_id in ["p1", "p2"] {
            h.start_turn(prompt_id).await;
            h.actor.turn_report.start_next_turn();
            let _ = h.cancel(CancelTrigger::CtrlC).await;
            h.queue.as_mut().expect("a live queue").flush().await;
        }

        h.drain_turn_ends().await;
        let fired = h.fired_payloads();
        assert_eq!(fired.len(), 2);
        assert_eq!(fired[0]["promptId"], "p1");
        assert_eq!(fired[1]["promptId"], "p2");
    })
    .await;
}
