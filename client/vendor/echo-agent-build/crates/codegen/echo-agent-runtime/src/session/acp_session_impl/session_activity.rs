//! Activity that must survive disconnects and transport cleanup.
use super::*;

impl SessionActor {
    /// Includes work that can survive a model turn: shells, children,
    /// schedules, goals, workflows and user interactions.
    pub(super) async fn has_live_tool_work(&self) -> bool {
        if self.has_foreground_work().await {
            return true;
        }
        if self
            .tool_context
            .task_completion_reservations
            .as_ref()
            .is_some_and(|reservations| !reservations.snapshot().is_empty())
        {
            return true;
        }
        let bridge = self.tool_bridge_handle();
        if bridge
            .list_tasks()
            .await
            .unwrap_or_default()
            .iter()
            .any(|task| {
                task.is_outstanding()
                    && task
                        .owner_session_id
                        .as_deref()
                        .is_none_or(|owner| owner == self.session_info.id.0.as_ref())
            })
        {
            return true;
        }
        use echo_agent_tools::implementations::echo_agent_build::{
            scheduler::types::{SchedulerCommand, SchedulerHandle},
            task::types::SubagentEvent,
        };
        if let Some(tx) = &self.tool_context.subagent_event_tx {
            let (respond_to, response) = tokio::sync::oneshot::channel();
            if tx
                .send(SubagentEvent::HasLiveChildren {
                    parent_session_id: self.session_id_string(),
                    respond_to,
                })
                .is_err()
                || response.await.unwrap_or(true)
            {
                return true;
            }
        }
        let scheduler = {
            let toolset = bridge.toolset();
            let resources = toolset.resources.lock().await;
            resources.get::<SchedulerHandle>().cloned()
        };
        if let Some(scheduler) = scheduler {
            let (reply, response) = tokio::sync::oneshot::channel();
            if scheduler.0.send(SchedulerCommand::List { reply }).is_err() {
                return true;
            }
            let Ok(snapshot) = response.await else {
                return true;
            };
            if snapshot
                .tasks
                .iter()
                .any(|task| task.pending_fire_at(chrono::Utc::now()).is_some())
            {
                return true;
            }
        }
        self.has_foreground_work().await
    }

    async fn has_foreground_work(&self) -> bool {
        let turn_busy = {
            let state = self.state.lock().await;
            state_is_busy(&state) || !state.pending_notifications.is_empty()
        };
        let goal_active = self.goal_tracker.lock().status()
            == Some(crate::session::goal_tracker::GoalStatus::Active);
        let needs_input = self
            .pending_interactions
            .lock()
            .map(|p| !p.is_empty())
            .unwrap_or(true);
        if turn_busy || goal_active || needs_input {
            return true;
        }
        self.workflow_manager.lock().await.has_live_runs()
    }
}

#[cfg(test)]
#[path = "session_activity_tests.rs"]
mod tests;
