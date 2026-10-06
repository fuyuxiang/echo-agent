//! Quality gates, delivery report and Git handoff.
//!
//! Every gate verdict is derived from stored evidence: exit codes, the change
//! set, review marks and acceptance evidence. A gate a project cannot run (no
//! lint script, for instance) reports as not-applicable rather than pretending
//! that a check ran. Automatic and human-reviewed completion remain distinct.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;

use crate::coding::changeset::{self, ChangeSet, FileChange, FileChangeView};
use crate::coding::diagnostics::{self, Problem};
use crate::coding::orchestrator::{self, RepairRound};
use crate::coding::review::{self, ReviewKind, ReviewRecord};
use crate::coding::task::{self, AcceptanceCriterion, CodingTask, TaskPhase};
use crate::coding::tdd;
use crate::coding::verification::{self, VerificationKind, VerificationRecord, VerificationStatus};
use crate::shell_fs::FilesystemAccess;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum GateId {
    Build,
    Test,
    Lint,
    TypeCheck,
    VerificationFreshness,
    DiffReview,
    Acceptance,
    RequirementsReview,
    CodeQualityReview,
    TestFirst,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    Satisfied,
    NotSatisfied,
    /// The project has no such check; nothing was verified and nothing failed.
    NotApplicable,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QualityGate {
    pub id: GateId,
    pub title: String,
    pub status: GateStatus,
    pub summary: String,
    pub evidence: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceEntry {
    pub criterion_id: String,
    pub kind: String,
    pub detail: String,
    pub source: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryReport {
    pub task: CodingTask,
    pub gates: Vec<QualityGate>,
    pub changes: Vec<FileChangeView>,
    pub total_added: u32,
    pub total_removed: u32,
    pub verifications: Vec<VerificationRecord>,
    pub problems: Vec<Problem>,
    pub repair_rounds: Vec<RepairRound>,
    pub evidence: Vec<EvidenceEntry>,
    pub blockers: Vec<String>,
    /// True only when no gate is NotSatisfied.
    pub deliverable: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CommitHunk {
    pub id: String,
    pub preview: String,
}

fn task_commit_hunks(root: &Path, task_id: &str, path: &str) -> Result<Vec<CommitHunk>, String> {
    if path
        .chars()
        .any(|character| matches!(character, '\n' | '\r' | '\t' | '\\'))
    {
        return Err("文件名包含无法安全表示为 Git 补丁的字符".into());
    }
    let set = changeset::load(root, task_id);
    let change = set
        .changes
        .iter()
        .find(|change| change.path == path && change.pre_existing)
        .ok_or_else(|| "只为任务开始前已有改动的文件提供差异块选择".to_string())?;
    if matches!(change.kind, changeset::ChangeKind::Deleted) {
        return Err("起始时已有改动的已删除文件暂不支持差异块提交".into());
    }
    let diff = changeset::change_diff(root, task_id, path)?;
    if diff.binary || diff.modified_hash != *set.change_hashes.get(path).unwrap_or(&String::new()) {
        return Err("当前文件不是可审阅的最新文本差异，请刷新变更集".into());
    }
    Ok(hunks_for_text(
        path,
        &diff.modified_hash,
        &diff.original,
        &diff.modified,
    ))
}

fn hunks_for_text(
    path: &str,
    modified_hash: &str,
    original: &str,
    modified: &str,
) -> Vec<CommitHunk> {
    let text_diff = similar::TextDiff::from_lines(original, modified);
    let mut formatter = text_diff.unified_diff();
    formatter.context_radius(3);
    formatter
        .iter_hunks()
        .map(|hunk| {
            let preview = hunk.to_string();
            let mut hasher = Sha256::new();
            hasher.update(path.as_bytes());
            hasher.update(modified_hash.as_bytes());
            hasher.update(preview.as_bytes());
            CommitHunk {
                id: format!("{:x}", hasher.finalize()),
                preview,
            }
        })
        .collect()
}

fn selected_task_content(
    original: &str,
    modified: &str,
    selected: &[String],
    hunks: &[CommitHunk],
) -> Result<String, String> {
    let diff = similar::TextDiff::from_lines(original, modified);
    let lines = original.split_inclusive('\n').collect::<Vec<_>>();
    let selected: std::collections::BTreeSet<&str> = selected.iter().map(String::as_str).collect();
    let mut result = String::new();
    let mut cursor = 0;
    for (index, group) in diff.grouped_ops(3).into_iter().enumerate() {
        let start = group
            .first()
            .ok_or_else(|| "差异块为空".to_string())?
            .old_range()
            .start;
        let end = group
            .last()
            .ok_or_else(|| "差异块为空".to_string())?
            .old_range()
            .end;
        result.push_str(&lines[cursor..start].concat());
        if selected.contains(
            hunks
                .get(index)
                .ok_or_else(|| "差异块版本不一致".to_string())?
                .id
                .as_str(),
        ) {
            for operation in &group {
                for change in diff.iter_changes(operation) {
                    if change.tag() != similar::ChangeTag::Delete {
                        result.push_str(change.value());
                    }
                }
            }
        } else {
            result.push_str(&lines[start..end].concat());
        }
        cursor = end;
    }
    result.push_str(&lines[cursor..].concat());
    Ok(result)
}

fn title_for(id: GateId) -> &'static str {
    match id {
        GateId::Build => "构建通过",
        GateId::Test => "测试通过",
        GateId::Lint => "静态检查通过",
        GateId::TypeCheck => "类型检查通过",
        GateId::VerificationFreshness => "验证对应当前代码",
        GateId::DiffReview => "变更已审阅",
        GateId::Acceptance => "验收标准已核销",
        GateId::RequirementsReview => "需求符合性审查",
        GateId::CodeQualityReview => "代码质量审查",
        GateId::TestFirst => "测试先行证据",
    }
}

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

fn verification_gate(
    id: GateId,
    kind: VerificationKind,
    records: &[VerificationRecord],
) -> QualityGate {
    let effective = latest_per_command(records);
    let relevant: Vec<&&VerificationRecord> = effective
        .iter()
        .filter(|record| record.kind == kind)
        .collect();

    if relevant.is_empty() {
        return QualityGate {
            id,
            title: title_for(id).into(),
            status: GateStatus::NotApplicable,
            summary: "当前工程未识别到该类检查命令".into(),
            evidence: Vec::new(),
        };
    }
    let failing_count = relevant
        .iter()
        .filter(|record| record.status != VerificationStatus::Passed)
        .count();
    let evidence: Vec<String> = relevant
        .iter()
        .map(|record| {
            let verdict = match record.status {
                VerificationStatus::Passed => "通过",
                VerificationStatus::Failed => "失败",
                VerificationStatus::TimedOut => "超时",
                VerificationStatus::Cancelled => "已取消",
                VerificationStatus::Running => "执行中",
                VerificationStatus::EnvironmentUnavailable => "环境未就绪",
            };
            let detail = record
                .test_summary
                .map(|summary| {
                    format!(
                        "，{} 通过 / {} 失败 / {} 跳过",
                        summary.passed, summary.failed, summary.skipped
                    )
                })
                .unwrap_or_default();
            format!(
                "{}：{verdict}（退出码 {}）{detail}",
                record.command,
                record
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "无".into())
            )
        })
        .collect();

    QualityGate {
        id,
        title: title_for(id).into(),
        status: if failing_count == 0 {
            GateStatus::Satisfied
        } else {
            GateStatus::NotSatisfied
        },
        summary: if failing_count == 0 {
            format!("{} 项检查全部通过", relevant.len())
        } else {
            format!("{failing_count} 项检查未通过")
        },
        evidence,
    }
}

pub fn evaluate_gates(
    records: &[VerificationRecord],
    set: &ChangeSet,
    criteria: &[AcceptanceCriterion],
    problems: &[Problem],
) -> Vec<QualityGate> {
    let mut gates = vec![
        verification_gate(GateId::Build, VerificationKind::Build, records),
        verification_gate(GateId::Test, VerificationKind::Test, records),
        verification_gate(GateId::Lint, VerificationKind::Lint, records),
        verification_gate(GateId::TypeCheck, VerificationKind::TypeCheck, records),
    ];

    let verification_current = set.verification_is_current();
    gates.push(QualityGate {
        id: GateId::VerificationFreshness,
        title: title_for(GateId::VerificationFreshness).into(),
        status: if verification_current {
            GateStatus::Satisfied
        } else {
            GateStatus::NotSatisfied
        },
        summary: if verification_current {
            "最近一次验证与当前文件内容一致".into()
        } else {
            "文件在最近一次验证后发生变化，请重新运行验证".into()
        },
        evidence: Vec::new(),
    });

    let task_changes: Vec<&FileChange> = set.changes.iter().collect();
    let reviewed_count = task_changes
        .iter()
        .filter(|change| {
            set.reviewed_hashes.get(&change.path) == set.change_hashes.get(&change.path)
        })
        .count();
    gates.push(QualityGate {
        id: GateId::DiffReview,
        title: title_for(GateId::DiffReview).into(),
        status: if task_changes.is_empty() {
            GateStatus::NotApplicable
        } else if reviewed_count == task_changes.len() {
            GateStatus::Satisfied
        } else {
            GateStatus::NotSatisfied
        },
        summary: if task_changes.is_empty() {
            "本任务没有产生代码变更".into()
        } else if reviewed_count == task_changes.len() {
            format!("{} 个变更文件均已审阅且内容未再变化", task_changes.len())
        } else {
            format!(
                "已审阅 {reviewed_count}/{} 个变更文件；请逐个打开剩余差异",
                task_changes.len()
            )
        },
        evidence: task_changes
            .iter()
            .map(|change| format!("{} (+{} -{})", change.path, change.added, change.removed))
            .collect(),
    });

    let unmet_count = criteria
        .iter()
        .filter(|criterion| !criterion.satisfied || criterion.evidence.is_empty())
        .count();
    gates.push(QualityGate {
        id: GateId::Acceptance,
        title: title_for(GateId::Acceptance).into(),
        status: if criteria.is_empty() || unmet_count > 0 {
            GateStatus::NotSatisfied
        } else {
            GateStatus::Satisfied
        },
        summary: if criteria.is_empty() {
            "尚未生成验收标准".into()
        } else if unmet_count == 0 {
            format!("{} 条验收标准均有验证证据", criteria.len())
        } else {
            format!("还有 {unmet_count} 条验收标准缺少验证证据")
        },
        evidence: criteria
            .iter()
            .map(|criterion| {
                format!(
                    "{}：{}",
                    criterion.content,
                    if criterion.evidence.is_empty() {
                        "无证据".to_string()
                    } else {
                        criterion.evidence.join("；")
                    }
                )
            })
            .collect(),
    });

    if !problems.is_empty() {
        if let Some(gate) = gates.iter_mut().find(|gate| gate.id == GateId::Test) {
            gate.evidence
                .push(format!("诊断中心仍有 {} 个未解决问题", problems.len()));
        }
    }
    gates
}

pub fn is_deliverable(gates: &[QualityGate]) -> bool {
    gates
        .iter()
        .all(|gate| gate.status != GateStatus::NotSatisfied)
}

pub fn committable_paths(set: &ChangeSet) -> Vec<String> {
    set.changes
        .iter()
        .filter(|change| !change.pre_existing)
        .map(|change| change.path.clone())
        .collect()
}

/// Structured input a model turns into a commit message. It describes the full
/// task change set; the commit command separately refuses dirty-at-start files.
pub fn commit_message_input(
    name: &str,
    requirement: &str,
    set: &ChangeSet,
    records: &[VerificationRecord],
) -> String {
    let files = set
        .changes
        .iter()
        .map(|change| {
            format!(
                "- {} ({:?}, +{} -{})",
                change.path, change.kind, change.added, change.removed
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let verifications = latest_per_command(records)
        .iter()
        .map(|record| format!("- {}：{:?}", record.command, record.status))
        .collect::<Vec<_>>()
        .join("\n");
    [
        format!("任务名称：{name}"),
        format!("原始需求：{requirement}"),
        "变更文件：".to_string(),
        if files.is_empty() {
            "- 无".into()
        } else {
            files
        },
        "验证结果：".to_string(),
        if verifications.is_empty() {
            "- 未执行".into()
        } else {
            verifications
        },
    ]
    .join("\n")
}

pub fn build_report(root: &Path, task_id: &str) -> Result<DeliveryReport, String> {
    let task = task::load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
    let set = changeset::load(root, task_id);
    let records = verification::list_records(root, task_id);
    let problems = diagnostics::load_snapshot(root, task_id);
    let mut gates = evaluate_gates(&records, &set, &task.acceptance_criteria, &problems);
    let tdd_evidence = tdd::load(root, task_id);
    let tdd_required = tdd::needs_test_first(root, &task, &set);
    let green = tdd::green_record(&tdd_evidence, &records, &set)
        .filter(|_| set.verified_revision.as_deref() == Some(set.content_revision().as_str()));
    gates.push(QualityGate {
        id: GateId::TestFirst,
        title: title_for(GateId::TestFirst).into(),
        status: if !tdd_required || tdd_evidence.waiver_reason.is_some() {
            GateStatus::NotApplicable
        } else if green.is_some() {
            GateStatus::Satisfied
        } else {
            GateStatus::NotSatisfied
        },
        summary: if !tdd_required {
            "当前任务没有适用的测试先行检查".into()
        } else if let Some(reason) = &tdd_evidence.waiver_reason {
            format!("已说明豁免：{reason}")
        } else if green.is_some() {
            "RED 失败与当前版本 GREEN 通过均有原生执行记录".into()
        } else if tdd_evidence.red_record_id.is_some() {
            "已记录 RED；等待当前实现版本的同命令 GREEN 通过".into()
        } else {
            "尚无测试文件先行变更后的真实 RED 失败记录".into()
        },
        evidence: green
            .map(|record| {
                vec![
                    format!(
                        "RED：{}，版本 {}",
                        tdd_evidence.red_record_id.as_deref().unwrap_or_default(),
                        tdd_evidence.red_revision.as_deref().unwrap_or_default()
                    ),
                    format!(
                        "GREEN：{}，退出码 {}",
                        record.id,
                        record.exit_code.unwrap_or_default()
                    ),
                ]
            })
            .unwrap_or_default(),
    });
    let reviews = review::list(root, task_id);
    for (kind, id) in [
        (ReviewKind::Requirements, GateId::RequirementsReview),
        (ReviewKind::CodeQuality, GateId::CodeQualityReview),
    ] {
        let current = current_review(&reviews, kind, &set);
        gates.push(QualityGate {
            id,
            title: title_for(id).into(),
            status: if set.changes.is_empty() {
                GateStatus::NotApplicable
            } else if current.is_some() {
                GateStatus::Satisfied
            } else {
                GateStatus::NotSatisfied
            },
            summary: if set.changes.is_empty() {
                "只读任务没有文件差异".into()
            } else if let Some(record) = current {
                format!("已于 {} 确认，绑定当前代码版本", record.confirmed_at)
            } else {
                "尚未对当前代码版本完成确认".into()
            },
            evidence: current
                .map(|record| vec![format!("版本 {}", record.content_revision)])
                .unwrap_or_default(),
        });
    }
    let evidence = task
        .acceptance_criteria
        .iter()
        .flat_map(|criterion| {
            criterion
                .evidence
                .iter()
                .map(|detail| EvidenceEntry {
                    criterion_id: criterion.id.clone(),
                    kind: "acceptance".into(),
                    detail: detail.clone(),
                    source: if detail.contains("用户已确认") || detail.contains("逐一审阅")
                    {
                        "human_review".into()
                    } else {
                        "verification".into()
                    },
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let blockers = gates
        .iter()
        .filter(|gate| gate.status == GateStatus::NotSatisfied)
        .map(|gate| format!("{}：{}", gate.title, gate.summary))
        .collect();
    Ok(DeliveryReport {
        total_added: set.total_added(),
        total_removed: set.total_removed(),
        changes: set.changes.iter().map(FileChangeView::from).collect(),
        deliverable: is_deliverable(&gates),
        gates,
        verifications: records,
        problems,
        repair_rounds: orchestrator::repair_rounds(root, task_id),
        evidence,
        blockers,
        task,
    })
}

fn current_review<'a>(
    records: &'a [ReviewRecord],
    kind: ReviewKind,
    set: &ChangeSet,
) -> Option<&'a ReviewRecord> {
    let revision = set.content_revision();
    records
        .iter()
        .find(|record| record.kind == kind && record.content_revision == revision)
}

async fn git(root: &PathBuf, arguments: &[&str]) -> Result<String, String> {
    let output = crate::process_supervisor::background_async_command("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .await
        .map_err(|error| format!("执行 git 失败：{error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn git_with_index(
    root: &PathBuf,
    index: &Path,
    arguments: &[&str],
) -> Result<String, String> {
    let output = crate::process_supervisor::background_async_command("git")
        .args(arguments)
        .env("GIT_INDEX_FILE", index)
        .current_dir(root)
        .output()
        .await
        .map_err(|error| format!("执行 git 失败：{error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn stage_task_hunks(
    root: &PathBuf,
    index: &Path,
    baseline_head: &str,
    path: &str,
    selected_content: &str,
    original_content: &str,
) -> Result<(), String> {
    let entry = git_bytes(root, &["ls-tree", "-z", baseline_head, "--", path]).await?;
    let header = entry
        .split(|byte| *byte == b'\t')
        .next()
        .unwrap_or_default();
    let mode = std::str::from_utf8(header)
        .ok()
        .and_then(|value| value.split_whitespace().next())
        .filter(|value| matches!(*value, "100644" | "100755"))
        .ok_or_else(|| format!("{path} 在任务 Git 基线中不是普通文本文件，无法分离原有改动"))?;
    let current_head = git_bytes(root, &["show", &format!("{baseline_head}:{path}")]).await?;
    std::str::from_utf8(&current_head).map_err(|_| format!("{path} 的 Git 基线不是文本文件"))?;
    let temp = tempfile::tempdir().map_err(|error| format!("创建差异块合并目录失败：{error}"))?;
    let ours = temp.path().join("head");
    let ancestor = temp.path().join("task-start");
    let theirs = temp.path().join("selected-task");
    std::fs::write(&ours, current_head).map_err(|error| format!("写入 Git 基线失败：{error}"))?;
    std::fs::write(&ancestor, original_content)
        .map_err(|error| format!("写入任务起点失败：{error}"))?;
    std::fs::write(&theirs, selected_content)
        .map_err(|error| format!("写入所选差异块失败：{error}"))?;
    let merged = git(
        root,
        &[
            "merge-file",
            "-p",
            &ours.to_string_lossy(),
            &ancestor.to_string_lossy(),
            &theirs.to_string_lossy(),
        ],
    )
    .await
    .map_err(|error| format!("{path} 的所选差异块与起始前改动冲突，原工作区未修改：{error}"))?;
    let merged_path = temp.path().join("merged");
    std::fs::write(&merged_path, merged.as_bytes())
        .map_err(|error| format!("写入合并结果失败：{error}"))?;
    let blob = git(root, &["hash-object", "-w", &merged_path.to_string_lossy()]).await?;
    git_with_index(
        root,
        index,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("{mode},{},{}", blob.trim(), path),
        ],
    )
    .await?;
    Ok(())
}

#[tauri::command]
pub async fn coding_delivery_commit_hunks(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    path: String,
) -> Result<Vec<CommitHunk>, String> {
    let root = access.require_workspace(&root)?;
    changeset::sync_changes(&root, &task_id).await?;
    tokio::task::spawn_blocking(move || task_commit_hunks(&root, &task_id, &path))
        .await
        .map_err(|error| format!("读取差异块失败：{error}"))?
}

async fn git_bytes(root: &Path, arguments: &[&str]) -> Result<Vec<u8>, String> {
    let output = crate::process_supervisor::background_async_command("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .await
        .map_err(|error| format!("执行 git 失败：{error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(output.stdout)
}

async fn tree_content_hash(root: &Path, tree: &str, path: &str) -> Result<String, String> {
    let entry = git_bytes(root, &["ls-tree", "-z", tree, "--", path]).await?;
    if entry.is_empty() {
        return Ok("<deleted>".into());
    }
    let header = entry
        .split(|byte| *byte == b'\t')
        .next()
        .ok_or_else(|| format!("无法读取 {path} 的暂存对象"))?;
    let header = std::str::from_utf8(header).map_err(|_| format!("{path} 的 Git tree 记录无效"))?;
    let mut fields = header.split_whitespace();
    let _mode = fields.next();
    let kind = fields.next();
    let object = fields.next();
    if kind != Some("blob") {
        return Ok("<directory>".into());
    }
    let object = object.ok_or_else(|| format!("{path} 缺少 Git blob 标识"))?;
    let bytes = git_bytes(root, &["cat-file", "blob", object]).await?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Validate the immutable tree that will be committed, rather than the live
/// worktree or mutable index. Once this succeeds, later editor/index changes
/// cannot alter the commit contents.
async fn ensure_commit_tree_matches(
    root: &PathBuf,
    tree: &str,
    set: &ChangeSet,
    task_paths: &[String],
    full_content_paths: &[String],
) -> Result<(), String> {
    for path in full_content_paths {
        let expected = set
            .change_hashes
            .get(path)
            .ok_or_else(|| format!("{path} 缺少交付内容版本"))?;
        let actual = tree_content_hash(root, tree, path).await?;
        if &actual != expected {
            return Err(format!("{path} 的暂存内容与已验证版本不一致，请刷新后重试"));
        }
    }
    let baseline = set
        .baseline_head
        .as_deref()
        .ok_or_else(|| "任务缺少 Git 基线提交".to_string())?;
    let changed = git(root, &["diff", "--name-only", "-z", baseline, tree, "--"]).await?;
    let unexpected = unexpected_staged_paths(&changed, task_paths);
    if !unexpected.is_empty() {
        return Err(format!(
            "待提交 tree 包含当前任务之外的变更：{}",
            unexpected.join("、")
        ));
    }
    Ok(())
}

fn unexpected_staged_paths(staged: &str, allowed: &[String]) -> Vec<String> {
    let allowed: std::collections::BTreeSet<&str> = allowed.iter().map(String::as_str).collect();
    let mut unexpected: Vec<String> = staged
        .split('\0')
        .filter(|path| !path.is_empty() && !allowed.contains(path))
        .map(str::to_owned)
        .collect();
    unexpected.sort();
    unexpected.dedup();
    unexpected
}

async fn ensure_selected_paths_unstaged(
    root: &PathBuf,
    task_paths: &[String],
) -> Result<(), String> {
    let staged = git(root, &["diff", "--cached", "--name-only", "-z", "--"]).await?;
    let selected: std::collections::BTreeSet<&str> =
        task_paths.iter().map(String::as_str).collect();
    let overlapping: Vec<&str> = staged
        .split('\0')
        .filter(|path| selected.contains(path))
        .collect();
    if overlapping.is_empty() {
        return Ok(());
    }
    Err(format!(
        "所选文件已有手动暂存内容，请先检查并取消这些文件的暂存后重试：{}",
        overlapping.join("、")
    ))
}

#[tauri::command]
pub async fn coding_delivery_report(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<DeliveryReport, String> {
    let root = access.require_workspace(&root)?;
    changeset::sync_changes(&root, &task_id).await?;
    tokio::task::spawn_blocking(move || build_report(&root, &task_id))
        .await
        .map_err(|error| format!("生成交付报告失败：{error}"))?
}

#[tauri::command]
pub async fn coding_delivery_commit_input(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<String, String> {
    let root = access.require_workspace(&root)?;
    changeset::sync_changes(&root, &task_id).await?;
    tokio::task::spawn_blocking(move || {
        let task = task::load(&root, &task_id).ok_or_else(|| "任务不存在".to_string())?;
        Ok(commit_message_input(
            &task.name,
            &task.requirement,
            &changeset::load(&root, &task_id),
            &verification::list_records(&root, &task_id),
        ))
    })
    .await
    .map_err(|error| format!("生成提交信息输入失败：{error}"))?
}

#[tauri::command]
pub async fn coding_delivery_pr_input(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<DeliveryReport, String> {
    // A PR description needs exactly the report's content; reuse it rather than
    // maintaining a second aggregation path.
    coding_delivery_report(access, root, task_id).await
}

/// Stage only this task's files and commit them. Never `git add -A`, so a
/// user's unrelated uncommitted work is never swept into the task's commit.
#[tauri::command]
pub async fn coding_git_commit(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    message: String,
    selected_paths: Option<Vec<String>>,
    selected_hunks: Option<BTreeMap<String, Vec<String>>>,
) -> Result<String, String> {
    let root = access.require_workspace(&root)?;
    let trimmed = message.trim().to_string();
    if trimmed.is_empty() {
        return Err("提交信息不能为空".into());
    }
    // Revalidate review hashes immediately before staging. A file changed in
    // an external editor after delivery must return the task to review instead
    // of being silently swept into the commit.
    changeset::sync_changes(&root, &task_id).await?;
    let set = {
        let root = root.clone();
        let task_id = task_id.clone();
        tokio::task::spawn_blocking(move || changeset::load(&root, &task_id))
            .await
            .map_err(|error| format!("读取变更集失败：{error}"))?
    };
    if set.committed_hash.is_some() {
        return Err("该任务已经提交".into());
    }
    if set.effective_baseline_mode() == changeset::BaselineMode::Filesystem {
        return Err(
            "当前任务使用本地检查点，不支持应用内 Git 提交；可手工提交，或初始化 Git 并新建任务后使用提交功能".into(),
        );
    }
    changeset::ensure_head_unchanged(&root, &set)?;
    changeset::ensure_changes_current(&root, &set)?;
    let report = {
        let report_root = root.clone();
        let report_task_id = task_id.clone();
        tokio::task::spawn_blocking(move || build_report(&report_root, &report_task_id))
            .await
            .map_err(|error| format!("读取交付门禁失败：{error}"))??
    };
    if report.task.phase != TaskPhase::Delivered || !report.deliverable {
        return Err("任务尚未通过交付门禁，不能提交".into());
    }
    let safe_paths = committable_paths(&set);
    let paths = selected_paths.unwrap_or_else(|| safe_paths.clone());
    let hunk_selection = selected_hunks.unwrap_or_default();
    if paths.is_empty() && hunk_selection.is_empty() {
        return Err("本任务没有可提交的变更".into());
    }
    let selected: std::collections::BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    if selected.len() != paths.len() || paths.iter().any(|path| !safe_paths.contains(path)) {
        return Err("所选文件不属于当前任务的安全可提交变更，请刷新后重试".into());
    }
    for path in &paths {
        // Validate path components rather than rejecting harmless names such
        // as `range..test.ts`. `git add --` below then treats leading dashes as
        // paths too, never command options.
        changeset::resolve_in_workspace(&root, path)?;
    }
    let mut hunk_merges = Vec::new();
    for (path, ids) in &hunk_selection {
        if ids.is_empty() || paths.contains(path) || !set.is_pre_existing(path) {
            return Err("差异块选择不属于当前任务的起始已修改文件".into());
        }
        changeset::resolve_in_workspace(&root, path)?;
        let available = task_commit_hunks(&root, &task_id, path)?;
        let selected_ids: std::collections::BTreeSet<_> = ids.iter().collect();
        if selected_ids.len() != ids.len()
            || ids
                .iter()
                .any(|id| !available.iter().any(|hunk| &hunk.id == id))
        {
            return Err(format!("{path} 的差异块已经变化，请重新选择"));
        }
        let diff = changeset::change_diff(&root, &task_id, path)?;
        let selected_content =
            selected_task_content(&diff.original, &diff.modified, ids, &available)?;
        hunk_merges.push((path.clone(), diff.original, selected_content));
    }
    let all_paths = paths
        .iter()
        .chain(hunk_selection.keys())
        .cloned()
        .collect::<Vec<_>>();
    ensure_selected_paths_unstaged(&root, &all_paths).await?;
    // Build the commit from HEAD in a private index. The user's staged files
    // stay untouched, even when they share the same repository.
    let index_dir = tempfile::tempdir().map_err(|error| format!("创建隔离暂存区失败：{error}"))?;
    let index_path = index_dir.path().join("index");
    let baseline_head = set
        .baseline_head
        .as_deref()
        .ok_or_else(|| "任务缺少 Git 基线提交".to_string())?;
    git_with_index(&root, &index_path, &["read-tree", baseline_head]).await?;
    if !paths.is_empty() {
        let mut add_arguments: Vec<&str> = vec!["add", "--"];
        add_arguments.extend(paths.iter().map(|path| path.as_str()));
        git_with_index(&root, &index_path, &add_arguments).await?;
    }
    for (path, original, selected_content) in &hunk_merges {
        stage_task_hunks(
            &root,
            &index_path,
            baseline_head,
            path,
            selected_content,
            original,
        )
        .await?;
    }
    // Freeze the current index as an immutable Git tree, then validate the
    // actual blob bytes that will be committed. A same-path restage after this
    // point may change the live index, but can no longer change `tree`.
    let tree = git_with_index(&root, &index_path, &["write-tree"])
        .await?
        .trim()
        .to_string();
    if tree
        == git(&root, &["rev-parse", &format!("{baseline_head}^{{tree}}")])
            .await?
            .trim()
    {
        return Err("所选差异块没有产生可提交的任务变更".into());
    }
    ensure_commit_tree_matches(&root, &tree, &set, &all_paths, &paths).await?;
    let hash = git(
        &root,
        &["commit-tree", &tree, "-p", baseline_head, "-m", &trimmed],
    )
    .await?
    .trim()
    .to_string();
    // Compare-and-swap HEAD: a concurrent commit cannot be overwritten.
    git(
        &root,
        &[
            "update-ref",
            "-m",
            &format!("commit: {trimmed}"),
            "HEAD",
            &hash,
            baseline_head,
        ],
    )
    .await?;
    // HEAD moved, so align only committed paths in the user's real index.
    // Other staged files retain their exact index entries.
    let mut reset_arguments: Vec<&str> = vec!["reset", "-q", "HEAD", "--"];
    reset_arguments.extend(all_paths.iter().map(|path| path.as_str()));
    let index_result = git(&root, &reset_arguments).await;
    let mark_root = root.clone();
    let mark_task = task_id.clone();
    let mark_hash = hash.clone();
    tokio::task::spawn_blocking(move || {
        changeset::mark_committed(&mark_root, &mark_task, &mark_hash, &all_paths)
    })
    .await
    .map_err(|error| format!("已创建提交 {hash}，但记录任务状态失败：{error}"))?
    .map_err(|error| format!("已创建提交 {hash}，但记录任务状态失败：{error}"))?;
    index_result.map_err(|error| format!("已创建提交 {hash}，但同步 Git 暂存区失败：{error}"))?;
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::changeset::ChangeKind;
    use crate::coding::verification::record_from_parts;

    fn record(kind: VerificationKind, command: &str, exit: i32) -> VerificationRecord {
        record_from_parts(
            "task-1",
            kind,
            command,
            Some(exit),
            String::new(),
            String::new(),
            10,
            false,
            false,
        )
    }

    fn change_set(reviewed: bool) -> ChangeSet {
        ChangeSet {
            task_id: "task-1".into(),
            baseline_files: Vec::new(),
            changes: vec![FileChange {
                path: "src/a.ts".into(),
                kind: ChangeKind::Modified,
                added: 10,
                removed: 2,
                baseline_content: Some("old".into()),
                pre_existing: false,
            }],
            created_at: "2026-09-11T00:00:00Z".into(),
            reviewed_files: if reviewed {
                vec!["src/a.ts".to_string()]
            } else {
                Vec::new()
            },
            reviewed_hashes: if reviewed {
                std::collections::BTreeMap::from([("src/a.ts".to_string(), "v1".to_string())])
            } else {
                std::collections::BTreeMap::new()
            },
            change_hashes: std::collections::BTreeMap::from([(
                "src/a.ts".to_string(),
                "v1".to_string(),
            )]),
            ..ChangeSet::default()
        }
    }

    fn criteria(satisfied: bool) -> Vec<AcceptanceCriterion> {
        vec![AcceptanceCriterion {
            id: "ac1".into(),
            content: "登录流程可用".into(),
            satisfied,
            evidence: if satisfied {
                vec!["pnpm test 通过".to_string()]
            } else {
                Vec::new()
            },
        }]
    }

    #[test]
    fn missing_command_kind_is_not_applicable_not_failure() {
        let gates = evaluate_gates(
            &[record(VerificationKind::Test, "pnpm test", 0)],
            &change_set(true),
            &criteria(true),
            &[],
        );
        let lint = gates.iter().find(|gate| gate.id == GateId::Lint).unwrap();
        assert_eq!(lint.status, GateStatus::NotApplicable);
        assert!(is_deliverable(&gates));
    }

    #[test]
    fn failing_test_makes_gate_not_satisfied_and_blocks_delivery() {
        let gates = evaluate_gates(
            &[record(VerificationKind::Test, "pnpm test", 1)],
            &change_set(true),
            &criteria(true),
            &[],
        );
        let test = gates.iter().find(|gate| gate.id == GateId::Test).unwrap();
        assert_eq!(test.status, GateStatus::NotSatisfied);
        assert!(!is_deliverable(&gates));
    }

    #[test]
    fn acceptance_without_evidence_is_not_satisfied() {
        let gates = evaluate_gates(
            &[record(VerificationKind::Test, "pnpm test", 0)],
            &change_set(true),
            &criteria(false),
            &[],
        );
        let acceptance = gates
            .iter()
            .find(|gate| gate.id == GateId::Acceptance)
            .unwrap();
        assert_eq!(acceptance.status, GateStatus::NotSatisfied);
        assert!(!is_deliverable(&gates));
    }

    #[test]
    fn unreviewed_diff_blocks_delivery() {
        let gates = evaluate_gates(
            &[record(VerificationKind::Test, "pnpm test", 0)],
            &change_set(false),
            &criteria(true),
            &[],
        );
        let review = gates
            .iter()
            .find(|gate| gate.id == GateId::DiffReview)
            .unwrap();
        assert_eq!(review.status, GateStatus::NotSatisfied);
        assert!(review.summary.contains("0/1"));
        assert!(!is_deliverable(&gates));
    }

    #[test]
    fn empty_change_set_is_reported_without_manual_review_gate() {
        let empty = ChangeSet {
            task_id: "task-1".into(),
            baseline_files: Vec::new(),
            changes: Vec::new(),
            created_at: "2026-09-11T00:00:00Z".into(),
            reviewed_files: Vec::new(),
            ..ChangeSet::default()
        };
        let gates = evaluate_gates(
            &[record(VerificationKind::Test, "pnpm test", 0)],
            &empty,
            &criteria(true),
            &[],
        );
        let review = gates
            .iter()
            .find(|gate| gate.id == GateId::DiffReview)
            .unwrap();
        assert_eq!(review.status, GateStatus::NotApplicable);
    }

    #[test]
    fn latest_run_of_a_command_decides_its_gate() {
        let gates = evaluate_gates(
            &[
                record(VerificationKind::Build, "pnpm build", 1),
                record(VerificationKind::Build, "pnpm build", 0),
            ],
            &change_set(true),
            &criteria(true),
            &[],
        );
        let build = gates.iter().find(|gate| gate.id == GateId::Build).unwrap();
        assert_eq!(build.status, GateStatus::Satisfied);
    }

    #[test]
    fn new_tasks_require_verification_for_the_current_content_revision() {
        let mut set = change_set(true);
        set.baseline_mode = Some(changeset::BaselineMode::Filesystem);
        let stale = evaluate_gates(&[], &set, &criteria(true), &[]);
        assert_eq!(
            stale
                .iter()
                .find(|gate| gate.id == GateId::VerificationFreshness)
                .unwrap()
                .status,
            GateStatus::NotSatisfied
        );

        set.verified_revision = Some(set.content_revision());
        let current = evaluate_gates(&[], &set, &criteria(true), &[]);
        assert_eq!(
            current
                .iter()
                .find(|gate| gate.id == GateId::VerificationFreshness)
                .unwrap()
                .status,
            GateStatus::Satisfied
        );
    }

    #[test]
    fn unrelated_pre_staged_files_are_detected_before_commit() {
        let allowed = vec!["src/task.ts".to_string(), "src/range..test.ts".to_string()];
        assert_eq!(
            unexpected_staged_paths(
                "src/task.ts\0docs/user-notes.md\0src/range..test.ts\0",
                &allowed,
            ),
            vec!["docs/user-notes.md".to_string()]
        );
    }

    #[test]
    fn all_gate_kinds_are_always_reported() {
        let gates = evaluate_gates(&[], &change_set(true), &criteria(true), &[]);
        for id in [
            GateId::Build,
            GateId::Test,
            GateId::Lint,
            GateId::TypeCheck,
            GateId::VerificationFreshness,
            GateId::DiffReview,
            GateId::Acceptance,
        ] {
            assert!(gates.iter().any(|gate| gate.id == id), "缺少门禁 {id:?}");
        }
    }

    #[test]
    fn automatic_delivery_does_not_fabricate_acceptance_evidence() {
        let gates = evaluate_gates(
            &[record(VerificationKind::Test, "pnpm test", 0)],
            &change_set(false),
            &criteria(false),
            &[],
        );
        assert_eq!(
            gates
                .iter()
                .find(|gate| gate.id == GateId::DiffReview)
                .unwrap()
                .status,
            GateStatus::NotSatisfied
        );
        assert_eq!(
            gates
                .iter()
                .find(|gate| gate.id == GateId::Acceptance)
                .unwrap()
                .status,
            GateStatus::NotSatisfied
        );
        assert!(!is_deliverable(&gates));
    }

    #[test]
    fn commit_message_input_lists_every_reviewed_task_change() {
        let mut set = change_set(true);
        set.changes.push(FileChange {
            path: "src/user-edit.ts".into(),
            kind: ChangeKind::Modified,
            added: 3,
            removed: 1,
            baseline_content: Some("x".into()),
            pre_existing: true,
        });
        let input = commit_message_input("重构登录", "把登录改成 OIDC", &set, &[]);
        assert!(input.contains("src/a.ts"));
        assert!(input.contains("src/user-edit.ts"));
    }

    #[test]
    fn commit_paths_exclude_pre_existing_files() {
        let mut set = change_set(true);
        set.changes.push(FileChange {
            path: "src/user-edit.ts".into(),
            kind: ChangeKind::Modified,
            added: 3,
            removed: 1,
            baseline_content: Some("x".into()),
            pre_existing: true,
        });
        let paths = committable_paths(&set);
        assert_eq!(paths, vec!["src/a.ts".to_string()]);
    }

    #[tokio::test]
    async fn frozen_commit_tree_cannot_be_changed_by_a_later_restage() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "baseline\n").unwrap();
        let run = |arguments: &[&str]| {
            let output = std::process::Command::new("git")
                .args(arguments)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        run(&["init", "--initial-branch=main"]);
        run(&["add", "--", "src/a.ts"]);
        run(&[
            "-c",
            "user.name=Echo Test",
            "-c",
            "user.email=echo@example.invalid",
            "commit",
            "-m",
            "baseline",
        ]);
        let baseline = run(&["rev-parse", "HEAD"]);

        let verified = b"verified\n";
        std::fs::write(root.join("src/a.ts"), verified).unwrap();
        run(&["add", "--", "src/a.ts"]);
        let tree = run(&["write-tree"]);
        let expected = format!("{:x}", sha2::Sha256::digest(verified));
        let set = ChangeSet {
            task_id: "task-1".into(),
            baseline_head: Some(baseline),
            changes: vec![FileChange {
                path: "src/a.ts".into(),
                kind: ChangeKind::Modified,
                added: 1,
                removed: 1,
                baseline_content: Some("baseline\n".into()),
                pre_existing: false,
            }],
            change_hashes: std::collections::BTreeMap::from([(
                "src/a.ts".into(),
                expected.clone(),
            )]),
            ..ChangeSet::default()
        };
        ensure_commit_tree_matches(
            &root,
            &tree,
            &set,
            &["src/a.ts".into()],
            &["src/a.ts".into()],
        )
        .await
        .unwrap();

        // The live index can race after write-tree, but the validated tree is
        // immutable and still contains the exact verified bytes.
        std::fs::write(root.join("src/a.ts"), "raced\n").unwrap();
        run(&["add", "--", "src/a.ts"]);
        assert_eq!(
            tree_content_hash(&root, &tree, "src/a.ts").await.unwrap(),
            expected
        );
        ensure_commit_tree_matches(
            &root,
            &tree,
            &set,
            &["src/a.ts".into()],
            &["src/a.ts".into()],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn private_commit_index_preserves_unrelated_staged_files() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().to_path_buf();
        std::fs::write(root.join("task.txt"), "before\n").unwrap();
        std::fs::write(root.join("personal.txt"), "before\n").unwrap();
        git(&root, &["init", "--initial-branch=main"])
            .await
            .unwrap();
        git(&root, &["add", "--", "task.txt", "personal.txt"])
            .await
            .unwrap();
        git(
            &root,
            &[
                "-c",
                "user.name=Echo Test",
                "-c",
                "user.email=echo@example.invalid",
                "commit",
                "-m",
                "baseline",
            ],
        )
        .await
        .unwrap();
        let baseline = git(&root, &["rev-parse", "HEAD"])
            .await
            .unwrap()
            .trim()
            .to_string();
        std::fs::write(root.join("task.txt"), "task change\n").unwrap();
        std::fs::write(root.join("personal.txt"), "personal change\n").unwrap();
        git(&root, &["add", "--", "personal.txt"]).await.unwrap();
        let selected = vec!["task.txt".to_string()];
        ensure_selected_paths_unstaged(&root, &selected)
            .await
            .unwrap();

        let private = tempfile::tempdir().unwrap();
        let index = private.path().join("index");
        git_with_index(&root, &index, &["read-tree", &baseline])
            .await
            .unwrap();
        git_with_index(&root, &index, &["add", "--", "task.txt"])
            .await
            .unwrap();
        let tree = git_with_index(&root, &index, &["write-tree"])
            .await
            .unwrap()
            .trim()
            .to_string();
        let expected = format!("{:x}", sha2::Sha256::digest(b"task change\n"));
        let set = ChangeSet {
            baseline_head: Some(baseline.clone()),
            change_hashes: std::collections::BTreeMap::from([("task.txt".into(), expected)]),
            ..ChangeSet::default()
        };
        ensure_commit_tree_matches(&root, &tree, &set, &selected, &selected)
            .await
            .unwrap();
        assert_eq!(
            git(&root, &["diff", "--cached", "--name-only"])
                .await
                .unwrap()
                .trim(),
            "personal.txt"
        );
        assert_eq!(
            git(&root, &["diff", "--name-only", &baseline, &tree])
                .await
                .unwrap()
                .trim(),
            "task.txt"
        );
    }

    #[tokio::test]
    async fn selected_hunk_commits_task_delta_without_preexisting_edit() {
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().to_path_buf();
        let head = (1..=12)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        std::fs::write(root.join("source.txt"), &head).unwrap();
        git(&root, &["init", "-q"]).await.unwrap();
        git(&root, &["add", "source.txt"]).await.unwrap();
        git(
            &root,
            &[
                "-c",
                "user.name=Echo Test",
                "-c",
                "user.email=echo@example.test",
                "commit",
                "-qm",
                "baseline",
            ],
        )
        .await
        .unwrap();
        let baseline_head = git(&root, &["rev-parse", "HEAD"])
            .await
            .unwrap()
            .trim()
            .to_string();
        let original = head.replace("line 1\n", "line 1 user\n");
        let modified = original.replace("line 10\n", "line 10 task\n");
        std::fs::write(root.join("source.txt"), &modified).unwrap();
        let hunks = hunks_for_text("source.txt", "current", &original, &modified);
        assert_eq!(hunks.len(), 1);
        let selected =
            selected_task_content(&original, &modified, &[hunks[0].id.clone()], &hunks).unwrap();
        let private = tempfile::tempdir().unwrap();
        let index = private.path().join("index");
        git_with_index(&root, &index, &["read-tree", &baseline_head])
            .await
            .unwrap();
        stage_task_hunks(
            &root,
            &index,
            &baseline_head,
            "source.txt",
            &selected,
            &original,
        )
        .await
        .unwrap();
        let tree = git_with_index(&root, &index, &["write-tree"])
            .await
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(
            git(&root, &["show", &format!("{tree}:source.txt")])
                .await
                .unwrap(),
            head.replace("line 10\n", "line 10 task\n")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("source.txt")).unwrap(),
            modified
        );
    }

    #[test]
    fn hunk_selection_keeps_unselected_task_changes_out() {
        let original = (1..=15)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let modified = original
            .replace("line 2\n", "line 2 task\n")
            .replace("line 14\n", "line 14 task\n");
        let hunks = hunks_for_text("source.txt", "current", &original, &modified);
        assert_eq!(hunks.len(), 2);
        let selected =
            selected_task_content(&original, &modified, &[hunks[0].id.clone()], &hunks).unwrap();
        assert!(selected.contains("line 2 task\n"));
        assert!(selected.contains("line 14\n"));
        assert!(!selected.contains("line 14 task\n"));
    }
}
