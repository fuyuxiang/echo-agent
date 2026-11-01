use super::*;
use crate::implementations::echo_agent_build::task::admission::{LimitBehavior, SubagentLimits};
use crate::implementations::echo_agent_build::task::backend::{ChannelBackend, SubagentBackend};
use crate::implementations::echo_agent_build::task::types::{
    SubagentCancelRequest, SubagentClearUsageNotAppliedRequest, SubagentCompletionsRequest,
    SubagentListActiveRequest, SubagentLoopUnitActiveRequest, SubagentMarkUsageNotAppliedRequest,
    SubagentOutstandingReply, SubagentOutstandingRequest, SubagentOwner, SubagentRegistryCounts,
    SubagentRequest, SubagentSnapshotStatus,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct TestControl {
    cancellation: CancellationToken,
}

impl ChildControl for TestControl {
    type ProgressFuture = std::future::Ready<SubagentProgress>;

    fn progress(&self) -> Self::ProgressFuture {
        std::future::ready(SubagentProgress {
            turn_count: 2,
            tool_call_count: 3,
            tokens_used: 100,
            context_window_tokens: 1_000,
            context_usage_pct: 10,
            tools_used: vec!["read_file".to_owned()],
            error_count: 0,
        })
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }
}

struct TestRunner {
    wait_before_start: bool,
    wait_after_cancel: bool,
    start: tokio::sync::broadcast::Sender<()>,
    finish: tokio::sync::broadcast::Sender<()>,
    completions: mpsc::UnboundedSender<CompletionDisposition>,
    requests: mpsc::UnboundedSender<SubagentRequest>,
    started: mpsc::UnboundedSender<String>,
    queue_waits: mpsc::UnboundedSender<(String, Option<std::time::Duration>, usize)>,
}

impl ChildRunner for TestRunner {
    type Control = TestControl;
    type CompletionData = ();
    type RunFuture = SendBoxFuture<ChildRunOutput<()>>;
    type ValidateFuture = SendBoxFuture<SubagentValidateTypeOutcome>;
    type DescribeFuture = SendBoxFuture<SubagentDescribeOutcome>;

    fn run(&self, run: ChildRunRequest<Self::Control>) -> Self::RunFuture {
        let wait_before_start = self.wait_before_start;
        let wait_after_cancel = self.wait_after_cancel;
        let mut start = self.start.subscribe();
        let mut finish = self.finish.subscribe();
        let requests = self.requests.clone();
        let started = self.started.clone();
        let queue_waits = self.queue_waits.clone();
        Box::pin(async move {
            let ChildRunRequest {
                request,
                cancellation,
                reporter,
                queued_for,
                session_running,
            } = run;
            let _ = queue_waits.send((request.id.clone(), queued_for, session_running));
            let _ = requests.send(request.clone());
            if wait_before_start {
                tokio::select! {
                    _ = cancellation.cancelled() => {
                        if wait_after_cancel {
                            let _ = finish.recv().await;
                        }
                        return ChildRunOutput {
                            result: cancelled_result(&request),
                            completion_data: (),
                            snapshot_ref: None,
                        };
                    }
                    _ = start.recv() => {}
                }
            }
            if !reporter
                .started(StartedChild {
                    child_session_id: request.id.clone(),
                    persona: None,
                    resumed_from: request.resume_from.clone(),
                    child_cwd: request.cwd.clone().unwrap_or_default(),
                    worktree_path: None,
                    effective_model_id: "test-model".to_owned(),
                    // Mock definition resolution: this type declares background.
                    definition_background: request.subagent_type == "background-default",
                    control: TestControl {
                        cancellation: cancellation.clone(),
                    },
                })
                .await
            {
                return ChildRunOutput {
                    result: cancelled_result(&request),
                    completion_data: (),
                    snapshot_ref: None,
                };
            }
            let _ = started.send(request.id.clone());
            let result = tokio::select! {
                _ = cancellation.cancelled() => {
                    if wait_after_cancel {
                        let _ = finish.recv().await;
                    }
                    cancelled_result(&request)
                },
                _ = finish.recv() => SubagentResult {
                    success: true,
                    output: request.prompt.clone().into(),
                    subagent_id: request.id.clone(),
                    child_session_id: request.id.clone(),
                    tool_calls: 3,
                    turns: 2,
                    ..Default::default()
                },
            };
            ChildRunOutput {
                result,
                completion_data: (),
                snapshot_ref: None,
            }
        })
    }

    fn validate_type(
        &self,
        _subagent_type: String,
        _parent_session_id: String,
    ) -> Self::ValidateFuture {
        Box::pin(std::future::ready(SubagentValidateTypeOutcome::Ok))
    }

    fn describe_type(
        &self,
        _subagent_type: String,
        _harness_agent_type: Option<String>,
        _parent_session_id: String,
    ) -> Self::DescribeFuture {
        Box::pin(std::future::ready(SubagentDescribeOutcome::Unavailable))
    }

    fn on_completed(&self, completion: ChildCompletion<Self::CompletionData>) {
        let _ = self.completions.send(completion.disposition);
    }
}

fn cancelled_result(request: &SubagentRequest) -> SubagentResult {
    SubagentResult {
        success: false,
        cancelled: true,
        error: Some("cancelled".to_owned()),
        subagent_id: request.id.clone(),
        child_session_id: request.id.clone(),
        ..Default::default()
    }
}

fn request(id: &str, background: bool) -> SubagentRequest {
    SubagentRequest {
        id: id.to_owned(),
        prompt: "work".to_owned(),
        description: "test child".to_owned(),
        subagent_type: "explore".to_owned(),
        parent_session_id: "parent".to_owned(),
        parent_prompt_id: Some("prompt".to_owned()),
        resume_from: None,
        cwd: None,
        runtime_overrides: Default::default(),
        run_in_background: background,
        surface_completion: true,
        await_to_completion: false,
        fork_context: false,
        owner: SubagentOwner::Task,
        cancel_token: CancellationToken::new(),
    }
}

struct Harness {
    backend: ChannelBackend,
    start: tokio::sync::broadcast::Sender<()>,
    finish: tokio::sync::broadcast::Sender<()>,
    completions: mpsc::UnboundedReceiver<CompletionDisposition>,
    requests: mpsc::UnboundedReceiver<SubagentRequest>,
    started: mpsc::UnboundedReceiver<String>,
    queue_waits: mpsc::UnboundedReceiver<(String, Option<std::time::Duration>, usize)>,
    actor: tokio::task::JoinHandle<()>,
}

fn harness(wait_before_start: bool, foreground_budget: std::time::Duration) -> Harness {
    harness_with_config(
        wait_before_start,
        CoordinatorConfig {
            foreground_budget,
            ..CoordinatorConfig::default()
        },
    )
}

fn harness_with_config(wait_before_start: bool, config: CoordinatorConfig) -> Harness {
    harness_with_options(wait_before_start, false, config)
}

fn harness_with_options(
    wait_before_start: bool,
    wait_after_cancel: bool,
    config: CoordinatorConfig,
) -> Harness {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (start, _) = tokio::sync::broadcast::channel(4);
    let (finish, _) = tokio::sync::broadcast::channel(4);
    let (completion_tx, completions) = mpsc::unbounded_channel();
    let (request_tx, requests) = mpsc::unbounded_channel();
    let (started_tx, started) = mpsc::unbounded_channel();
    let (queue_wait_tx, queue_waits) = mpsc::unbounded_channel();
    let actor = tokio::spawn(
        SubagentCoordinator::new(
            command_rx,
            TestRunner {
                wait_before_start,
                wait_after_cancel,
                start: start.clone(),
                finish: finish.clone(),
                completions: completion_tx,
                requests: request_tx,
                started: started_tx,
                queue_waits: queue_wait_tx,
            },
            config,
        )
        .run(),
    );
    Harness {
        // Unbound by default so tests can set request.parent_session_id
        // freely (e.g. nested reparent). ParentSession APIs must use
        // `parent_backend` so they stay session-scoped.
        backend: ChannelBackend::new(command_tx),
        start,
        finish,
        completions,
        requests,
        started,
        queue_waits,
        actor,
    }
}

/// Session-bound backend for ParentSession cancel / admission on the default
/// test parent (`"parent"`). Required because unbound cancel is rejected.
fn parent_backend(harness: &Harness) -> ChannelBackend {
    ChannelBackend::for_session(harness.backend.sender(), "parent")
}

async fn loop_unit_active(backend: &ChannelBackend, task_id: &str) -> bool {
    let (respond_to, response_rx) = oneshot::channel();
    backend
        .sender()
        .send(SubagentEvent::LoopUnitActive(
            SubagentLoopUnitActiveRequest {
                task_id: task_id.to_owned(),
                respond_to,
            },
        ))
        .expect("actor command channel open");
    response_rx.await.expect("loop activity response")
}

async fn outstanding(backend: &ChannelBackend, prompt_id: &str) -> SubagentOutstandingReply {
    let (respond_to, response_rx) = oneshot::channel();
    backend
        .sender()
        .send(SubagentEvent::Outstanding(SubagentOutstandingRequest {
            parent_session_id: "parent".to_owned(),
            prompt_id: prompt_id.to_owned(),
            respond_to,
        }))
        .expect("actor command channel open");
    response_rx.await.expect("outstanding response")
}

async fn has_live_children(backend: &ChannelBackend, parent: &str) -> bool {
    let (respond_to, response) = oneshot::channel();
    backend
        .sender()
        .send(SubagentEvent::HasLiveChildren {
            parent_session_id: parent.into(),
            respond_to,
        })
        .unwrap();
    response.await.unwrap()
}

#[tokio::test]
async fn session_activity_covers_initializing_queued_running_and_finished_children() {
    let mut harness = harness_with_config(
        true,
        CoordinatorConfig {
            limits: SubagentLimits {
                max_concurrent: 1,
                behavior: LimitBehavior::Queue,
            },
            ..Default::default()
        },
    );
    assert!(!has_live_children(&harness.backend, "parent").await);
    let first = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("first", true)).await }
    });
    harness.requests.recv().await.unwrap(); // Still initializing, no active child.
    assert!(has_live_children(&harness.backend, "parent").await);
    assert!(!has_live_children(&harness.backend, "other-parent").await);
    let second = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("second", true)).await }
    });
    while harness.backend.registry_counts().await.queued == 0 {
        tokio::task::yield_now().await;
    }
    assert!(has_live_children(&harness.backend, "parent").await);
    harness.start.send(()).unwrap();
    harness.started.recv().await.unwrap();
    assert!(has_live_children(&harness.backend, "parent").await);
    harness.finish.send(()).unwrap();
    first.await.unwrap().unwrap();
    harness.requests.recv().await.unwrap(); // The queued child is now initializing.
    assert!(has_live_children(&harness.backend, "parent").await);
    harness.start.send(()).unwrap();
    harness.started.recv().await.unwrap();
    harness.finish.send(()).unwrap();
    second.await.unwrap().unwrap();
    // Background spawn acknowledges startup before completion. Observe both
    // completion callbacks before asserting that no child remains resident.
    harness.completions.recv().await.unwrap();
    harness.completions.recv().await.unwrap();
    assert!(!has_live_children(&harness.backend, "parent").await);
    harness.actor.abort();
}

#[tokio::test]
async fn foreground_completion_is_delivered_inline() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("inline", false)).await }
    });
    tokio::task::yield_now().await;
    let _ = harness.finish.send(());

    let result = spawn.await.unwrap().unwrap();
    assert!(result.success);
    let disposition = harness.completions.recv().await.unwrap();
    assert!(disposition.foreground_delivered);
    assert!(!disposition.should_surface);
    harness.actor.abort();
}

#[tokio::test(start_paused = true)]
async fn foreground_deadline_hands_off_without_stopping_child() {
    let mut harness = harness(false, std::time::Duration::from_secs(1));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("slow", false)).await }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let interim = spawn.await.unwrap().unwrap();
    assert!(interim.backgrounded);
    // Interim handoff must not read as a completion (status() contract).
    assert!(!interim.success);
    assert_eq!(
        outstanding(&harness.backend, "prompt").await,
        SubagentOutstandingReply {
            live_ids: Vec::new(),
            background_live: true,
            subagent_usage_not_applied: false,
        }
    );
    assert_eq!(
        harness.backend.registry_counts().await,
        SubagentRegistryCounts {
            pending: 0,
            active: 1,
            completed: 0,
            queued: 0,
        }
    );

    let running = harness.backend.query("slow", false, None).await.unwrap();
    assert!(running.is_running());
    let _ = harness.finish.send(());
    let disposition = harness.completions.recv().await.unwrap();
    assert!(disposition.backgrounded);
    assert!(disposition.should_surface);
    harness.actor.abort();
}

#[tokio::test]
async fn live_blocking_waiter_suppresses_async_surface() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("waited", true)).await }
    });
    tokio::task::yield_now().await;
    let wait = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.query("waited", true, Some(60_000)).await }
    });
    tokio::task::yield_now().await;
    let _ = harness.finish.send(());

    assert!(wait.await.unwrap().unwrap().status.is_terminal());
    let disposition = harness.completions.recv().await.unwrap();
    assert!(disposition.waiter_delivered);
    assert!(!disposition.should_surface);
    assert!(spawn.await.unwrap().unwrap().success);
    harness.actor.abort();
}

#[tokio::test(start_paused = true)]
async fn timed_out_waiter_does_not_suppress_later_completion() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("timeout", true)).await }
    });
    tokio::task::yield_now().await;
    let snapshot = harness
        .backend
        .query("timeout", true, Some(1_000))
        .await
        .unwrap();
    assert!(snapshot.is_running());

    let _ = harness.finish.send(());
    let disposition = harness.completions.recv().await.unwrap();
    assert!(!disposition.waiter_delivered);
    assert!(disposition.should_surface);
    assert!(spawn.await.unwrap().unwrap().success);
    harness.actor.abort();
}

#[tokio::test(start_paused = true)]
async fn surviving_waiter_suppresses_after_peer_times_out() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("two-waiters", true)).await }
    });
    tokio::task::yield_now().await;
    let short = tokio::spawn({
        let backend = harness.backend.clone();
        async move {
            backend
                .query("two-waiters", true, Some(1_000))
                .await
                .unwrap()
        }
    });
    let long = tokio::spawn({
        let backend = harness.backend.clone();
        async move {
            backend
                .query("two-waiters", true, Some(60_000))
                .await
                .unwrap()
        }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert!(short.await.unwrap().is_running());

    let _ = harness.finish.send(());
    assert!(long.await.unwrap().status.is_terminal());
    let disposition = harness.completions.recv().await.unwrap();
    assert!(disposition.waiter_delivered);
    assert!(!disposition.should_surface);
    assert!(spawn.await.unwrap().unwrap().success);
    harness.actor.abort();
}

#[tokio::test]
async fn dropped_waiter_does_not_suppress_completion() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("dropped-wait", true)).await }
    });
    tokio::task::yield_now().await;
    let wait = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.query("dropped-wait", true, Some(60_000)).await }
    });
    tokio::task::yield_now().await;
    wait.abort();
    let _ = wait.await;

    let _ = harness.finish.send(());
    let disposition = harness.completions.recv().await.unwrap();
    assert!(!disposition.waiter_delivered);
    assert!(disposition.should_surface);
    assert!(spawn.await.unwrap().unwrap().success);
    harness.actor.abort();
}

#[tokio::test]
async fn pending_cancel_delivers_waiter_once() {
    let mut harness = harness(true, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("pending-cancel", true)).await }
    });
    tokio::task::yield_now().await;
    let wait = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.query("pending-cancel", true, Some(60_000)).await }
    });
    tokio::task::yield_now().await;
    assert!(matches!(
        harness.backend.cancel("pending-cancel").await,
        SubagentCancelOutcome::Cancelled
    ));
    let snapshot = wait.await.unwrap().unwrap();
    assert!(matches!(
        snapshot.status,
        SubagentSnapshotStatus::Cancelled { .. }
    ));
    let disposition = harness.completions.recv().await.unwrap();
    assert!(disposition.waiter_delivered);
    assert!(disposition.explicitly_killed);
    assert!(!disposition.should_surface);
    assert!(spawn.await.unwrap().unwrap().cancelled);
    harness.actor.abort();
}

#[tokio::test]
async fn caller_drop_during_initialization_does_not_drop_owned_run() {
    let mut harness = harness(true, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("owned", false)).await }
    });
    tokio::task::yield_now().await;
    spawn.abort();
    let _ = spawn.await;

    let initializing = harness.backend.query("owned", false, None).await.unwrap();
    assert!(matches!(
        initializing.status,
        SubagentSnapshotStatus::Initializing
    ));
    let _ = harness.start.send(());
    tokio::task::yield_now().await;
    let _ = harness.finish.send(());
    let disposition = harness.completions.recv().await.unwrap();
    assert!(
        disposition.should_surface,
        "dropped foreground receiver becomes handle-only"
    );
    let terminal = harness.backend.query("owned", false, None).await.unwrap();
    assert!(terminal.status.is_terminal());
    harness.actor.abort();
}

#[tokio::test]
async fn abandoned_foreground_caller_clears_outstanding() {
    // ParentGone parity: dropping the spawn await must leave Outstanding
    // (turn-freeze) without waiting for the foreground budget.
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("abandoned", false)).await }
    });
    tokio::task::yield_now().await;
    assert_eq!(
        outstanding(&harness.backend, "prompt").await.live_ids,
        vec!["abandoned".to_owned()],
        "live foreground child blocks the turn"
    );

    spawn.abort();
    let _ = spawn.await;
    assert_eq!(
        outstanding(&harness.backend, "prompt").await,
        SubagentOutstandingReply {
            live_ids: Vec::new(),
            background_live: true,
            subagent_usage_not_applied: false,
        },
        "caller-gone foreground is handle-only for Outstanding"
    );
    let running = harness
        .backend
        .query("abandoned", false, None)
        .await
        .unwrap();
    assert!(running.is_running(), "child keeps running after ParentGone");

    let _ = harness.finish.send(());
    let disposition = harness.completions.recv().await.unwrap();
    assert!(disposition.backgrounded);
    assert!(disposition.should_surface);
    harness.actor.abort();
}

#[tokio::test]
async fn duplicate_subagent_id_is_rejected_without_replacing_live_child() {
    let harness = harness(false, std::time::Duration::from_secs(60));
    let first = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("duplicate", true)).await }
    });
    tokio::task::yield_now().await;

    let duplicate = harness
        .backend
        .spawn(request("duplicate", false))
        .await
        .expect("duplicate rejection is a lifecycle result");
    assert!(!duplicate.success);
    assert!(
        duplicate
            .error
            .as_deref()
            .is_some_and(|error| error.contains("already exists"))
    );

    let running = harness
        .backend
        .query("duplicate", false, None)
        .await
        .expect("original child remains queryable");
    assert!(running.is_running());
    let _ = harness.finish.send(());
    assert!(first.await.unwrap().unwrap().success);
    harness.actor.abort();
}

#[tokio::test]
async fn external_cancel_token_cancels_live_child() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let request = request("external-cancel", false);
    let cancel_token = request.cancel_token.clone();
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request).await }
    });
    assert_eq!(
        harness.started.recv().await.as_deref(),
        Some("external-cancel")
    );

    cancel_token.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), spawn)
        .await
        .expect("external cancellation should finish")
        .unwrap()
        .unwrap();
    assert!(result.cancelled);
    let disposition = harness.completions.recv().await.unwrap();
    assert!(!disposition.explicitly_killed);
    harness.actor.abort();
}

#[tokio::test]
async fn dropping_coordinator_cancels_live_child() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let cancellation = CancellationToken::new();
    let mut request = request("owner-drop", true);
    request.cancel_token = cancellation.clone();
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("owner-drop"));

    harness.actor.abort();
    tokio::time::timeout(std::time::Duration::from_secs(1), cancellation.cancelled())
        .await
        .expect("coordinator drop should cancel child");
    assert!(spawn.await.unwrap().is_err());
}

#[tokio::test(start_paused = true)]
async fn await_to_completion_has_no_foreground_deadline() {
    let mut harness = harness(false, std::time::Duration::from_secs(1));
    let mut request = request("await-completion", false);
    request.await_to_completion = true;
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request).await }
    });
    assert_eq!(
        harness.started.recv().await.as_deref(),
        Some("await-completion")
    );

    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    assert!(!spawn.is_finished());
    let _ = harness.finish.send(());
    let result = spawn.await.unwrap().unwrap();
    assert!(result.success);
    assert!(!result.backgrounded);
    harness.actor.abort();
}

#[tokio::test]
async fn workflow_cancel_waits_for_drain_and_hides_owned_children() {
    let mut harness = harness_with_options(
        true,
        true,
        CoordinatorConfig {
            buffer_completions: true,
            ..CoordinatorConfig::default()
        },
    );

    let mut active_request = request("workflow-active", false);
    active_request.await_to_completion = true;
    active_request.owner = SubagentOwner::workflow("workflow-run");
    let active_spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(active_request).await }
    });
    assert_eq!(
        harness
            .requests
            .recv()
            .await
            .as_ref()
            .map(|request| request.id.as_str()),
        Some("workflow-active")
    );
    let _ = harness.start.send(());
    assert_eq!(
        harness.started.recv().await.as_deref(),
        Some("workflow-active")
    );

    let mut pending_request = request("workflow-pending", false);
    pending_request.await_to_completion = true;
    pending_request.owner = SubagentOwner::workflow("workflow-run");
    let pending_spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(pending_request).await }
    });
    assert_eq!(
        harness
            .requests
            .recv()
            .await
            .as_ref()
            .map(|request| request.id.as_str()),
        Some("workflow-pending")
    );

    assert!(
        harness
            .backend
            .query("workflow-active", false, None)
            .await
            .is_none()
    );
    assert!(
        harness
            .backend
            .query("workflow-pending", false, None)
            .await
            .is_none()
    );
    assert!(harness.backend.inspect("workflow-active").await.is_some());
    assert!(harness.backend.inspect("workflow-pending").await.is_some());
    assert!(harness.backend.list_running("parent").await.is_empty());
    let (list_respond_to, list_response_rx) = oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::ListActive(SubagentListActiveRequest {
            parent_session_id: "parent".to_owned(),
            respond_to: list_respond_to,
        }))
        .expect("actor command channel open");
    assert!(list_response_rx.await.unwrap().is_empty());

    let (cancel_respond_to, mut cancel_response_rx) = oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::Cancel(SubagentCancelRequest {
            parent_session_id: Some("parent".to_owned()),
            target: SubagentCancelTarget::WorkflowRunId("workflow-run".to_owned()),
            respond_to: cancel_respond_to,
        }))
        .expect("actor command channel open");
    assert!(harness.backend.inspect("workflow-active").await.is_some());
    assert!(matches!(
        cancel_response_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    let _ = harness.finish.send(());
    assert!(matches!(
        cancel_response_rx.await.unwrap(),
        SubagentCancelOutcome::Cancelled
    ));
    assert!(active_spawn.await.unwrap().unwrap().cancelled);
    assert!(pending_spawn.await.unwrap().unwrap().cancelled);
    assert!(
        harness
            .backend
            .query("workflow-active", false, None)
            .await
            .is_none()
    );
    assert!(harness.backend.inspect("workflow-active").await.is_some());

    let (completions_respond_to, completions_response_rx) = oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::Completions(SubagentCompletionsRequest {
            parent_session_id: Some("parent".to_owned()),
            suppress_ids: Vec::new(),
            respond_to: completions_respond_to,
        }))
        .expect("actor command channel open");
    assert!(completions_response_rx.await.unwrap().is_empty());
    harness.actor.abort();
}

/// Spawn an await-to-completion child under `session` and consume its request
/// event, returning the join handle for the in-flight spawn.
async fn spawn_session_child(
    harness: &mut Harness,
    id: &str,
    session: &str,
) -> tokio::task::JoinHandle<Result<SubagentResult, echo_agent_tool_runtime::ToolError>> {
    let mut req = request(id, false);
    req.await_to_completion = true;
    req.parent_session_id = session.to_owned();
    let backend = harness.backend.clone();
    let handle = tokio::spawn(async move { backend.spawn(req).await });
    assert_eq!(
        harness
            .requests
            .recv()
            .await
            .as_ref()
            .map(|r| r.id.as_str()),
        Some(id)
    );
    handle
}

#[tokio::test]
async fn teardown_session_children_spares_other_sessions() {
    let mut harness = harness(true, std::time::Duration::from_secs(60));

    // Two children under "parent" (one active, one pending) plus one under a
    // different session that must survive.
    let keep = spawn_session_child(&mut harness, "keep-active", "other").await;
    let kill_active = spawn_session_child(&mut harness, "kill-active", "parent").await;

    // Start the children spawned so far; kill-pending subscribes after start, so
    // it never receives it and stays pending.
    let _ = harness.start.send(());
    let mut started = std::collections::HashSet::new();
    started.insert(harness.started.recv().await.unwrap());
    started.insert(harness.started.recv().await.unwrap());
    assert!(started.contains("keep-active") && started.contains("kill-active"));

    let kill_pending = spawn_session_child(&mut harness, "kill-pending", "parent").await;

    harness
        .backend
        .sender()
        .send(SubagentEvent::TeardownSession {
            parent_session_id: "parent".to_owned(),
            respond_to: None,
        })
        .expect("actor command channel open");

    assert!(kill_active.await.unwrap().unwrap().cancelled);
    assert!(kill_pending.await.unwrap().unwrap().cancelled);

    assert!(
        !keep.is_finished(),
        "a different session's child must not be cancelled"
    );
    let _ = harness.finish.send(());
    let keep = keep.await.unwrap().unwrap();
    assert!(keep.success && !keep.cancelled);
    harness.actor.abort();
}

#[tokio::test]
async fn teardown_holds_admission_until_children_drain_then_reopens() {
    // wait_after_cancel keeps the cancelled child in `active` until finish, so
    // the delete-path hold stays live across the whole flow: a mid-drain spawn
    // is refused, OpenSpawnAdmission cannot reopen it, and once the child
    // finishes the ack resolves and a later spawn is admitted again.
    let mut harness = harness_with_options(true, true, CoordinatorConfig::default());
    let child = spawn_session_child(&mut harness, "slow", "parent").await;
    let _ = harness.start.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("slow"));

    let (tx, rx) = tokio::sync::oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::TeardownSession {
            parent_session_id: "parent".to_owned(),
            respond_to: Some(tx),
        })
        .expect("actor command channel open");

    // Same channel as TeardownSession: FIFO guarantees the block is set first.
    let mut late = request("late", false);
    late.parent_session_id = "parent".to_owned();
    assert!(
        harness.backend.spawn(late).await.unwrap().cancelled,
        "spawn must be refused while the teardown drains"
    );

    // OpenSpawnAdmission must not reopen mid-drain (CancelTurn → next-prompt
    // race during /delete).
    harness
        .backend
        .sender()
        .send(SubagentEvent::OpenSpawnAdmission {
            parent_session_id: "parent".to_owned(),
        })
        .expect("actor command channel open");
    let mut still_blocked = request("still-blocked", false);
    still_blocked.parent_session_id = "parent".to_owned();
    assert!(
        harness
            .backend
            .spawn(still_blocked)
            .await
            .unwrap()
            .cancelled,
        "OpenSpawnAdmission must not reopen during teardown drain"
    );

    // Finishing the cancelled child drains the session and resolves the ack.
    let _ = harness.finish.send(());
    assert!(child.await.unwrap().unwrap().cancelled);
    tokio::time::timeout(std::time::Duration::from_secs(2), rx)
        .await
        .expect("drain ack")
        .expect("drain channel");

    // A later spawn on the same session is admitted and runs to completion.
    let admitted = tokio::spawn({
        let backend = harness.backend.clone();
        let mut req = request("after-drain", false);
        req.parent_session_id = "parent".to_owned();
        async move { backend.spawn(req).await }
    });
    assert_eq!(
        harness
            .requests
            .recv()
            .await
            .as_ref()
            .map(|r| r.id.as_str()),
        Some("after-drain")
    );
    let _ = harness.start.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("after-drain"));
    let _ = harness.finish.send(());
    let result = admitted.await.unwrap().unwrap();
    assert!(
        result.success && !result.cancelled,
        "spawn must be admitted once the teardown drain completes: {result:?}"
    );
    harness.actor.abort();
}

#[tokio::test(start_paused = true)]
async fn teardown_drain_deadline_reopens_spawns() {
    // A cancelled child that never finishes must not block spawns for the
    // process lifetime: the coordinator's backstop deadline force-reopens.
    let mut harness = harness_with_options(true, true, CoordinatorConfig::default());
    let child = spawn_session_child(&mut harness, "stuck", "parent").await;
    let _ = harness.start.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("stuck"));

    let (tx, rx) = tokio::sync::oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::TeardownSession {
            parent_session_id: "parent".to_owned(),
            respond_to: Some(tx),
        })
        .expect("actor command channel open");

    // Blocked: the cancelled child stays in `active` (finish never fired).
    let mut blocked = request("blocked", false);
    blocked.parent_session_id = "parent".to_owned();
    assert!(
        harness.backend.spawn(blocked).await.unwrap().cancelled,
        "spawn must be refused while the teardown drains"
    );

    // Past the backstop: the hold force-clears and the ack resolves even
    // though the child is still stuck.
    tokio::time::advance(TEARDOWN_DRAIN_MAX + std::time::Duration::from_secs(1)).await;
    tokio::time::timeout(std::time::Duration::from_secs(2), rx)
        .await
        .expect("drain ack after deadline")
        .expect("drain channel");

    // Admission reopened: a later spawn on the same session starts.
    let _admitted = tokio::spawn({
        let backend = harness.backend.clone();
        let mut req = request("after-deadline", false);
        req.parent_session_id = "parent".to_owned();
        async move { backend.spawn(req).await }
    });
    assert_eq!(
        harness
            .requests
            .recv()
            .await
            .as_ref()
            .map(|r| r.id.as_str()),
        Some("after-deadline"),
        "spawn must be admitted once the drain deadline force-reopens admission"
    );

    let _ = harness.finish.send(());
    let _ = child.await;
    harness.actor.abort();
}

#[tokio::test]
async fn teardown_cancels_background_child_without_rebuffering() {
    let mut harness = harness_with_config(
        true,
        CoordinatorConfig {
            buffer_completions: true,
            ..CoordinatorConfig::default()
        },
    );

    // A background subagent that outlives its parent is the production case that
    // rebuffers a completion for a later resume of the same session id.
    let mut req = request("bg", true);
    req.parent_session_id = "parent".to_owned();
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(req).await }
    });
    assert_eq!(
        harness
            .requests
            .recv()
            .await
            .as_ref()
            .map(|r| r.id.as_str()),
        Some("bg")
    );
    let _ = harness.start.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("bg"));

    harness
        .backend
        .sender()
        .send(SubagentEvent::TeardownSession {
            parent_session_id: "parent".to_owned(),
            respond_to: None,
        })
        .expect("actor command channel open");

    // Wait for the cancelled child to finish, then assert it buffered nothing.
    let _ = harness.completions.recv().await;
    let (tx, rx) = oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::Completions(SubagentCompletionsRequest {
            parent_session_id: Some("parent".to_owned()),
            suppress_ids: Vec::new(),
            respond_to: tx,
        }))
        .expect("actor command channel open");
    assert!(
        rx.await.unwrap().is_empty(),
        "torn-down background child must not rebuffer a completion"
    );
    let _ = spawn.await;
    harness.actor.abort();
}

#[tokio::test]
async fn teardown_rejects_spawn_from_cancelled_parent() {
    // wait_after_cancel keeps the cancelled parent in `active`, so its late
    // nested Spawn still finds it.
    let mut harness = harness_with_options(true, true, CoordinatorConfig::default());

    // A parent subagent whose child_session_id is "A".
    let parent = spawn_session_child(&mut harness, "A", "parent").await;
    let _ = harness.start.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("A"));

    harness
        .backend
        .sender()
        .send(SubagentEvent::TeardownSession {
            parent_session_id: "parent".to_owned(),
            respond_to: None,
        })
        .expect("actor command channel open");

    // A nested Spawn from the now-cancelled parent (parent_session_id = its
    // child_session_id) must be rejected, not reparented and left running.
    let mut nested = request("B", false);
    nested.await_to_completion = true;
    nested.parent_session_id = "A".to_owned();
    let outcome = harness.backend.spawn(nested).await.unwrap();
    assert!(outcome.cancelled && !outcome.success);

    let _ = harness.finish.send(());
    let _ = parent.await;
    harness.actor.abort();
}

#[tokio::test]
async fn usage_events_feed_sorted_outstanding_reply() {
    let mut harness = harness(true, std::time::Duration::from_secs(60));
    let mut spawns = Vec::new();
    for (id, is_background) in [
        ("z-foreground", false),
        ("a-foreground", false),
        ("background", true),
    ] {
        spawns.push(tokio::spawn({
            let backend = harness.backend.clone();
            async move { backend.spawn(request(id, is_background)).await }
        }));
        assert_eq!(
            harness
                .requests
                .recv()
                .await
                .as_ref()
                .map(|request| request.id.as_str()),
            Some(id)
        );
    }

    let (foreign_respond_to, foreign_response_rx) = oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::MarkUsageNotApplied(
            SubagentMarkUsageNotAppliedRequest {
                parent_session_id: "foreign".to_owned(),
                prompt_id: "prompt".to_owned(),
                respond_to: foreign_respond_to,
            },
        ))
        .expect("actor command channel open");
    foreign_response_rx.await.expect("mark acknowledgement");
    assert!(
        !outstanding(&harness.backend, "prompt")
            .await
            .subagent_usage_not_applied
    );

    let (mark_respond_to, mark_response_rx) = oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::MarkUsageNotApplied(
            SubagentMarkUsageNotAppliedRequest {
                parent_session_id: "parent".to_owned(),
                prompt_id: "prompt".to_owned(),
                respond_to: mark_respond_to,
            },
        ))
        .expect("actor command channel open");
    mark_response_rx.await.expect("mark acknowledgement");
    assert_eq!(
        outstanding(&harness.backend, "prompt").await,
        SubagentOutstandingReply {
            live_ids: vec!["a-foreground".to_owned(), "z-foreground".to_owned()],
            background_live: true,
            subagent_usage_not_applied: true,
        }
    );

    harness
        .backend
        .sender()
        .send(SubagentEvent::ClearUsageNotApplied(
            SubagentClearUsageNotAppliedRequest {
                parent_session_id: "parent".to_owned(),
                prompt_id: "prompt".to_owned(),
            },
        ))
        .expect("actor command channel open");
    assert_eq!(
        outstanding(&harness.backend, "prompt").await,
        SubagentOutstandingReply {
            live_ids: vec!["a-foreground".to_owned(), "z-foreground".to_owned()],
            background_live: true,
            subagent_usage_not_applied: false,
        }
    );

    assert!(matches!(
        harness.backend.cancel_parent_prompt("prompt").await,
        SubagentCancelOutcome::Cancelled
    ));
    for spawn in spawns {
        assert!(spawn.await.unwrap().unwrap().cancelled);
    }
    harness.actor.abort();
}

/// Prior-turn background + current-turn children all die on ParentSession cancel (GBT-4942).
#[tokio::test]
async fn cancel_parent_session_kills_prior_turn_background() {
    let mut harness = harness(true, std::time::Duration::from_secs(60));
    let mut prior = request("prior-bg", true);
    prior.parent_prompt_id = Some("turn-1".into());
    let mut current = request("current", false);
    current.parent_prompt_id = Some("turn-2".into());
    let mut spawns = Vec::new();
    for req in [prior, current] {
        let id = req.id.clone();
        spawns.push(tokio::spawn({
            let backend = harness.backend.clone();
            async move { backend.spawn(req).await }
        }));
        assert_eq!(
            harness
                .requests
                .recv()
                .await
                .as_ref()
                .map(|r| r.id.as_str()),
            Some(id.as_str())
        );
        let _ = harness.start.send(());
        assert_eq!(harness.started.recv().await.as_deref(), Some(id.as_str()));
    }
    assert!(matches!(
        parent_backend(&harness).cancel_parent_session().await,
        SubagentCancelOutcome::Cancelled
    ));
    for spawn in spawns {
        assert!(
            spawn.await.unwrap().unwrap().cancelled,
            "ParentSession cancel must kill prior-turn and current-turn children"
        );
    }
    harness.actor.abort();
}

/// A foreign session's children must not die when this session Stop fires.
#[tokio::test]
async fn cancel_parent_session_does_not_touch_foreign_session() {
    let mut harness = harness(true, std::time::Duration::from_secs(60));
    let mut foreign = request("foreign-child", true);
    foreign.parent_session_id = "other-session".into();
    let foreign_spawn = tokio::spawn({
        let backend = ChannelBackend::for_session(harness.backend.sender(), "other-session");
        async move { backend.spawn(foreign).await }
    });
    assert_eq!(
        harness
            .requests
            .recv()
            .await
            .as_ref()
            .map(|r| r.id.as_str()),
        Some("foreign-child")
    );
    let _ = harness.start.send(());
    assert_eq!(
        harness.started.recv().await.as_deref(),
        Some("foreign-child")
    );

    assert!(matches!(
        parent_backend(&harness).cancel_parent_session().await,
        SubagentCancelOutcome::Cancelled
    ));
    // Foreign child still running — finish it successfully.
    let _ = harness.finish.send(());
    let result = foreign_spawn.await.unwrap().unwrap();
    assert!(result.success && !result.cancelled);
    harness.actor.abort();
}

/// Unbound backend must not wildcard-cancel (rejects before send).
#[tokio::test]
async fn cancel_parent_session_unbound_backend_is_not_found() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let unbound = ChannelBackend::new(tx);
    assert!(matches!(
        unbound.cancel_parent_session().await,
        SubagentCancelOutcome::NotFound
    ));
}

/// Late Task spawn after ParentSession cancel is rejected until admission reopens.
#[tokio::test]
async fn cancel_parent_session_rejects_late_spawn_until_admission_reopens() {
    let mut harness = harness(true, std::time::Duration::from_secs(60));
    let bound = parent_backend(&harness);
    let prior = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("prior", true)).await }
    });
    assert_eq!(
        harness
            .requests
            .recv()
            .await
            .as_ref()
            .map(|r| r.id.as_str()),
        Some("prior")
    );
    let _ = harness.start.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("prior"));

    assert!(matches!(
        bound.cancel_parent_session().await,
        SubagentCancelOutcome::Cancelled
    ));
    assert!(prior.await.unwrap().unwrap().cancelled);

    // Late Task spawn is rejected by the coordinator gate (request still carries
    // parent="parent" via unbound backend + request default).
    let late = harness
        .backend
        .spawn(request("late-after-stop", true))
        .await
        .unwrap();
    assert!(
        late.cancelled && !late.success,
        "late Task spawn after ParentSession must be rejected"
    );

    assert!(bound.open_spawn_admission());
    let allowed = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("after-reopen", true)).await }
    });
    assert_eq!(
        harness
            .requests
            .recv()
            .await
            .as_ref()
            .map(|r| r.id.as_str()),
        Some("after-reopen")
    );
    let _ = harness.start.send(());
    assert_eq!(
        harness.started.recv().await.as_deref(),
        Some("after-reopen")
    );
    let _ = harness.finish.send(());
    assert!(allowed.await.unwrap().unwrap().success);
    harness.actor.abort();
}

/// Nested children of a workflow subagent keep workflow ownership after reparent
/// and survive ParentSession cancel (active + pending).
#[tokio::test]
async fn cancel_parent_session_spares_nested_workflow_children() {
    // wait_before_start only: keep one child in pending through ParentSession.
    // (wait_after_cancel not needed — workflow lineage is not cancelled.)
    let mut harness = harness(true, std::time::Duration::from_secs(60));

    // Workflow-owned parent child (child_session_id = "wf-child").
    let mut wf_parent = request("wf-child", true);
    wf_parent.owner = SubagentOwner::workflow("run-1");
    let wf_spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(wf_parent).await }
    });
    assert_eq!(
        harness
            .requests
            .recv()
            .await
            .as_ref()
            .map(|r| r.id.as_str()),
        Some("wf-child")
    );
    let _ = harness.start.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("wf-child"));

    // Nested spawns use a backend bound to the workflow child's session id
    // (production binds ChannelBackend::for_session to the child session).
    let child_backend = ChannelBackend::for_session(harness.backend.sender(), "wf-child");

    // Nested Task-owned spawn from the workflow child (reparented to root parent).
    let nested_active = request("nested-active", true);
    let nested_active_spawn = tokio::spawn({
        let backend = child_backend.clone();
        async move { backend.spawn(nested_active).await }
    });
    let observed = harness
        .requests
        .recv()
        .await
        .expect("nested active observed");
    assert_eq!(observed.parent_session_id, "parent");
    assert_eq!(
        observed.owner.workflow_run_id(),
        Some("run-1"),
        "reparent must copy workflow lineage"
    );
    let _ = harness.start.send(());
    assert_eq!(
        harness.started.recv().await.as_deref(),
        Some("nested-active")
    );

    // Nested pending (not started yet) under the same workflow child.
    let nested_pending = request("nested-pending", true);
    let nested_pending_spawn = tokio::spawn({
        let backend = child_backend.clone();
        async move { backend.spawn(nested_pending).await }
    });
    let observed_pending = harness
        .requests
        .recv()
        .await
        .expect("nested pending observed");
    assert_eq!(observed_pending.owner.workflow_run_id(), Some("run-1"));

    assert!(matches!(
        parent_backend(&harness).cancel_parent_session().await,
        SubagentCancelOutcome::Cancelled
    ));

    // Promote pending → active, then finish all three (must wait until each is
    // subscribed on finish; broadcast does not buffer for late receivers).
    let _ = harness.start.send(());
    assert_eq!(
        harness.started.recv().await.as_deref(),
        Some("nested-pending")
    );
    let _ = harness.finish.send(());
    assert!(
        nested_active_spawn.await.unwrap().unwrap().success,
        "active nested workflow child must survive ParentSession"
    );
    assert!(
        nested_pending_spawn.await.unwrap().unwrap().success,
        "pending nested workflow child must survive ParentSession"
    );
    assert!(
        wf_spawn.await.unwrap().unwrap().success,
        "workflow parent must survive ParentSession"
    );
    harness.actor.abort();
}

#[tokio::test]
async fn loop_tracking_covers_pending_active_and_nested_reparenting() {
    let mut harness = harness(true, std::time::Duration::from_secs(60));
    let mut outer_request = request("outer", true);
    outer_request.runtime_overrides.loop_task_id = Some("loop-task".to_owned());
    let outer_spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(outer_request).await }
    });
    let observed_outer = harness.requests.recv().await.unwrap();
    assert_eq!(observed_outer.parent_session_id, "parent");
    assert!(loop_unit_active(&harness.backend, "loop-task").await);

    let _ = harness.start.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("outer"));
    let refs = harness
        .backend
        .spawned_refs_for_prompt("parent", "prompt")
        .await;
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].description, "test child");

    let mut nested_request = request("nested", true);
    nested_request.parent_session_id = "outer".to_owned();
    let nested_spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(nested_request).await }
    });
    let observed_nested = harness.requests.recv().await.unwrap();
    assert_eq!(observed_nested.parent_session_id, "parent");
    assert!(!observed_nested.surface_completion);
    assert_eq!(
        observed_nested.runtime_overrides.loop_task_id.as_deref(),
        Some("loop-task")
    );
    assert!(loop_unit_active(&harness.backend, "loop-task").await);

    let _ = harness.start.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("nested"));
    let _ = harness.finish.send(());
    assert!(outer_spawn.await.unwrap().unwrap().success);
    assert!(nested_spawn.await.unwrap().unwrap().success);
    assert!(!loop_unit_active(&harness.backend, "loop-task").await);
    harness.actor.abort();
}

#[tokio::test]
async fn completion_buffer_caps_summary_without_mutating_result() {
    let mut harness = harness_with_config(
        false,
        CoordinatorConfig {
            buffer_completions: true,
            ..CoordinatorConfig::default()
        },
    );
    let mut request = request("buffered", true);
    request.prompt = "aéb".to_owned();
    request.runtime_overrides.completion_output_cap = Some(2);
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("buffered"));
    let _ = harness.finish.send(());
    let result = spawn.await.unwrap().unwrap();
    assert_eq!(result.output.as_ref(), "aéb");
    let _ = harness.completions.recv().await;
    let snapshot = harness
        .backend
        .query("buffered", false, None)
        .await
        .unwrap();
    let SubagentSnapshotStatus::Completed { output, .. } = snapshot.status else {
        panic!("expected completed snapshot");
    };
    assert_eq!(output, "aéb");

    let (respond_to, response_rx) = oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::Completions(SubagentCompletionsRequest {
            parent_session_id: Some("parent".to_owned()),
            suppress_ids: Vec::new(),
            respond_to,
        }))
        .expect("actor command channel open");
    let buffered = response_rx.await.expect("completion response");
    assert_eq!(buffered.len(), 1);
    assert_eq!(buffered[0].subagent_id, "buffered");
    assert_eq!(
        buffered[0].output.as_ref(),
        "a\n[output truncated: 1 of 4 bytes shown]"
    );
    harness.actor.abort();
}

/// Regression (review): an agent definition with `background: true` spawned
/// with a BLOCKING tool call (`run_in_background: false`) is background for
/// Outstanding/freeze accounting — not turn-blocking — while the spawn caller
/// still receives the result inline.
#[tokio::test]
async fn definition_background_counts_as_background_for_outstanding() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let mut blocking_request = request("bg-def", false);
    blocking_request.subagent_type = "background-default".to_owned();
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(blocking_request).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("bg-def"));

    // Started with definition background: live for the child itself but not
    // turn-blocking; the prompt sees it as background work.
    assert_eq!(
        outstanding(&harness.backend, "prompt").await,
        SubagentOutstandingReply {
            live_ids: Vec::new(),
            background_live: true,
            subagent_usage_not_applied: false,
        }
    );

    // The blocking caller still gets the completed result inline.
    let _ = harness.finish.send(());
    let result = spawn.await.unwrap().unwrap();
    assert!(result.success);
    assert!(!result.backgrounded);
    harness.actor.abort();
}

#[tokio::test]
async fn buffered_completion_output_cap_bounds_buffered_summary() {
    let mut harness = harness_with_config(
        false,
        CoordinatorConfig {
            buffer_completions: true,
            buffered_completion_output_cap: Some(8),
            ..CoordinatorConfig::default()
        },
    );
    let mut request = request("capped", true);
    request.prompt = "x".repeat(64);
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("capped"));
    let _ = harness.finish.send(());
    // Spawn result and queryable snapshot keep the full output…
    let result = spawn.await.unwrap().unwrap();
    assert_eq!(result.output.len(), 64);
    let _ = harness.completions.recv().await;

    // …only the buffered reminder copy is truncated.
    let (respond_to, response_rx) = oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::Completions(SubagentCompletionsRequest {
            parent_session_id: Some("parent".to_owned()),
            suppress_ids: Vec::new(),
            respond_to,
        }))
        .expect("actor command channel open");
    let buffered = response_rx.await.expect("completion response");
    assert_eq!(buffered.len(), 1);
    assert!(
        buffered[0]
            .output
            .contains("[output truncated: 8 of 64 bytes shown]"),
        "buffered output must be capped, got: {}",
        buffered[0].output
    );
    harness.actor.abort();
}

#[tokio::test]
async fn teardown_session_drops_only_that_sessions_buffer() {
    let mut harness = harness_with_config(
        false,
        CoordinatorConfig {
            buffer_completions: true,
            ..CoordinatorConfig::default()
        },
    );
    for (id, parent) in [("child-a", "parent-a"), ("child-b", "parent-b")] {
        let mut request = request(id, true);
        request.parent_session_id = parent.to_owned();
        let spawn = tokio::spawn({
            let backend = harness.backend.clone();
            async move { backend.spawn(request).await }
        });
        assert_eq!(harness.started.recv().await.as_deref(), Some(id));
        let _ = harness.finish.send(());
        assert!(spawn.await.unwrap().unwrap().success);
        let _ = harness.completions.recv().await;
    }

    // Tearing down parent-a discards its buffered completion...
    harness
        .backend
        .sender()
        .send(SubagentEvent::TeardownSession {
            parent_session_id: "parent-a".to_owned(),
            respond_to: None,
        })
        .expect("actor command channel open");

    let drain = |parent: &str| {
        let sender = harness.backend.sender();
        let parent = parent.to_owned();
        async move {
            let (respond_to, response_rx) = oneshot::channel();
            sender
                .send(SubagentEvent::Completions(SubagentCompletionsRequest {
                    parent_session_id: Some(parent),
                    suppress_ids: Vec::new(),
                    respond_to,
                }))
                .expect("actor command channel open");
            response_rx.await.expect("completion response")
        }
    };
    assert!(drain("parent-a").await.is_empty());
    // ...while parent-b's completion stays buffered for its own drain.
    let b = drain("parent-b").await;
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].subagent_id, "child-b");
    harness.actor.abort();
}

#[tokio::test]
async fn completion_drain_is_scoped_to_parent_session() {
    let mut harness = harness_with_config(
        false,
        CoordinatorConfig {
            buffer_completions: true,
            ..CoordinatorConfig::default()
        },
    );
    for (id, parent) in [("child-a", "parent-a"), ("child-b", "parent-b")] {
        let mut request = request(id, true);
        request.parent_session_id = parent.to_owned();
        let spawn = tokio::spawn({
            let backend = harness.backend.clone();
            async move { backend.spawn(request).await }
        });
        assert_eq!(harness.started.recv().await.as_deref(), Some(id));
        let _ = harness.finish.send(());
        assert!(spawn.await.unwrap().unwrap().success);
        let _ = harness.completions.recv().await;
    }

    for (parent, expected_id) in [("parent-a", "child-a"), ("parent-b", "child-b")] {
        let (respond_to, response_rx) = oneshot::channel();
        harness
            .backend
            .sender()
            .send(SubagentEvent::Completions(SubagentCompletionsRequest {
                parent_session_id: Some(parent.to_owned()),
                suppress_ids: Vec::new(),
                respond_to,
            }))
            .expect("actor command channel open");
        let completions = response_rx.await.expect("completion response");
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].subagent_id, expected_id);
    }
    harness.actor.abort();
}

#[tokio::test]
async fn blocking_query_of_completed_child_returns_immediately() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("already-done", true)).await }
    });
    tokio::task::yield_now().await;
    let _ = harness.finish.send(());
    assert!(spawn.await.unwrap().unwrap().success);
    let _ = harness.completions.recv().await;

    let started = std::time::Instant::now();
    let snapshot = harness
        .backend
        .query("already-done", true, Some(600_000))
        .await
        .expect("completed child");
    assert!(snapshot.status.is_terminal());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "already-completed query must not burn the 600s cap; elapsed {:?}",
        started.elapsed()
    );
    harness.actor.abort();
}

#[tokio::test]
async fn blocking_query_of_cancelled_child_returns_immediately() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("already-killed", true)).await }
    });
    assert_eq!(
        harness.started.recv().await.as_deref(),
        Some("already-killed")
    );
    assert!(matches!(
        harness.backend.cancel("already-killed").await,
        SubagentCancelOutcome::Cancelled
    ));
    assert!(spawn.await.unwrap().unwrap().cancelled);
    let _ = harness.completions.recv().await;

    let started = std::time::Instant::now();
    let snapshot = harness
        .backend
        .query("already-killed", true, Some(600_000))
        .await
        .expect("cancelled child");
    assert!(matches!(
        snapshot.status,
        SubagentSnapshotStatus::Cancelled { .. }
    ));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "already-cancelled query must not burn the 600s cap; elapsed {:?}",
        started.elapsed()
    );
    harness.actor.abort();
}

#[tokio::test]
async fn blocking_query_of_unknown_id_returns_immediately() {
    let harness = harness(false, std::time::Duration::from_secs(60));
    let started = std::time::Instant::now();
    let snapshot = harness
        .backend
        .query("never-existed", true, Some(600_000))
        .await;
    assert!(snapshot.is_none());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "not-found query must not burn the 600s cap; elapsed {:?}",
        started.elapsed()
    );
    harness.actor.abort();
}

#[tokio::test]
async fn session_backend_cannot_query_or_cancel_foreign_child() {
    let mut harness = harness(false, std::time::Duration::from_secs(60));
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("scoped", true)).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("scoped"));

    let foreign = ChannelBackend::for_session(harness.backend.sender(), "foreign-parent");
    assert!(foreign.query("scoped", false, None).await.is_none());
    assert!(foreign.inspect("scoped").await.is_none());
    assert!(matches!(
        foreign.cancel("scoped").await,
        SubagentCancelOutcome::NotFound
    ));

    assert!(matches!(
        harness.backend.cancel("scoped").await,
        SubagentCancelOutcome::Cancelled
    ));
    assert!(spawn.await.unwrap().unwrap().cancelled);
    let _ = harness.completions.recv().await;
    harness.actor.abort();
}

#[tokio::test]
async fn completed_cache_evicts_oldest_entry_at_cap() {
    let mut harness = harness_with_config(
        false,
        CoordinatorConfig {
            foreground_budget: std::time::Duration::from_secs(60),
            ..CoordinatorConfig::default()
        },
    );
    for index in 0..=MAX_COMPLETED_ENTRIES {
        let id = format!("cache-{index:04}");
        let spawn = tokio::spawn({
            let backend = harness.backend.clone();
            let request = request(&id, true);
            async move { backend.spawn(request).await }
        });
        assert_eq!(harness.started.recv().await.as_deref(), Some(id.as_str()));
        let _ = harness.finish.send(());
        assert!(spawn.await.unwrap().unwrap().success);
    }

    assert!(
        harness
            .backend
            .query("cache-0000", false, None)
            .await
            .is_none()
    );
    assert!(
        harness
            .backend
            .query("cache-0001", false, None)
            .await
            .is_some()
    );
    assert!(
        harness
            .backend
            .query(&format!("cache-{MAX_COMPLETED_ENTRIES:04}"), false, None,)
            .await
            .is_some()
    );
    harness.actor.abort();
}

fn limited(max_concurrent: usize, behavior: LimitBehavior) -> CoordinatorConfig {
    CoordinatorConfig {
        limits: SubagentLimits {
            max_concurrent,
            behavior,
        },
        ..CoordinatorConfig::default()
    }
}

/// `limited`, plus a recording sink so tests can assert the notice payloads.
fn limited_with_sink(
    max_concurrent: usize,
    behavior: LimitBehavior,
) -> (
    CoordinatorConfig,
    mpsc::UnboundedReceiver<SubagentLimitNotice>,
) {
    let (notice_tx, notices) = mpsc::unbounded_channel();
    let config = CoordinatorConfig {
        limit_sink: Some(std::sync::Arc::new(move |notice| {
            let _ = notice_tx.send(notice);
        })),
        ..limited(max_concurrent, behavior)
    };
    (config, notices)
}

async fn await_queued(backend: &ChannelBackend, queued: usize) {
    for _ in 0..400 {
        if backend.registry_counts().await.queued == queued {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("queued count never reached {queued}");
}

#[tokio::test]
async fn spawns_past_the_concurrent_limit_queue_until_a_slot_frees() {
    let mut harness = harness_with_config(false, limited(2, LimitBehavior::Queue));
    let spawns: Vec<_> = (0..4)
        .map(|i| {
            tokio::spawn({
                let backend = harness.backend.clone();
                async move { backend.spawn(request(&format!("wave-{i}"), true)).await }
            })
        })
        .collect();

    for _ in 0..2 {
        harness.requests.recv().await.expect("child started");
    }
    await_queued(&harness.backend, 2).await;
    assert!(
        harness.requests.try_recv().is_err(),
        "a child started past the concurrent limit"
    );
    for i in 0..4 {
        assert!(
            harness
                .backend
                .query(&format!("wave-{i}"), false, None)
                .await
                .is_some(),
            "a queued or running spawn must stay queryable"
        );
    }

    let _ = harness.finish.send(());
    for _ in 0..2 {
        harness.requests.recv().await.expect("queued child started");
    }
    let _ = harness.finish.send(());
    for spawn in spawns {
        let result = spawn.await.expect("join").expect("spawn round-trips");
        assert!(result.success, "every queued spawn still runs: {result:?}");
    }
    // The launch-time concurrency count never exceeds the limit.
    for _ in 0..4 {
        let (id, _, session_running) = harness.queue_waits.recv().await.expect("ran");
        assert!(
            (1..=2).contains(&session_running),
            "{id} launched with session_running={session_running}, limit is 2"
        );
    }
    harness.actor.abort();
}

#[tokio::test]
async fn fail_mode_rejects_at_the_limit_and_recovers_when_a_slot_frees() {
    let mut harness = harness_with_config(false, limited(1, LimitBehavior::Fail));
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    harness.requests.recv().await.expect("first child started");

    let rejected = harness
        .backend
        .spawn(request("rejected", true))
        .await
        .expect("spawn round-trips");
    assert!(!rejected.success);
    assert!(
        rejected
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Concurrent subagent limit reached"),
        "unexpected error: {:?}",
        rejected.error
    );
    // The rejection surfaces like any failed background child and leaves a
    // failed record, so the id the model holds does not vanish.
    let disposition = harness.completions.recv().await.expect("disposition");
    assert!(disposition.should_surface);
    let snapshot = harness
        .backend
        .query("rejected", false, None)
        .await
        .expect("rejected id resolves");
    assert!(
        matches!(snapshot.status, SubagentSnapshotStatus::Failed { .. }),
        "expected a failed record, got {:?}",
        snapshot.status
    );

    let _ = harness.finish.send(());
    let held = held.await.expect("join").expect("spawn round-trips");
    assert!(held.success);

    let next = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("next", true)).await }
    });
    harness
        .requests
        .recv()
        .await
        .expect("spawning succeeds again once a slot frees");
    let _ = harness.finish.send(());
    let next = next.await.expect("join").expect("spawn round-trips");
    assert!(next.success);
    harness.actor.abort();
}

#[tokio::test]
async fn limit_notices_report_running_count_queue_depth_and_origin() {
    let (config, mut notices) = limited_with_sink(1, LimitBehavior::Queue);
    let mut harness = harness_with_config(false, config);
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    harness.requests.recv().await.expect("first child started");
    assert!(
        notices.try_recv().is_err(),
        "an admitted spawn must not notify the sink"
    );

    let parked = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("parked", true)).await }
    });
    await_queued(&harness.backend, 1).await;
    let notice = notices.recv().await.expect("queued notice");
    assert_eq!(notice.parent_session_id, "parent");
    assert_eq!(
        notice.decision,
        SubagentLimitDecision::QueuedAtConcurrentLimit { limit: 1 }
    );
    assert_eq!(
        (notice.running, notice.queue_depth),
        (1, 1),
        "a queued spawn counts itself in queue_depth"
    );
    assert_eq!(notice.origin, LimitedSpawnOrigin::Task);

    let mut loop_request = request("loop-fire", true);
    loop_request.runtime_overrides.loop_task_id = Some("loop-1".to_owned());
    let looped = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(loop_request).await }
    });
    await_queued(&harness.backend, 2).await;
    let notice = notices.recv().await.expect("loop-fire notice");
    assert_eq!((notice.running, notice.queue_depth), (1, 2));
    assert_eq!(notice.origin, LimitedSpawnOrigin::SchedulerLoop);

    let _ = harness.finish.send(());
    harness.requests.recv().await.expect("parked started");
    let _ = harness.finish.send(());
    harness.requests.recv().await.expect("loop-fire started");
    let _ = harness.finish.send(());
    for spawn in [held, parked, looped] {
        assert!(spawn.await.expect("join").expect("round-trips").success);
    }
    harness.actor.abort();
}

#[tokio::test]
async fn a_rejected_spawn_notice_excludes_itself_from_queue_depth() {
    let (config, mut notices) = limited_with_sink(1, LimitBehavior::Fail);
    let mut harness = harness_with_config(false, config);
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    harness.requests.recv().await.expect("first child started");

    let rejected = harness
        .backend
        .spawn(request("rejected", true))
        .await
        .expect("spawn round-trips");
    assert!(!rejected.success);
    let notice = notices.recv().await.expect("rejected notice");
    assert_eq!(
        notice.decision,
        SubagentLimitDecision::RejectedAtConcurrentLimit { limit: 1 }
    );
    assert_eq!(
        (notice.running, notice.queue_depth),
        (1, 0),
        "a rejected spawn never enters the queue"
    );

    let _ = harness.finish.send(());
    assert!(held.await.expect("join").expect("round-trips").success);
    harness.actor.abort();
}

#[tokio::test]
async fn teardown_purges_a_queued_spawn_without_rebuffering() {
    let mut harness = harness_with_config(
        false,
        CoordinatorConfig {
            buffer_completions: true,
            ..limited(1, LimitBehavior::Queue)
        },
    );
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("held"));
    let parked = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("parked", true)).await }
    });
    await_queued(&harness.backend, 1).await;

    harness
        .backend
        .sender()
        .send(SubagentEvent::TeardownSession {
            parent_session_id: "parent".to_owned(),
            respond_to: None,
        })
        .expect("actor command channel open");

    let parked = parked.await.expect("join").expect("spawn round-trips");
    assert!(parked.cancelled, "teardown must cancel the queued spawn");
    let held = held.await.expect("join").expect("spawn round-trips");
    assert!(held.cancelled);
    // Both completions processed; neither may rebuffer for a later resume
    // of the torn-down session id.
    for _ in 0..2 {
        let _ = harness.completions.recv().await;
    }
    let (tx, rx) = oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::Completions(SubagentCompletionsRequest {
            parent_session_id: Some("parent".to_owned()),
            suppress_ids: Vec::new(),
            respond_to: tx,
        }))
        .expect("actor command channel open");
    assert!(
        rx.await.unwrap().is_empty(),
        "a spawn parked at teardown must not rebuffer a completion"
    );
    harness.actor.abort();
}

#[tokio::test]
async fn dropping_the_actor_resolves_queued_callers_without_host_callbacks() {
    let mut harness = harness_with_config(false, limited(1, LimitBehavior::Queue));
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("held"));
    let parked = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("parked", true)).await }
    });
    await_queued(&harness.backend, 1).await;

    // Aborting the actor drops it mid-run: the destructor path.
    harness.actor.abort();
    let parked = parked.await.expect("join").expect("queued caller resolves");
    assert!(
        parked.cancelled,
        "the destructor must resolve a queued caller: {parked:?}"
    );
    // The running child's caller gets the channel-closed error: its reply
    // sender dies with the actor (pre-existing contract).
    assert!(held.await.expect("join").is_err());
    assert!(
        harness.completions.try_recv().is_err(),
        "the destructor must not run host completion callbacks"
    );
}

#[tokio::test]
async fn a_spawn_cancelled_while_queued_never_starts() {
    let mut harness = harness_with_config(false, limited(1, LimitBehavior::Queue));
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    harness.requests.recv().await.expect("first child started");

    let mut queued = request("queued", true);
    let cancel = CancellationToken::new();
    queued.cancel_token = cancel.clone();
    let queued = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(queued).await }
    });
    await_queued(&harness.backend, 1).await;

    cancel.cancel();
    let _ = harness.finish.send(());
    let result = queued.await.expect("join").expect("spawn round-trips");
    assert!(
        result.cancelled,
        "a spawn cancelled while queued must not run: {result:?}"
    );
    assert!(
        held.await
            .expect("join")
            .expect("spawn round-trips")
            .success
    );
    assert!(
        harness.requests.try_recv().is_err(),
        "the cancelled spawn reached the runner"
    );
    let snapshot = harness
        .backend
        .query("queued", false, None)
        .await
        .expect("a spawn cancelled while queued leaves a completed record");
    assert!(
        matches!(snapshot.status, SubagentSnapshotStatus::Cancelled { .. }),
        "queued-cancelled id must read as cancelled: {:?}",
        snapshot.status
    );
    harness.actor.abort();
}

#[tokio::test(start_paused = true)]
async fn a_queued_spawn_auto_backgrounds_its_caller_at_the_await_budget() {
    let mut harness = harness_with_config(
        false,
        CoordinatorConfig {
            foreground_budget: std::time::Duration::from_secs(1),
            limits: SubagentLimits {
                max_concurrent: 1,
                behavior: LimitBehavior::Queue,
            },
            ..CoordinatorConfig::default()
        },
    );
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("held"));

    let parked = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("parked", false)).await }
    });
    await_queued(&harness.backend, 1).await;

    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let interim = parked.await.expect("join").expect("spawn round-trips");
    assert!(
        interim.backgrounded,
        "queued caller must be handed off at the await budget: {interim:?}"
    );
    assert!(
        !interim.success,
        "interim handoff must not read as completed"
    );
    assert_eq!(
        harness.backend.registry_counts().await.queued,
        1,
        "the spawn itself stays queued for a slot"
    );
    assert_eq!(
        outstanding(&harness.backend, "prompt").await,
        SubagentOutstandingReply {
            live_ids: Vec::new(),
            background_live: true,
            subagent_usage_not_applied: false,
        }
    );

    let _ = harness.finish.send(());
    assert!(
        held.await
            .expect("join")
            .expect("spawn round-trips")
            .success
    );
    // The freed slot starts the parked spawn as a background child.
    assert_eq!(harness.started.recv().await.as_deref(), Some("parked"));
    let _ = harness.finish.send(());
    harness.actor.abort();
}

#[tokio::test]
async fn an_abandoned_queued_caller_stops_turn_blocking() {
    let mut harness = harness_with_config(false, limited(1, LimitBehavior::Queue));
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("held"));

    // No await budget: only the caller-gone reap can unblock the turn.
    let mut abandoned = request("abandoned", false);
    abandoned.await_to_completion = true;
    let spawn = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(abandoned).await }
    });
    await_queued(&harness.backend, 1).await;
    spawn.abort();
    let _ = spawn.await;

    assert_eq!(
        outstanding(&harness.backend, "prompt").await,
        SubagentOutstandingReply {
            live_ids: Vec::new(),
            background_live: true,
            subagent_usage_not_applied: false,
        }
    );

    let _ = harness.finish.send(());
    assert!(
        held.await
            .expect("join")
            .expect("spawn round-trips")
            .success
    );
    harness.actor.abort();
}

#[tokio::test(start_paused = true)]
async fn a_dequeued_spawn_keeps_spending_its_enqueue_await_budget() {
    let mut harness = harness_with_config(
        false,
        CoordinatorConfig {
            foreground_budget: std::time::Duration::from_secs(2),
            limits: SubagentLimits {
                max_concurrent: 1,
                behavior: LimitBehavior::Queue,
            },
            ..CoordinatorConfig::default()
        },
    );
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("held"));

    let parked = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("parked", false)).await }
    });
    await_queued(&harness.backend, 1).await;
    assert!(
        outstanding(&harness.backend, "prompt")
            .await
            .live_ids
            .contains(&"parked".to_owned()),
        "a queued foreground spawn blocks the turn until the budget handoff"
    );

    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let _ = harness.finish.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("parked"));

    let (id, queued_for, _) = harness.queue_waits.recv().await.expect("held ran");
    assert_eq!((id.as_str(), queued_for), ("held", None));
    let (id, queued_for, _) = harness.queue_waits.recv().await.expect("parked ran");
    assert_eq!(id, "parked");
    let queued_for = queued_for.expect("a dequeued spawn reports its time parked");
    assert!(
        queued_for >= std::time::Duration::from_secs(1)
            && queued_for < std::time::Duration::from_secs(2),
        "queued_for must measure the paused-clock park time, got {queued_for:?}"
    );

    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(
        parked.is_finished(),
        "the caller must be handed off at the enqueue-time deadline, \
         not a restarted budget"
    );
    let interim = parked.await.expect("join").expect("spawn round-trips");
    assert!(interim.backgrounded);
    assert!(
        held.await
            .expect("join")
            .expect("spawn round-trips")
            .success
    );
    let _ = harness.finish.send(());
    harness.actor.abort();
}

#[tokio::test(start_paused = true)]
async fn an_out_of_band_token_cancel_resolves_without_other_actor_traffic() {
    let mut harness = harness_with_config(false, limited(1, LimitBehavior::Queue));
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("held"));

    let mut queued = request("queued", true);
    let cancel = CancellationToken::new();
    queued.cancel_token = cancel.clone();
    let queued = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(queued).await }
    });
    await_queued(&harness.backend, 1).await;

    // Send nothing after the cancel: the periodic queue sweep must resolve
    // it alone.
    cancel.cancel();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(
        queued.is_finished(),
        "a token-cancelled queued spawn must resolve without other traffic"
    );
    let result = queued.await.expect("join").expect("spawn round-trips");
    assert!(result.cancelled);

    let _ = harness.finish.send(());
    assert!(
        held.await
            .expect("join")
            .expect("spawn round-trips")
            .success
    );
    harness.actor.abort();
}

#[tokio::test]
async fn a_cancel_command_by_id_resolves_a_queued_spawn() {
    let mut harness = harness_with_config(false, limited(1, LimitBehavior::Queue));
    let held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("held", true)).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("held"));

    let queued = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("queued", true)).await }
    });
    await_queued(&harness.backend, 1).await;

    let (respond_to, outcome) = oneshot::channel();
    harness
        .backend
        .sender()
        .send(SubagentEvent::Cancel(SubagentCancelRequest {
            parent_session_id: Some("parent".to_owned()),
            target: SubagentCancelTarget::SubagentId("queued".to_owned()),
            respond_to,
        }))
        .expect("actor command channel open");
    assert!(matches!(
        outcome.await.unwrap(),
        SubagentCancelOutcome::Cancelled
    ));
    let result = queued.await.expect("join").expect("spawn round-trips");
    assert!(
        result.cancelled,
        "cancel by id must resolve the spawn caller"
    );
    let snapshot = harness
        .backend
        .query("queued", false, None)
        .await
        .expect("cancelled queued id stays queryable");
    assert!(matches!(
        snapshot.status,
        SubagentSnapshotStatus::Cancelled { .. }
    ));

    let _ = harness.finish.send(());
    assert!(
        held.await
            .expect("join")
            .expect("spawn round-trips")
            .success
    );
    harness.actor.abort();
}

#[tokio::test]
async fn a_saturated_session_does_not_block_another_sessions_spawns() {
    let mut harness = harness_with_config(false, limited(1, LimitBehavior::Queue));
    let a_held = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("a-held", true)).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("a-held"));

    let a_queued = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("a-queued", true)).await }
    });
    await_queued(&harness.backend, 1).await;

    // Session B must start despite sitting behind session A's parked entry.
    let mut for_b = request("b-first", true);
    for_b.parent_session_id = "other".to_owned();
    let b_first = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(for_b).await }
    });
    assert_eq!(harness.started.recv().await.as_deref(), Some("b-first"));

    let _ = harness.finish.send(());
    assert_eq!(harness.started.recv().await.as_deref(), Some("a-queued"));
    let _ = harness.finish.send(());
    for spawn in [a_held, a_queued, b_first] {
        assert!(
            spawn
                .await
                .expect("join")
                .expect("spawn round-trips")
                .success
        );
    }
    harness.actor.abort();
}

#[tokio::test]
async fn workflow_spawns_bypass_the_session_concurrent_limit() {
    let mut harness = harness_with_config(false, limited(1, LimitBehavior::Fail));
    let task_child = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(request("task-child", true)).await }
    });
    harness.requests.recv().await.expect("task child started");

    let mut workflow = request("wf-child", true);
    workflow.owner = SubagentOwner::workflow("run-1");
    let workflow = tokio::spawn({
        let backend = harness.backend.clone();
        async move { backend.spawn(workflow).await }
    });
    harness
        .requests
        .recv()
        .await
        .expect("workflow child started with the session at the concurrent limit");

    let _ = harness.finish.send(());
    assert!(
        task_child
            .await
            .expect("join")
            .expect("spawn round-trips")
            .success
    );
    assert!(
        workflow
            .await
            .expect("join")
            .expect("spawn round-trips")
            .success
    );
    harness.actor.abort();
}
