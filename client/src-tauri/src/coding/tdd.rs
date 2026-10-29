//! Test-first evidence is based on an actual failed native test run, not an
//! Agent assertion. A native checkpoint or a live typed Runtime test can prove
//! RED before implementation, including an existing failing regression test.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::coding::changeset::{self, ChangeSet};
use crate::coding::store;
use crate::coding::task::{self, CodingTask, TaskPhase};
use crate::coding::verification::{self, VerificationKind, VerificationRecord, VerificationStatus};
use crate::shell_fs::FilesystemAccess;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TddEvidence {
    pub red_command: Option<String>,
    pub red_record_id: Option<String>,
    pub red_revision: Option<String>,
    pub red_at: Option<String>,
    pub waiver_reason: Option<String>,
    /// Keep RED independently of the current verification batch, which is
    /// deliberately cleared when GREEN begins. Older evidence remains readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red_record: Option<VerificationRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_provenance: Option<RuntimeRedProvenance>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRedProvenance {
    pub session_id: String,
    pub model_id: String,
    pub tool_call_id: String,
    pub workspace: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeTestStart {
    session_id: String,
    model_id: String,
    command: String,
    revision: String,
    started_at: String,
}

fn starts_path(root: &Path, task_id: &str) -> std::path::PathBuf {
    store::task_dir(root, task_id).join("tdd-runtime-starts.json")
}

fn path(root: &Path, task_id: &str) -> std::path::PathBuf {
    store::task_dir(root, task_id).join("tdd.json")
}

pub fn load(root: &Path, task_id: &str) -> TddEvidence {
    store::read_json(&path(root, task_id)).unwrap_or_default()
}

pub fn clear(root: &Path, task_id: &str) -> Result<(), String> {
    store::write_json(
        &starts_path(root, task_id),
        &BTreeMap::<String, RuntimeTestStart>::new(),
    )?;
    store::write_json(&path(root, task_id), &TddEvidence::default())
}

/// Only normalize the package manager's exact built-in test-script shorthand.
/// Arguments, shell composition, other scripts and executables stay distinct.
pub fn same_test_command(left: &str, right: &str) -> bool {
    fn canonical(command: &str) -> String {
        let words = command
            .split([' ', '\t'])
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        match words.as_slice() {
            [manager, "test"] | [manager, "run", "test"]
                if matches!(*manager, "npm" | "pnpm" | "yarn") =>
            {
                format!("{manager} test")
            }
            _ => command.to_string(),
        }
    }
    canonical(left) == canonical(right)
}

#[path = "tdd_test_paths.rs"]
mod test_paths;
pub use test_paths::is_test_path;

pub fn test_commands(root: &Path, task: &CodingTask) -> Vec<String> {
    let mut commands = verification::detect_commands(root)
        .into_iter()
        .filter(|command| command.kind == VerificationKind::Test)
        .map(|command| command.command)
        .collect::<Vec<_>>();
    for command in task
        .task_nodes
        .iter()
        .flat_map(|node| node.verification_commands.iter())
    {
        let lower = command.to_lowercase();
        if ["test", "pytest", "vitest", "jest", "spec"]
            .iter()
            .any(|word| lower.contains(word))
            && !commands.contains(command)
        {
            commands.push(command.clone());
        }
    }
    commands
}

pub fn only_test_changes(set: &ChangeSet) -> bool {
    !set.changes.is_empty() && set.changes.iter().all(|change| is_test_path(&change.path))
}

pub fn needs_test_first(root: &Path, task: &CodingTask, set: &ChangeSet) -> bool {
    task.strict_workflow
        && !crate::coding::documentation::is_read_only_task(task)
        && !test_commands(root, task).is_empty()
        && set.changes.iter().any(|change| !is_test_path(&change.path))
}

pub fn should_pause_for_red(root: &Path, task: &CodingTask, set: &ChangeSet) -> bool {
    let evidence = load(root, &task.id);
    task.strict_workflow
        && evidence.red_record_id.is_none()
        && evidence.waiver_reason.is_none()
        && !test_commands(root, task).is_empty()
        && only_test_changes(set)
}

pub fn can_resume(root: &Path, task: &CodingTask) -> bool {
    if !task.strict_workflow || task.phase_reason.as_deref() != Some("等待红灯测试验证") {
        return true;
    }
    let evidence = load(root, &task.id);
    evidence.red_record_id.is_some() || evidence.waiver_reason.is_some()
}

fn at_or_after(actual: &str, threshold: &str) -> bool {
    match (
        DateTime::parse_from_rfc3339(actual),
        DateTime::parse_from_rfc3339(threshold),
    ) {
        (Ok(actual), Ok(threshold)) => actual >= threshold,
        _ => false,
    }
}

fn live_event_time(meta: Option<&serde_json::Value>) -> Option<String> {
    let meta = meta?;
    if meta.get("isReplay").and_then(serde_json::Value::as_bool) == Some(true) {
        return None;
    }
    let timestamp = meta.get("agentTimestampMs")?.as_i64()?;
    let now = chrono::Utc::now().timestamp_millis();
    // Historical replay must never reconstruct an implementation checkpoint.
    if timestamp > now + 5_000 || timestamp < now - 5 * 60_000 {
        return None;
    }
    chrono::DateTime::from_timestamp_millis(timestamp).map(|time| time.to_rfc3339())
}

fn can_capture_runtime_red(task: &CodingTask, set: &ChangeSet) -> bool {
    matches!(task.phase, TaskPhase::Discovering | TaskPhase::Implementing)
        && set.baseline_mode.is_some()
        && set.rollback_unsafe_files.is_empty()
        && (set.changes.is_empty() || only_test_changes(set))
}

/// Called only from the native ACP dispatcher, never from renderer IPC. Both
/// ends of the real foreground Bash tool must see the same pre-implementation
/// revision. A late completion after Write, a different session/model, replay,
/// cancellation, timeout, changed cwd or a shell-composed command cannot prove
/// RED. We do not execute or grant approval to any additional command here.
pub(crate) async fn observe_runtime_update(
    root: &Path,
    session_id: &str,
    update: &serde_json::Value,
    meta: Option<&serde_json::Value>,
) -> Result<bool, String> {
    if !matches!(
        update
            .get("sessionUpdate")
            .and_then(serde_json::Value::as_str),
        Some("tool_call" | "tool_call_update")
    ) {
        return Ok(false);
    }
    let terminal_status = matches!(
        update.get("status").and_then(serde_json::Value::as_str),
        Some("completed" | "failed" | "cancelled")
    );
    if update
        .get("rawInput")
        .and_then(|input| input.get("variant"))
        .and_then(serde_json::Value::as_str)
        != Some("Bash")
        && update
            .get("rawOutput")
            .and_then(|output| output.get("type"))
            .and_then(serde_json::Value::as_str)
            != Some("Bash")
        && !terminal_status
    {
        return Ok(false);
    }
    let Some(time) = live_event_time(meta) else {
        return Ok(false);
    };
    let Some(tool_id) = update
        .get("toolCallId")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 100)
    else {
        return Ok(false);
    };
    let mut tasks = task::list_tasks(root)
        .into_iter()
        .filter_map(|summary| task::load(root, &summary.id))
        .filter(|task| task.session_id.as_deref() == Some(session_id));
    let Some(task) = tasks.next() else {
        return Ok(false);
    };
    // One session cannot establish evidence for two independent tasks.
    if tasks.next().is_some() || !at_or_after(&time, &task.created_at) {
        return Ok(false);
    }
    let Some(model_id) = task.model_id.as_deref().filter(|id| !id.is_empty()) else {
        return Ok(false);
    };
    // Denied/cancelled tools can finish with Error instead of Bash output.
    // Release only this native task/session/model's known candidate so repeated
    // rejections cannot exhaust the bounded cache and block the next real test.
    if terminal_status
        && update
            .get("rawOutput")
            .and_then(|output| output.get("type"))
            .and_then(serde_json::Value::as_str)
            != Some("Bash")
    {
        let candidate =
            store::read_json::<BTreeMap<String, RuntimeTestStart>>(&starts_path(root, &task.id))
                .and_then(|starts| starts.get(tool_id).cloned());
        if candidate
            .is_some_and(|start| start.session_id == session_id && start.model_id == model_id)
        {
            store::update_json::<BTreeMap<String, RuntimeTestStart>, _>(
                &starts_path(root, &task.id),
                |starts| {
                    if starts.get(tool_id).is_some_and(|start| {
                        start.session_id == session_id && start.model_id == model_id
                    }) {
                        starts.remove(tool_id);
                    }
                    Ok(())
                },
            )?;
        }
        return Ok(false);
    }
    let evidence = load(root, &task.id);
    if evidence.red_record_id.is_some() || evidence.waiver_reason.is_some() {
        return Ok(false);
    }
    let commands = test_commands(root, &task);
    if commands.is_empty() {
        return Ok(false);
    }

    // ACP input refinements omit status to preserve the initial Pending state
    // (the real Runtime does this). A completed/failed update or any actual
    // output must consume a previous start, never manufacture one at the end.
    let start_status = match update.get("status") {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(status)) => {
            matches!(status.as_str(), "pending" | "in_progress")
        }
        _ => false,
    };
    if let Some(input) = update.get("rawInput").filter(|input| {
        start_status
            && update
                .get("rawOutput")
                .is_none_or(serde_json::Value::is_null)
            && input.get("variant").and_then(serde_json::Value::as_str) == Some("Bash")
    }) {
        let Some(command) = input.get("command").and_then(serde_json::Value::as_str) else {
            return Ok(false);
        };
        if input
            .get("is_background")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            || !commands
                .iter()
                .any(|expected| same_test_command(expected, command))
        {
            return Ok(false);
        }
        let set = changeset::sync_changes(root, &task.id).await?;
        if !can_capture_runtime_red(&task, &set) {
            return Ok(false);
        }
        changeset::ensure_changes_current(root, &set)?;
        let start = RuntimeTestStart {
            session_id: session_id.into(),
            model_id: model_id.into(),
            command: command.into(),
            revision: set.content_revision(),
            started_at: time.clone(),
        };
        store::update_json::<BTreeMap<String, RuntimeTestStart>, _>(
            &starts_path(root, &task.id),
            |starts| {
                let threshold = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
                starts.retain(|_, start| at_or_after(&start.started_at, &threshold));
                if starts.len() < 16 {
                    starts.entry(tool_id.into()).or_insert(start);
                }
                Ok(())
            },
        )?;
    }

    let Some(output) = update
        .get("rawOutput")
        .filter(|output| output.get("type").and_then(serde_json::Value::as_str) == Some("Bash"))
    else {
        return Ok(false);
    };
    let start = store::update_json::<BTreeMap<String, RuntimeTestStart>, _>(
        &starts_path(root, &task.id),
        |starts| Ok(starts.remove(tool_id)),
    )?;
    let Some(start) = start else { return Ok(false) };
    let exit = output
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .and_then(|exit| i32::try_from(exit).ok());
    let command = output
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let cwd = output
        .get("current_dir")
        .and_then(serde_json::Value::as_str)
        .and_then(|cwd| Path::new(cwd).canonicalize().ok());
    if update.get("status").and_then(serde_json::Value::as_str) != Some("completed")
        || start.session_id != session_id
        || start.model_id != model_id
        || command != start.command
        || exit.is_none()
        || exit == Some(0)
        || output.get("timed_out").and_then(serde_json::Value::as_bool) != Some(false)
        || output.get("signal").is_some_and(|signal| !signal.is_null())
        || output.get("truncated").and_then(serde_json::Value::as_bool) != Some(false)
        || cwd.as_deref() != Some(root)
        || !at_or_after(&time, &start.started_at)
    {
        return Ok(false);
    }
    let bytes = output
        .get("output")
        .and_then(serde_json::Value::as_array)
        .filter(|bytes| bytes.len() <= 1024 * 1024)
        .and_then(|bytes| {
            bytes
                .iter()
                .map(|byte| byte.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                .collect::<Option<Vec<_>>>()
        });
    let Some(bytes) = bytes else { return Ok(false) };
    // Runtime Bash has one combined byte stream. Classify it as error output
    // too, so a missing test runner cannot become RED merely because it was
    // delivered through the combined output field.
    let combined = String::from_utf8_lossy(&bytes).into_owned();
    let mut record = verification::record_from_parts(
        &task.id,
        VerificationKind::Test,
        command,
        exit,
        String::new(),
        combined.clone(),
        0,
        false,
        false,
    );
    if record.status != VerificationStatus::Failed {
        return Ok(false);
    }
    record.stdout = combined;
    record.stderr.clear();
    let set = changeset::sync_changes(root, &task.id).await?;
    let current_task = task::load(root, &task.id).ok_or("任务不存在")?;
    if current_task.session_id.as_deref() != Some(session_id)
        || current_task.model_id.as_deref() != Some(model_id)
        || !can_capture_runtime_red(&current_task, &set)
        || set.content_revision() != start.revision
    {
        return Ok(false);
    }
    changeset::ensure_changes_current(root, &set)?;
    record.id = format!("runtime-{tool_id}");
    record.started_at = start.started_at;
    record.finished_at = time;
    record.content_revision = Some(start.revision);
    record.duration_ms = DateTime::parse_from_rfc3339(&record.finished_at)
        .ok()
        .zip(DateTime::parse_from_rfc3339(&record.started_at).ok())
        .map(|(finished, started)| (finished - started).num_milliseconds().max(0) as u64)
        .unwrap_or_default();
    let evidence = TddEvidence {
        red_command: Some(record.command.clone()),
        red_record_id: Some(record.id.clone()),
        red_revision: record.content_revision.clone(),
        red_at: Some(record.finished_at.clone()),
        waiver_reason: None,
        red_record: Some(record),
        runtime_provenance: Some(RuntimeRedProvenance {
            session_id: session_id.into(),
            model_id: model_id.into(),
            tool_call_id: tool_id.into(),
            workspace: root.to_string_lossy().into_owned(),
        }),
    };
    store::update_json::<TddEvidence, _>(&path(root, &task.id), |current| {
        if current.red_record_id.is_none() && current.waiver_reason.is_none() {
            *current = evidence;
        }
        Ok(())
    })?;
    Ok(true)
}

pub fn record_red(root: &Path, task_id: &str, record_id: &str) -> Result<TddEvidence, String> {
    let task = task::load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
    if task.phase != TaskPhase::Paused || task.phase_reason.as_deref() != Some("等待红灯测试验证")
    {
        return Err("任务未停在测试先行检查点".into());
    }
    let set = changeset::load(root, task_id);
    changeset::ensure_changes_current(root, &set)?;
    if !only_test_changes(&set) {
        return Err("红灯运行前只允许测试文件发生变化".into());
    }
    let record = verification::list_records(root, task_id)
        .into_iter()
        .find(|record| record.id == record_id)
        .ok_or_else(|| "未找到该次验证的原生执行记录".to_string())?;
    if record.kind != VerificationKind::Test
        || record.status != VerificationStatus::Failed
        || !test_commands(root, &task)
            .iter()
            .any(|command| same_test_command(command, &record.command))
        || record.content_revision.as_deref() != Some(set.content_revision().as_str())
        || !at_or_after(&record.started_at, &task.updated_at)
    {
        return Err("只有检查点之后执行的计划内测试真实失败结果可作为 RED 证据".into());
    }
    let evidence = TddEvidence {
        red_command: Some(record.command.clone()),
        red_record_id: Some(record.id.clone()),
        red_revision: Some(set.content_revision()),
        red_at: Some(record.finished_at.clone()),
        waiver_reason: None,
        red_record: Some(record),
        runtime_provenance: None,
    };
    store::write_json(&path(root, task_id), &evidence)?;
    Ok(evidence)
}

pub fn waive(root: &Path, task_id: &str, reason: &str) -> Result<TddEvidence, String> {
    let task = task::load(root, task_id).ok_or_else(|| "任务不存在".to_string())?;
    if !matches!(
        task.phase,
        TaskPhase::Paused | TaskPhase::Delivered | TaskPhase::Blocked
    ) {
        return Err("请在任务暂停或完成后说明测试先行豁免原因".into());
    }
    let reason = reason.trim();
    if reason.chars().count() < 8 || reason.chars().count() > 500 {
        return Err("请填写 8 到 500 字的具体豁免原因".into());
    }
    let evidence = TddEvidence {
        waiver_reason: Some(reason.into()),
        ..TddEvidence::default()
    };
    store::write_json(&path(root, task_id), &evidence)?;
    Ok(evidence)
}

pub fn green_record<'a>(
    evidence: &TddEvidence,
    records: &'a [VerificationRecord],
    set: &ChangeSet,
) -> Option<&'a VerificationRecord> {
    if evidence.red_revision.as_deref() == Some(set.content_revision().as_str()) {
        return None;
    }
    let command = evidence.red_command.as_deref()?;
    let red_at = evidence.red_at.as_deref()?;
    let revision = set.content_revision();
    records.iter().rev().find(|record| {
        record.kind == VerificationKind::Test
            && same_test_command(&record.command, command)
            && record.status == VerificationStatus::Passed
            && record.content_revision.as_deref() == Some(revision.as_str())
            && at_or_after(&record.started_at, red_at)
    })
}

#[tauri::command]
pub async fn coding_tdd_status(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
) -> Result<TddEvidence, String> {
    let root = access.require_workspace(&root)?;
    store::validate_task_id(&task_id)?;
    Ok(load(&root, &task_id))
}

#[tauri::command]
pub async fn coding_tdd_record_red(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    record_id: String,
) -> Result<TddEvidence, String> {
    let root = access.require_workspace(&root)?;
    changeset::sync_changes(&root, &task_id).await?;
    tokio::task::spawn_blocking(move || record_red(&root, &task_id, &record_id))
        .await
        .map_err(|error| format!("记录红灯测试失败：{error}"))?
}

#[tauri::command]
pub async fn coding_tdd_waive(
    access: State<'_, FilesystemAccess>,
    root: String,
    task_id: String,
    reason: String,
) -> Result<TddEvidence, String> {
    let root = access.require_workspace(&root)?;
    tokio::task::spawn_blocking(move || waive(&root, &task_id, &reason))
        .await
        .map_err(|error| format!("记录测试先行豁免失败：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn runtime_fixture(root: &Path) -> CodingTask {
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"test":"node --test"}}"#,
        )
        .unwrap();
        std::fs::write(root.join("test.cjs"), "const test=require('node:test'); const assert=require('node:assert/strict'); test('existing regression',()=>assert.equal(require('./implementation.cjs').value,42));\n").unwrap();
        let mut task = task::create_task(root, "Existing regression", "Fix existing test").unwrap();
        task.strict_workflow = true;
        task.phase = TaskPhase::Discovering;
        task.created_at = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
        task.task_nodes[0].verification_commands = vec!["node --test".into()];
        task.session_id = Some("native-runtime-session".into());
        task.model_id = Some("native-runtime-model".into());
        task::save(root, &task).unwrap();
        changeset::capture_filesystem_baseline(root, &task.id).unwrap();
        task
    }

    fn runtime_start(command: &str, tool_id: &str) -> Value {
        json!({"sessionUpdate":"tool_call_update", "toolCallId":tool_id,
            "rawInput":{"variant":"Bash","command":command,"is_background":false}})
    }

    fn runtime_end(root: &Path, command: &str, tool_id: &str, exit: i32, bytes: &[u8]) -> Value {
        json!({"sessionUpdate":"tool_call_update", "toolCallId":tool_id,"status":"completed",
            "rawOutput":{"type":"Bash","command":command,"exit_code":exit,"current_dir":root,
                "timed_out":false,"signal":null,"truncated":false,"output":bytes}})
    }

    fn live_meta() -> Value {
        json!({"agentTimestampMs":chrono::Utc::now().timestamp_millis()})
    }

    #[test]
    fn package_test_shorthand_is_equivalent_without_ignoring_arguments_or_shells() {
        for manager in ["npm", "pnpm", "yarn"] {
            assert!(same_test_command(
                &format!("{manager} test"),
                &format!("{manager} run test")
            ));
        }
        for other in [
            "npm test -- --run",
            "npm test; echo 0",
            "npm run test:unit",
            "pnpm test",
            "echo npm test",
            "npm\ntest",
            "npm test\n",
            "npm\u{00a0}test",
        ] {
            assert!(!same_test_command("npm test", other), "{other}");
        }
        assert!(!same_test_command("node --test", "node --test test.cjs"));
        assert!(!same_test_command("bun test", "bun run test"));
        assert!(same_test_command("bun test", "bun test"));
        assert!(same_test_command("bun run test", "bun run test"));
    }

    #[tokio::test]
    async fn existing_failed_test_records_native_red_before_implementation_and_survives_green_batch(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let task = runtime_fixture(&root);
        let mut start = runtime_start("node --test", "native-existing-test");
        assert!(!observe_runtime_update(
            &root,
            "native-runtime-session",
            &start,
            Some(&live_meta())
        )
        .await
        .unwrap());
        let failed = crate::process_supervisor::background_async_command("node")
            .args(["--test"])
            .current_dir(&root)
            .output()
            .await
            .unwrap();
        assert!(!failed.status.success());
        let mut output = failed.stdout;
        output.extend(failed.stderr);
        let end = runtime_end(
            &root,
            "node --test",
            "native-existing-test",
            failed.status.code().unwrap(),
            &output,
        );
        assert!(
            observe_runtime_update(&root, "native-runtime-session", &end, Some(&live_meta()))
                .await
                .unwrap()
        );
        let evidence = load(&root, &task.id);
        assert_eq!(evidence.red_command.as_deref(), Some("node --test"));
        assert_eq!(
            evidence.runtime_provenance.as_ref().unwrap().model_id,
            "native-runtime-model"
        );
        assert_eq!(
            evidence.red_record.as_ref().unwrap().status,
            VerificationStatus::Failed
        );
        assert!(evidence
            .red_record
            .as_ref()
            .unwrap()
            .stdout
            .contains("MODULE_NOT_FOUND"));
        verification::clear_records(&root, &task.id).unwrap();
        assert!(load(&root, &task.id).red_record.is_some());
        std::fs::write(root.join("implementation.cjs"), "exports.value=42;\n").unwrap();
        let set = changeset::sync_changes(&root, &task.id).await.unwrap();
        let passed = crate::process_supervisor::background_async_command("node")
            .args(["--test"])
            .current_dir(&root)
            .output()
            .await
            .unwrap();
        assert!(passed.status.success());
        let mut green = verification::record_from_parts(
            &task.id,
            VerificationKind::Test,
            "node --test",
            passed.status.code(),
            String::from_utf8_lossy(&passed.stdout).into_owned(),
            String::from_utf8_lossy(&passed.stderr).into_owned(),
            1,
            false,
            false,
        );
        green.content_revision = Some(set.content_revision());
        assert!(green_record(&evidence, &[green], &set).is_some());
        // Duplicate or late Runtime results cannot replace the original RED.
        start["rawInput"]["command"] = json!("npm run test");
        observe_runtime_update(&root, "native-runtime-session", &start, Some(&live_meta()))
            .await
            .unwrap();
        assert_eq!(load(&root, &task.id).red_record_id, evidence.red_record_id);
    }

    #[tokio::test]
    async fn runtime_red_rejects_implementation_written_during_test() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let task = runtime_fixture(&root);
        observe_runtime_update(
            &root,
            "native-runtime-session",
            &runtime_start("npm test", "race"),
            Some(&live_meta()),
        )
        .await
        .unwrap();
        std::fs::write(root.join("implementation.cjs"), "exports.value=42").unwrap();
        assert!(!observe_runtime_update(
            &root,
            "native-runtime-session",
            &runtime_end(&root, "npm test", "race", 1, b"failed"),
            Some(&live_meta())
        )
        .await
        .unwrap());
        assert!(load(&root, &task.id).red_record_id.is_none());
    }

    #[tokio::test]
    async fn runtime_red_rejects_replay_foreign_scope_bad_outcomes_and_unplanned_commands() {
        for scenario in [
            "replay",
            "wrong_cwd",
            "passed",
            "timeout",
            "signal",
            "truncated",
            "missing_exit",
            "environment",
            "background",
            "foreign_session",
            "rebound_model",
            "unplanned",
            "old_timestamp",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            let mut task = runtime_fixture(&root);
            let mut start = runtime_start("npm test", scenario);
            let mut end = runtime_end(&root, "npm test", scenario, 1, b"failed");
            let mut meta = live_meta();
            let mut session = "native-runtime-session";
            match scenario {
                "replay" => meta["isReplay"] = json!(true),
                "background" => start["rawInput"]["is_background"] = json!(true),
                "foreign_session" => session = "other-session",
                "unplanned" => start["rawInput"]["command"] = json!("npm test; echo anything"),
                "old_timestamp" => meta["agentTimestampMs"] = json!(1),
                _ => {}
            }
            observe_runtime_update(&root, session, &start, Some(&meta))
                .await
                .unwrap();
            match scenario {
                "wrong_cwd" => {
                    end["rawOutput"]["current_dir"] = json!(directory.path().parent().unwrap())
                }
                "passed" => end["rawOutput"]["exit_code"] = json!(0),
                "timeout" => end["rawOutput"]["timed_out"] = json!(true),
                "signal" => end["rawOutput"]["signal"] = json!("SIGTERM"),
                "truncated" => end["rawOutput"]["truncated"] = json!(true),
                "missing_exit" => {
                    end["rawOutput"]
                        .as_object_mut()
                        .unwrap()
                        .remove("exit_code");
                }
                "environment" => end["rawOutput"]["exit_code"] = json!(127),
                "rebound_model" => {
                    task.model_id = Some("different-model".into());
                    task::save(&root, &task).unwrap();
                }
                _ => {}
            }
            assert!(
                !observe_runtime_update(&root, session, &end, Some(&meta))
                    .await
                    .unwrap(),
                "{scenario}"
            );
            assert!(load(&root, &task.id).red_record_id.is_none(), "{scenario}");
        }
    }

    #[tokio::test]
    async fn runtime_red_requires_start_and_unique_task_binding() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let task = runtime_fixture(&root);
        assert!(!observe_runtime_update(
            &root,
            "native-runtime-session",
            &runtime_end(&root, "npm test", "no-start", 1, b"failed"),
            Some(&live_meta())
        )
        .await
        .unwrap());
        let mut completed_with_input =
            runtime_end(&root, "npm test", "no-start-full", 1, b"failed");
        completed_with_input["rawInput"] =
            runtime_start("npm test", "no-start-full")["rawInput"].clone();
        assert!(!observe_runtime_update(
            &root,
            "native-runtime-session",
            &completed_with_input,
            Some(&live_meta())
        )
        .await
        .unwrap());
        assert!(load(&root, &task.id).red_record_id.is_none());
        let foreign_root = tempfile::tempdir().unwrap();
        for (index, status) in ["failed", "cancelled", "completed"]
            .into_iter()
            .cycle()
            .take(20)
            .enumerate()
        {
            let tool_id = format!("permission-rejection-{index}");
            let mut start = runtime_start("npm test", &tool_id);
            start["status"] = json!("in_progress");
            observe_runtime_update(&root, "native-runtime-session", &start, Some(&live_meta()))
                .await
                .unwrap();
            let terminal = json!({"sessionUpdate":"tool_call_update", "toolCallId":tool_id,
                "status":status, "rawOutput":{"type":"Error","message":"permission rejected"}});
            // A foreign session/root cannot clear this task's pending test.
            observe_runtime_update(&root, "other-session", &terminal, Some(&live_meta()))
                .await
                .unwrap();
            observe_runtime_update(
                foreign_root.path(),
                "native-runtime-session",
                &terminal,
                Some(&live_meta()),
            )
            .await
            .unwrap();
            let starts = store::read_json::<BTreeMap<String, RuntimeTestStart>>(&starts_path(
                &root, &task.id,
            ))
            .unwrap();
            assert!(starts.contains_key(&tool_id));
            assert!(!observe_runtime_update(
                &root,
                "native-runtime-session",
                &terminal,
                Some(&live_meta())
            )
            .await
            .unwrap());
            assert!(
                store::read_json::<BTreeMap<String, RuntimeTestStart>>(&starts_path(
                    &root, &task.id
                ))
                .unwrap()
                .is_empty()
            );
            assert!(load(&root, &task.id).red_record_id.is_none());
        }
        // More than the 16-entry cap of rejections must not block recovery.
        let mut recovered_start = runtime_start("npm test", "recovered");
        recovered_start["status"] = json!("pending");
        observe_runtime_update(
            &root,
            "native-runtime-session",
            &recovered_start,
            Some(&live_meta()),
        )
        .await
        .unwrap();
        assert!(
            store::read_json::<BTreeMap<String, RuntimeTestStart>>(&starts_path(&root, &task.id))
                .unwrap()
                .contains_key("recovered")
        );
        let mut duplicate = task::create_task(&root, "Second", "Other task").unwrap();
        duplicate.session_id = task.session_id.clone();
        duplicate.model_id = task.model_id.clone();
        task::save(&root, &duplicate).unwrap();
        observe_runtime_update(
            &root,
            "native-runtime-session",
            &runtime_start("npm test", "duplicate"),
            Some(&live_meta()),
        )
        .await
        .unwrap();
        assert!(!observe_runtime_update(
            &root,
            "native-runtime-session",
            &runtime_end(&root, "npm test", "duplicate", 1, b"failed"),
            Some(&live_meta())
        )
        .await
        .unwrap());
        assert!(load(&root, &task.id).red_record_id.is_none());
    }

    #[test]
    fn compares_timestamps_by_instant_across_offsets_and_precision() {
        assert!(at_or_after(
            "2026-09-25T08:00:00+08:00",
            "2026-09-25T00:00:00Z"
        ));
        assert!(at_or_after(
            "2026-09-25T00:00:00.100Z",
            "2026-09-25T00:00:00Z"
        ));
        assert!(!at_or_after(
            "2026-09-25T00:00:00Z",
            "2026-09-25T08:00:00+07:00"
        ));
        assert!(!at_or_after("invalid", "2026-09-25T00:00:00Z"));
    }

    #[test]
    fn identifies_common_test_files_without_classifying_source_files() {
        for path in [
            "src/user.test.ts",
            "src/__tests__/user.ts",
            "tests/api.py",
            "src/user_test.rs",
            "src/UserTest.java",
            "test.cjs",
            "src/test.mjs",
            "src/user_test.py",
        ] {
            assert!(is_test_path(path), "{path}");
        }
        assert!(!is_test_path("src/user.ts"));
    }

    #[tokio::test]
    async fn red_failure_then_green_pass_is_revision_bound() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut coding_task = task::create_task(root, "TDD", "Fix behavior").unwrap();
        coding_task.strict_workflow = true;
        coding_task.task_nodes[0].verification_commands = vec!["pnpm test".into()];
        coding_task.phase = TaskPhase::Paused;
        coding_task.phase_reason = Some("等待红灯测试验证".into());
        task::save(root, &coding_task).unwrap();
        changeset::capture_filesystem_baseline(root, &coding_task.id).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("tests/user.test.ts"),
            "expect(false).toBe(true)\n",
        )
        .unwrap();
        let red_set = changeset::sync_changes(root, &coding_task.id)
            .await
            .unwrap();
        assert!(only_test_changes(&red_set));
        assert!(should_pause_for_red(root, &coding_task, &red_set));
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
        red.content_revision = Some(red_set.content_revision());
        verification::append_record(root, &red).unwrap();
        let evidence = record_red(root, &coding_task.id, &red.id).unwrap();
        assert!(can_resume(root, &coding_task));
        assert!(green_record(&evidence, &[], &red_set).is_none());

        std::fs::write(root.join("src.ts"), "export const fixed = true;\n").unwrap();
        let green_set = changeset::sync_changes(root, &coding_task.id)
            .await
            .unwrap();
        let mut green = verification::record_from_parts(
            &coding_task.id,
            VerificationKind::Test,
            "pnpm test",
            Some(0),
            "passed".into(),
            String::new(),
            1,
            false,
            false,
        );
        assert!(green_record(&evidence, &[green.clone()], &green_set).is_none());
        green.content_revision = Some(green_set.content_revision());
        let mut shorthand = green.clone();
        shorthand.command = "pnpm run test".into();
        assert!(green_record(&evidence, &[shorthand], &green_set).is_some());
        assert_eq!(
            green_record(&evidence, &[green], &green_set).map(|record| record.status),
            Some(VerificationStatus::Passed)
        );
    }
}
