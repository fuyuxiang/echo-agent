//! Session lifecycle, roster deltas, and the idle-session supervisor for [`MvpAgent`].
//! Co-located `#[path]`-style child of `mvp_agent` (`use super::*`) so the `impl`
//! block keeps access to `MvpAgent`'s private fields.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]
use super::*;
/// Bound on close's wait for a prompt still in intake.
pub(super) const CLOSE_INTAKE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// Bound on close's wait for an in-flight attach.
const CLOSE_ATTACH_SETTLE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// Cap on the sum of every close wait.
pub(super) const CLOSE_TOTAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(8);
/// Bound on delete's wait for the subagent coordinator to drain a session's
/// children. Separate from [`DRAIN_OLD_THREAD_WAIT`] (which bounds waiting out
/// a flushing actor thread) so the two budgets can move independently.
const DRAIN_SUBAGENTS_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// Cap on the sum of every delete wait (subagent drain + old-thread drain),
/// mirroring [`CLOSE_TOTAL_BUDGET`] so the delete toast cannot outlast it.
const DELETE_TOTAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(8);
/// `cap`, shrunk to what remains under `deadline`.
fn stage_budget(deadline: tokio::time::Instant, cap: std::time::Duration) -> std::time::Duration {
    cap.min(deadline.saturating_duration_since(tokio::time::Instant::now()))
}
/// What a close did. `Superseded`: a live session replaced the target and
/// survived.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseOutcome {
    Closed,
    NotResident,
    Superseded,
}
impl CloseOutcome {
    /// The spelling clients see in the close response.
    pub(crate) fn wire_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::NotResident => "notResident",
            Self::Superseded => "superseded",
        }
    }
}
impl MvpAgent {
    /// Ask a live session actor to shut down.
    pub(crate) fn request_session_shutdown(&self, id: &acp::SessionId) {
        if let Some(handle) = self.resident_handle(id) {
            let _ = handle
                .cmd_tx
                .send(SessionCommand::Shutdown(ShutdownKind::Graceful));
        }
    }
    /// ACP `session/close` and its pre-ACP spelling. Orders behind prompt
    /// intake; every wait spends from [`CLOSE_TOTAL_BUDGET`].
    pub(crate) async fn close_active_session(&self, id: &acp::SessionId) -> CloseOutcome {
        let deadline = tokio::time::Instant::now() + CLOSE_TOTAL_BUDGET;
        self.wait_for_load_to_settle(id, stage_budget(deadline, CLOSE_ATTACH_SETTLE_WAIT))
            .await;
        let Some(target) = self.resident_handle(id).map(|h| h.cmd_tx.clone()) else {
            return CloseOutcome::NotResident;
        };
        let intake = self.dispatch_lock(id);
        let intake_guard =
            tokio::time::timeout(stage_budget(deadline, CLOSE_INTAKE_WAIT), intake.lock())
                .await
                .ok();
        match self.resident_handle(id).map(|h| h.cmd_tx.clone()) {
            None => return CloseOutcome::NotResident,
            Some(current) if !current.same_channel(&target) => {
                return CloseOutcome::Superseded;
            }
            Some(_) => {}
        }
        if !self.hard_stop_resident(id, CancelTrigger::SessionClose) {
            return CloseOutcome::NotResident;
        }
        drop(intake_guard);
        self.remove_session_terminal(id, SessionLiveState::Completed);
        self.drain_old_session_thread_within(id, stage_budget(deadline, DRAIN_OLD_THREAD_WAIT))
            .await;
        self.finalize_session_replica(id);
        CloseOutcome::Closed
    }
    /// Cancel the running turn and shut the actor down; `false` when not
    /// resident. Close finalizes the replica afterward, delete must not.
    fn hard_stop_resident(&self, id: &acp::SessionId, trigger: CancelTrigger) -> bool {
        let Some(handle) = self.resident_handle(id) else {
            return false;
        };
        let _ = handle.cmd_tx.send(SessionCommand::Cancel(CancelOptions {
            cancel_subagents: true,
            kill_background_tasks: true,
            trigger: Some(trigger),
            ..Default::default()
        }));
        let _ = handle
            .cmd_tx
            .send(SessionCommand::Shutdown(ShutdownKind::CancelRunningTurn));
        true
    }
    /// Hard-stop before wiping history so delete cannot race live writers.
    ///
    /// Order matches [`Self::close_active_session`]: drop residency *before*
    /// any await (the supervisor treats a finished still-resident actor as a
    /// crash; awaiting subagent drain while resident races that sweep), and
    /// every wait spends from a shared [`DELETE_TOTAL_BUDGET`] so the two
    /// drains cannot stack into a toast twice as long as close's.
    pub(crate) async fn teardown_live_session_before_delete(&self, id: &acp::SessionId) {
        let deadline = tokio::time::Instant::now() + DELETE_TOTAL_BUDGET;
        let resident = self.hard_stop_resident(id, CancelTrigger::SessionDelete);
        if resident {
            self.remove_session_terminal(id, SessionLiveState::Completed);
        }
        echo_agent_tools::implementations::echo_agent_build::task::backend::ChannelBackend::new(
            self.subagent_event_tx.clone(),
        )
        .teardown_session_and_drain(&id.0, stage_budget(deadline, DRAIN_SUBAGENTS_WAIT))
        .await;
        if resident {
            self.drain_old_session_thread_within(id, stage_budget(deadline, DRAIN_OLD_THREAD_WAIT))
                .await;
        }
    }
    /// Move the replica `active` -> `completed`. A hosting signal, not a
    /// conversation ending: only an explicit close sends it.
    pub(super) fn finalize_session_replica(&self, id: &acp::SessionId) {
        #[cfg(test)]
        self.finalize_spy.borrow_mut().push(id.0.to_string());
        if let Some(client) = self.session_registry_client() {
            let sid = id.0.to_string();
            tokio::spawn(async move {
                if let Err(e) = client.finalize(&sid).await {
                    tracing::warn!(error = %e, "session registry finalize failed (non-fatal)");
                }
            });
        }
    }
    /// Clone of the hosted handle, if any. Callers must not hold a registry
    /// borrow across an await: clone the handle out first.
    pub(crate) fn resident_handle(&self, id: &acp::SessionId) -> Option<SessionHandle> {
        self.session_registry.resident_handle(id)
    }
    pub(crate) fn is_resident(&self, id: &acp::SessionId) -> bool {
        self.session_registry.is_resident(id)
    }
    /// Register or replace the hosted handle. Returns the displaced handle.
    pub(crate) fn insert_resident(
        &self,
        id: &acp::SessionId,
        handle: SessionHandle,
    ) -> Option<SessionHandle> {
        self.session_registry.put_resident(id, handle)
    }
    pub(crate) fn resident_count(&self) -> usize {
        self.session_registry.resident_count()
    }
    pub(crate) fn resident_ids(&self) -> Vec<acp::SessionId> {
        self.session_registry.resident_ids()
    }
    pub(crate) fn with_resident_mut<R>(
        &self,
        id: &acp::SessionId,
        f: impl FnOnce(&mut SessionHandle) -> R,
    ) -> Option<R> {
        self.session_registry.with_resident_mut(id, f)
    }
    pub(crate) fn resident_cmd_txs(
        &self,
    ) -> Vec<tokio::sync::mpsc::UnboundedSender<SessionCommand>> {
        let mut txs = Vec::new();
        self.session_registry
            .for_each_resident(|_, handle| txs.push(handle.cmd_tx.clone()));
        txs
    }
    pub(crate) fn for_each_resident(&self, f: impl FnMut(&acp::SessionId, &SessionHandle)) {
        self.session_registry.for_each_resident(f)
    }
    /// The funnel for a handle leaving residency: SIGKILLs the child-process
    /// tree before the actor drains `Shutdown`, even on idle-unload, so a
    /// wedged session's tree is still reclaimed.
    pub(super) fn take_session(&self, id: &acp::SessionId) -> Option<SessionHandle> {
        let handle = self.session_registry.take_resident(id);
        if let Some(handle) = &handle
            && let Some(scope) = &handle.tool_context.process_scope
        {
            scope.kill_all();
        }
        handle
    }
    /// Remove a session without finalizing; it stays resumable on disk.
    pub(crate) fn remove_session(&self, id: &acp::SessionId) {
        let _ = self
            .subagent_event_tx
            .send(echo_agent_tools::implementations::echo_agent_build::task::types::SubagentEvent::TeardownSession {
                parent_session_id: id.0.to_string(),
                respond_to: None,
            });
        self.take_session(id);
        self.session_registry.release(id);
        if let Some(ops) = self.workspace_ops.borrow().as_ref() {
            ops.end_local_session(id.0.as_ref());
        }
        self.log_resource_usage(echo_agent_telemetry::events::ResourceReportTrigger::SessionClose);
    }
    /// Per-session prompt-intake lock: prompts land in submission order and a
    /// cancel cannot overtake the prompt it targets. Keep preambles lean.
    pub(super) fn dispatch_lock(&self, id: &acp::SessionId) -> std::rc::Rc<tokio::sync::Mutex<()>> {
        self.session_registry.dispatch_lock(id)
    }
    /// Record the coarse lifecycle state for a session.
    pub(super) fn set_session_live_state(&self, id: &acp::SessionId, state: SessionLiveState) {
        self.session_registry.set_live(id, state);
    }
    /// Read the recorded lifecycle state for a session (test observability).
    #[cfg(test)]
    pub(super) fn session_live_state_for(&self, id: &acp::SessionId) -> Option<SessionLiveState> {
        self.session_registry.live(id)
    }
    /// Broadcast the removal delta; the spy records it because the live-state
    /// entry is dropped with the session.
    pub(super) fn record_roster_delta(&self, id: &acp::SessionId, final_state: SessionLiveState) {
        #[cfg(test)]
        self.roster_delta_spy
            .borrow_mut()
            .push((id.0.to_string(), final_state));
        tracing::debug!(
            session_id = %id.0,
            ?final_state,
            "roster delta: session removed"
        );
        self.emit_roster_changed(Vec::new(), vec![id.0.to_string()]);
    }
    /// Broadcast the upsert delta for a resident session.
    pub(crate) fn push_roster_delta_upserted(&self, id: &acp::SessionId) {
        if let Some(entry) = self.resident_roster_entry(id) {
            self.emit_roster_changed(vec![entry], Vec::new());
        }
    }
    /// Upsert with a caller-supplied activity: at turn-start the actor has not
    /// published `current_prompt_id` yet, so a natural read would say Idle.
    pub(super) fn push_roster_activity_delta(
        &self,
        id: &acp::SessionId,
        activity: crate::agent::roster::RosterActivity,
    ) {
        if let Some(mut entry) = self.resident_roster_entry(id) {
            entry.activity = activity;
            self.emit_roster_changed(vec![entry], Vec::new());
        }
    }
    /// Roster-wide notification (no `sessionId`): the leader broadcasts it to
    /// every client instead of routing by session.
    pub(super) fn emit_roster_changed(
        &self,
        upserted: Vec<crate::agent::roster::RosterEntry>,
        removed: Vec<String>,
    ) {
        if upserted.is_empty() && removed.is_empty() {
            return;
        }
        let payload = crate::agent::roster::RosterChanged { upserted, removed };
        if let Ok(params) = serde_json::value::to_raw_value(&payload) {
            self.gateway
                .forward_fire_and_forget(acp::ExtNotification::new(
                    crate::agent::roster::SESSIONS_CHANGED_METHOD,
                    params.into(),
                ));
        }
    }
    /// Dashboard activity. Precedence: NeedsInput (even mid-turn), then
    /// Working, then the coarse `SessionLiveState`.
    pub(super) fn resident_activity(
        &self,
        id: &acp::SessionId,
    ) -> crate::agent::roster::RosterActivity {
        use crate::agent::roster::RosterActivity;
        let (needs_input, turn_running) = self
            .resident_handle(id)
            .map(|h| {
                let needs_input = h
                    .pending_interactions
                    .lock()
                    .map(|g| !g.is_empty())
                    .unwrap_or(false);
                let turn_running = h
                    .current_prompt_id
                    .lock()
                    .map(|g| g.is_some())
                    .unwrap_or(false);
                (needs_input, turn_running)
            })
            .unwrap_or((false, false));
        if needs_input {
            return RosterActivity::NeedsInput;
        }
        if turn_running {
            return RosterActivity::Working;
        }
        match self.session_registry.live(id) {
            Some(SessionLiveState::Completed) => RosterActivity::Completed,
            Some(SessionLiveState::DeadFailed) => RosterActivity::Dead,
            Some(SessionLiveState::Dormant) => RosterActivity::Dormant,
            Some(SessionLiveState::Attaching | SessionLiveState::Working) => RosterActivity::Idle,
            Some(SessionLiveState::IdleResident) | None => RosterActivity::Idle,
        }
    }
    /// Roster entry for a resident session; `None` when not resident.
    pub(super) fn resident_roster_entry(
        &self,
        id: &acp::SessionId,
    ) -> Option<crate::agent::roster::RosterEntry> {
        let session_id = id.0.to_string();
        let (cwd, is_worktree, model_id, reasoning_effort, yolo) = {
            let h = self.resident_handle(id)?;
            (
                h.display_cwd.clone().unwrap_or_else(|| h.info.cwd.clone()),
                h.display_cwd.is_some(),
                Some(h.model_id.0.to_string()),
                h.reasoning_effort,
                h.yolo_mode,
            )
        };
        let (title, last_turn_summary) = self
            .resident_roster_titles
            .borrow()
            .get(&session_id)
            .cloned()
            .unwrap_or_default();
        Some(crate::agent::roster::RosterEntry {
            title,
            last_turn_summary,
            session_id,
            cwd,
            is_worktree,
            model_id,
            reasoning_effort,
            yolo,
            activity: self.resident_activity(id),
            resident: true,
            last_change_unix_ms: chrono::Utc::now().timestamp_millis(),
            origin: crate::agent::roster::RosterOrigin::Local,
        })
    }
    /// Snapshot all resident sessions as roster entries (synchronous; no disk).
    pub(super) fn resident_roster_entries(&self) -> Vec<crate::agent::roster::RosterEntry> {
        let ids: Vec<acp::SessionId> = self.resident_ids();
        ids.iter()
            .filter_map(|id| self.resident_roster_entry(id))
            .collect()
    }
    /// Full roster: resident actors plus recent on-disk sessions; resident
    /// wins an id collision.
    pub(crate) async fn build_roster(&self) -> Vec<crate::agent::roster::RosterEntry> {
        let resident = self.resident_roster_entries();
        let summaries = crate::session::persistence::list_recent_summaries(200)
            .await
            .unwrap_or_default();
        let entries = crate::agent::roster::merge_roster(resident, summaries);
        self.cache_resident_titles(&entries);
        entries
    }
    /// Refresh `resident_roster_titles` from the freshly-built roster.
    pub(super) fn cache_resident_titles(&self, entries: &[crate::agent::roster::RosterEntry]) {
        *self.resident_roster_titles.borrow_mut() = entries
            .iter()
            .filter(|e| e.resident)
            .map(|e| {
                (
                    e.session_id.clone(),
                    (e.title.clone(), e.last_turn_summary.clone()),
                )
            })
            .collect();
    }
    /// Emit the final roster delta, then drop the session from all maps.
    pub(super) fn remove_session_terminal(
        &self,
        id: &acp::SessionId,
        final_state: SessionLiveState,
    ) {
        self.record_roster_delta(id, final_state);
        self.remove_session(id);
    }
    /// Reap a resident actor that exited unexpectedly; the conversation stays
    /// resumable on disk.
    pub(super) fn reap_dead_session(&self, id: &acp::SessionId) {
        self.remove_session_terminal(id, SessionLiveState::DeadFailed);
    }
    /// Reap finished actor threads. Resident and finished is a crash
    /// (`DeadFailed`); non-resident and finished is the expected clean exit,
    /// dropped without demotion. `is_finished()` alone cannot tell them apart,
    /// which is why residency decides.
    pub(super) fn sweep_dead_sessions(&self) {
        let dead = self.session_registry.finished_threads();
        for id in dead {
            if self.session_registry.live(&id) == Some(SessionLiveState::Attaching)
                && !self.is_resident(&id)
            {
                continue;
            }
            if self.is_resident(&id) {
                tracing::warn!(
                    session_id = %id.0,
                    "Resident session actor exited unexpectedly; reaping as DeadFailed"
                );
                self.reap_dead_session(&id);
            } else {
                self.session_registry.clear_exited_thread(&id);
                tracing::debug!(
                    session_id = %id.0,
                    "Reaped finished thread for non-resident session (clean exit)"
                );
            }
        }
    }
    /// Idempotent join-handle supervisor: polls `is_finished()` each tick
    /// (JoinHandle is not awaitable) and sweeps under `catch_unwind` so one
    /// bad sweep cannot end supervision. The `LocalRef` to `self` is sound
    /// because the agent owns and outlives the `LocalSet`.
    pub(super) fn ensure_session_supervisor(&self) {
        if self.supervisor_started.replace(true) {
            return;
        }
        #[cfg(test)]
        self.supervisor_spawn_count
            .set(self.supervisor_spawn_count.get() + 1);
        let agent_ref = LocalRef::new(self);
        tokio::task::spawn_local(async move {
            loop {
                tokio::time::sleep(SESSION_SUPERVISOR_TICK).await;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    agent_ref.get().sweep_dead_sessions();
                }));
                if result.is_err() {
                    tracing::error!("session supervisor sweep panicked; continuing supervision");
                }
            }
        });
    }
    /// Any work in flight? Sync running-turn and parked-plan-approval checks,
    /// then an async queue probe; conservative (busy) on poison or timeout.
    ///
    /// The actor probe also covers background commands, children, schedules,
    /// workflows and pending user interactions.
    pub(super) async fn session_has_live_work(&self, id: &acp::SessionId) -> bool {
        let Some(handle) = self.resident_handle(id) else {
            return false;
        };
        let turn_running = handle
            .current_prompt_id
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(true);
        if turn_running {
            return true;
        }
        if crate::session::pending_interaction::has_parked_plan_approval(
            &handle.pending_interactions,
        ) {
            return true;
        }
        tokio::time::timeout(IDLE_QUERY_TIMEOUT, handle.is_busy())
            .await
            .unwrap_or(true)
    }
    /// Counts for `echo.agent/debug/agent`, including maps outside the registry.
    pub(crate) async fn registry_snapshot(&self) -> RegistrySnapshot {
        let subagents =
            echo_agent_tools::implementations::echo_agent_build::task::backend::ChannelBackend::new(
                self.subagent_event_tx.clone(),
            )
            .registry_counts()
            .await;
        let workspace_ops = self.workspace_ops.borrow();
        let workspace = workspace_ops
            .as_ref()
            .and_then(|ops| ops.workspace_handle());
        let counts = self.session_registry.counts();
        RegistrySnapshot {
            sessions: self.resident_count(),
            loading_sessions: self.session_registry.attaching_count(),
            session_registry_entries: counts.entries,
            session_threads: counts.session_threads,
            resident_resources: counts.resident_resources,
            retained_resources: counts.retained_resources,
            dispatch_locks: counts.dispatch_locks,
            live_orphan_heal_locks: counts.live_orphan_heal_locks,
            session_turn_numbers: counts.session_turn_numbers,
            permission_event_receivers: counts.permission_event_receivers,
            model_unavailable_sessions: counts.model_unavailable_sessions,
            session_live_state: counts.session_live_state,
            session_index_claims: counts.session_index_claims,
            require_gateway_sessions: counts.require_gateway_sessions,
            subagent_pending: subagents.pending,
            subagent_active: subagents.active,
            subagent_completed: subagents.completed,
            subagent_queued: subagents.queued,
            workspace_bindings: workspace.map(|h| h.session_count()),
            workspace_activity_sessions: workspace.map(|h| h.activity_tracker().session_count()),
        }
    }
}
/// Field names are the wire contract of `echo.agent/debug/agent`'s `registries`
/// object; each maps to the same-named registry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct RegistrySnapshot {
    pub sessions: usize,
    pub loading_sessions: usize,
    /// Ids the session registry still tracks. Non-zero when every count below
    /// is zero means an entry survived with a field none of them name.
    pub session_registry_entries: usize,
    pub session_threads: usize,
    pub resident_resources: usize,
    pub retained_resources: usize,
    pub dispatch_locks: usize,
    pub live_orphan_heal_locks: usize,
    pub session_turn_numbers: usize,
    pub permission_event_receivers: usize,
    pub model_unavailable_sessions: usize,
    pub session_live_state: usize,
    pub session_index_claims: usize,
    pub require_gateway_sessions: usize,
    pub subagent_pending: usize,
    pub subagent_active: usize,
    pub subagent_completed: usize,
    /// Spawns parked at the session concurrent limit.
    pub subagent_queued: usize,
    pub workspace_bindings: Option<usize>,
    pub workspace_activity_sessions: Option<usize>,
}
