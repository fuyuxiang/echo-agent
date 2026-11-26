use super::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

fn workflow_run(name: &str, status: &str) -> crate::views::workflows::WorkflowRunSnapshot {
    crate::views::workflows::WorkflowRunSnapshot {
        run_id: format!("wf_{name}"),
        name: name.to_owned(),
        objective: "objective".to_owned(),
        status: status.to_owned(),
        management_available: true,
        builtin: false,
        phases: Vec::new(),
        current_phase: Some("Scan".to_owned()),
        agents: Vec::new(),
        agent_budget: None,
        agents_used: 0,
        agents_reserved: 0,
        agents_remaining: None,
        agent_usage_incomplete: false,
        active_agents: 0,
        elapsed_ms: 5_000,
        received_at: Instant::now(),
        pause_message: None,
        result_summary: None,
    }
}

#[test]
fn status_counts_classify_retained_non_terminal_runs_as_paused() {
    let pane = TasksPane::new();
    let empty_bg = BTreeMap::new();
    let empty_subagents = HashMap::new();
    let empty_scheduled = HashMap::new();

    for status in [
        "user_paused",
        "back_off_paused",
        "no_progress_paused",
        "infra_paused",
        "blocked",
        "budget_limited",
        "paused",
        "future_retained_status",
    ] {
        let runs = [workflow_run(status, status)];
        assert_eq!(
            pane.status_counts(&empty_bg, &empty_subagents, &empty_scheduled, &runs,),
            TaskStatusCounts {
                running: 0,
                paused_workflows: 1,
            },
            "status {status} must remain visible as paused"
        );
    }

    for status in ["complete", "failed", "cancelled", "interrupted"] {
        let runs = [workflow_run(status, status)];
        assert_eq!(
            pane.status_counts(&empty_bg, &empty_subagents, &empty_scheduled, &runs,),
            TaskStatusCounts::default(),
            "terminal status {status} must not appear in the status bar"
        );
    }

    let runs = [
        workflow_run("active", "active"),
        workflow_run("paused", "user_paused"),
        workflow_run("failed", "failed"),
    ];
    assert_eq!(
        pane.status_counts(&empty_bg, &empty_subagents, &empty_scheduled, &runs,),
        TaskStatusCounts {
            running: 1,
            paused_workflows: 1,
        }
    );
}

#[test]
fn mixed_workflows_coalesce_children_and_keep_standalone_work_running() {
    let pane = TasksPane::new();
    let mut workflow_child_a =
        crate::app::agent_view::test_fixtures::running_subagent_info("workflow-child-a");
    workflow_child_a.workflow_run_id = Some("wf_active".into());
    let mut workflow_child_b =
        crate::app::agent_view::test_fixtures::running_subagent_info("workflow-child-b");
    workflow_child_b.workflow_run_id = Some("wf_paused".into());
    let standalone = crate::app::agent_view::test_fixtures::running_subagent_info("standalone");
    let subagents = HashMap::from([
        ("workflow-child-a".to_owned(), workflow_child_a),
        ("workflow-child-b".to_owned(), workflow_child_b),
        ("standalone".to_owned(), standalone),
    ]);
    let runs = [
        workflow_run("active", "active"),
        workflow_run("paused", "user_paused"),
    ];

    assert_eq!(
        pane.status_counts(&BTreeMap::new(), &subagents, &HashMap::new(), &runs),
        TaskStatusCounts {
            running: 2,
            paused_workflows: 1,
        }
    );
}

#[test]
fn active_to_paused_stays_visible_without_ticks_then_terminal_closes() {
    let mut pane = TasksPane::new();
    let active = [workflow_run("gate", "active")];
    pane.sync(
        &BTreeMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &HashSet::new(),
        &active,
    );
    assert!(pane.is_visible());
    assert!(pane.needs_tick());
    pane.overlay.focused = false;

    let paused = [workflow_run("gate", "user_paused")];
    pane.sync(
        &BTreeMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &HashSet::new(),
        &paused,
    );
    assert!(pane.is_visible());
    assert!(!pane.needs_tick());
    pane.sync(
        &BTreeMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &HashSet::new(),
        &paused,
    );
    assert!(pane.is_visible(), "paused-only syncs must not auto-close");

    let terminal = [workflow_run("gate", "complete")];
    pane.sync(
        &BTreeMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        &HashSet::new(),
        &terminal,
    );
    assert!(!pane.is_visible());
}
