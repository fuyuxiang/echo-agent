//! Completion snapshots for background tasks, through the terminal actor.
//! Unix only: the test logs are built with `head`, `tr`, and `/dev/zero`.

use std::collections::HashMap;
use std::time::Duration;

use pretty_assertions::assert_eq;

use crate::computer::task_log::MAX_SNAPSHOT_BYTES;
use crate::computer::types::{TaskKind, TaskSnapshot, TerminalBackend, TerminalRunRequest};
use crate::notification::types::ToolNotificationHandle;
use crate::util::truncate::FRONT_BACK_TRUNCATION_MARKER;

use super::LocalTerminalBackend;

/// The snapshot a later `get_task_output` sees, once output has left memory.
async fn snapshot_after_completion(command: &str, output_byte_limit: usize) -> TaskSnapshot {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let backend = LocalTerminalBackend::new();
    let tmp = tempfile::TempDir::new().unwrap();
    let request = TerminalRunRequest {
        command: command.to_string(),
        working_directory: tmp.path().to_path_buf(),
        env: HashMap::new(),
        timeout: Duration::from_secs(60),
        output_byte_limit,
        output_file: tmp.path().join("task.log"),
        notification_handle: ToolNotificationHandle::from_sender(tx),
        tool_call_id: "snapshot-call".to_string(),
        display_command: None,
        auto_background_on_timeout: false,
        foreground_block_budget: None,
        kind: TaskKind::Bash,
        owner_session_id: None,
        description: None,
    };

    let bg = backend.run_background(request).await.unwrap();
    backend
        .wait_for_completion(&bg.task_id, Some(Duration::from_secs(30)))
        .await
        .expect("bg task should complete");

    for _ in 0..100 {
        let snapshot = backend.get_task(&bg.task_id).await.expect("task snapshot");
        if !snapshot
            .output
            .contains(FRONT_BACK_TRUNCATION_MARKER.trim())
        {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("completed task never reloaded its output from disk");
}

/// The in-memory limit applies while a task runs, not to what it reports at
/// the end. `truncated` stays true because the task did run past that limit.
#[tokio::test]
async fn a_finished_task_reports_its_whole_log() {
    let snapshot = snapshot_after_completion(
        "head -c 50000 /dev/zero | tr '\\0' 'X'",
        /*output_byte_limit*/ 500,
    )
    .await;

    assert_eq!(snapshot.output, "X".repeat(50_000));
}

#[tokio::test]
async fn a_log_past_the_bound_is_cut_and_marked() {
    let bytes = MAX_SNAPSHOT_BYTES + 200_000;
    let snapshot = snapshot_after_completion(
        &format!("head -c {bytes} /dev/zero | tr '\\0' 'X'"),
        /*output_byte_limit*/ 500,
    )
    .await;

    assert_eq!(snapshot.output.len(), MAX_SNAPSHOT_BYTES);
    assert!(snapshot.truncated);
    assert!(
        snapshot.output_total_bytes >= bytes,
        "the snapshot holds {} bytes but must report the task's {bytes}",
        snapshot.output.len()
    );
}

#[tokio::test]
async fn a_finished_task_reports_completed_with_its_output() {
    let snapshot = snapshot_after_completion("echo done", /*output_byte_limit*/ 10_000).await;

    assert!(snapshot.completed);
    assert_eq!(snapshot.output, "done\n");
}

#[tokio::test]
async fn a_missing_log_is_not_reported_as_empty_output() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let backend = LocalTerminalBackend::new();
    let tmp = tempfile::TempDir::new().unwrap();
    let output_file = tmp.path().join("task.log");
    let request = TerminalRunRequest {
        command: "echo gone".to_string(),
        working_directory: tmp.path().to_path_buf(),
        env: HashMap::new(),
        timeout: Duration::from_secs(60),
        output_byte_limit: 10_000,
        output_file: output_file.clone(),
        notification_handle: ToolNotificationHandle::from_sender(tx),
        tool_call_id: "missing-log-call".to_string(),
        display_command: None,
        auto_background_on_timeout: false,
        foreground_block_budget: None,
        kind: TaskKind::Bash,
        owner_session_id: None,
        description: None,
    };

    let bg = backend.run_background(request).await.unwrap();
    backend
        .wait_for_completion(&bg.task_id, Some(Duration::from_secs(30)))
        .await
        .expect("bg task should complete");

    for _ in 0..100 {
        tokio::fs::remove_file(&output_file).await.ok();
        let snapshot = backend.get_task(&bg.task_id).await.expect("task snapshot");
        if snapshot.output.is_empty() {
            assert!(snapshot.truncated);
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("snapshot never fell back to the deleted log");
}
