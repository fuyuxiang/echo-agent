use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::types::{
    ActiveSubagentSummary, SubagentCompletionSummary, SubagentDescribeOutcome, SubagentInspection,
    SubagentRequest, SubagentResult, SubagentResumeLookup, SubagentSnapshot,
    SubagentSnapshotStatus, SubagentValidateTypeOutcome,
};

/// Cap on retained completed-subagent entries before the oldest are evicted.
pub const MAX_COMPLETED_ENTRIES: usize = 1024;
pub(super) const OUTPUT_UNAVAILABLE_PLACEHOLDER: &str = "[subagent output no longer available]";

pub type LocalBoxFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;
pub type SendBoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Runtime-specific live progress for one active child.
#[derive(Debug, Clone, Default)]
pub struct SubagentProgress {
    pub turn_count: u32,
    pub tool_call_count: u32,
    pub tokens_used: u64,
    pub context_window_tokens: u64,
    pub context_usage_pct: u8,
    pub tools_used: Vec<String>,
    pub error_count: u32,
}

/// Runtime handle retained while a child is active.
pub trait ChildControl: 'static {
    type ProgressFuture: Future<Output = SubagentProgress> + 'static;

    fn progress(&self) -> Self::ProgressFuture;
    fn cancel(&self);
}

/// Data reported when runtime initialization has produced a live child.
pub struct StartedChild<C> {
    pub child_session_id: String,
    pub persona: Option<String>,
    pub resumed_from: Option<String>,
    pub child_cwd: String,
    pub worktree_path: Option<String>,
    pub effective_model_id: String,
    /// The resolved agent definition declares `background: true`. Folded into
    /// `Outstanding` accounting (background, never turn-blocking) while the
    /// foreground await budget stays gated on the tool's own
    /// `run_in_background` flag.
    pub definition_background: bool,
    pub control: C,
}

/// Input to one runtime-specific child run.
pub struct ChildRunRequest<C> {
    pub request: SubagentRequest,
    pub cancellation: CancellationToken,
    pub reporter: ChildReporter<C>,
    /// Time parked in the admission queue; `None` if admitted immediately.
    pub queued_for: Option<std::time::Duration>,
    /// The session's running non-workflow children when this spawn started,
    /// including itself when it is one of them.
    pub session_running: usize,
}

/// Terminal output from one runtime-specific child run.
pub struct ChildRunOutput<D> {
    pub result: SubagentResult,
    pub completion_data: D,
    pub snapshot_ref: Option<String>,
}

/// Coordinator-owned delivery decision passed to host presentation.
#[derive(Debug, Clone)]
pub struct CompletionDisposition {
    pub foreground_delivered: bool,
    pub backgrounded: bool,
    pub waiter_delivered: bool,
    pub explicitly_killed: bool,
    pub should_surface: bool,
}

/// Terminal event delivered to the runtime adapter after state is committed.
pub struct ChildCompletion<D> {
    pub request: SubagentRequest,
    pub result: SubagentResult,
    pub completion_data: D,
    pub disposition: CompletionDisposition,
}

/// The only host-specific seam.
///
/// Associated future types intentionally carry no unconditional `Send` bound.
/// A local runner may return non-`Send` futures, while a multithreaded runner
/// may return `Send` futures.
pub trait ChildRunner: 'static {
    type Control: ChildControl;
    type CompletionData: Default + 'static;
    type RunFuture: Future<Output = ChildRunOutput<Self::CompletionData>> + 'static;
    type ValidateFuture: Future<Output = SubagentValidateTypeOutcome> + 'static;
    type DescribeFuture: Future<Output = SubagentDescribeOutcome> + 'static;

    fn run(&self, request: ChildRunRequest<Self::Control>) -> Self::RunFuture;

    fn validate_type(
        &self,
        subagent_type: String,
        parent_session_id: String,
    ) -> Self::ValidateFuture;

    fn describe_type(
        &self,
        subagent_type: String,
        harness_agent_type: Option<String>,
        parent_session_id: String,
    ) -> Self::DescribeFuture;

    fn on_completed(&self, completion: ChildCompletion<Self::CompletionData>);

    fn running_count_changed(&self, _running: usize) {}

    fn persisted_output_ref(&self, _completion_data: &Self::CompletionData) -> Option<String> {
        None
    }

    fn load_persisted_output(&self, _reference: &str) -> Option<Arc<str>> {
        None
    }
}

/// What an admission limit did to a spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentLimitDecision {
    QueuedAtConcurrentLimit { limit: usize },
    RejectedAtConcurrentLimit { limit: usize },
}

/// What asked for the spawn that hit a limit. Workflow-owned spawns bypass
/// admission, so a workflow arm never appears here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitedSpawnOrigin {
    Task,
    SchedulerLoop,
}

/// One admission decision at a session limit; workflow-owned spawns bypass
/// admission and never appear here.
#[derive(Debug, Clone)]
pub struct SubagentLimitNotice {
    pub parent_session_id: String,
    pub decision: SubagentLimitDecision,
    /// Session children running against the limit at decision time.
    pub running: usize,
    /// The session's queue depth after the decision applies: a queued spawn
    /// counts itself, a rejected spawn does not.
    pub queue_depth: usize,
    pub origin: LimitedSpawnOrigin,
}

/// Host-injected observer for admission decisions.
pub type SubagentLimitSink = Arc<dyn Fn(SubagentLimitNotice) + Send + Sync>;

/// Host-configurable lifecycle policy. The transition logic remains shared.
#[derive(Clone)]
pub struct CoordinatorConfig {
    pub foreground_budget: std::time::Duration,
    /// Session-scoped spawn limits enforced by the coordinator.
    pub limits: super::admission::SubagentLimits,
    /// Observer for spawns that hit an admission limit.
    pub limit_sink: Option<SubagentLimitSink>,
    /// Whether the host drains completion summaries between turns.
    pub buffer_completions: bool,
    /// Extra cap applied to BUFFERED summary outputs only (the request's own
    /// `completion_output_cap` still applies first). Buffered entries pin the
    /// child's output `Arc` until drained; hosts whose reminder rendering
    /// never inlines the output (a polling tool exists, e.g. the callback
    /// tools-server) should bound it. `None` keeps outputs verbatim — the
    /// shell needs this for toolsets with no polling tool, where the inline
    /// reminder is the model's only chance to see the output.
    pub buffered_completion_output_cap: Option<usize>,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            foreground_budget: std::time::Duration::from_secs(45),
            limits: super::admission::SubagentLimits::default(),
            limit_sink: None,
            buffer_completions: false,
            buffered_completion_output_cap: None,
        }
    }
}

impl std::fmt::Debug for CoordinatorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoordinatorConfig")
            .field("foreground_budget", &self.foreground_budget)
            .field("limits", &self.limits)
            .field("limit_sink", &self.limit_sink.is_some())
            .field("buffer_completions", &self.buffer_completions)
            .field(
                "buffered_completion_output_cap",
                &self.buffered_completion_output_cap,
            )
            .finish()
    }
}

/// Runner-side channel back into the actor.
pub struct ChildReporter<C> {
    pub(super) subagent_id: String,
    pub(super) tx: mpsc::UnboundedSender<InternalEvent<C>>,
}

impl<C> Clone for ChildReporter<C> {
    fn clone(&self) -> Self {
        Self {
            subagent_id: self.subagent_id.clone(),
            tx: self.tx.clone(),
        }
    }
}

impl<C: 'static> ChildReporter<C> {
    /// Promote the pending child to active. The acknowledgement closes the
    /// cancel-at-promote race: `false` means cancellation won and the adapter
    /// must tear down the half-initialized runtime.
    pub async fn started(&self, child: StartedChild<C>) -> bool {
        let (respond_to, response_rx) = oneshot::channel();
        if self
            .tx
            .send(InternalEvent::Started {
                subagent_id: self.subagent_id.clone(),
                child,
                respond_to,
            })
            .is_err()
        {
            return false;
        }
        response_rx.await.unwrap_or(false)
    }

    /// Resolve an in-memory resume source without sharing coordinator state.
    pub async fn resume_source(
        &self,
        source_id: &str,
        parent_session_id: &str,
    ) -> SubagentResumeLookup {
        let (respond_to, response_rx) = oneshot::channel();
        if self
            .tx
            .send(InternalEvent::ResumeSource {
                source_id: source_id.to_owned(),
                parent_session_id: parent_session_id.to_owned(),
                respond_to,
            })
            .is_err()
        {
            return SubagentResumeLookup::Missing;
        }
        response_rx.await.unwrap_or(SubagentResumeLookup::Missing)
    }
}

pub(super) enum InternalEvent<C> {
    Started {
        subagent_id: String,
        child: StartedChild<C>,
        respond_to: oneshot::Sender<bool>,
    },
    ResumeSource {
        source_id: String,
        parent_session_id: String,
        respond_to: oneshot::Sender<SubagentResumeLookup>,
    },
}

pub(super) struct PendingChild {
    pub(super) request: SubagentRequest,
    pub(super) started_at: std::time::Instant,
    pub(super) cancellation: CancellationToken,
    pub(super) spawn_reply: Option<oneshot::Sender<SubagentResult>>,
    pub(super) foreground_deadline: Option<tokio::time::Instant>,
    pub(super) handle_only: bool,
    pub(super) explicitly_killed: bool,
}

pub(super) struct ActiveChild<C> {
    pub(super) request: SubagentRequest,
    pub(super) started_at: std::time::Instant,
    pub(super) cancellation: CancellationToken,
    pub(super) spawn_reply: Option<oneshot::Sender<SubagentResult>>,
    pub(super) foreground_deadline: Option<tokio::time::Instant>,
    pub(super) handle_only: bool,
    /// Definition-declared background (see [`StartedChild`]): background for
    /// `Outstanding` accounting even while the spawn caller block-awaits.
    pub(super) definition_background: bool,
    pub(super) explicitly_killed: bool,
    pub(super) child_session_id: String,
    pub(super) persona: Option<String>,
    pub(super) resumed_from: Option<String>,
    pub(super) child_cwd: String,
    pub(super) worktree_path: Option<String>,
    pub(super) effective_model_id: String,
    pub(super) control: C,
}

pub(super) struct CompletedChild {
    pub(super) request: SubagentRequest,
    pub(super) started_at: std::time::Instant,
    pub(super) child_session_id: String,
    pub(super) persona: Option<String>,
    pub(super) resumed_from: Option<String>,
    pub(super) child_cwd: String,
    pub(super) worktree_path: Option<String>,
    pub(super) snapshot_ref: Option<String>,
    pub(super) persisted_output_ref: Option<String>,
    pub(super) effective_model_id: String,
    pub(super) result: SubagentResult,
}

pub(super) struct BlockingWaiter {
    pub(super) deadline: tokio::time::Instant,
    pub(super) respond_to: oneshot::Sender<Option<SubagentSnapshot>>,
}

pub(super) struct BufferedCompletion {
    pub(super) parent_session_id: String,
    pub(super) summary: SubagentCompletionSummary,
}

pub(super) struct TaggedFuture<F> {
    pub(super) subagent_id: String,
    pub(super) future: Pin<Box<F>>,
}

impl<F: Future> Future for TaggedFuture<F> {
    type Output = (String, F::Output);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.future
            .as_mut()
            .poll(cx)
            .map(|output| (this.subagent_id.clone(), output))
    }
}

pub(super) struct ReplyFuture<F, T> {
    pub(super) future: Pin<Box<F>>,
    pub(super) respond_to: Option<oneshot::Sender<T>>,
}

impl<F, T> Future for ReplyFuture<F, T>
where
    F: Future<Output = T>,
{
    type Output = (oneshot::Sender<T>, T);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.future.as_mut().poll(cx).map(|output| {
            let respond_to = match this.respond_to.take() {
                Some(respond_to) => respond_to,
                None => unreachable!("reply future polled after completion"),
            };
            (respond_to, output)
        })
    }
}

#[derive(Clone)]
pub(super) struct RunningSeed {
    pub(super) subagent_id: String,
    pub(super) description: String,
    pub(super) subagent_type: String,
    pub(super) started_at_epoch_ms: u64,
    pub(super) duration_ms: u64,
    pub(super) persona: Option<String>,
    pub(super) parent_session_id: String,
    pub(super) child_session_id: String,
    pub(super) fork_parent_prompt_id: Option<String>,
    pub(super) resumed_from: Option<String>,
}

pub(super) enum ProgressTarget {
    Query(oneshot::Sender<Option<SubagentSnapshot>>),
    Inspect(oneshot::Sender<Option<SubagentInspection>>),
    List { request_id: u64, index: usize },
}

pub(super) struct ProgressFuture<F> {
    pub(super) future: Pin<Box<F>>,
    pub(super) seed: Option<RunningSeed>,
    pub(super) target: Option<ProgressTarget>,
}

impl<F> Future for ProgressFuture<F>
where
    F: Future<Output = SubagentProgress>,
{
    type Output = (RunningSeed, ProgressTarget, SubagentProgress);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.future.as_mut().poll(cx).map(|progress| {
            let seed = match this.seed.take() {
                Some(seed) => seed,
                None => unreachable!("progress future polled without a seed"),
            };
            let target = match this.target.take() {
                Some(target) => target,
                None => unreachable!("progress future polled without a target"),
            };
            (seed, target, progress)
        })
    }
}

pub(super) struct ListRequest {
    pub(super) slots: Vec<Option<SubagentInspection>>,
    pub(super) remaining: usize,
    pub(super) respond_to: oneshot::Sender<Vec<SubagentInspection>>,
}

pub(super) enum ChildRecord<C> {
    Pending(PendingChild),
    Active(ActiveChild<C>),
}

impl<C> ChildRecord<C> {
    pub(super) fn request(&self) -> &SubagentRequest {
        match self {
            Self::Pending(child) => &child.request,
            Self::Active(child) => &child.request,
        }
    }

    pub(super) fn explicitly_killed(&self) -> bool {
        match self {
            Self::Pending(child) => child.explicitly_killed,
            Self::Active(child) => child.explicitly_killed,
        }
    }
}

pub(super) trait ForegroundChild {
    fn id(&self) -> &str;
    fn child_session_id(&self) -> &str;
    fn deadline(&self) -> Option<tokio::time::Instant>;
    /// True when the spawn caller dropped its result receiver while this
    /// child was still treated as turn-blocking (old shell `ParentGone`).
    fn caller_gone(&self) -> bool;
    fn is_workflow(&self) -> bool;
    fn take_reply(&mut self) -> Option<oneshot::Sender<SubagentResult>>;
    fn mark_backgrounded(&mut self);
    /// Cancel the child's execution (token + active control where present).
    fn cancel(&mut self);
}

impl ForegroundChild for PendingChild {
    fn id(&self) -> &str {
        &self.request.id
    }

    fn child_session_id(&self) -> &str {
        &self.request.id
    }

    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.foreground_deadline
    }

    fn caller_gone(&self) -> bool {
        !self.handle_only && self.spawn_reply.as_ref().is_some_and(|tx| tx.is_closed())
    }

    fn is_workflow(&self) -> bool {
        self.request.owner.is_workflow()
    }

    fn take_reply(&mut self) -> Option<oneshot::Sender<SubagentResult>> {
        self.spawn_reply.take()
    }

    fn mark_backgrounded(&mut self) {
        self.handle_only = true;
        self.foreground_deadline = None;
    }

    fn cancel(&mut self) {
        self.cancellation.cancel();
    }
}

impl<C: ChildControl> ForegroundChild for ActiveChild<C> {
    fn id(&self) -> &str {
        &self.request.id
    }

    fn child_session_id(&self) -> &str {
        &self.child_session_id
    }

    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.foreground_deadline
    }

    fn caller_gone(&self) -> bool {
        !self.handle_only && self.spawn_reply.as_ref().is_some_and(|tx| tx.is_closed())
    }

    fn is_workflow(&self) -> bool {
        self.request.owner.is_workflow()
    }

    fn take_reply(&mut self) -> Option<oneshot::Sender<SubagentResult>> {
        self.spawn_reply.take()
    }

    fn mark_backgrounded(&mut self) {
        self.handle_only = true;
        self.foreground_deadline = None;
    }

    fn cancel(&mut self) {
        self.cancellation.cancel();
        self.control.cancel();
    }
}

pub(super) fn background_at_deadline(
    child: &mut impl ForegroundChild,
    now: tokio::time::Instant,
    budget: std::time::Duration,
) {
    if child.deadline().is_none_or(|deadline| deadline > now) {
        return;
    }
    tracing::warn!(
        subagent_id = child.id(),
        budget_ms = budget.as_millis() as u64,
        "foreground subagent exceeded await budget; auto-backgrounding (child keeps running)",
    );
    if let Some(respond_to) = child.take_reply() {
        // Interim handoff, not a completion: keep `success: false` (default)
        // so `SubagentResult::status()` consumers cannot record a completed
        // status for a still-running child. Callers branch on `backgrounded`.
        let _ = respond_to.send(SubagentResult {
            backgrounded: true,
            subagent_id: child.id().to_owned(),
            child_session_id: child.child_session_id().to_owned(),
            ..Default::default()
        });
    }
    child.mark_backgrounded();
}

/// Handle a foreground child whose spawn caller dropped the result channel
/// (parent turn stop / cancelled await). Task-owned children keep running and
/// just leave the turn-blocking `Outstanding` set — shell `ParentGone` parity.
/// Workflow-owned children are CANCELLED instead (old shell `ParentGone`
/// cancelled workflow children); `ChannelBackend`'s drop-cancel arming remains
/// defense in depth for hosts that go through it.
pub(super) fn background_if_caller_gone(child: &mut impl ForegroundChild) {
    if !child.caller_gone() {
        return;
    }
    let _ = child.take_reply();
    if child.is_workflow() {
        tracing::debug!(
            subagent_id = child.id(),
            "workflow subagent caller gone; cancelling child",
        );
        child.cancel();
        return;
    }
    tracing::debug!(
        subagent_id = child.id(),
        "foreground subagent caller gone; auto-backgrounding (child keeps running)",
    );
    child.mark_backgrounded();
}

pub(super) async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn instant_to_epoch_ms(instant: std::time::Instant) -> u64 {
    let now_instant = std::time::Instant::now();
    let now_system = std::time::SystemTime::now();
    let elapsed = now_instant.saturating_duration_since(instant);
    now_system
        .checked_sub(elapsed)
        .unwrap_or(now_system)
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(super) fn active_summary<C>(child: &ActiveChild<C>) -> ActiveSubagentSummary {
    ActiveSubagentSummary {
        subagent_id: child.request.id.clone(),
        subagent_type: child.request.subagent_type.clone(),
        description: child.request.description.clone(),
        elapsed_ms: child.started_at.elapsed().as_millis() as u64,
    }
}

pub(super) fn running_seed<C>(child: &ActiveChild<C>) -> RunningSeed {
    RunningSeed {
        subagent_id: child.request.id.clone(),
        description: child.request.description.clone(),
        subagent_type: child.request.subagent_type.clone(),
        started_at_epoch_ms: instant_to_epoch_ms(child.started_at),
        duration_ms: child.started_at.elapsed().as_millis() as u64,
        persona: child.persona.clone(),
        parent_session_id: child.request.parent_session_id.clone(),
        child_session_id: child.child_session_id.clone(),
        fork_parent_prompt_id: child.request.parent_prompt_id.clone(),
        resumed_from: child.resumed_from.clone(),
    }
}

pub(super) fn running_inspection(
    seed: RunningSeed,
    progress: SubagentProgress,
) -> SubagentInspection {
    SubagentInspection {
        snapshot: SubagentSnapshot {
            subagent_id: seed.subagent_id,
            description: seed.description,
            subagent_type: seed.subagent_type,
            status: SubagentSnapshotStatus::Running {
                turn_count: progress.turn_count,
                tool_call_count: progress.tool_call_count,
                tokens_used: progress.tokens_used,
                context_window_tokens: progress.context_window_tokens,
                context_usage_pct: progress.context_usage_pct,
                tools_used: progress.tools_used,
                error_count: progress.error_count,
            },
            started_at_epoch_ms: seed.started_at_epoch_ms,
            duration_ms: seed.duration_ms,
            persona: seed.persona,
        },
        parent_session_id: seed.parent_session_id,
        child_session_id: seed.child_session_id,
        fork_parent_prompt_id: seed.fork_parent_prompt_id,
        resumed_from: seed.resumed_from,
    }
}

pub(super) fn pending_snapshot(child: &PendingChild) -> SubagentSnapshot {
    SubagentSnapshot {
        subagent_id: child.request.id.clone(),
        description: child.request.description.clone(),
        subagent_type: child.request.subagent_type.clone(),
        status: SubagentSnapshotStatus::Initializing,
        started_at_epoch_ms: instant_to_epoch_ms(child.started_at),
        duration_ms: child.started_at.elapsed().as_millis() as u64,
        persona: child.request.runtime_overrides.persona.clone(),
    }
}

pub(super) fn pending_inspection(child: &PendingChild) -> SubagentInspection {
    SubagentInspection {
        snapshot: pending_snapshot(child),
        parent_session_id: child.request.parent_session_id.clone(),
        child_session_id: String::new(),
        fork_parent_prompt_id: child.request.parent_prompt_id.clone(),
        resumed_from: child.request.resume_from.clone(),
    }
}

/// A spawn parked at the session's concurrent limit reads as initializing
/// rather than "not found".
pub(super) fn queued_snapshot(
    request: &SubagentRequest,
    queued_at: std::time::Instant,
) -> SubagentSnapshot {
    SubagentSnapshot {
        subagent_id: request.id.clone(),
        description: request.description.clone(),
        subagent_type: request.subagent_type.clone(),
        status: SubagentSnapshotStatus::Initializing,
        // Stable across polls: the enqueue time, with the duration showing
        // how long the spawn has waited for a slot.
        started_at_epoch_ms: instant_to_epoch_ms(queued_at),
        duration_ms: queued_at.elapsed().as_millis() as u64,
        persona: request.runtime_overrides.persona.clone(),
    }
}

pub(super) fn queued_inspection(
    request: &SubagentRequest,
    queued_at: std::time::Instant,
) -> SubagentInspection {
    SubagentInspection {
        snapshot: queued_snapshot(request, queued_at),
        parent_session_id: request.parent_session_id.clone(),
        child_session_id: String::new(),
        fork_parent_prompt_id: request.parent_prompt_id.clone(),
        resumed_from: request.resume_from.clone(),
    }
}

pub(super) fn completed_snapshot(
    child: &CompletedChild,
    persisted_output: Option<&str>,
) -> SubagentSnapshot {
    let status = if child.result.cancelled {
        SubagentSnapshotStatus::Cancelled {
            reason: child.result.error.clone(),
        }
    } else if child.result.success {
        SubagentSnapshotStatus::Completed {
            output: persisted_output
                .map(str::to_owned)
                .unwrap_or_else(|| child.result.output.to_string()),
            tool_calls: child.result.tool_calls,
            turns: child.result.turns,
            worktree_path: child.result.worktree_path.clone(),
        }
    } else {
        SubagentSnapshotStatus::Failed {
            error: child
                .result
                .error
                .clone()
                .unwrap_or_else(|| "Unknown error".to_owned()),
        }
    };
    SubagentSnapshot {
        subagent_id: child.request.id.clone(),
        description: child.request.description.clone(),
        subagent_type: child.request.subagent_type.clone(),
        status,
        started_at_epoch_ms: instant_to_epoch_ms(child.started_at),
        duration_ms: child.result.duration_ms,
        persona: child.persona.clone(),
    }
}

pub(super) fn completed_inspection(
    child: &CompletedChild,
    persisted_output: Option<&str>,
) -> SubagentInspection {
    SubagentInspection {
        snapshot: completed_snapshot(child, persisted_output),
        parent_session_id: child.request.parent_session_id.clone(),
        child_session_id: child.child_session_id.clone(),
        fork_parent_prompt_id: child.request.parent_prompt_id.clone(),
        resumed_from: child.resumed_from.clone(),
    }
}

/// Truncate `output` to `cap` bytes (UTF-8 safe) with a truncation footer.
/// Returns a refcount clone when already within the cap.
pub fn cap_completion_output(output: &Arc<str>, cap: usize) -> Arc<str> {
    if output.len() <= cap {
        return output.clone();
    }
    let mut end = cap;
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    Arc::from(format!(
        "{}\n[output truncated: {} of {} bytes shown]",
        &output[..end],
        end,
        output.len()
    ))
}

/// Model-facing summary for a finished child, honoring the request's
/// `completion_output_cap`. Shared by the coordinator's buffered reminder
/// path and the shell's auto-wake synthetic prompt.
pub fn completion_summary(
    request: &SubagentRequest,
    result: &SubagentResult,
) -> SubagentCompletionSummary {
    let output = match request.runtime_overrides.completion_output_cap {
        Some(cap) => cap_completion_output(&result.output, cap),
        None => result.output.clone(),
    };
    SubagentCompletionSummary {
        subagent_id: request.id.clone(),
        subagent_type: request.subagent_type.clone(),
        description: request.description.clone(),
        success: result.success && !result.cancelled,
        duration_ms: result.duration_ms,
        tool_calls: result.tool_calls,
        turns: result.turns,
        output,
    }
}

pub(super) fn workflow_outstanding<C>(
    pending: &HashMap<String, PendingChild>,
    active: &HashMap<String, ActiveChild<C>>,
    run_id: &str,
) -> usize {
    pending
        .values()
        .filter(|child| child.request.owner.workflow_run_id() == Some(run_id))
        .count()
        + active
            .values()
            .filter(|child| child.request.owner.workflow_run_id() == Some(run_id))
            .count()
}
