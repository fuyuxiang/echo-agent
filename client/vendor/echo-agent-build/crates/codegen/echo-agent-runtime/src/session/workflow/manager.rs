use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use echo_agent_sampling_types::ReasoningEffort;
use echo_agent_workflow::{Journal, WorkflowOutcome, WorkflowRunParams};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::host_service::{
    HostDrainOutcome, TelemetryHook, WorkflowHostParams, spawn_workflow_host_service,
};
use super::notify::WorkflowNotifySender;
use super::registry::{ResolvedWorkflow, WorkflowSource};
use super::store::WorkflowRunStore;
use super::tracker::WorkflowTracker;

pub(crate) const WORKFLOW_MAX_ACTIVE_RUNS_PER_SESSION: usize = 4;
pub(crate) const WORKFLOW_DEFAULT_AGENT_BUDGET: u64 = echo_agent_workflow::DEFAULT_AGENT_BUDGET;

struct ActiveRun {
    cancel: CancellationToken,
    pause_intent: Arc<AtomicBool>,
    done: oneshot::Receiver<()>,
}

pub(crate) struct LaunchSpec {
    pub objective: String,
    pub args: serde_json::Value,
    pub agent_budget: Option<u64>,
    pub effort: Option<ReasoningEffort>,
    pub resume_run_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum LaunchError {
    #[error("workflow run not found: {0}")]
    UnknownRun(String),
    #[error("journal error: {0}")]
    Journal(String),
    #[error("workflow store error: {0}")]
    Store(String),
    #[error("run is not resumable (status: {0})")]
    NotResumable(String),
    #[error(
        "run is budget-limited at {used} of {limit} agents; resume it \
         with an agent_budget above {used}"
    )]
    BudgetNotRaised { used: u64, limit: u64 },
    #[error(
        "session already has the maximum of {WORKFLOW_MAX_ACTIVE_RUNS_PER_SESSION} active workflow runs"
    )]
    TooManyActiveRuns,
}

pub(crate) struct WorkflowManager {
    session_id: String,
    session_dir: Option<PathBuf>,
    cwd: PathBuf,
    tracker: Arc<parking_lot::Mutex<WorkflowTracker>>,
    store: WorkflowRunStore,
    notify: WorkflowNotifySender,
    subagent_event_tx: mpsc::UnboundedSender<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent,
    >,
    telemetry: TelemetryHook,
    session_cmd_tx: mpsc::UnboundedSender<crate::session::commands::SessionCommand>,
    templates: HashMap<String, String>,
    active: HashMap<String, ActiveRun>,
    retiring: Vec<(String, oneshot::Receiver<()>)>,
    max_concurrent_agents: usize,
}

impl WorkflowManager {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session_id: String,
        session_dir: Option<PathBuf>,
        cwd: PathBuf,
        tracker: Arc<parking_lot::Mutex<WorkflowTracker>>,
        store: WorkflowRunStore,
        notify: WorkflowNotifySender,
        subagent_event_tx: mpsc::UnboundedSender<
            echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent,
        >,
        telemetry: TelemetryHook,
        session_cmd_tx: mpsc::UnboundedSender<crate::session::commands::SessionCommand>,
        templates: HashMap<String, String>,
        max_concurrent_agents: usize,
    ) -> Self {
        Self {
            session_id,
            session_dir,
            cwd,
            tracker,
            store,
            notify,
            subagent_event_tx,
            telemetry,
            session_cmd_tx,
            templates,
            active: HashMap::new(),
            retiring: Vec::new(),
            max_concurrent_agents: super::host_service::workflow_max_concurrent_agents(
                max_concurrent_agents,
            ),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_set_max_concurrent_agents(&mut self, n: usize) {
        self.max_concurrent_agents = n.max(1);
    }

    pub(crate) fn tracker(&self) -> Arc<parking_lot::Mutex<WorkflowTracker>> {
        self.tracker.clone()
    }

    pub(crate) fn launch(
        &mut self,
        resolved: ResolvedWorkflow,
        mut spec: LaunchSpec,
    ) -> Result<(String, oneshot::Receiver<WorkflowOutcome>), LaunchError> {
        self.reap_terminal_runs();
        if self.active.len().saturating_add(self.retiring.len())
            >= WORKFLOW_MAX_ACTIVE_RUNS_PER_SESSION
        {
            return Err(LaunchError::TooManyActiveRuns);
        }

        let allow_fork_context = resolved.source == WorkflowSource::Builtin;
        let mut execution_script = resolved.script;
        let (run_id, journal, state) = match &spec.resume_run_id {
            Some(run_id) => {
                let existing = self
                    .tracker
                    .lock()
                    .get(run_id)
                    .ok_or_else(|| LaunchError::UnknownRun(run_id.clone()))?;
                if !existing.status.is_resumable() {
                    return Err(LaunchError::NotResumable(
                        existing.status.as_str().to_string(),
                    ));
                }
                if existing.status
                    == crate::session::workflow::tracker::WorkflowRunStatus::BudgetLimited
                    && existing.agents_used >= echo_agent_workflow::MAX_AGENT_BUDGET
                {
                    return Err(LaunchError::NotResumable(
                        "maximum agent budget reached; start a new run".into(),
                    ));
                }
                let original_args = self.store.args_for(run_id).ok_or_else(|| {
                    LaunchError::Store("immutable launch args are missing".into())
                })?;
                spec.effort = self.store.effort_for(run_id);
                if original_args != spec.args {
                    return Err(LaunchError::Store(
                        "workflow launch args are immutable across resume".into(),
                    ));
                }
                execution_script = self.store.script_for(run_id).ok_or_else(|| {
                    LaunchError::Store("immutable workflow script is missing".into())
                })?;
                let mut journal = match existing
                    .journal_path
                    .as_ref()
                    .and_then(|p| self.session_dir.as_ref().map(|d| (d, p)))
                {
                    Some((session_dir, relative)) => {
                        let expected = format!("workflows/{run_id}/journal.jsonl");
                        if relative != &expected {
                            return Err(LaunchError::Journal(
                                "persisted journal path does not match its workflow run".into(),
                            ));
                        }
                        Journal::load(session_dir.join(relative))
                            .map_err(|e| LaunchError::Journal(e.to_string()))?
                    }
                    None => Journal::new(None),
                };
                if existing.status == crate::session::workflow::tracker::WorkflowRunStatus::Failed {
                    journal
                        .prune_trailing_host_error(existing.pause_message.as_deref().unwrap_or(""))
                        .map_err(|e| LaunchError::Journal(e.to_string()))?;
                }
                let state = {
                    let mut tracker = self.tracker.lock();
                    tracker.reconcile_agents_used(run_id, journal.agent_reservation_count());
                    tracker.resume_run(run_id, spec.agent_budget)
                }
                .ok_or_else(|| {
                    if existing.status
                        == crate::session::workflow::tracker::WorkflowRunStatus::BudgetLimited
                    {
                        LaunchError::BudgetNotRaised {
                            used: existing.agents_used,
                            limit: existing.agent_budget.unwrap_or(0),
                        }
                    } else {
                        LaunchError::NotResumable(existing.status.as_str().into())
                    }
                })?;

                (run_id.clone(), journal, state)
            }
            None => {
                let run_id = format!("wf_{}", uuid::Uuid::now_v7().simple());
                let agent_budget = spec.agent_budget.unwrap_or(WORKFLOW_DEFAULT_AGENT_BUDGET);
                self.store
                    .register(&run_id, &execution_script, &spec.args, spec.effort)
                    .map_err(|error| LaunchError::Store(error.to_string()))?;
                let journal_rel = format!("workflows/{run_id}/journal.jsonl");
                let journal_path = self.session_dir.as_ref().map(|d| d.join(&journal_rel));
                let journal = Journal::new(journal_path);
                let state = self.tracker.lock().start_run(
                    run_id.clone(),
                    resolved.meta.name,
                    spec.objective.clone(),
                    resolved.meta.phases,
                    Some(agent_budget),
                    self.session_dir.as_ref().map(|_| journal_rel),
                );
                (run_id, journal, state)
            }
        };

        if let Err(error) = self.store.persist_now(&state) {
            if spec.resume_run_id.is_some() {
                if let Some(interrupted) = self.tracker.lock().interrupt(
                    &run_id,
                    "workflow state persistence failed before resume; start a new run",
                ) && let Err(persist_error) = self.store.persist(&interrupted)
                {
                    tracing::warn!(run_id = %run_id, %persist_error, "failed to queue interrupted workflow state");
                }
            } else {
                self.tracker.lock().clear_run(&run_id);
                self.store.remove(&run_id);
            }
            return Err(LaunchError::Store(error.to_string()));
        }
        self.notify
            .emit(&state, self.tracker.lock().elapsed_ms(&run_id), 0);

        let active = echo_agent_telemetry::activity::WORKFLOW_RUNS_ACTIVE.enter();
        debug_assert!(
            echo_agent_telemetry::activity::WORKFLOW_RUNS_ACTIVE.get() >= 1,
            "WorkflowRunStarted must stamp a self-inclusive count"
        );
        log_run_started(
            &run_id,
            &self.session_id,
            &resolved.source,
            &state,
            self.max_concurrent_agents,
            spec.resume_run_id.is_some(),
        );

        let (host_tx, host_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let scratch_dir = self
            .session_dir
            .clone()
            .unwrap_or_else(std::env::temp_dir)
            .join("workflows")
            .join(&run_id)
            .join("scratch");

        let agent_stats = Arc::new(super::host_service::WorkflowAgentStats::default());
        let (host_service, host_drained) = spawn_workflow_host_service(
            WorkflowHostParams {
                run_id: run_id.clone(),
                max_concurrent_agents: self.max_concurrent_agents,
                cwd: self.cwd.clone(),
                scratch_dir,
                tracker: self.tracker.clone(),
                store: self.store.clone(),
                notify: self.notify.clone(),
                subagent_event_tx: self.subagent_event_tx.clone(),
                parent_session_id: self.session_id.clone(),
                allow_fork_context,
                effort: spec.effort,
                templates: self.templates.clone(),
                telemetry: self.telemetry.clone(),
                stats: agent_stats.clone(),
                cancel: cancel.clone(),
            },
            host_rx,
        );

        let script = execution_script;
        let args = spec.args;
        let exec_cancel = cancel.clone();
        let exec = tokio::task::spawn_blocking(move || {
            echo_agent_workflow::run_workflow(WorkflowRunParams {
                script,
                args,
                journal,
                host_tx,
                cancel: exec_cancel,
                max_ops: WorkflowRunParams::DEFAULT_MAX_OPS,
            })
        });

        let pause_intent = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = oneshot::channel();
        self.active.insert(
            run_id.clone(),
            ActiveRun {
                cancel: cancel.clone(),
                pause_intent: pause_intent.clone(),
                done: done_rx,
            },
        );

        let (outcome_tx, outcome_rx) = oneshot::channel();
        let tracker = self.tracker.clone();
        let store = self.store.clone();
        let notify = self.notify.clone();
        let session_cmd_tx = self.session_cmd_tx.clone();
        let watcher_run_id = run_id.clone();
        let watcher_cancel = cancel.clone();
        let watcher_session_id = self.session_id.clone();
        let watcher_agent_stats = agent_stats;
        let execution_epoch = self.tracker.lock().execution_epoch(&run_id).unwrap_or(0);
        tokio::spawn(async move {
            let _active = active;
            let mut outcome = exec.await.unwrap_or_else(|e| WorkflowOutcome::Failed {
                error: format!("workflow executor panicked: {e}"),
            });
            if !host_service.is_finished() {
                watcher_cancel.cancel();
            }
            let host_drain =
                tokio::time::timeout(std::time::Duration::from_secs(25), host_drained).await;
            let drain_failed = !matches!(host_drain, Ok(Ok(HostDrainOutcome::Drained)));
            if drain_failed {
                host_service.abort();
            }
            let _ = host_service.await;
            if drain_failed {
                tracing::warn!(run_id = %watcher_run_id, "workflow host/child drain did not complete before lifecycle update");
                outcome = WorkflowOutcome::Failed {
                    error:
                        "workflow cleanup did not complete; run is interrupted and cannot resume"
                            .into(),
                };
            }
            // Epoch check and lifecycle mutation stay under one lock, or a
            // quick resume could let this stale watcher stomp the successor.
            let (epoch_matches, state) = {
                let mut tracker = tracker.lock();
                if tracker.execution_epoch(&watcher_run_id) != Some(execution_epoch) {
                    (false, None)
                } else if drain_failed {
                    (
                        true,
                        tracker.interrupt(
                            &watcher_run_id,
                            "workflow cleanup timed out or could not be acknowledged; start a \
                             new run",
                        ),
                    )
                } else if pause_intent.load(Ordering::Relaxed)
                    && matches!(
                        outcome,
                        WorkflowOutcome::Cancelled | WorkflowOutcome::Paused { .. }
                    )
                {
                    (true, tracker.pause_user(&watcher_run_id, None))
                } else {
                    (true, tracker.apply_outcome(&watcher_run_id, &outcome))
                }
            };
            if !epoch_matches {
                // A quick resume took over; close this episode as superseded
                // (cumulative fields may reflect the successor).
                let (elapsed, agents_used, agent_budget) = {
                    let tracker = tracker.lock();
                    let run = tracker.get(&watcher_run_id);
                    (
                        tracker.elapsed_ms(&watcher_run_id),
                        run.as_ref().map(|run| run.agents_used).unwrap_or_default(),
                        run.as_ref().and_then(|run| run.agent_budget),
                    )
                };
                log_run_ended(
                    RunEndMetadata {
                        run_id: &watcher_run_id,
                        parent_session_id: &watcher_session_id,
                        status: echo_agent_telemetry::events::WorkflowRunEndStatus::Superseded,
                        duration_ms: elapsed,
                        agents_used,
                        agent_budget,
                    },
                    &watcher_agent_stats,
                );
                let _ = done_tx.send(());
                let _ = outcome_tx.send(outcome);
                return;
            }
            if state.is_none() {
                // The run left the tracker; close the episode so its
                // `workflow_run_started` is not orphaned.
                log_run_ended(
                    RunEndMetadata {
                        run_id: &watcher_run_id,
                        parent_session_id: &watcher_session_id,
                        status: echo_agent_telemetry::events::WorkflowRunEndStatus::Interrupted,
                        duration_ms: 0,
                        agents_used: 0,
                        agent_budget: None,
                    },
                    &watcher_agent_stats,
                );
            }
            if let Some(mut state) = state {
                let mut persisted = true;
                if let Err(error) = store.persist_ack(&state).await {
                    tracing::warn!(run_id = %watcher_run_id, %error, "workflow terminal manifest was not durably written");
                    outcome = WorkflowOutcome::Failed {
                        error: format!(
                            "workflow terminal state could not be persisted: {error}; run is interrupted"
                        ),
                    };
                    state = tracker
                        .lock()
                        .interrupt(
                            &watcher_run_id,
                            format!(
                                "workflow terminal state could not be persisted: {error}; start a new run"
                            ),
                        )
                        .unwrap_or(state);
                    if let Err(interrupt_error) = store.persist_ack(&state).await {
                        persisted = false;
                        tracing::error!(run_id = %watcher_run_id, %interrupt_error, "failed to persist workflow interruption marker");
                    }
                }
                // Emit before the persist-failure return: starts always pair.
                let elapsed = tracker.lock().elapsed_ms(&watcher_run_id);
                log_run_ended(
                    RunEndMetadata {
                        run_id: &watcher_run_id,
                        parent_session_id: &watcher_session_id,
                        status: run_ended_status(state.status),
                        duration_ms: elapsed,
                        agents_used: state.agents_used,
                        agent_budget: state.agent_budget,
                    },
                    &watcher_agent_stats,
                );
                if !persisted {
                    let _ = done_tx.send(());
                    let _ = outcome_tx.send(outcome);
                    return;
                }
                notify.broadcast(&state, elapsed, 0, true);
                if state.status.is_completion_reportable() {
                    let _ = session_cmd_tx.send(
                        crate::session::commands::SessionCommand::WorkflowCompletionTurn {
                            run_id: watcher_run_id.clone(),
                            revision: state.revision,
                        },
                    );
                }
            }
            let _ = done_tx.send(());
            let _ = outcome_tx.send(outcome);
        });

        Ok((run_id, outcome_rx))
    }

    #[cfg(test)]
    pub(crate) fn test_bundle() -> (
        Arc<tokio::sync::Mutex<WorkflowManager>>,
        Arc<parking_lot::Mutex<WorkflowTracker>>,
    ) {
        Self::test_bundle_with_session_dir(None)
    }

    #[cfg(test)]
    pub(crate) fn test_bundle_with_session_dir(
        session_dir: Option<PathBuf>,
    ) -> (
        Arc<tokio::sync::Mutex<WorkflowManager>>,
        Arc<parking_lot::Mutex<WorkflowTracker>>,
    ) {
        let tracker = Arc::new(parking_lot::Mutex::new(WorkflowTracker::default()));
        let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
        let (persist_tx, _persist_rx) = mpsc::unbounded_channel();
        let store = WorkflowRunStore::new(session_dir.clone(), persist_tx.clone());
        let notify = super::notify::WorkflowNotifySender::new(
            agent_client_protocol::SessionId::new("test-session"),
            echo_agent_acp::AcpAgentGatewaySender::new(gateway_tx),
            persist_tx,
            store.clone(),
        );
        let manager = Arc::new(tokio::sync::Mutex::new(WorkflowManager::new(
            "test-session".into(),
            session_dir,
            std::env::temp_dir(),
            tracker.clone(),
            store,
            notify,
            mpsc::unbounded_channel().0,
            Arc::new(|_, _, _| {}),
            mpsc::unbounded_channel().0,
            std::collections::HashMap::new(),
            super::host_service::DEFAULT_WORKFLOW_MAX_CONCURRENT_AGENTS,
        )));
        (manager, tracker)
    }

    fn reap_terminal_runs(&mut self) {
        let terminal: Vec<String> = self
            .active
            .keys()
            .filter(|run_id| {
                self.tracker
                    .lock()
                    .get(run_id)
                    .is_some_and(|state| state.status.is_terminal())
            })
            .cloned()
            .collect();
        for run_id in terminal {
            if let Some(run) = self.active.remove(&run_id) {
                self.retiring.push((run_id.to_owned(), run.done));
            }
        }

        self.retiring.retain_mut(|(_, done)| match done.try_recv() {
            Ok(()) | Err(oneshot::error::TryRecvError::Closed) => false,
            Err(oneshot::error::TryRecvError::Empty) => true,
        });
    }

    fn reap_if_terminal(&mut self, run_id: &str) -> bool {
        let terminal = self
            .tracker
            .lock()
            .get(run_id)
            .is_some_and(|s| s.status.is_terminal());
        if terminal && let Some(run) = self.active.remove(run_id) {
            self.retiring.push((run_id.to_owned(), run.done));
        }
        terminal
    }

    fn cancel_children_for_run(&self, run_id: &str) -> bool {
        let (respond_to, _response) = oneshot::channel();
        self.subagent_event_tx
            .send(
                echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent::Cancel(
                    echo_agent_tools::implementations::echo_agent_build::task::types::SubagentCancelRequest {
                        parent_session_id: Some(self.session_id.clone()),
                        target: echo_agent_tools::implementations::echo_agent_build::task::types::SubagentCancelTarget::WorkflowRunId(
                            run_id.to_owned(),
                        ),
                        respond_to,
                    },
                ),
            )
            .is_ok()
    }

    pub(crate) fn pause(&mut self, run_id: &str) -> bool {
        if self.reap_if_terminal(run_id) {
            return false;
        }
        let Some(run) = self.active.remove(run_id) else {
            return false;
        };
        run.pause_intent.store(true, Ordering::Relaxed);
        run.cancel.cancel();
        let _ = self.cancel_children_for_run(run_id);
        self.retiring.push((run_id.to_owned(), run.done));
        let paused = {
            let mut tracker = self.tracker.lock();
            let active = tracker
                .get(run_id)
                .is_some_and(|state| !state.status.is_terminal());
            if active {
                let state = tracker.pause_user(run_id, None);
                state.map(|state| (state, tracker.elapsed_ms(run_id)))
            } else {
                None
            }
        };
        if let Some((state, elapsed)) = paused {
            self.notify.emit(&state, elapsed, 0);
        }
        true
    }

    pub(crate) fn cancel(&mut self, run_id: &str) -> bool {
        if self.reap_if_terminal(run_id) {
            return false;
        }
        if let Some(run) = self.active.remove(run_id) {
            run.cancel.cancel();
            let _ = self.cancel_children_for_run(run_id);
            self.retiring.push((run_id.to_owned(), run.done));
            let state = {
                let mut tracker = self.tracker.lock();
                match tracker.get(run_id) {
                    Some(state) if !state.status.is_terminal() => {
                        tracker.apply_outcome(run_id, &WorkflowOutcome::Cancelled)
                    }
                    _ => None,
                }
            };
            if state.is_some() {
                let (state, elapsed) = {
                    let tracker = self.tracker.lock();
                    (
                        tracker.get(run_id).expect("run still tracked"),
                        tracker.elapsed_ms(run_id),
                    )
                };
                self.notify.emit(&state, elapsed, 0);
            }
            return true;
        }
        let _ = self.cancel_children_for_run(run_id);
        let state = {
            let mut tracker = self.tracker.lock();
            match tracker.get(run_id) {
                Some(state) if !state.status.is_terminal() => {
                    tracker.apply_outcome(run_id, &WorkflowOutcome::Cancelled)
                }
                _ => None,
            }
        };
        match state {
            Some(_) => {
                let (state, elapsed) = {
                    let tracker = self.tracker.lock();
                    (
                        tracker.get(run_id).expect("run still tracked"),
                        tracker.elapsed_ms(run_id),
                    )
                };
                self.notify.emit(&state, elapsed, 0);
                true
            }
            None => false,
        }
    }

    pub(crate) fn has_live_runs(&mut self) -> bool {
        self.reap_terminal_runs();
        !self.active.is_empty() || !self.retiring.is_empty()
    }

    pub(crate) async fn cancel_all_and_drain(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<(), Vec<String>> {
        let active: Vec<(String, ActiveRun)> = self.active.drain().collect();
        for (run_id, run) in &active {
            run.cancel.cancel();
            let _ = self.cancel_children_for_run(run_id);
        }
        let mut pending: Vec<(Option<String>, oneshot::Receiver<()>)> = active
            .into_iter()
            .map(|(run_id, run)| (Some(run_id), run.done))
            .collect();
        pending.extend(
            self.retiring
                .drain(..)
                .map(|(run_id, done)| (Some(run_id), done)),
        );

        let mut timed_out = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;
        let mut pending = pending.into_iter();
        while let Some((run_id, done)) = pending.next() {
            if tokio::time::timeout_at(deadline, done).await.is_err() {
                if let Some(run_id) = run_id {
                    timed_out.push(run_id);
                }
                timed_out.extend(pending.filter_map(|(run_id, _)| run_id));
                break;
            }
        }
        if timed_out.is_empty() {
            return Ok(());
        }

        tracing::warn!(
            run_ids = ?timed_out,
            "workflow shutdown drain timed out; marking runs interrupted"
        );
        for run_id in &timed_out {
            if self
                .tracker
                .lock()
                .get(run_id)
                .is_some_and(|s| s.status.is_terminal() || s.status.is_paused())
            {
                continue;
            }
            let state = {
                let mut tracker = self.tracker.lock();
                tracker.interrupt(
                    run_id,
                    "session shutdown timed out before workflow cleanup completed; the run cannot resume",
                )
            };
            if let Some(state) = state {
                if let Err(error) = self.store.persist_now(&state) {
                    tracing::error!(%run_id, %error, "failed to persist workflow shutdown interruption");
                }
                let elapsed = self.tracker.lock().elapsed_ms(run_id);
                self.notify.broadcast(&state, elapsed, 0, true);
            }
        }
        Err(timed_out)
    }

    #[cfg(test)]
    pub(crate) fn test_insert_active_run(&mut self, run_id: String, done: oneshot::Receiver<()>) {
        self.active.insert(
            run_id,
            ActiveRun {
                cancel: CancellationToken::new(),
                pause_intent: Arc::new(AtomicBool::new(false)),
                done,
            },
        );
    }

    pub(crate) fn script_copy_for(&self, run_id: &str) -> Option<String> {
        self.store.script_for(run_id)
    }

    pub(crate) fn script_copy_path(&self, run_id: &str) -> Option<std::path::PathBuf> {
        self.store.script_copy_path(run_id)
    }

    pub(crate) fn args_copy_for(&self, run_id: &str) -> serde_json::Value {
        self.store
            .args_for(run_id)
            .unwrap_or(serde_json::Value::Null)
    }
}

fn log_run_started(
    run_id: &str,
    parent_session_id: &str,
    source: &WorkflowSource,
    state: &crate::session::workflow::tracker::WorkflowRunState,
    max_concurrent_agents: usize,
    resumed: bool,
) {
    use echo_agent_telemetry::events::{WorkflowRunStarted, WorkflowSourceKind};
    echo_agent_telemetry::session_ctx::log_event(WorkflowRunStarted {
        run_id: run_id.to_owned(),
        parent_session_id: parent_session_id.to_owned(),
        source: match source {
            WorkflowSource::Builtin => WorkflowSourceKind::Builtin,
            WorkflowSource::Inline => WorkflowSourceKind::Inline,
            WorkflowSource::File(_) => WorkflowSourceKind::File,
        },
        // Only built-in workflow names leave the machine; user script names
        // and paths stay local.
        workflow_name: (*source == WorkflowSource::Builtin).then(|| state.name.clone()),
        agent_budget: state.agent_budget,
        max_concurrent_agents: u32::try_from(max_concurrent_agents).unwrap_or(u32::MAX),
        resumed,
    });
}

struct RunEndMetadata<'a> {
    run_id: &'a str,
    parent_session_id: &'a str,
    status: echo_agent_telemetry::events::WorkflowRunEndStatus,
    duration_ms: u64,
    agents_used: u64,
    agent_budget: Option<u64>,
}

fn log_run_ended(episode: RunEndMetadata<'_>, stats: &super::host_service::WorkflowAgentStats) {
    echo_agent_telemetry::session_ctx::log_event(echo_agent_telemetry::events::WorkflowRunEnded {
        run_id: episode.run_id.to_owned(),
        parent_session_id: episode.parent_session_id.to_owned(),
        status: episode.status,
        duration_ms: episode.duration_ms,
        agents_used: episode.agents_used,
        agent_budget: episode.agent_budget,
        agents_failed: stats.agents_failed.load(Ordering::Relaxed),
        peak_concurrent_agents: stats.peak_concurrent.load(Ordering::Relaxed),
        slot_waits: stats.slot_waits.load(Ordering::Relaxed),
        slot_wait_ms_total: stats.slot_wait_ms_total.load(Ordering::Relaxed),
        slot_wait_ms_max: stats.slot_wait_ms_max.load(Ordering::Relaxed),
    });
}

/// Exhaustive so a new tracker status forces a decision here.
fn run_ended_status(
    status: crate::session::workflow::tracker::WorkflowRunStatus,
) -> echo_agent_telemetry::events::WorkflowRunEndStatus {
    use crate::session::workflow::tracker::WorkflowRunStatus as S;
    use echo_agent_telemetry::events::WorkflowRunEndStatus as E;
    match status {
        S::Active => E::Active,
        S::UserPaused => E::UserPaused,
        S::BackOffPaused => E::BackOffPaused,
        S::NoProgressPaused => E::NoProgressPaused,
        S::InfraPaused => E::InfraPaused,
        S::Blocked => E::Blocked,
        S::BudgetLimited => E::BudgetLimited,
        S::Interrupted => E::Interrupted,
        S::Complete => E::Complete,
        S::Failed => E::Failed,
        S::Cancelled => E::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::persistence::PersistenceMsg;
    use crate::session::workflow::registry::resolve_inline;

    type SubagentEventRx = mpsc::UnboundedReceiver<
        echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent,
    >;
    type CancelLog = Arc<
        parking_lot::Mutex<
            Vec<echo_agent_tools::implementations::echo_agent_build::task::types::SubagentCancelTarget>,
        >,
    >;

    fn test_manager(session_dir: Option<PathBuf>) -> (WorkflowManager, SubagentEventRx) {
        let (manager, events, _cancels) = test_manager_with_cancels(session_dir);
        (manager, events)
    }

    fn test_manager_with_cancels(
        session_dir: Option<PathBuf>,
    ) -> (WorkflowManager, SubagentEventRx, CancelLog) {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentCancelOutcome, SubagentEvent,
        };

        let (subagent_tx, mut raw_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let cancels: CancelLog = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let cancels_stub = cancels.clone();
        tokio::spawn(async move {
            while let Some(event) = raw_rx.recv().await {
                match event {
                    SubagentEvent::Cancel(request) => {
                        cancels_stub.lock().push(request.target.clone());
                        let _ = request.respond_to.send(SubagentCancelOutcome::Cancelled);
                    }
                    other => {
                        if event_tx.send(other).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let (persist_tx, mut persist_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(message) = persist_rx.recv().await {
                if let PersistenceMsg::WorkflowRunStateAndAck { respond_to, .. } = message {
                    let _ = respond_to.send(Ok(()));
                }
            }
        });
        let (gateway_tx, _gateway_rx) = mpsc::unbounded_channel();
        let store = WorkflowRunStore::new(session_dir.clone(), persist_tx.clone());
        let notify = WorkflowNotifySender::new(
            agent_client_protocol::SessionId::new("test-session"),
            echo_agent_acp::AcpAgentGatewaySender::new(gateway_tx),
            persist_tx,
            store.clone(),
        );
        let tracker = Arc::new(parking_lot::Mutex::new(WorkflowTracker::default()));
        let manager = WorkflowManager::new(
            "test-session".into(),
            session_dir,
            std::env::temp_dir(),
            tracker,
            store,
            notify,
            subagent_tx,
            Arc::new(|_, _, _| {}),
            mpsc::unbounded_channel().0,
            HashMap::new(),
            crate::session::workflow::host_service::DEFAULT_WORKFLOW_MAX_CONCURRENT_AGENTS,
        );
        (manager, event_rx, cancels)
    }

    fn spec() -> LaunchSpec {
        LaunchSpec {
            objective: "obj".into(),
            args: serde_json::json!({}),
            agent_budget: None,
            effort: None,
            resume_run_id: None,
        }
    }

    fn parallel_n_script(n: usize) -> String {
        format!(
            "let meta = #{{ name: \"t\", description: \"d\" }};\n\
             let jobs = [];\n\
             let i = 0;\n\
             while i < {n} {{\n\
                 jobs.push(#{{ prompt: \"work \" + i.to_string() }});\n\
                 i += 1;\n\
             }}\n\
             let results = parallel(jobs);\n\
             complete(results.len());"
        )
    }

    async fn recv_spawn(
        rx: &mut SubagentEventRx,
    ) -> echo_agent_tools::implementations::echo_agent_build::task::types::SubagentSpawnRequest
    {
        use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent;
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            Ok(Some(SubagentEvent::Spawn(req))) => req,
            Ok(Some(_)) => panic!("expected spawn, got a non-spawn event"),
            Ok(None) => panic!("expected spawn, channel closed"),
            Err(_) => panic!("expected spawn, timed out"),
        }
    }

    fn complete_spawn(
        req: echo_agent_tools::implementations::echo_agent_build::task::types::SubagentSpawnRequest,
    ) {
        use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentResult;
        let id = req.id.clone();
        let _ = req.result_tx.send(SubagentResult {
            success: true,
            output: std::sync::Arc::from("ok"),
            subagent_id: id.clone(),
            child_session_id: id,
            ..Default::default()
        });
    }

    #[tokio::test]
    async fn launch_completes_and_updates_tracker() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, _rx) = test_manager(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\ncomplete(\"done\");".into(),
        )
        .unwrap();
        let (run_id, outcome_rx) = manager.launch(resolved, spec()).unwrap();

        let outcome = outcome_rx.await.unwrap();
        assert!(matches!(outcome, WorkflowOutcome::Completed { .. }));
        let state = manager.tracker.lock().get(&run_id).unwrap();
        assert_eq!(
            state.status,
            crate::session::workflow::tracker::WorkflowRunStatus::Complete
        );
        assert_eq!(state.result_summary.as_deref(), Some("done"));
        assert!(
            dir.path()
                .join("workflows")
                .join(&run_id)
                .join("script.rhai")
                .exists()
        );
    }

    #[tokio::test]
    async fn plain_resume_uses_immutable_script_not_edited_projection() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, _rx) = test_manager(Some(dir.path().to_path_buf()));
        let script = "let meta = #{ name: \"t\", description: \"d\" };\nawait_user(\"user\", \"pause\");\ncomplete(\"original\");";
        let (run_id, outcome_rx) = manager
            .launch(resolve_inline(script.into()).unwrap(), spec())
            .unwrap();
        assert!(matches!(
            outcome_rx.await.unwrap(),
            WorkflowOutcome::Paused { .. }
        ));
        std::fs::write(
            manager.script_copy_path(&run_id).unwrap(),
            "let meta = #{ name: \"t\", description: \"d\" };\ncomplete(\"edited\");",
        )
        .unwrap();

        let (_same_id, outcome_rx) = manager
            .launch(
                resolve_inline(
                    "let meta = #{ name: \"t\", description: \"d\" };\ncomplete(\"caller copy\");"
                        .into(),
                )
                .unwrap(),
                LaunchSpec {
                    resume_run_id: Some(run_id),
                    ..spec()
                },
            )
            .unwrap();
        match outcome_rx.await.unwrap() {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!("original"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_reuses_immutable_launch_effort() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent;

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let script = "let meta = #{ name: \"t\", description: \"d\" };\n\
                      await_user(\"user\", \"pause\");\n\
                      let r = agent(\"after resume\");\n\
                      complete(r.output);";
        let (run_id, first_outcome) = manager
            .launch(
                resolve_inline(script.into()).unwrap(),
                LaunchSpec {
                    effort: Some(ReasoningEffort::High),
                    ..spec()
                },
            )
            .unwrap();
        assert!(matches!(
            first_outcome.await.unwrap(),
            WorkflowOutcome::Paused { .. }
        ));

        let (_same_id, resumed_outcome) = manager
            .launch(
                resolve_inline(script.into()).unwrap(),
                LaunchSpec {
                    effort: None,
                    resume_run_id: Some(run_id),
                    ..spec()
                },
            )
            .unwrap();
        let SubagentEvent::Spawn(req) = subagent_rx.recv().await.expect("resumed spawn") else {
            panic!("expected resumed spawn event");
        };
        assert_eq!(
            req.runtime_overrides.reasoning_effort.as_deref(),
            Some("high")
        );
        complete_spawn(req);
        assert!(matches!(
            resumed_outcome.await.unwrap(),
            WorkflowOutcome::Completed { .. }
        ));
    }

    #[tokio::test]
    async fn pause_eagerly_marks_user_paused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, _rx) = test_manager(Some(dir.path().to_path_buf()));
        let run_id = "wf_pause_eager".to_string();
        manager
            .store
            .register(
                &run_id,
                "let meta = #{ name: \"t\", description: \"d\" };",
                &serde_json::json!({}),
                None,
            )
            .unwrap();
        manager.tracker.lock().start_run(
            run_id.clone(),
            "t".into(),
            "obj".into(),
            Vec::new(),
            None,
            None,
        );
        let (_done_tx, done_rx) = oneshot::channel();
        manager.test_insert_active_run(run_id.clone(), done_rx);

        assert_eq!(
            manager.tracker.lock().get(&run_id).unwrap().status,
            crate::session::workflow::tracker::WorkflowRunStatus::Active,
        );
        assert!(manager.pause(&run_id));
        let state = manager.tracker.lock().get(&run_id).unwrap();
        assert_eq!(
            state.status,
            crate::session::workflow::tracker::WorkflowRunStatus::UserPaused,
            "pause() must eagerly mark UserPaused so status is not still Active"
        );
        assert!(!manager.active.contains_key(&run_id));
    }

    #[tokio::test]
    async fn pause_marks_user_paused_and_resume_replays() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let script = "let meta = #{ name: \"t\", description: \"d\" };\nlet r = agent(\"work\");\ncomplete(r.output);";
        let resolved = resolve_inline(script.into()).unwrap();
        let (run_id, outcome_rx) = manager.launch(resolved, spec()).unwrap();

        use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent;
        let spawn_req = subagent_rx.recv().await.expect("spawn request");
        let SubagentEvent::Spawn(_spawn) = spawn_req else {
            panic!("expected spawn request");
        };
        assert!(manager.pause(&run_id));
        assert_eq!(
            manager.tracker.lock().get(&run_id).unwrap().status,
            crate::session::workflow::tracker::WorkflowRunStatus::UserPaused,
            "pause() must mark UserPaused immediately"
        );
        let outcome = outcome_rx.await.unwrap();
        assert!(matches!(outcome, WorkflowOutcome::Cancelled));
        let state = manager.tracker.lock().get(&run_id).unwrap();
        assert_eq!(
            state.status,
            crate::session::workflow::tracker::WorkflowRunStatus::UserPaused,
            "pause intent must map Cancelled → UserPaused"
        );

        let resolved = resolve_inline(script.into()).unwrap();
        let (_run_id2, outcome_rx) = manager
            .launch(
                resolved,
                LaunchSpec {
                    resume_run_id: Some(run_id.clone()),
                    ..spec()
                },
            )
            .unwrap();
        let spawn_req = subagent_rx.recv().await.expect("respawned agent");
        use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentResult;
        if let SubagentEvent::Spawn(req) = spawn_req {
            let id = req.id.clone();
            let _ = req.result_tx.send(SubagentResult {
                success: true,
                output: std::sync::Arc::from("resumed output"),
                subagent_id: id,
                ..Default::default()
            });
        } else {
            panic!("expected spawn event");
        }
        let outcome = outcome_rx.await.unwrap();
        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!("resumed output"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_reconciles_agents_used_from_journal_no_double_charge() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentResult,
        };

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let script = "let meta = #{ name: \"t\", description: \"d\" };\nlet r = agent(\"work\");\ncomplete(r.output);";
        let (run_id, outcome_rx) = manager
            .launch(resolve_inline(script.into()).unwrap(), spec())
            .unwrap();

        let SubagentEvent::Spawn(_first) = subagent_rx.recv().await.expect("first spawn") else {
            panic!("expected spawn event");
        };
        assert_eq!(
            manager.tracker.lock().get(&run_id).unwrap().agents_used,
            1,
            "the live agent reserves one slot before it spawns"
        );

        assert!(manager.pause(&run_id));
        assert!(matches!(
            outcome_rx.await.unwrap(),
            WorkflowOutcome::Cancelled
        ));
        assert_eq!(
            manager.tracker.lock().get(&run_id).unwrap().agents_used,
            1,
            "cancel tears the host down before the release lands, so the reserved slot leaks in memory"
        );

        let (_resumed_id, outcome_rx) = manager
            .launch(
                resolve_inline(script.into()).unwrap(),
                LaunchSpec {
                    resume_run_id: Some(run_id.clone()),
                    ..spec()
                },
            )
            .unwrap();
        let SubagentEvent::Spawn(req) = subagent_rx.recv().await.expect("respawned agent") else {
            panic!("expected respawn event");
        };
        let id = req.id.clone();
        let _ = req.result_tx.send(SubagentResult {
            success: true,
            output: std::sync::Arc::from("resumed output"),
            subagent_id: id,
            ..Default::default()
        });
        assert!(matches!(
            outcome_rx.await.unwrap(),
            WorkflowOutcome::Completed { .. }
        ));

        assert_eq!(
            manager.tracker.lock().get(&run_id).unwrap().agents_used,
            1,
            "resume reconciles agents_used from the journal (0 journaled) then re-reserves once; \
             without the reconcile the leaked slot double-charges to 2"
        );
    }

    #[tokio::test]
    async fn failed_run_resumes_and_reexecutes_failed_host_call_live() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, _rx) = test_manager(Some(dir.path().to_path_buf()));
        let script = "let meta = #{ name: \"t\", description: \"d\" };\n\
                      let content = read_scratch_file(\"data.txt\");\n\
                      complete(content);";
        let (run_id, outcome_rx) = manager
            .launch(resolve_inline(script.into()).unwrap(), spec())
            .unwrap();
        match outcome_rx.await.unwrap() {
            WorkflowOutcome::Failed { error } => {
                assert!(error.contains("scratch"), "{error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            manager.tracker.lock().get(&run_id).unwrap().status,
            crate::session::workflow::tracker::WorkflowRunStatus::Failed
        );
        let journal_path = dir
            .path()
            .join("workflows")
            .join(&run_id)
            .join("journal.jsonl");
        assert!(
            std::fs::read_to_string(&journal_path)
                .unwrap()
                .contains("__echo_agent_workflow_host_error"),
            "the uncaught host error must be journaled as a trailing sentinel"
        );

        let scratch = dir.path().join("workflows").join(&run_id).join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("data.txt"), "hello").unwrap();

        let (_same_id, outcome_rx) = manager
            .launch(
                resolve_inline(script.into()).unwrap(),
                LaunchSpec {
                    resume_run_id: Some(run_id.clone()),
                    ..spec()
                },
            )
            .unwrap();
        match outcome_rx.await.unwrap() {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(
                    result,
                    serde_json::json!("hello"),
                    "the failed host call must go live instead of replaying the sentinel"
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert_eq!(
            manager.tracker.lock().get(&run_id).unwrap().status,
            crate::session::workflow::tracker::WorkflowRunStatus::Complete
        );
        assert!(
            !std::fs::read_to_string(&journal_path)
                .unwrap()
                .contains("__echo_agent_workflow_host_error"),
            "the trailing sentinel must be pruned and replaced by the live result"
        );
    }

    #[tokio::test]
    async fn completed_and_interrupted_runs_are_not_resumable() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentResult,
        };

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let script = "let meta = #{ name: \"t\", description: \"d\" };\nlet a = agent(\"step one\");\ncomplete(a.output);";
        let (run_id, outcome_rx) = manager
            .launch(resolve_inline(script.into()).unwrap(), spec())
            .unwrap();
        let SubagentEvent::Spawn(req) = subagent_rx.recv().await.expect("first spawn") else {
            panic!("expected spawn event");
        };
        let id = req.id.clone();
        let _ = req.result_tx.send(SubagentResult {
            success: true,
            output: std::sync::Arc::from("one"),
            subagent_id: id,
            ..Default::default()
        });
        assert!(matches!(
            outcome_rx.await.unwrap(),
            WorkflowOutcome::Completed { .. }
        ));

        let state = manager.tracker.lock().get(&run_id).unwrap();
        for status in [
            crate::session::workflow::tracker::WorkflowRunStatus::Complete,
            crate::session::workflow::tracker::WorkflowRunStatus::Interrupted,
        ] {
            let mut restored = state.clone();
            restored.status = status;
            let original_tracker = manager.tracker.clone();
            manager.tracker = Arc::new(parking_lot::Mutex::new(WorkflowTracker::from_snapshot(
                vec![restored],
            )));
            let err = manager
                .launch(
                    resolve_inline(script.into()).unwrap(),
                    LaunchSpec {
                        resume_run_id: Some(run_id.clone()),
                        ..spec()
                    },
                )
                .unwrap_err();
            manager.tracker = original_tracker;
            assert!(
                matches!(err, LaunchError::NotResumable(_)),
                "{status:?}: {err}"
            );
        }

        let mut cancelled = state.clone();
        cancelled.status = crate::session::workflow::tracker::WorkflowRunStatus::Cancelled;
        manager.tracker = Arc::new(parking_lot::Mutex::new(WorkflowTracker::from_snapshot(
            vec![cancelled],
        )));
        manager
            .launch(
                resolve_inline(script.into()).unwrap(),
                LaunchSpec {
                    resume_run_id: Some(run_id.clone()),
                    ..spec()
                },
            )
            .expect("cancelled /workflow stop runs stay resumable from the journal");
    }

    #[tokio::test]
    async fn shutdown_timeout_marks_active_run_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, _rx) = test_manager(Some(dir.path().to_path_buf()));
        let run_id = "wf_timeout".to_string();
        manager
            .store
            .register(
                &run_id,
                "let meta = #{ name: \"t\", description: \"d\" };",
                &serde_json::json!({}),
                None,
            )
            .unwrap();
        manager.tracker.lock().start_run(
            run_id.clone(),
            "t".into(),
            "obj".into(),
            Vec::new(),
            None,
            None,
        );
        let (_done_tx, done_rx) = oneshot::channel();
        manager.test_insert_active_run(run_id.clone(), done_rx);

        let result = manager
            .cancel_all_and_drain(std::time::Duration::from_millis(1))
            .await;
        assert_eq!(result.unwrap_err(), vec![run_id.clone()]);
        let state = manager.tracker.lock().get(&run_id).unwrap();
        assert_eq!(
            state.status,
            crate::session::workflow::tracker::WorkflowRunStatus::Interrupted
        );
        assert!(!state.status.is_paused());
    }

    #[tokio::test]
    async fn workflow_spawns_await_to_completion() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentResult,
        };

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\n\
             let r = agent(\"work\");\n\
             complete(r.output);"
                .into(),
        )
        .unwrap();
        let (_run_id, outcome_rx) = manager.launch(resolved, spec()).unwrap();

        let spawn_req = subagent_rx.recv().await.expect("spawn event");
        let SubagentEvent::Spawn(req) = spawn_req else {
            panic!("expected spawn event");
        };
        assert!(
            req.await_to_completion,
            "workflow agent spawns must disable the ordinary task-tool await budget"
        );
        assert!(
            req.owner.is_workflow(),
            "workflow agent spawns must carry run lifecycle ownership"
        );
        assert_eq!(
            req.runtime_overrides.model_override_provenance,
            echo_agent_tools::implementations::echo_agent_build::task::types::ModelOverrideProvenance::Tool,
            "script model overrides are untrusted tool provenance"
        );
        assert_eq!(req.runtime_overrides.reasoning_effort, None);
        let id = req.id.clone();
        let _ = req.result_tx.send(SubagentResult {
            success: true,
            output: std::sync::Arc::from("slow but done"),
            subagent_id: id,
            ..Default::default()
        });
        let outcome = outcome_rx.await.unwrap();
        assert!(matches!(outcome, WorkflowOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn launch_effort_applies_to_children_and_child_override_wins() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent;

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\n\
             let results = parallel([\n\
                 #{ prompt: \"inherits\" },\n\
                 #{ prompt: \"overrides\", effort: \"LoW\" },\n\
             ]);\n\
             complete(results.len());"
                .into(),
        )
        .unwrap();
        let (_run_id, outcome_rx) = manager
            .launch(
                resolved,
                LaunchSpec {
                    effort: Some(ReasoningEffort::High),
                    ..spec()
                },
            )
            .unwrap();

        let mut efforts = HashMap::new();
        for _ in 0..2 {
            let SubagentEvent::Spawn(req) = subagent_rx.recv().await.expect("spawn") else {
                panic!("expected spawn event");
            };
            efforts.insert(
                req.request.prompt.clone(),
                req.request.runtime_overrides.reasoning_effort.clone(),
            );
            complete_spawn(req);
        }
        assert_eq!(
            efforts.get("inherits").and_then(Option::as_deref),
            Some("high")
        );
        assert_eq!(
            efforts.get("overrides").and_then(Option::as_deref),
            Some("low")
        );
        assert!(matches!(
            outcome_rx.await.unwrap(),
            WorkflowOutcome::Completed { .. }
        ));
    }

    #[tokio::test]
    async fn agent_rejects_invalid_effort_before_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\n\
             agent(\"work\", #{ effort: \"turbo\" });"
                .into(),
        )
        .unwrap();
        let (_run_id, outcome_rx) = manager.launch(resolved, spec()).unwrap();

        match outcome_rx.await.unwrap() {
            WorkflowOutcome::Failed { error } => {
                assert!(error.contains("invalid workflow agent effort"), "{error}");
                assert!(error.contains("turbo"), "{error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(
            subagent_rx.try_recv().is_err(),
            "invalid effort must not reach the coordinator"
        );
    }

    #[tokio::test]
    async fn parallel_nulls_invalid_child_effort_without_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\n\
             let results = parallel([#{ prompt: \"work\", effort: \"turbo\" }]);\n\
             complete(results);"
                .into(),
        )
        .unwrap();
        let (_run_id, outcome_rx) = manager.launch(resolved, spec()).unwrap();

        match outcome_rx.await.unwrap() {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!([null]));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert!(
            subagent_rx.try_recv().is_err(),
            "invalid effort must not reach the coordinator"
        );
    }

    #[tokio::test]
    async fn active_run_admission_is_bounded_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let script = "let meta = #{ name: \"t\", description: \"d\" };\n\
                      let r = agent(\"work\");\ncomplete(r.output);";
        let mut outcomes = Vec::new();
        let mut spawned = Vec::new();
        for _ in 0..WORKFLOW_MAX_ACTIVE_RUNS_PER_SESSION {
            let (_, outcome) = manager
                .launch(resolve_inline(script.into()).unwrap(), spec())
                .unwrap();
            outcomes.push(outcome);
            spawned.push(subagent_rx.recv().await.expect("spawn event"));
        }
        let error = manager
            .launch(resolve_inline(script.into()).unwrap(), spec())
            .unwrap_err();
        assert!(matches!(error, LaunchError::TooManyActiveRuns));
        drop(spawned);
        let _ = manager
            .cancel_all_and_drain(std::time::Duration::from_secs(1))
            .await;
        drop(outcomes);
    }

    #[tokio::test]
    async fn retiring_runs_still_consume_session_admission() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, _subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let mut done_senders = Vec::new();
        for index in 0..WORKFLOW_MAX_ACTIVE_RUNS_PER_SESSION {
            let (done_tx, done_rx) = oneshot::channel();
            manager
                .retiring
                .push((format!("retiring-{index}"), done_rx));
            done_senders.push(done_tx);
            assert_eq!(manager.retiring.len(), index + 1);
        }

        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\ncomplete(\"done\");".into(),
        )
        .unwrap();
        assert!(matches!(
            manager.launch(resolved, spec()).unwrap_err(),
            LaunchError::TooManyActiveRuns
        ));

        done_senders.pop().unwrap().send(()).unwrap();
        manager.reap_terminal_runs();
        assert_eq!(
            manager.retiring.len(),
            WORKFLOW_MAX_ACTIVE_RUNS_PER_SESSION - 1
        );
        assert!(
            manager.active.len().saturating_add(manager.retiring.len())
                < WORKFLOW_MAX_ACTIVE_RUNS_PER_SESSION
        );
        drop(done_senders);
        assert!(!manager.has_live_runs(), "finished retiring runs must not pin an idle session");
    }

    #[tokio::test]
    async fn untrusted_workflow_cannot_fork_parent_context() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\n\
             let r = agent(\"work\", #{ fork_context: true });\n\
             complete(r.output);"
                .into(),
        )
        .unwrap();
        let (_run_id, outcome_rx) = manager.launch(resolved, spec()).unwrap();

        match outcome_rx.await.unwrap() {
            WorkflowOutcome::Failed { error } => {
                assert!(error.contains("fork_context is restricted to built-in workflows"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(
            subagent_rx.try_recv().is_err(),
            "rejected fork_context must not reach the coordinator"
        );
    }

    #[tokio::test]
    async fn output_schema_stays_host_side_with_one_corrective_retry() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentResult,
        };

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\n\
             let r = agent(\"scan\", #{ output_schema: #{ \"type\": \"object\", \
             \"required\": [\"ok\"], \"properties\": #{ \"ok\": #{ \"type\": \"boolean\" } } } });\n\
             complete(r.output.ok);"
                .into(),
        )
        .unwrap();
        let (_run_id, outcome_rx) = manager
            .launch(
                resolved,
                LaunchSpec {
                    agent_budget: Some(5),
                    ..spec()
                },
            )
            .unwrap();

        let SubagentEvent::Spawn(req) = subagent_rx.recv().await.expect("first spawn") else {
            panic!("expected spawn event");
        };
        assert!(
            req.runtime_overrides.output_schema.is_none(),
            "schema must not be passed to the child runtime"
        );
        assert!(
            req.prompt.contains("<output-contract>"),
            "prompt must carry the schema contract"
        );
        assert!(req.resume_from.is_none());
        assert_eq!(req.runtime_overrides.output_token_budget, None);
        let first_id = req.id.clone();
        let _ = req.result_tx.send(SubagentResult {
            success: true,
            output: std::sync::Arc::from("All files scanned, nothing found."),
            subagent_id: first_id.clone(),
            child_session_id: first_id.clone(),
            tokens_used: 100,
            output_tokens_used: 100,
            total_tokens_used: 100,
            ..Default::default()
        });

        let SubagentEvent::Spawn(retry) = subagent_rx.recv().await.expect("corrective retry")
        else {
            panic!("expected retry spawn event");
        };
        assert_eq!(retry.resume_from.as_deref(), Some(first_id.as_str()));
        assert!(retry.prompt.contains("did not satisfy the output contract"));
        assert_eq!(retry.runtime_overrides.output_token_budget, None);
        let retry_id = retry.id.clone();
        let _ = retry.result_tx.send(SubagentResult {
            success: true,
            output: std::sync::Arc::from("```json\n{\"ok\": true}\n```"),
            subagent_id: retry_id.clone(),
            child_session_id: retry_id,
            tokens_used: 50,
            output_tokens_used: 50,
            total_tokens_used: 50,
            ..Default::default()
        });

        let outcome = outcome_rx.await.unwrap();
        match outcome {
            WorkflowOutcome::Completed { result } => {
                assert_eq!(result, serde_json::json!(true));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        let state = manager.tracker.lock().list().into_iter().next().unwrap();
        assert_eq!(state.agents_used, 1, "schema retry is one logical agent");
        assert_eq!(state.agent_budget, Some(5));
    }

    #[tokio::test]
    async fn explicit_max_output_tokens_is_ignored_and_run_charges_totals() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentResult,
        };

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\n\
             let r = agent(\"work\", #{ max_output_tokens: 900 });\n\
             complete(r.output);"
                .into(),
        )
        .unwrap();
        let (run_id, outcome_rx) = manager
            .launch(
                resolved,
                LaunchSpec {
                    agent_budget: Some(2),
                    ..spec()
                },
            )
            .unwrap();
        let SubagentEvent::Spawn(req) = subagent_rx.recv().await.expect("spawn") else {
            panic!("expected spawn");
        };
        assert_eq!(req.runtime_overrides.output_token_budget, None);
        let id = req.id.clone();
        let _ = req.result_tx.send(SubagentResult {
            success: true,
            output: std::sync::Arc::from("done"),
            subagent_id: id.clone(),
            child_session_id: id,
            output_tokens_used: 120,
            total_tokens_used: 120,
            ..Default::default()
        });
        assert!(matches!(
            outcome_rx.await.unwrap(),
            WorkflowOutcome::Completed { .. }
        ));
        let state = manager.tracker.lock().get(&run_id).unwrap();
        assert_eq!(state.agents_used, 1);
        assert_eq!(state.agent_budget, Some(2));
    }

    #[tokio::test]
    async fn children_spawn_without_output_clamp() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentResult,
        };

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\n\
             let r = agent(\"work\");\ncomplete(r.output);"
                .into(),
        )
        .unwrap();
        let (_run_id, _outcome_rx) = manager.launch(resolved, spec()).unwrap();
        let SubagentEvent::Spawn(req) = subagent_rx.recv().await.expect("spawn") else {
            panic!("expected spawn");
        };
        assert_eq!(req.runtime_overrides.output_token_budget, None);
        let id = req.id.clone();
        let _ = req.result_tx.send(SubagentResult {
            success: true,
            output: std::sync::Arc::from("done"),
            subagent_id: id.clone(),
            child_session_id: id,
            output_tokens_used: 1,
            ..Default::default()
        });
    }

    #[tokio::test]
    async fn cancellation_uses_run_owned_cancel_event_without_parent_detach() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentCancelTarget, SubagentEvent,
        };

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx, cancels) =
            test_manager_with_cancels(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\n\
             let r = agent(\"work\");\ncomplete(r.output);"
                .into(),
        )
        .unwrap();
        let (run_id, outcome_rx) = manager
            .launch(
                resolved,
                LaunchSpec {
                    agent_budget: Some(100),
                    ..spec()
                },
            )
            .unwrap();
        let SubagentEvent::Spawn(req) = subagent_rx.recv().await.expect("spawn") else {
            panic!("expected spawn");
        };
        assert!(req.owner.is_workflow());
        assert!(manager.cancel(&run_id));
        let _ = outcome_rx.await;
        assert!(
            req.cancel_token.is_cancelled(),
            "run cancel must cancel the child token, not silently detach the receiver"
        );
        assert!(
            cancels.lock().iter().any(|target| matches!(
                target,
                SubagentCancelTarget::WorkflowRunId(id) if id == &run_id
            )),
            "cancellation must emit an explicit run-owned cancel event"
        );
    }

    #[tokio::test]
    async fn backgrounded_stub_fails_loudly() {
        use echo_agent_tools::implementations::echo_agent_build::task::types::{
            SubagentEvent, SubagentResult,
        };

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        let resolved = resolve_inline(
            "let meta = #{ name: \"t\", description: \"d\" };\n\
             let r = agent(\"work\");\n\
             complete(r.output);"
                .into(),
        )
        .unwrap();
        let (run_id, outcome_rx) = manager.launch(resolved, spec()).unwrap();

        let spawn_req = subagent_rx.recv().await.expect("spawn event");
        let SubagentEvent::Spawn(req) = spawn_req else {
            panic!("expected spawn event");
        };
        let id = req.id.clone();
        let _ = req.result_tx.send(SubagentResult {
            backgrounded: true,
            subagent_id: id,
            ..Default::default()
        });
        let outcome = outcome_rx.await.unwrap();
        match outcome {
            WorkflowOutcome::Failed { error } => {
                assert!(
                    error.contains("auto-backgrounded"),
                    "distinct engine-bug message expected, got: {error}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        let state = manager.tracker.lock().get(&run_id).unwrap();
        assert_eq!(
            state.status,
            crate::session::workflow::tracker::WorkflowRunStatus::Failed
        );
    }

    #[tokio::test]
    async fn parallel_panel_respects_concurrency_cap() {
        const CAP: usize = 2;
        const N: usize = 6;

        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        manager.test_set_max_concurrent_agents(CAP);
        let (_run_id, outcome_rx) = manager
            .launch(resolve_inline(parallel_n_script(N)).unwrap(), spec())
            .unwrap();

        let mut live = Vec::new();
        for _ in 0..CAP {
            live.push(recv_spawn(&mut subagent_rx).await);
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), subagent_rx.recv())
                .await
                .is_err(),
            "more than {CAP} children were live"
        );

        let mut completed = 0usize;
        while completed + live.len() < N {
            complete_spawn(live.remove(0));
            completed += 1;
            live.push(recv_spawn(&mut subagent_rx).await);
        }
        for req in live {
            complete_spawn(req);
        }

        match outcome_rx.await.unwrap() {
            WorkflowOutcome::Completed { result } => assert_eq!(result, serde_json::json!(N)),
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_drops_queued_spawns_before_coordinator() {
        let dir = tempfile::tempdir().unwrap();
        let (mut manager, mut subagent_rx) = test_manager(Some(dir.path().to_path_buf()));
        manager.test_set_max_concurrent_agents(1);
        let (run_id, outcome_rx) = manager
            .launch(resolve_inline(parallel_n_script(4)).unwrap(), spec())
            .unwrap();

        let first = recv_spawn(&mut subagent_rx).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), subagent_rx.recv())
                .await
                .is_err(),
            "queued agents reached the coordinator before cancel"
        );

        assert!(manager.cancel(&run_id));
        let _ = outcome_rx.await;
        assert!(first.cancel_token.is_cancelled());
        assert!(subagent_rx.try_recv().is_err());
    }
}
