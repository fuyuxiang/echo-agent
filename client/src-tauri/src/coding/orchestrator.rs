//! Phase orchestration for a coding task.
//!
//! The workbench — not the model — decides when a task moves forward. A phase
//! transition is derived from real evidence: process exit codes, the change set
//! on disk and diagnostic fingerprints. This is what stops a model that claims
//! "all tests pass" from marking work complete, and what stops a repair loop
//! from burning tokens on the same failure forever.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::coding::changeset;
use crate::coding::diagnostics::{self, Problem};
use crate::coding::documentation;
use crate::coding::store;
use crate::coding::task::{
    self, AcceptanceCriterion, CodingTask, ExecutionLedgerEvent, PlanIssueSeverity,
    RuntimePlanEntry, TaskNextAction, TaskNode, TaskNodeStatus, TaskPhase,
};
use crate::coding::tdd;
use crate::coding::verification::{self, VerificationRecord, VerificationStatus};
use crate::shell_fs::FilesystemAccess;

pub const DEFAULT_MAX_REPAIR_ROUNDS: u32 = 3;
pub const DEFAULT_MAX_NODE_ATTEMPTS: u32 = 3;
pub const DEFAULT_MAX_PLAN_REVISIONS: usize = 3;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RepairOutcome {
    Fixed,
    SameErrors,
    NewErrors,
    RoundsExhausted,
}

/// A user-controlled end to an Agent turn is recoverable and must never be
/// presented as an implementation failure.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TaskInterruption {
    Paused,
    Stopped,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RepairRound {
    pub round: u32,
    pub problem_fingerprints: Vec<String>,
    pub started_at: String,
    pub outcome: Option<RepairOutcome>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PhaseDecision {
    pub next_phase: TaskPhase,
    pub reason: String,
    /// Human-readable blocker, set only when `next_phase` is `Blocked`.
    pub blocker: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorState {
    pub task: CodingTask,
    pub problems: Vec<Problem>,
    pub repair_rounds: Vec<RepairRound>,
    pub changed_file_count: usize,
    pub max_repair_rounds: u32,
}

fn repairs_path(root: &Path, task_id: &str) -> PathBuf {
    store::task_dir(root, task_id).join("repairs.jsonl")
}

pub fn repair_rounds(root: &Path, task_id: &str) -> Vec<RepairRound> {
    store::read_jsonl(&repairs_path(root, task_id))
}

fn append_round(root: &Path, task_id: &str, round: &RepairRound) -> Result<(), String> {
    store::append_jsonl(&repairs_path(root, task_id), round)
}

/// Keep only the most recent run of each command, so a failure that a later run
/// fixed does not hold the task in a red state.
fn latest_per_command(records: &[VerificationRecord]) -> Vec<&VerificationRecord> {
    let mut latest: Vec<&VerificationRecord> = Vec::new();
    for record in records {
        match latest
            .iter_mut()
            .find(|existing| existing.command == record.command)
        {
            Some(existing) => *existing = record,
            None => latest.push(record),
        }
    }
    latest
}

/// Bind acceptance rows only to evidence traceable to the current revision.
/// Plan rows require every command declared by their own node; high-level user
/// requirements require an explicit, revision-bound confirmation.
fn bind_acceptance_evidence(
    task: &mut CodingTask,
    change_set: &changeset::ChangeSet,
    verification_records: &[VerificationRecord],
) {
    let passed_records = latest_per_command(verification_records)
        .into_iter()
        .filter(|record| record.status == VerificationStatus::Passed)
        .collect::<Vec<_>>();
    let commands_by_node = task
        .task_nodes
        .iter()
        .map(|node| (node.plan_key.clone(), node.verification_commands.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let content_revision = change_set.content_revision();
    for criterion in &mut task.acceptance_criteria {
        let node_key = criterion.id.split_once(":acceptance:").map(|(key, _)| key);
        let Some(commands) = node_key.and_then(|key| commands_by_node.get(key)) else {
            let current_confirmation = criterion.satisfied
                && criterion.evidence.iter().any(|evidence| {
                    evidence.contains("用户于") && evidence.contains(&content_revision)
                });
            if !current_confirmation {
                criterion.satisfied = false;
                criterion.evidence.clear();
            }
            continue;
        };
        let matching = passed_records
            .iter()
            .filter(|record| commands.contains(&record.command))
            .collect::<Vec<_>>();
        criterion.satisfied = !commands.is_empty() && matching.len() == commands.len();
        criterion.evidence = if criterion.satisfied {
            matching
                .iter()
                .map(|record| format!("{} 通过（退出码 0）", record.command))
                .collect()
        } else {
            Vec::new()
        };
    }
}

fn missing_detected_commands(root: &Path, records: &[VerificationRecord]) -> Vec<String> {
    let completed: HashSet<&str> = records
        .iter()
        .map(|record| record.command.as_str())
        .collect();
    verification::detect_commands(root)
        .into_iter()
        .filter(|expected| !completed.contains(expected.command.as_str()))
        .map(|expected| expected.command)
        .collect()
}

fn missing_planned_commands(task: &CodingTask, records: &[VerificationRecord]) -> Vec<String> {
    let completed: HashSet<&str> = records
        .iter()
        .map(|record| record.command.as_str())
        .collect();
    let mut missing = Vec::new();
    for command in task
        .task_nodes
        .iter()
        .flat_map(|node| node.verification_commands.iter())
    {
        if !completed.contains(command.as_str()) && !missing.contains(command) {
            missing.push(command.clone());
        }
    }
    missing
}

fn count_ledger_events(root: &Path, task_id: &str, kind: &str) -> usize {
    task::execution_ledger(root, task_id)
        .iter()
        .filter(|event| event.kind == kind)
        .count()
}

/// Select exactly one ready node and persist the retry attempt on the task.
/// The renderer uses `next_action` to start the next managed Agent turn; it
/// never guesses readiness from display state.
fn schedule_next_node(task: &mut CodingTask) -> Result<String, String> {
    let statuses = task
        .task_nodes
        .iter()
        .map(|node| (node.plan_key.clone(), node.status))
        .collect::<std::collections::HashMap<_, _>>();
    let candidate =
        task.task_nodes
            .iter()
            .position(|node| node.status == TaskNodeStatus::Running)
            .or_else(|| {
                task.task_nodes.iter().position(|node| {
                    matches!(
                        node.status,
                        TaskNodeStatus::Pending | TaskNodeStatus::Failed
                    ) && node.dependencies.iter().all(|dependency| {
                        statuses.get(dependency) == Some(&TaskNodeStatus::Success)
                    })
                })
            })
            .ok_or_else(|| "没有依赖已满足的可执行节点".to_string())?;
    let node = &mut task.task_nodes[candidate];
    if node.attempt >= DEFAULT_MAX_NODE_ATTEMPTS {
        return Err(format!(
            "节点“{}”连续执行 {} 次仍未完成",
            node.plan_key, node.attempt
        ));
    }
    node.status = TaskNodeStatus::Running;
    node.attempt = node.attempt.saturating_add(1);
    node.started_at
        .get_or_insert_with(|| chrono::Utc::now().to_rfc3339());
    node.completed_at = None;
    node.failure = None;
    Ok(node.plan_key.clone())
}

/// Decide where a task goes once verification finished.
pub fn decide_after_verification(
    records: &[VerificationRecord],
    _changed_file_count: usize,
) -> PhaseDecision {
    let effective = latest_per_command(records);
    if effective
        .iter()
        .any(|record| record.status == VerificationStatus::EnvironmentUnavailable)
    {
        return PhaseDecision {
            next_phase: TaskPhase::Blocked,
            reason: "验证环境未就绪，请安装项目声明的工具或修正验证命令后重试".into(),
            blocker: Some("验证环境未就绪".into()),
        };
    }
    let failing: Vec<&&VerificationRecord> = effective
        .iter()
        .filter(|record| record.status != VerificationStatus::Passed)
        .collect();

    if !failing.is_empty() {
        let commands = failing
            .iter()
            .map(|record| record.command.as_str())
            .collect::<Vec<_>>()
            .join("、");
        return PhaseDecision {
            next_phase: TaskPhase::Diagnosing,
            reason: format!("以下验证未通过：{commands}"),
            blocker: None,
        };
    }

    PhaseDecision {
        next_phase: TaskPhase::Delivered,
        reason: if effective.is_empty() {
            "任务已完成；当前工程未检测到可运行的自动检查".into()
        } else {
            format!("任务已完成，{} 项自动检查通过", effective.len())
        },
        blocker: None,
    }
}

/// Decide whether another repair round is worth attempting.
pub fn next_after_repair(
    problems: &[Problem],
    previous_fingerprints: &[String],
    completed_rounds: u32,
    max_rounds: u32,
) -> PhaseDecision {
    if problems.is_empty() {
        return PhaseDecision {
            next_phase: TaskPhase::Verifying,
            reason: "修复后没有残留问题，重新执行验证".into(),
            blocker: None,
        };
    }

    let current: HashSet<&str> = problems
        .iter()
        .map(|problem| problem.fingerprint.as_str())
        .collect();
    let previous: HashSet<&str> = previous_fingerprints
        .iter()
        .map(|value| value.as_str())
        .collect();

    if !previous.is_empty() {
        let introduced: Vec<&&str> = current.difference(&previous).collect();
        if !introduced.is_empty() {
            return PhaseDecision {
                next_phase: TaskPhase::Blocked,
                reason: "修复引入了新的错误".into(),
                blocker: Some(format!(
                    "上一轮修复引入了 {} 个新的错误，已停止自动修复以避免问题扩散。请人工确认修改方向。",
                    introduced.len()
                )),
            };
        }
        if current == previous {
            return PhaseDecision {
                next_phase: TaskPhase::Blocked,
                reason: "修复后仍是相同错误".into(),
                blocker: Some(
                    "修复后出现完全相同错误，说明当前思路无效，已停止自动修复以避免空转。请人工介入。"
                        .into(),
                ),
            };
        }
    }

    if completed_rounds >= max_rounds {
        return PhaseDecision {
            next_phase: TaskPhase::Blocked,
            reason: "修复轮次已用尽".into(),
            blocker: Some(format!(
                "已执行 {completed_rounds} 轮自动修复仍有 {} 个问题未解决，修复轮次上限为 {max_rounds}。请人工介入。",
                problems.len()
            )),
        };
    }

    PhaseDecision {
        next_phase: TaskPhase::Repairing,
        reason: format!("问题数量下降到 {}，继续自动修复", problems.len()),
        blocker: None,
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum OrchestratorEvent {
    /// The user submitted a requirement; the task leaves Idle.
    RequirementSubmitted,
    /// A valid structured plan was emitted by the runtime.
    PlanSynchronized,
    /// Creating or starting the bound Agent session failed.
    StartFailed {
        reason: String,
    },
    /// The user asked the bound Agent to make another change after a terminal
    /// or review phase. Re-open the same task before any file can be written.
    FollowupStarted {
        requirement: String,
    },
    /// The user paused or stopped an in-flight Agent turn. The workspace is
    /// synchronized before this event, but the round is not considered done.
    ImplementationInterrupted {
        interruption: TaskInterruption,
    },
    /// Continue an interrupted round without discarding its execution graph.
    InterruptedTaskResumed,
    /// The Agent finished writing code for this round.
    ImplementationFinished,
    ReadonlyFinished,
    /// The user explicitly re-runs checks from a settled phase.
    VerificationStarted,
    /// A verification batch finished; recompute from records on disk.
    VerificationFinished,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PhaseChangedPayload {
    root: String,
    task_id: String,
    phase: TaskPhase,
    reason: String,
    blocker: Option<String>,
}

/// Apply one event and persist the resulting phase.
fn apply_unlocked(
    root: &Path,
    task_id: &str,
    event: OrchestratorEvent,
) -> Result<(CodingTask, PhaseDecision), String> {
    let mut task = task::load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
    let change_set = changeset::load(root, task_id);
    let changed_file_count = change_set.changes.len();
    task.next_action = None;
    let mut workflow_ledger: Option<(&str, Option<String>, String)> = None;
    let event_kind = match &event {
        OrchestratorEvent::RequirementSubmitted => "requirement_submitted",
        OrchestratorEvent::PlanSynchronized => "plan_progress",
        OrchestratorEvent::StartFailed { .. } => "agent_start_failed",
        OrchestratorEvent::FollowupStarted { .. } => "followup_started",
        OrchestratorEvent::ImplementationInterrupted { .. } => "implementation_interrupted",
        OrchestratorEvent::InterruptedTaskResumed => "interrupted_task_resumed",
        OrchestratorEvent::ImplementationFinished => "implementation_finished",
        OrchestratorEvent::ReadonlyFinished => "readonly_finished",
        OrchestratorEvent::VerificationStarted => "verification_started",
        OrchestratorEvent::VerificationFinished => "verification_finished",
    };

    let decision = match event {
        OrchestratorEvent::RequirementSubmitted => {
            if task.phase != TaskPhase::Idle {
                return Err("只有未开始任务可以提交需求".into());
            }
            if let Some(node) = task.task_nodes.first_mut() {
                node.status = TaskNodeStatus::Running;
                node.attempt = 1;
                node.started_at = Some(chrono::Utc::now().to_rfc3339());
            }
            PhaseDecision {
                next_phase: TaskPhase::Discovering,
                reason: "Agent 正在理解需求、分析工程并准备执行计划".into(),
                blocker: None,
            }
        }
        OrchestratorEvent::PlanSynchronized => {
            if !matches!(
                task.phase,
                TaskPhase::Discovering | TaskPhase::Implementing | TaskPhase::Repairing
            ) {
                return Err("当前任务阶段不能同步执行计划".into());
            }
            PhaseDecision {
                next_phase: TaskPhase::Implementing,
                reason: format!(
                    "已拆解 {} 个执行节点，Agent 正按依赖顺序实施",
                    task.task_nodes.len()
                ),
                blocker: None,
            }
        }
        OrchestratorEvent::StartFailed { reason } => {
            let was_verifying = task.phase == TaskPhase::Verifying;
            if !matches!(
                task.phase,
                TaskPhase::Idle
                    | TaskPhase::Discovering
                    | TaskPhase::Implementing
                    | TaskPhase::Repairing
                    | TaskPhase::Verifying
            ) {
                return Err("当前任务已离开执行阶段，已忽略过期的失败事件".into());
            }
            for node in &mut task.task_nodes {
                if node.status == TaskNodeStatus::Running {
                    node.status = TaskNodeStatus::Failed;
                    node.completed_at = Some(chrono::Utc::now().to_rfc3339());
                    node.failure = Some(reason.clone());
                }
            }
            PhaseDecision {
                next_phase: TaskPhase::Blocked,
                reason: if was_verifying {
                    "验证执行中断".into()
                } else {
                    "Agent 会话执行失败".into()
                },
                blocker: Some(reason),
            }
        }
        OrchestratorEvent::FollowupStarted { requirement } => {
            if !matches!(
                task.phase,
                TaskPhase::Paused | TaskPhase::Stopped | TaskPhase::Delivered | TaskPhase::Blocked
            ) {
                return Err("只有已暂停、已停止、已完成或已阻塞的任务可以继续开发".into());
            }
            let requirement = requirement.trim();
            if requirement.is_empty() {
                return Err("补充要求不能为空".into());
            }
            tdd::clear(root, task_id)?;
            for criterion in &mut task.acceptance_criteria {
                criterion.satisfied = false;
                criterion.evidence.clear();
            }
            task.acceptance_criteria.push(AcceptanceCriterion {
                id: uuid::Uuid::now_v7().to_string(),
                content: requirement.to_string(),
                satisfied: false,
                evidence: Vec::new(),
            });
            task.plan_revision = None;
            task.plan_updated_at = None;
            task.plan_issues.clear();
            task.task_nodes = vec![TaskNode {
                id: uuid::Uuid::now_v7().to_string(),
                plan_key: "ROOT".into(),
                content: requirement.to_string(),
                dependencies: Vec::new(),
                related_files: Vec::new(),
                read_set: Vec::new(),
                write_set: Vec::new(),
                consumes: Vec::new(),
                produces: Vec::new(),
                acceptance_criteria: vec![requirement.to_string()],
                verification_commands: Vec::new(),
                status: TaskNodeStatus::Running,
                priority: "high".into(),
                attempt: 1,
                started_at: Some(chrono::Utc::now().to_rfc3339()),
                completed_at: None,
                failure: None,
            }];
            PhaseDecision {
                next_phase: TaskPhase::Discovering,
                reason: "已接收补充要求，Agent 正在重新分析影响并执行".into(),
                blocker: None,
            }
        }
        OrchestratorEvent::ImplementationInterrupted { interruption } => {
            // Releases created before interruption-aware lifecycle reporting
            // could incorrectly persist a user stop as this exact blocker.
            // Repair only that known state; never mask a real blocker.
            let legacy_stop_misclassification = task.phase == TaskPhase::Blocked
                && task.phase_reason.as_deref() == Some("实现阶段结束但没有代码变更")
                && task.blocker.as_deref()
                    == Some(
                        "Agent 结束了实现但没有写入任何文件。请检查是否只在会话里返回了示例代码。",
                    );
            if !matches!(
                task.phase,
                TaskPhase::Discovering
                    | TaskPhase::Implementing
                    | TaskPhase::Repairing
                    | TaskPhase::Verifying
                    | TaskPhase::Diagnosing
            ) && !legacy_stop_misclassification
            {
                return Err("只有正在执行的任务可以报告暂停或停止".into());
            }
            for node in &mut task.task_nodes {
                if node.status == TaskNodeStatus::Running {
                    node.status = TaskNodeStatus::Pending;
                    node.completed_at = None;
                    node.failure = None;
                }
            }
            match interruption {
                TaskInterruption::Paused => PhaseDecision {
                    next_phase: TaskPhase::Paused,
                    reason: "任务已暂停，已保留会话上下文、执行计划和当前文件变更".into(),
                    blocker: None,
                },
                TaskInterruption::Stopped => PhaseDecision {
                    next_phase: TaskPhase::Stopped,
                    reason: "任务已停止，已保留执行记录和当前工作区状态".into(),
                    blocker: None,
                },
            }
        }
        OrchestratorEvent::InterruptedTaskResumed => {
            if !matches!(task.phase, TaskPhase::Paused | TaskPhase::Stopped) {
                return Err("只有已暂停或已停止的任务可以原地继续".into());
            }
            if !tdd::can_resume(root, &task) {
                return Err("请先运行并记录失败的红灯测试，或说明不适用原因".into());
            }
            if !task
                .task_nodes
                .iter()
                .any(|node| node.status == TaskNodeStatus::Running)
            {
                if let Some(node) = task.task_nodes.iter_mut().find(|node| {
                    node.status == TaskNodeStatus::Pending
                        && node.started_at.is_some()
                        && node.completed_at.is_none()
                }) {
                    node.status = TaskNodeStatus::Running;
                    node.failure = None;
                } else {
                    schedule_next_node(&mut task)?;
                }
            }
            PhaseDecision {
                next_phase: if task.plan_revision.is_some() {
                    TaskPhase::Implementing
                } else {
                    TaskPhase::Discovering
                },
                reason: "已恢复任务，Agent 将从中断位置继续执行".into(),
                blocker: None,
            }
        }
        OrchestratorEvent::ReadonlyFinished => {
            if !matches!(
                task.phase,
                TaskPhase::Discovering | TaskPhase::Implementing | TaskPhase::Repairing
            ) {
                return Err("当前阶段不能结束只读轮次".into());
            }
            PhaseDecision {
                next_phase: TaskPhase::Delivered,
                reason: "本轮只读分析或计划已完成；已有修改保留，未自动运行工程命令".into(),
                blocker: None,
            }
        }
        OrchestratorEvent::ImplementationFinished => {
            if !matches!(
                task.phase,
                TaskPhase::Discovering | TaskPhase::Implementing | TaskPhase::Repairing
            ) {
                return Err("只有实现或修复阶段可以报告代码写入完成".into());
            }
            let documentation_violations =
                documentation::validate_documentation_changes(root, &task, &change_set);
            if !documentation_violations.is_empty() {
                let read_only_violation = documentation::is_read_only_task(&task);
                let executable_change = documentation_violations
                    .iter()
                    .any(|violation| violation.message.contains("可执行"));
                let problems = documentation_violations
                    .iter()
                    .map(|violation| {
                        diagnostics::documentation_problem(
                            violation.file.clone(),
                            violation.message.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                diagnostics::save_snapshot(root, task_id, &problems)?;
                let attempts =
                    count_ledger_events(root, task_id, "documentation_validation_requested");
                workflow_ledger = Some((
                    "documentation_validation_requested",
                    None,
                    format!(
                        "文档安全校验发现 {} 个越界文件变更",
                        documentation_violations.len()
                    ),
                ));
                if attempts < 2 {
                    PhaseDecision {
                        next_phase: TaskPhase::Repairing,
                        reason: if read_only_violation {
                            "只读解释产生了文件变更，Agent 将自动恢复原始文件".into()
                        } else if executable_change {
                            "注释生成包含可执行逻辑变更，Agent 将自动撤销越界修改".into()
                        } else {
                            "文档产物未满足原始需求，Agent 将自动补齐并重新校验".into()
                        },
                        blocker: None,
                    }
                } else {
                    PhaseDecision {
                        next_phase: TaskPhase::Blocked,
                        reason: "文档安全校验多次未通过".into(),
                        blocker: Some(if read_only_violation {
                            "Agent 多次在只读解释任务中修改文件，已停止自动修复并保留现场。请在变更集中核对并回滚相关文件。"
                                .into()
                        } else if executable_change {
                            "Agent 多次改变了可执行逻辑，已停止自动修复并保留现场。请在变更集中核对相关文件。"
                                .into()
                        } else {
                            "Agent 多次未能生成符合原始需求且可安全校验的注释或文档，已停止自动重试。请检查目标范围和文件类型。"
                                .into()
                        }),
                    }
                }
            } else if documentation::requires_comment_only_validation(&task)
                && changed_file_count == 0
            {
                diagnostics::save_snapshot(
                    root,
                    task_id,
                    &[diagnostics::documentation_problem(
                        None,
                        "Agent 未产生任何文件变更；注释/文档任务尚未真正写入工程".into(),
                    )],
                )?;
                let attempts =
                    count_ledger_events(root, task_id, "documentation_validation_requested");
                workflow_ledger = Some((
                    "documentation_validation_requested",
                    None,
                    "注释任务未产生文件变更，请求 Agent 自动补写".into(),
                ));
                if attempts < 2 {
                    PhaseDecision {
                        next_phase: TaskPhase::Repairing,
                        reason: "注释/文档尚未实际写入，Agent 将根据原始目标自动补全".into(),
                        blocker: None,
                    }
                } else {
                    PhaseDecision {
                        next_phase: TaskPhase::Blocked,
                        reason: "注释任务多次未产生实际变更".into(),
                        blocker: Some(
                            "Agent 多次未能把注释或文档写入工程，已停止自动重试。请检查目标文件是否可写或需求范围是否明确。"
                                .into(),
                        ),
                    }
                }
            } else if documentation::is_read_only_task(&task) && changed_file_count == 0 {
                diagnostics::save_snapshot(root, task_id, &[])?;
                for node in &mut task.task_nodes {
                    node.status = TaskNodeStatus::Success;
                    node.completed_at = Some(chrono::Utc::now().to_rfc3339());
                }
                PhaseDecision {
                    next_phase: TaskPhase::Delivered,
                    reason: "代码解释已完成，本次只读分析未修改工程文件".into(),
                    blocker: None,
                }
            } else if !task.strict_workflow {
                diagnostics::save_snapshot(root, task_id, &[])?;
                for node in &mut task.task_nodes {
                    node.status = TaskNodeStatus::Success;
                    node.completed_at = Some(chrono::Utc::now().to_rfc3339());
                }
                PhaseDecision {
                    next_phase: if changed_file_count > 0
                        && verification::project_config(root)
                            .checks
                            .iter()
                            .any(|check| check.auto_run)
                    {
                        TaskPhase::Verifying
                    } else {
                        TaskPhase::Delivered
                    },
                    reason: if changed_file_count == 0 {
                        "本轮已完成，未产生文件修改".into()
                    } else {
                        format!(
                            "本轮已完成，保留 {changed_file_count} 个文件变更；检查结果单独记录"
                        )
                    },
                    blocker: None,
                }
            } else if tdd::should_pause_for_red(root, &task, &change_set) {
                if !task
                    .task_nodes
                    .iter()
                    .any(|node| node.status == TaskNodeStatus::Running)
                {
                    if let Some(node) = task
                        .task_nodes
                        .iter_mut()
                        .rev()
                        .find(|node| node.status == TaskNodeStatus::Success)
                    {
                        node.status = TaskNodeStatus::Running;
                        node.completed_at = None;
                    }
                }
                workflow_ledger = Some((
                    "tdd_red_requested",
                    task.task_nodes
                        .iter()
                        .find(|node| node.status == TaskNodeStatus::Running)
                        .map(|node| node.plan_key.clone()),
                    "测试文件已写入，等待真实失败的测试结果".into(),
                ));
                PhaseDecision {
                    next_phase: TaskPhase::Paused,
                    reason: "等待红灯测试验证".into(),
                    blocker: None,
                }
            } else {
                let plan_errors = task
                    .plan_issues
                    .iter()
                    .filter(|issue| issue.severity == PlanIssueSeverity::Error)
                    .map(|issue| issue.message.clone())
                    .collect::<Vec<_>>();
                let unfinished = task
                    .task_nodes
                    .iter()
                    .filter(|node| node.status != TaskNodeStatus::Success)
                    .map(|node| node.plan_key.clone())
                    .collect::<Vec<_>>();
                if !plan_errors.is_empty() {
                    let attempts = count_ledger_events(root, task_id, "plan_correction_requested");
                    if attempts < DEFAULT_MAX_PLAN_REVISIONS {
                        task.next_action = Some(TaskNextAction::RevisePlan);
                        workflow_ledger = Some((
                            "plan_correction_requested",
                            None,
                            format!("请求自动修订执行计划：{}", plan_errors.join("；")),
                        ));
                        PhaseDecision {
                            next_phase: TaskPhase::Implementing,
                            reason: format!(
                                "执行计划需要自动修订（{}/{DEFAULT_MAX_PLAN_REVISIONS}）",
                                attempts + 1
                            ),
                            blocker: None,
                        }
                    } else {
                        PhaseDecision {
                            next_phase: TaskPhase::Blocked,
                            reason: "执行计划多次校验失败".into(),
                            blocker: Some(format!(
                                "执行计划连续 {attempts} 次存在结构错误：{}",
                                plan_errors.join("；")
                            )),
                        }
                    }
                } else if task.plan_revision.is_some() {
                    let unplanned = change_set
                        .changes
                        .iter()
                        .filter(|change| !task::plan_covers_path(&task, &change.path))
                        .map(|change| change.path.clone())
                        .collect::<Vec<_>>();
                    if !unplanned.is_empty() {
                        let attempts =
                            count_ledger_events(root, task_id, "plan_correction_requested");
                        if attempts < DEFAULT_MAX_PLAN_REVISIONS {
                            task.plan_issues.push(crate::coding::task::PlanIssue {
                                severity: PlanIssueSeverity::Error,
                                code: "unplanned_write".into(),
                                message: format!(
                                    "实际变更超出计划写入范围：{}",
                                    unplanned.join("、")
                                ),
                                node_keys: Vec::new(),
                            });
                            task.next_action = Some(TaskNextAction::RevisePlan);
                            workflow_ledger = Some((
                                "plan_correction_requested",
                                None,
                                format!("需要把实际变更纳入计划：{}", unplanned.join("、")),
                            ));
                            PhaseDecision {
                                next_phase: TaskPhase::Implementing,
                                reason: "检测到计划外变更，正在自动修订执行范围".into(),
                                blocker: None,
                            }
                        } else {
                            PhaseDecision {
                                next_phase: TaskPhase::Blocked,
                                reason: "计划写入范围多次与实际变更不一致".into(),
                                blocker: Some(format!(
                                    "无法在 {DEFAULT_MAX_PLAN_REVISIONS} 次修订内覆盖实际变更：{}",
                                    unplanned.join("、")
                                )),
                            }
                        }
                    } else if !unfinished.is_empty() {
                        match schedule_next_node(&mut task) {
                            Ok(node_key) => {
                                task.next_action = Some(TaskNextAction::ContinueNode);
                                workflow_ledger = Some((
                                    "node_scheduled",
                                    Some(node_key.clone()),
                                    format!("已调度节点“{node_key}”进入下一执行回合"),
                                ));
                                PhaseDecision {
                                    next_phase: TaskPhase::Implementing,
                                    reason: format!(
                                        "继续执行节点 {node_key}，剩余 {} 个节点",
                                        unfinished.len()
                                    ),
                                    blocker: None,
                                }
                            }
                            Err(reason) => PhaseDecision {
                                next_phase: TaskPhase::Blocked,
                                reason: "执行图无法继续调度".into(),
                                blocker: Some(format!(
                                    "{reason}。已保留现场，请检查节点拆解或实现方向。"
                                )),
                            },
                        }
                    } else if changed_file_count == 0 {
                        PhaseDecision {
                            next_phase: TaskPhase::Blocked,
                            reason: "实现阶段结束但没有代码变更".into(),
                            blocker: Some(
                                "Agent 已结束本轮执行，但当前工作区与任务开始时一致。可能未写入代码，或产生的变更已被撤销或移除；请查看执行记录后补充要求。"
                                    .into(),
                            ),
                        }
                    } else {
                        diagnostics::save_snapshot(root, task_id, &[])?;
                        PhaseDecision {
                            next_phase: TaskPhase::Verifying,
                            reason: format!("已产生 {changed_file_count} 个文件变更，开始验证"),
                            blocker: None,
                        }
                    }
                } else if changed_file_count > 2 {
                    PhaseDecision {
                        next_phase: TaskPhase::Blocked,
                        reason: "复杂变更缺少结构化执行计划".into(),
                        blocker: Some(format!(
                            "本轮修改了 {changed_file_count} 个文件，但 Agent 未发布可追踪的执行计划。已保留文件变更，请补充要求后让 Agent 先拆解依赖再继续。"
                        )),
                    }
                } else if changed_file_count == 0 {
                    PhaseDecision {
                        next_phase: TaskPhase::Blocked,
                        reason: "实现阶段结束但没有代码变更".into(),
                        blocker: Some(
                            "Agent 已结束本轮执行，但当前工作区与任务开始时一致。可能未写入代码，或产生的变更已被撤销或移除；请查看执行记录后补充要求。"
                                .into(),
                        ),
                    }
                } else {
                    // Test/lint/build passes are content-bound evidence. Never let
                    // a new implementation round inherit an earlier green batch.
                    diagnostics::save_snapshot(root, task_id, &[])?;
                    if task.plan_revision.is_none() {
                        for node in &mut task.task_nodes {
                            node.status = TaskNodeStatus::Success;
                            node.completed_at = Some(chrono::Utc::now().to_rfc3339());
                        }
                    }
                    PhaseDecision {
                        next_phase: TaskPhase::Verifying,
                        reason: format!("已产生 {changed_file_count} 个文件变更，开始验证"),
                        blocker: None,
                    }
                }
            }
        }
        OrchestratorEvent::VerificationStarted => {
            if !matches!(
                task.phase,
                TaskPhase::Verifying
                    | TaskPhase::Paused
                    | TaskPhase::Stopped
                    | TaskPhase::Delivered
                    | TaskPhase::Blocked
            ) {
                return Err("只有验证中、已暂停、已停止、已完成或已阻塞的任务可以启动验证".into());
            }
            // Validate and bind the batch before clearing prior evidence. If
            // there are no current changes, starting verification must not
            // destroy the last useful report as a side effect.
            changeset::mark_verification_started(root, task_id)?;
            diagnostics::save_snapshot(root, task_id, &[])?;
            for criterion in &mut task.acceptance_criteria {
                criterion.satisfied = false;
                criterion.evidence.clear();
            }
            PhaseDecision {
                next_phase: TaskPhase::Verifying,
                reason: "已开始重新验证当前任务内容".into(),
                blocker: None,
            }
        }
        OrchestratorEvent::VerificationFinished => {
            if task.phase != TaskPhase::Verifying {
                return Err("只有验证阶段可以报告验证完成".into());
            }
            let records = verification::current_records(root, task_id, &change_set);
            let has_current_failure = records
                .iter()
                .any(|record| record.status != VerificationStatus::Passed);
            if task.strict_workflow && !has_current_failure {
                let mut missing = missing_detected_commands(root, &records);
                missing.extend(missing_planned_commands(&task, &records));
                missing.sort();
                missing.dedup();
                if !missing.is_empty() {
                    return Err(format!("尚有验证命令未运行：{}", missing.join("、")));
                }
            }
            let problems: Vec<Problem> = latest_per_command(&records)
                .iter()
                .flat_map(|record| diagnostics::parse_record(record))
                .collect();
            diagnostics::save_snapshot(root, task_id, &problems)?;

            let rounds = repair_rounds(root, task_id);
            let completed = rounds.len() as u32;
            let previous: Vec<String> = rounds
                .last()
                .map(|round| round.problem_fingerprints.clone())
                .unwrap_or_default();

            let verdict = decide_after_verification(&records, changed_file_count);
            if verdict.next_phase == TaskPhase::Delivered && !records.is_empty() {
                // Bind all green verification evidence to the exact file
                // revision that produced it. A later external edit clears this
                // marker during native change-set synchronization.
                changeset::mark_verified_revision(root, task_id)?;
            }
            if !task.strict_workflow {
                PhaseDecision {
                    next_phase: TaskPhase::Delivered,
                    reason: if verdict.next_phase == TaskPhase::Delivered {
                        verdict.reason
                    } else {
                        format!("本轮检查已结束：{}；可继续修改或重跑检查", verdict.reason)
                    },
                    blocker: None,
                }
            } else if verdict.next_phase != TaskPhase::Diagnosing {
                verdict
            } else {
                // Failing verification: decide whether to repair again, and open
                // a round so the next comparison has a baseline.
                let decision =
                    next_after_repair(&problems, &previous, completed, DEFAULT_MAX_REPAIR_ROUNDS);
                if decision.next_phase == TaskPhase::Repairing {
                    for node in &mut task.task_nodes {
                        node.status = TaskNodeStatus::Failed;
                    }
                    append_round(
                        root,
                        task_id,
                        &RepairRound {
                            round: completed + 1,
                            problem_fingerprints: problems
                                .iter()
                                .map(|problem| problem.fingerprint.clone())
                                .collect(),
                            started_at: chrono::Utc::now().to_rfc3339(),
                            outcome: None,
                        },
                    )?;
                }
                decision
            }
        }
    };

    if decision.next_phase == TaskPhase::Delivered {
        for node in &mut task.task_nodes {
            node.status = TaskNodeStatus::Success;
            node.completed_at
                .get_or_insert_with(|| chrono::Utc::now().to_rfc3339());
        }
        let verification_records = verification::current_records(root, task_id, &change_set);
        bind_acceptance_evidence(&mut task, &change_set, &verification_records);
    } else if decision.next_phase == TaskPhase::Blocked {
        // A terminal task must never retain a node that claims to be running.
        for node in &mut task.task_nodes {
            if node.status == TaskNodeStatus::Running {
                node.status = TaskNodeStatus::Blocked;
                node.completed_at = Some(chrono::Utc::now().to_rfc3339());
                node.failure = decision.blocker.clone();
            }
        }
    }
    task.phase = decision.next_phase;
    task.phase_reason = Some(decision.reason.clone());
    task.blocker = decision.blocker.clone();
    task::save(root, &task)?;
    // Return the persisted copy so its updatedAt exactly matches subsequent
    // reads. The frontend uses that revision to de-duplicate automatic
    // verification; returning the pre-save timestamp can start the same batch
    // twice after refresh.
    let persisted = task::load(root, task_id).ok_or_else(|| "任务保存后无法读取".to_string())?;
    let _ = task::append_ledger(
        root,
        task_id,
        event_kind,
        None,
        decision.reason.clone(),
        persisted.plan_revision.clone(),
    );
    if let Some((kind, node_key, message)) = workflow_ledger {
        let _ = task::append_ledger(
            root,
            task_id,
            kind,
            node_key,
            message,
            persisted.plan_revision.clone(),
        );
    }
    Ok((persisted, decision))
}

pub fn apply(
    root: &Path,
    task_id: &str,
    event: OrchestratorEvent,
) -> Result<(CodingTask, PhaseDecision), String> {
    apply_at_revision(root, task_id, event, None)
}

fn apply_at_revision(
    root: &Path,
    task_id: &str,
    event: OrchestratorEvent,
    expected: Option<&str>,
) -> Result<(CodingTask, PhaseDecision), String> {
    store::with_task_transaction(root, task_id, || {
        if let Some(expected) = expected {
            let current = task::load(root, task_id).ok_or("任务不存在")?;
            if current.updated_at != expected {
                return Err("任务状态已变化，请确认当前状态后重试".into());
            }
        }
        apply_unlocked(root, task_id, event)
    })
}

async fn apply_and_emit(
    app: AppHandle,
    root: PathBuf,
    task_id: String,
    event: OrchestratorEvent,
) -> Result<CodingTask, String> {
    apply_and_emit_at_revision(app, root, task_id, event, None).await
}

async fn apply_and_emit_at_revision(
    app: AppHandle,
    root: PathBuf,
    task_id: String,
    event: OrchestratorEvent,
    expected: Option<String>,
) -> Result<CodingTask, String> {
    let (task, decision) = tokio::task::spawn_blocking({
        let root = root.clone();
        let task_id = task_id.clone();
        move || match expected {
            Some(expected) => apply_at_revision(&root, &task_id, event, Some(&expected)),
            None => apply(&root, &task_id, event),
        }
    })
    .await
    .map_err(|error| format!("推进任务阶段失败：{error}"))??;
    let _ = app.emit(
        "coding://task-phase-changed",
        PhaseChangedPayload {
            root: root.to_string_lossy().into_owned(),
            task_id,
            phase: task.phase,
            reason: decision.reason,
            blocker: decision.blocker,
        },
    );
    Ok(task)
}

#[tauri::command]
pub async fn coding_task_submit_requirement(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    apply_and_emit(app, root, task_id, OrchestratorEvent::RequirementSubmitted).await
}

#[tauri::command]
pub async fn coding_task_sync_plan(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    entries: Vec<RuntimePlanEntry>,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    let synchronized = tokio::task::spawn_blocking({
        let root = root.clone();
        let task_id = task_id.clone();
        move || task::sync_runtime_plan(&root, &task_id, entries)
    })
    .await
    .map_err(|error| format!("同步执行计划失败：{error}"))??;
    if synchronized
        .plan_issues
        .iter()
        .any(|issue| issue.severity == PlanIssueSeverity::Error)
    {
        return Ok(synchronized);
    }
    apply_and_emit(app, root, task_id, OrchestratorEvent::PlanSynchronized).await
}

#[tauri::command]
pub async fn coding_task_execution_ledger(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<Vec<ExecutionLedgerEvent>, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || task::execution_ledger(&root, &task_id))
        .await
        .map_err(|error| format!("读取执行账本失败：{error}"))
}

#[tauri::command]
pub async fn coding_task_report_start_failed(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    reason: String,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    apply_and_emit(
        app,
        root,
        task_id,
        OrchestratorEvent::StartFailed { reason },
    )
    .await
}

#[tauri::command]
pub async fn coding_task_begin_followup(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    requirement: String,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    apply_and_emit(
        app,
        root,
        task_id,
        OrchestratorEvent::FollowupStarted { requirement },
    )
    .await
}

#[tauri::command]
pub async fn coding_task_report_interrupted(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    interruption: TaskInterruption,
    expected_updated_at: Option<String>,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    // Preserve partial work before changing the phase. A stopped task remains
    // reviewable and can be resumed or rolled back without guessing from chat.
    changeset::sync_changes(&root, &task_id).await?;
    apply_and_emit_at_revision(
        app,
        root,
        task_id,
        OrchestratorEvent::ImplementationInterrupted { interruption },
        expected_updated_at,
    )
    .await
}

#[tauri::command]
pub async fn coding_task_resume(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    apply_and_emit(
        app,
        root,
        task_id,
        OrchestratorEvent::InterruptedTaskResumed,
    )
    .await
}

#[tauri::command]
pub async fn coding_orchestrator_report_implementation(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    run_id: Option<String>,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    let readonly = run_id
        .as_deref()
        .map(|run| changeset::checkpoint_run_is_readonly(&root, &task_id, run))
        .transpose()?
        .unwrap_or(false);
    apply_and_emit(
        app,
        root,
        task_id,
        if readonly {
            OrchestratorEvent::ReadonlyFinished
        } else {
            OrchestratorEvent::ImplementationFinished
        },
    )
    .await
}

#[tauri::command]
pub async fn coding_task_begin_verification(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    expected_updated_at: Option<String>,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    changeset::sync_changes(&root, &task_id).await?;
    apply_and_emit_at_revision(
        app,
        root,
        task_id,
        OrchestratorEvent::VerificationStarted,
        expected_updated_at,
    )
    .await
}

#[tauri::command]
pub async fn coding_orchestrator_report_verification(
    app: AppHandle,
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    // Close the external-editor race: verification only becomes delivery
    // evidence when the workspace still matches the revision captured at the
    // start of this batch.
    changeset::sync_changes(&root, &task_id).await?;
    apply_and_emit(app, root, task_id, OrchestratorEvent::VerificationFinished).await
}

#[tauri::command]
pub async fn coding_orchestrator_state(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<OrchestratorState, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || {
        let task = task::load(&root, &task_id).ok_or_else(|| "任务不存在".to_string())?;
        Ok(OrchestratorState {
            problems: diagnostics::load_snapshot(&root, &task_id),
            repair_rounds: repair_rounds(&root, &task_id),
            changed_file_count: changeset::load(&root, &task_id).changes.len(),
            max_repair_rounds: DEFAULT_MAX_REPAIR_ROUNDS,
            task,
        })
    })
    .await
    .map_err(|error| format!("读取编排状态失败：{error}"))?
}

#[cfg(test)]
mod tests_v2 {
    use super::*;
    use crate::coding::task::RuntimePlanEntry;
    use crate::coding::verification::{record_from_parts, VerificationKind};

    fn record(command: &str, exit: i32) -> VerificationRecord {
        record_from_parts(
            "task-1",
            VerificationKind::Test,
            command,
            Some(exit),
            String::new(),
            String::new(),
            10,
            false,
            false,
        )
    }

    #[test]
    fn fresh_green_batch_delivers_without_a_manual_acceptance_phase() {
        let decision =
            decide_after_verification(&[record("pnpm build", 0), record("pnpm test", 0)], 3);
        assert_eq!(decision.next_phase, TaskPhase::Delivered);
        assert_eq!(decision.reason, "任务已完成，2 项自动检查通过");
        assert!(decision.blocker.is_none());
    }

    #[test]
    fn failed_checks_remain_failures_and_unchanged_runs_can_finish() {
        assert_eq!(
            decide_after_verification(&[record("pnpm test", 1)], 2).next_phase,
            TaskPhase::Diagnosing
        );
        let unchanged = decide_after_verification(&[record("pnpm test", 0)], 0);
        assert_eq!(unchanged.next_phase, TaskPhase::Delivered);
        assert!(unchanged.blocker.is_none());
    }

    #[test]
    fn requirement_enters_discovery_and_starts_the_root_node() {
        let dir = tempfile::tempdir().unwrap();
        let task = task::create_task(dir.path(), "task", "implement it").unwrap();
        let (started, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();
        assert_eq!(decision.next_phase, TaskPhase::Discovering);
        assert_eq!(started.task_nodes[0].status, TaskNodeStatus::Running);
        assert_eq!(started.task_nodes[0].attempt, 1);
        assert_eq!(
            started.updated_at,
            task::load(dir.path(), &task.id).unwrap().updated_at
        );
        assert!(task::execution_ledger(dir.path(), &task.id)
            .iter()
            .any(|entry| entry.kind == "requirement_submitted"));
    }

    #[test]
    fn read_only_code_explanation_delivers_without_fake_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let task = task::create_task(
            dir.path(),
            "explain",
            "解释当前模块调用链，只做分析，不修改文件",
        )
        .unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();

        let (delivered, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();

        assert_eq!(decision.next_phase, TaskPhase::Delivered);
        assert!(decision.reason.contains("只读分析未修改工程文件"));
        assert_eq!(delivered.task_nodes[0].status, TaskNodeStatus::Success);
        assert!(delivered.blocker.is_none());
    }

    #[test]
    fn read_only_code_explanation_repairs_any_file_write() {
        let dir = tempfile::tempdir().unwrap();
        let task = task::create_task(
            dir.path(),
            "explain",
            "解释当前模块调用链，只做分析，不修改文件",
        )
        .unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();
        std::fs::write(dir.path().join("analysis.md"), "Should be chat output\n").unwrap();
        changeset::sync_from_filesystem(dir.path(), &task.id).unwrap();

        let (_, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();

        assert_eq!(decision.next_phase, TaskPhase::Repairing);
        assert!(diagnostics::load_snapshot(dir.path(), &task.id)
            .iter()
            .any(|problem| problem.message.contains("只读代码解释任务不得修改")));
    }

    #[test]
    fn ordinary_no_change_task_is_not_mistaken_for_read_only_constraint() {
        let dir = tempfile::tempdir().unwrap();
        let task =
            task::create_task(dir.path(), "implement", "添加接口测试，不要修改业务逻辑").unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();

        let (_, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();

        assert_eq!(decision.next_phase, TaskPhase::Delivered);
        assert_eq!(decision.reason, "本轮已完成，未产生文件修改");
    }

    #[test]
    fn mixed_implementation_and_comments_use_the_normal_verification_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cache.ts"), "export const ttl = 1;\n").unwrap();
        let task =
            task::create_task(dir.path(), "implement", "为登录服务增加缓存并补充代码注释").unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cache.ts"),
            "// Login cache TTL.\nexport const ttl = 30;\n",
        )
        .unwrap();
        changeset::sync_from_filesystem(dir.path(), &task.id).unwrap();

        let (_, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();

        assert_eq!(decision.next_phase, TaskPhase::Delivered);
        assert!(!decision.reason.contains("注释生成包含可执行逻辑变更"));
    }

    #[test]
    fn comment_only_task_repairs_executable_changes_before_verification() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("total.ts"), "export const total = 1;\n").unwrap();
        let mut task = task::create_task(
            dir.path(),
            "document",
            "为 total.ts 添加代码注释，不改变任何可执行逻辑",
        )
        .unwrap();
        task.acceptance_criteria.push(AcceptanceCriterion {
            id: "DOC:acceptance:0".into(),
            content: "typecheck passes".into(),
            satisfied: false,
            evidence: Vec::new(),
        });
        task::save(dir.path(), &task).unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("total.ts"),
            "/** Current total. */\nexport const total = 2;\n",
        )
        .unwrap();
        changeset::sync_from_filesystem(dir.path(), &task.id).unwrap();

        let (repairing, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();

        assert_eq!(decision.next_phase, TaskPhase::Repairing);
        assert!(decision.reason.contains("可执行逻辑变更"));
        assert!(diagnostics::load_snapshot(dir.path(), &task.id)
            .iter()
            .any(|problem| problem.kind == diagnostics::ProblemKind::Documentation));
        assert!(task::execution_ledger(dir.path(), &task.id)
            .iter()
            .any(|entry| entry.kind == "documentation_validation_requested"));
        assert!(repairing.blocker.is_none());
    }

    #[test]
    fn comment_only_task_retries_when_agent_only_claimed_completion() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("total.ts"), "export const total = 1;\n").unwrap();
        let task = task::create_task(
            dir.path(),
            "document",
            "为 total.ts 添加代码注释，不改变任何可执行逻辑",
        )
        .unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();

        let (_, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();

        assert_eq!(decision.next_phase, TaskPhase::Repairing);
        assert!(decision.reason.contains("尚未实际写入"));
        assert!(diagnostics::load_snapshot(dir.path(), &task.id)
            .iter()
            .any(|problem| problem.message.contains("未产生任何文件变更")));
    }

    #[test]
    fn source_comment_request_rejects_document_only_substitution() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("total.ts"), "export const total = 1;\n").unwrap();
        let task = task::create_task(
            dir.path(),
            "document",
            "为 total.ts 添加代码注释，不改变任何可执行逻辑",
        )
        .unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.md"), "Example comments only\n").unwrap();
        changeset::sync_from_filesystem(dir.path(), &task.id).unwrap();

        let (_, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();

        assert_eq!(decision.next_phase, TaskPhase::Repairing);
        assert!(decision.reason.contains("文档产物未满足原始需求"));
        assert!(diagnostics::load_snapshot(dir.path(), &task.id)
            .iter()
            .any(|problem| problem.message.contains("不能用单独的说明文档替代")));
    }

    #[test]
    fn architecture_document_request_requires_a_reviewable_document_artifact() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("total.ts"), "export const total = 1;\n").unwrap();
        let task = task::create_task(
            dir.path(),
            "architecture",
            "生成系统级架构文档，不改变任何可执行逻辑",
        )
        .unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("total.ts"),
            "/** Public total. */\nexport const total = 1;\n",
        )
        .unwrap();
        changeset::sync_from_filesystem(dir.path(), &task.id).unwrap();

        let (_, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();

        assert_eq!(decision.next_phase, TaskPhase::Repairing);
        assert!(diagnostics::load_snapshot(dir.path(), &task.id)
            .iter()
            .any(|problem| problem.message.contains("没有可审阅的文档文件")));
    }

    #[test]
    fn comment_only_task_reaches_normal_verification_when_logic_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("total.ts"), "export const total = 1;\n").unwrap();
        let task = task::create_task(
            dir.path(),
            "document",
            "为 total.ts 添加代码注释，不改变任何可执行逻辑",
        )
        .unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("total.ts"),
            "/** Current total. */\nexport const total = 1;\n",
        )
        .unwrap();
        changeset::sync_from_filesystem(dir.path(), &task.id).unwrap();

        let (_, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();

        assert_eq!(decision.next_phase, TaskPhase::Delivered);
        assert!(diagnostics::load_snapshot(dir.path(), &task.id).is_empty());
    }

    #[test]
    fn verification_execution_errors_leave_a_recoverable_blocked_task() {
        let dir = tempfile::tempdir().unwrap();
        let mut task = task::create_task(dir.path(), "task", "implement it").unwrap();
        task.phase = TaskPhase::Verifying;
        task::save(dir.path(), &task).unwrap();

        let (blocked, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::StartFailed {
                reason: "verification command could not start".into(),
            },
        )
        .unwrap();

        assert_eq!(decision.next_phase, TaskPhase::Blocked);
        assert_eq!(decision.reason, "验证执行中断");
        assert_eq!(
            blocked.blocker.as_deref(),
            Some("verification command could not start")
        );
    }

    #[test]
    fn structured_plan_automatically_schedules_unfinished_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let task = task::create_task(dir.path(), "task", "implement it").unwrap();
        let mut task = task;
        task.strict_workflow = true;
        task::save(dir.path(), &task).unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();
        task::sync_runtime_plan(
            dir.path(),
            &task.id,
            vec![RuntimePlanEntry {
                key: "T1".into(),
                content: "write code".into(),
                dependencies: vec![],
                related_files: vec!["result.txt".into()],
                read_set: vec![],
                write_set: vec!["result.txt".into()],
                consumes: vec![],
                produces: vec![],
                acceptance_criteria: vec!["result exists".into()],
                verification_commands: vec!["test -f result.txt".into()],
                status: TaskNodeStatus::Pending,
                priority: "high".into(),
            }],
        )
        .unwrap();
        apply(dir.path(), &task.id, OrchestratorEvent::PlanSynchronized).unwrap();
        std::fs::write(dir.path().join("result.txt"), "done\n").unwrap();
        changeset::sync_from_filesystem(dir.path(), &task.id).unwrap();
        let (scheduled, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();
        assert_eq!(decision.next_phase, TaskPhase::Implementing);
        assert_eq!(scheduled.next_action, Some(TaskNextAction::ContinueNode));
        assert_eq!(scheduled.task_nodes[0].status, TaskNodeStatus::Running);
        assert_eq!(scheduled.task_nodes[0].attempt, 1);
        assert!(task::execution_ledger(dir.path(), &task.id)
            .iter()
            .any(|entry| entry.kind == "node_scheduled"));
    }

    #[test]
    fn structured_plan_requires_every_declared_verification_command() {
        let dir = tempfile::tempdir().unwrap();
        let mut task = task::create_task(dir.path(), "task", "implement it").unwrap();
        task.task_nodes[0].verification_commands = vec!["pnpm test -- unit".into()];
        let records = vec![record("pnpm build", 0)];
        assert_eq!(
            missing_planned_commands(&task, &records),
            ["pnpm test -- unit"]
        );
        assert!(missing_planned_commands(&task, &[record("pnpm test -- unit", 0)]).is_empty());
    }

    #[test]
    fn acceptance_evidence_is_specific_and_revision_bound() {
        let dir = tempfile::tempdir().unwrap();
        let mut coding_task = task::create_task(dir.path(), "task", "implement it").unwrap();
        coding_task.task_nodes[0].plan_key = "T1".into();
        coding_task.task_nodes[0].verification_commands = vec!["pnpm test -- unit".into()];
        coding_task.acceptance_criteria[0].satisfied = true;
        coding_task.acceptance_criteria[0].evidence = vec!["generic evidence".into()];
        coding_task.acceptance_criteria.push(AcceptanceCriterion {
            id: "T1:acceptance:0".into(),
            content: "unit behavior works".into(),
            satisfied: false,
            evidence: Vec::new(),
        });
        let set = changeset::load(dir.path(), &coding_task.id);

        bind_acceptance_evidence(&mut coding_task, &set, &[record("pnpm test -- unit", 0)]);

        assert!(!coding_task.acceptance_criteria[0].satisfied);
        assert!(coding_task.acceptance_criteria[0].evidence.is_empty());
        assert!(coding_task.acceptance_criteria[1].satisfied);
        assert_eq!(
            coding_task.acceptance_criteria[1].evidence,
            ["pnpm test -- unit 通过（退出码 0）"]
        );

        let revision = set.content_revision();
        coding_task.acceptance_criteria[0].satisfied = true;
        coding_task.acceptance_criteria[0].evidence = vec![format!(
            "用户于 2026-09-14T00:00:00Z 在交付报告中确认已满足（内容版本 {revision}）"
        )];
        bind_acceptance_evidence(&mut coding_task, &set, &[record("pnpm test -- unit", 0)]);
        assert!(coding_task.acceptance_criteria[0].satisfied);
    }

    #[test]
    fn actual_writes_force_replan_before_verification_and_can_recover() {
        let dir = tempfile::tempdir().unwrap();
        let task = task::create_task(dir.path(), "task", "implement it").unwrap();
        let mut task = task;
        task.strict_workflow = true;
        task::save(dir.path(), &task).unwrap();
        changeset::capture_filesystem_baseline(dir.path(), &task.id).unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();
        let plan = |write_set: Vec<String>| RuntimePlanEntry {
            key: "T1".into(),
            content: "write code".into(),
            dependencies: vec![],
            related_files: write_set.clone(),
            read_set: vec![],
            write_set,
            consumes: vec![],
            produces: vec![],
            acceptance_criteria: vec!["result exists".into()],
            verification_commands: vec!["test -f extra.txt".into()],
            status: TaskNodeStatus::Success,
            priority: "high".into(),
        };
        task::sync_runtime_plan(dir.path(), &task.id, vec![plan(vec!["planned.txt".into()])])
            .unwrap();
        apply(dir.path(), &task.id, OrchestratorEvent::PlanSynchronized).unwrap();
        std::fs::write(dir.path().join("extra.txt"), "done\n").unwrap();
        changeset::sync_from_filesystem(dir.path(), &task.id).unwrap();

        let (replan, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();
        assert_eq!(decision.next_phase, TaskPhase::Implementing);
        assert_eq!(replan.next_action, Some(TaskNextAction::RevisePlan));
        assert!(replan
            .plan_issues
            .iter()
            .any(|issue| issue.code == "unplanned_write"));

        task::sync_runtime_plan(dir.path(), &task.id, vec![plan(vec!["extra.txt".into()])])
            .unwrap();
        apply(dir.path(), &task.id, OrchestratorEvent::PlanSynchronized).unwrap();
        let (ready, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();
        assert_eq!(decision.next_phase, TaskPhase::Verifying);
        assert!(ready.next_action.is_none());
    }

    #[test]
    fn node_scheduler_stops_after_bounded_no_progress_retries() {
        let dir = tempfile::tempdir().unwrap();
        let mut task = task::create_task(dir.path(), "task", "implement it").unwrap();
        task.task_nodes[0].plan_key = "T1".into();
        task.task_nodes[0].attempt = DEFAULT_MAX_NODE_ATTEMPTS;
        task.task_nodes[0].status = TaskNodeStatus::Running;
        let error = schedule_next_node(&mut task).unwrap_err();
        assert!(error.contains("连续执行 3 次"));
    }

    #[test]
    fn followup_reopens_terminal_task_as_the_same_agent_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let mut task = task::create_task(dir.path(), "task", "implement it").unwrap();
        task.phase = TaskPhase::Delivered;
        task::save(dir.path(), &task).unwrap();
        let (reopened, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::FollowupStarted {
                requirement: "add regression test".into(),
            },
        )
        .unwrap();
        assert_eq!(decision.next_phase, TaskPhase::Discovering);
        assert_eq!(reopened.task_nodes[0].plan_key, "ROOT");
        assert_eq!(reopened.task_nodes[0].status, TaskNodeStatus::Running);
        assert!(reopened
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.content == "add regression test"));
    }

    #[test]
    fn delayed_verification_admission_cannot_reopen_a_stopped_task() {
        let dir = tempfile::tempdir().unwrap();
        let mut current = task::create_task(dir.path(), "task", "verify it").unwrap();
        current.phase = TaskPhase::Verifying;
        current.updated_at = "original-revision".into();
        task::save(dir.path(), &current).unwrap();
        apply(
            dir.path(),
            &current.id,
            OrchestratorEvent::ImplementationInterrupted {
                interruption: TaskInterruption::Stopped,
            },
        )
        .unwrap();
        assert!(apply_at_revision(
            dir.path(),
            &current.id,
            OrchestratorEvent::VerificationStarted,
            Some(&current.updated_at)
        )
        .is_err());
        assert_eq!(
            task::load(dir.path(), &current.id).unwrap().phase,
            TaskPhase::Stopped
        );
    }

    #[test]
    fn abandoned_followup_settles_only_its_own_revision() {
        for newer_round in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut current = task::create_task(dir.path(), "task", "implement it").unwrap();
            current.phase = TaskPhase::Delivered;
            task::save(dir.path(), &current).unwrap();
            let (reopened, _) = apply(
                dir.path(),
                &current.id,
                OrchestratorEvent::FollowupStarted {
                    requirement: "queued requirement".into(),
                },
            )
            .unwrap();
            if newer_round {
                apply(
                    dir.path(),
                    &current.id,
                    OrchestratorEvent::ImplementationInterrupted {
                        interruption: TaskInterruption::Stopped,
                    },
                )
                .unwrap();
                assert!(apply_at_revision(
                    dir.path(),
                    &current.id,
                    OrchestratorEvent::ImplementationInterrupted {
                        interruption: TaskInterruption::Paused
                    },
                    Some(&reopened.updated_at),
                )
                .is_err());
                assert_eq!(
                    task::load(dir.path(), &current.id).unwrap().phase,
                    TaskPhase::Stopped
                );
                let (newer, _) = apply(
                    dir.path(),
                    &current.id,
                    OrchestratorEvent::FollowupStarted {
                        requirement: "newer requirement".into(),
                    },
                )
                .unwrap();
                assert!(apply_at_revision(
                    dir.path(),
                    &current.id,
                    OrchestratorEvent::ImplementationInterrupted {
                        interruption: TaskInterruption::Paused
                    },
                    Some(&reopened.updated_at),
                )
                .is_err());
                let latest = task::load(dir.path(), &current.id).unwrap();
                assert_eq!(latest.updated_at, newer.updated_at);
                assert_eq!(latest.phase, TaskPhase::Discovering);
            } else {
                let (paused, _) = apply_at_revision(
                    dir.path(),
                    &current.id,
                    OrchestratorEvent::ImplementationInterrupted {
                        interruption: TaskInterruption::Paused,
                    },
                    Some(&reopened.updated_at),
                )
                .unwrap();
                assert_eq!(paused.phase, TaskPhase::Paused);
                assert_eq!(paused.task_nodes[0].status, TaskNodeStatus::Pending);
            }
        }
    }

    #[test]
    fn verification_and_diagnosis_can_be_stopped_without_becoming_start_failures() {
        for phase in [TaskPhase::Verifying, TaskPhase::Diagnosing] {
            let dir = tempfile::tempdir().unwrap();
            let mut current = task::create_task(dir.path(), "task", "verify it").unwrap();
            current.phase = phase;
            task::save(dir.path(), &current).unwrap();
            let (stopped, _) = apply(
                dir.path(),
                &current.id,
                OrchestratorEvent::ImplementationInterrupted {
                    interruption: TaskInterruption::Stopped,
                },
            )
            .unwrap();
            assert_eq!(stopped.phase, TaskPhase::Stopped);
            assert!(stopped.next_action.is_none());
            assert!(apply(
                dir.path(),
                &current.id,
                OrchestratorEvent::StartFailed {
                    reason: "late verification error".into()
                }
            )
            .is_err());
            assert_eq!(
                task::load(dir.path(), &current.id).unwrap().phase,
                TaskPhase::Stopped
            );
        }
    }

    #[test]
    fn user_stop_is_recoverable_and_resume_preserves_the_round() {
        let dir = tempfile::tempdir().unwrap();
        let task = task::create_task(dir.path(), "task", "implement it").unwrap();
        apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::RequirementSubmitted,
        )
        .unwrap();

        let (stopped, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationInterrupted {
                interruption: TaskInterruption::Stopped,
            },
        )
        .unwrap();
        assert_eq!(decision.next_phase, TaskPhase::Stopped);
        assert!(stopped.blocker.is_none());
        assert_eq!(stopped.task_nodes[0].status, TaskNodeStatus::Pending);
        assert_eq!(stopped.task_nodes[0].attempt, 1);

        let (resumed, decision) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::InterruptedTaskResumed,
        )
        .unwrap();
        assert_eq!(decision.next_phase, TaskPhase::Discovering);
        assert_eq!(resumed.task_nodes[0].status, TaskNodeStatus::Running);
        assert_eq!(resumed.task_nodes[0].attempt, 1);
        let kinds = task::execution_ledger(dir.path(), &task.id)
            .into_iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>();
        assert!(kinds.contains(&"implementation_interrupted".to_string()));
        assert!(kinds.contains(&"interrupted_task_resumed".to_string()));
    }

    #[test]
    fn only_the_known_legacy_stop_blocker_can_be_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let mut task = task::create_task(dir.path(), "task", "implement it").unwrap();
        task.phase = TaskPhase::Blocked;
        task.phase_reason = Some("实现阶段结束但没有代码变更".into());
        task.blocker =
            Some("Agent 结束了实现但没有写入任何文件。请检查是否只在会话里返回了示例代码。".into());
        task.task_nodes[0].status = TaskNodeStatus::Running;
        task::save(dir.path(), &task).unwrap();

        let (repaired, _) = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationInterrupted {
                interruption: TaskInterruption::Stopped,
            },
        )
        .unwrap();
        assert_eq!(repaired.phase, TaskPhase::Stopped);
        assert!(repaired.blocker.is_none());

        let mut real_blocker = repaired;
        real_blocker.phase = TaskPhase::Blocked;
        real_blocker.phase_reason = Some("执行图无法继续调度".into());
        real_blocker.blocker = Some("循环依赖".into());
        task::save(dir.path(), &real_blocker).unwrap();
        let error = apply(
            dir.path(),
            &task.id,
            OrchestratorEvent::ImplementationInterrupted {
                interruption: TaskInterruption::Stopped,
            },
        )
        .unwrap_err();
        assert!(error.contains("正在执行"));
        let unchanged = task::load(dir.path(), &task.id).unwrap();
        assert_eq!(unchanged.phase, TaskPhase::Blocked);
        assert_eq!(unchanged.blocker.as_deref(), Some("循环依赖"));
    }

    #[test]
    fn identical_repair_failures_stop_instead_of_looping() {
        let mut problem =
            crate::coding::diagnostics::parse_record(&record("pnpm test", 1)).remove(0);
        problem.fingerprint = "same".into();
        let decision = next_after_repair(&[problem], &["same".into()], 1, 3);
        assert_eq!(decision.next_phase, TaskPhase::Blocked);
        assert!(decision.blocker.as_deref().unwrap().contains("相同错误"));
    }

    #[tokio::test]
    async fn test_only_turn_pauses_until_real_red_failure_is_recorded() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut coding_task = task::create_task(root, "TDD", "Fix behavior").unwrap();
        coding_task.phase = TaskPhase::Implementing;
        coding_task.plan_revision = Some("test-plan".into());
        coding_task.task_nodes[0].status = TaskNodeStatus::Running;
        coding_task.task_nodes[0].verification_commands = vec!["pnpm test".into()];
        task::save(root, &coding_task).unwrap();
        let mut coding_task = coding_task;
        coding_task.strict_workflow = true;
        task::save(root, &coding_task).unwrap();
        changeset::capture_filesystem_baseline(root, &coding_task.id).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("tests/behavior.test.ts"),
            "test('red', () => { throw new Error('red') });\n",
        )
        .unwrap();
        changeset::sync_changes(root, &coding_task.id)
            .await
            .unwrap();
        let (paused, _) = apply(
            root,
            &coding_task.id,
            OrchestratorEvent::ImplementationFinished,
        )
        .unwrap();
        assert_eq!(paused.phase, TaskPhase::Paused);
        assert_eq!(paused.phase_reason.as_deref(), Some("等待红灯测试验证"));
        assert!(apply(
            root,
            &coding_task.id,
            OrchestratorEvent::InterruptedTaskResumed
        )
        .is_err());
        let mut red = verification::record_from_parts(
            &coding_task.id,
            VerificationKind::Test,
            "pnpm test",
            Some(1),
            String::new(),
            "failed".into(),
            1,
            false,
            false,
        );
        red.content_revision = Some(changeset::load(root, &coding_task.id).content_revision());
        verification::append_record(root, &red).unwrap();
        tdd::record_red(root, &coding_task.id, &red.id).unwrap();
        let (resumed, _) = apply(
            root,
            &coding_task.id,
            OrchestratorEvent::InterruptedTaskResumed,
        )
        .unwrap();
        assert_eq!(resumed.phase, TaskPhase::Implementing);
    }
    #[test]
    fn readonly_completion_does_not_run_checks_for_accumulated_agent_changes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(root.join("source.ts"), "before").unwrap();
        let task = task::create_task(root, "Plan", "给模块添加代码注释").unwrap();
        changeset::capture_filesystem_baseline(root, &task.id).unwrap();
        apply(root, &task.id, OrchestratorEvent::RequirementSubmitted).unwrap();
        std::fs::write(root.join("source.ts"), "earlier agent changes").unwrap();
        changeset::sync_from_filesystem(root, &task.id).unwrap();
        store::write_json(&store::workspace_dir(root).join("verification-config.json"),
            &serde_json::json!({"checks":[{"id":"check","kind":"test","command":"touch effect","label":"check","cwd":".","env":{},"autoRun":true}]})).unwrap();
        let (_, decision) = apply(root, &task.id, OrchestratorEvent::ReadonlyFinished).unwrap();
        assert_eq!(decision.next_phase, TaskPhase::Delivered);
        assert!(!root.join("effect").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("source.ts")).unwrap(),
            "earlier agent changes"
        );
    }
}
