//! Parent↔subagent seam: every parent-side lifecycle call site, in order.
//!
//! 1. `MvpAgent::start_subagent_coordinator` (parent thread, in `mvp_agent`):
//!    hands the event receiver + concurrency limit to `spawn_subagent_coordinator`
//!    here, which drives the coordinator (living in `echo-agent-tools`).
//! 2. `ShellChildRunner::run` (parent thread): gathers what a child needs from
//!    the parent via `MvpAgent::try_build_subagent_spawn_context` (the
//!    parent→child snapshot, built by the owner in `mvp_agent`), then runs the
//!    spawn work on the worker pool (`worker_runtime()`, built on first use).
//! 3. `run_shell_child` (worker pool, in `handle_request.rs`): prepares the
//!    child (toolset, optional worktree, context) and starts it on its own
//!    thread via `spawn_session_on_thread`.
//! 4. `on_completed` → `present_child_completion` (worker pool): reports the
//!    child finished, persists the result, and optionally wakes the parent.
//!
//! Stage timings are recorded in `subagent_spawn::SubagentSpawnPhase`.
use super::ShellCompletionData;
use crate::agent::mvp_agent::{LocalRef, MvpAgent};
use crate::extensions::notification::{SessionNotification, SessionUpdate};
use crate::session::SessionCommand;
use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
pub(crate) use echo_agent_tools::implementations::echo_agent_build::task::coordinator::{
    self, ChildCompletion, ChildControl, ChildRunOutput, LocalBoxFuture, StartedChild,
    SubagentProgress,
};
use echo_agent_tools::implementations::echo_agent_build::task::types::{
    SubagentRequest, SubagentResult,
};
use tokio::sync::mpsc;
/// Floor keeps the pool responsive when `available_parallelism` is tiny.
const MIN_WORKER_THREADS: usize = 2;
/// Four suffice for 32 children (each runs on its own OS thread);
/// `ECHO_AGENT_SUBAGENT_WORKER_THREADS` overrides.
const MAX_WORKER_THREADS: usize = 4;
/// Pool for the per-child pipeline: a dedicated multi-thread runtime so
/// concurrent subagents run in parallel, off the user-facing session's
/// `LocalSet`.
pub(crate) fn worker_runtime() -> Result<&'static tokio::runtime::Handle, std::io::Error> {
    static WORKER: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    static INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    if let Some(runtime) = WORKER.get() {
        return Ok(runtime.handle());
    }
    let _guard = INIT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(runtime) = WORKER.get() {
        return Ok(runtime.handle());
    }
    let runtime = build_worker_runtime()?;
    Ok(WORKER.get_or_init(|| runtime).handle())
}
fn build_worker_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    let workers = std::env::var("ECHO_AGENT_SUBAGENT_WORKER_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(MAX_WORKER_THREADS)
                .clamp(MIN_WORKER_THREADS, MAX_WORKER_THREADS)
        });
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder
        .worker_threads(workers)
        .thread_name("subagent-worker");
    echo_agent_tty_utils::runtime::apply_blocking_pool(builder.enable_all()).build()
}
struct ShellChildRunner {
    agent_ref: LocalRef<MvpAgent>,
    /// Owned: panics are logged, coordinator teardown aborts stragglers.
    presentations: std::cell::RefCell<Vec<tokio_util::task::AbortOnDropHandle<()>>>,
}
/// Resumes worker panics into the coordinator's `catch_unwind`
/// (`finish_panicked_child`); the handle aborts on drop.
pub(crate) async fn join_worker_task<T>(task: tokio::task::JoinHandle<T>) -> T {
    let mut task = tokio_util::task::AbortOnDropHandle::new(task);
    match (&mut task).await {
        Ok(output) => output,
        Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
        Err(_) => unreachable!("worker runtime is never shut down"),
    }
}
impl coordinator::ChildRunner for ShellChildRunner {
    type Control = crate::agent::subagent::ShellChildRuntime;
    type CompletionData = crate::agent::subagent::ShellCompletionData;
    type RunFuture = coordinator::LocalBoxFuture<coordinator::ChildRunOutput<Self::CompletionData>>;
    type ValidateFuture = coordinator::LocalBoxFuture<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentValidateTypeOutcome,
    >;
    type DescribeFuture = coordinator::LocalBoxFuture<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentDescribeOutcome,
    >;
    fn run(&self, run: coordinator::ChildRunRequest<Self::Control>) -> Self::RunFuture {
        let agent_ref = self.agent_ref.clone();
        Box::pin(async move {
            let this = agent_ref.get();
            let parent_sid = run.request.parent_session_id.clone();
            let Some(mut ctx) = this.try_build_subagent_spawn_context(&parent_sid) else {
                tracing::warn!(
                    parent_session_id = %parent_sid,
                    subagent_id = %run.request.id,
                    "Spawn for unknown or evicted parent session"
                );
                return coordinator::ChildRunOutput {
                    result: echo_agent_tools::implementations::echo_agent_build::task::types::SubagentResult {
                        success: false,
                        error: Some(
                            "Parent session not found (evicted or torn down); cannot spawn subagent."
                                .to_owned(),
                        ),
                        subagent_id: run.request.id.clone(),
                        child_session_id: run.request.id,
                        ..Default::default()
                    },
                    completion_data: Default::default(),
                    snapshot_ref: None,
                };
            };
            let parent_handle = {
                let parent_sid = acp::SessionId::new(parent_sid);
                this.resident_handle(&parent_sid)
            };
            if let Some(handle) = parent_handle {
                let (pool, hooks, mut definitions, authority) = tokio::join!(
                    handle.snapshot_mcp_pool(),
                    handle.snapshot_client_hooks(),
                    handle.snapshot_tool_definitions(),
                    handle.snapshot_turn_authority()
                );
                let authority=match authority {
                    Ok(authority)=>authority,
                    Err(error)=>return coordinator::ChildRunOutput {
                        result: echo_agent_tools::implementations::echo_agent_build::task::types::SubagentResult {
                            success:false,error:Some(error),subagent_id:run.request.id.clone(),child_session_id:run.request.id,..Default::default()
                        },completion_data:Default::default(),snapshot_ref:None,
                    },
                };
                ctx.parent_turn_authority = Some(authority);
                ctx.parent_mcp_pool = pool;
                ctx.client_hooks = hooks;
                super::strip_ask_user_question_tool(&mut definitions);
                ctx.parent_tool_definitions = (!definitions.is_empty()).then_some(definitions);
            }
            let gateway = this.gateway.clone();
            let handle = match crate::agent::subagent::worker_runtime() {
                Ok(handle) => handle,
                Err(err) => {
                    tracing::error!(
                        subagent_id = %run.request.id,
                        error = %err,
                        "subagent worker runtime failed to build"
                    );
                    return coordinator::ChildRunOutput {
                        result: echo_agent_tools::implementations::echo_agent_build::task::types::SubagentResult {
                            success: false,
                            error: Some(
                                format!(
                                "Failed to start the subagent worker runtime: {err}"
                            ),
                            ),
                            subagent_id: run.request.id.clone(),
                            child_session_id: run.request.id,
                            ..Default::default()
                        },
                        completion_data: Default::default(),
                        snapshot_ref: None,
                    };
                }
            };
            join_worker_task(
                handle.spawn(crate::agent::subagent::run_shell_child(run, ctx, gateway)),
            )
            .await
        })
    }
    fn validate_type(
        &self,
        subagent_type: String,
        parent_session_id: String,
    ) -> Self::ValidateFuture {
        let agent_ref = self.agent_ref.clone();
        Box::pin(async move {
            let this = agent_ref.get();
            let ctx = this.build_subagent_validation_context(&parent_session_id);
            crate::agent::subagent::validate_subagent_type(&subagent_type, &ctx)
        })
    }
    fn describe_type(
        &self,
        subagent_type: String,
        harness_agent_type: Option<String>,
        parent_session_id: String,
    ) -> Self::DescribeFuture {
        let agent_ref = self.agent_ref.clone();
        Box::pin(async move {
            let this = agent_ref.get();
            match this.try_build_subagent_spawn_context(&parent_session_id) {
                Some(ctx) => crate::agent::subagent::describe_subagent_type(
                    &subagent_type,
                    harness_agent_type.as_deref(),
                    &ctx,
                ),
                None => {
                    tracing::warn!(
                        parent_session_id,
                        subagent_type,
                        "DescribeType for unknown/evicted parent session, replying Unavailable",
                    );
                    echo_agent_tools::implementations::echo_agent_build::task::types::SubagentDescribeOutcome::Unavailable
                }
            }
        })
    }
    fn on_completed(&self, completion: coordinator::ChildCompletion<Self::CompletionData>) {
        let gateway = self.agent_ref.get().gateway.clone();
        let will_wake = will_wake_for(&completion);
        let reservations = completion
            .completion_data
            .task_completion_reservations
            .clone();
        if will_wake && let Some(reservations) = &reservations {
            reservations.reserve(completion.request.id.clone());
        }
        let subagent_id = completion.request.id.clone();
        let present = move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                present_child_completion(completion, &gateway, will_wake)
            }))
            .is_err()
            {
                if will_wake && let Some(reservations) = &reservations {
                    reservations.release(&subagent_id);
                }
                tracing::error!(subagent_id, "subagent completion presentation panicked");
            }
        };
        match worker_runtime() {
            Ok(handle) => {
                let task = handle.spawn(async move { present() });
                let mut tasks = self.presentations.borrow_mut();
                tasks.retain(|t| !t.is_finished());
                tasks.push(tokio_util::task::AbortOnDropHandle::new(task));
            }
            Err(_) => present(),
        }
    }
    fn running_count_changed(&self, running: usize) {
        self.agent_ref
            .get()
            .activity
            .subagent_gauge()
            .store(running, std::sync::atomic::Ordering::Relaxed);
    }
    fn persisted_output_ref(&self, completion_data: &Self::CompletionData) -> Option<String> {
        completion_data
            .persisted_output_dir()
            .map(|path| path.to_string_lossy().into_owned())
    }
    fn load_persisted_output(&self, reference: &str) -> Option<std::sync::Arc<str>> {
        crate::agent::subagent::read_subagent_output(std::path::Path::new(reference))
            .map(std::sync::Arc::from)
    }
}
/// Coordinator limit sink; the coordinator cannot link telemetry directly.
fn log_limit_notice(notice: coordinator::SubagentLimitNotice) {
    use coordinator::{LimitedSpawnOrigin, SubagentLimitDecision};
    use echo_agent_telemetry::events::{
        SubagentLimitDisposition, SubagentLimitHit, SubagentOwnerKind,
    };
    let (disposition, limit) = match notice.decision {
        SubagentLimitDecision::QueuedAtConcurrentLimit { limit } => {
            (SubagentLimitDisposition::Queued, limit as u64)
        }
        SubagentLimitDecision::RejectedAtConcurrentLimit { limit } => {
            (SubagentLimitDisposition::Failed, limit as u64)
        }
    };
    echo_agent_telemetry::session_ctx::log_event(SubagentLimitHit::session_concurrent(
        notice.parent_session_id,
        disposition,
        limit,
        u32::try_from(notice.running).unwrap_or(u32::MAX),
        u32::try_from(notice.queue_depth).unwrap_or(u32::MAX),
        match notice.origin {
            LimitedSpawnOrigin::SchedulerLoop => SubagentOwnerKind::SchedulerLoop,
            LimitedSpawnOrigin::Task => SubagentOwnerKind::Task,
        },
    ));
}
/// Wire the shared subagent coordinator actor onto the current `LocalSet`:
/// build the `ShellChildRunner`, attach the limit sink, and `spawn_local` the
/// `SubagentCoordinator` draining `rx`. Coordinator/runner construction lives
/// here in the seam; `MvpAgent::start_subagent_coordinator` owns the parent
/// state (the event receiver + concurrency limits) it feeds in.
pub(crate) fn spawn_subagent_coordinator(
    agent_ref: LocalRef<MvpAgent>,
    rx: mpsc::UnboundedReceiver<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent,
    >,
    limits: echo_agent_tools::implementations::echo_agent_build::task::admission::SubagentLimits,
) {
    let runner = ShellChildRunner {
        agent_ref,
        presentations: Default::default(),
    };
    let limit_sink: coordinator::SubagentLimitSink = std::sync::Arc::new(log_limit_notice);
    let config = coordinator::CoordinatorConfig {
        foreground_budget:
            echo_agent_tools::implementations::echo_agent_build::task::backend::env_duration_or(
                "ECHO_AGENT_SUBAGENT_AWAIT_BUDGET_MS",
                std::time::Duration::from_secs(600),
            ),
        limits,
        limit_sink: Some(limit_sink),
        buffer_completions: true,
        buffered_completion_output_cap: None,
    };
    tokio::task::spawn_local(coordinator::SubagentCoordinator::new(rx, runner, config).run());
}
/// Whether this completion will inject an auto-wake prompt; decided (and
/// the reservation taken) on the coordinator thread in `on_completed`.
pub(crate) fn will_wake_for(completion: &ChildCompletion<ShellCompletionData>) -> bool {
    should_auto_wake_subagent(AutoWakeInputs::from_completion(completion))
        && completion.disposition.should_surface
}
pub(crate) fn present_child_completion(
    completion: ChildCompletion<ShellCompletionData>,
    gateway: &GatewaySender,
    will_wake: bool,
) {
    let ChildCompletion {
        request,
        result,
        completion_data,
        disposition: _,
    } = completion;
    if completion_data.spawned_notification_emitted || request.run_in_background {
        emit_subagent_notification(
            gateway,
            &request.parent_session_id,
            SessionUpdate::SubagentFinished {
                subagent_id: request.id.clone(),
                child_session_id: result.child_session_id.clone(),
                status: result.status().to_owned(),
                error: result.error.clone(),
                tool_calls: result.tool_calls,
                turns: result.turns,
                duration_ms: result.duration_ms,
                tokens_used: completion_data.telemetry_tokens,
                output: result.success.then(|| result.output.to_string()),
                will_wake,
            },
            completion_data.parent_cmd_tx.as_ref(),
        );
    }
    if will_wake {
        inject_subagent_completed_prompt(InjectParams {
            subagent_id: &request.id,
            result: &result,
            request: &request,
            task_completion_reservations: &completion_data.task_completion_reservations,
            parent_cmd_tx: completion_data.parent_cmd_tx.as_ref(),
            task_output_tool_name: &completion_data.task_output_tool_name,
            synthetic_trace_tx: &completion_data.synthetic_trace_tx,
            goal_loop_active: &completion_data.goal_loop_active,
        });
    }
}
/// Inputs to the auto-wake gate, one field per suppression reason.
#[derive(Clone, Copy)]
pub(crate) struct AutoWakeInputs {
    pub run_in_background: bool,
    pub cancelled: bool,
    pub auto_wake_enabled: bool,
    pub block_waited: bool,
    pub explicitly_killed: bool,
    pub goal_loop_active: bool,
    pub parent_channel_open: bool,
}
impl AutoWakeInputs {
    pub(crate) fn from_completion(completion: &ChildCompletion<ShellCompletionData>) -> Self {
        Self {
            run_in_background: completion.disposition.backgrounded,
            cancelled: completion.result.cancelled,
            auto_wake_enabled: completion.completion_data.auto_wake_enabled,
            block_waited: completion.disposition.waiter_delivered,
            explicitly_killed: completion.disposition.explicitly_killed,
            goal_loop_active: completion
                .completion_data
                .goal_loop_active
                .load(std::sync::atomic::Ordering::Relaxed),
            parent_channel_open: completion
                .completion_data
                .parent_cmd_tx
                .as_ref()
                .is_some_and(|tx| !tx.is_closed()),
        }
    }
}
/// Auto-wake gate. `parent_channel_open` folds the inject's no-channel bail
/// into the decision, so a stamped `will_wake` never promises a wake the
/// inject won't do. `cancelled` never wakes: the Ctrl+C race can background
/// a foreground child moments before its cancel lands, and waking would
/// prompt the model right after the user stopped everything.
pub(crate) fn should_auto_wake_subagent(inputs: AutoWakeInputs) -> bool {
    inputs.run_in_background
        && !inputs.cancelled
        && inputs.auto_wake_enabled
        && !inputs.block_waited
        && !inputs.explicitly_killed
        && !inputs.goal_loop_active
        && inputs.parent_channel_open
}
/// Inputs to [`inject_subagent_completed_prompt`], grouped so the call site
/// names each field (mirrors [`AutoWakeInputs`]).
pub(crate) struct InjectParams<'a> {
    pub subagent_id: &'a str,
    pub result: &'a SubagentResult,
    pub request: &'a SubagentRequest,
    pub task_completion_reservations:
        &'a Option<echo_agent_tools::reminders::task_completion::TaskCompletionReservations>,
    pub parent_cmd_tx: Option<&'a mpsc::UnboundedSender<SessionCommand>>,
    pub task_output_tool_name: &'a str,
    pub synthetic_trace_tx:
        &'a Option<mpsc::UnboundedSender<crate::upload::turn::SyntheticTurnTraceRequest>>,
    pub goal_loop_active: &'a std::sync::atomic::AtomicBool,
}
/// Inject the auto-wake synthetic prompt for a completed background subagent.
pub(crate) fn inject_subagent_completed_prompt(params: InjectParams) {
    let InjectParams {
        subagent_id,
        result,
        request,
        task_completion_reservations,
        parent_cmd_tx,
        task_output_tool_name,
        synthetic_trace_tx,
        goal_loop_active,
    } = params;
    if goal_loop_active.load(std::sync::atomic::Ordering::Relaxed) {
        if let Some(reservations) = task_completion_reservations {
            reservations.release(subagent_id);
        }
        return;
    }
    let Some(cmd_tx) = parent_cmd_tx else {
        if let Some(reservations) = task_completion_reservations {
            reservations.release(subagent_id);
        }
        return;
    };
    let summary = echo_agent_tools::implementations::echo_agent_build::task::completion_summary(
        request, result,
    );
    let message = echo_agent_tools::reminders::task_completion::format_subagent_completion(
        &summary,
        Some(task_output_tool_name),
    );
    let wrapped = echo_agent_tools::reminders::wrap_reminder(&message);
    let prompt_id = format!("subagent-completed-{subagent_id}");
    let before_rx = if synthetic_trace_tx.is_some() {
        let (before_tx, before_rx) = tokio::sync::oneshot::channel();
        let _ = cmd_tx.send(SessionCommand::CopyFile {
            respond_to: before_tx,
        });
        Some(before_rx)
    } else {
        None
    };
    let (respond_to, completion_rx) = tokio::sync::oneshot::channel();
    let prompt_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(wrapped))];
    if cmd_tx
        .send(SessionCommand::Prompt {
            prompt_id: prompt_id.clone(),
            prompt_blocks,
            prompt_mode: crate::session::plan_mode::PromptMode::Agent,
            artifact_upload_ctx: None,
            client_identifier: None,
            screen_mode: None,
            verbatim: true,
            traceparent: None,
            json_schema: None,
            send_now: false,
            admission: None,
            tool_overrides_update: None,
            respond_to,
            persist_ack: None,
            parsed_prompt_tx: None,
        })
        .is_err()
    {
        if let Some(reservations) = task_completion_reservations {
            reservations.release(subagent_id);
        }
        return;
    }
    if let Some(trace_tx) = synthetic_trace_tx {
        let _ = trace_tx.send(crate::upload::turn::SyntheticTurnTraceRequest {
            session_id: acp::SessionId::new(request.parent_session_id.clone()),
            prompt_id,
            completion_rx,
            before_session_copy_rx: before_rx
                .expect("before_rx set when synthetic_trace_tx is Some"),
        });
    }
}
pub(crate) fn emit_subagent_notification(
    gateway: &GatewaySender,
    parent_session_id: &str,
    update: SessionUpdate,
    parent_cmd_tx: Option<&mpsc::UnboundedSender<SessionCommand>>,
) {
    let mut meta = None;
    crate::util::event_id::ensure_event_id_meta(parent_session_id, &mut meta);
    let notification = SessionNotification {
        session_id: acp::SessionId::new(parent_session_id),
        update,
        meta: meta.map(serde_json::Value::Object),
    };
    if let Some(cmd_tx) = parent_cmd_tx {
        let _ = cmd_tx.send(SessionCommand::EchoAgentSessionNotification {
            notification: notification.clone(),
        });
    }
    let params = serde_json::to_value(&notification)
        .and_then(|v| serde_json::value::to_raw_value(&v))
        .ok();
    if let Some(params) = params {
        let ext_notification =
            acp::ExtNotification::new("echo.agent/session_notification", params.into());
        gateway.forward_fire_and_forget(ext_notification);
    }
}
