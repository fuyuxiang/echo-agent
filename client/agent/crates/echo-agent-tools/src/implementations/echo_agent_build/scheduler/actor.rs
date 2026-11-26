use std::collections::HashSet;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::implementations::echo_agent_build::task::types::{
    SessionIdResource, SubagentEvent, SubagentEventSender, SubagentLoopUnitActiveRequest,
    SubagentOwner, SubagentQueryRequest, SubagentRequest, SubagentRuntimeOverrides,
    SubagentSnapshotStatus, SubagentSpawnRequest,
};
use crate::notification::types::ToolNotificationHandle;
use crate::notification::{
    DurableNotificationTargets, ScheduledTaskCreated, ScheduledTaskFired, ScheduledTaskRemoved,
    ScheduledTaskRemovedReason,
};
use crate::reminders::format_loop_iteration_prompt;
use crate::types::resources::{SharedResources, State};

use super::interval::interval_to_human;
use super::types::{
    LOOP_COMPLETION_OUTPUT_CAP, LOOP_FRESH_CHAIN_EVERY, ScheduledTask, SchedulerClock,
    SchedulerCommand, SchedulerError, SchedulerSnapshot, SchedulerState, SchedulerVersion,
};

const MAX_SCHEDULED_TASKS: usize = 50;
const DURABILITY_BARRIER_TIMEOUT: Duration = Duration::from_secs(30);

enum LoopFireOutcome {
    Spawned(String),
    Foreground,
    Skipped,
}

enum ExpiryPersistenceOutcome {
    Committed,
    NotCommitted(std::io::Error),
    Unknown(SchedulerError),
}

pub(crate) struct PendingDurableRemoval {
    task_id: String,
    reservation: super::types::SchedulerReservation,
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}\u{2026}")
}

fn task_created_payload(task: &ScheduledTask, version: SchedulerVersion) -> ScheduledTaskCreated {
    ScheduledTaskCreated {
        task_id: task.id.clone(),
        prompt: task.prompt.clone(),
        human_schedule: interval_to_human(task.interval_secs),
        next_fire_at: Some(task.next_fire_at().to_rfc3339()),
        generation: version.generation(),
        revision: version.revision(),
    }
}

fn task_removed_payload(
    task_id: String,
    reason: ScheduledTaskRemovedReason,
    version: SchedulerVersion,
) -> ScheduledTaskRemoved {
    ScheduledTaskRemoved {
        task_id,
        reason,
        generation: version.generation(),
        revision: version.revision(),
    }
}

fn log_rollover(
    transition: &'static str,
    task_id: Option<&str>,
    rollover: Option<super::types::GenerationRollover>,
) {
    let Some(rollover) = rollover else {
        return;
    };
    if let Some(task_id) = task_id {
        tracing::error!(
            transition,
            task_id,
            old_generation = %rollover.old_generation,
            new_generation = %rollover.new_generation,
            "Scheduler generation rotated after revision exhaustion"
        );
    } else {
        tracing::error!(
            transition,
            old_generation = %rollover.old_generation,
            new_generation = %rollover.new_generation,
            "Scheduler generation rotated after revision exhaustion"
        );
    }
}

pub struct SchedulerActor {
    pub(crate) resources: SharedResources,
    pub(crate) resources_persistence: std::sync::Arc<crate::persistence::ResourcesPersistence>,
    pub(crate) notification_handle: ToolNotificationHandle,
    pub(crate) cmd_rx: mpsc::UnboundedReceiver<SchedulerCommand>,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) clock: SchedulerClock,
    pub(crate) pending_removal: Option<PendingDurableRemoval>,
    pub(crate) blocked_expiries: HashSet<String>,
}

impl SchedulerActor {
    async fn await_bounded<T, E>(
        &self,
        future: impl std::future::Future<Output = Result<T, E>>,
        map_error: impl FnOnce(E) -> SchedulerError,
    ) -> Result<T, SchedulerError> {
        tokio::select! {
            _ = self.cancel_token.cancelled() => Err(SchedulerError::Cancelled),
            result = tokio::time::timeout(DURABILITY_BARRIER_TIMEOUT, future) => {
                result.map_err(|_| SchedulerError::Timeout)?.map_err(map_error)
            }
        }
    }

    async fn persist_resources(&self) -> Result<(), SchedulerError> {
        let acknowledgement = {
            let resources = self.resources.lock().await;
            self.resources_persistence
                .enqueue_save_and_flush(resources.serialize())
                .map_err(SchedulerError::Persistence)?
        };
        self.await_bounded(
            crate::persistence::ResourcesPersistence::await_save_and_flush(acknowledgement),
            SchedulerError::Persistence,
        )
        .await
    }

    async fn publish_durable_removal(
        &self,
        task_id: String,
        reason: ScheduledTaskRemovedReason,
        version: SchedulerVersion,
    ) -> Result<(), SchedulerError> {
        let acknowledgements = self
            .notification_handle
            .send_scheduled_task_removed_acknowledged(task_removed_payload(
                task_id, reason, version,
            ));
        self.await_bounded(acknowledgements.wait(), SchedulerError::Notification)
            .await
    }

    async fn complete_pending_removal(&mut self) -> Result<bool, SchedulerError> {
        if self.notification_handle.durable_targets() == DurableNotificationTargets::None {
            // Targets are immutable, so retaining the reservation would wedge all later commands.
            let task_id = self
                .pending_removal
                .take()
                .expect("pending durable removal exists")
                .task_id;
            tracing::error!(%task_id, "Durable scheduler removal unavailable");
            return Err(SchedulerError::NoDurableNotificationConsumer);
        }

        self.persist_resources().await?;

        let (task_id, version) = {
            let pending = self
                .pending_removal
                .as_ref()
                .expect("pending durable removal exists");
            (pending.task_id.clone(), pending.reservation.version_at(0))
        };
        self.publish_durable_removal(task_id, ScheduledTaskRemovedReason::Deleted, version)
            .await?;
        let mut pending = self
            .pending_removal
            .take()
            .expect("pending durable removal exists");
        let commit = pending.reservation.commit_next(&mut self.clock);
        log_rollover("durable_removal", Some(&pending.task_id), commit.rollover);
        Ok(true)
    }

    pub async fn run(mut self) {
        self.await_subagent_wiring_for_due_tasks().await;
        self.announce_existing_tasks().await;

        loop {
            let next_fire = self.compute_next_fire_delay().await;

            tokio::select! {
                biased;

                _ = self.cancel_token.cancelled() => {
                    tracing::debug!("SchedulerActor shutting down (cancelled)");
                    break;
                }

                Some(cmd) = self.cmd_rx.recv() => {
                    self.handle_command(cmd).await;
                }

                _ = tokio::time::sleep(next_fire), if self.pending_removal.is_none() => {
                    self.fire_next_task().await;
                }
            }
        }

        let task_ids: Vec<String> = {
            let res = self.resources.lock().await;
            res.get::<State<SchedulerState>>()
                .map(|state| state.tasks.iter().map(|task| task.id.clone()).collect())
                .unwrap_or_default()
        };
        if task_ids.is_empty() {
            return;
        }
        let mut reservation = self.clock.prepare_transition(task_ids.len());
        for task_id in task_ids {
            let commit = reservation.commit_next(&mut self.clock);
            log_rollover("shutdown", None, commit.rollover);
            self.notification_handle
                .send_scheduled_task_removed(task_removed_payload(
                    task_id,
                    ScheduledTaskRemovedReason::Shutdown,
                    commit.version,
                ));
        }
    }

    async fn await_subagent_wiring_for_due_tasks(&self) {
        const STARTUP_WIRING_GRACE: Duration = Duration::from_secs(10);
        const POLL_INTERVAL: Duration = Duration::from_millis(250);
        let deadline = tokio::time::Instant::now() + STARTUP_WIRING_GRACE;
        loop {
            let (needs_wiring, wired) = {
                let res = self.resources.lock().await;
                let enabled = res
                    .get::<crate::types::resources::SchedulerBackgroundLoops>()
                    .is_none_or(|v| v.0);
                let soon = Utc::now() + chrono::Duration::seconds(2);
                let needs_wiring = enabled
                    && res.get::<State<SchedulerState>>().is_some_and(|s| {
                        s.tasks
                            .iter()
                            .any(|t| t.recurring && !t.foreground && t.next_wake_at() <= soon)
                    });
                let wired = res.get::<SubagentEventSender>().is_some()
                    && res.get::<SessionIdResource>().is_some();
                (needs_wiring, wired)
            };
            if !needs_wiring || wired || tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::select! {
                _ = self.cancel_token.cancelled() => return,
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    }

    async fn compute_next_fire_delay(&self) -> Duration {
        let res = self.resources.lock().await;
        let scheduler_state = res.get::<State<SchedulerState>>();
        scheduler_state
            .map(|s| {
                s.tasks
                    .iter()
                    .filter(|task| !self.blocked_expiries.contains(&task.id))
                    .map(ScheduledTask::next_wake_at)
                    .min()
                    .map(|next| {
                        let now = Utc::now();
                        if next <= now {
                            Duration::ZERO
                        } else {
                            (next - now).to_std().unwrap_or(Duration::ZERO)
                        }
                    })
                    .unwrap_or(Duration::MAX)
            })
            .unwrap_or(Duration::MAX)
    }

    async fn fire_next_task(&mut self) {
        debug_assert!(self.pending_removal.is_none());
        let now = Utc::now();
        let mut res = self.resources.lock().await;
        let state = res.get_or_default::<State<SchedulerState>>();
        let idx = state.tasks.iter().position(|task| {
            task.next_wake_at() <= now && !self.blocked_expiries.contains(&task.id)
        });

        let Some(idx) = idx else {
            return;
        };

        let task = &state.tasks[idx];
        let task_id = task.id.clone();
        let is_expired = task.recurring && task.is_expired(now);
        let should_remove = !task.recurring;
        let prompt = task.prompt.clone();
        let human_schedule = interval_to_human(task.interval_secs);
        let is_durable = task.durable;
        let foreground = task.foreground;
        let last_subagent_id = task.last_subagent_id.clone();
        let iterations_since_fresh = task.iterations_since_fresh;
        let chain_reset_pending = task.chain_reset_pending;
        let transition_count = if is_expired {
            1
        } else if should_remove {
            2
        } else {
            1
        };
        let transition = if is_expired { "expiry" } else { "fire" };

        if is_expired && is_durable {
            if self.notification_handle.durable_targets() == DurableNotificationTargets::None {
                tracing::error!(%task_id, "Durable scheduler expiry unavailable");
                self.blocked_expiries.insert(task_id);
                return;
            }
            let mut reservation = self.clock.prepare_transition(1);
            let expired_task = state.tasks.remove(idx);
            let acknowledgement = self
                .resources_persistence
                .enqueue_save_and_flush(res.serialize());
            drop(res);
            tracing::info!(task_id = %task_id, "Scheduled task expired; removing without firing");

            let persistence = match acknowledgement {
                Ok(acknowledgement) => {
                    let deadline = tokio::time::Instant::now() + DURABILITY_BARRIER_TIMEOUT;
                    tokio::select! {
                        _ = self.cancel_token.cancelled() => {
                            ExpiryPersistenceOutcome::Unknown(SchedulerError::Cancelled)
                        }
                        result = tokio::time::timeout_at(deadline, acknowledgement) => {
                            match result {
                                Ok(Ok(Ok(()))) => ExpiryPersistenceOutcome::Committed,
                                Ok(Ok(Err(error))) => {
                                    ExpiryPersistenceOutcome::NotCommitted(error)
                                }
                                Ok(Err(_)) => ExpiryPersistenceOutcome::Unknown(
                                    SchedulerError::Persistence(std::io::Error::new(
                                        std::io::ErrorKind::BrokenPipe,
                                        "resources persistence writer dropped acknowledgement",
                                    )),
                                ),
                                Err(_) => {
                                    ExpiryPersistenceOutcome::Unknown(SchedulerError::Timeout)
                                }
                            }
                        }
                    }
                }
                Err(error) => ExpiryPersistenceOutcome::NotCommitted(error),
            };
            match persistence {
                ExpiryPersistenceOutcome::Committed => {}
                ExpiryPersistenceOutcome::NotCommitted(error) => {
                    tracing::warn!(
                        %task_id,
                        %error,
                        "Durable scheduler expiry was not persisted"
                    );
                    let mut resources = self.resources.lock().await;
                    resources
                        .get_or_default::<State<SchedulerState>>()
                        .tasks
                        .insert(idx, expired_task);
                    self.blocked_expiries.insert(task_id);
                    return;
                }
                ExpiryPersistenceOutcome::Unknown(error) => {
                    tracing::warn!(
                        %task_id,
                        %error,
                        "Durable scheduler expiry persistence outcome is unknown"
                    );
                    return;
                }
            }

            let version = reservation.version_at(0);
            if let Err(error) = self
                .publish_durable_removal(
                    task_id.clone(),
                    ScheduledTaskRemovedReason::Expired,
                    version,
                )
                .await
            {
                tracing::warn!(
                    %task_id,
                    %error,
                    "Failed to acknowledge durable scheduler expiry"
                );
                return;
            }
            let commit = reservation.commit_next(&mut self.clock);
            log_rollover(transition, Some(&task_id), commit.rollover);
            return;
        }

        let mut reservation = self.clock.prepare_transition(transition_count);

        if is_expired {
            state.tasks.remove(idx);
            drop(res);
            // Flush the absence before announcing, so a crash in the debounce window cannot
            // resurrect and re-expire the task on restart. Best-effort (non-durable semantics):
            // on failure the removal proceeds with a warn. The re-lock is race-free; only this
            // actor task mutates scheduler state.
            if let Err(error) = self.persist_resources().await {
                tracing::warn!(
                    task_id = %task_id,
                    %error,
                    "Expired task's absence was not persisted; a restart may resurrect and re-expire it"
                );
            }
            tracing::info!(task_id = %task_id, "Scheduled task expired; removing without firing");
            let commit = reservation.commit_next(&mut self.clock);
            log_rollover(transition, Some(&task_id), commit.rollover);
            self.notification_handle
                .send_scheduled_task_removed(task_removed_payload(
                    task_id,
                    ScheduledTaskRemovedReason::Expired,
                    commit.version,
                ));
            return;
        }

        // Advance the cadence before releasing actor state to avoid immediate reselection.
        let task = &mut state.tasks[idx];
        task.last_fired_at = Some(now);
        let next_fire_at = task.recurring.then(|| task.next_fire_at().to_rfc3339());
        if should_remove {
            state.tasks.remove(idx);
        }

        let background_enabled = res
            .get::<crate::types::resources::SchedulerBackgroundLoops>()
            .is_none_or(|v| v.0);
        let spawn_deps = if foreground || should_remove || !background_enabled {
            None
        } else {
            let events = res.get::<SubagentEventSender>().cloned();
            let session = res.get::<SessionIdResource>().map(|s| s.0.clone());
            events.zip(session)
        };

        drop(res);

        tracing::info!(
            task_id = %task_id,
            schedule = %human_schedule,
            background = spawn_deps.is_some(),
            "Firing scheduled task"
        );

        let removed_task_id = should_remove.then(|| task_id.clone());

        let outcome = match spawn_deps {
            Some((events, parent_session_id)) => {
                self.fire_as_loop_subagent(
                    &events,
                    parent_session_id,
                    &task_id,
                    &prompt,
                    &human_schedule,
                    last_subagent_id,
                    iterations_since_fresh,
                    chain_reset_pending,
                )
                .await
            }
            None => LoopFireOutcome::Foreground,
        };

        if matches!(&outcome, LoopFireOutcome::Skipped) {
            let version = self.clock.snapshot();
            let payload = {
                let res = self.resources.lock().await;
                res.get::<State<SchedulerState>>()
                    .and_then(|state| state.tasks.iter().find(|task| task.id == task_id))
                    .map(|task| task_created_payload(task, version))
            };
            if let Some(payload) = payload {
                self.notification_handle
                    .send_scheduled_task_created(payload);
            }
            return;
        }

        match outcome {
            LoopFireOutcome::Skipped => unreachable!("skipped outcome returned above"),
            LoopFireOutcome::Foreground => {
                let commit = reservation.commit_next(&mut self.clock);
                log_rollover(transition, Some(&task_id), commit.rollover);
                let fire_version = commit.version;
                self.notification_handle
                    .send_scheduled_task_fired(ScheduledTaskFired {
                        task_id,
                        prompt,
                        human_schedule,
                        next_fire_at,
                        subagent_id: None,
                        generation: fire_version.generation(),
                        revision: fire_version.revision(),
                    });
            }
            LoopFireOutcome::Spawned(id) => {
                let commit = reservation.commit_next(&mut self.clock);
                log_rollover(transition, Some(&task_id), commit.rollover);
                let fire_version = commit.version;
                self.notification_handle
                    .send_scheduled_task_fired(ScheduledTaskFired {
                        task_id,
                        prompt,
                        human_schedule,
                        next_fire_at,
                        subagent_id: Some(id),
                        generation: fire_version.generation(),
                        revision: fire_version.revision(),
                    });
            }
        }

        if let Some(task_id) = removed_task_id {
            let removal = reservation.commit_next(&mut self.clock);
            debug_assert!(removal.rollover.is_none());
            self.notification_handle
                .send_scheduled_task_removed(task_removed_payload(
                    task_id,
                    ScheduledTaskRemovedReason::Completed,
                    removal.version,
                ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn fire_as_loop_subagent(
        &self,
        events: &SubagentEventSender,
        parent_session_id: String,
        task_id: &str,
        prompt: &str,
        human_schedule: &str,
        last_subagent_id: Option<String>,
        iterations_since_fresh: u32,
        chain_reset_pending: bool,
    ) -> LoopFireOutcome {
        let prev_snapshot = match &last_subagent_id {
            Some(prev_id) => {
                let (respond_to, rx) = tokio::sync::oneshot::channel();
                let query_sent = events
                    .0
                    .send(SubagentEvent::Query(SubagentQueryRequest {
                        subagent_id: prev_id.clone(),
                        parent_session_id: Some(parent_session_id.clone()),
                        block: false,
                        timeout_ms: None,
                        respond_to,
                    }))
                    .is_ok();
                if query_sent {
                    tokio::select! {
                        biased;
                        _ = self.cancel_token.cancelled() => return LoopFireOutcome::Skipped,
                        snapshot = rx => match snapshot {
                            Ok(s) => s,
                            Err(_) => return LoopFireOutcome::Skipped,
                        },
                        _ = tokio::time::sleep(Duration::from_secs(10)) => {
                            tracing::warn!(
                                task_id = %task_id,
                                previous_subagent = %prev_id,
                                "Loop in-flight query timed out; skipping this fire"
                            );
                            return LoopFireOutcome::Skipped;
                        }
                    }
                } else {
                    // Coordinator channel closed — cannot verify whether the
                    // previous iteration is still running. Skip rather than
                    // treating this as "no previous snapshot", which could
                    // fall through to Foreground inject and double-execute.
                    tracing::warn!(
                        task_id = %task_id,
                        previous_subagent = %prev_id,
                        "Loop in-flight query send failed (coordinator unavailable); skipping this fire"
                    );
                    return LoopFireOutcome::Skipped;
                }
            }
            None => None,
        };

        if let Some(snapshot) = &prev_snapshot
            && matches!(
                snapshot.status,
                SubagentSnapshotStatus::Initializing | SubagentSnapshotStatus::Running { .. }
            )
        {
            tracing::info!(
                task_id = %task_id,
                previous_subagent = %snapshot.subagent_id,
                "Skipping loop fire: previous iteration still running"
            );
            return LoopFireOutcome::Skipped;
        }

        if last_subagent_id.is_some() {
            let (respond_to, rx) = tokio::sync::oneshot::channel();
            if events
                .0
                .send(SubagentEvent::LoopUnitActive(
                    SubagentLoopUnitActiveRequest {
                        task_id: task_id.to_string(),
                        respond_to,
                    },
                ))
                .is_err()
            {
                return LoopFireOutcome::Skipped;
            }
            tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => return LoopFireOutcome::Skipped,
                active = rx => match active {
                    Ok(true) => {
                        tracing::info!(
                            task_id = %task_id,
                            "Skipping loop fire: a descendant of the previous iteration is still running"
                        );
                        return LoopFireOutcome::Skipped;
                    }
                    Ok(false) => {}
                    Err(_) => return LoopFireOutcome::Skipped,
                },
                _ = tokio::time::sleep(Duration::from_secs(10)) => {
                    tracing::warn!(
                        task_id = %task_id,
                        "Loop unit-active query timed out; skipping this fire"
                    );
                    return LoopFireOutcome::Skipped;
                }
            }
        }

        let prev_completed_output = prev_snapshot.as_ref().and_then(|s| match &s.status {
            SubagentSnapshotStatus::Completed { output, .. } => Some(output.clone()),
            _ => None,
        });
        let chain_due_for_restart = iterations_since_fresh >= LOOP_FRESH_CHAIN_EVERY;
        let anchor_usable = prev_completed_output.is_some() && last_subagent_id.is_some();
        let (resume_from, prior_summary, next_iterations_since_fresh) = if chain_reset_pending {
            (None, None, 1)
        } else if chain_due_for_restart || !anchor_usable {
            let summary = prev_completed_output
                .as_deref()
                .map(|o| truncate_chars(o, 600));
            (None, summary, 1)
        } else {
            (last_subagent_id.clone(), None, iterations_since_fresh + 1)
        };

        let subagent_id = uuid::Uuid::now_v7().to_string();
        let framed_prompt =
            format_loop_iteration_prompt(prompt, task_id, human_schedule, prior_summary.as_deref());
        let description = format!(
            "loop: {} ({human_schedule})",
            truncate_chars(prompt.lines().next().unwrap_or(prompt), 60)
        );

        {
            let mut res = self.resources.lock().await;
            let state = res.get_or_default::<State<SchedulerState>>();
            let Some(task) = state.tasks.iter_mut().find(|t| t.id == task_id) else {
                tracing::info!(task_id = %task_id, "Loop task deleted mid-fire; not spawning");
                return LoopFireOutcome::Skipped;
            };
            task.last_subagent_id = Some(subagent_id.clone());
            task.iterations_since_fresh = next_iterations_since_fresh;
            task.chain_reset_pending = false;
        }

        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let request = SubagentRequest {
            id: subagent_id.clone(),
            prompt: framed_prompt,
            description,
            subagent_type: "general-purpose".to_string(),
            parent_session_id,
            parent_prompt_id: None,
            resume_from,
            cwd: None,
            runtime_overrides: SubagentRuntimeOverrides {
                completion_output_cap: Some(LOOP_COMPLETION_OUTPUT_CAP),
                spawn_depth: Some(0),
                loop_task_id: Some(task_id.to_string()),
                ..Default::default()
            },
            run_in_background: true,
            surface_completion: true,
            await_to_completion: false,
            fork_context: false,
            owner: SubagentOwner::Task,
            // A child of the actor's token, so shutdown cancels a fire the
            // coordinator still has queued at the concurrent limit.
            cancel_token: self.cancel_token.child_token(),
        };

        if events
            .0
            .send(SubagentEvent::Spawn(SubagentSpawnRequest {
                request: Box::new(request),
                result_tx,
            }))
            .is_err()
        {
            let mut res = self.resources.lock().await;
            let state = res.get_or_default::<State<SchedulerState>>();
            if let Some(task) = state.tasks.iter_mut().find(|t| t.id == task_id) {
                // Restore the full pre-fire snapshot, including a pending
                // chain reset the aborted spawn had consumed — losing it
                // would let a later fire resume the old chain under a new
                // prompt.
                task.last_subagent_id = last_subagent_id;
                task.iterations_since_fresh = iterations_since_fresh;
                task.chain_reset_pending = chain_reset_pending;
            }
            return LoopFireOutcome::Foreground;
        }

        let resources = self.resources.clone();
        let guard_task_id = task_id.to_string();
        let spawned_id = subagent_id.clone();
        tokio::spawn(async move {
            let Ok(result) = result_rx.await else {
                return;
            };
            if result.error.is_none() {
                return;
            }
            tracing::warn!(
                task_id = %guard_task_id,
                subagent_id = %spawned_id,
                error = ?result.error,
                "Loop iteration spawn failed; clearing chain anchor"
            );
            let mut res = resources.lock().await;
            let state = res.get_or_default::<State<SchedulerState>>();
            if let Some(task) = state
                .tasks
                .iter_mut()
                .find(|t| t.last_subagent_id.as_deref() == Some(spawned_id.as_str()))
            {
                task.last_subagent_id = None;
                task.iterations_since_fresh = 0;
            }
        });

        LoopFireOutcome::Spawned(subagent_id)
    }

    async fn announce_existing_tasks(&self) {
        let version = self.clock.snapshot();
        let snapshot: Vec<ScheduledTaskCreated> = {
            let res = self.resources.lock().await;
            let Some(state) = res.get::<State<SchedulerState>>() else {
                return;
            };
            if state.tasks.is_empty() {
                return;
            }
            let now = Utc::now();
            // Iteration order matters: the pager sorts the tasks pane by
            // created_at, but every re-announced task gets a synthetic
            // `Instant::now()` on the pager side, so the relative order is
            // determined by the order we send notifications here. Keep
            // state.tasks as a Vec so insertion order survives.
            state
                .tasks
                .iter()
                .filter(|task| task.recurring || task.next_fire_at() > now)
                .map(|task| task_created_payload(task, version))
                .collect()
        };

        tracing::info!(
            count = snapshot.len(),
            "Re-announcing scheduled tasks after restore"
        );

        for created in snapshot {
            self.notification_handle
                .send_scheduled_task_created(created);
        }
    }

    async fn handle_command(&mut self, cmd: SchedulerCommand) {
        match cmd {
            SchedulerCommand::Create { task, reply } => {
                if let Some(pending) = &self.pending_removal {
                    let _ =
                        reply.send(Err(SchedulerError::RemovalPending(pending.task_id.clone())));
                    return;
                }
                let mut res = self.resources.lock().await;
                let state = res.get_or_default::<State<SchedulerState>>();
                if state.tasks.len() >= MAX_SCHEDULED_TASKS {
                    let _ = reply.send(Err(SchedulerError::TaskLimitReached(MAX_SCHEDULED_TASKS)));
                    return;
                }
                let mut reservation = self.clock.prepare_transition(1);
                state.tasks.push(task.clone());
                let commit = reservation.commit_next(&mut self.clock);
                log_rollover("create", Some(&task.id), commit.rollover);
                self.notification_handle
                    .send_scheduled_task_created(task_created_payload(&task, commit.version));
                let _ = reply.send(Ok(task));
            }
            SchedulerCommand::Update {
                id,
                prompt,
                interval_secs,
                reply,
            } => {
                if let Some(pending) = &self.pending_removal {
                    let _ =
                        reply.send(Err(SchedulerError::RemovalPending(pending.task_id.clone())));
                    return;
                }
                let mut res = self.resources.lock().await;
                let state = res.get_or_default::<State<SchedulerState>>();
                let Some(index) = state.tasks.iter().position(|task| task.id == id) else {
                    let _ = reply.send(Err(SchedulerError::TaskNotFound(id)));
                    return;
                };
                let mut reservation = self.clock.prepare_transition(1);
                let task = &mut state.tasks[index];
                if let Some(prompt) = prompt {
                    // A new prompt is a new job: the next fire starts a fresh
                    // transcript. The anchor is kept (not cleared) so the
                    // in-flight guard still sees a running old iteration —
                    // clearing it here would let the next fire double-spawn.
                    if prompt != task.prompt {
                        task.chain_reset_pending = true;
                        task.iterations_since_fresh = 0;
                    }
                    task.prompt = prompt;
                }
                if let Some(interval_secs) = interval_secs {
                    task.interval_secs = interval_secs;
                    if task.next_fire_at() <= Utc::now() {
                        task.last_fired_at = Some(Utc::now());
                    }
                }
                let updated = task.clone();
                drop(res);

                let commit = reservation.commit_next(&mut self.clock);
                log_rollover("update", Some(&id), commit.rollover);
                self.notification_handle
                    .send_scheduled_task_created(task_created_payload(&updated, commit.version));
                let _ = reply.send(Ok(updated));
            }
            SchedulerCommand::Delete { id, reply } => {
                if self
                    .pending_removal
                    .as_ref()
                    .is_some_and(|pending| pending.task_id == id)
                {
                    let _ = reply.send(self.complete_pending_removal().await);
                    return;
                }
                if let Some(pending) = &self.pending_removal {
                    let _ =
                        reply.send(Err(SchedulerError::RemovalPending(pending.task_id.clone())));
                    return;
                }

                let mut res = self.resources.lock().await;
                let state = res.get_or_default::<State<SchedulerState>>();
                let Some(index) = state.tasks.iter().position(|task| task.id == id) else {
                    let _ = reply.send(Ok(false));
                    return;
                };
                // A replay create may exist even for a currently non-durable task.
                if self.notification_handle.durable_targets() == DurableNotificationTargets::None {
                    let _ = reply.send(Err(SchedulerError::NoDurableNotificationConsumer));
                    return;
                }
                state.tasks.remove(index);
                drop(res);
                self.pending_removal = Some(PendingDurableRemoval {
                    task_id: id,
                    reservation: self.clock.prepare_transition(1),
                });
                let _ = reply.send(self.complete_pending_removal().await);
            }
            SchedulerCommand::List { reply } => {
                let res = self.resources.lock().await;
                let tasks = res
                    .get::<State<SchedulerState>>()
                    .map(|state| state.tasks.clone())
                    .unwrap_or_default();
                let _ = reply.send(SchedulerSnapshot {
                    version: self.clock.snapshot(),
                    tasks,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementations::echo_agent_build::scheduler::types::{
        ScheduledTask, SchedulerHandle, scheduler_tool_error,
    };
    use crate::notification::{AcknowledgedToolNotification, ToolNotification};
    use crate::types::resources::{Resources, WebCitationCounter};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    macro_rules! notification {
        ($event:expr, $variant:ident) => {{
            let ToolNotification::$variant(event) = $event else {
                panic!(concat!("expected ", stringify!($variant)));
            };
            event
        }};
    }

    fn make_durable_actor(
        tasks: Vec<ScheduledTask>,
        persistence: Arc<crate::persistence::ResourcesPersistence>,
    ) -> (
        SchedulerHandle,
        CancellationToken,
        mpsc::UnboundedReceiver<AcknowledgedToolNotification>,
        SharedResources,
    ) {
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();
        resources.register_state::<WebCitationCounter>();
        resources.get_or_default::<State<SchedulerState>>().tasks = tasks;
        let shared = Arc::new(Mutex::new(resources));
        let (notification_handle, notifications) = ToolNotificationHandle::acknowledged_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();
        tokio::spawn(
            SchedulerActor {
                resources: shared.clone(),
                resources_persistence: persistence,
                notification_handle,
                cmd_rx,
                cancel_token: cancel_token.clone(),
                clock: SchedulerClock::new(),
                pending_removal: None,
                blocked_expiries: HashSet::new(),
            }
            .run(),
        );
        (SchedulerHandle(cmd_tx), cancel_token, notifications, shared)
    }

    async fn delete(handle: &SchedulerHandle, id: &str) -> Result<bool, SchedulerError> {
        let (reply, response) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Delete {
                id: id.into(),
                reply,
            })
            .unwrap();
        response.await.unwrap()
    }

    async fn next_acknowledged(
        notifications: &mut mpsc::UnboundedReceiver<AcknowledgedToolNotification>,
    ) -> AcknowledgedToolNotification {
        tokio::time::timeout(Duration::from_secs(2), notifications.recv())
            .await
            .expect("notification timeout")
            .expect("notification channel closed")
    }

    fn expired_task(id: &str, durable: bool) -> ScheduledTask {
        let mut task = ScheduledTask::new(1, id.into(), true, durable);
        task.id = id.into();
        task.created_at = Utc::now() - chrono::Duration::seconds(10);
        task.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
        task.foreground = true;
        task
    }

    fn due_one_shot(id: &str) -> ScheduledTask {
        let mut task = ScheduledTask::new(1, id.into(), false, false);
        task.id = id.into();
        task.created_at = Utc::now() - chrono::Duration::seconds(10);
        task
    }

    fn auto_acknowledged_notifications() -> (
        ToolNotificationHandle,
        mpsc::UnboundedReceiver<ToolNotification>,
    ) {
        let (plain, notifications) = ToolNotificationHandle::channel();
        let (acknowledged, mut deliveries) = ToolNotificationHandle::acknowledged_channel();
        tokio::spawn(async move {
            while let Some(delivery) = deliveries.recv().await {
                if let Some(acknowledgement) = delivery.acknowledgement {
                    let _ = acknowledgement.send(Ok(()));
                }
            }
        });
        (
            ToolNotificationHandle::tee(vec![plain, acknowledged]),
            notifications,
        )
    }

    fn make_boundary_actor(
        tasks: Vec<ScheduledTask>,
        revision: u64,
    ) -> (SchedulerActor, mpsc::UnboundedReceiver<ToolNotification>) {
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();
        resources.get_or_default::<State<SchedulerState>>().tasks = tasks;
        let (notification_handle, notifications) = auto_acknowledged_notifications();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        (
            SchedulerActor {
                resources: Arc::new(Mutex::new(resources)),
                resources_persistence: Arc::new(crate::persistence::ResourcesPersistence::noop()),
                notification_handle,
                cmd_rx,
                cancel_token: CancellationToken::new(),
                clock: SchedulerClock::at_revision_for_test(revision),
                pending_removal: None,
                blocked_expiries: HashSet::new(),
            },
            notifications,
        )
    }

    fn make_test_actor() -> (
        SchedulerHandle,
        CancellationToken,
        mpsc::UnboundedReceiver<ToolNotification>,
    ) {
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();
        let shared = Arc::new(Mutex::new(resources));

        let (notif_handle, notif_rx) = auto_acknowledged_notifications();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();

        let actor = SchedulerActor {
            resources: shared,
            resources_persistence: Arc::new(crate::persistence::ResourcesPersistence::noop()),
            notification_handle: notif_handle,
            cmd_rx,
            cancel_token: cancel_token.clone(),
            clock: SchedulerClock::new(),
            pending_removal: None,
            blocked_expiries: HashSet::new(),
        };

        tokio::spawn(actor.run());

        (SchedulerHandle(cmd_tx), cancel_token, notif_rx)
    }

    #[tokio::test]
    async fn create_and_list_task() {
        let (handle, cancel, _notif_rx) = make_test_actor();

        let task = ScheduledTask::new(300, "check deploy".into(), true, false);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Create {
                task: task.clone(),
                reply: reply_tx,
            })
            .unwrap();
        let result = reply_rx.await.unwrap();
        assert!(result.is_ok());

        let (list_tx, list_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::List { reply: list_tx })
            .unwrap();
        let snapshot = list_rx.await.unwrap();
        assert_eq!(snapshot.version.revision(), 1);
        assert_eq!(snapshot.tasks.len(), 1);
        assert_eq!(snapshot.tasks[0].prompt, "check deploy");

        cancel.cancel();
    }

    #[tokio::test]
    async fn delete_task_existing_then_absent_is_truthful() {
        let (handle, cancel, _notif_rx) = make_test_actor();

        let task = ScheduledTask::new(300, "test".into(), true, false);
        let task_id = task.id.clone();

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Create {
                task,
                reply: reply_tx,
            })
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        let (del_tx, del_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Delete {
                id: task_id.clone(),
                reply: del_tx,
            })
            .unwrap();
        let removed = del_rx.await.unwrap().unwrap();
        assert!(removed);
        assert!(!delete(&handle, &task_id).await.unwrap());

        let (list_tx, list_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::List { reply: list_tx })
            .unwrap();
        let tasks = list_rx.await.unwrap().tasks;
        assert!(tasks.is_empty());

        cancel.cancel();
    }

    #[tokio::test]
    async fn delete_nonexistent_returns_false() {
        let (handle, cancel, _notif_rx) = make_test_actor();

        let (del_tx, del_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Delete {
                id: "nonexistent".into(),
                reply: del_tx,
            })
            .unwrap();
        let removed = del_rx.await.unwrap().unwrap();
        assert!(!removed);

        cancel.cancel();
    }

    #[tokio::test]
    async fn max_task_limit_enforced() {
        let (handle, cancel, _notif_rx) = make_test_actor();

        for i in 0..MAX_SCHEDULED_TASKS {
            let task = ScheduledTask::new(300, format!("task {i}"), true, false);
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            handle
                .0
                .send(SchedulerCommand::Create {
                    task,
                    reply: reply_tx,
                })
                .unwrap();
            reply_rx.await.unwrap().unwrap();
        }

        let task = ScheduledTask::new(300, "one too many".into(), true, false);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Create {
                task,
                reply: reply_tx,
            })
            .unwrap();
        let result = reply_rx.await.unwrap();
        assert!(result.is_err());

        cancel.cancel();
    }

    #[tokio::test]
    async fn one_shot_boundary_rotates_generation_without_retrying() {
        let mut task = ScheduledTask::new(1, "due one-shot".into(), false, false);
        task.created_at = Utc::now() - chrono::Duration::seconds(10);
        let (mut actor, mut notifications) = make_boundary_actor(vec![task], u64::MAX - 1);
        let old_generation = actor.clock.snapshot().generation();

        actor.fire_next_task().await;

        let fired = notification!(notifications.try_recv().unwrap(), ScheduledTaskFired);
        let removed = notification!(notifications.try_recv().unwrap(), ScheduledTaskRemoved);
        assert_ne!(fired.generation, old_generation);
        assert_eq!(removed.generation, fired.generation);
        assert_eq!((fired.revision, removed.revision), (1, 2));
    }

    #[tokio::test]
    async fn expired_task_and_explicit_delete_rotate_at_boundary() {
        let mut expired = ScheduledTask::new(300, "expired".into(), true, false);
        expired.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
        expired.created_at = Utc::now() - chrono::Duration::seconds(600);
        let mut deleted = ScheduledTask::new(300, "delete me".into(), true, false);
        deleted.id = "delete-me".into();
        let (mut actor, mut notifications) = make_boundary_actor(vec![expired, deleted], u64::MAX);

        actor.fire_next_task().await;
        let expired = notification!(notifications.try_recv().unwrap(), ScheduledTaskRemoved);
        assert_eq!(expired.revision, 1);
        assert_eq!(expired.reason, ScheduledTaskRemovedReason::Expired);

        actor.clock = SchedulerClock::at_revision_for_test(u64::MAX);
        let (acknowledged, mut deliveries) = ToolNotificationHandle::acknowledged_channel();
        actor.notification_handle =
            ToolNotificationHandle::tee(vec![actor.notification_handle.clone(), acknowledged]);
        let (reply, response) = tokio::sync::oneshot::channel();
        let delete = actor.handle_command(SchedulerCommand::Delete {
            id: "delete-me".into(),
            reply,
        });
        tokio::pin!(delete);
        let delivery = tokio::select! {
            _ = &mut delete => panic!("delete must wait for acknowledgement"),
            delivery = tokio::time::timeout(Duration::from_secs(1), deliveries.recv()) => {
                delivery.unwrap().unwrap()
            },
        };
        delivery.acknowledgement.unwrap().send(Ok(())).unwrap();
        tokio::time::timeout(Duration::from_secs(1), delete)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), response)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        );
        let deleted = notification!(notifications.try_recv().unwrap(), ScheduledTaskRemoved);
        assert_eq!(deleted.revision, 1);
        assert_eq!(deleted.reason, ScheduledTaskRemovedReason::Deleted);
    }

    /// A recurring task whose interval stretches past the TTL must still expire at `expires_at`:
    /// the actor wakes on min(next_fire_at, expires_at), so an interval longer than the TTL
    /// cannot delay the expiry to its first fire.
    #[tokio::test]
    async fn expiry_fires_at_ttl_when_interval_outlives_it() {
        let mut task = ScheduledTask::new(8 * 86_400, "interval outlives ttl".into(), true, false);
        task.id = "slow-loop".into();
        task.created_at = Utc::now() - chrono::Duration::seconds(10);
        task.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
        let (mut actor, mut notifications) = make_boundary_actor(vec![task], 0);

        assert_eq!(actor.compute_next_fire_delay().await, Duration::ZERO);

        actor.fire_next_task().await;
        let removed = notification!(notifications.try_recv().unwrap(), ScheduledTaskRemoved);
        assert_eq!(removed.task_id, "slow-loop");
        assert_eq!(removed.reason, ScheduledTaskRemovedReason::Expired);
        let res = actor.resources.lock().await;
        let state = res.get::<State<SchedulerState>>().unwrap();
        assert!(state.tasks.is_empty(), "expired task must be removed");
    }

    #[tokio::test]
    async fn create_and_update_rotate_at_boundary() {
        let (mut actor, mut notifications) = make_boundary_actor(Vec::new(), u64::MAX);
        let task = ScheduledTask::new(300, "create".into(), true, false);
        let task_id = task.id.clone();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        actor
            .handle_command(SchedulerCommand::Create {
                task,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.unwrap().unwrap();
        let created = notification!(notifications.try_recv().unwrap(), ScheduledTaskCreated);
        assert_eq!(created.revision, 1);

        actor.clock = SchedulerClock::at_revision_for_test(u64::MAX);
        let (reply, _) = tokio::sync::oneshot::channel();
        actor
            .handle_command(SchedulerCommand::Update {
                id: task_id,
                prompt: Some("updated".into()),
                interval_secs: None,
                reply,
            })
            .await;
        let updated = notification!(notifications.try_recv().unwrap(), ScheduledTaskCreated);
        assert_eq!(updated.revision, 1);
    }

    #[tokio::test]
    async fn shutdown_rotates_once_for_fifo_batch() {
        let mut first = ScheduledTask::new(300, "first".into(), true, false);
        first.id = "first".into();
        let mut second = ScheduledTask::new(300, "second".into(), true, false);
        second.id = "second".into();
        let (actor, mut notifications) = make_boundary_actor(vec![first, second], u64::MAX - 1);
        let old_generation = actor.clock.snapshot().generation();
        actor.cancel_token.cancel();

        actor.run().await;

        for id in ["first", "second"] {
            let created = notification!(notifications.try_recv().unwrap(), ScheduledTaskCreated);
            assert_eq!(
                (
                    created.task_id.as_str(),
                    created.generation.as_str(),
                    created.revision
                ),
                (id, old_generation.as_str(), u64::MAX - 1)
            );
        }
        let first = notification!(notifications.try_recv().unwrap(), ScheduledTaskRemoved);
        let second = notification!(notifications.try_recv().unwrap(), ScheduledTaskRemoved);
        assert_eq!((first.task_id.as_str(), first.revision), ("first", 1));
        assert_eq!((second.task_id.as_str(), second.revision), ("second", 2));
        assert_eq!(first.reason, ScheduledTaskRemovedReason::Shutdown);
        assert_eq!(second.reason, ScheduledTaskRemovedReason::Shutdown);
        assert_eq!(first.generation, second.generation);
        assert_ne!(first.generation, old_generation);
    }

    /// A plain (non-durable) recurring `/loop` must survive a process restart: persisted with
    /// the session resources, then re-announced and re-armed by the next process's actor.
    #[tokio::test]
    async fn recurring_task_survives_restart_and_rearms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resources_state.json");

        // "First process": a due, non-durable recurring task exists; persist
        // the resources the way the tool bridge does after a tool call.
        let persistence = Arc::new(crate::persistence::ResourcesPersistence::new(path.clone()));
        let mut task = ScheduledTask::new(1, "babysit the pipeline".into(), true, false);
        task.id = "loop-1".into();
        task.created_at = Utc::now() - chrono::Duration::seconds(10);
        task.foreground = true;
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();
        resources.get_or_default::<State<SchedulerState>>().tasks = vec![task];
        persistence.save(&resources);
        persistence.flush().await;
        drop(resources); // process dies

        // "Second process": fresh resources restored from disk re-arm the loop.
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();
        assert!(
            crate::persistence::ResourcesPersistence::new(path).load(&mut resources),
            "persisted scheduler state must load on restart"
        );
        let shared = Arc::new(Mutex::new(resources));
        let (notif_handle, mut notif_rx) = auto_acknowledged_notifications();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();
        tokio::spawn(
            SchedulerActor {
                resources: shared,
                resources_persistence: Arc::new(crate::persistence::ResourcesPersistence::noop()),
                notification_handle: notif_handle,
                cmd_rx,
                cancel_token: cancel_token.clone(),
                clock: SchedulerClock::new(),
                pending_removal: None,
                blocked_expiries: HashSet::new(),
            }
            .run(),
        );

        let announced = notification!(next_event(&mut notif_rx).await, ScheduledTaskCreated);
        assert_eq!(announced.task_id, "loop-1", "restored loop is re-announced");
        let fired = notification!(next_event(&mut notif_rx).await, ScheduledTaskFired);
        assert_eq!(fired.task_id, "loop-1", "overdue restored loop fires");
        cancel_token.cancel();
    }

    /// A non-durable expiry must persist the task's absence, otherwise every restart resurrects
    /// the task, re-expires it, and stacks a duplicate expiry record in the session transcript.
    #[tokio::test]
    async fn non_durable_expiry_persists_task_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resources_state.json");
        let persistence = Arc::new(crate::persistence::ResourcesPersistence::new(path.clone()));

        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();
        resources.get_or_default::<State<SchedulerState>>().tasks =
            vec![expired_task("stale-loop", false)];
        persistence.save(&resources);
        persistence.flush().await;

        let (notif_handle, mut notif_rx) = auto_acknowledged_notifications();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let mut actor = SchedulerActor {
            resources: Arc::new(Mutex::new(resources)),
            resources_persistence: persistence.clone(),
            notification_handle: notif_handle,
            cmd_rx,
            cancel_token: CancellationToken::new(),
            clock: SchedulerClock::new(),
            pending_removal: None,
            blocked_expiries: HashSet::new(),
        };
        actor.fire_next_task().await;
        persistence.flush().await;

        let removed = notification!(next_event(&mut notif_rx).await, ScheduledTaskRemoved);
        assert_eq!(removed.reason, ScheduledTaskRemovedReason::Expired);
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            persisted["state"]["echo_agent_build.Scheduler"]["tasks"],
            serde_json::json!([]),
            "expired task must not survive on disk"
        );
    }

    #[tokio::test]
    async fn recurring_task_fires_repeatedly_without_external_clear() {
        let (handle, cancel, mut notif_rx) = make_test_actor();

        let mut task = ScheduledTask::new(1, "recurring".into(), true, false);
        task.created_at = chrono::Utc::now() - chrono::Duration::seconds(10);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Create {
                task,
                reply: reply_tx,
            })
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        // Drain ScheduledTaskCreated.
        let notif = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv())
            .await
            .expect("created")
            .expect("channel open");
        assert!(matches!(notif, ToolNotification::ScheduledTaskCreated(_)));

        // First fire.
        let notif = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv())
            .await
            .expect("first fire")
            .expect("channel open");
        assert!(matches!(notif, ToolNotification::ScheduledTaskFired(_)));

        // The task re-fires purely from `next_fire_at` rescheduling, with no
        // in-flight guard to clear.
        let notif = tokio::time::timeout(Duration::from_secs(3), notif_rx.recv())
            .await
            .expect("second fire")
            .expect("channel open");
        assert!(matches!(notif, ToolNotification::ScheduledTaskFired(_)));

        cancel.cancel();
    }

    #[tokio::test]
    async fn announces_existing_tasks_on_startup() {
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();

        // Simulate session restore: scheduler state already contains two
        // recurring tasks before the actor spawns. Both intervals are far
        // enough in the future that neither will fire during the test.
        let state = resources.get_or_default::<State<SchedulerState>>();
        let mut task_a = ScheduledTask::new(300, "task A".into(), true, false);
        task_a.id = "restored-A".to_string();
        state.tasks.push(task_a);
        let mut task_b = ScheduledTask::new(600, "task B".into(), true, false);
        task_b.id = "restored-B".to_string();
        state.tasks.push(task_b);

        let shared = Arc::new(Mutex::new(resources));
        let (notif_handle, mut notif_rx) = auto_acknowledged_notifications();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();

        let actor = SchedulerActor {
            resources: shared,
            resources_persistence: Arc::new(crate::persistence::ResourcesPersistence::noop()),
            notification_handle: notif_handle,
            cmd_rx,
            cancel_token: cancel_token.clone(),
            clock: SchedulerClock::new(),
            pending_removal: None,
            blocked_expiries: HashSet::new(),
        };

        tokio::spawn(actor.run());

        // The first two events on the channel must be ScheduledTaskCreated
        // for the two restored tasks, in insertion order.
        let n1 = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv())
            .await
            .expect("first notification")
            .expect("channel open");
        let n2 = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv())
            .await
            .expect("second notification")
            .expect("channel open");

        let ToolNotification::ScheduledTaskCreated(c1) = n1 else {
            panic!("expected ScheduledTaskCreated, got {n1:?}");
        };
        let ToolNotification::ScheduledTaskCreated(c2) = n2 else {
            panic!("expected ScheduledTaskCreated, got {n2:?}");
        };

        assert_eq!(c1.task_id, "restored-A");
        assert_eq!(c1.prompt, "task A");
        assert_eq!(c1.human_schedule, "every 5 minutes");
        assert!(c1.next_fire_at.is_some());

        assert_eq!(c2.task_id, "restored-B");
        assert_eq!(c2.prompt, "task B");
        assert_eq!(c2.human_schedule, "every 10 minutes");
        assert!(c2.next_fire_at.is_some());
        assert_eq!(c1.generation, c2.generation);
        assert_eq!((c1.revision, c2.revision), (0, 0));

        // No further events should arrive within a short window: tasks are
        // not yet due and no commands are in flight.
        let extra = tokio::time::timeout(Duration::from_millis(150), notif_rx.recv()).await;
        assert!(
            extra.is_err(),
            "no further notifications expected, got {:?}",
            extra.ok()
        );

        cancel_token.cancel();
    }

    #[tokio::test]
    async fn announces_no_tasks_when_state_empty() {
        let (_handle, cancel, mut notif_rx) = make_test_actor();

        // Brand-new session: state.tasks is empty, the re-announce step must
        // be a silent no-op.
        let result = tokio::time::timeout(Duration::from_millis(150), notif_rx.recv()).await;
        assert!(
            result.is_err(),
            "expected no notifications, got {:?}",
            result.ok()
        );

        cancel.cancel();
    }

    #[tokio::test]
    async fn legacy_one_shot_fires_via_normal_loop_and_is_removed() {
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();

        let state = resources.get_or_default::<State<SchedulerState>>();
        let mut task = ScheduledTask::new(1, "legacy one-shot".into(), false, false);
        task.id = "legacy-1".to_string();
        task.created_at = chrono::Utc::now() - chrono::Duration::seconds(60);
        state.tasks.push(task);

        let shared = Arc::new(Mutex::new(resources));
        let (notif_handle, mut notif_rx) = auto_acknowledged_notifications();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();

        let actor = SchedulerActor {
            resources: shared.clone(),
            resources_persistence: Arc::new(crate::persistence::ResourcesPersistence::noop()),
            notification_handle: notif_handle,
            cmd_rx,
            cancel_token: cancel_token.clone(),
            clock: SchedulerClock::new(),
            pending_removal: None,
            blocked_expiries: HashSet::new(),
        };
        tokio::spawn(actor.run());

        let mut kinds = Vec::new();
        for _ in 0..2 {
            let notif = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv())
                .await
                .expect("notification")
                .expect("channel open");
            kinds.push(match notif {
                ToolNotification::ScheduledTaskCreated(_) => "created",
                ToolNotification::ScheduledTaskFired(_) => "fired",
                ToolNotification::ScheduledTaskRemoved(removed) => {
                    assert_eq!(removed.reason, ScheduledTaskRemovedReason::Completed);
                    "removed"
                }
                _ => "other",
            });
        }
        assert_eq!(kinds, vec!["fired", "removed"]);

        let res = shared.lock().await;
        let remaining = res
            .get::<State<SchedulerState>>()
            .map(|s| s.tasks.len())
            .unwrap_or(0);
        assert_eq!(remaining, 0, "legacy one-shot removed after firing");
        cancel_token.cancel();
    }

    #[tokio::test]
    async fn update_patches_in_place_preserving_identity_and_phase() {
        let (handle, cancel, _notif_rx) = make_test_actor();

        let task = ScheduledTask::new(300, "check deploy".into(), true, false);
        let task_id = task.id.clone();
        let created_at = task.created_at;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Create {
                task,
                reply: reply_tx,
            })
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        let (up_tx, up_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Update {
                id: task_id.clone(),
                prompt: None,
                interval_secs: Some(600),
                reply: up_tx,
            })
            .unwrap();
        let updated = up_rx.await.unwrap().unwrap();
        assert_eq!(updated.id, task_id, "identity preserved");
        assert_eq!(updated.interval_secs, 600);
        assert_eq!(updated.prompt, "check deploy");
        assert_eq!(updated.created_at, created_at, "phase anchor preserved");
        assert!(updated.last_fired_at.is_none());

        let (up_tx, up_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Update {
                id: task_id.clone(),
                prompt: Some("check rollback".into()),
                interval_secs: None,
                reply: up_tx,
            })
            .unwrap();
        let updated = up_rx.await.unwrap().unwrap();
        assert_eq!(updated.interval_secs, 600);
        assert_eq!(updated.prompt, "check rollback");

        let (list_tx, list_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::List { reply: list_tx })
            .unwrap();
        assert_eq!(list_rx.await.unwrap().tasks.len(), 1);

        cancel.cancel();
    }

    #[tokio::test]
    async fn update_shrinking_interval_never_fires_immediately() {
        let (handle, cancel, mut notif_rx) = make_test_actor();

        let mut task = ScheduledTask::new(3600, "check deploy".into(), true, false);
        task.created_at = chrono::Utc::now() - chrono::Duration::seconds(600);
        let task_id = task.id.clone();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Create {
                task,
                reply: reply_tx,
            })
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        let (up_tx, up_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Update {
                id: task_id,
                prompt: None,
                interval_secs: Some(60),
                reply: up_tx,
            })
            .unwrap();
        let updated = up_rx.await.unwrap().unwrap();
        assert!(
            updated.next_fire_at() > chrono::Utc::now(),
            "an update must never leave the task immediately due"
        );

        let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(100), notif_rx.recv()).await {
                Ok(Some(ToolNotification::ScheduledTaskFired(_))) => {
                    panic!("update must not trigger a fire");
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }

        cancel.cancel();
    }

    #[tokio::test]
    async fn update_unknown_id_errors_and_never_creates() {
        let (handle, cancel, _notif_rx) = make_test_actor();

        let (up_tx, up_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Update {
                id: "nonexistent".into(),
                prompt: Some("new prompt".into()),
                interval_secs: Some(300),
                reply: up_tx,
            })
            .unwrap();
        let result = up_rx.await.unwrap();
        assert!(matches!(result, Err(SchedulerError::TaskNotFound(_))));

        let (list_tx, list_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::List { reply: list_tx })
            .unwrap();
        assert!(
            list_rx.await.unwrap().tasks.is_empty(),
            "a failed update must not fall back to creating a task"
        );

        cancel.cancel();
    }

    #[tokio::test]
    async fn update_emits_created_notification_as_upsert() {
        let (handle, cancel, mut notif_rx) = make_test_actor();

        let task = ScheduledTask::new(300, "check deploy".into(), true, false);
        let task_id = task.id.clone();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Create {
                task,
                reply: reply_tx,
            })
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv()).await;

        let (up_tx, up_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Update {
                id: task_id.clone(),
                prompt: Some("check rollback".into()),
                interval_secs: None,
                reply: up_tx,
            })
            .unwrap();
        up_rx.await.unwrap().unwrap();

        let notif = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv())
            .await
            .expect("upsert notification")
            .expect("channel open");
        let ToolNotification::ScheduledTaskCreated(created) = notif else {
            panic!("expected ScheduledTaskCreated upsert, got {notif:?}");
        };
        assert_eq!(created.task_id, task_id, "same chip identity");
        assert_eq!(created.prompt, "check rollback");

        cancel.cancel();
    }

    #[tokio::test]
    async fn cancel_sends_removed_for_remaining_tasks_without_draining_state() {
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();

        let state = resources.get_or_default::<State<SchedulerState>>();
        let mut task_a = ScheduledTask::new(300, "task A".into(), true, false);
        task_a.id = "cancel-A".to_string();
        state.tasks.push(task_a);
        let mut task_b = ScheduledTask::new(600, "task B".into(), true, false);
        task_b.id = "cancel-B".to_string();
        state.tasks.push(task_b);

        let shared = Arc::new(Mutex::new(resources));
        let (notif_handle, mut notif_rx) = auto_acknowledged_notifications();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();

        let actor = SchedulerActor {
            resources: shared.clone(),
            resources_persistence: Arc::new(crate::persistence::ResourcesPersistence::noop()),
            notification_handle: notif_handle,
            cmd_rx,
            cancel_token: cancel_token.clone(),
            clock: SchedulerClock::new(),
            pending_removal: None,
            blocked_expiries: HashSet::new(),
        };

        let handle = tokio::spawn(actor.run());

        // Drain the ScheduledTaskCreated announcements.
        let _ = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv()).await;
        let _ = tokio::time::timeout(Duration::from_secs(1), notif_rx.recv()).await;

        cancel_token.cancel();
        handle.await.expect("actor should complete");

        // Collect all ScheduledTaskRemoved notifications.
        let mut removed_ids = Vec::new();
        while let Ok(notif) = notif_rx.try_recv() {
            if let ToolNotification::ScheduledTaskRemoved(r) = notif {
                removed_ids.push(r.task_id);
            }
        }
        removed_ids.sort();
        assert_eq!(removed_ids, vec!["cancel-A", "cancel-B"]);
        assert_eq!(
            shared
                .lock()
                .await
                .get::<State<SchedulerState>>()
                .unwrap()
                .tasks
                .len(),
            2
        );
    }

    fn make_test_actor_with_subagents_at(
        revision: u64,
    ) -> (
        SchedulerHandle,
        CancellationToken,
        mpsc::UnboundedReceiver<ToolNotification>,
        mpsc::UnboundedReceiver<SubagentEvent>,
        SharedResources,
    ) {
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();
        let (subagent_tx, subagent_rx) = mpsc::unbounded_channel();
        resources.insert(SubagentEventSender(subagent_tx));
        resources.insert(
            crate::implementations::echo_agent_build::task::types::SessionIdResource(
                "parent-session".to_string(),
            ),
        );
        let shared = Arc::new(Mutex::new(resources));

        let (notif_handle, notif_rx) = auto_acknowledged_notifications();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();

        let actor = SchedulerActor {
            resources: shared.clone(),
            resources_persistence: Arc::new(crate::persistence::ResourcesPersistence::noop()),
            notification_handle: notif_handle,
            cmd_rx,
            cancel_token: cancel_token.clone(),
            clock: SchedulerClock::at_revision_for_test(revision),
            pending_removal: None,
            blocked_expiries: HashSet::new(),
        };
        tokio::spawn(actor.run());

        (
            SchedulerHandle(cmd_tx),
            cancel_token,
            notif_rx,
            subagent_rx,
            shared,
        )
    }

    fn make_test_actor_with_subagents() -> (
        SchedulerHandle,
        CancellationToken,
        mpsc::UnboundedReceiver<ToolNotification>,
        mpsc::UnboundedReceiver<SubagentEvent>,
    ) {
        let (handle, cancel, notifications, subagents, _) = make_test_actor_with_subagents_at(0);
        (handle, cancel, notifications, subagents)
    }

    async fn create_due_task(handle: &SchedulerHandle, prompt: &str, foreground: bool) -> String {
        let mut task = ScheduledTask::new(1, prompt.into(), true, false);
        task.foreground = foreground;
        task.created_at = chrono::Utc::now() - chrono::Duration::seconds(10);
        let task_id = task.id.clone();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Create {
                task,
                reply: reply_tx,
            })
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        task_id
    }

    async fn next_event<T>(rx: &mut mpsc::UnboundedReceiver<T>) -> T {
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("event timeout")
            .expect("channel closed")
    }

    fn subagent_snapshot(
        subagent_id: &str,
        status: SubagentSnapshotStatus,
    ) -> crate::implementations::echo_agent_build::task::types::SubagentSnapshot {
        crate::implementations::echo_agent_build::task::types::SubagentSnapshot {
            subagent_id: subagent_id.into(),
            description: "loop: watch ci".into(),
            subagent_type: "general-purpose".into(),
            status,
            started_at_epoch_ms: 0,
            duration_ms: 100,
            persona: None,
        }
    }

    async fn answer_subagent_query(
        rx: &mut mpsc::UnboundedReceiver<SubagentEvent>,
        subagent_id: &str,
        status: SubagentSnapshotStatus,
    ) {
        let SubagentEvent::Query(query) = next_event(rx).await else {
            panic!("expected subagent query");
        };
        assert_eq!(query.subagent_id, subagent_id);
        assert!(!query.block);
        query
            .respond_to
            .send(Some(subagent_snapshot(subagent_id, status)))
            .unwrap();
    }

    async fn next_subagent_spawn(
        rx: &mut mpsc::UnboundedReceiver<SubagentEvent>,
    ) -> Box<SubagentRequest> {
        let SubagentEvent::Spawn(spawn) = next_event(rx).await else {
            panic!("expected subagent spawn");
        };
        spawn.request
    }

    async fn answer_loop_unit_active(
        rx: &mut mpsc::UnboundedReceiver<SubagentEvent>,
        active: bool,
    ) {
        let SubagentEvent::LoopUnitActive(req) = next_event(rx).await else {
            panic!("expected loop-unit-active query");
        };
        req.respond_to.send(active).unwrap();
    }

    #[tokio::test]
    async fn background_fire_spawns_loop_subagent() {
        let (handle, cancel, mut notif_rx, mut subagent_rx) = make_test_actor_with_subagents();
        let task_id = create_due_task(&handle, "check deploy status", false).await;

        let event = next_event(&mut subagent_rx).await;
        let SubagentEvent::Spawn(request) = event else {
            panic!("expected Spawn, got a different event");
        };
        assert!(request.run_in_background);
        assert!(request.surface_completion);
        assert!(request.parent_prompt_id.is_none());
        assert!(request.resume_from.is_none(), "first iteration is fresh");
        assert_eq!(request.subagent_type, "general-purpose");
        assert_eq!(request.parent_session_id, "parent-session");
        assert!(request.prompt.contains("check deploy status"));
        assert!(request.prompt.contains(&task_id));
        assert!(request.prompt.contains("short status"));
        assert_eq!(
            request.runtime_overrides.completion_output_cap,
            Some(LOOP_COMPLETION_OUTPUT_CAP)
        );
        assert_eq!(request.runtime_overrides.spawn_depth, Some(0));
        assert!(request.description.starts_with("loop: "));

        let mut fired_subagent_id = None;
        for _ in 0..4 {
            let notif = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv())
                .await
                .expect("notification")
                .expect("channel open");
            if let ToolNotification::ScheduledTaskFired(f) = notif {
                fired_subagent_id = f.subagent_id;
                break;
            }
        }
        assert_eq!(fired_subagent_id.as_deref(), Some(request.id.as_str()));

        let (list_tx, list_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::List { reply: list_tx })
            .unwrap();
        let task = &list_rx.await.unwrap().tasks[0];
        assert_eq!(task.last_subagent_id.as_deref(), Some(request.id.as_str()));
        assert_eq!(task.iterations_since_fresh, 1);

        cancel.cancel();
    }

    #[tokio::test]
    async fn in_flight_iteration_skips_fire_then_resumes_chain() {
        let (handle, cancel, mut notif_rx, mut subagent_rx, resources) =
            make_test_actor_with_subagents_at(u64::MAX - 2);
        create_due_task(&handle, "watch ci", false).await;
        resources
            .lock()
            .await
            .get_or_default::<State<SchedulerState>>()
            .tasks[0]
            .last_fired_at = Some(Utc::now() - chrono::Duration::seconds(1));

        let first = next_subagent_spawn(&mut subagent_rx).await;
        let first_id = first.id.clone();
        let old_generation = loop {
            if let ToolNotification::ScheduledTaskFired(fired) = next_event(&mut notif_rx).await {
                assert_eq!(fired.revision, u64::MAX);
                break fired.generation;
            }
        };
        answer_subagent_query(
            &mut subagent_rx,
            &first_id,
            SubagentSnapshotStatus::Running {
                turn_count: 1,
                tool_call_count: 2,
                tokens_used: 0,
                context_window_tokens: 0,
                context_usage_pct: 0,
                tools_used: vec![],
                error_count: 0,
            },
        )
        .await;

        let skipped = notification!(next_event(&mut notif_rx).await, ScheduledTaskCreated);
        assert_eq!(
            (skipped.generation, skipped.revision),
            (old_generation.clone(), u64::MAX)
        );
        let (list_tx, list_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::List { reply: list_tx })
            .unwrap();
        let skipped_version = list_rx.await.unwrap().version;
        assert_eq!(skipped_version.generation(), old_generation);
        assert_eq!(skipped_version.revision(), u64::MAX);

        let SubagentEvent::Query(query2) = next_event(&mut subagent_rx).await else {
            panic!("expected completed query");
        };
        let completed = SubagentSnapshotStatus::Completed {
            output: "ci green".into(),
            tool_calls: 3,
            turns: 1,
            worktree_path: None,
        };
        query2
            .respond_to
            .send(Some(subagent_snapshot(&first_id, completed)))
            .unwrap();
        answer_loop_unit_active(&mut subagent_rx, false).await;
        resources
            .lock()
            .await
            .get_or_default::<State<SchedulerState>>()
            .tasks[0]
            .last_fired_at = Some(Utc::now() - chrono::Duration::seconds(1));
        let second = next_subagent_spawn(&mut subagent_rx).await;
        let fired = notification!(next_event(&mut notif_rx).await, ScheduledTaskFired);
        assert_ne!(fired.generation, old_generation);
        assert_eq!(fired.revision, 1);
        assert_eq!(second.resume_from.as_deref(), Some(first_id.as_str()));
        assert_ne!(second.id, first_id);

        cancel.cancel();
    }

    #[tokio::test]
    async fn prompt_update_resets_chain_interval_update_keeps_it() {
        let (handle, cancel, _notif_rx, mut subagent_rx) = make_test_actor_with_subagents();
        let task_id = create_due_task(&handle, "watch ci", false).await;
        let first = next_subagent_spawn(&mut subagent_rx).await;

        let (up_tx, up_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Update {
                id: task_id.clone(),
                prompt: None,
                interval_secs: Some(3600),
                reply: up_tx,
            })
            .unwrap();
        let updated = up_rx.await.unwrap().unwrap();
        assert_eq!(updated.last_subagent_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(updated.iterations_since_fresh, 1);

        let (up_tx, up_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Update {
                id: task_id,
                prompt: Some("watch deploys instead".into()),
                interval_secs: None,
                reply: up_tx,
            })
            .unwrap();
        let updated = up_rx.await.unwrap().unwrap();
        assert_eq!(updated.last_subagent_id.as_deref(), Some(first.id.as_str()));
        assert!(updated.chain_reset_pending);
        assert_eq!(updated.iterations_since_fresh, 0);

        cancel.cancel();
    }

    #[tokio::test]
    async fn prompt_update_keeps_in_flight_guard_then_spawns_fresh() {
        let (handle, cancel, _notif_rx, mut subagent_rx) = make_test_actor_with_subagents();
        let task_id = create_due_task(&handle, "watch ci", false).await;

        let SubagentEvent::Spawn(first) = next_event(&mut subagent_rx).await else {
            panic!("expected first Spawn");
        };
        let first_id = first.id.clone();

        // Patch the prompt while iteration 1 is still running.
        let (up_tx, up_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Update {
                id: task_id,
                prompt: Some("watch deploys instead".into()),
                interval_secs: None,
                reply: up_tx,
            })
            .unwrap();
        up_rx.await.unwrap().unwrap();

        // Next tick still queries the old iteration; Running must skip.
        let SubagentEvent::Query(q1) = next_event(&mut subagent_rx).await else {
            panic!("expected Query after prompt update");
        };
        assert_eq!(q1.subagent_id, first_id, "guard queries the kept anchor");
        let _ = q1.respond_to.send(Some(
            crate::implementations::echo_agent_build::task::types::SubagentSnapshot {
                subagent_id: first_id.clone(),
                description: "loop: watch ci".into(),
                subagent_type: "general-purpose".into(),
                status: SubagentSnapshotStatus::Running {
                    turn_count: 1,
                    tool_call_count: 1,
                    tokens_used: 0,
                    context_window_tokens: 0,
                    context_usage_pct: 0,
                    tools_used: vec![],
                    error_count: 0,
                },
                started_at_epoch_ms: 0,
                duration_ms: 100,
                persona: None,
            },
        ));

        // Old iteration done: the following tick spawns the NEW job fresh —
        // no resume_from, and the framing carries no old-task output.
        let SubagentEvent::Query(q2) = next_event(&mut subagent_rx).await else {
            panic!("expected second Query, not a Spawn");
        };
        let _ = q2.respond_to.send(Some(
            crate::implementations::echo_agent_build::task::types::SubagentSnapshot {
                subagent_id: first_id.clone(),
                description: "loop: watch ci".into(),
                subagent_type: "general-purpose".into(),
                status: SubagentSnapshotStatus::Completed {
                    output: "old task output".into(),
                    tool_calls: 1,
                    turns: 1,
                    worktree_path: None,
                },
                started_at_epoch_ms: 0,
                duration_ms: 100,
                persona: None,
            },
        ));
        answer_loop_unit_active(&mut subagent_rx, false).await;
        let SubagentEvent::Spawn(second) = next_event(&mut subagent_rx).await else {
            panic!("expected fresh Spawn after old iteration completed");
        };
        assert!(second.resume_from.is_none(), "prompt change spawns fresh");
        assert!(second.prompt.contains("watch deploys instead"));
        assert!(
            !second.prompt.contains("old task output"),
            "old task's output must not leak into the new job"
        );

        cancel.cancel();
    }

    #[tokio::test]
    async fn failed_spawn_resets_chain_state() {
        let (handle, cancel, _notif_rx, mut subagent_rx) = make_test_actor_with_subagents();
        // One-hour interval, backdated to fire exactly once: the assertion
        // window cannot be raced by a second fire re-pointing the anchor.
        let mut task = ScheduledTask::new(3600, "watch ci".into(), true, false);
        task.created_at = chrono::Utc::now() - chrono::Duration::seconds(3610);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Create {
                task,
                reply: reply_tx,
            })
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        let SubagentEvent::Spawn(request) = next_event(&mut subagent_rx).await else {
            panic!("expected Spawn");
        };
        let spawned_id = request.id.clone();
        let _ = request.result_tx.send(
            crate::implementations::echo_agent_build::task::types::SubagentResult {
                success: false,
                error: Some("worktree creation failed".into()),
                subagent_id: spawned_id.clone(),
                ..Default::default()
            },
        );

        // The watcher clears the anchor AND the fresh-chain counter so the
        // next fire starts clean.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let (list_tx, list_rx) = tokio::sync::oneshot::channel();
            handle
                .0
                .send(SchedulerCommand::List { reply: list_tx })
                .unwrap();
            let tasks = list_rx.await.unwrap().tasks;
            if tasks[0].last_subagent_id.is_none() {
                assert_eq!(tasks[0].iterations_since_fresh, 0);
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "watcher did not reset chain state"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        cancel.cancel();
    }

    #[tokio::test]
    async fn spawn_send_failure_restores_pending_chain_reset() {
        let (handle, cancel, _notif_rx, mut subagent_rx) = make_test_actor_with_subagents();
        let task_id = create_due_task(&handle, "watch ci", false).await;

        let SubagentEvent::Spawn(first) = next_event(&mut subagent_rx).await else {
            panic!("expected first Spawn");
        };
        let first_id = first.id.clone();

        let (up_tx, up_rx) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Update {
                id: task_id.clone(),
                prompt: Some("watch deploys instead".into()),
                interval_secs: None,
                reply: up_tx,
            })
            .unwrap();
        up_rx.await.unwrap().unwrap();

        // Coordinator gone: the next fire's guard query and spawn send both
        // fail, taking the rollback path.
        drop(subagent_rx);

        // Wait for the failed fire's rollback to land in state.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let (list_tx, list_rx) = tokio::sync::oneshot::channel();
            handle
                .0
                .send(SchedulerCommand::List { reply: list_tx })
                .unwrap();
            let task = list_rx.await.unwrap().tasks.remove(0);
            // The rollback restores the pre-fire snapshot exactly: old
            // anchor, pre-fire counter, and the still-pending reset.
            if task.last_subagent_id.as_deref() == Some(first_id.as_str())
                && task.chain_reset_pending
                && task.iterations_since_fresh == 0
                && task.last_fired_at.is_some()
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "rollback did not restore the full snapshot: {task:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        cancel.cancel();
    }

    #[tokio::test]
    async fn background_loops_disabled_forces_legacy_path() {
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();
        let (subagent_tx, mut subagent_rx) = mpsc::unbounded_channel();
        resources.insert(SubagentEventSender(subagent_tx));
        resources.insert(
            crate::implementations::echo_agent_build::task::types::SessionIdResource(
                "parent-session".to_string(),
            ),
        );
        resources.insert(crate::types::resources::SchedulerBackgroundLoops(false));
        let shared = Arc::new(Mutex::new(resources));

        let (notif_handle, mut notif_rx) = auto_acknowledged_notifications();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();
        tokio::spawn(
            SchedulerActor {
                resources: shared,
                resources_persistence: Arc::new(crate::persistence::ResourcesPersistence::noop()),
                notification_handle: notif_handle,
                cmd_rx,
                cancel_token: cancel_token.clone(),
                clock: SchedulerClock::new(),
                pending_removal: None,
                blocked_expiries: HashSet::new(),
            }
            .run(),
        );
        let handle = SchedulerHandle(cmd_tx);
        create_due_task(&handle, "watch ci", false).await;

        let mut fired = None;
        for _ in 0..4 {
            let notif = tokio::time::timeout(Duration::from_secs(3), notif_rx.recv())
                .await
                .expect("notification")
                .expect("channel open");
            if let ToolNotification::ScheduledTaskFired(f) = notif {
                fired = Some(f);
                break;
            }
        }
        assert!(
            fired.expect("fired notification").subagent_id.is_none(),
            "disabled config must take the legacy inject path"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), subagent_rx.recv())
                .await
                .is_err(),
            "disabled config must not touch the subagent coordinator"
        );

        cancel_token.cancel();
    }

    #[tokio::test]
    async fn foreground_task_fires_legacy_inject_path() {
        let (handle, cancel, mut notif_rx, mut subagent_rx) = make_test_actor_with_subagents();
        create_due_task(&handle, "needs main context", true).await;

        let mut fired = None;
        for _ in 0..4 {
            let notif = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv())
                .await
                .expect("notification")
                .expect("channel open");
            if let ToolNotification::ScheduledTaskFired(f) = notif {
                fired = Some(f);
                break;
            }
        }
        let fired = fired.expect("fired notification");
        assert!(fired.subagent_id.is_none());

        assert!(
            tokio::time::timeout(Duration::from_millis(200), subagent_rx.recv())
                .await
                .is_err(),
            "foreground fire must not touch the subagent coordinator"
        );

        cancel.cancel();
    }

    #[tokio::test]
    async fn descendant_still_running_skips_fire() {
        let (handle, cancel, _notif_rx, mut subagent_rx, resources) =
            make_test_actor_with_subagents_at(0);
        create_due_task(&handle, "watch ci", false).await;
        resources
            .lock()
            .await
            .get_or_default::<State<SchedulerState>>()
            .tasks[0]
            .last_fired_at = Some(Utc::now() - chrono::Duration::seconds(1));

        let first = next_subagent_spawn(&mut subagent_rx).await;
        let first_id = first.id.clone();
        answer_subagent_query(
            &mut subagent_rx,
            &first_id,
            SubagentSnapshotStatus::Completed {
                output: "ci checked".into(),
                tool_calls: 1,
                turns: 1,
                worktree_path: None,
            },
        )
        .await;
        answer_loop_unit_active(&mut subagent_rx, true).await;
        let next = tokio::time::timeout(Duration::from_millis(300), subagent_rx.recv()).await;
        assert!(!matches!(next, Ok(Some(SubagentEvent::Spawn(_)))));
        cancel.cancel();
    }

    #[tokio::test]
    async fn durable_delete_retries_persistence_and_reuses_version() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("missing");
        let persistence = Arc::new(crate::persistence::ResourcesPersistence::new(
            parent.join("resources_state.json"),
        ));
        let mut task = ScheduledTask::new(300, "retry".into(), true, true);
        task.id = "retry".into();
        let (handle, cancel, mut notifications, resources) =
            make_durable_actor(vec![task], persistence);
        let _ = next_acknowledged(&mut notifications).await;
        let error = delete(&handle, "retry").await.unwrap_err();
        assert_eq!(
            scheduler_tool_error(error).details.unwrap()["code"],
            "scheduler_persistence"
        );
        let (reply, response) = tokio::sync::oneshot::channel();
        handle
            .0
            .send(SchedulerCommand::Create {
                task: ScheduledTask::new(600, "unrelated".into(), true, true),
                reply,
            })
            .unwrap();
        assert!(matches!(
            response.await.unwrap(),
            Err(SchedulerError::RemovalPending(_))
        ));
        resources
            .lock()
            .await
            .get_or_default::<State<WebCitationCounter>>()
            .counter = 7;
        std::fs::create_dir(&parent).unwrap();
        let pending = tokio::spawn({
            let handle = handle.clone();
            async move { delete(&handle, "retry").await }
        });
        let delivery = next_acknowledged(&mut notifications).await;
        let removed = notification!(delivery.notification, ScheduledTaskRemoved);
        assert_eq!(removed.revision, 1);
        delivery.acknowledgement.unwrap().send(Ok(())).unwrap();
        assert!(pending.await.unwrap().unwrap());
        let persisted: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(parent.join("resources_state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            persisted["state"]["echo_agent_build.WebCitation"]["counter"],
            7
        );
        assert_eq!(
            persisted["state"]["echo_agent_build.Scheduler"]["tasks"],
            serde_json::json!([])
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn nondurable_explicit_delete_retries_same_tombstone() {
        let mut task = ScheduledTask::new(300, "retry ack".into(), true, false);
        task.id = "retry-ack".into();
        let (handle, cancel, mut notifications, _) = make_durable_actor(
            vec![task],
            Arc::new(crate::persistence::ResourcesPersistence::noop()),
        );
        let _ = next_acknowledged(&mut notifications).await;
        let first = tokio::spawn({
            let handle = handle.clone();
            async move { delete(&handle, "retry-ack").await }
        });
        let delivery = next_acknowledged(&mut notifications).await;
        let first_removed = notification!(delivery.notification, ScheduledTaskRemoved);
        delivery
            .acknowledgement
            .unwrap()
            .send(Err("updates unavailable".into()))
            .unwrap();
        assert_eq!(
            scheduler_tool_error(first.await.unwrap().unwrap_err())
                .details
                .unwrap()["code"],
            "scheduler_notification"
        );
        let retry = tokio::spawn({
            let handle = handle.clone();
            async move { delete(&handle, "retry-ack").await }
        });
        let delivery = next_acknowledged(&mut notifications).await;
        assert_eq!(
            notification!(delivery.notification, ScheduledTaskRemoved),
            first_removed
        );
        delivery.acknowledgement.unwrap().send(Ok(())).unwrap();
        assert!(retry.await.unwrap().unwrap());
        assert!(!delete(&handle, "retry-ack").await.unwrap());
        cancel.cancel();
    }

    #[tokio::test]
    async fn pending_delete_abandons_closed_durable_target_and_unblocks_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("missing");
        let mut task = ScheduledTask::new(300, "stuck".into(), true, true);
        task.id = "stuck".into();
        let (mut actor, _) = make_boundary_actor(vec![task], 0);
        actor.resources_persistence = Arc::new(crate::persistence::ResourcesPersistence::new(
            parent.join("resources_state.json"),
        ));
        let (plain, mut notifications) = ToolNotificationHandle::channel();
        let (durable, durable_rx) = ToolNotificationHandle::acknowledged_channel();
        actor.notification_handle = ToolNotificationHandle::tee(vec![plain, durable]);
        let version = actor.clock.snapshot();

        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(SchedulerCommand::Delete {
                id: "stuck".into(),
                reply,
            })
            .await;
        assert!(matches!(
            response.await.unwrap(),
            Err(SchedulerError::Persistence(_))
        ));
        assert!(actor.pending_removal.is_some());
        assert!(notifications.try_recv().is_err());

        drop(durable_rx);
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(SchedulerCommand::Delete {
                id: "stuck".into(),
                reply,
            })
            .await;
        assert!(matches!(
            response.await.unwrap(),
            Err(SchedulerError::NoDurableNotificationConsumer)
        ));
        assert!(actor.pending_removal.is_none());
        assert_eq!(actor.clock.snapshot(), version);
        assert!(notifications.try_recv().is_err());

        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(SchedulerCommand::Delete {
                id: "stuck".into(),
                reply,
            })
            .await;
        assert!(!response.await.unwrap().unwrap());

        let mut replacement = ScheduledTask::new(300, "replacement".into(), true, true);
        replacement.id = "replacement".into();
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(SchedulerCommand::Create {
                task: replacement,
                reply,
            })
            .await;
        response.await.unwrap().unwrap();
        let created = notification!(notifications.recv().await.unwrap(), ScheduledTaskCreated);
        assert_eq!(created.revision, 1);

        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(SchedulerCommand::Update {
                id: "replacement".into(),
                prompt: Some("updated".into()),
                interval_secs: None,
                reply,
            })
            .await;
        response.await.unwrap().unwrap();
        let updated = notification!(notifications.recv().await.unwrap(), ScheduledTaskCreated);
        assert_eq!(updated.revision, 2);
        assert!(notifications.try_recv().is_err());
    }

    #[tokio::test]
    async fn explicit_delete_without_live_durable_target_has_no_side_effects() {
        let mut task = ScheduledTask::new(300, "no target".into(), true, false);
        task.id = "no-target".into();
        let (mut actor, _) = make_boundary_actor(vec![task], 0);
        let version = actor.clock.snapshot();
        let (plain, mut notifications) = ToolNotificationHandle::channel();
        let (closed, closed_rx) = ToolNotificationHandle::acknowledged_channel();
        drop(closed_rx);
        actor.notification_handle = ToolNotificationHandle::tee(vec![plain, closed]);
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_command(SchedulerCommand::Delete {
                id: "no-target".into(),
                reply,
            })
            .await;
        assert!(matches!(
            response.await.unwrap(),
            Err(SchedulerError::NoDurableNotificationConsumer)
        ));
        assert!(notifications.try_recv().is_err());
        assert_eq!(actor.clock.snapshot(), version);
        assert!(actor.pending_removal.is_none());
        assert_eq!(
            actor
                .resources
                .lock()
                .await
                .get::<State<SchedulerState>>()
                .unwrap()
                .tasks
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn pending_ack_cancellation_returns_before_actor_stops() {
        let mut task = ScheduledTask::new(300, "cancel".into(), true, true);
        task.id = "cancel".into();
        let (handle, cancel, mut notifications, resources) = make_durable_actor(
            vec![task],
            Arc::new(crate::persistence::ResourcesPersistence::noop()),
        );
        let _ = next_acknowledged(&mut notifications).await;
        let deletion = tokio::spawn({
            let handle = handle.clone();
            async move { delete(&handle, "cancel").await }
        });
        let delivery = next_acknowledged(&mut notifications).await;
        let _acknowledgement = delivery.acknowledgement;
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), deletion)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, Err(SchedulerError::Cancelled)));
        tokio::time::timeout(Duration::from_secs(1), handle.0.closed())
            .await
            .unwrap();
        assert!(
            resources
                .lock()
                .await
                .get::<State<SchedulerState>>()
                .unwrap()
                .tasks
                .is_empty()
        );
    }

    #[tokio::test]
    async fn durable_expiry_persists_before_ack_and_commits_version() {
        let (persistence, mut saves) = crate::persistence::ResourcesPersistence::controlled();
        let mut actor = make_boundary_actor(vec![expired_task("expired", true)], 0).0;
        actor.resources_persistence = Arc::new(persistence);
        let (notification_handle, mut notifications) =
            ToolNotificationHandle::acknowledged_channel();
        actor.notification_handle = notification_handle;

        {
            let expiry = actor.fire_next_task();
            tokio::pin!(expiry);
            let (snapshot, persisted) = tokio::select! {
                _ = expiry.as_mut() => panic!("expiry must wait for resource persistence"),
                save = next_event(&mut saves) => save,
            };
            assert_eq!(
                snapshot["state"]["echo_agent_build.Scheduler"]["tasks"],
                serde_json::json!([])
            );
            assert!(notifications.try_recv().is_err());
            persisted.send(Ok(())).unwrap();
            let delivery = tokio::select! {
                _ = expiry.as_mut() => panic!("expiry must wait for tombstone acknowledgement"),
                delivery = next_acknowledged(&mut notifications) => delivery,
            };
            let removed = notification!(delivery.notification, ScheduledTaskRemoved);
            assert_eq!(removed.revision, 1);
            delivery.acknowledgement.unwrap().send(Ok(())).unwrap();
            tokio::time::timeout(Duration::from_secs(1), expiry.as_mut())
                .await
                .unwrap();
        }
        assert_eq!(actor.clock.snapshot().revision(), 1);
        assert!(
            actor
                .resources
                .lock()
                .await
                .get::<State<SchedulerState>>()
                .unwrap()
                .tasks
                .is_empty()
        );
    }

    #[tokio::test]
    async fn expiry_persistence_failure_restores_and_blocks_while_plain_task_fires() {
        let (persistence, mut saves) = crate::persistence::ResourcesPersistence::controlled();
        let tasks = vec![expired_task("expired", true), due_one_shot("plain")];
        let (mut actor, mut notifications) = make_boundary_actor(tasks, 0);
        actor.resources_persistence = Arc::new(persistence);

        {
            let expiry = actor.fire_next_task();
            tokio::pin!(expiry);
            let (_, persisted) = tokio::select! {
                _ = expiry.as_mut() => panic!("expiry must wait for resource persistence"),
                save = next_event(&mut saves) => save,
            };
            persisted
                .send(Err(std::io::Error::other("disk unavailable")))
                .unwrap();
            tokio::time::timeout(Duration::from_secs(1), expiry.as_mut())
                .await
                .unwrap();
        }
        assert!(actor.blocked_expiries.contains("expired"));
        assert_eq!(actor.clock.snapshot().revision(), 0);
        assert!(notifications.try_recv().is_err());

        actor.fire_next_task().await;
        assert_eq!(
            (
                notification!(notifications.try_recv().unwrap(), ScheduledTaskFired).task_id,
                notification!(notifications.try_recv().unwrap(), ScheduledTaskRemoved).task_id,
            ),
            ("plain".to_string(), "plain".to_string())
        );
        actor.fire_next_task().await;
        assert!(saves.try_recv().is_err());
        assert!(notifications.try_recv().is_err());
    }

    #[tokio::test]
    async fn expiry_without_durable_target_blocks_only_expiry() {
        let tasks = vec![expired_task("expired", true), due_one_shot("plain")];
        let (mut actor, _) = make_boundary_actor(tasks, 0);
        let (notification_handle, mut notifications) = ToolNotificationHandle::channel();
        actor.notification_handle = notification_handle;

        actor.fire_next_task().await;
        assert!(actor.blocked_expiries.contains("expired"));
        assert!(notifications.try_recv().is_err());
        actor.fire_next_task().await;
        assert_eq!(
            (
                notification!(notifications.try_recv().unwrap(), ScheduledTaskFired).task_id,
                notification!(notifications.try_recv().unwrap(), ScheduledTaskRemoved).task_id,
            ),
            ("plain".to_string(), "plain".to_string())
        );
        actor.fire_next_task().await;
        assert!(notifications.try_recv().is_err());
    }

    #[tokio::test]
    async fn expiry_ack_failure_leaves_absent_and_continues_without_version_commit() {
        let tasks = vec![expired_task("expired", true), due_one_shot("plain")];
        let mut actor = make_boundary_actor(tasks, 0).0;
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("resources_state.json");
        actor.resources_persistence = Arc::new(crate::persistence::ResourcesPersistence::new(
            state_path.clone(),
        ));
        let (notification_handle, mut notifications) =
            ToolNotificationHandle::acknowledged_channel();
        actor.notification_handle = notification_handle;

        let removed_revision = {
            let expiry = actor.fire_next_task();
            tokio::pin!(expiry);
            let delivery = tokio::select! {
                _ = expiry.as_mut() => panic!("expiry must wait for tombstone acknowledgement"),
                delivery = next_acknowledged(&mut notifications) => delivery,
            };
            let removed = notification!(delivery.notification, ScheduledTaskRemoved);
            let removed_revision = removed.revision;
            delivery
                .acknowledgement
                .unwrap()
                .send(Err("append failed".into()))
                .unwrap();
            tokio::time::timeout(Duration::from_secs(1), expiry.as_mut())
                .await
                .unwrap();
            removed_revision
        };
        assert_eq!(actor.clock.snapshot().revision(), 0);
        assert!(
            actor
                .resources
                .lock()
                .await
                .get::<State<SchedulerState>>()
                .unwrap()
                .tasks
                .iter()
                .all(|task| task.id != "expired")
        );
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(state_path).unwrap()).unwrap();
        assert!(
            persisted["state"]["echo_agent_build.Scheduler"]["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|task| task["id"] != "expired")
        );

        actor.fire_next_task().await;
        assert_eq!(
            notification!(
                next_acknowledged(&mut notifications).await.notification,
                ScheduledTaskFired
            )
            .task_id,
            "plain"
        );
        let plain_removed = next_acknowledged(&mut notifications).await;
        assert!(plain_removed.acknowledgement.is_none());
        assert_eq!(
            notification!(plain_removed.notification, ScheduledTaskRemoved).revision,
            removed_revision + 1
        );
        actor.fire_next_task().await;
        assert!(notifications.try_recv().is_err());
    }

    #[tokio::test]
    async fn expiry_persistence_cancellation_leaves_absent_and_stops_actor() {
        let (persistence, mut saves) = crate::persistence::ResourcesPersistence::controlled();
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();
        resources.get_or_default::<State<SchedulerState>>().tasks =
            vec![expired_task("expired", true)];
        let shared = Arc::new(Mutex::new(resources));
        let (notification_handle, mut notifications) =
            ToolNotificationHandle::acknowledged_channel();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();
        let actor = SchedulerActor {
            resources: shared.clone(),
            resources_persistence: Arc::new(persistence),
            notification_handle,
            cmd_rx,
            cancel_token: cancel_token.clone(),
            clock: SchedulerClock::new(),
            pending_removal: None,
            blocked_expiries: HashSet::new(),
        };
        let actor_task = tokio::spawn(actor.run());
        let (_, _withheld) = next_event(&mut saves).await;
        let announced = next_acknowledged(&mut notifications).await;
        assert!(announced.acknowledgement.is_none());
        assert!(matches!(
            announced.notification,
            ToolNotification::ScheduledTaskCreated(_)
        ));
        assert!(!actor_task.is_finished());
        cancel_token.cancel();
        tokio::time::timeout(Duration::from_secs(1), actor_task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            shared
                .lock()
                .await
                .get::<State<SchedulerState>>()
                .unwrap()
                .tasks
                .is_empty()
        );
        assert!(notifications.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancel_with_no_tasks_sends_no_removed() {
        let mut resources = Resources::new();
        resources.register_state::<SchedulerState>();

        let shared = Arc::new(Mutex::new(resources));
        let (notif_handle, mut notif_rx) = auto_acknowledged_notifications();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();

        let actor = SchedulerActor {
            resources: shared,
            resources_persistence: Arc::new(crate::persistence::ResourcesPersistence::noop()),
            notification_handle: notif_handle,
            cmd_rx,
            cancel_token: cancel_token.clone(),
            clock: SchedulerClock::new(),
            pending_removal: None,
            blocked_expiries: HashSet::new(),
        };

        let handle = tokio::spawn(actor.run());
        cancel_token.cancel();
        handle.await.expect("actor should complete");

        assert!(
            notif_rx.try_recv().is_err(),
            "no notifications expected when state is empty"
        );
    }
}
