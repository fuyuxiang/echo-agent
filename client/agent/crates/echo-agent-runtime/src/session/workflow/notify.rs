use crate::extensions::notification::{
    SessionNotification as EchoAgentSessionNotification, SessionUpdate as EchoAgentSessionUpdate,
    WorkflowAgentInfo, WorkflowPhaseInfo,
};
use crate::session::persistence::PersistenceMsg;

use super::store::WorkflowRunStore;
use super::tracker::WorkflowRunState;

#[derive(Clone)]
pub(crate) struct WorkflowNotifySender {
    session_id: agent_client_protocol::SessionId,
    gateway: echo_agent_acp::AcpAgentGatewaySender,
    persistence_tx: tokio::sync::mpsc::UnboundedSender<PersistenceMsg>,
    store: WorkflowRunStore,
}

impl WorkflowNotifySender {
    pub(crate) fn new(
        session_id: agent_client_protocol::SessionId,
        gateway: echo_agent_acp::AcpAgentGatewaySender,
        persistence_tx: tokio::sync::mpsc::UnboundedSender<PersistenceMsg>,
        store: WorkflowRunStore,
    ) -> Self {
        Self {
            session_id,
            gateway,
            persistence_tx,
            store,
        }
    }

    pub(crate) fn emit(&self, state: &WorkflowRunState, elapsed_ms: u64, active_agents: u32) {
        if let Err(error) = self.store.persist(state) {
            tracing::warn!(run_id = %state.run_id, %error, "failed to queue workflow manifest");
        }
        self.dispatch(
            build_workflow_updated(state, elapsed_ms, active_agents),
            true,
        );
    }

    pub(crate) fn emit_ephemeral(
        &self,
        state: &WorkflowRunState,
        elapsed_ms: u64,
        active_agents: u32,
    ) {
        if let Err(error) = self.store.persist(state) {
            tracing::warn!(run_id = %state.run_id, %error, "failed to queue workflow manifest");
        }
        self.dispatch(
            build_workflow_updated(state, elapsed_ms, active_agents),
            false,
        );
    }

    pub(crate) fn broadcast(
        &self,
        state: &WorkflowRunState,
        elapsed_ms: u64,
        active_agents: u32,
        persist_update: bool,
    ) {
        self.dispatch(
            build_workflow_updated(state, elapsed_ms, active_agents),
            persist_update,
        );
    }

    fn dispatch(&self, update: EchoAgentSessionUpdate, persist: bool) {
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

pub(crate) fn build_workflow_updated(
    state: &WorkflowRunState,
    elapsed_ms: u64,
    _active_agents: u32,
) -> EchoAgentSessionUpdate {
    let last = state.history.last();

    let run_complete = state.status == super::tracker::WorkflowRunStatus::Complete;
    let current_idx = state
        .current_phase
        .as_deref()
        .and_then(|cur| state.phases.iter().position(|p| p.title == cur));
    let phases = state
        .phases
        .iter()
        .enumerate()
        .map(|(idx, p)| WorkflowPhaseInfo {
            title: p.title.clone(),
            state: match current_idx {
                Some(cur) if idx < cur => "done",
                Some(cur) if idx == cur && run_complete => "done",
                Some(cur) if idx == cur => "active",
                _ => "pending",
            }
            .to_string(),
        })
        .collect();

    let agents: Vec<WorkflowAgentInfo> = state
        .agents
        .iter()
        .map(|a| WorkflowAgentInfo {
            agent_id: a.agent_id.clone(),
            label: a.label.clone(),
            phase: a.phase.clone(),
            model: a.model.clone(),
            state: a.state.clone(),
            tokens_used: a.tokens_used,
            duration_ms: a.duration_ms,
        })
        .collect();
    let current_agent_label = agents
        .iter()
        .rev()
        .find(|a| a.state == "running")
        .map(|a| a.label.clone());

    let active_agents = agents
        .iter()
        .filter(|agent| agent.state == "running")
        .count() as u32;

    let agents_reserved = 0u64;
    let agents_remaining = state
        .agent_budget
        .map(|total| total.saturating_sub(state.agents_used.saturating_add(agents_reserved)));

    EchoAgentSessionUpdate::WorkflowUpdated {
        run_id: state.run_id.clone(),
        revision: state.revision,
        name: state.name.clone(),
        objective: state.objective.clone(),
        status: state.status.as_str().to_string(),
        foreground: false,
        phases,
        current_phase: state.current_phase.clone(),
        agent_budget: state.agent_budget,
        agents_used: state.agents_used,
        agents_reserved,
        agents_remaining,
        agent_usage_incomplete: state.agent_usage_incomplete,
        elapsed_ms,
        active_agents,
        current_agent_label,
        agents,
        last_event: last.map(|e| e.event.clone()),
        last_event_detail: last.and_then(|e| e.detail.clone()),
        last_event_timestamp: last.map(|e| e.at.clone()),
        pause_message: state.pause_message.clone(),
        result_summary: state.result_summary.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::workflow::tracker::WorkflowTracker;

    #[test]
    fn phase_states_derive_from_current() {
        let mut t = WorkflowTracker::default();
        t.start_run(
            "wf_1".into(),
            "demo".into(),
            "obj".into(),
            vec![
                echo_agent_workflow::PhaseMeta {
                    title: "Plan".into(),
                    detail: None,
                },
                echo_agent_workflow::PhaseMeta {
                    title: "Execute".into(),
                    detail: None,
                },
                echo_agent_workflow::PhaseMeta {
                    title: "Verify".into(),
                    detail: None,
                },
            ],
            None,
            None,
        );
        t.set_phase("wf_1", "Execute");
        let state = t.get("wf_1").unwrap();
        match build_workflow_updated(&state, 0, 0) {
            EchoAgentSessionUpdate::WorkflowUpdated { phases, .. } => {
                let states: Vec<(String, String)> =
                    phases.into_iter().map(|p| (p.title, p.state)).collect();
                assert_eq!(
                    states,
                    vec![
                        ("Plan".to_string(), "done".to_string()),
                        ("Execute".to_string(), "active".to_string()),
                        ("Verify".to_string(), "pending".to_string()),
                    ]
                );
            }
            _ => panic!("expected WorkflowUpdated"),
        }
    }

    #[test]
    fn completed_run_marks_current_phase_done() {
        let mut t = WorkflowTracker::default();
        t.start_run(
            "wf_2".into(),
            "demo".into(),
            "obj".into(),
            vec![
                echo_agent_workflow::PhaseMeta {
                    title: "Plan".into(),
                    detail: None,
                },
                echo_agent_workflow::PhaseMeta {
                    title: "Verify".into(),
                    detail: None,
                },
            ],
            None,
            None,
        );
        t.set_phase("wf_2", "Verify");
        t.apply_outcome(
            "wf_2",
            &echo_agent_workflow::WorkflowOutcome::Completed {
                result: serde_json::json!("done"),
            },
        );
        let state = t.get("wf_2").unwrap();
        match build_workflow_updated(&state, 0, 0) {
            EchoAgentSessionUpdate::WorkflowUpdated { phases, status, .. } => {
                assert_eq!(status, "complete");
                let states: Vec<(String, String)> =
                    phases.into_iter().map(|p| (p.title, p.state)).collect();
                assert_eq!(
                    states,
                    vec![
                        ("Plan".to_string(), "done".to_string()),
                        ("Verify".to_string(), "done".to_string()),
                    ]
                );
            }
            _ => panic!("expected WorkflowUpdated"),
        }
    }
}
