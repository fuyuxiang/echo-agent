//! Coding task lifecycle. A workspace can hold several concurrent tasks; each
//! owns its requirement, acceptance criteria, plan and phase, and survives a
//! restart so an interrupted run can be resumed.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;

use crate::coding::changeset;
use crate::coding::store;
use crate::shell_fs::FilesystemAccess;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase {
    Idle,
    Discovering,
    Implementing,
    Verifying,
    Diagnosing,
    Repairing,
    Paused,
    Stopped,
    Delivered,
    Blocked,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TaskNodeStatus {
    Pending,
    Running,
    Success,
    Failed,
    Blocked,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TaskNextAction {
    RevisePlan,
    ContinueNode,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AcceptanceCriterion {
    pub id: String,
    pub content: String,
    /// Only set by the delivery layer from real evidence, never self-reported.
    pub satisfied: bool,
    pub evidence: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TaskNode {
    pub id: String,
    /// Stable identifier supplied by the runtime plan (for example `T3`).
    /// This key is mandatory in the current v2 task schema.
    #[serde(default)]
    pub plan_key: String,
    pub content: String,
    pub dependencies: Vec<String>,
    pub related_files: Vec<String>,
    /// Files that may be read or changed while executing this node.  Keeping
    /// these separate lets the scheduler detect unsafe concurrent writes.
    #[serde(default)]
    pub read_set: Vec<String>,
    #[serde(default)]
    pub write_set: Vec<String>,
    /// Exact interfaces this node expects and provides.  These are planning
    /// contracts, not guesses derived from the final diff.
    #[serde(default)]
    pub consumes: Vec<String>,
    #[serde(default)]
    pub produces: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub verification_commands: Vec<String>,
    pub status: TaskNodeStatus,
    pub priority: String,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub failure: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanIssueSeverity {
    Warning,
    Error,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlanIssue {
    pub severity: PlanIssueSeverity,
    pub code: String,
    pub message: String,
    pub node_keys: Vec<String>,
}

/// Structured form of one ACP plan entry.  ACP itself only carries display
/// strings, so the frontend parses the documented contract fields before the
/// plan crosses this trusted persistence boundary.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePlanEntry {
    pub key: String,
    pub content: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub related_files: Vec<String>,
    #[serde(default)]
    pub read_set: Vec<String>,
    #[serde(default)]
    pub write_set: Vec<String>,
    #[serde(default)]
    pub consumes: Vec<String>,
    #[serde(default)]
    pub produces: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub verification_commands: Vec<String>,
    pub status: TaskNodeStatus,
    pub priority: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionLedgerEvent {
    pub id: String,
    pub task_id: String,
    pub kind: String,
    #[serde(default)]
    pub node_key: Option<String>,
    pub message: String,
    #[serde(default)]
    pub plan_revision: Option<String>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CodingTask {
    /// Legacy versions are upgraded in memory and saved on the next mutation.
    #[serde(default)]
    pub schema_version: u32,
    /// Explicit opt-in for the former DAG/TDD/delivery policy.
    #[serde(default)]
    pub strict_workflow: bool,
    pub id: String,
    pub name: String,
    pub requirement: String,
    /// Workspace-relative paths explicitly pinned by the user for this task.
    /// Keeping them with the task prevents context from leaking across tasks
    /// and makes a resumed task reconstruct the same Agent input contract.
    #[serde(default)]
    pub context_paths: Vec<String>,
    pub phase: TaskPhase,
    #[serde(default)]
    pub phase_reason: Option<String>,
    #[serde(default)]
    pub blocker: Option<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    #[serde(default)]
    pub task_nodes: Vec<TaskNode>,
    /// Hash of the latest structured runtime plan. Missing only while a simple
    /// task still uses its initial ROOT node.
    #[serde(default)]
    pub plan_revision: Option<String>,
    #[serde(default)]
    pub plan_updated_at: Option<String>,
    #[serde(default)]
    pub plan_issues: Vec<PlanIssue>,
    #[serde(default)]
    pub global_constraints: Vec<String>,
    /// Machine-readable instruction for the renderer.  The renderer may start
    /// another managed Agent turn only when this field is present, which avoids
    /// racing a plan update that arrives just as the previous stream closes.
    #[serde(default)]
    pub next_action: Option<TaskNextAction>,
    pub model_id: Option<String>,
    pub session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TaskSummary {
    pub id: String,
    pub name: String,
    pub phase: TaskPhase,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Default)]
struct TaskIndex {
    tasks: Vec<TaskSummary>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn task_path(root: &Path, task_id: &str) -> std::path::PathBuf {
    store::task_dir(root, task_id).join("task.json")
}

fn ledger_path(root: &Path, task_id: &str) -> std::path::PathBuf {
    store::task_dir(root, task_id).join("execution-ledger.jsonl")
}

pub fn execution_ledger(root: &Path, task_id: &str) -> Vec<ExecutionLedgerEvent> {
    store::read_jsonl(&ledger_path(root, task_id))
}

pub(crate) fn append_ledger(
    root: &Path,
    task_id: &str,
    kind: &str,
    node_key: Option<String>,
    message: String,
    plan_revision: Option<String>,
) -> Result<(), String> {
    store::validate_task_id(task_id)?;
    store::append_jsonl(
        &ledger_path(root, task_id),
        &ExecutionLedgerEvent {
            id: uuid::Uuid::now_v7().to_string(),
            task_id: task_id.to_string(),
            kind: kind.to_string(),
            node_key,
            message,
            plan_revision,
            created_at: now(),
        },
    )
}

pub fn list_tasks(root: &Path) -> Vec<TaskSummary> {
    store::read_json::<TaskIndex>(&store::tasks_index_path(root))
        .unwrap_or_default()
        .tasks
        .into_iter()
        .filter(|summary| load(root, &summary.id).is_some())
        .collect()
}

/// Refresh this task's row in the index, keeping insertion order stable so the
/// task switcher does not reshuffle while a run is in progress.
fn upsert_index(root: &Path, task: &CodingTask) -> Result<(), String> {
    let summary = TaskSummary {
        id: task.id.clone(),
        name: task.name.clone(),
        phase: task.phase,
        updated_at: task.updated_at.clone(),
    };
    store::update_json(&store::tasks_index_path(root), |index: &mut TaskIndex| {
        // Keep unreadable/future-version rows for diagnosis. A normal save
        // must never erase another task's history or migration failure.
        match index.tasks.iter_mut().find(|entry| entry.id == task.id) {
            Some(entry) => *entry = summary,
            None => index.tasks.push(summary),
        }
        Ok(())
    })
}

pub fn load(root: &Path, task_id: &str) -> Option<CodingTask> {
    store::validate_task_id(task_id).ok()?;
    let mut task = store::read_json::<CodingTask>(&task_path(root, task_id))?;
    if task.id != task_id || task.schema_version > 2 {
        return None;
    }
    // Additive compatibility: never delete or rewrite a legacy record while
    // reading it. The next normal save persists this idempotent upgrade.
    task.schema_version = 2;
    if !task.strict_workflow {
        task.global_constraints.retain(|constraint| {
            constraint
                != "Complex work must use a validated dependency graph and bounded write scopes"
                && constraint != "Every planned verification command must pass before delivery"
        });
        for constraint in &mut task.global_constraints {
            if constraint == "Do not claim completion without fresh verification evidence" {
                *constraint = "Distinguish turn completion from actual check results; never claim unexecuted checks passed".into();
            }
        }
    }
    for (index, node) in task.task_nodes.iter_mut().enumerate() {
        if node.plan_key.is_empty() {
            node.plan_key = if index == 0 {
                "ROOT".into()
            } else {
                format!("T{}", index + 1)
            };
        }
    }
    Some(task)
}

pub fn save(root: &Path, task: &CodingTask) -> Result<(), String> {
    store::validate_task_id(&task.id)?;
    if task.schema_version != 2 {
        return Err("不支持的代码开发任务数据版本".into());
    }
    let path = task_path(root, &task.id);
    if path.exists() {
        let original = store::read_json::<serde_json::Value>(&path)
            .ok_or_else(|| format!("旧任务记录不可读取，已保留 {}", path.display()))?;
        let version = original
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if version > 2 {
            return Err("未来版本任务记录不能被当前版本覆盖".into());
        }
        if version < 2 {
            let backup = path.with_file_name("task.legacy.json");
            if !backup.exists() {
                let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
                store::write_bytes_atomic(&backup, &bytes)?;
            }
        }
    }
    let mut stored = task.clone();
    stored.updated_at = now();
    store::write_json(&path, &stored)?;
    upsert_index(root, &stored)
}

pub fn create_task(root: &Path, name: &str, requirement: &str) -> Result<CodingTask, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("任务名称不能为空".into());
    }
    let timestamp = now();
    let requirement = requirement.trim().to_string();
    if requirement.is_empty() {
        return Err("任务需求不能为空".into());
    }
    let task = CodingTask {
        schema_version: 2,
        strict_workflow: false,
        id: uuid::Uuid::now_v7().to_string(),
        name: trimmed.to_string(),
        requirement: requirement.clone(),
        context_paths: Vec::new(),
        phase: TaskPhase::Idle,
        phase_reason: None,
        blocker: None,
        acceptance_criteria: vec![AcceptanceCriterion {
            id: "requirement".into(),
            content: requirement.clone(),
            satisfied: false,
            evidence: Vec::new(),
        }],
        task_nodes: vec![TaskNode {
            id: uuid::Uuid::now_v7().to_string(),
            plan_key: "ROOT".into(),
            content: requirement,
            dependencies: Vec::new(),
            related_files: Vec::new(),
            read_set: Vec::new(),
            write_set: Vec::new(),
            consumes: Vec::new(),
            produces: Vec::new(),
            acceptance_criteria: Vec::new(),
            verification_commands: Vec::new(),
            status: TaskNodeStatus::Pending,
            priority: "high".into(),
            attempt: 0,
            started_at: None,
            completed_at: None,
            failure: None,
        }],
        plan_revision: None,
        plan_updated_at: None,
        plan_issues: Vec::new(),
        global_constraints: vec![
            "Preserve existing behavior outside the requested scope".into(),
            "Distinguish turn completion from actual check results; never claim unexecuted checks passed".into(),
            "Choose relevant checks and report their actual results independently of turn completion".into(),
        ],
        next_action: None,
        model_id: None,
        session_id: None,
        created_at: timestamp.clone(),
        updated_at: timestamp,
    };
    save(root, &task)?;
    crate::coding::isolation::associate(root, &task.id, None)?;
    Ok(task)
}

pub fn set_context_paths(
    root: &Path,
    task_id: &str,
    paths: Vec<String>,
) -> Result<CodingTask, String> {
    store::with_task_transaction(root, task_id, || {
        let mut normalized = Vec::new();
        let mut seen = HashSet::new();
        for raw in paths {
            let path = raw.trim().replace('\\', "/");
            if path.starts_with('/')
                || path.contains(':')
                || path.contains('\0')
                || path.split('/').any(|segment| segment == "..")
            {
                return Err(format!("无效的任务上下文路径：{raw}"));
            }
            let path = path.trim_start_matches("./").trim_matches('/').to_string();
            if path.is_empty()
                || path.len() > 4_096
                || path
                    .split('/')
                    .any(|segment| segment.is_empty() || segment == ".")
            {
                return Err(format!("无效的任务上下文路径：{raw}"));
            }
            if seen.insert(path.clone()) {
                normalized.push(path);
            }
            if normalized.len() > 256 {
                return Err("每个任务最多可固定 256 个上下文路径".into());
            }
        }
        let mut task = load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
        task.context_paths = normalized;
        save(root, &task)?;
        Ok(load(root, task_id).unwrap_or(task))
    })
}

pub fn bind_runtime(
    root: &Path,
    task_id: &str,
    session_id: &str,
    model_id: &str,
) -> Result<CodingTask, String> {
    store::with_task_transaction(root, task_id, || {
        if session_id.trim().is_empty() || model_id.trim().is_empty() {
            return Err("会话和模型标识不能为空".into());
        }
        let mut task = load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
        crate::coding::isolation::associate(root, task_id, Some(session_id))?;
        task.session_id = Some(session_id.to_string());
        task.model_id = Some(model_id.to_string());
        save(root, &task)?;
        Ok(load(root, task_id).unwrap_or(task))
    })
}

fn clean_values(values: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .take(limit)
        .collect()
}

fn is_valid_plan_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn normalize_plan_path(value: &str) -> Option<String> {
    let replaced = value.trim().replace('\\', "/");
    if replaced.is_empty()
        || replaced.starts_with('/')
        || replaced.contains(':')
        || (replaced.contains('*') && !replaced.ends_with("/**"))
    {
        return None;
    }
    let directory_scope = replaced.ends_with("/**");
    let base = replaced.strip_suffix("/**").unwrap_or(&replaced);
    let mut parts = Vec::new();
    for part in base.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        return None;
    }
    let normalized = parts.join("/");
    Some(if directory_scope {
        format!("{normalized}/**")
    } else {
        normalized
    })
}

fn path_scope_covers(scope: &str, path: &str) -> bool {
    let Some(scope) = normalize_plan_path(scope) else {
        return false;
    };
    let Some(path) = normalize_plan_path(path) else {
        return false;
    };
    let (scope_folded, path_folded) = if cfg!(target_os = "windows") || cfg!(target_os = "macos") {
        (scope.to_ascii_lowercase(), path.to_ascii_lowercase())
    } else {
        (scope, path)
    };
    if let Some(prefix) = scope_folded.strip_suffix("/**") {
        path_folded == prefix || path_folded.starts_with(&format!("{prefix}/"))
    } else {
        scope_folded == path_folded
    }
}

fn scopes_overlap(left: &str, right: &str) -> bool {
    path_scope_covers(left, right) || path_scope_covers(right, left)
}

fn normalize_plan_paths(values: &mut Vec<String>) {
    *values = values
        .drain(..)
        .filter_map(|value| normalize_plan_path(&value))
        .collect();
    values.sort();
    values.dedup();
}

/// True when at least one node explicitly owns the changed path. Directory
/// scopes use a trailing `/**`; all other values are exact repository paths.
pub fn plan_covers_path(task: &CodingTask, path: &str) -> bool {
    task.task_nodes.iter().any(|node| {
        node.write_set
            .iter()
            .any(|scope| path_scope_covers(scope, path))
    })
}

fn transitively_depends_on(
    node: &str,
    target: &str,
    dependencies: &HashMap<String, Vec<String>>,
    seen: &mut HashSet<String>,
) -> bool {
    if !seen.insert(node.to_string()) {
        return false;
    }
    dependencies.get(node).is_some_and(|direct| {
        direct.iter().any(|dependency| {
            dependency == target || transitively_depends_on(dependency, target, dependencies, seen)
        })
    })
}

fn validate_runtime_plan(entries: &[RuntimePlanEntry]) -> Vec<PlanIssue> {
    let mut issues = Vec::new();
    if entries.is_empty() {
        issues.push(PlanIssue {
            severity: PlanIssueSeverity::Error,
            code: "empty_plan".into(),
            message: "执行计划不能为空".into(),
            node_keys: Vec::new(),
        });
        return issues;
    }

    let mut keys = HashSet::new();
    for entry in entries {
        if !is_valid_plan_key(&entry.key) {
            issues.push(PlanIssue {
                severity: PlanIssueSeverity::Error,
                code: "invalid_key".into(),
                message: format!("节点标识“{}”不合法", entry.key),
                node_keys: vec![entry.key.clone()],
            });
        } else if !keys.insert(entry.key.clone()) {
            issues.push(PlanIssue {
                severity: PlanIssueSeverity::Error,
                code: "duplicate_key".into(),
                message: format!("节点标识“{}”重复", entry.key),
                node_keys: vec![entry.key.clone()],
            });
        }
        if entry.content.trim().is_empty() {
            issues.push(PlanIssue {
                severity: PlanIssueSeverity::Error,
                code: "empty_goal".into(),
                message: format!("节点“{}”没有可执行目标", entry.key),
                node_keys: vec![entry.key.clone()],
            });
        }
        if entry.write_set.is_empty() {
            issues.push(PlanIssue {
                severity: PlanIssueSeverity::Error,
                code: "missing_write_set".into(),
                message: format!("节点“{}”没有声明写入文件范围", entry.key),
                node_keys: vec![entry.key.clone()],
            });
        }
        if entry.acceptance_criteria.is_empty() {
            issues.push(PlanIssue {
                severity: PlanIssueSeverity::Error,
                code: "missing_acceptance".into(),
                message: format!("节点“{}”没有可观测的验收条件", entry.key),
                node_keys: vec![entry.key.clone()],
            });
        }
        if entry.verification_commands.is_empty() {
            issues.push(PlanIssue {
                severity: PlanIssueSeverity::Error,
                code: "missing_verification".into(),
                message: format!("节点“{}”没有声明验证命令", entry.key),
                node_keys: vec![entry.key.clone()],
            });
        }
        for path in entry
            .related_files
            .iter()
            .chain(&entry.read_set)
            .chain(&entry.write_set)
        {
            if normalize_plan_path(path).is_none() {
                issues.push(PlanIssue {
                    severity: PlanIssueSeverity::Error,
                    code: "invalid_file_scope".into(),
                    message: format!("节点“{}”包含不安全或无法识别的文件范围“{path}”", entry.key),
                    node_keys: vec![entry.key.clone()],
                });
            }
        }
    }

    let dependencies: HashMap<String, Vec<String>> = entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.dependencies.clone()))
        .collect();
    for entry in entries {
        for dependency in &entry.dependencies {
            if dependency == &entry.key {
                issues.push(PlanIssue {
                    severity: PlanIssueSeverity::Error,
                    code: "self_dependency".into(),
                    message: format!("节点“{}”不能依赖自己", entry.key),
                    node_keys: vec![entry.key.clone()],
                });
            } else if !keys.contains(dependency) {
                issues.push(PlanIssue {
                    severity: PlanIssueSeverity::Error,
                    code: "missing_dependency".into(),
                    message: format!("节点“{}”依赖不存在的节点“{dependency}”", entry.key),
                    node_keys: vec![entry.key.clone(), dependency.clone()],
                });
            }
        }
        if transitively_depends_on(&entry.key, &entry.key, &dependencies, &mut HashSet::new()) {
            issues.push(PlanIssue {
                severity: PlanIssueSeverity::Error,
                code: "dependency_cycle".into(),
                message: format!("从节点“{}”发现循环依赖", entry.key),
                node_keys: vec![entry.key.clone()],
            });
        }
    }

    let running = entries
        .iter()
        .filter(|entry| entry.status == TaskNodeStatus::Running)
        .map(|entry| entry.key.clone())
        .collect::<Vec<_>>();
    if running.len() > 1 {
        issues.push(PlanIssue {
            severity: PlanIssueSeverity::Warning,
            code: "concurrent_nodes".into(),
            message: "多个实现节点同时运行，只有写入集完全隔离时才安全".into(),
            node_keys: running,
        });
    }

    for (index, left) in entries.iter().enumerate() {
        for right in entries.iter().skip(index + 1) {
            let overlaps = left.write_set.iter().any(|left_scope| {
                right
                    .write_set
                    .iter()
                    .any(|right_scope| scopes_overlap(left_scope, right_scope))
            });
            if !overlaps {
                continue;
            }
            let ordered =
                transitively_depends_on(&left.key, &right.key, &dependencies, &mut HashSet::new())
                    || transitively_depends_on(
                        &right.key,
                        &left.key,
                        &dependencies,
                        &mut HashSet::new(),
                    );
            if !ordered {
                issues.push(PlanIssue {
                    severity: PlanIssueSeverity::Error,
                    code: "write_conflict".into(),
                    message: format!(
                        "节点“{}”与“{}”修改相同文件却没有依赖顺序",
                        left.key, right.key
                    ),
                    node_keys: vec![left.key.clone(), right.key.clone()],
                });
            }
        }
    }

    let mut producers: HashMap<&str, &str> = HashMap::new();
    for entry in entries {
        for contract in &entry.produces {
            if let Some(previous) = producers.insert(contract.as_str(), entry.key.as_str()) {
                issues.push(PlanIssue {
                    severity: PlanIssueSeverity::Warning,
                    code: "duplicate_producer".into(),
                    message: format!(
                        "接口“{contract}”同时由节点“{previous}”和“{}”产出",
                        entry.key
                    ),
                    node_keys: vec![previous.to_string(), entry.key.clone()],
                });
            }
        }
    }

    let statuses: HashMap<&str, TaskNodeStatus> = entries
        .iter()
        .map(|entry| (entry.key.as_str(), entry.status))
        .collect();
    for entry in entries {
        // A plan update is also the execution scheduler's source of truth.
        // Reject impossible progress instead of displaying a downstream node
        // as running while one of its prerequisites is still pending.
        if matches!(
            entry.status,
            TaskNodeStatus::Running | TaskNodeStatus::Success
        ) {
            for dependency in &entry.dependencies {
                if keys.contains(dependency)
                    && statuses.get(dependency.as_str()) != Some(&TaskNodeStatus::Success)
                {
                    issues.push(PlanIssue {
                        severity: PlanIssueSeverity::Error,
                        code: "dependency_not_completed".into(),
                        message: format!(
                            "节点“{}”已开始，但依赖节点“{dependency}”尚未完成",
                            entry.key
                        ),
                        node_keys: vec![entry.key.clone(), dependency.clone()],
                    });
                }
            }
        }

        // When an interface is produced inside this plan, consumers must
        // explicitly depend on the producer so cross-module generation cannot
        // race ahead with stale type or API assumptions.
        for consumed in &entry.consumes {
            let Some(producer) = producers.get(consumed.as_str()) else {
                continue;
            };
            if *producer != entry.key.as_str()
                && !transitively_depends_on(
                    &entry.key,
                    producer,
                    &dependencies,
                    &mut HashSet::new(),
                )
            {
                issues.push(PlanIssue {
                    severity: PlanIssueSeverity::Error,
                    code: "interface_dependency_missing".into(),
                    message: format!(
                        "节点“{}”消费接口“{consumed}”，但未依赖其产出节点“{producer}”",
                        entry.key
                    ),
                    node_keys: vec![entry.key.clone(), (*producer).to_string()],
                });
            }
        }
    }
    issues
}

fn structural_revision(entries: &[RuntimePlanEntry]) -> Result<String, String> {
    let structure = entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "key": entry.key,
                "content": entry.content,
                "dependencies": entry.dependencies,
                "relatedFiles": entry.related_files,
                "readSet": entry.read_set,
                "writeSet": entry.write_set,
                "consumes": entry.consumes,
                "produces": entry.produces,
                "acceptanceCriteria": entry.acceptance_criteria,
                "verificationCommands": entry.verification_commands,
                "priority": entry.priority,
            })
        })
        .collect::<Vec<_>>();
    let bytes =
        serde_json::to_vec(&structure).map_err(|error| format!("计划序列化失败：{error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn node_contract_matches(node: &TaskNode, entry: &RuntimePlanEntry) -> bool {
    node.content == entry.content
        && node.dependencies == entry.dependencies
        && node.related_files == entry.related_files
        && node.read_set == entry.read_set
        && node.write_set == entry.write_set
        && node.consumes == entry.consumes
        && node.produces == entry.produces
        && node.acceptance_criteria == entry.acceptance_criteria
        && node.verification_commands == entry.verification_commands
}

/// Persist the runtime's current plan as an executable DAG. Repeated ACP plan
/// updates replace structure but preserve node identity, attempts and timing.
fn sync_runtime_plan_unlocked(
    root: &Path,
    task_id: &str,
    entries: Vec<RuntimePlanEntry>,
) -> Result<CodingTask, String> {
    let mut task = load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
    if entries.len() > 100 {
        return Err("执行计划最多支持 100 个节点".into());
    }
    let mut entries = entries
        .into_iter()
        .map(|mut entry| {
            entry.key = entry.key.trim().to_ascii_uppercase();
            entry.content = entry.content.trim().to_string();
            entry.dependencies = clean_values(entry.dependencies, 100)
                .into_iter()
                .map(|value| value.to_ascii_uppercase())
                .collect();
            entry.related_files = clean_values(entry.related_files, 200)
                .into_iter()
                .map(|value| normalize_plan_path(&value).unwrap_or(value))
                .collect();
            entry.read_set = clean_values(entry.read_set, 200)
                .into_iter()
                .map(|value| normalize_plan_path(&value).unwrap_or(value))
                .collect();
            entry.write_set = clean_values(entry.write_set, 200)
                .into_iter()
                .map(|value| normalize_plan_path(&value).unwrap_or(value))
                .collect();
            entry.consumes = clean_values(entry.consumes, 100);
            entry.produces = clean_values(entry.produces, 100);
            entry.acceptance_criteria = clean_values(entry.acceptance_criteria, 100);
            entry.verification_commands = clean_values(entry.verification_commands, 50);
            entry.priority = match entry.priority.as_str() {
                "high" | "low" => entry.priority,
                _ => "medium".into(),
            };
            entry
        })
        .collect::<Vec<_>>();

    let issues = validate_runtime_plan(&entries);
    task.next_action = None;
    task.plan_issues = issues.clone();
    if issues
        .iter()
        .any(|issue| issue.severity == PlanIssueSeverity::Error)
    {
        save(root, &task)?;
        let _ = append_ledger(
            root,
            task_id,
            "plan_rejected",
            None,
            issues
                .iter()
                .filter(|issue| issue.severity == PlanIssueSeverity::Error)
                .map(|issue| issue.message.clone())
                .collect::<Vec<_>>()
                .join("；"),
            task.plan_revision.clone(),
        );
        return Ok(load(root, task_id).unwrap_or(task));
    }
    for entry in &mut entries {
        normalize_plan_paths(&mut entry.related_files);
        normalize_plan_paths(&mut entry.read_set);
        normalize_plan_paths(&mut entry.write_set);
    }

    let revision = structural_revision(&entries)?;
    let previous = std::mem::take(&mut task.acceptance_criteria);
    // Requirements submitted by the user (the original and follow-ups) are
    // durable. Only acceptance rows generated from an earlier plan revision
    // are replaced when the Agent revises its DAG.
    let mut acceptance_criteria = previous
        .iter()
        .filter(|criterion| !criterion.id.contains(":acceptance:"))
        .cloned()
        .collect::<Vec<_>>();
    if !acceptance_criteria
        .iter()
        .any(|criterion| criterion.id == "requirement")
    {
        acceptance_criteria.insert(
            0,
            AcceptanceCriterion {
                id: "requirement".into(),
                content: task.requirement.clone(),
                satisfied: false,
                evidence: Vec::new(),
            },
        );
    }
    let mut previous_plan_criteria: HashMap<String, AcceptanceCriterion> = previous
        .into_iter()
        .filter(|criterion| criterion.id.contains(":acceptance:"))
        .map(|criterion| (criterion.id.clone(), criterion))
        .collect();
    for entry in &entries {
        for (index, content) in entry.acceptance_criteria.iter().enumerate() {
            let id = format!("{}:acceptance:{index}", entry.key);
            acceptance_criteria.push(
                previous_plan_criteria
                    .remove(&id)
                    .filter(|criterion| criterion.content == *content)
                    .unwrap_or_else(|| AcceptanceCriterion {
                        id,
                        content: content.clone(),
                        satisfied: false,
                        evidence: Vec::new(),
                    }),
            );
        }
    }
    task.acceptance_criteria = acceptance_criteria;
    let previous_revision = task.plan_revision.clone();
    let mut existing: HashMap<String, TaskNode> = task
        .task_nodes
        .drain(..)
        .filter(|node| !node.plan_key.is_empty())
        .map(|node| (node.plan_key.clone(), node))
        .collect();
    let timestamp = now();
    let mut ledger_events = Vec::<(String, String, String)>::new();
    task.task_nodes = entries
        .into_iter()
        .map(|entry| {
            let previous = existing.remove(&entry.key);
            let previous_status = previous.as_ref().map(|node| node.status);
            // A re-plan may reorder downstream work, but an unchanged node
            // that already passed must not be generated a second time merely
            // because the model re-emitted it as pending.
            let status = if previous.as_ref().is_some_and(|node| {
                node.status == TaskNodeStatus::Success && node_contract_matches(node, &entry)
            }) {
                TaskNodeStatus::Success
            } else {
                entry.status
            };
            let mut attempt = previous.as_ref().map(|node| node.attempt).unwrap_or(0);
            let mut started_at = previous.as_ref().and_then(|node| node.started_at.clone());
            let mut completed_at = previous.as_ref().and_then(|node| node.completed_at.clone());
            if status == TaskNodeStatus::Running && previous_status != Some(TaskNodeStatus::Running)
            {
                attempt = attempt.saturating_add(1);
                started_at = Some(timestamp.clone());
                completed_at = None;
                ledger_events.push((
                    "node_started".into(),
                    entry.key.clone(),
                    format!("开始执行：{}", entry.content),
                ));
            }
            if status == TaskNodeStatus::Success && previous_status != Some(TaskNodeStatus::Success)
            {
                completed_at = Some(timestamp.clone());
                ledger_events.push((
                    "node_completed".into(),
                    entry.key.clone(),
                    format!("执行完成：{}", entry.content),
                ));
            }
            TaskNode {
                id: previous
                    .as_ref()
                    .map(|node| node.id.clone())
                    .unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
                plan_key: entry.key,
                content: entry.content,
                dependencies: entry.dependencies,
                related_files: entry.related_files,
                read_set: entry.read_set,
                write_set: entry.write_set,
                consumes: entry.consumes,
                produces: entry.produces,
                acceptance_criteria: entry.acceptance_criteria,
                verification_commands: entry.verification_commands,
                status,
                priority: entry.priority,
                attempt,
                started_at,
                completed_at,
                failure: None,
            }
        })
        .collect();
    task.plan_revision = Some(revision.clone());
    task.plan_updated_at = Some(timestamp);
    save(root, &task)?;

    if previous_revision.as_deref() != Some(revision.as_str()) {
        let _ = append_ledger(
            root,
            task_id,
            "plan_synchronized",
            None,
            format!("已同步 {} 个执行节点", task.task_nodes.len()),
            Some(revision.clone()),
        );
    }
    for (kind, key, message) in ledger_events {
        let _ = append_ledger(
            root,
            task_id,
            &kind,
            Some(key),
            message,
            Some(revision.clone()),
        );
    }
    Ok(load(root, task_id).unwrap_or(task))
}

pub fn sync_runtime_plan(
    root: &Path,
    task_id: &str,
    entries: Vec<RuntimePlanEntry>,
) -> Result<CodingTask, String> {
    store::with_task_transaction(root, task_id, || {
        sync_runtime_plan_unlocked(root, task_id, entries)
    })
}

pub fn delete_task(root: &Path, task_id: &str) -> Result<(), String> {
    store::with_task_transaction(root, task_id, || {
        let task = load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
        if matches!(
            task.phase,
            TaskPhase::Discovering
                | TaskPhase::Implementing
                | TaskPhase::Verifying
                | TaskPhase::Diagnosing
                | TaskPhase::Repairing
        ) {
            return Err("任务正在执行或验证，请先停止任务再删除".into());
        }
        let dir = store::task_dir(root, task_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|error| format!("删除任务目录失败：{error}"))?;
        }
        store::update_json(&store::tasks_index_path(root), |index: &mut TaskIndex| {
            index.tasks.retain(|entry| entry.id != task_id);
            Ok(())
        })
    })
}

pub fn rename_task(root: &Path, task_id: &str, name: &str) -> Result<CodingTask, String> {
    store::with_task_transaction(root, task_id, || {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("任务名称不能为空".into());
        }
        let mut task = load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
        task.name = trimmed.to_string();
        save(root, &task)?;
        Ok(load(root, task_id).unwrap_or(task))
    })
}

pub fn confirm_acceptance(
    root: &Path,
    task_id: &str,
    criterion_id: &str,
) -> Result<CodingTask, String> {
    store::with_task_transaction(root, task_id, || {
        let mut task = load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
        if task.phase != TaskPhase::Delivered {
            return Err("只有已完成验证的任务可以人工确认验收标准".into());
        }
        let criterion = task
            .acceptance_criteria
            .iter_mut()
            .find(|criterion| criterion.id == criterion_id)
            .ok_or_else(|| "验收标准不存在".to_string())?;
        criterion.satisfied = true;
        let content_revision = changeset::load(root, task_id).content_revision();
        criterion.evidence = vec![format!(
            "用户于 {} 在交付报告中确认已满足（内容版本 {content_revision}）",
            chrono::Utc::now().to_rfc3339(),
        )];
        save(root, &task)?;
        Ok(load(root, task_id).unwrap_or(task))
    })
}

#[tauri::command]
pub async fn coding_task_for_session(
    access: State<'_, FilesystemAccess>,
    root: String,
    session_id: String,
) -> Result<Option<CodingTask>, String> {
    let root = access.require_workspace(&root)?;
    if session_id.trim().is_empty() {
        return Ok(None);
    }
    tokio::task::spawn_blocking(move || {
        let mut matches = list_tasks(&root)
            .into_iter()
            .filter_map(|summary| load(&root, &summary.id))
            .filter(|task| task.session_id.as_deref() == Some(session_id.as_str()))
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        matches.into_iter().next()
    })
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn coding_task_list(
    access: State<'_, FilesystemAccess>,
    root: String,
) -> Result<Vec<TaskSummary>, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || list_tasks(&root))
        .await
        .map_err(|error| format!("读取任务列表失败：{error}"))
}

#[tauri::command]
pub async fn coding_task_create(
    access: State<'_, FilesystemAccess>,
    root: String,
    name: String,
    requirement: String,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || create_task(&root, &name, &requirement))
        .await
        .map_err(|error| format!("创建任务失败：{error}"))?
}

#[tauri::command]
pub async fn coding_task_get(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<Option<CodingTask>, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || load(&root, &task_id))
        .await
        .map_err(|error| format!("读取任务失败：{error}"))
}

#[tauri::command]
pub async fn coding_task_delete(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<(), String> {
    let root = access.require_workspace(&root)?;
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
    tokio::task::spawn_blocking(move || delete_task(&root, &task_id))
        .await
        .map_err(|error| format!("删除任务失败：{error}"))?
}

#[tauri::command]
pub async fn coding_task_rename(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    name: String,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || rename_task(&root, &task_id, &name))
        .await
        .map_err(|error| format!("重命名任务失败：{error}"))?
}

#[tauri::command]
pub async fn coding_task_bind_runtime(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    session_id: String,
    model_id: String,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || bind_runtime(&root, &task_id, &session_id, &model_id))
        .await
        .map_err(|error| format!("绑定任务会话失败：{error}"))?
}

#[tauri::command]
pub async fn coding_task_set_context(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    paths: Vec<String>,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || set_context_paths(&root, &task_id, paths))
        .await
        .map_err(|error| format!("保存任务上下文失败：{error}"))?
}

#[tauri::command]
pub async fn coding_task_confirm_acceptance(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    criterion_id: String,
) -> Result<CodingTask, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || confirm_acceptance(&root, &task_id, criterion_id.trim()))
        .await
        .map_err(|error| format!("确认验收标准失败：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn temp_root() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("coding-task-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_multiple_tasks_per_workspace() {
        let root = temp_root();
        let first = create_task(&root, "重构登录", "把登录改成 OIDC").unwrap();
        let second = create_task(&root, "修复导出", "导出乱码").unwrap();
        assert_ne!(first.id, second.id);
        let list = list_tasks(&root);
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|task| task.name == "重构登录"));
        assert!(list.iter().any(|task| task.name == "修复导出"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn context_paths_are_task_scoped_deduplicated_and_workspace_relative() {
        let root = temp_root();
        let task = create_task(&root, "修复登录", "修复登录流程").unwrap();
        let updated = set_context_paths(
            &root,
            &task.id,
            vec!["./src/auth.ts".into(), "src/auth.ts".into(), "docs".into()],
        )
        .unwrap();
        assert_eq!(updated.context_paths, vec!["src/auth.ts", "docs"]);
        assert!(set_context_paths(&root, &task.id, vec!["/etc/passwd".into()]).is_err());
        assert!(set_context_paths(&root, &task.id, vec!["../outside".into()]).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn concurrent_task_creation_does_not_lose_index_rows() {
        let root = temp_root();
        let barrier = Arc::new(Barrier::new(9));
        let mut threads = Vec::new();
        for index in 0..8 {
            let root = root.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                create_task(&root, &format!("task-{index}"), "requirement").unwrap();
            }));
        }
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(list_tasks(&root).len(), 8);
        std::fs::remove_dir_all(store::workspace_dir(&root)).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn concurrent_task_updates_preserve_independent_fields() {
        let root = temp_root();
        let task = create_task(&root, "old-name", "requirement").unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let rename_root = root.clone();
        let rename_id = task.id.clone();
        let rename_barrier = barrier.clone();
        let rename = std::thread::spawn(move || {
            rename_barrier.wait();
            rename_task(&rename_root, &rename_id, "new-name").unwrap();
        });
        let bind_root = root.clone();
        let bind_id = task.id.clone();
        let bind_barrier = barrier.clone();
        let bind = std::thread::spawn(move || {
            bind_barrier.wait();
            bind_runtime(&bind_root, &bind_id, "session-1", "model-1").unwrap();
        });
        barrier.wait();
        rename.join().unwrap();
        bind.join().unwrap();

        let reloaded = load(&root, &task.id).unwrap();
        assert_eq!(reloaded.name, "new-name");
        assert_eq!(reloaded.session_id.as_deref(), Some("session-1"));
        assert_eq!(reloaded.model_id.as_deref(), Some("model-1"));
        std::fs::remove_dir_all(store::workspace_dir(&root)).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn task_survives_reload_with_phase_and_criteria() {
        let root = temp_root();
        let mut task = create_task(&root, "重构登录", "把登录改成 OIDC").unwrap();
        task.phase = TaskPhase::Verifying;
        task.acceptance_criteria = vec![AcceptanceCriterion {
            id: "ac1".into(),
            content: "登录流程可用".into(),
            satisfied: false,
            evidence: Vec::new(),
        }];
        save(&root, &task).unwrap();
        let reloaded = load(&root, &task.id).unwrap();
        assert_eq!(reloaded.phase, TaskPhase::Verifying);
        assert_eq!(reloaded.acceptance_criteria.len(), 1);
        assert_eq!(reloaded.acceptance_criteria[0].content, "登录流程可用");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn legacy_task_is_read_compatibly_without_erasing_history() {
        let root = temp_root();
        let task = create_task(&root, "旧任务", "完成修改").unwrap();
        let mut value = serde_json::to_value(&task).unwrap();
        value["schemaVersion"] = serde_json::json!(1);
        value["legacyCustomField"] = serde_json::json!({"keep":"original unknown data"});
        value.as_object_mut().unwrap().remove("strictWorkflow");
        value["taskNodes"][0]
            .as_object_mut()
            .unwrap()
            .remove("planKey");
        store::write_json(&task_path(&root, &task.id), &value).unwrap();
        let upgraded = load(&root, &task.id).unwrap();
        assert_eq!(upgraded.schema_version, 2);
        assert!(!upgraded.strict_workflow);
        assert_eq!(upgraded.task_nodes[0].plan_key, "ROOT");
        assert_eq!(upgraded.requirement, task.requirement);
        assert_eq!(list_tasks(&root).len(), 1);
        // Reads do not rewrite user data. The normal save persists migration.
        assert_eq!(
            store::read_json::<serde_json::Value>(&task_path(&root, &task.id)).unwrap()
                ["schemaVersion"],
            1
        );
        save(&root, &upgraded).unwrap();
        let backup = store::read_json::<serde_json::Value>(
            &task_path(&root, &task.id).with_file_name("task.legacy.json"),
        )
        .unwrap();
        assert_eq!(backup["legacyCustomField"]["keep"], "original unknown data");
        let again = load(&root, &task.id).unwrap();
        assert_eq!(again.task_nodes[0].id, task.task_nodes[0].id);
        assert_eq!(again.created_at, task.created_at);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn valid_plan_persists_dag_acceptance_and_ledger() {
        let root = temp_root();
        let mut task = create_task(&root, "订单", "实现订单流程").unwrap();
        task.acceptance_criteria.push(AcceptanceCriterion {
            id: "followup-1".into(),
            content: "同时记录审计事件".into(),
            satisfied: false,
            evidence: Vec::new(),
        });
        save(&root, &task).unwrap();
        let updated = sync_runtime_plan(
            &root,
            &task.id,
            vec![
                RuntimePlanEntry {
                    key: "T1".into(),
                    content: "定义订单接口".into(),
                    dependencies: vec![],
                    related_files: vec!["src/order.ts".into()],
                    read_set: vec![],
                    write_set: vec!["src/order.ts".into()],
                    consumes: vec![],
                    produces: vec!["OrderService".into()],
                    acceptance_criteria: vec!["接口可编译".into()],
                    verification_commands: vec!["cargo check".into()],
                    status: TaskNodeStatus::Success,
                    priority: "high".into(),
                },
                RuntimePlanEntry {
                    key: "T2".into(),
                    content: "实现订单 API".into(),
                    dependencies: vec!["T1".into()],
                    related_files: vec!["src/api.rs".into()],
                    read_set: vec!["src/order.ts".into()],
                    write_set: vec!["src/api.rs".into()],
                    consumes: vec!["OrderService".into()],
                    produces: vec!["OrderApi".into()],
                    acceptance_criteria: vec!["API 返回订单".into()],
                    verification_commands: vec!["cargo test order".into()],
                    status: TaskNodeStatus::Running,
                    priority: "medium".into(),
                },
            ],
        )
        .unwrap();
        assert!(updated.plan_issues.is_empty());
        assert!(updated.plan_revision.is_some());
        assert_eq!(updated.task_nodes.len(), 2);
        assert_eq!(updated.acceptance_criteria.len(), 4);
        assert!(updated
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.id == "followup-1"));
        assert_eq!(updated.task_nodes[1].attempt, 1);
        let ledger = execution_ledger(&root, &task.id);
        assert!(ledger.iter().any(|event| event.kind == "plan_synchronized"));
        assert!(ledger.iter().any(|event| {
            event.kind == "node_started" && event.node_key.as_deref() == Some("T2")
        }));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_unordered_nodes_that_write_the_same_file() {
        let root = temp_root();
        let task = create_task(&root, "冲突", "修改共享接口").unwrap();
        let entry = |key: &str| RuntimePlanEntry {
            key: key.into(),
            content: format!("执行 {key}"),
            dependencies: vec![],
            related_files: vec!["src/shared.ts".into()],
            read_set: vec![],
            write_set: vec!["src/shared.ts".into()],
            consumes: vec![],
            produces: vec![],
            acceptance_criteria: vec![],
            verification_commands: vec![],
            status: TaskNodeStatus::Pending,
            priority: "medium".into(),
        };
        let updated = sync_runtime_plan(&root, &task.id, vec![entry("T1"), entry("T2")]).unwrap();
        assert!(updated.plan_revision.is_none());
        assert!(updated
            .plan_issues
            .iter()
            .any(|issue| issue.code == "write_conflict"));
        assert!(execution_ledger(&root, &task.id)
            .iter()
            .any(|event| event.kind == "plan_rejected"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_incomplete_or_unsafe_execution_contracts() {
        let root = temp_root();
        let task = create_task(&root, "不完整计划", "跨模块实现").unwrap();
        let updated = sync_runtime_plan(
            &root,
            &task.id,
            vec![RuntimePlanEntry {
                key: "t1".into(),
                content: "实现模块".into(),
                dependencies: vec![],
                related_files: vec![],
                read_set: vec!["../secret".into()],
                write_set: vec![],
                consumes: vec![],
                produces: vec![],
                acceptance_criteria: vec![],
                verification_commands: vec![],
                status: TaskNodeStatus::Pending,
                priority: "high".into(),
            }],
        )
        .unwrap();
        let codes = updated
            .plan_issues
            .iter()
            .map(|issue| issue.code.as_str())
            .collect::<HashSet<_>>();
        assert!(codes.contains("missing_write_set"));
        assert!(codes.contains("missing_acceptance"));
        assert!(codes.contains("missing_verification"));
        assert!(codes.contains("invalid_file_scope"));
        assert!(updated.plan_revision.is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn directory_write_scope_covers_actual_module_files() {
        let root = temp_root();
        let task = create_task(&root, "模块计划", "实现订单模块").unwrap();
        let updated = sync_runtime_plan(
            &root,
            &task.id,
            vec![RuntimePlanEntry {
                key: "t1".into(),
                content: "实现订单模块".into(),
                dependencies: vec![],
                related_files: vec!["./src/orders/**".into()],
                read_set: vec![],
                write_set: vec!["./src/orders/**".into()],
                consumes: vec![],
                produces: vec!["OrderService".into()],
                acceptance_criteria: vec!["订单模块可用".into()],
                verification_commands: vec!["pnpm test -- orders".into()],
                status: TaskNodeStatus::Pending,
                priority: "high".into(),
            }],
        )
        .unwrap();
        assert!(updated.plan_issues.is_empty());
        assert_eq!(updated.task_nodes[0].plan_key, "T1");
        assert_eq!(updated.task_nodes[0].write_set, ["src/orders/**"]);
        assert!(plan_covers_path(&updated, "src/orders/service.ts"));
        assert!(!plan_covers_path(&updated, "src/payments/service.ts"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_removes_task_from_index_and_disk() {
        let root = temp_root();
        let task = create_task(&root, "重构登录", "需求").unwrap();
        let dir = crate::coding::store::task_dir(&root, &task.id);
        assert!(dir.exists());
        delete_task(&root, &task.id).unwrap();
        assert!(list_tasks(&root).is_empty());
        assert!(!dir.exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_rejects_an_active_task_but_allows_a_stopped_task() {
        let root = temp_root();
        let mut task = create_task(&root, "执行中任务", "需求").unwrap();
        task.phase = TaskPhase::Implementing;
        save(&root, &task).unwrap();
        let error = delete_task(&root, &task.id).unwrap_err();
        assert!(error.contains("请先停止任务再删除"));
        assert!(load(&root, &task.id).is_some());

        task.phase = TaskPhase::Stopped;
        save(&root, &task).unwrap();
        delete_task(&root, &task.id).unwrap();
        assert!(load(&root, &task.id).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rename_keeps_id_and_updates_index() {
        let root = temp_root();
        let task = create_task(&root, "旧名", "需求").unwrap();
        rename_task(&root, &task.id, "新名").unwrap();
        let list = list_tasks(&root);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, task.id);
        assert_eq!(list[0].name, "新名");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn acceptance_confirmation_requires_delivery_and_records_user_evidence() {
        let root = temp_root();
        let mut task = create_task(&root, "验收任务", "流程可用").unwrap();
        assert!(confirm_acceptance(&root, &task.id, "requirement").is_err());
        task.phase = TaskPhase::Delivered;
        save(&root, &task).unwrap();

        let confirmed = confirm_acceptance(&root, &task.id, "requirement").unwrap();
        let criterion = &confirmed.acceptance_criteria[0];
        assert!(criterion.satisfied);
        assert!(criterion.evidence[0].contains("用户于"));
        std::fs::remove_dir_all(store::workspace_dir(&root)).ok();
        std::fs::remove_dir_all(&root).ok();
    }
}
