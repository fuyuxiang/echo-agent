//! Spawn admission: reparenting, duplicate checks, and the admit decision.

use tokio::sync::oneshot;

use super::super::admission::{AdmissionDecision, AdmissionError};
use super::super::coordinator_state::PendingChild;
use super::super::types::{SubagentOwner, SubagentRequest, SubagentResult, SubagentSpawnRequest};
use super::queue::{QueuedCaller, QueuedSpawn, StartOrigin};
use super::{
    ChildRunOutput, ChildRunner, LimitedSpawnOrigin, SubagentCoordinator, SubagentLimitDecision,
    SubagentLimitNotice,
};

impl<R: ChildRunner> SubagentCoordinator<R> {
    pub(super) fn handle_spawn(&mut self, command: SubagentSpawnRequest) {
        let SubagentSpawnRequest {
            mut request,
            result_tx,
        } = command;
        if let Err(rejection) = self.reparent_nested_spawn(&mut request) {
            let _ = result_tx.send(rejection);
            return;
        }
        // Late Task spawn after user Stop (detached TaskTool background).
        if !request.owner.is_workflow()
            && self
                .spawn_blocked_sessions
                .contains(&request.parent_session_id)
        {
            let _ = result_tx.send(rejected_spawn_result(
                &request.id,
                "parent session is stopped",
                true,
            ));
            return;
        }
        let id = request.id.clone();
        if self.pending.contains_key(&id)
            || self.active.contains_key(&id)
            || self.completed.contains_key(&id)
            || self.queued.contains_id(&id)
        {
            let _ = result_tx.send(rejected_spawn_result(
                &id,
                &format!("Subagent id '{id}' already exists"),
                false,
            ));
            return;
        }
        let running = self.session_running_count(&request.parent_session_id);
        match self.admission.admit(&request, running) {
            AdmissionDecision::Start => {
                self.start_child(*request, Some(result_tx), StartOrigin::Direct)
            }
            AdmissionDecision::Enqueue => {
                debug_assert!(
                    !request.owner.is_workflow(),
                    "workflow spawns bypass admission and must never queue"
                );
                tracing::info!(
                    subagent_id = %request.id,
                    parent_session_id = %request.parent_session_id,
                    running,
                    "subagent queued at the concurrent limit"
                );
                self.notify_limit(
                    &request,
                    SubagentLimitDecision::QueuedAtConcurrentLimit {
                        limit: self.admission.max_concurrent(),
                    },
                );
                let deadline = request
                    .awaits_in_foreground()
                    .then(|| tokio::time::Instant::now() + self.config.foreground_budget);
                self.queued.push_back(QueuedSpawn {
                    request,
                    queued_at: tokio::time::Instant::now(),
                    caller: QueuedCaller::Awaiting {
                        result_tx,
                        deadline,
                    },
                });
            }
            AdmissionDecision::Reject(error) => {
                self.notify_limit(
                    &request,
                    match &error {
                        AdmissionError::ConcurrentLimitReached { limit } => {
                            SubagentLimitDecision::RejectedAtConcurrentLimit { limit: *limit }
                        }
                    },
                );
                let result = SubagentResult {
                    success: false,
                    error: Some(error.message()),
                    subagent_id: id.clone(),
                    child_session_id: id,
                    ..Default::default()
                };
                self.finish_never_started(
                    *request,
                    Some(result_tx),
                    result,
                    std::time::Instant::now(),
                );
            }
        }
    }

    /// Re-key a nested spawn (its parent is itself a subagent) to the root
    /// session, inheriting workflow lineage and loop identity; rejects the
    /// spawn when its parent subagent is already being torn down.
    fn reparent_nested_spawn(&self, request: &mut SubagentRequest) -> Result<(), SubagentResult> {
        let Some((root_parent, loop_task_id, spawner_cancelled, spawner_owner)) = self
            .active
            .values()
            .find(|child| child.child_session_id == request.parent_session_id)
            .map(|child| {
                (
                    child.request.parent_session_id.clone(),
                    child.request.runtime_overrides.loop_task_id.clone(),
                    child.cancellation.is_cancelled(),
                    child.request.owner.clone(),
                )
            })
        else {
            return Ok(());
        };
        if spawner_cancelled {
            // The parent subagent is being torn down, so its late child
            // would be orphaned against the closed scope.
            return Err(rejected_spawn_result(
                &request.id,
                "parent subagent is being torn down",
                true,
            ));
        }
        request.parent_session_id = root_parent;
        request.surface_completion = false;
        // Nested children keep workflow lineage after reparent so
        // ParentSession Stop does not kill in-flight workflow work.
        if !request.owner.is_workflow()
            && let Some(run_id) = spawner_owner.workflow_run_id()
        {
            request.owner = SubagentOwner::workflow(run_id);
        }
        if request.runtime_overrides.loop_task_id.is_none() {
            request.runtime_overrides.loop_task_id = loop_task_id;
        }
        Ok(())
    }

    /// Counts are computed here, not at call sites: a queued spawn counts
    /// itself in `queue_depth` (the notice fires before the push), a rejected
    /// spawn does not.
    fn notify_limit(&self, request: &SubagentRequest, decision: SubagentLimitDecision) {
        let Some(sink) = &self.config.limit_sink else {
            return;
        };
        let queued = self.session_queued_count(&request.parent_session_id);
        sink(SubagentLimitNotice {
            parent_session_id: request.parent_session_id.clone(),
            decision,
            running: self.session_running_count(&request.parent_session_id),
            queue_depth: match decision {
                SubagentLimitDecision::QueuedAtConcurrentLimit { .. } => queued + 1,
                SubagentLimitDecision::RejectedAtConcurrentLimit { .. } => queued,
            },
            origin: if request.from_scheduler_loop() {
                LimitedSpawnOrigin::SchedulerLoop
            } else {
                LimitedSpawnOrigin::Task
            },
        });
    }

    /// Route a spawn that never reached the runner through `finish_child`,
    /// so waiters resolve and the id stays queryable; `since` anchors the
    /// record's duration.
    pub(super) fn finish_never_started(
        &mut self,
        request: SubagentRequest,
        spawn_reply: Option<oneshot::Sender<SubagentResult>>,
        result: SubagentResult,
        since: std::time::Instant,
    ) {
        let id = request.id.clone();
        self.pending.insert(
            id.clone(),
            PendingChild {
                started_at: since,
                cancellation: request.cancel_token.clone(),
                spawn_reply,
                foreground_deadline: None,
                handle_only: request.run_in_background,
                explicitly_killed: false,
                request,
            },
        );
        self.finish_child(
            &id,
            ChildRunOutput {
                result,
                completion_data: R::CompletionData::default(),
                snapshot_ref: None,
            },
        );
    }
}

/// A spawn refused before it ever became a child record.
fn rejected_spawn_result(id: &str, error: &str, cancelled: bool) -> SubagentResult {
    SubagentResult {
        success: false,
        cancelled,
        error: Some(error.to_owned()),
        subagent_id: id.to_owned(),
        child_session_id: id.to_owned(),
        ..Default::default()
    }
}
