use super::*;

#[allow(clippy::too_many_arguments)]
fn upsert_workflow_block(
    agent: &mut AgentView,
    run_id: &str,
    name: &str,
    objective: &str,
    status: &str,
    phases: &[echo_agent_runtime::extensions::notification::WorkflowPhaseInfo],
    current_phase: Option<&str>,
    active_agents: u32,
    elapsed_ms: u64,
) {
    use crate::scrollback::blocks::{WorkflowBlock, WorkflowBlockPhase, WorkflowBlockStatus};

    let elapsed = std::time::Duration::from_millis(elapsed_ms);
    let block_status = match status {
        "active" => WorkflowBlockStatus::Running,
        "complete" => WorkflowBlockStatus::Done { elapsed },
        "failed" | "interrupted" => WorkflowBlockStatus::Failed { elapsed },
        "cancelled" => WorkflowBlockStatus::Cancelled { elapsed },
        "cleared" => {
            if let Some(id) = agent.workflow_blocks.remove(run_id) {
                agent.scrollback.finish_running(id);
            }
            return;
        }
        _ => WorkflowBlockStatus::Paused { elapsed },
    };
    let is_running = matches!(block_status, WorkflowBlockStatus::Running);
    let terminal = matches!(
        block_status,
        WorkflowBlockStatus::Done { .. }
            | WorkflowBlockStatus::Failed { .. }
            | WorkflowBlockStatus::Cancelled { .. }
    );

    let mapped_entry = agent
        .workflow_blocks
        .get(run_id)
        .copied()
        .filter(|id| agent.scrollback.get_by_id(*id).is_some());
    if mapped_entry.is_none() {
        agent.workflow_blocks.remove(run_id);
    }
    let entry_id = match mapped_entry {
        Some(id) => id,
        None => {
            let block = WorkflowBlock::started(run_id, name, objective);
            let id = agent.scrollback.push_block(RenderBlock::Workflow(block));
            agent.scrollback.set_last_running(true);
            agent.workflow_blocks.insert(run_id.to_string(), id);
            id
        }
    };

    if let Some(entry) = agent.scrollback.get_by_id_mut(entry_id)
        && let RenderBlock::Workflow(ref mut wb) = entry.block
    {
        wb.status = block_status;
        wb.phases = phases
            .iter()
            .map(|p| WorkflowBlockPhase {
                title: p.title.clone(),
                state: p.state.clone(),
            })
            .collect();
        wb.current_phase = current_phase.map(str::to_owned);
        wb.active_agents = active_agents;
        wb.elapsed = elapsed;
        entry.invalidate_cache();
    }
    if is_running {
        agent.scrollback.set_entry_running(entry_id, true);
    } else {
        agent.scrollback.finish_running(entry_id);
        if terminal {
            agent.workflow_blocks.remove(run_id);
        }
    }
}

pub(super) fn ingest_workflow_update(
    agent: &mut AgentView,
    update: EchoAgentSessionUpdate,
) -> bool {
    let EchoAgentSessionUpdate::WorkflowUpdated {
        run_id,
        revision,
        name,
        objective,
        status,
        foreground: _,
        phases,
        current_phase,
        agent_budget,
        agents_used,
        agents_reserved,
        agents_remaining,
        agent_usage_incomplete,
        elapsed_ms,
        active_agents: _,
        current_agent_label: _,
        agents,
        last_event: _,
        last_event_detail: _,
        last_event_timestamp: _,
        pause_message,
        result_summary,
        ..
    } = update
    else {
        return false;
    };
    if status != "cleared" {
        match agent.workflow_run_revisions.get(&run_id).copied() {
            Some(last) if revision == 0 && last > 0 => return false,
            Some(last) if revision > 0 && revision <= last => return false,
            _ => {}
        }
        if revision == 0 && agent.cleared_workflow_runs.contains(&run_id) {
            return false;
        }
    }
    if revision > 0 {
        agent
            .workflow_run_revisions
            .insert(run_id.clone(), revision);
    }
    if status == "cleared" {
        agent.cleared_workflow_runs.insert(run_id.clone());
    }
    let management_available = agent
        .session
        .available_commands
        .iter()
        .any(|c| c.name == "workflow");
    let builtin = super::is_builtin_workflow_handle(&agent.session.available_commands, &name);
    if status == "cleared" {
        agent.workflow_runs.retain(|run| run.run_id != run_id);
    } else {
        let snapshot = crate::views::workflows::WorkflowRunSnapshot {
            run_id: run_id.clone(),
            name: name.clone(),
            objective: objective.clone(),
            status: status.clone(),
            management_available,
            builtin,
            phases: phases
                .iter()
                .map(|p| (p.title.clone(), p.state.clone()))
                .collect(),
            current_phase: current_phase.clone(),
            agents: agents
                .iter()
                .map(|a| crate::views::workflows::WorkflowAgentRowView {
                    agent_id: a.agent_id.clone(),
                    label: a.label.clone(),
                    phase: a.phase.clone(),
                    model: a.model.clone(),
                    state: a.state.clone(),
                    tokens_used: a.tokens_used,
                    duration_ms: a.duration_ms,
                })
                .collect(),
            agent_budget,
            agents_used,
            agents_reserved,
            agents_remaining,
            agent_usage_incomplete,
            active_agents: agents.iter().filter(|a| a.state == "running").count() as u32,
            elapsed_ms,
            received_at: std::time::Instant::now(),
            pause_message: pause_message.clone(),
            result_summary: result_summary.clone(),
        };
        match agent
            .workflow_runs
            .iter_mut()
            .find(|run| run.run_id == run_id)
        {
            Some(existing) => *existing = snapshot,
            None => agent.workflow_runs.push(snapshot),
        }
    }
    let active = agents.iter().filter(|a| a.state == "running").count() as u32;
    upsert_workflow_block(
        agent,
        &run_id,
        &name,
        &objective,
        &status,
        &phases,
        current_phase.as_deref(),
        active,
        elapsed_ms,
    );
    true
}
