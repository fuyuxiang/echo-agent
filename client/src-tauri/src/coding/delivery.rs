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
use crate::coding::verification::{self, VerificationKind, VerificationRecord, VerificationStatus};
use crate::coding::{store, tdd};
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
    set.changes
        .iter()
        .find(|change| change.path == path)
        .ok_or_else(|| "该文件不属于当前会话的变更".to_string())?;
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

pub fn revert_hunk_preview(
    root: &Path,
    task_id: &str,
    path: &str,
    hunk_id: &str,
) -> Result<changeset::HunkRevertPreview, String> {
    let task = task::load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
    if matches!(
        task.phase,
        TaskPhase::Discovering | TaskPhase::Implementing | TaskPhase::Repairing
    ) {
        return Err("Agent 仍在修改工程，请先停止当前运行再取舍差异块".into());
    }
    let set = changeset::load(root, task_id);
    let change = set
        .changes
        .iter()
        .find(|change| change.path == path)
        .ok_or_else(|| "该文件不属于当前会话的变更".to_string())?;
    let target = changeset::resolve_in_workspace(root, path)?;
    if change.kind == changeset::ChangeKind::Deleted
        || std::fs::symlink_metadata(&target)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Err("已删除的文件请按文件恢复，以保留原始内容及可执行权限".into());
    }
    let hunks = task_commit_hunks(root, task_id, path)?;
    if hunks.is_empty() {
        return Err("该变更没有文本差异块；文件模式或二进制变化请按文件处理".into());
    }
    if !hunks.iter().any(|hunk| hunk.id == hunk_id) {
        return Err("差异块内容或文件版本已变化，请重新打开 Diff 后重试".into());
    }
    let diff = changeset::change_diff(root, task_id, path)?;
    let keep = hunks
        .iter()
        .filter(|hunk| hunk.id != hunk_id)
        .map(|hunk| hunk.id.clone())
        .collect::<Vec<_>>();
    let modified = selected_task_content(&diff.original, &diff.modified, &keep, &hunks)?;
    Ok(changeset::HunkRevertPreview {
        path: path.into(),
        original: diff.modified,
        modified,
    })
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
        status: if set.changes.is_empty() || records.is_empty() {
            GateStatus::NotApplicable
        } else if verification_current {
            GateStatus::Satisfied
        } else {
            GateStatus::NotSatisfied
        },
        summary: if set.changes.is_empty() {
            "没有文件修改，本次检查不适用".into()
        } else if records.is_empty() {
            "尚未运行检查".into()
        } else if verification_current {
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
    let current_records = verification::current_records(root, task_id, &set);
    let mut gates = evaluate_gates(&current_records, &set, &task.acceptance_criteria, &problems);
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
    if !task.strict_workflow {
        for gate in &mut gates {
            if matches!(
                gate.id,
                GateId::DiffReview
                    | GateId::Acceptance
                    | GateId::RequirementsReview
                    | GateId::CodeQualityReview
                    | GateId::TestFirst
            ) {
                gate.status = GateStatus::NotApplicable;
                gate.summary = "按需使用；不影响继续开发或本地提交".into();
            }
        }
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
    records.iter().rev().find(|record| {
        record.runtime_review && record.kind == kind && record.content_revision == revision
    })
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
    if !root.join(path).exists() && selected_content.is_empty() {
        git_with_index(root, index, &["update-index", "--force-remove", "--", path]).await?;
        return Ok(());
    }
    let header = entry
        .split(|byte| *byte == b'\t')
        .next()
        .unwrap_or_default();
    let original_mode = std::str::from_utf8(header)
        .ok()
        .and_then(|value| value.split_whitespace().next());
    let mode = if entry.is_empty() {
        if !original_content.is_empty() {
            return Err(format!(
                "{path} 在任务开始前尚未跟踪，无法将已有内容与本轮差异块分离，请按文件核对"
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(root.join(path))
                .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
            {
                "100755"
            } else {
                "100644"
            }
        }
        #[cfg(not(unix))]
        {
            "100644"
        }
    } else {
        original_mode
            .filter(|value| matches!(*value, "100644" | "100755"))
            .ok_or_else(|| {
                format!("{path} 在任务 Git 基线中不是普通文本文件，无法安全提交差异块")
            })?
    };
    let current_head = if entry.is_empty() {
        Vec::new()
    } else {
        git_bytes(root, &["show", &format!("{baseline_head}:{path}")]).await?
    };
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
    let mode = fields.next().unwrap_or("100644");
    let kind = fields.next();
    let object = fields.next();
    if kind != Some("blob") {
        return Ok("<directory>".into());
    }
    let object = object.ok_or_else(|| format!("{path} 缺少 Git blob 标识"))?;
    let bytes = git_bytes(root, &["cat-file", "blob", object]).await?;
    Ok(changeset::content_hash_for_mode(&bytes, mode))
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

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CommitEvidence {
    pub hash: String,
    pub tree: String,
    pub paths: Vec<String>,
    pub content_revision: String,
    pub verified: bool,
    pub verification_record_ids: Vec<String>,
    pub hook_changed_tree: bool,
    pub created_at: String,
}

fn store_commit_evidence(
    root: &Path,
    task_id: &str,
    evidence: &CommitEvidence,
) -> Result<(), String> {
    store::append_jsonl(
        &store::task_dir(root, task_id).join("commits.jsonl"),
        evidence,
    )
}

#[tauri::command]
pub async fn coding_git_commit_history(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<Vec<CommitEvidence>, String> {
    let root = access.require_workspace(&root)?;
    store::validate_task_id(&task_id)?;
    Ok(store::read_jsonl(
        &store::task_dir(&root, &task_id).join("commits.jsonl"),
    ))
}

struct RealIndexGuard {
    path: PathBuf,
    lock: PathBuf,
    original: Option<Vec<u8>>,
}
impl RealIndexGuard {
    async fn acquire(root: &PathBuf) -> Result<Self, String> {
        let relative = git(root, &["rev-parse", "--git-path", "index"]).await?;
        let relative = PathBuf::from(relative.trim());
        let path = if relative.is_absolute() {
            relative
        } else {
            root.join(relative)
        };
        let lock = PathBuf::from(format!("{}.lock", path.to_string_lossy()));
        let _file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(|error| format!("Git 暂存区正在被其他操作占用，请稍后重试：{error}"))?;
        let original = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                let _ = std::fs::remove_file(&lock);
                return Err(error.to_string());
            }
        };
        Ok(Self {
            path,
            lock,
            original,
        })
    }
    fn install(&self, bytes: &[u8]) -> Result<(), String> {
        let current = std::fs::read(&self.path).ok();
        if current != self.original {
            return Err("提交期间用户暂存区发生变化，已保留当前暂存区，请手工核对".into());
        }
        std::fs::write(&self.lock, bytes).map_err(|error| error.to_string())?;
        std::fs::rename(&self.lock, &self.path).map_err(|error| error.to_string())
    }
}
impl Drop for RealIndexGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock);
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Delegate every commit hook to the configured project hook. A final native
/// Git reference-transaction hook checks the tree *after* pre-commit/commit-msg
/// have run and before HEAD moves, so a hook cannot sweep unselected user work
/// into an otherwise valid local commit.
struct CommitHookProxy {
    directory: tempfile::TempDir,
}
impl CommitHookProxy {
    async fn create(
        root: &PathBuf,
        baseline: &str,
        tree: &str,
        allowed_paths: &[String],
        partial_paths: &[String],
    ) -> Result<Self, String> {
        let version = git(root, &["--version"]).await?;
        let numbers = version
            .split_whitespace()
            .nth(2)
            .unwrap_or("")
            .split('.')
            .take(2)
            .filter_map(|value| value.parse::<u32>().ok())
            .collect::<Vec<_>>();
        if numbers.len() < 2 || numbers[0] < 2 || (numbers[0] == 2 && numbers[1] < 29) {
            return Err(
                "安全的部分提交需要 Git 2.29 或更新版本（reference-transaction hook）".into(),
            );
        }
        let hooks = git(root, &["rev-parse", "--git-path", "hooks"]).await?;
        let hooks = PathBuf::from(hooks.trim());
        let hooks = if hooks.is_absolute() {
            hooks
        } else {
            root.join(hooks)
        };
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_pair = shell_quote(&format!(
            "{}={}",
            shell_quote("core.hooksPath"),
            shell_quote(&hooks.to_string_lossy())
        ));
        let receipt = directory.path().join("reference-input");
        let exclusions = allowed_paths
            .iter()
            .map(|path| shell_quote(&format!(":(top,literal,exclude){path}")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut partial_guard = String::new();
        for path in partial_paths {
            let expected = git(root, &["ls-tree", tree, "--", path]).await?;
            partial_guard.push_str(&format!("actual=$(git ls-tree \"$new\" -- {path}) || exit 1\nif [ \"$actual\" != {expected} ]; then\n echo 'Git hook changed a partially selected file; refresh its hunks before committing' >&2\n exit 1\nfi\n", path=shell_quote(path), expected=shell_quote(expected.trim_end())));
        }
        for name in [
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
            "reference-transaction",
            "post-index-change",
            "post-rewrite",
            "pre-auto-gc",
        ] {
            let original = shell_quote(&hooks.join(name).to_string_lossy());
            let mut script = String::from("#!/bin/sh\n");
            if name == "reference-transaction" {
                script.push_str(&format!(
                    "cat > {} || exit 1\n",
                    shell_quote(&receipt.to_string_lossy())
                ));
            }
            script.push_str(&format!("if [ -x {original} ]; then\n GIT_CONFIG_PARAMETERS=\"${{GIT_CONFIG_PARAMETERS}} \"{config_pair} {original} \"$@\"{input}\n result=$?\n [ \"$result\" -eq 0 ] || exit \"$result\"\nfi\n", input=if name == "reference-transaction" { format!(" < {}", shell_quote(&receipt.to_string_lossy())) } else { String::new() }));
            if name == "reference-transaction" {
                script.push_str(&format!("if [ \"$1\" = prepared ]; then\n while read -r old new reference; do\n  case \"$reference\" in HEAD|refs/heads/*)\n   if [ \"$old\" != {baseline} ]; then echo 'Git HEAD changed while preparing this commit; no reference was updated' >&2; exit 1; fi\n   git diff-tree --no-ext-diff -r --quiet --exit-code {baseline} \"$new\" -- . {exclusions} || {{ echo 'Git hook staged files outside the selected candidate; user changes were preserved' >&2; exit 1; }}\n   {partial_guard}\n   ;;\n  esac\n done < {receipt}\nfi\n", baseline=shell_quote(baseline), receipt=shell_quote(&receipt.to_string_lossy())));
            }
            script.push_str("exit 0\n");
            let path = directory.path().join(name);
            std::fs::write(&path, script).map_err(|error| error.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(Self { directory })
    }
    fn configuration(&self) -> String {
        format!("core.hooksPath={}", self.directory.path().display())
    }
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
    commit_task(root, task_id, message, selected_paths, selected_hunks).await
}

pub async fn commit_task(
    root: PathBuf,
    task_id: String,
    message: String,
    selected_paths: Option<Vec<String>>,
    selected_hunks: Option<BTreeMap<String, Vec<String>>>,
) -> Result<String, String> {
    crate::coding_document_bridge::ensure_workspace_admission(&root.to_string_lossy()).await?;
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
    if matches!(
        report.task.phase,
        TaskPhase::Discovering
            | TaskPhase::Implementing
            | TaskPhase::Repairing
            | TaskPhase::Verifying
    ) {
        return Err("任务或检查仍在执行，请等待结束或停止后提交".into());
    }
    if report.task.strict_workflow && !report.deliverable {
        return Err("当前任务已显式启用严格工程策略，请完成该策略的检查后提交".into());
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
        if ids.is_empty() || paths.contains(path) {
            return Err("差异块选择为空，或与所选整文件重复".into());
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
    // Standard Git commit semantics are required: hooks, signing, identity,
    // reflog and ref locking all belong to Git, not a commit-tree substitute.
    let guard = RealIndexGuard::acquire(&root).await?;
    changeset::ensure_head_unchanged(&root, &set)?;
    changeset::ensure_changes_current(&root, &set)?;
    let hook_proxy = CommitHookProxy::create(
        &root,
        baseline_head,
        &tree,
        &all_paths,
        &hunk_selection.keys().cloned().collect::<Vec<_>>(),
    )
    .await?;
    git_with_index(
        &root,
        &index_path,
        &["-c", &hook_proxy.configuration(), "commit", "-m", &trimmed],
    )
    .await?;
    let hash = git(&root, &["rev-parse", "HEAD"]).await?.trim().to_string();
    let parent = git(&root, &["rev-parse", "HEAD^"]).await?;
    if parent.trim() != baseline_head {
        return Err(format!(
            "Git 已创建提交 {hash}，但并发 HEAD 变化使父提交与候选不符；保留现场，请检查 Git 历史"
        ));
    }
    let actual_tree = git(&root, &["rev-parse", "HEAD^{tree}"])
        .await?
        .trim()
        .to_string();
    let actual_paths = git(
        &root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-z",
            "HEAD",
        ],
    )
    .await?
    .split('\0')
    .filter(|path| !path.is_empty())
    .map(str::to_owned)
    .collect::<Vec<_>>();
    // Hooks are allowed to amend candidate content. Evidence is recomputed
    // against the final tree and never inherited from a partial worktree pass.
    let records = verification::current_records(&root, &task_id, &set);
    let expected = verification::detect_commands(&root);
    let mut evidence_ids = Vec::new();
    for check in &expected {
        if let Some(record) = records
            .iter()
            .rev()
            .find(|record| record.command == check.command)
        {
            if record.status == VerificationStatus::Passed
                && record.workspace_tree.as_deref() == Some(actual_tree.as_str())
            {
                evidence_ids.push(record.id.clone());
            }
        }
    }
    let evidence = CommitEvidence {
        hash: hash.clone(),
        tree: actual_tree.clone(),
        paths: actual_paths.clone(),
        content_revision: set.content_revision(),
        verified: !expected.is_empty() && evidence_ids.len() == expected.len(),
        verification_record_ids: evidence_ids,
        hook_changed_tree: actual_tree != tree,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    store_commit_evidence(&root, &task_id, &evidence)
        .map_err(|error| format!("已创建提交 {hash}，但保存提交证据失败：{error}"))?;
    let partial_baselines = hunk_merges
        .into_iter()
        .map(|(path, _, content)| (path, content))
        .collect::<BTreeMap<_, _>>();
    changeset::mark_committed_with_baselines(
        &root,
        &task_id,
        &hash,
        &actual_paths,
        &partial_baselines,
    )
    .map_err(|error| format!("已创建提交 {hash}，但记录会话变更失败：{error}"))?;
    // Compute the replacement real index in isolation, starting with the
    // original staging bytes. The guard keeps other normal Git writers out.
    let sync_index = index_dir.path().join("user-index");
    if let Some(bytes) = &guard.original {
        std::fs::write(&sync_index, bytes).map_err(|error| error.to_string())?;
    } else {
        // An absent user index means an empty staging area, including staged
        // removals relative to HEAD. Do not silently reconstruct unrelated
        // tracked entries while synchronizing only the committed selection.
        git_with_index(&root, &sync_index, &["read-tree", "--empty"]).await?;
    }
    let mut reset_arguments: Vec<&str> = vec!["reset", "-q", "HEAD", "--"];
    reset_arguments.extend(actual_paths.iter().map(String::as_str));
    git_with_index(&root, &sync_index, &reset_arguments)
        .await
        .map_err(|error| format!("已创建提交 {hash}，但同步暂存区失败：{error}"))?;
    guard
        .install(&std::fs::read(&sync_index).map_err(|error| error.to_string())?)
        .map_err(|error| format!("已创建提交 {hash}，但保留用户暂存区失败：{error}"))?;
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
        let records = [record(VerificationKind::Test, "pnpm test", 0)];
        let stale = evaluate_gates(&records, &set, &criteria(true), &[]);
        assert_eq!(
            stale
                .iter()
                .find(|gate| gate.id == GateId::VerificationFreshness)
                .unwrap()
                .status,
            GateStatus::NotSatisfied
        );

        set.verified_revision = Some(set.content_revision());
        let current = evaluate_gates(&records, &set, &criteria(true), &[]);
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
    async fn commit_fixture() -> (tempfile::TempDir, CodingTask) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        git(&root, &["init", "-q"]).await.unwrap();
        for (key, value) in [
            ("user.name", "Echo test"),
            ("user.email", "echo@example.test"),
            ("commit.gpgsign", "false"),
        ] {
            git(&root, &["config", key, value]).await.unwrap();
        }
        for path in ["a.txt", "b.txt", "user.txt"] {
            std::fs::write(root.join(path), "base\n").unwrap();
        }
        git(&root, &["add", "."]).await.unwrap();
        git(&root, &["commit", "-qm", "baseline"]).await.unwrap();
        let mut task = task::create_task(&root, "Continuous session", "edit files").unwrap();
        task.phase = TaskPhase::Delivered;
        task::save(&root, &task).unwrap();
        changeset::capture_baseline(&root, &task.id, Vec::new()).unwrap();
        (directory, task)
    }

    #[tokio::test]
    async fn continuous_partial_commits_preserve_user_index_and_unselected_changes() {
        let (directory, task) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        std::fs::write(root.join("user.txt"), "user staged\n").unwrap();
        git(&root, &["add", "--", "user.txt"]).await.unwrap();
        let user_entry = git(&root, &["ls-files", "--stage", "--", "user.txt"])
            .await
            .unwrap();
        // The task baseline must protect changes that already existed at start.
        // Capture through a fresh task so the real staged user content is the
        // original snapshot, while both Agent files start from clean HEAD.
        let mut task = task::create_task(&root, "Partial session", "edit a and b").unwrap();
        task.phase = TaskPhase::Delivered;
        task::save(&root, &task).unwrap();
        changeset::capture_baseline(&root, &task.id, vec!["user.txt".into()]).unwrap();
        std::fs::write(root.join("a.txt"), "agent a\n").unwrap();
        std::fs::write(root.join("b.txt"), "agent b\n").unwrap();
        changeset::sync_changes(&root, &task.id).await.unwrap();
        let first = commit_task(
            root.clone(),
            task.id.clone(),
            "first partial".into(),
            Some(vec!["a.txt".into()]),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            git(&root, &["show", &format!("{first}:a.txt")])
                .await
                .unwrap(),
            "agent a\n"
        );
        assert_eq!(
            git(&root, &["show", &format!("{first}:b.txt")])
                .await
                .unwrap(),
            "base\n"
        );
        let remaining = changeset::sync_changes(&root, &task.id).await.unwrap();
        assert_eq!(
            remaining
                .changes
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            ["b.txt"]
        );
        assert_eq!(
            git(&root, &["ls-files", "--stage", "--", "user.txt"])
                .await
                .unwrap(),
            user_entry
        );
        let second = commit_task(
            root.clone(),
            task.id.clone(),
            "second partial".into(),
            Some(vec!["b.txt".into()]),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            git(&root, &["rev-parse", &format!("{second}^")])
                .await
                .unwrap()
                .trim(),
            first
        );
        assert!(changeset::sync_changes(&root, &task.id)
            .await
            .unwrap()
            .changes
            .is_empty());
        assert_eq!(
            git(&root, &["ls-files", "--stage", "--", "user.txt"])
                .await
                .unwrap(),
            user_entry
        );
        assert_eq!(
            std::fs::read_to_string(root.join("user.txt")).unwrap(),
            "user staged\n"
        );
        assert_eq!(
            store::read_jsonl::<CommitEvidence>(
                &store::task_dir(&root, &task.id).join("commits.jsonl")
            )
            .len(),
            2
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejecting_git_hook_prevents_commit_and_keeps_staging_recoverable() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, task) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        let original_head = git(&root, &["rev-parse", "HEAD"]).await.unwrap();
        let original_index = std::fs::read(root.join(".git/index")).unwrap();
        let hook = root.join(".git/hooks/pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho 'real hook rejected candidate' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(root.join("a.txt"), "agent a\n").unwrap();
        let error = commit_task(
            root.clone(),
            task.id.clone(),
            "rejected".into(),
            Some(vec!["a.txt".into()]),
            None,
        )
        .await
        .unwrap_err();
        assert!(error.contains("real hook rejected candidate"));
        assert_eq!(
            git(&root, &["rev-parse", "HEAD"]).await.unwrap(),
            original_head
        );
        assert_eq!(
            std::fs::read(root.join(".git/index")).unwrap(),
            original_index
        );
        assert!(!root.join(".git/index.lock").exists());
        assert!(changeset::load(&root, &task.id).committed_hash.is_none());
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "agent a\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hook_modified_tree_is_recorded_and_never_inherits_other_tree_evidence() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, task) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        std::fs::write(root.join("a.txt"), "agent a\n").unwrap();
        let hook = root.join(".git/hooks/pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\nprintf 'hook formatting\\n' >> a.txt\ngit add -- a.txt\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hash = commit_task(
            root.clone(),
            task.id.clone(),
            "hook candidate".into(),
            Some(vec!["a.txt".into()]),
            None,
        )
        .await
        .unwrap();
        let evidence = store::read_jsonl::<CommitEvidence>(
            &store::task_dir(&root, &task.id).join("commits.jsonl"),
        );
        assert!(evidence[0].hook_changed_tree);
        assert!(!evidence[0].verified);
        assert!(git(&root, &["show", &format!("{hash}:a.txt")])
            .await
            .unwrap()
            .contains("hook formatting"));
        assert!(changeset::sync_changes(&root, &task.id)
            .await
            .unwrap()
            .changes
            .is_empty());
    }
    async fn capture_real_check(root: &Path, task_id: &str, command: &str) {
        let status = std::process::Command::new("sh")
            .args(["-c", command])
            .current_dir(root)
            .status()
            .unwrap();
        let set = changeset::sync_changes(root, task_id).await.unwrap();
        let mut record = record_from_parts(
            task_id,
            VerificationKind::Test,
            command,
            status.code(),
            String::new(),
            String::new(),
            1,
            false,
            false,
        );
        record.content_revision = Some(set.content_revision());
        record.configuration_revision = Some(verification::configuration_revision(root));
        record.workspace_tree = verification::workspace_tree(root).await;
        verification::append_record(root, &record).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn partial_candidate_requires_its_own_tree_evidence_and_wip_keeps_failed_status() {
        let (directory, task) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        let config = verification::VerificationProjectConfig {
            checks: vec![verification::ConfiguredCheck {
                id: "test".into(),
                kind: VerificationKind::Test,
                command: "true".into(),
                label: "Real shell pass".into(),
                cwd: ".".into(),
                env: BTreeMap::new(),
                timeout_secs: Some(10),
                auto_run: false,
            }],
        };
        store::write_json(
            &store::workspace_dir(&root).join("verification-config.json"),
            &config,
        )
        .unwrap();
        std::fs::write(root.join("a.txt"), "depends on b change\n").unwrap();
        std::fs::write(root.join("b.txt"), "dependency change\n").unwrap();
        capture_real_check(&root, &task.id, "true").await;
        commit_task(
            root.clone(),
            task.id.clone(),
            "partial candidate".into(),
            Some(vec!["a.txt".into()]),
            None,
        )
        .await
        .unwrap();
        let history_path = store::task_dir(&root, &task.id).join("commits.jsonl");
        let first = store::read_jsonl::<CommitEvidence>(&history_path);
        assert!(
            !first[0].verified,
            "a worktree pass must not validate a different partial tree"
        );
        capture_real_check(&root, &task.id, "true").await;
        commit_task(
            root.clone(),
            task.id.clone(),
            "exact candidate".into(),
            Some(vec!["b.txt".into()]),
            None,
        )
        .await
        .unwrap();
        let second = store::read_jsonl::<CommitEvidence>(&history_path);
        assert!(
            second[1].verified,
            "the exact final tree has a real passing command"
        );
        std::fs::write(root.join("a.txt"), "work in progress\n").unwrap();
        let mut failing = config;
        failing.checks[0].command = "false".into();
        store::write_json(
            &store::workspace_dir(&root).join("verification-config.json"),
            &failing,
        )
        .unwrap();
        capture_real_check(&root, &task.id, "false").await;
        assert!(!build_report(&root, &task.id).unwrap().deliverable);
        commit_task(
            root.clone(),
            task.id.clone(),
            "local WIP".into(),
            Some(vec!["a.txt".into()]),
            None,
        )
        .await
        .unwrap();
        let history = store::read_jsonl::<CommitEvidence>(&history_path);
        assert_eq!(history.len(), 3);
        assert!(!history[2].verified);
        assert_eq!(
            verification::list_records(&root, &task.id)
                .last()
                .unwrap()
                .status,
            VerificationStatus::Failed
        );
    }
    #[tokio::test]
    async fn hunk_rejection_is_a_version_bound_proposal_and_preserves_initial_user_edits() {
        let (directory, _) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        let baseline = (1..=24)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        std::fs::write(root.join("a.txt"), &baseline).unwrap();
        git(&root, &["add", "a.txt"]).await.unwrap();
        git(&root, &["commit", "-qm", "long baseline"])
            .await
            .unwrap();
        let user_content = baseline.replace("line 2\n", "line 2 user start\n");
        std::fs::write(root.join("a.txt"), &user_content).unwrap();
        let mut coding_task = task::create_task(&root, "Hunk review", "Edit two sections").unwrap();
        coding_task.phase = TaskPhase::Delivered;
        task::save(&root, &coding_task).unwrap();
        changeset::capture_baseline(&root, &coding_task.id, vec!["a.txt".into()]).unwrap();
        let current = user_content
            .replace("line 8\n", "line 8 Agent\n")
            .replace("line 22\n", "line 22 Agent\n");
        std::fs::write(root.join("a.txt"), &current).unwrap();
        changeset::sync_changes(&root, &coding_task.id)
            .await
            .unwrap();
        let hunks = task_commit_hunks(&root, &coding_task.id, "a.txt").unwrap();
        assert_eq!(hunks.len(), 2);
        let proposal = revert_hunk_preview(&root, &coding_task.id, "a.txt", &hunks[0].id).unwrap();
        assert_eq!(proposal.original, current);
        assert!(proposal.modified.contains("line 2 user start\n"));
        assert!(proposal.modified.contains("line 8\n"));
        assert!(proposal.modified.contains("line 22 Agent\n"));
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            current,
            "preview must not write the project"
        );
        std::fs::write(root.join("a.txt"), "user changed after preview\n").unwrap();
        changeset::sync_changes(&root, &coding_task.id)
            .await
            .unwrap();
        assert!(revert_hunk_preview(&root, &coding_task.id, "a.txt", &hunks[0].id).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deleted_executable_hunks_remain_committable_but_restore_requires_file_metadata() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, _) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        std::fs::set_permissions(root.join("a.txt"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let mut coding_task =
            task::create_task(&root, "Deleted executable", "review deletion").unwrap();
        coding_task.phase = TaskPhase::Delivered;
        task::save(&root, &coding_task).unwrap();
        changeset::capture_baseline(&root, &coding_task.id, vec!["a.txt".into()]).unwrap();
        let original = std::fs::read(root.join("a.txt")).unwrap();
        std::fs::remove_file(root.join("a.txt")).unwrap();
        changeset::sync_changes(&root, &coding_task.id)
            .await
            .unwrap();
        let hunks = task_commit_hunks(&root, &coding_task.id, "a.txt").unwrap();
        assert!(
            !hunks.is_empty(),
            "deleted text hunks still support partial commits"
        );
        let error = revert_hunk_preview(&root, &coding_task.id, "a.txt", &hunks[0].id).unwrap_err();
        assert!(error.contains("按文件恢复") && error.contains("可执行"));
        assert!(
            !root.join("a.txt").exists(),
            "preview never recreates a missing file"
        );
        changeset::discard_file(&root, &coding_task.id, "a.txt").unwrap();
        assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), original);
        assert_eq!(
            std::fs::metadata(root.join("a.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    #[tokio::test]
    async fn clean_tracked_file_supports_two_hunk_commits_without_freezing_the_session() {
        let (directory, _) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        let original = (1..=24)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        std::fs::write(root.join("a.txt"), &original).unwrap();
        git(&root, &["add", "a.txt"]).await.unwrap();
        git(&root, &["commit", "-qm", "long baseline"])
            .await
            .unwrap();
        let mut coding_task = task::create_task(&root, "Partial hunks", "Edit sections").unwrap();
        coding_task.phase = TaskPhase::Delivered;
        task::save(&root, &coding_task).unwrap();
        changeset::capture_baseline(&root, &coding_task.id, Vec::new()).unwrap();
        let modified = original
            .replace("line 2\n", "line 2 Agent\n")
            .replace("line 22\n", "line 22 Agent\n");
        std::fs::write(root.join("a.txt"), &modified).unwrap();
        changeset::sync_changes(&root, &coding_task.id)
            .await
            .unwrap();
        let hunks = task_commit_hunks(&root, &coding_task.id, "a.txt").unwrap();
        assert_eq!(hunks.len(), 2);
        let first = commit_task(
            root.clone(),
            coding_task.id.clone(),
            "first hunk".into(),
            Some(Vec::new()),
            Some(BTreeMap::from([(
                "a.txt".into(),
                vec![hunks[0].id.clone()],
            )])),
        )
        .await
        .unwrap();
        let first_text = git(&root, &["show", &format!("{first}:a.txt")])
            .await
            .unwrap();
        assert!(first_text.contains("line 2 Agent\n"));
        assert!(first_text.contains("line 22\n"));
        let remaining = changeset::sync_changes(&root, &coding_task.id)
            .await
            .unwrap();
        assert_eq!(remaining.changes.len(), 1);
        assert!(!remaining.changes[0].pre_existing);
        let remaining_hunks = task_commit_hunks(&root, &coding_task.id, "a.txt").unwrap();
        assert_eq!(remaining_hunks.len(), 1);
        let second = commit_task(
            root.clone(),
            coding_task.id.clone(),
            "second hunk".into(),
            Some(Vec::new()),
            Some(BTreeMap::from([(
                "a.txt".into(),
                vec![remaining_hunks[0].id.clone()],
            )])),
        )
        .await
        .unwrap();
        assert_eq!(
            git(&root, &["show", &format!("{second}:a.txt")])
                .await
                .unwrap(),
            modified
        );
        assert!(changeset::sync_changes(&root, &coding_task.id)
            .await
            .unwrap()
            .changes
            .is_empty());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn hook_cannot_sweep_unselected_user_files_into_the_final_tree() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, task) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        let head = git(&root, &["rev-parse", "HEAD"]).await.unwrap();
        std::fs::write(root.join("a.txt"), "selected Agent edit\n").unwrap();
        std::fs::write(root.join("user.txt"), "unselected user work\n").unwrap();
        git(&root, &["add", "user.txt"]).await.unwrap();
        let index = std::fs::read(root.join(".git/index")).unwrap();
        let hook = root.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\ngit add -A\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = commit_task(
            root.clone(),
            task.id.clone(),
            "selected only".into(),
            Some(vec!["a.txt".into()]),
            None,
        )
        .await
        .unwrap_err();
        assert!(error.contains("outside the selected candidate"), "{error}");
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).await.unwrap(), head);
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
        assert_eq!(
            std::fs::read_to_string(root.join("user.txt")).unwrap(),
            "unselected user work\n"
        );
        assert!(changeset::load(&root, &task.id).committed_hash.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn configured_hooks_receive_normal_paths_arguments_and_reference_phases() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, task) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        let hooks = root.join("project hooks ' literal $dollar");
        std::fs::create_dir(&hooks).unwrap();
        git(
            &root,
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
        )
        .await
        .unwrap();
        let scripts = [
            (
                "pre-commit",
                "#!/bin/sh\ngit config --path core.hooksPath > .git/hook-path-seen\n",
            ),
            ("commit-msg", "#!/bin/sh\ncat \"$1\" > .git/message-seen\n"),
            (
                "reference-transaction",
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> .git/reference-phases\ncat >/dev/null\n",
            ),
        ];
        for (name, content) in scripts {
            let path = hooks.join(name);
            std::fs::write(&path, content).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Hook files are a user-owned pre-existing untracked directory. A
        // selected file commit must not include them.
        std::fs::write(root.join("a.txt"), "selected edit\n").unwrap();
        commit_task(
            root.clone(),
            task.id.clone(),
            "message passed normally".into(),
            Some(vec!["a.txt".into()]),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(".git/hook-path-seen"))
                .unwrap()
                .trim(),
            hooks.to_string_lossy()
        );
        assert!(std::fs::read_to_string(root.join(".git/message-seen"))
            .unwrap()
            .contains("message passed normally"));
        let phases = std::fs::read_to_string(root.join(".git/reference-phases")).unwrap();
        assert!(phases.contains("prepared"));
        assert!(phases.contains("committed"));
        assert_eq!(
            git(&root, &["config", "core.hooksPath"])
                .await
                .unwrap()
                .trim(),
            hooks.to_string_lossy()
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn hook_cannot_expand_a_partial_hunk_to_unselected_sections() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, _) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        let original = (1..=24)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        std::fs::write(root.join("a.txt"), &original).unwrap();
        git(&root, &["add", "a.txt"]).await.unwrap();
        git(&root, &["commit", "-qm", "long baseline"])
            .await
            .unwrap();
        let mut coding_task = task::create_task(&root, "Hook hunk scope", "edit sections").unwrap();
        coding_task.phase = TaskPhase::Delivered;
        task::save(&root, &coding_task).unwrap();
        changeset::capture_baseline(&root, &coding_task.id, Vec::new()).unwrap();
        let modified = original
            .replace("line 2\n", "line 2 selected\n")
            .replace("line 22\n", "line 22 unselected\n");
        std::fs::write(root.join("a.txt"), &modified).unwrap();
        changeset::sync_changes(&root, &coding_task.id)
            .await
            .unwrap();
        let hunks = task_commit_hunks(&root, &coding_task.id, "a.txt").unwrap();
        assert_eq!(hunks.len(), 2);
        let head = git(&root, &["rev-parse", "HEAD"]).await.unwrap();
        let index = std::fs::read(root.join(".git/index")).unwrap();
        let hook = root.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\ngit add a.txt\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = commit_task(
            root.clone(),
            coding_task.id.clone(),
            "one section only".into(),
            Some(Vec::new()),
            Some(BTreeMap::from([(
                "a.txt".into(),
                vec![hunks[0].id.clone()],
            )])),
        )
        .await
        .unwrap_err();
        assert!(error.contains("partially selected file"), "{error}");
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).await.unwrap(), head);
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            modified
        );
        assert!(changeset::load(&root, &coding_task.id)
            .committed_hash
            .is_none());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn final_hook_scope_recurses_and_isolates_selected_files_inside_directories() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, coding_task) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/selected.ts"), "first selected\n").unwrap();
        std::fs::write(root.join("src/user.ts"), "unselected user work\n").unwrap();
        let first = commit_task(
            root.clone(),
            coding_task.id.clone(),
            "nested selected".into(),
            Some(vec!["src/selected.ts".into()]),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            git(&root, &["show", &format!("{first}:src/selected.ts")])
                .await
                .unwrap(),
            "first selected\n"
        );
        assert!(git(&root, &["show", &format!("{first}:src/user.ts")])
            .await
            .is_err());
        std::fs::write(root.join("src/selected.ts"), "second selected\n").unwrap();
        let hook = root.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\ngit add -A\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = commit_task(
            root.clone(),
            coding_task.id.clone(),
            "nested sweep rejected".into(),
            Some(vec!["src/selected.ts".into()]),
            None,
        )
        .await
        .unwrap_err();
        assert!(error.contains("outside the selected candidate"), "{error}");
        assert_eq!(
            git(&root, &["rev-parse", "HEAD"]).await.unwrap().trim(),
            first
        );
        assert_eq!(
            std::fs::read_to_string(root.join("src/user.ts")).unwrap(),
            "unselected user work\n"
        );
    }
    #[tokio::test]
    async fn an_absent_user_index_preserves_unrelated_staged_removals() {
        let (directory, coding_task) = commit_fixture().await;
        let root = directory.path().to_path_buf();
        std::fs::write(root.join("added.txt"), "selected addition\n").unwrap();
        std::fs::remove_file(root.join(".git/index")).unwrap();
        commit_task(
            root.clone(),
            coding_task.id.clone(),
            "new file only".into(),
            Some(vec!["added.txt".into()]),
            None,
        )
        .await
        .unwrap();
        let staged = git(&root, &["diff", "--cached", "--name-only"])
            .await
            .unwrap();
        for path in ["a.txt", "b.txt", "user.txt"] {
            assert!(
                staged.lines().any(|line| line == path),
                "missing original staged removal: {staged}"
            );
        }
        let index_paths = git(&root, &["ls-files"]).await.unwrap();
        assert_eq!(index_paths.trim(), "added.txt");
        assert_eq!(
            std::fs::read_to_string(root.join("user.txt")).unwrap(),
            "base\n"
        );
    }
}
