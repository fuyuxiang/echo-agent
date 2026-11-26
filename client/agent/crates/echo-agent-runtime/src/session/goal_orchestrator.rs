//! Goal mode support — notification helpers and state formatters.
//!

use crate::extensions::notification::{
    SessionNotification as EchoAgentSessionNotification, SessionUpdate as EchoAgentSessionUpdate,
};
use crate::session::goal_tracker::{GoalOrchestration, GoalPhase, GoalStatus, GoalTracker};
use crate::session::persistence::PersistenceMsg;

// ---------------------------------------------------------------------------
// GoalNotifySender — fire-and-forget notification sender
// ---------------------------------------------------------------------------

/// Lightweight notification sender for goal progress updates.
pub(crate) struct GoalNotifySender {
    session_id: agent_client_protocol::SessionId,
    gateway: echo_agent_acp::AcpAgentGatewaySender,
    persistence_tx: tokio::sync::mpsc::UnboundedSender<PersistenceMsg>,
}

impl GoalNotifySender {
    pub(crate) fn new(
        session_id: agent_client_protocol::SessionId,
        gateway: echo_agent_acp::AcpAgentGatewaySender,
        persistence_tx: tokio::sync::mpsc::UnboundedSender<PersistenceMsg>,
    ) -> Self {
        Self {
            session_id,
            gateway,
            persistence_tx,
        }
    }

    /// Send a `GoalUpdated` built from the snapshot; token args come
    /// from [`SessionActor::goal_tokens`].
    pub(crate) fn emit_goal_updated(
        &self,
        tracker: &mut GoalTracker,
        tokens_used: i64,
        finished_subagent_tokens: i64,
    ) {
        tracker.account_elapsed();
        let Some(o) = tracker.snapshot() else { return };
        let _ = self
            .persistence_tx
            .send(PersistenceMsg::GoalModeState(o.clone()));
        self.send_update(build_goal_updated(o, tokens_used, finished_subagent_tokens));
    }

    pub(crate) fn persist_goal_state(&self, tracker: &GoalTracker) {
        let Some(snapshot) = tracker.snapshot() else {
            return;
        };
        let _ = self
            .persistence_tx
            .send(PersistenceMsg::GoalModeState(snapshot.clone()));
    }

    /// Like [`Self::emit_goal_updated`] but fire-and-forget to the gateway
    /// ONLY — the update is not appended to the session JSONL. Used by the
    /// high-frequency `SubagentProgress` live-token path: those ticks recur
    /// (~every `PROGRESS_PUBLISH_INTERVAL`) while a subagent runs, so
    /// persisting each one would grow the updates log without bound. The
    /// durable goal total survives via `PersistenceMsg::GoalModeState` and
    /// the next persisted state-transition `GoalUpdated`; the pager
    /// self-heals the live figure on the next tick. Mirrors the gateway-only
    /// `scheduled_task_fired` convention.
    pub(crate) fn emit_goal_updated_ephemeral(
        &self,
        tracker: &mut GoalTracker,
        tokens_used: i64,
        finished_subagent_tokens: i64,
    ) {
        tracker.account_elapsed();
        let Some(o) = tracker.snapshot() else { return };
        self.dispatch_update(
            build_goal_updated(o, tokens_used, finished_subagent_tokens),
            false,
        );
    }

    /// Persist + fire-and-forget a notification to the gateway. Used for
    /// snapshot-derived payloads and for the "planning…" / "Verifying…"
    /// latch updates that must not run the `send_echo_agent_notification`
    /// rewind-window-close side effect.
    pub(crate) fn send_update(&self, update: EchoAgentSessionUpdate) {
        self.dispatch_update(update, true);
    }

    /// Stamp, optionally persist, and fire-and-forget a notification.
    /// `persist == false` ships the update to the gateway only (no JSONL
    /// append) for recurring/transient ticks — see
    /// [`Self::emit_goal_updated_ephemeral`].
    fn dispatch_update(&self, update: EchoAgentSessionUpdate, persist: bool) {
        // Stamped before the persist/broadcast fork — see `ensure_event_id_meta`.
        let mut meta = None;
        crate::util::event_id::ensure_event_id_meta(&self.session_id.0, &mut meta);
        let notification = EchoAgentSessionNotification {
            session_id: self.session_id.clone(),
            update,
            meta: meta.map(serde_json::Value::Object),
        };
        let raw = serde_json::to_value(&notification)
            .and_then(|v| serde_json::value::to_raw_value(&v))
            .ok();
        if persist {
            let _ = self.persistence_tx.send(PersistenceMsg::Update(
                crate::session::storage::SessionUpdate::EchoAgent(Box::new(notification)),
            ));
        }
        if let Some(raw) = raw {
            let ext = agent_client_protocol::ExtNotification::new(
                "echo.agent/session_notification",
                raw.into(),
            );
            self.gateway.forward_fire_and_forget(ext);
        }
    }
}

/// Map a `GoalEvent` to its snake_case wire name.
fn goal_event_as_str(event: &crate::session::goal_tracker::GoalEvent) -> &'static str {
    use crate::session::goal_tracker::GoalEvent;
    match event {
        GoalEvent::GoalCreated => "goal_created",
        GoalEvent::PlanningStarted => "planning_started",
        GoalEvent::PlanningCompleted => "planning_completed",
        GoalEvent::PlanningFailed => "planning_failed",
        GoalEvent::WorkerStarted => "worker_started",
        GoalEvent::WorkerCompleted => "worker_completed",
        GoalEvent::WorkerFailed => "worker_failed",
        GoalEvent::ContextRotated => "context_rotated",
        GoalEvent::GoalPaused => "goal_paused",
        GoalEvent::GoalResumed => "goal_resumed",
        GoalEvent::GoalCompleted => "goal_completed",
        GoalEvent::GoalCleared => "goal_cleared",
        GoalEvent::BudgetExceeded => "budget_exceeded",
        GoalEvent::PrematureStopDetected => "premature_stop_detected",
        GoalEvent::Unknown => "unknown",
    }
}

/// Build a `GoalUpdated` from an orchestration snapshot. `tokens_used`
/// already includes every subagent's marginal (live progress ticks
/// advance the records), so `o.live_subagent_tokens` is NOT folded in —
/// it ships only as the separate Active-subagent wire field.
pub(crate) fn build_goal_updated(
    o: &GoalOrchestration,
    tokens_used: i64,
    finished_subagent_tokens: i64,
) -> EchoAgentSessionUpdate {
    let last_entry = o.history.last();

    let status_str = match o.status {
        GoalStatus::Active => "active",
        GoalStatus::UserPaused => "user_paused",
        GoalStatus::BackOffPaused => "back_off_paused",
        GoalStatus::NoProgressPaused => "no_progress_paused",
        GoalStatus::InfraPaused => "infra_paused",
        GoalStatus::Blocked => "blocked",
        GoalStatus::BudgetLimited => "budget_limited",
        GoalStatus::Complete => "complete",
    };
    let phase_str = match o.phase {
        GoalPhase::Idle => "idle",
        GoalPhase::Planning => "planning",
        GoalPhase::Executing => "executing",
    };

    let last_event = last_entry.map(|e| goal_event_as_str(&e.event).to_owned());

    EchoAgentSessionUpdate::GoalUpdated {
        goal_id: o.goal_id.clone(),
        objective: o.objective.clone(),
        status: status_str.to_owned(),
        phase: phase_str.to_owned(),
        token_budget: o.token_budget,
        tokens_used,
        elapsed_ms: o.elapsed_ms,
        total_deliverables: 0,
        completed_deliverables: 0,
        current_deliverable_id: None,
        current_deliverable_title: None,
        current_subagent_role: o.current_subagent_role.clone(),
        total_worker_rounds: o.total_worker_rounds,
        total_verify_rounds: o.total_verify_rounds,
        token_baseline: o.token_baseline,
        finished_subagent_tokens,
        live_subagent_tokens: (o.live_subagent_tokens > 0).then_some(o.live_subagent_tokens),
        // Only transmit the breakdown when ≥2 distinct models appear; a
        // single-model (or all-inherit) goal collapses to the single
        // tokens line, so a 1-element vec is never sent. This makes the
        // wire-field doc literally true and is the single source of the
        // collapse rule (the pager re-checks ≥2 as defence in depth).
        // `o.live_tokens_by_model` is a live active-subagent-window field
        // (cleared on `SubagentFinished`, like `live_subagent_tokens`); the
        // pager renders it only under the "Active subagent" block, so this
        // producer must keep its populate gate on that same axis.
        live_tokens_by_model: if o.live_tokens_by_model.len() >= 2 {
            o.live_tokens_by_model.clone()
        } else {
            Vec::new()
        },
        live_context_pct: (o.live_context_pct > 0).then_some(o.live_context_pct),
        live_turn_count: (o.live_turn_count > 0).then_some(o.live_turn_count),
        live_tool_call_count: (o.live_tool_call_count > 0).then_some(o.live_tool_call_count),
        last_event,
        last_event_detail: last_entry.and_then(|e| e.detail.clone()),
        last_event_timestamp: last_entry.map(|e| e.timestamp.clone()),
        deliverables: Vec::new(),
        pause_message: o.pause_message.clone(),
        // Suppress the counter on the wire when no classifier run has
        // happened yet — mirrors the `total_worker_rounds`-style
        // convention used elsewhere for "no activity" counters.
        classifier_runs_attempted: (o.classifier_runs_attempted > 0)
            .then_some(o.classifier_runs_attempted),
        classifier_max_runs: o.classifier_max_runs,
        last_classifier_verdict: o.last_classifier_verdict,
        last_classifier_details_path: o.last_classifier_details_path.clone(),
        // Both badges are sourced from their `*_in_flight` latches (see the
        // field docs), NOT one-shot wire flags — so the token-accounting /
        // continuation `GoalUpdated`s that fire mid-run keep carrying the
        // correct flag instead of clearing it.
        verifying_completion: o.verifying_in_flight.then_some(true),
        planning: o.planning_in_flight.then_some(true),
    }
}

/// Build a `GoalUpdated` with `status: "cleared"` to tell the pager to
/// drop its goal state.
pub(crate) fn build_goal_cleared() -> EchoAgentSessionUpdate {
    EchoAgentSessionUpdate::GoalUpdated {
        goal_id: String::new(),
        objective: String::new(),
        status: "cleared".to_owned(),
        phase: "idle".to_owned(),
        token_budget: None,
        tokens_used: 0,
        elapsed_ms: 0,
        total_deliverables: 0,
        completed_deliverables: 0,
        current_deliverable_id: None,
        current_deliverable_title: None,
        current_subagent_role: None,
        total_worker_rounds: 0,
        total_verify_rounds: 0,
        token_baseline: 0,
        finished_subagent_tokens: 0,
        live_subagent_tokens: None,
        live_tokens_by_model: Vec::new(),
        live_context_pct: None,
        live_turn_count: None,
        live_tool_call_count: None,
        last_event: None,
        last_event_detail: None,
        last_event_timestamp: None,
        deliverables: Vec::new(),
        pause_message: None,
        classifier_runs_attempted: None,
        classifier_max_runs: None,
        last_classifier_verdict: None,
        last_classifier_details_path: None,
        verifying_completion: None,
        planning: None,
    }
}

/// Format elapsed milliseconds as a compact human-readable duration.
pub(crate) fn format_elapsed(ms: u64) -> String {
    let total_secs = ms / 1000;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours}h{mins:02}m")
    } else if mins > 0 {
        format!("{mins}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::goal_tracker::make_base_orchestration;

    #[test]
    fn format_elapsed_seconds() {
        assert_eq!(format_elapsed(5_000), "5s");
    }

    #[test]
    fn format_elapsed_minutes() {
        assert_eq!(format_elapsed(125_000), "2m05s");
    }

    #[test]
    fn format_elapsed_hours() {
        assert_eq!(format_elapsed(3_661_000), "1h01m");
    }

    #[test]
    fn build_goal_updated_empty_orchestration() {
        let o = make_base_orchestration();
        let update = build_goal_updated(&o, 0, 0);
        match update {
            EchoAgentSessionUpdate::GoalUpdated {
                total_deliverables,
                completed_deliverables,
                classifier_runs_attempted,
                classifier_max_runs,
                last_classifier_verdict,
                last_classifier_details_path,
                verifying_completion,
                planning,
                ..
            } => {
                assert_eq!(total_deliverables, 0);
                assert_eq!(completed_deliverables, 0);
                assert_eq!(classifier_runs_attempted, None);
                assert_eq!(classifier_max_runs, None);
                assert_eq!(last_classifier_verdict, None);
                assert_eq!(last_classifier_details_path, None);
                assert_eq!(verifying_completion, None);
                assert_eq!(planning, None);
            }
            _ => panic!("expected GoalUpdated"),
        }
    }

    #[test]
    fn build_goal_updated_emits_classifier_fields() {
        use crate::session::goal_tracker::GoalClassifierVerdict;

        let mut o = make_base_orchestration();
        o.classifier_runs_attempted = 2;
        o.classifier_max_runs = Some(3);
        o.last_classifier_verdict = Some(GoalClassifierVerdict::NotAchieved);
        o.last_classifier_details_path = Some("/tmp/details.md".into());

        let update = build_goal_updated(&o, 0, 0);
        match update {
            EchoAgentSessionUpdate::GoalUpdated {
                classifier_runs_attempted,
                classifier_max_runs,
                last_classifier_verdict,
                last_classifier_details_path,
                verifying_completion,
                ..
            } => {
                assert_eq!(classifier_runs_attempted, Some(2));
                assert_eq!(classifier_max_runs, Some(3));
                assert_eq!(
                    last_classifier_verdict,
                    Some(GoalClassifierVerdict::NotAchieved)
                );
                assert_eq!(
                    last_classifier_details_path.as_deref(),
                    Some("/tmp/details.md")
                );
                assert_eq!(verifying_completion, None);
            }
            _ => panic!("expected GoalUpdated"),
        }
    }

    #[test]
    fn build_goal_updated_planning_flag_reflects_latch() {
        let mut o = make_base_orchestration();
        assert_eq!(
            planning_field(&build_goal_updated(&o, 0, 0)),
            None,
            "latch off → no badge",
        );
        o.planning_in_flight = true;
        assert_eq!(
            planning_field(&build_goal_updated(&o, 0, 0)),
            Some(true),
            "latch on → every snapshot-derived update keeps the badge",
        );
    }

    fn planning_field(update: &EchoAgentSessionUpdate) -> Option<bool> {
        match update {
            EchoAgentSessionUpdate::GoalUpdated { planning, .. } => *planning,
            _ => panic!("expected GoalUpdated"),
        }
    }

    #[test]
    fn build_goal_updated_verifying_flag_reflects_latch() {
        // Regression: a one-shot verifying flag flickered off the moment any
        // mid-verification GoalUpdated fired. Sourced from the latch, every
        // snapshot-derived update keeps the "Verifying…" badge.
        let mut o = make_base_orchestration();
        let verifying = |u: &EchoAgentSessionUpdate| match u {
            EchoAgentSessionUpdate::GoalUpdated {
                verifying_completion,
                ..
            } => *verifying_completion,
            _ => panic!("expected GoalUpdated"),
        };
        assert_eq!(verifying(&build_goal_updated(&o, 0, 0)), None, "latch off");
        o.verifying_in_flight = true;
        assert_eq!(
            verifying(&build_goal_updated(&o, 0, 0)),
            Some(true),
            "latch on → badge persists across mid-verification updates",
        );
    }

    #[test]
    fn build_goal_updated_propagates_tokens_to_wire() {
        let mut o = make_base_orchestration();
        o.live_subagent_tokens = 5_000;
        let update = build_goal_updated(&o, 60_000, 40_000);
        match update {
            EchoAgentSessionUpdate::GoalUpdated {
                tokens_used,
                finished_subagent_tokens,
                live_subagent_tokens,
                ..
            } => {
                // Live tokens are already inside `tokens_used` via the
                // records; folding them in again would double-count.
                assert_eq!(tokens_used, 60_000);
                assert_eq!(finished_subagent_tokens, 40_000);
                assert_eq!(live_subagent_tokens, Some(5_000));
            }
            _ => panic!("expected GoalUpdated"),
        }
    }

    #[test]
    fn build_goal_updated_gates_per_model_breakdown_on_two_or_more_models() {
        // A single-model (or all-inherit) breakdown must NOT be transmitted
        // — the wire field stays empty so the doc contract holds and we
        // don't ship a 1-element vec the pager would collapse anyway.
        let mut o = make_base_orchestration();
        o.live_tokens_by_model = vec![("echo-agent-4".into(), 5_000)];
        match build_goal_updated(&o, 0, 0) {
            EchoAgentSessionUpdate::GoalUpdated {
                live_tokens_by_model,
                ..
            } => assert!(
                live_tokens_by_model.is_empty(),
                "single model must not be transmitted"
            ),
            _ => panic!("expected GoalUpdated"),
        }

        // ≥2 distinct models are transmitted verbatim.
        o.live_tokens_by_model = vec![
            ("echo-agent-4".into(), 5_000),
            ("echo-agent-3".into(), 3_000),
        ];
        match build_goal_updated(&o, 0, 0) {
            EchoAgentSessionUpdate::GoalUpdated {
                live_tokens_by_model,
                ..
            } => assert_eq!(
                live_tokens_by_model,
                vec![
                    ("echo-agent-4".to_owned(), 5_000),
                    ("echo-agent-3".to_owned(), 3_000)
                ]
            ),
            _ => panic!("expected GoalUpdated"),
        }
    }

    #[test]
    fn build_goal_cleared_is_cleared() {
        let update = build_goal_cleared();
        match update {
            EchoAgentSessionUpdate::GoalUpdated {
                status,
                classifier_runs_attempted,
                classifier_max_runs,
                last_classifier_verdict,
                last_classifier_details_path,
                verifying_completion,
                live_tokens_by_model,
                ..
            } => {
                assert_eq!(status, "cleared");
                assert_eq!(classifier_runs_attempted, None);
                assert_eq!(classifier_max_runs, None);
                assert_eq!(last_classifier_verdict, None);
                assert_eq!(last_classifier_details_path, None);
                assert_eq!(verifying_completion, None);
                assert!(live_tokens_by_model.is_empty());
            }
            _ => panic!("expected GoalUpdated"),
        }
    }
}
