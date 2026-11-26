use std::time::Instant;

use echo_agent_workflow::{PauseKind, PhaseMeta, WorkflowOutcome};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Active,
    UserPaused,
    BackOffPaused,
    NoProgressPaused,
    InfraPaused,
    Blocked,
    BudgetLimited,
    Interrupted,
    Complete,
    Failed,
    Cancelled,
}

impl WorkflowRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::UserPaused => "user_paused",
            Self::BackOffPaused => "back_off_paused",
            Self::NoProgressPaused => "no_progress_paused",
            Self::InfraPaused => "infra_paused",
            Self::Blocked => "blocked",
            Self::BudgetLimited => "budget_limited",
            Self::Interrupted => "interrupted",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Interrupted | Self::Complete | Self::Failed | Self::Cancelled
        )
    }

    pub(crate) fn is_completion_reportable(self) -> bool {
        self.is_terminal() || self == Self::BudgetLimited
    }

    pub fn is_paused(self) -> bool {
        matches!(
            self,
            Self::UserPaused
                | Self::BackOffPaused
                | Self::NoProgressPaused
                | Self::InfraPaused
                | Self::Blocked
                | Self::BudgetLimited
        )
    }

    pub(crate) fn is_resumable(self) -> bool {
        // Cancelled (`/workflow stop`) keeps the journal; resume continues
        // it the same way a pause does. Complete/interrupted stay terminal.
        self.is_paused() || self == Self::Failed || self == Self::Cancelled
    }

    fn from_pause(kind: PauseKind) -> Self {
        match kind {
            PauseKind::User => Self::UserPaused,
            PauseKind::BackOff => Self::BackOffPaused,
            PauseKind::NoProgress => Self::NoProgressPaused,
            PauseKind::Verification => Self::Blocked,
            PauseKind::Infra => Self::InfraPaused,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowHistoryEntry {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub at: String,
}

pub(crate) const WORKFLOW_HISTORY_MAX: usize = 64;
const WORKFLOW_PAUSE_MESSAGE_MAX_BYTES: usize = 4 * 1024;

fn capped_pause_message(message: impl Into<String>) -> String {
    let message = message.into();
    echo_agent_tools::util::truncate_str(&message, WORKFLOW_PAUSE_MESSAGE_MAX_BYTES).to_string()
}

fn default_label_for(agents: &[WorkflowAgentRow], phase: Option<&str>) -> String {
    let ordinal = agents
        .iter()
        .filter(|a| a.phase.as_deref() == phase)
        .count()
        + 1;
    match phase {
        Some(p) => {
            let slug: String = p
                .to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '-' })
                .collect::<String>()
                .split('-')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            format!("{slug}-{ordinal}")
        }
        None => format!("agent-{ordinal}"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowTokenLease {
    pub lease_id: String,
    pub grant: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowAgentRow {
    pub agent_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub state: String,
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default)]
    pub duration_ms: u64,
}

pub(crate) const WORKFLOW_AGENT_ROWS_MAX: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunState {
    pub run_id: String,
    #[serde(default)]
    pub revision: u64,
    pub name: String,
    pub objective: String,
    pub status: WorkflowRunStatus,
    #[serde(default)]
    pub phases: Vec<PhaseMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_budget: Option<u64>,
    #[serde(default)]
    pub agents_used: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub token_leases: Vec<WorkflowTokenLease>,
    #[serde(default)]
    pub agent_usage_incomplete: bool,
    #[serde(default)]
    pub elapsed_ms_floor: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<WorkflowHistoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<WorkflowAgentRow>,
}

impl WorkflowRunState {
    fn advance_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    fn record_event(&mut self, event: &str, detail: Option<String>) {
        self.advance_revision();
        self.history.push(WorkflowHistoryEntry {
            event: event.to_string(),
            detail,
            at: now_rfc3339(),
        });
        if self.history.len() > WORKFLOW_HISTORY_MAX {
            let excess = self.history.len() - WORKFLOW_HISTORY_MAX;
            self.history.drain(..excess);
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[derive(Debug, Default)]
pub(crate) struct WorkflowTracker {
    runs: Vec<TrackedRun>,
    reported_terminal_run_ids: std::collections::HashSet<String>,
    terminal_at_restore_run_ids: std::collections::HashSet<String>,
    status_reported_revisions: std::collections::HashMap<String, u64>,
}

#[derive(Debug)]
struct TrackedRun {
    state: WorkflowRunState,
    active_since: Option<Instant>,
    execution_epoch: u64,
}

impl WorkflowTracker {
    pub(crate) fn start_run(
        &mut self,
        run_id: String,
        name: String,
        objective: String,
        phases: Vec<PhaseMeta>,
        agent_budget: Option<u64>,
        journal_path: Option<String>,
    ) -> WorkflowRunState {
        let name = {
            let taken = |candidate: &str| self.runs.iter().any(|r| r.state.name == candidate);
            if !taken(&name) {
                name
            } else {
                let mut n = 2;
                loop {
                    let candidate = format!("{name}-{n}");
                    if !taken(&candidate) {
                        break candidate;
                    }
                    n += 1;
                }
            }
        };
        let mut state = WorkflowRunState {
            run_id,
            revision: 0,
            name,
            objective,
            status: WorkflowRunStatus::Active,
            phases,
            current_phase: None,
            agent_budget,
            agents_used: 0,
            token_leases: Vec::new(),
            agent_usage_incomplete: false,
            elapsed_ms_floor: 0,
            pause_message: None,
            history: Vec::new(),
            journal_path,
            result_summary: None,
            agents: Vec::new(),
        };
        state.record_event("workflow_started", None);
        self.runs.push(TrackedRun {
            state: state.clone(),
            active_since: Some(Instant::now()),
            execution_epoch: 0,
        });
        state
    }

    pub(crate) fn resume_run(
        &mut self,
        run_id: &str,
        new_agent_budget: Option<u64>,
    ) -> Option<WorkflowRunState> {
        let run = self.run_mut(run_id)?;
        if !run.state.status.is_resumable() {
            return None;
        }
        let candidate_budget = match new_agent_budget {
            Some(raised) => Some(raised.max(run.state.agent_budget.unwrap_or(0))),
            None => run.state.agent_budget,
        };
        let raised = new_agent_budget.is_some_and(|b| b > run.state.agent_budget.unwrap_or(0));
        if run.state.status == WorkflowRunStatus::BudgetLimited
            && (!raised || candidate_budget.is_some_and(|limit| run.state.agents_used >= limit))
        {
            return None;
        }
        if let Some(budget) = candidate_budget {
            run.state.agent_budget = Some(budget);
        }
        run.state.status = WorkflowRunStatus::Active;
        run.state.pause_message = None;
        run.state.result_summary = None;
        for agent in &mut run.state.agents {
            if agent.state == "running" {
                agent.state = "cancelled".to_string();
            }
        }
        run.state.record_event("workflow_resumed", None);
        run.active_since = Some(Instant::now());
        run.execution_epoch = run.execution_epoch.wrapping_add(1);
        let state = run.state.clone();
        self.reported_terminal_run_ids.remove(run_id);
        self.terminal_at_restore_run_ids.remove(run_id);
        Some(state)
    }

    pub(crate) fn set_phase(&mut self, run_id: &str, title: &str) -> Option<WorkflowRunState> {
        let run = self.run_mut(run_id)?;
        if run.state.current_phase.as_deref() != Some(title) {
            run.state.current_phase = Some(title.to_string());
            run.state
                .record_event("phase_entered", Some(title.to_string()));
        }
        Some(run.state.clone())
    }

    #[cfg(test)]
    pub(crate) fn remaining_agents(&self, run_id: &str) -> Option<Option<u64>> {
        let run = self.runs.iter().find(|run| run.state.run_id == run_id)?;
        Some(
            run.state
                .agent_budget
                .map(|limit| limit.saturating_sub(run.state.agents_used)),
        )
    }

    pub(crate) fn reserve_agents(
        &mut self,
        run_id: &str,
        count: u64,
    ) -> Result<WorkflowRunState, (u64, u64)> {
        let Some(run) = self.run_mut(run_id) else {
            return Err((count, 0));
        };
        let requested = run.state.agents_used.saturating_add(count);
        if let Some(limit) = run.state.agent_budget
            && requested > limit
        {
            return Err((requested, limit));
        }
        run.state.agents_used = requested;
        run.state.advance_revision();
        Ok(run.state.clone())
    }

    pub(crate) fn release_agents(&mut self, run_id: &str, count: u64) -> Option<WorkflowRunState> {
        let run = self.run_mut(run_id)?;
        run.state.agents_used = run.state.agents_used.saturating_sub(count);
        run.state.advance_revision();
        Some(run.state.clone())
    }

    pub(crate) fn execution_epoch(&self, run_id: &str) -> Option<u64> {
        self.runs
            .iter()
            .find(|run| run.state.run_id == run_id)
            .map(|run| run.execution_epoch)
    }

    pub(crate) fn reconcile_agents_used(
        &mut self,
        run_id: &str,
        used: u64,
    ) -> Option<WorkflowRunState> {
        let run = self.run_mut(run_id)?;
        if run.state.agents_used != used {
            run.state.agents_used = used;
            run.state.advance_revision();
        }
        Some(run.state.clone())
    }

    #[cfg(test)]
    pub(crate) fn fail_budget_accounting(
        &mut self,
        run_id: &str,
        message: impl Into<String>,
    ) -> Option<WorkflowRunState> {
        let run = self.run_mut(run_id)?;
        let reserved = run
            .state
            .token_leases
            .drain(..)
            .fold(0u64, |sum, lease| sum.saturating_add(lease.grant));
        run.state.agents_used = run.state.agents_used.saturating_add(reserved);
        if let Some(limit) = run.state.agent_budget {
            run.state.agents_used = run.state.agents_used.max(limit);
        }
        run.state.agent_usage_incomplete = true;
        let message = capped_pause_message(message);
        run.state.record_event(
            "workflow_budget_accounting_incomplete",
            Some(message.clone()),
        );
        run.fold_elapsed();
        run.state.status = WorkflowRunStatus::Interrupted;
        run.state.pause_message = Some(message);
        run.state.result_summary = None;
        Some(run.state.clone())
    }

    pub(crate) fn agent_started(&mut self, run_id: &str, mut row: WorkflowAgentRow) -> String {
        let Some(run) = self.run_mut(run_id) else {
            return if row.label.is_empty() {
                "agent".to_string()
            } else {
                row.label
            };
        };
        if row.phase.is_none() {
            row.phase = run.state.current_phase.clone();
        }
        if row.label.is_empty() {
            row.label = default_label_for(&run.state.agents, row.phase.as_deref());
        }
        let label = row.label.clone();
        run.state.advance_revision();
        run.state.agents.push(row);
        if run.state.agents.len() > WORKFLOW_AGENT_ROWS_MAX
            && let Some(pos) = run.state.agents.iter().position(|a| a.state != "running")
        {
            run.state.agents.remove(pos);
        }
        label
    }

    /// Point a roster row at a fresh child session id. Contract retries
    /// spawn a new child session per attempt; the row must follow so live
    /// progress lookups and transcript clicks resolve to the current child.
    pub(crate) fn rebind_agent_id(&mut self, run_id: &str, agent_id: &str, new_agent_id: &str) {
        let Some(run) = self.run_mut(run_id) else {
            return;
        };
        if let Some(row) = run.state.agents.iter_mut().find(|a| a.agent_id == agent_id) {
            row.agent_id = new_agent_id.to_string();
            run.state.advance_revision();
        }
    }

    pub(crate) fn agent_finished(
        &mut self,
        run_id: &str,
        agent_id: &str,
        state: &str,
        tokens_used: u64,
        duration_ms: u64,
    ) {
        let Some(run) = self.run_mut(run_id) else {
            return;
        };
        if let Some(row) = run.state.agents.iter_mut().find(|a| a.agent_id == agent_id) {
            row.state = state.to_string();
            row.tokens_used = tokens_used;
            row.duration_ms = duration_ms;
            run.state.advance_revision();
        }
    }

    pub(crate) fn log_message(&mut self, run_id: &str, message: &str) -> Option<WorkflowRunState> {
        let run = self.run_mut(run_id)?;
        run.state.record_event("log", Some(message.to_string()));
        Some(run.state.clone())
    }

    pub(crate) fn pause_user(
        &mut self,
        run_id: &str,
        message: Option<String>,
    ) -> Option<WorkflowRunState> {
        let run = self.run_mut(run_id)?;
        if run.state.status != WorkflowRunStatus::Active {
            return Some(run.state.clone());
        }
        run.fold_elapsed();
        run.state.status = WorkflowRunStatus::UserPaused;
        run.state.pause_message = message.map(capped_pause_message);
        run.state
            .record_event("workflow_paused", Some("user".into()));
        Some(run.state.clone())
    }

    pub(crate) fn interrupt(
        &mut self,
        run_id: &str,
        message: impl Into<String>,
    ) -> Option<WorkflowRunState> {
        let run = self.run_mut(run_id)?;
        run.fold_elapsed();
        let message = capped_pause_message(message);
        run.state.status = WorkflowRunStatus::Interrupted;
        run.state.pause_message = Some(message.clone());
        run.state.result_summary = None;
        run.state
            .record_event("workflow_interrupted", Some(message));
        Some(run.state.clone())
    }

    pub(crate) fn apply_outcome(
        &mut self,
        run_id: &str,
        outcome: &WorkflowOutcome,
    ) -> Option<WorkflowRunState> {
        let run = self.run_mut(run_id)?;
        run.fold_elapsed();
        if matches!(
            run.state.status,
            WorkflowRunStatus::Interrupted
                | WorkflowRunStatus::Failed
                | WorkflowRunStatus::Cancelled
        ) {
            run.state.record_event(
                "workflow_outcome_ignored",
                Some(format!(
                    "ignored {} while status is {}",
                    outcome_kind(outcome),
                    run.state.status.as_str()
                )),
            );
            return Some(run.state.clone());
        }
        match outcome {
            WorkflowOutcome::Completed { result } => {
                run.state.status = WorkflowRunStatus::Complete;
                run.state.result_summary = Some(summarize_result(result));
                run.state.record_event("workflow_completed", None);
            }
            WorkflowOutcome::Paused { kind, message } => {
                run.state.status = WorkflowRunStatus::from_pause(*kind);
                run.state.pause_message = Some(capped_pause_message(message.clone()));
                run.state
                    .record_event("workflow_paused", Some(kind.as_str().to_string()));
            }
            WorkflowOutcome::BudgetExceeded { message } => {
                run.state.status = WorkflowRunStatus::BudgetLimited;
                let hint = if run.state.agents_used >= echo_agent_workflow::MAX_AGENT_BUDGET {
                    "finished work is kept, but this run reached the maximum agent budget and \
                     cannot be resumed; start a new run"
                } else {
                    "finished work is kept; resume the run with a higher absolute agent budget \
                     to continue"
                };
                run.state.pause_message = Some(capped_pause_message(format!("{message} — {hint}")));
                run.state.record_event("workflow_budget_limited", None);
            }
            WorkflowOutcome::Cancelled => {
                run.state.status = WorkflowRunStatus::Cancelled;
                run.state.record_event("workflow_cancelled", None);
            }
            WorkflowOutcome::Failed { error } => {
                run.state.status = WorkflowRunStatus::Failed;
                let error = capped_pause_message(error.clone());
                run.state.pause_message = Some(error.clone());
                run.state.record_event("workflow_failed", Some(error));
            }
        }
        Some(run.state.clone())
    }

    pub(crate) fn clear_run(&mut self, run_id: &str) -> Option<WorkflowRunState> {
        let idx = self.runs.iter().position(|r| r.state.run_id == run_id)?;
        let mut removed = self.runs.remove(idx);
        removed.fold_elapsed();
        removed.state.record_event("workflow_cleared", None);
        Some(removed.state)
    }

    pub(crate) fn get(&self, run_id: &str) -> Option<WorkflowRunState> {
        self.runs
            .iter()
            .find(|r| r.state.run_id == run_id)
            .map(|r| r.state.clone())
    }

    pub(crate) fn list(&self) -> Vec<WorkflowRunState> {
        self.runs.iter().map(|r| r.state.clone()).collect()
    }

    pub(crate) fn elapsed_ms(&self, run_id: &str) -> u64 {
        self.runs
            .iter()
            .find(|r| r.state.run_id == run_id)
            .map(TrackedRun::live_elapsed_ms)
            .unwrap_or(0)
    }

    pub(crate) fn from_snapshot(snapshots: Vec<WorkflowRunState>) -> Self {
        let runs = snapshots
            .into_iter()
            .map(|mut state| {
                if !state.token_leases.is_empty() {
                    let charge = state
                        .token_leases
                        .drain(..)
                        .fold(0u64, |sum, lease| sum.saturating_add(lease.grant));
                    state.agents_used = state.agents_used.saturating_add(charge);
                    state.agent_usage_incomplete = true;
                    state.record_event(
                        "workflow_budget_accounting_incomplete",
                        Some("unresolved reservations charged on restore".into()),
                    );
                }
                if state.status == WorkflowRunStatus::Active {
                    state.status = WorkflowRunStatus::Interrupted;
                    state.pause_message = Some(
                        "the session ended while this workflow was active; start a new run"
                            .to_string(),
                    );
                    state.record_event(
                        "workflow_interrupted",
                        Some("restored_without_stable_operation_identity".into()),
                    );
                }
                let mut cancelled_ghost = false;
                for agent in &mut state.agents {
                    if agent.state == "running" {
                        agent.state = "cancelled".to_string();
                        cancelled_ghost = true;
                    }
                }
                if cancelled_ghost {
                    state.advance_revision();
                }
                TrackedRun {
                    state,
                    active_since: None,
                    execution_epoch: 0,
                }
            })
            .collect::<Vec<TrackedRun>>();
        let terminal_at_restore_run_ids = runs
            .iter()
            .filter(|r| r.state.status.is_completion_reportable())
            .map(|r| r.state.run_id.clone())
            .collect();
        Self {
            runs,
            reported_terminal_run_ids: std::collections::HashSet::new(),
            terminal_at_restore_run_ids,
            status_reported_revisions: std::collections::HashMap::new(),
        }
    }

    pub(crate) fn is_unreported_completion(&self, run_id: &str, revision: u64) -> bool {
        self.runs.iter().any(|run| {
            run.state.run_id == run_id
                && run.state.revision == revision
                && run.state.status.is_completion_reportable()
                && !self.reported_terminal_run_ids.contains(run_id)
        })
    }

    pub(crate) fn take_unreported_terminal_runs(
        &mut self,
    ) -> (Vec<WorkflowRunState>, Vec<WorkflowRunState>) {
        let mut restored = Vec::new();
        let mut fresh = Vec::new();
        for run in &self.runs {
            if !run.state.status.is_completion_reportable()
                || self.reported_terminal_run_ids.contains(&run.state.run_id)
            {
                continue;
            }
            if self.terminal_at_restore_run_ids.contains(&run.state.run_id) {
                restored.push(run.state.clone());
            } else {
                fresh.push(run.state.clone());
            }
        }
        for state in restored.iter().chain(fresh.iter()) {
            self.reported_terminal_run_ids.insert(state.run_id.clone());
        }
        (restored, fresh)
    }

    pub(crate) fn snapshot(&self) -> Vec<WorkflowRunState> {
        self.list()
    }

    pub(crate) fn take_status_report(&mut self) -> Vec<WorkflowRunState> {
        let live: Vec<&TrackedRun> = self
            .runs
            .iter()
            .filter(|r| !r.state.status.is_completion_reportable())
            .collect();
        let moved = live.iter().any(|r| {
            self.status_reported_revisions.get(&r.state.run_id) != Some(&r.state.revision)
        });
        if !moved {
            return Vec::new();
        }
        let report: Vec<WorkflowRunState> = live
            .iter()
            .map(|run| {
                let mut state = run.state.clone();
                state.elapsed_ms_floor = run.live_elapsed_ms();
                state
            })
            .collect();
        let current_ids: std::collections::HashSet<&str> =
            self.runs.iter().map(|r| r.state.run_id.as_str()).collect();
        self.status_reported_revisions
            .retain(|id, _| current_ids.contains(id.as_str()));
        for state in &report {
            self.status_reported_revisions
                .insert(state.run_id.clone(), state.revision);
        }
        report
    }

    fn run_mut(&mut self, run_id: &str) -> Option<&mut TrackedRun> {
        self.runs.iter_mut().find(|r| r.state.run_id == run_id)
    }
}

impl TrackedRun {
    fn live_elapsed_ms(&self) -> u64 {
        self.state.elapsed_ms_floor.saturating_add(
            self.active_since
                .map(|since| since.elapsed().as_millis() as u64)
                .unwrap_or(0),
        )
    }

    fn fold_elapsed(&mut self) {
        if self.active_since.is_some() {
            self.state.elapsed_ms_floor = self.live_elapsed_ms();
            self.active_since = None;
        }
    }
}

fn outcome_kind(outcome: &WorkflowOutcome) -> &'static str {
    match outcome {
        WorkflowOutcome::Completed { .. } => "completed",
        WorkflowOutcome::Paused { .. } => "paused",
        WorkflowOutcome::BudgetExceeded { .. } => "budget_exceeded",
        WorkflowOutcome::Cancelled => "cancelled",
        WorkflowOutcome::Failed { .. } => "failed",
    }
}

fn summarize_result(result: &serde_json::Value) -> String {
    const MAX: usize = 16 * 1024;
    let text = match result {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "done".to_string(),
        serde_json::Value::Object(map) => match (
            map.get("report").and_then(|r| r.as_str()),
            map.get("path").and_then(|p| p.as_str()),
        ) {
            (Some(report), Some(path)) => format!("{report}\n\n_Full report: {path}_"),
            (Some(report), None) => report.to_string(),
            _ => serde_json::Value::Object(map.clone()).to_string(),
        },
        other => other.to_string(),
    };
    if text.len() > MAX {
        let mut end = MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker_with_run() -> (WorkflowTracker, String) {
        let mut t = WorkflowTracker::default();
        let state = t.start_run(
            "wf_1".into(),
            "goal".into(),
            "ship it".into(),
            vec![],
            Some(1000),
            None,
        );
        assert_eq!(state.status, WorkflowRunStatus::Active);
        (t, "wf_1".into())
    }

    #[test]
    fn outcome_transitions() {
        let (mut t, id) = tracker_with_run();
        let s = t
            .apply_outcome(
                &id,
                &WorkflowOutcome::Paused {
                    kind: PauseKind::BackOff,
                    message: "cap".into(),
                },
            )
            .unwrap();
        assert_eq!(s.status, WorkflowRunStatus::BackOffPaused);
        assert_eq!(s.pause_message.as_deref(), Some("cap"));

        let s = t.resume_run(&id, None).unwrap();
        assert_eq!(s.status, WorkflowRunStatus::Active);
        assert!(s.pause_message.is_none());

        let s = t
            .apply_outcome(
                &id,
                &WorkflowOutcome::Completed {
                    result: serde_json::json!("shipped"),
                },
            )
            .unwrap();
        assert_eq!(s.status, WorkflowRunStatus::Complete);
        assert_eq!(s.result_summary.as_deref(), Some("shipped"));
    }

    #[test]
    fn rebind_agent_id_points_row_at_retry_child_and_bumps_revision() {
        let (mut t, id) = tracker_with_run();
        t.agent_started(
            &id,
            WorkflowAgentRow {
                agent_id: "child-attempt-1".into(),
                label: "worker".into(),
                phase: None,
                model: None,
                state: "running".into(),
                tokens_used: 0,
                duration_ms: 0,
            },
        );
        let before = t.get(&id).unwrap().revision;
        t.rebind_agent_id(&id, "child-attempt-1", "child-attempt-2");
        let run = t.get(&id).unwrap();
        assert_eq!(run.agents.len(), 1);
        assert_eq!(run.agents[0].agent_id, "child-attempt-2");
        assert_eq!(run.agents[0].label, "worker");
        assert_eq!(run.agents[0].state, "running");
        assert!(run.revision > before);

        t.agent_finished(&id, "child-attempt-2", "done", 42, 1_000);
        assert_eq!(t.get(&id).unwrap().agents[0].state, "done");
    }

    #[test]
    fn snapshot_restore_marks_active_non_resumable_interrupted() {
        let (t, _) = tracker_with_run();
        let restored = WorkflowTracker::from_snapshot(t.snapshot());
        let run = restored.get("wf_1").unwrap();
        assert_eq!(run.status, WorkflowRunStatus::Interrupted);
        assert!(!run.status.is_paused());
    }

    #[test]
    fn snapshot_restore_interrupts_run_and_cancels_ghost_agent_rows() {
        let (mut t, id) = tracker_with_run();
        t.agent_started(
            &id,
            WorkflowAgentRow {
                agent_id: "child".into(),
                label: "worker".into(),
                phase: None,
                model: None,
                state: "running".into(),
                tokens_used: 0,
                duration_ms: 0,
            },
        );
        let restored = WorkflowTracker::from_snapshot(t.snapshot());
        let run = restored.get(&id).unwrap();
        assert_eq!(run.status, WorkflowRunStatus::Interrupted);
        assert!(!run.status.is_paused());
        assert_eq!(run.agents[0].state, "cancelled");
    }

    #[test]
    fn resume_rejects_nonresumable_states() {
        let (mut t, id) = tracker_with_run();
        t.interrupt(&id, "lost executor").unwrap();
        assert!(t.resume_run(&id, None).is_none());
        assert_eq!(t.get(&id).unwrap().status, WorkflowRunStatus::Interrupted);

        let (mut t, id) = tracker_with_run();
        t.apply_outcome(&id, &WorkflowOutcome::Cancelled);
        let resumed = t.resume_run(&id, None).expect("cancelled is resumable");
        assert_eq!(resumed.status, WorkflowRunStatus::Active);

        let (mut t, id) = tracker_with_run();
        t.apply_outcome(
            &id,
            &WorkflowOutcome::Completed {
                result: serde_json::json!("done"),
            },
        );
        assert!(t.resume_run(&id, None).is_none());
        assert_eq!(t.get(&id).unwrap().status, WorkflowRunStatus::Complete);
    }

    #[test]
    fn failed_run_resumes_to_active_bumps_epoch_and_cancels_ghost_agents() {
        let (mut t, id) = tracker_with_run();
        t.agent_started(
            &id,
            WorkflowAgentRow {
                agent_id: "child".into(),
                label: "worker".into(),
                phase: None,
                model: None,
                state: "running".into(),
                tokens_used: 0,
                duration_ms: 0,
            },
        );
        t.apply_outcome(
            &id,
            &WorkflowOutcome::Failed {
                error: "scratch byte quota exceeded".into(),
            },
        );
        let failed = t.get(&id).unwrap();
        assert_eq!(failed.status, WorkflowRunStatus::Failed);
        assert!(failed.status.is_resumable());
        assert!(failed.status.is_terminal());
        assert!(!failed.status.is_paused());
        assert_eq!(t.execution_epoch(&id), Some(0));

        let resumed = t.resume_run(&id, None).unwrap();
        assert_eq!(resumed.status, WorkflowRunStatus::Active);
        assert!(resumed.pause_message.is_none());
        assert_eq!(
            resumed.agents[0].state, "cancelled",
            "ghost running agent rows must be cancelled on resume"
        );
        assert_eq!(t.execution_epoch(&id), Some(1));
    }

    #[test]
    fn apply_outcome_does_not_demote_interrupted_to_complete() {
        let (mut t, id) = tracker_with_run();
        t.fail_budget_accounting(&id, "settlement persist failed");
        assert_eq!(t.get(&id).unwrap().status, WorkflowRunStatus::Interrupted);
        t.apply_outcome(
            &id,
            &WorkflowOutcome::Completed {
                result: serde_json::json!("done"),
            },
        );
        let run = t.get(&id).unwrap();
        assert_eq!(run.status, WorkflowRunStatus::Interrupted);
        assert!(
            run.history
                .iter()
                .any(|e| e.event == "workflow_outcome_ignored"),
            "expected ignored-outcome history entry, got {:?}",
            run.history
        );
    }

    #[test]
    fn apply_outcome_does_not_demote_interrupted_to_cancelled() {
        let (mut t, id) = tracker_with_run();
        t.fail_budget_accounting(&id, "settlement persist failed");
        t.apply_outcome(&id, &WorkflowOutcome::Cancelled);
        assert_eq!(t.get(&id).unwrap().status, WorkflowRunStatus::Interrupted);
    }

    #[test]
    fn apply_outcome_does_not_demote_cancelled_to_complete() {
        let (mut t, id) = tracker_with_run();
        t.apply_outcome(&id, &WorkflowOutcome::Cancelled);
        assert_eq!(t.get(&id).unwrap().status, WorkflowRunStatus::Cancelled);
        t.apply_outcome(
            &id,
            &WorkflowOutcome::Completed {
                result: serde_json::json!("done"),
            },
        );
        let run = t.get(&id).unwrap();
        assert_eq!(run.status, WorkflowRunStatus::Cancelled);
        assert!(
            run.history
                .iter()
                .any(|e| e.event == "workflow_outcome_ignored"),
            "expected ignored-outcome history entry, got {:?}",
            run.history
        );
    }

    #[test]
    fn apply_outcome_does_not_demote_cancelled_to_budget_exceeded() {
        let (mut t, id) = tracker_with_run();
        t.apply_outcome(&id, &WorkflowOutcome::Cancelled);
        t.apply_outcome(
            &id,
            &WorkflowOutcome::BudgetExceeded {
                message: "spent".into(),
            },
        );
        let run = t.get(&id).unwrap();
        assert_eq!(run.status, WorkflowRunStatus::Cancelled);
        assert!(
            run.history
                .iter()
                .any(|e| e.event == "workflow_outcome_ignored"),
            "expected ignored-outcome history entry, got {:?}",
            run.history
        );
    }

    #[test]
    fn release_agents_returns_used_to_prior_saturating() {
        let (mut t, id) = tracker_with_run();
        t.reserve_agents(&id, 100).unwrap();
        assert_eq!(t.get(&id).unwrap().agents_used, 100);
        let before_rev = t.get(&id).unwrap().revision;
        let state = t.release_agents(&id, 40).unwrap();
        assert_eq!(state.agents_used, 60);
        assert!(state.revision > before_rev);
        assert_eq!(t.remaining_agents(&id), Some(Some(940)));
        let state = t.release_agents(&id, 999).unwrap();
        assert_eq!(state.agents_used, 0);
        assert_eq!(t.remaining_agents(&id), Some(Some(1000)));
        assert!(t.release_agents("missing", 1).is_none());
    }

    #[test]
    fn reconcile_agents_used_sets_absolute_count() {
        let (mut t, id) = tracker_with_run();
        t.reserve_agents(&id, 5).unwrap();
        let before_rev = t.get(&id).unwrap().revision;
        let state = t.reconcile_agents_used(&id, 2).unwrap();
        assert_eq!(state.agents_used, 2);
        assert!(state.revision > before_rev);
        assert_eq!(t.remaining_agents(&id), Some(Some(998)));
        let steady_rev = t.get(&id).unwrap().revision;
        t.reconcile_agents_used(&id, 2).unwrap();
        assert_eq!(
            t.get(&id).unwrap().revision,
            steady_rev,
            "reconciling to an unchanged value must not churn the revision"
        );
        assert!(t.reconcile_agents_used("missing", 0).is_none());
    }

    #[test]
    fn execution_epoch_advances_only_on_resume() {
        let (mut t, id) = tracker_with_run();
        assert_eq!(t.execution_epoch(&id), Some(0));
        t.reserve_agents(&id, 1).unwrap();
        assert_eq!(
            t.execution_epoch(&id),
            Some(0),
            "non-resume mutations must not advance the execution epoch"
        );
        t.pause_user(&id, None);
        assert_eq!(
            t.resume_run(&id, None).map(|s| s.status),
            Some(WorkflowRunStatus::Active)
        );
        assert_eq!(
            t.execution_epoch(&id),
            Some(1),
            "resume starts a new execution so a stale watcher's captured epoch no longer matches"
        );
        t.pause_user(&id, None);
        t.resume_run(&id, None);
        assert_eq!(t.execution_epoch(&id), Some(2));
        assert_eq!(t.execution_epoch("missing"), None);
    }

    #[test]
    fn resume_run_insufficient_raise_after_overshoot_leaves_budget_unchanged() {
        let (mut t, id) = tracker_with_run();
        t.run_mut(&id).unwrap().state.agents_used = 1200;
        t.apply_outcome(
            &id,
            &WorkflowOutcome::BudgetExceeded {
                message: "spent".into(),
            },
        );
        assert_eq!(t.get(&id).unwrap().agent_budget, Some(1000));
        assert!(t.resume_run(&id, Some(1100)).is_none());
        assert_eq!(t.get(&id).unwrap().agent_budget, Some(1000));
        assert_eq!(t.get(&id).unwrap().status, WorkflowRunStatus::BudgetLimited);
    }

    #[test]
    fn fail_budget_accounting_never_lowers_overshoot_spend() {
        let (mut t, id) = tracker_with_run();
        t.run_mut(&id).unwrap().state.agents_used = 1500;
        let state = t.fail_budget_accounting(&id, "persist failed").unwrap();
        assert_eq!(state.agents_used, 1500);
        assert!(state.agent_usage_incomplete);
        assert_eq!(state.status, WorkflowRunStatus::Interrupted);
    }

    #[test]
    fn budget_limited_resume_preserves_cap_and_stays_stopped() {
        let (mut t, id) = tracker_with_run();
        t.apply_outcome(
            &id,
            &WorkflowOutcome::BudgetExceeded {
                message: "spent".into(),
            },
        );
        assert!(t.resume_run(&id, None).is_none());
        assert_eq!(t.get(&id).unwrap().agent_budget, Some(1000));
    }

    #[test]
    fn budget_limited_resume_with_raised_cap_reactivates_and_rereports() {
        let (mut t, id) = tracker_with_run();
        assert_eq!(t.take_status_report().len(), 1);
        t.reserve_agents(&id, 1000).ok();
        t.apply_outcome(
            &id,
            &WorkflowOutcome::BudgetExceeded {
                message: "spent".into(),
            },
        );
        let (_, fresh) = t.take_unreported_terminal_runs();
        assert_eq!(fresh.len(), 1);
        assert!(t.take_unreported_terminal_runs().1.is_empty());
        assert!(t.take_status_report().is_empty());
        assert!(t.resume_run(&id, Some(500)).is_none());
        let state = t.resume_run(&id, Some(1024)).unwrap();
        assert_eq!(state.status, WorkflowRunStatus::Active);
        assert_eq!(state.agent_budget, Some(1024));
        let resumed_status = t.take_status_report();
        assert_eq!(resumed_status.len(), 1);
        assert_eq!(resumed_status[0].revision, state.revision);
        t.apply_outcome(
            &id,
            &WorkflowOutcome::Completed {
                result: serde_json::json!("ok"),
            },
        );
        assert_eq!(t.take_unreported_terminal_runs().1.len(), 1);
    }

    #[test]
    fn restored_budget_limited_run_is_reported_as_pre_resume_not_status() {
        let (mut t, id) = tracker_with_run();
        t.reserve_agents(&id, 1000).ok();
        t.apply_outcome(
            &id,
            &WorkflowOutcome::BudgetExceeded {
                message: "spent".into(),
            },
        );

        let mut restored = WorkflowTracker::from_snapshot(t.snapshot());
        let (pre_resume, fresh) = restored.take_unreported_terminal_runs();
        assert_eq!(pre_resume.len(), 1);
        assert_eq!(pre_resume[0].run_id, id);
        assert!(fresh.is_empty());
        assert!(restored.take_status_report().is_empty());

        let resumed = restored.resume_run(&id, Some(1_024)).unwrap();
        assert_eq!(resumed.status, WorkflowRunStatus::Active);
        restored.apply_outcome(
            &id,
            &WorkflowOutcome::Completed {
                result: serde_json::json!("done later"),
            },
        );
        let (pre_resume, fresh) = restored.take_unreported_terminal_runs();
        assert!(pre_resume.is_empty());
        assert_eq!(fresh.len(), 1);
    }

    #[test]
    fn active_status_snapshot_includes_live_elapsed_without_folding_it() {
        let (mut t, id) = tracker_with_run();
        let run = t.run_mut(&id).unwrap();
        run.state.elapsed_ms_floor = 2_000;
        run.active_since = Some(Instant::now() - std::time::Duration::from_millis(5_000));

        let report = t.take_status_report();
        assert_eq!(report.len(), 1);
        assert!(report[0].elapsed_ms_floor >= 7_000);
        assert_eq!(t.get(&id).unwrap().elapsed_ms_floor, 2_000);
    }

    #[test]
    fn history_is_capped() {
        let (mut t, id) = tracker_with_run();
        for i in 0..(WORKFLOW_HISTORY_MAX + 10) {
            t.set_phase(&id, &format!("phase-{i}"));
        }
        let run = t.get(&id).unwrap();
        assert_eq!(run.history.len(), WORKFLOW_HISTORY_MAX);
    }

    #[test]
    fn phase_dedupes_consecutive() {
        let (mut t, id) = tracker_with_run();
        t.set_phase(&id, "Scan");
        t.set_phase(&id, "Scan");
        let run = t.get(&id).unwrap();
        let phase_events = run
            .history
            .iter()
            .filter(|e| e.event == "phase_entered")
            .count();
        assert_eq!(phase_events, 1);
    }

    #[test]
    fn reservations_accumulate_and_shrink_remaining() {
        let (mut t, id) = tracker_with_run();
        let state = t.reserve_agents(&id, 125).unwrap();
        assert_eq!(state.agents_used, 125);
        assert_eq!(t.remaining_agents(&id), Some(Some(875)));
        let state = t.reserve_agents(&id, 50).unwrap();
        assert_eq!(state.agents_used, 175);
        assert_eq!(t.remaining_agents(&id), Some(Some(825)));
        assert!(!state.agent_usage_incomplete);
    }

    #[test]
    fn incomplete_accounting_remains_sticky_across_reservations() {
        let (mut t, id) = tracker_with_run();
        t.run_mut(&id).unwrap().state.agent_usage_incomplete = true;
        t.reserve_agents(&id, 100).unwrap();
        let state = t.reserve_agents(&id, 40).unwrap();
        assert_eq!(state.agents_used, 140);
        assert!(state.agent_usage_incomplete);
        assert_eq!(t.remaining_agents(&id), Some(Some(860)));
        let state = t.reserve_agents(&id, 25).unwrap();
        assert_eq!(state.agents_used, 165);
        assert!(state.agent_usage_incomplete);
    }

    #[test]
    fn reservation_over_budget_is_atomic() {
        let (mut t, id) = tracker_with_run();
        t.reserve_agents(&id, 900).unwrap();
        assert_eq!(t.reserve_agents(&id, 300).unwrap_err(), (1200, 1000));
        assert_eq!(t.get(&id).unwrap().agents_used, 900);
        assert_eq!(t.remaining_agents(&id), Some(Some(100)));
    }

    #[test]
    fn unbudgeted_run_reserves_without_remaining() {
        let mut t = WorkflowTracker::default();
        let id = "wf_unlimited";
        t.start_run(
            id.into(),
            "demo".into(),
            "obj".into(),
            Vec::new(),
            None,
            None,
        );
        t.reserve_agents(id, 20).ok();
        assert_eq!(t.get(id).unwrap().agents_used, 20);
        assert_eq!(t.remaining_agents(id), Some(None));
    }

    #[test]
    fn restore_charges_unresolved_lease_and_preserves_cap() {
        let (mut t, id) = tracker_with_run();
        let mut snapshot = t.snapshot();
        snapshot[0].token_leases.push(WorkflowTokenLease {
            lease_id: "lease_legacy".into(),
            grant: 400,
        });
        let _ = &mut t;
        let restored = WorkflowTracker::from_snapshot(snapshot);
        let run = restored.get(&id).unwrap();
        assert_eq!(run.agent_budget, Some(1000));
        assert_eq!(run.agents_used, 400);
        assert!(run.token_leases.is_empty());
        assert!(run.agent_usage_incomplete);
        assert_eq!(run.status, WorkflowRunStatus::Interrupted);
    }

    #[test]
    fn revisions_advance_with_authoritative_state() {
        let (mut t, id) = tracker_with_run();
        let started = t.get(&id).unwrap().revision;
        let phase = t.set_phase(&id, "Scan").unwrap().revision;
        assert!(phase > started);
        assert_eq!(t.set_phase(&id, "Scan").unwrap().revision, phase);

        t.agent_started(
            &id,
            WorkflowAgentRow {
                agent_id: "child-1".into(),
                label: "scanner".into(),
                phase: None,
                model: None,
                state: "running".into(),
                tokens_used: 0,
                duration_ms: 0,
            },
        );
        let spawned = t.get(&id).unwrap().revision;
        assert!(spawned > phase);
        t.agent_finished(&id, "child-1", "done", 12, 34);
        assert!(t.get(&id).unwrap().revision > spawned);
    }
}
